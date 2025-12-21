//! Single-producer single-consumer (SPSC) queue.
//!
//! This is the fastest queue variant, as it requires no atomic synchronization for the data buffer itself,
//! only for the head and tail indices. It is inspired by the `ProducerConsumerQueue` in Facebook's Folly library.
//!
//! # Performance
//!
//! **Improvements over original inspiration:**
//! - **Single Allocation:** The queue metadata (head/tail indices) and the data buffer are allocated
//!   in a single contiguous memory block. This reduces cache misses by keeping related data close in memory.
//! - **False Sharing Prevention:** The head and tail indices are explicitly padded to separate cache lines
//!   to prevent false sharing between the producer and consumer cores.
//!
//! # When to use
//!
//! Use this queue for 1-to-1 thread communication. It offers the best possible throughput and latency.
//!
//! # Reference
//!
//! * [Facebook Folly ProducerConsumerQueue](https://github.com/facebook/folly/blob/main/folly/ProducerConsumerQueue.h)

use std::num::NonZeroUsize;

pub(crate) use self::queue::QueuePtr;
pub(crate) mod shards;
pub use self::{receiver::Receiver, sender::Sender};

mod queue;
mod receiver;
mod sender;

/// Creates a new single-producer single-consumer (SPSC) queue.
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
/// use std::num::NonZeroUsize;
/// use gil::spsc::channel;
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

    #[cfg(feature = "async")]
    #[test]
    fn test_async_send() {
        futures::executor::block_on(async {
            const COUNTS: NonZeroUsize = NonZeroUsize::new(4096).unwrap();

            let (mut tx, mut rx) = channel::<usize>(COUNTS);

            thread::spawn(move || {
                for i in 0..COUNTS.get() << 1 {
                    futures::executor::block_on(tx.send_async(i));
                }
                drop(tx);
            });
            for i in 0..COUNTS.get() << 1 {
                assert_eq!(rx.recv_async().await, i);
            }
        });
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
            let buffer = rx.read_buffer();
            if buffer.is_empty() {
                continue;
            }
            for &value in buffer.iter() {
                assert_eq!(value, expected);
                expected += 1;
            }
            let count = buffer.len();
            unsafe { rx.advance(count) };
            received += count;
        }

        assert_eq!(received, TOTAL_ITEMS);
    }

    #[test]
    fn test_drop_remaining_elements() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

        #[derive(Clone)]
        struct DropCounter(#[allow(dead_code)] Arc<()>);

        impl Drop for DropCounter {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::SeqCst);
            }
        }

        DROP_COUNT.store(0, Ordering::SeqCst);

        {
            let (mut tx, rx) = channel::<DropCounter>(NonZeroUsize::new(16).unwrap());

            // Send 5 items but don't receive them
            for _ in 0..5 {
                tx.send(DropCounter(Arc::new(())));
            }

            // Drop both ends - remaining items should be dropped
            drop(tx);
            drop(rx);
        }

        // All 5 items should have been dropped
        assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 5);
    }
}

#[cfg(all(test, feature = "loom"))]
mod loom_test {
    use std::num::NonZeroUsize;

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
                        for i in 0..count {
                            buf[i].write(sent + i);
                        }
                        unsafe { tx.commit(count) };
                        sent += count;
                    }
                    loom::thread::yield_now();
                }
            });

            let mut received = 0;
            while received < total {
                let buf = rx.read_buffer();
                if !buf.is_empty() {
                    let count = buf.len();
                    for i in 0..count {
                        assert_eq!(buf[i], received + i);
                    }
                    unsafe { rx.advance(count) };
                    received += count;
                }
                loom::thread::yield_now();
            }
        })
    }
}
