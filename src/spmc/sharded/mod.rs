//! Sharded single-producer multi-consumer channel.
//!
//! The sender writes to shards in strict round-robin fashion, ensuring even
//! distribution. Each receiver is bound to a single shard and spins/yields
//! while waiting for data.
//!
//! # Examples
//!
//! ```
//! use std::thread;
//! use core::num::NonZeroUsize;
//! use gil::spmc::sharded::channel;
//!
//! let (mut tx, mut rx) = channel::<usize>(
//!     NonZeroUsize::new(2).unwrap(),
//!     NonZeroUsize::new(256).unwrap(),
//! );
//!
//! let mut rx2 = rx.clone().expect("shard available");
//!
//! tx.send(1);
//! tx.send(2);
//!
//! // Strict round-robin: first item goes to shard 0, second to shard 1
//! let a = rx.recv();
//! let b = rx2.recv();
//! assert_eq!(a + b, 3);
//! ```

use core::num::NonZeroUsize;

use crate::spsc::shards::ShardsPtr;

mod receiver;
mod sender;

pub use receiver::Receiver;
pub use sender::Sender;

/// Creates a new sharded single-producer multi-consumer channel.
///
/// # Arguments
///
/// * `max_shards` - The maximum number of shards (must be a power of two).
/// * `capacity_per_shard` - The capacity of each individual shard.
pub fn channel<T>(
    max_shards: NonZeroUsize,
    capacity_per_shard: NonZeroUsize,
) -> (Sender<T>, Receiver<T>) {
    debug_assert!(
        max_shards.is_power_of_two(),
        "number of shards must be a power of 2"
    );

    let shards = ShardsPtr::new(max_shards, capacity_per_shard);

    (
        Sender::new(shards.clone(), max_shards.get()),
        Receiver::new(shards, max_shards),
    )
}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use super::*;

    use crate::thread;

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
                let mut rx = rx.clone().unwrap();
                scope.spawn(move || {
                    for _ in 0..ITER {
                        let _ = rx.recv();
                    }
                });
            }
            let mut rx = rx;
            scope.spawn(move || {
                for _ in 0..ITER {
                    let _ = rx.recv();
                }
            });

            // Strict round-robin: each shard gets exactly ITER items
            for i in 0..THREADS * ITER {
                tx.send(i);
            }
        });
    }

    #[test]
    fn test_try_ops() {
        let (mut tx, mut rx) =
            channel::<usize>(NonZeroUsize::new(1).unwrap(), NonZeroUsize::new(4).unwrap());

        assert_eq!(rx.try_recv(), None);

        for i in 0..4 {
            tx.try_send(i).unwrap();
        }
        // Shard full
        assert!(tx.try_send(99).is_err());

        for i in 0..4 {
            assert_eq!(rx.try_recv(), Some(i));
        }
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn test_batched() {
        const CAPACITY: usize = 256;
        const TOTAL: usize = 1024;

        let (mut tx, mut rx) =
            channel::<usize>(NonZeroUsize::new(1).unwrap(), NonZeroUsize::new(CAPACITY).unwrap());

        thread::scope(|scope| {
            scope.spawn(move || {
                let mut sent = 0;
                while sent < TOTAL {
                    let buf = tx.write_buffer();
                    let batch = buf.len().min(TOTAL - sent);
                    for i in 0..batch {
                        buf[i].write(sent + i);
                    }
                    unsafe { tx.commit(batch) };
                    sent += batch;
                }
            });

            let mut received = 0;
            while received < TOTAL {
                let mut guard = rx.read_guard();
                if guard.is_empty() {
                    continue;
                }
                for (i, &val) in guard.as_slice().iter().enumerate() {
                    assert_eq!(val, received + i);
                }
                let count = guard.len();
                guard.advance(count);
                received += count;
            }
        });
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

    #[test]
    fn test_multi_receiver() {
        let (mut tx, rx) = channel::<usize>(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(64).unwrap(),
        );

        let mut rx2 = rx.clone().unwrap();
        // Third clone should fail (only 2 shards)
        assert!(rx.clone().is_none());

        let mut rx = rx;

        // Send 10 items — strict round-robin: even indices to shard 0, odd to shard 1
        for i in 0..10 {
            tx.send(i);
        }

        // rx (shard 0) gets items 0, 2, 4, 6, 8
        for i in 0..5 {
            assert_eq!(rx.recv(), i * 2);
        }
        // rx2 (shard 1) gets items 1, 3, 5, 7, 9
        for i in 0..5 {
            assert_eq!(rx2.recv(), i * 2 + 1);
        }
    }
}
