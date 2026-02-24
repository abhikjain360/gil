//! Multi-producer single-consumer (MPSC) queue.
//!
//! This queue is an adaptation of Dmitry Vyukov's bounded MPMC queue, optimized for a single consumer.
//!
//! # Examples
//!
//! ```
//! use std::thread;
//! use core::num::NonZeroUsize;
//! use gil::mpsc::channel;
//!
//! let (tx, mut rx) = channel::<usize>(NonZeroUsize::new(1024).unwrap());
//!
//! for _ in 0..4 {
//!     let mut tx = tx.clone();
//!     thread::spawn(move || {
//!         tx.send(42);
//!     });
//! }
//! drop(tx);
//!
//! for _ in 0..4 {
//!     assert_eq!(rx.recv(), 42);
//! }
//! ```
//!
//! # Performance
//!
//! **Improvements over standard implementations:**
//! - **Single Allocation:** The queue header and buffer are allocated contiguously, improving cache locality.
//! - **False Sharing Prevention:** Head and tail pointers are padded to prevent false sharing.
//!
//! # When to use
//!
//! Use this queue when you have multiple producer threads sending data to a single consumer thread.
//! It typically outperforms a general-purpose MPMC queue in this scenario because the consumer
//! does not need to contend with other consumers.
//!
//! # Gotchas
//!
//! - **Cloneability:** [`Sender`] implements `Clone`, but [`Receiver`] does not. Clone senders to
//!   distribute to multiple producer threads.
//! - **No Async:** Unlike SPSC, this queue does not have async support.
//! - **No Batch Operations:** The non-sharded variant does not support batch operations. Use
//!   [`sharded`] if you need zero-copy batch operations.
//! - **Capacity Rounding:** The actual capacity is rounded up to the next power of two.
//!
//! # Reference
//!
//! * Adapted from [Dmitry Vyukov's Bounded MPMC Queue](http://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue)

use core::num::NonZeroUsize;

pub use self::{receiver::Receiver, sender::Sender};

mod queue;
// mod queue_old;
mod receiver;
mod sender;
pub mod sharded;
#[cfg(feature = "std")]
pub mod sharded_parking;

/// Creates a new multi-producer single-consumer (MPSC) queue.
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
/// use gil::mpsc::channel;
///
/// let (tx, rx) = channel::<usize>(NonZeroUsize::new(1024).unwrap());
/// ```
pub fn channel<T>(capacity: NonZeroUsize) -> (Sender<T>, Receiver<T>) {
    let queue = queue::QueuePtr::with_size(capacity);
    queue.initialize::<queue::Initializer<T>>();

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

        let (tx, mut rx) = channel::<(u32, u32)>(NonZeroUsize::new(4).unwrap());

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
