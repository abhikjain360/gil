//! Sharded parking single-producer multi-consumer channel.
//!
//! Like [`super::sharded`], the sender distributes items in strict round-robin
//! across shards. Unlike the spinning variant, receivers **park** on a shared
//! futex when their shard is empty, and are woken by the sender after writing.
//!
//! # Examples
//!
//! ```
//! use std::thread;
//! use core::num::NonZeroUsize;
//! use gil::spmc::sharded_parking::channel;
//!
//! let (mut tx, mut rx) = channel::<usize>(
//!     NonZeroUsize::new(2).unwrap(),
//!     NonZeroUsize::new(256).unwrap(),
//! );
//!
//! let mut rx2 = rx.try_clone().expect("shard available");
//!
//! tx.send(1);
//! tx.send(2);
//!
//! let a = rx.recv();
//! let b = rx2.recv();
//! assert_eq!(a + b, 3);
//! ```

use core::num::NonZeroUsize;

use crate::shard_table::ShardTable;

mod receiver;
mod sender;

pub use receiver::Receiver;
pub use sender::Sender;

/// Creates a new sharded parking single-producer multi-consumer channel.
///
/// # Arguments
///
/// * `max_shards` - The maximum number of shards.
/// * `capacity_per_shard` - The capacity of each individual shard.
pub fn channel<T>(
    max_shards: NonZeroUsize,
    capacity_per_shard: NonZeroUsize,
) -> (Sender<T>, Receiver<T>) {
    let table = ShardTable::new(max_shards, capacity_per_shard);

    let sender = Sender::new(&table);
    let receiver = Receiver::new(table);

    (sender, receiver)
}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use super::*;

    use crate::thread;
    use alloc_crate::vec::Vec;

    #[test]
    fn basic() {
        const THREADS: u32 = 4;
        const ITER: u32 = 100;

        let (mut tx, rx) = channel(
            NonZeroUsize::new(THREADS as usize).unwrap(),
            NonZeroUsize::new(16).unwrap(),
        );

        thread::scope(move |scope| {
            for _ in 0..THREADS - 1 {
                let mut rx = rx.try_clone().unwrap();
                scope.spawn(move || {
                    for _ in 0..ITER {
                        _ = rx.recv();
                    }
                });
            }
            let mut rx = rx;
            scope.spawn(move || {
                for _ in 0..ITER {
                    _ = rx.recv();
                }
            });

            for i in 0..THREADS * ITER {
                tx.send(i);
            }
        });
    }

    #[test]
    fn receiver_try_clone_reuses_dropped_shard() {
        let (mut tx, mut rx0) =
            channel::<usize>(NonZeroUsize::new(2).unwrap(), NonZeroUsize::new(4).unwrap());
        let mut rx1 = rx0.try_clone().unwrap();

        tx.send(0);
        tx.send(1);
        assert_eq!(rx0.recv(), 0);
        assert_eq!(rx1.recv(), 1);

        drop(rx0);

        let mut rx2 = rx1.try_clone().unwrap();
        assert!(rx1.try_clone().is_none());

        tx.send(2);
        assert_eq!(rx2.try_recv(), Some(2));
    }

    #[test]
    fn test_try_ops() {
        let (mut tx, mut rx) =
            channel::<usize>(NonZeroUsize::new(1).unwrap(), NonZeroUsize::new(4).unwrap());

        assert_eq!(rx.try_recv(), None);

        for i in 0..4 {
            tx.try_send(i).unwrap();
        }
        assert!(tx.try_send(99).is_err());

        for i in 0..4 {
            assert_eq!(rx.try_recv(), Some(i));
        }
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn shard_futex_wakes_receiver_for_written_shard() {
        use std::sync::mpsc::channel as std_channel;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use std::time::Duration;

        const SHARDS: usize = 8;
        const ORDERS: [[usize; SHARDS]; 4] = [
            [1, 2, 3, 0, 4, 5, 6, 7],
            [7, 6, 5, 0, 4, 3, 2, 1],
            [2, 4, 6, 0, 1, 3, 5, 7],
            [7, 5, 3, 0, 6, 4, 2, 1],
        ];

        for attempt in 0..10 {
            let (mut tx, rx0) = channel::<usize>(
                NonZeroUsize::new(SHARDS).unwrap(),
                NonZeroUsize::new(1).unwrap(),
            );

            let mut receivers = Vec::new();
            receivers.push(rx0);
            for shard in 1..SHARDS {
                let rx = receivers[shard - 1].try_clone().unwrap();
                receivers.push(rx);
            }
            let mut receivers: Vec<_> = receivers.into_iter().map(Some).collect();

            let (done_tx, done_rx) = std_channel();
            let ready = Arc::new(AtomicUsize::new(0));
            let mut handles = Vec::new();
            for shard in ORDERS[attempt % ORDERS.len()] {
                let mut rx = receivers[shard].take().unwrap();
                let done_tx = done_tx.clone();
                let ready = ready.clone();
                handles.push(thread::spawn(move || {
                    assert_eq!(rx.try_recv(), None);
                    ready.fetch_add(1, Ordering::Release);
                    let value = rx.recv();
                    done_tx.send((shard, value)).unwrap();
                }));
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(done_tx);

            while ready.load(Ordering::Acquire) != SHARDS {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(100));

            tx.send(0);
            let first_woken = done_rx.recv_timeout(Duration::from_millis(50));
            let target_woke = first_woken == Ok((0, 0));

            for shard in 1..SHARDS {
                tx.send(shard * 10);
            }

            let mut completed = [false; SHARDS];
            if let Ok((shard, _value)) = first_woken {
                completed[shard] = true;
            }
            let cleanup_started = std::time::Instant::now();
            while !completed.iter().all(|done| *done)
                && cleanup_started.elapsed() < Duration::from_secs(1)
            {
                while let Ok((shard, _value)) = done_rx.try_recv() {
                    completed[shard] = true;
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
                "attempt {attempt}: writing to shard 0 did not wake the receiver bound to shard 0; first completion: {first_woken:?}, completed: {completed:?}"
            );
        }
    }

    #[test]
    fn test_receiver_parks_and_wakes() {
        let (mut tx, mut rx) = channel::<usize>(
            NonZeroUsize::new(1).unwrap(),
            NonZeroUsize::new(16).unwrap(),
        );

        // Spawn receiver that will need to park
        let h = thread::spawn(move || {
            let val = rx.recv();
            assert_eq!(val, 42);
        });

        // Delay to let receiver park
        std::thread::sleep(std::time::Duration::from_millis(10));
        tx.send(42);

        h.join().unwrap();
    }

    #[test]
    fn test_multi_receiver() {
        let (mut tx, rx) = channel::<usize>(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(64).unwrap(),
        );

        let mut rx2 = rx.try_clone().unwrap();
        assert!(rx.try_clone().is_none());
        let mut rx = rx;

        for i in 0..10 {
            tx.send(i);
        }

        // Strict round-robin: even to shard 0, odd to shard 1
        for i in 0..5 {
            assert_eq!(rx.recv(), i * 2);
        }
        for i in 0..5 {
            assert_eq!(rx2.recv(), i * 2 + 1);
        }
    }

    #[test]
    fn test_drop_remaining() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct DropCounter(Arc<AtomicUsize>);
        impl Drop for DropCounter {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicUsize::new(0));
        {
            let (mut tx, _rx) = channel::<DropCounter>(
                NonZeroUsize::new(1).unwrap(),
                NonZeroUsize::new(8).unwrap(),
            );
            for _ in 0..5 {
                tx.send(DropCounter(dropped.clone()));
            }
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 5);
    }
}
