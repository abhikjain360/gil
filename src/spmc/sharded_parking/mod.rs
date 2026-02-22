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
//! let mut rx2 = rx.clone().expect("shard available");
//!
//! tx.send(1);
//! tx.send(2);
//!
//! let a = rx.recv();
//! let b = rx2.recv();
//! assert_eq!(a + b, 3);
//! ```

use core::num::NonZeroUsize;

use crate::spsc::parking_shards::ParkingShardsPtr;

mod receiver;
mod sender;

pub use receiver::Receiver;
pub use sender::Sender;

/// Creates a new sharded parking single-producer multi-consumer channel.
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

    let shards = ParkingShardsPtr::new(max_shards, capacity_per_shard);

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
        assert!(tx.try_send(99).is_err());

        for i in 0..4 {
            assert_eq!(rx.try_recv(), Some(i));
        }
        assert_eq!(rx.try_recv(), None);
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

        let mut rx2 = rx.clone().unwrap();
        assert!(rx.clone().is_none());
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
