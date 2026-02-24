//! Sharded multi-producer single-consumer channel.
//!
//! The channel is composed of multiple shards, each being a single-producer single-consumer (SPSC) queue.
//! This structure allows for high throughput when multiple threads are sending concurrently,
//! as they will each use a different shard.
//!
//! # Examples
//!
//! ```
//! use std::thread;
//! use core::num::NonZeroUsize;
//! use gil::mpsc::sharded::channel;
//!
//! let (mut tx, mut rx) = channel::<usize>(
//!     NonZeroUsize::new(4).unwrap(),   // 4 shards
//!     NonZeroUsize::new(256).unwrap(), // 256 capacity per shard
//! );
//!
//! // Each clone binds to a different shard
//! let mut tx2 = tx.clone().expect("shard available");
//! let h = thread::spawn(move || tx2.send(1));
//!
//! tx.send(2);
//!
//! let mut values = [rx.recv(), rx.recv()];
//! values.sort();
//! assert_eq!(values, [1, 2]);
//! h.join().unwrap();
//! ```
//!
//! # Performance Implications
//!
//! **Sharded vs. Normal MPSC:**
//!
//! *   **Reduced Contention:** Standard MPSC queues have a single contention point (the tail) for all producers.
//!     This sharded implementation gives each producer (up to `max_shards`) its own dedicated SPSC queue to write to.
//!     This eliminates producer-side contention entirely.
//! *   **Consumer Overhead:** The consumer must poll all shards. This adds slight overhead compared to reading
//!     from a single queue, but is usually outweighed by the throughput gains from contention-free sending.
//! *   **Trade-offs:**
//!     *   **Bounded Producers:** The number of concurrent producers is limited by `max_shards`.
//!     *   **Memory:** Higher memory usage due to multiple buffers.
//!     *   **Ordering:** Messages are FIFO within a shard, but there is no strict ordering between messages
//!         sent to different shards.
//!
//! # Gotchas
//!
//! - **Fallible Clone:** `Sender::clone()` returns `Option<Sender>`. It returns `None` if all shards
//!   are already occupied. Always handle this case in production code.
//! - **Power of Two:** `max_shards` must be a power of two.
//! - **Batch Operations:** This variant supports batch operations via `write_buffer`/`commit` and
//!   `read_buffer`/`advance`.

use core::num::NonZeroUsize;

use crate::spsc::shards::ShardsPtr;

mod receiver;
mod sender;

pub use receiver::Receiver;
pub use sender::Sender;

/// Creates a new sharded multi-producer single-consumer channel.
///
/// See the [module-level documentation](self) for more details on performance implications.
///
/// # Arguments
///
/// * `max_shards` - The maximum number of shards to create. This must be a power of two.
/// * `capacity_per_shard` - The capacity of each individual shard.
///
/// # Returns
///
/// A tuple containing a [`Sender`] and a [`Receiver`].
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::mpsc::sharded::channel;
///
/// let (tx, rx) = channel::<usize>(
///     NonZeroUsize::new(4).unwrap(),
///     NonZeroUsize::new(256).unwrap(),
/// );
/// ```
pub fn channel<T>(
    max_shards: NonZeroUsize,
    capacity_per_shard: NonZeroUsize,
) -> (sender::Sender<T>, receiver::Receiver<T>) {
    debug_assert_ne!(max_shards.get(), 0, "number of shards must be > 0");
    debug_assert!(
        max_shards.is_power_of_two(),
        "number of shards must be a power of 2"
    );

    let shards = ShardsPtr::new(max_shards, capacity_per_shard);

    (
        sender::Sender::new(shards.clone(), max_shards),
        receiver::Receiver::new(shards, max_shards.get()),
    )
}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use super::*;

    use crate::thread;
    use alloc_crate::vec;

    #[test]
    fn basic() {
        const THREADS: u32 = 8;
        const ITER: u32 = 10;

        let (mut tx, mut rx) = channel(
            NonZeroUsize::new(THREADS as usize).unwrap(),
            NonZeroUsize::new(4).unwrap(),
        );

        thread::scope(move |scope| {
            for thread_id in 0..THREADS - 1 {
                let mut tx = tx.clone().unwrap();
                scope.spawn(move || {
                    for i in 0..ITER {
                        tx.send((thread_id, i));
                    }
                });
            }
            scope.spawn(move || {
                for i in 0..ITER {
                    tx.send((THREADS - 1, i));
                }
            });

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
        let (mut tx, mut rx) =
            channel::<usize>(NonZeroUsize::new(1).unwrap(), NonZeroUsize::new(4).unwrap());
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
            let (mut tx, _rx) = channel::<DropCounter>(
                NonZeroUsize::new(1).unwrap(),
                NonZeroUsize::new(4).unwrap(),
            );

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

    #[test]
    fn test_batched_sharded_send_recv() {
        const SHARDS: usize = 4;
        const CAPACITY_PER_SHARD: usize = 256;
        const TOTAL_ITEMS_PER_THREAD: usize = 1024;

        let (mut tx, mut rx) = channel(
            NonZeroUsize::new(SHARDS).unwrap(),
            NonZeroUsize::new(CAPACITY_PER_SHARD).unwrap(),
        );

        thread::scope(|scope| {
            for thread_id in 0..SHARDS - 1 {
                let mut tx = tx.clone().unwrap();
                scope.spawn(move || {
                    let mut sent = 0;
                    while sent < TOTAL_ITEMS_PER_THREAD {
                        let buffer = tx.write_buffer();
                        let batch_size = buffer.len().min(TOTAL_ITEMS_PER_THREAD - sent);
                        for (i, slot) in buffer.iter_mut().enumerate().take(batch_size) {
                            slot.write(thread_id * 10000 + sent + i);
                        }
                        unsafe { tx.commit(batch_size) };
                        sent += batch_size;
                    }
                });
            }

            scope.spawn(move || {
                let thread_id = SHARDS - 1;
                let mut sent = 0;
                while sent < TOTAL_ITEMS_PER_THREAD {
                    let buffer = tx.write_buffer();
                    let batch_size = buffer.len().min(TOTAL_ITEMS_PER_THREAD - sent);
                    for (i, slot) in buffer.iter_mut().enumerate().take(batch_size) {
                        slot.write(thread_id * 10000 + sent + i);
                    }
                    unsafe { tx.commit(batch_size) };
                    sent += batch_size;
                }
            });

            let mut received_counts = vec![0; SHARDS];
            let mut total_received = 0;
            let total_expected = SHARDS * TOTAL_ITEMS_PER_THREAD;

            while total_received < total_expected {
                let mut guard = rx.read_guard();
                if guard.is_empty() {
                    continue;
                }

                for &value in guard.as_slice() {
                    let thread_id = value / 10000;
                    let sent_id = value % 10000;
                    assert_eq!(sent_id, received_counts[thread_id]);
                    received_counts[thread_id] += 1;
                    total_received += 1;
                }

                let count = guard.len();
                guard.advance(count);
            }

            assert_eq!(total_received, total_expected);
            for count in received_counts {
                assert_eq!(count, TOTAL_ITEMS_PER_THREAD);
            }
        });
    }
}
