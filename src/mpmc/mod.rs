//! Multi-producer multi-consumer (MPMC) queue.
//!
//! This queue is based on the bounded MPMC queue algorithm by Dmitry Vyukov.
//! It provides high throughput and low latency for concurrent message passing.
//!
//! # Examples
//!
//! ```
//! use std::thread;
//! use core::num::NonZeroUsize;
//! use gil::mpmc::channel;
//!
//! let (tx, mut rx) = channel::<usize>(NonZeroUsize::new(1024).unwrap());
//!
//! // Multiple producers
//! let mut tx2 = tx.clone();
//! let h1 = thread::spawn(move || tx2.send(1));
//! let mut tx3 = tx.clone();
//! let h2 = thread::spawn(move || tx3.send(2));
//! drop(tx);
//!
//! let mut values = [rx.recv(), rx.recv()];
//! values.sort();
//! assert_eq!(values, [1, 2]);
//!
//! h1.join().unwrap();
//! h2.join().unwrap();
//! ```
//!
//! # Performance
//!
//! **Improvements over original implementation:**
//! - **Single Allocation:** The queue header (metadata) and the buffer are allocated as a single
//!   contiguous memory block. This reduces memory fragmentation and improves cache locality.
//! - **False Sharing Prevention:** Critical atomic counters (head and tail) are padded to match
//!   cache line sizes, preventing false sharing between producers and consumers.
//!
//! # When to use
//!
//! Use this queue when you have multiple threads sending messages and multiple threads
//! receiving messages. If you only have a single consumer, consider using [`mpsc::channel`](crate::mpsc::channel)
//! for potentially better performance.
//!
//! # Gotchas
//!
//! - **Cloneability:** Both [`Sender`] and [`Receiver`] implement `Clone`. Clone them to distribute
//!   to multiple producer and consumer threads.
//! - **No Async:** Unlike SPSC, this queue does not have async support.
//! - **No Batch Operations:** The non-sharded variant does not support batch operations. Use
//!   [`sharded`] if you need zero-copy batch operations.
//! - **Capacity Rounding:** The usable capacity is rounded up to the next power of two
//!   (e.g. requesting 1000 yields a queue that holds 1024 items). This differs from the SPSC
//!   queue, which holds exactly the requested capacity.
//!
//! # Reference
//!
//! * [Dmitry Vyukov's Bounded MPMC Queue](http://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue)

use core::num::NonZeroUsize;

pub use self::{receiver::Receiver, sender::Sender};

mod queue;
mod receiver;
mod sender;
pub mod sharded;

/// Creates a new multi-producer multi-consumer (MPMC) queue.
///
/// See the [module-level documentation](self) for more details on performance and usage.
///
/// # Arguments
///
/// * `capacity` - The capacity of the queue.
///
/// # Returns
///
/// A tuple containing the [`Sender`] and [`Receiver`] handles.
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::mpmc::channel;
///
/// let (tx, rx) = channel::<usize>(NonZeroUsize::new(1024).unwrap());
/// ```
pub fn channel<T>(capacity: NonZeroUsize) -> (Sender<T>, Receiver<T>) {
    let queue = queue::QueuePtr::with_size(capacity);
    queue.initialize::<crate::cell::CellInit<T>>();

    (Sender::new(queue.clone()), Receiver::new(queue))
}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use super::*;

    use crate::thread;

    #[test]
    fn basic() {
        const THREADS: u32 = 10;
        const ITER: u32 = 1000;

        let (tx, mut rx) = channel(NonZeroUsize::new(4).unwrap());

        thread::scope(move |scope| {
            for thread_id in 0..THREADS {
                let mut tx = tx.clone();
                scope.spawn(move || {
                    for i in 0..ITER {
                        tx.send((thread_id, i));
                    }
                });
            }

            let mut sum = 0;
            for _ in 0..THREADS {
                for _ in 0..ITER {
                    let (_thread_id, i) = rx.recv();
                    sum += i;
                }
            }

            assert_eq!(sum, (ITER * (ITER - 1)) / 2 * THREADS);
        });
    }

    #[test]
    fn multiple_senders_multiple_receivers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const SENDERS: usize = 4;
        const RECEIVERS: usize = 4;
        const MESSAGES: usize = 1000;

        let (tx, rx) = channel(NonZeroUsize::new(128).unwrap());
        let total_received = std::sync::Arc::new(AtomicUsize::new(0));
        let total_sum = std::sync::Arc::new(AtomicUsize::new(0));

        thread::scope(|s| {
            for t in 0..SENDERS {
                let mut tx = tx.clone();
                s.spawn(move || {
                    for i in 0..MESSAGES {
                        tx.send(t * MESSAGES + i);
                    }
                });
            }

            for _ in 0..RECEIVERS {
                let mut rx = rx.clone();
                let total_received = total_received.clone();
                let total_sum = total_sum.clone();
                s.spawn(move || {
                    for _ in 0..(SENDERS * MESSAGES / RECEIVERS) {
                        let val = rx.recv();
                        total_received.fetch_add(1, Ordering::SeqCst);
                        total_sum.fetch_add(val, Ordering::SeqCst);
                    }
                });
            }
        });

        assert_eq!(total_received.load(Ordering::SeqCst), SENDERS * MESSAGES);
        let n = SENDERS * MESSAGES;
        let expected_sum = n * (n - 1) / 2;
        assert_eq!(total_sum.load(Ordering::SeqCst), expected_sum);
    }

    #[test]
    fn multiple_senders_multiple_receivers_try() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const SENDERS: usize = 4;
        const RECEIVERS: usize = 4;
        const MESSAGES: usize = 1000;

        let (tx, rx) = channel(NonZeroUsize::new(128).unwrap());
        let total_received = std::sync::Arc::new(AtomicUsize::new(0));
        let total_sum = std::sync::Arc::new(AtomicUsize::new(0));

        thread::scope(|s| {
            for t in 0..SENDERS {
                let mut tx = tx.clone();
                s.spawn(move || {
                    for i in 0..MESSAGES {
                        let mut backoff = crate::Backoff::with_spin_count(1);
                        while tx.try_send(t * MESSAGES + i).is_err() {
                            backoff.backoff();
                        }
                    }
                });
            }

            for _ in 0..RECEIVERS {
                let mut rx = rx.clone();
                let total_received = total_received.clone();
                let total_sum = total_sum.clone();
                s.spawn(move || {
                    let mut count = 0;
                    let mut backoff = crate::Backoff::with_spin_count(1);
                    while count < (SENDERS * MESSAGES / RECEIVERS) {
                        if let Some(val) = rx.try_recv() {
                            total_received.fetch_add(1, Ordering::SeqCst);
                            total_sum.fetch_add(val, Ordering::SeqCst);
                            count += 1;
                        } else {
                            backoff.backoff();
                        }
                    }
                });
            }
        });

        assert_eq!(total_received.load(Ordering::SeqCst), SENDERS * MESSAGES);
        let n = SENDERS * MESSAGES;
        let expected_sum = n * (n - 1) / 2;
        assert_eq!(total_sum.load(Ordering::SeqCst), expected_sum);
    }

    #[test]
    fn test_valid_try_sends() {
        let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(4).unwrap());
        for _ in 0..4 {
            assert!(rx.try_recv().is_none());
        }
        for i in 0..4 {
            tx.try_send(i).unwrap();
        }
        assert!(tx.try_send(5).is_err());

        for i in 0..4 {
            assert_eq!(rx.try_recv(), Some(i));
        }
        assert!(rx.try_recv().is_none());
        for i in 0..4 {
            tx.try_send(i).unwrap();
        }
    }

    #[test]
    fn test_drop_full_capacity() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct DropCounter(Arc<AtomicUsize>);

        impl Drop for DropCounter {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let dropped_count = Arc::new(AtomicUsize::new(0));

        {
            // Request size 3. Capacity will be 4.
            let (mut tx, _rx) = channel::<DropCounter>(NonZeroUsize::new(3).unwrap());

            // Push 4 items.
            for _ in 0..4 {
                tx.send(DropCounter(dropped_count.clone()));
            }
        }

        let count = dropped_count.load(Ordering::SeqCst);
        assert_eq!(
            count, 4,
            "Expected 4 items to be dropped, but got {}",
            count
        );
    }
}
