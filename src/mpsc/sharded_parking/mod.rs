//! Sharded parking multi-producer single-consumer channel.
//!
//! Like [`super::sharded`], this channel uses multiple SPSC shards to eliminate
//! producer contention. Unlike the spinning variant, senders **park** on a
//! per-shard futexes when their shard is full, and are woken by the receiver after
//! it drains items. This trades a small amount of latency for significantly
//! lower CPU usage when senders are frequently blocked.
//!
//! # Examples
//!
//! ```
//! use std::thread;
//! use core::num::NonZeroUsize;
//! use gil::mpsc::sharded_parking::channel;
//!
//! let (mut tx, mut rx) = channel::<usize>(
//!     NonZeroUsize::new(4).unwrap(),
//!     NonZeroUsize::new(256).unwrap(),
//! );
//!
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

use core::num::NonZeroUsize;

use crate::spsc::parking_shards::ParkingShardsPtr;

mod receiver;
mod sender;

pub use receiver::Receiver;
pub use sender::Sender;

/// Creates a new sharded parking multi-producer single-consumer channel.
///
/// # Arguments
///
/// * `max_shards` - The maximum number of shards (must be a power of two).
/// * `capacity_per_shard` - The capacity of each individual shard.
///
/// # Returns
///
/// A tuple containing a [`Sender`] and a [`Receiver`].
pub fn channel<T>(
    max_shards: NonZeroUsize,
    capacity_per_shard: NonZeroUsize,
) -> (Sender<T>, Receiver<T>) {
    debug_assert!(
        max_shards.is_power_of_two(),
        "number of shards must be a power of 2"
    );

    let shards = ParkingShardsPtr::new(max_shards, capacity_per_shard);

    // The receiver owns the consumer side of every shard. Claim those slots
    // before creating the first sender so producer ownership is the only
    // fallible slot acquisition left for sender cloning/reuse.
    let receiver = Receiver::new(shards.clone(), max_shards.get());
    let sender = Sender::new(shards, max_shards);

    (sender, receiver)
}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use super::*;

    use crate::thread;
    use alloc_crate::{vec, vec::Vec};

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
    fn sender_clone_reuses_dropped_shard() {
        let (tx0, mut rx) =
            channel::<usize>(NonZeroUsize::new(2).unwrap(), NonZeroUsize::new(4).unwrap());
        let mut tx1 = tx0.clone().unwrap();

        tx1.send(1);
        assert_eq!(rx.recv(), 1);

        drop(tx0);

        let mut tx2 = tx1.clone().unwrap();
        assert!(tx1.clone().is_none());

        tx2.try_send(2).unwrap();
        assert_eq!(rx.try_recv(), Some(2));
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

    #[test]
    fn shard_futex_wakes_sender_for_freed_shard() {
        use std::sync::mpsc::channel as std_channel;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use std::time::{Duration, Instant};

        const SHARDS: usize = 8;
        const ORDERS: [[usize; SHARDS]; 4] = [
            [1, 2, 3, 0, 4, 5, 6, 7],
            [7, 6, 5, 0, 4, 3, 2, 1],
            [2, 4, 6, 0, 1, 3, 5, 7],
            [7, 5, 3, 0, 6, 4, 2, 1],
        ];

        for attempt in 0..10 {
            let (tx0, mut rx) = channel::<usize>(
                NonZeroUsize::new(SHARDS).unwrap(),
                NonZeroUsize::new(1).unwrap(),
            );

            let mut senders = Vec::new();
            senders.push(tx0);
            for shard in 1..SHARDS {
                let tx = senders[shard - 1].clone().unwrap();
                senders.push(tx);
            }

            for (shard, tx) in senders.iter_mut().enumerate() {
                tx.send(shard * 10);
            }
            let mut senders: Vec<_> = senders.into_iter().map(Some).collect();

            let (done_tx, done_rx) = std_channel();
            let ready = Arc::new(AtomicUsize::new(0));
            let mut handles = Vec::new();
            for shard in ORDERS[attempt % ORDERS.len()] {
                let mut tx = senders[shard].take().unwrap();
                let done_tx = done_tx.clone();
                let ready = ready.clone();
                handles.push(thread::spawn(move || {
                    let value = shard * 10 + 1;
                    assert_eq!(tx.try_send(value), Err(value));
                    ready.fetch_add(1, Ordering::Release);
                    tx.send(value);
                    done_tx.send(shard).unwrap();
                }));
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(done_tx);

            while ready.load(Ordering::Acquire) != SHARDS {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(100));

            assert_eq!(rx.recv(), 0);
            let first_woken = done_rx.recv_timeout(Duration::from_millis(50));
            let target_woke = first_woken == Ok(0);

            let mut completed = [false; SHARDS];
            if let Ok(shard) = first_woken {
                completed[shard] = true;
            }

            let cleanup_started = Instant::now();
            while !completed.iter().all(|done| *done)
                && cleanup_started.elapsed() < Duration::from_secs(1)
            {
                while rx.try_recv().is_some() {}
                while let Ok(id) = done_rx.try_recv() {
                    completed[id] = true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            if completed.iter().all(|done| *done) {
                for handle in handles {
                    handle.join().unwrap();
                }
            }

            assert!(
                target_woke,
                "attempt {attempt}: freeing shard 0 did not wake the sender bound to shard 0; first completion: {first_woken:?}, completed: {completed:?}"
            );
        }
    }

    #[test]
    fn test_sender_parks_and_wakes() {
        let (mut tx, mut rx) =
            channel::<usize>(NonZeroUsize::new(1).unwrap(), NonZeroUsize::new(2).unwrap());

        // Fill the queue
        tx.send(1);
        tx.send(2);

        // Sender will need to park — spawn it on another thread
        let h = thread::spawn(move || {
            tx.send(3); // This should park and be woken by receiver
            tx.send(4);
        });

        // Small delay to let sender park
        std::thread::sleep(std::time::Duration::from_millis(10));

        assert_eq!(rx.recv(), 1);
        assert_eq!(rx.recv(), 2);
        assert_eq!(rx.recv(), 3);
        assert_eq!(rx.recv(), 4);

        h.join().unwrap();
    }
}
