//! Single-producer multi-consumer (SPMC) queue.
//!
//! This queue is an adaptation of Dmitry Vyukov's bounded MPMC queue, optimized for a single producer.
//!
//! # Examples
//!
//! ```
//! use std::thread;
//! use core::num::NonZeroUsize;
//! use gil::spmc::channel;
//!
//! let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(1024).unwrap());
//! let mut rx2 = rx.clone();
//!
//! tx.send(1);
//! tx.send(2);
//!
//! let a = rx.recv();
//! let b = rx2.recv();
//! assert_eq!(a + b, 3);
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
//! Use this queue when you have a single thread distributing work to multiple consumer threads.
//! It avoids the overhead of multi-producer synchronization.
//!
//! # Gotchas
//!
//! - **Cloneability:** [`Receiver`] implements `Clone`, but [`Sender`] does not. This is the
//!   opposite of MPSC. Clone receivers to distribute to multiple consumer threads.
//! - **No Async:** Unlike SPSC, this queue does not have async support.
//! - **No Batch Operations:** This queue does not support batch operations.
//! - **Capacity Rounding:** The actual capacity is rounded up to the next power of two.
//!
//! # Reference
//!
//! * Adapted from [Dmitry Vyukov's Bounded MPMC Queue](http://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue)

use core::num::NonZeroUsize;

pub use self::{receiver::Receiver, sender::Sender};

mod queue;
mod receiver;
mod sender;
pub mod sharded;
pub mod sharded_parking;

/// Creates a new single-producer multi-consumer (SPMC) queue.
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
/// use gil::spmc::channel;
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

        let (mut tx, rx) = channel(NonZeroUsize::new(4).unwrap());

        thread::scope(move |scope| {
            for _ in 0..THREADS {
                let mut rx = rx.clone();
                scope.spawn(move || {
                    let mut sum = 0;
                    for _ in 0..ITER {
                        let (_, i) = rx.recv();
                        sum += i;
                    }
                    assert!(sum > 0 || ITER == 0);
                });
            }

            for thread_id in 0..THREADS {
                for i in 0..ITER {
                    tx.send((thread_id, i));
                }
            }
        });
    }

    #[test]
    fn test_valid_try_receives() {
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
    }
}
