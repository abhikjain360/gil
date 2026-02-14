//! Parking variant of the SPSC queue.
//!
//! This module provides a single-producer single-consumer queue that uses
//! futex-based parking instead of pure spin-waiting. After a short spin phase
//! and a yield phase, blocked threads park via [`atomic_wait`] and are woken by
//! the other side, freeing the CPU for other work.
//!
//! Use this variant when latency spikes from idle spinning are unacceptable, or
//! when the producer and consumer may be idle for extended periods. For the
//! lowest-latency spin-only variant, see [`spsc::channel`](super::channel).
//!
//! # Examples
//!
//! ```
//! use std::thread;
//! use core::num::NonZeroUsize;
//! use gil::spsc::parking::channel;
//!
//! let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(1024).unwrap());
//!
//! thread::spawn(move || {
//!     for i in 0..100 {
//!         tx.send(i);
//!     }
//! });
//!
//! for i in 0..100 {
//!     assert_eq!(rx.recv(), i);
//! }
//! ```
//!
//! # Performance
//!
//! The blocking strategy is a three-phase backoff controlled by
//! [`ParkingBackoff`](crate::ParkingBackoff):
//!
//! 1. **Spin** — a configurable number of [`spin_loop`](core::hint::spin_loop) hints.
//! 2. **Yield** — a configurable number of [`yield_now`](std::thread::yield_now) calls.
//! 3. **Park** — the thread parks on a futex and is woken by the other side.
//!
//! Because the queue shares a single futex between sender and receiver, at most
//! one side can be parked at any time. This is sufficient for SPSC since the
//! queue cannot be simultaneously full (sender waits) and empty (receiver waits).
//!
//! # When to use
//!
//! Use this queue for 1-to-1 thread communication where threads may be idle for
//! long periods or where CPU usage from spinning is a concern.
//!
//! # Gotchas
//!
//! - **Not Cloneable:** Neither [`Sender`] nor [`Receiver`] implement `Clone`. They are `Send` but
//!   not `Sync`, so they can be moved to another thread but not shared.
//! - **Batch Operations:** Use [`Sender::write_buffer`]/[`Sender::commit`] and
//!   [`Receiver::read_buffer`]/[`Receiver::advance`] for zero-copy batch operations.

use core::num::NonZeroUsize;

pub use self::{receiver::Receiver, sender::Sender};

mod queue;
mod receiver;
mod sender;

/// Creates a new parking single-producer single-consumer (SPSC) queue.
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
/// use gil::spsc::parking::channel;
///
/// let (tx, rx) = channel::<usize>(NonZeroUsize::new(1024).unwrap());
/// ```
pub fn channel<T>(capacity: NonZeroUsize) -> (Sender<T>, Receiver<T>) {
    let queue = queue::QueuePtr::with_size(capacity);
    (Sender::new(queue.clone()), Receiver::new(queue))
}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use std::num::NonZeroUsize;

    use super::*;
    use crate::thread;

    #[test]
    fn test_valid_sends() {
        const COUNTS: NonZeroUsize = NonZeroUsize::new(4096).unwrap();
        let (mut tx, mut rx) = channel::<usize>(COUNTS);

        thread::spawn(move || {
            for i in 0..COUNTS.get() << 3 {
                tx.send(i as usize);
            }
        });

        for i in 0..COUNTS.get() << 3 {
            let r = rx.recv();
            assert_eq!(r, i as usize);
        }
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
    fn test_batched_send_recv() {
        const CAPACITY: NonZeroUsize = NonZeroUsize::new(1024).unwrap();
        const TOTAL_ITEMS: usize = 1024 << 4;
        let (mut tx, mut rx) = channel::<usize>(CAPACITY);

        thread::spawn(move || {
            let mut sent = 0;
            while sent < TOTAL_ITEMS {
                let buffer = tx.write_buffer();
                let batch_size = buffer.len().min(TOTAL_ITEMS - sent);
                for i in 0..batch_size {
                    buffer[i].write(sent + i);
                }
                unsafe { tx.commit(batch_size) };
                sent += batch_size;
            }
        });

        let mut received = 0;
        let mut expected = 0;

        while received < TOTAL_ITEMS {
            let mut guard = rx.read_guard();
            if guard.is_empty() {
                continue;
            }
            for &value in guard.as_slice() {
                assert_eq!(value, expected);
                expected += 1;
            }
            let count = guard.len();
            guard.advance(count);
            received += count;
        }

        assert_eq!(received, TOTAL_ITEMS);
    }

    #[test]
    fn test_drop_remaining_elements() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

        #[derive(Clone)]
        struct DropCounter;

        impl Drop for DropCounter {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::SeqCst);
            }
        }

        DROP_COUNT.store(0, Ordering::SeqCst);

        {
            let (mut tx, rx) = channel::<DropCounter>(NonZeroUsize::new(16).unwrap());

            for _ in 0..5 {
                tx.send(DropCounter);
            }

            drop(tx);
            drop(rx);
        }

        assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 5);
    }
}

#[cfg(all(test, feature = "loom"))]
mod loom_test {
    use core::num::NonZeroUsize;

    use super::*;
    use crate::thread;

    #[test]
    fn basic_loom() {
        loom::model(|| {
            let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(2).unwrap());
            let counts = 3;

            thread::spawn(move || {
                for i in 0..counts {
                    tx.send(i);
                }
            });

            for i in 0..counts {
                let r = rx.recv();
                assert_eq!(r, i);
            }
        })
    }

    #[test]
    fn try_ops_loom() {
        loom::model(|| {
            let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(2).unwrap());

            thread::spawn(move || {
                let mut i = 0;
                while i < 3 {
                    if tx.try_send(i).is_ok() {
                        i += 1;
                    }
                    loom::thread::yield_now();
                }
            });

            let mut i = 0;
            while i < 3 {
                if let Some(val) = rx.try_recv() {
                    assert_eq!(val, i);
                    i += 1;
                }
                loom::thread::yield_now();
            }
        })
    }

    #[test]
    fn batched_ops_loom() {
        loom::model(|| {
            let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(2).unwrap());
            let total = 3;

            thread::spawn(move || {
                let mut sent = 0;
                while sent < total {
                    let buf = tx.write_buffer();
                    if !buf.is_empty() {
                        let count = buf.len().min(total - sent);
                        for (i, item) in buf.iter_mut().take(count).enumerate() {
                            item.write(sent + i);
                        }
                        unsafe { tx.commit(count) };
                        sent += count;
                    }
                    loom::thread::yield_now();
                }
            });

            let mut received = 0;
            while received < total {
                let mut guard = rx.read_guard();
                if !guard.is_empty() {
                    let count = guard.len();
                    for (i, item) in guard.as_slice().iter().enumerate() {
                        assert_eq!(*item, received + i);
                    }
                    guard.advance(count);
                    received += count;
                }
                loom::thread::yield_now();
            }
        })
    }
}
