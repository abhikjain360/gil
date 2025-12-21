use std::{num::NonZeroUsize, ptr::NonNull};

use crate::spsc;

mod receiver;
mod sender;

pub use receiver::{ReadGuard, Receiver};
pub use sender::Sender;

pub fn channel<T>(
    max_shards: NonZeroUsize,
    capacity_per_shard: NonZeroUsize,
) -> (sender::Sender<T>, receiver::Receiver<T>) {
    debug_assert_ne!(max_shards.get(), 0, "number of shards must be > 0");
    debug_assert!(
        max_shards.is_power_of_two(),
        "number of shards must be a power of 2"
    );

    let mut shards = Box::new_uninit_slice(max_shards.get());
    for i in 0..max_shards.get() {
        shards[i].write(spsc::QueuePtr::<T>::with_size(capacity_per_shard));
    }
    // SAFETY: Box::new was valid
    let shards = unsafe { NonNull::new_unchecked(Box::into_raw(shards)).cast() };

    // SAFETY: Sender::init(..) will clone, while Receiver::new(..) will move
    (sender::Sender::new(shards, max_shards), unsafe {
        receiver::Receiver::new(shards, max_shards.get())
    })
}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use super::*;

    use crate::thread;

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
    fn multiple_senders_multiple_receivers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        const SENDERS: usize = 4;
        const RECEIVERS: usize = 4;
        const MESSAGES: usize = 1000;

        let (tx, rx) = channel(
            NonZeroUsize::new(SENDERS).unwrap(),
            NonZeroUsize::new(128).unwrap(),
        );
        let total_received = Arc::new(AtomicUsize::new(0));
        let total_sum = Arc::new(AtomicUsize::new(0));

        thread::scope(|s| {
            for t in 0..SENDERS - 1 {
                let mut tx = tx.clone().unwrap();
                s.spawn(move || {
                    for i in 0..MESSAGES {
                        tx.send(t * MESSAGES + i);
                    }
                });
            }
            let mut tx = tx;
            s.spawn(move || {
                for i in 0..MESSAGES {
                    tx.send((SENDERS - 1) * MESSAGES + i);
                }
            });

            for _ in 0..RECEIVERS - 1 {
                let mut rx = rx.clone().unwrap();
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
            let mut rx = rx;
            let total_received = total_received.clone();
            let total_sum = total_sum.clone();
            s.spawn(move || {
                for _ in 0..(SENDERS * MESSAGES / RECEIVERS) {
                    let val = rx.recv();
                    total_received.fetch_add(1, Ordering::SeqCst);
                    total_sum.fetch_add(val, Ordering::SeqCst);
                }
            });
        });

        assert_eq!(total_received.load(Ordering::SeqCst), SENDERS * MESSAGES);
        let n = SENDERS * MESSAGES;
        let expected_sum = n * (n - 1) / 2;
        assert_eq!(total_sum.load(Ordering::SeqCst), expected_sum);
    }

    #[test]
    fn multiple_senders_multiple_receivers_try() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        const SENDERS: usize = 4;
        const RECEIVERS: usize = 4;
        const MESSAGES: usize = 1000;

        let (tx, rx) = channel(
            NonZeroUsize::new(SENDERS).unwrap(),
            NonZeroUsize::new(128).unwrap(),
        );
        let total_received = Arc::new(AtomicUsize::new(0));
        let total_sum = Arc::new(AtomicUsize::new(0));

        thread::scope(|s| {
            for t in 0..SENDERS - 1 {
                let mut tx = tx.clone().unwrap();
                s.spawn(move || {
                    for i in 0..MESSAGES {
                        let mut backoff = crate::Backoff::with_spin_count(1);
                        while tx.try_send(t * MESSAGES + i).is_err() {
                            backoff.backoff();
                        }
                    }
                });
            }
            let mut tx = tx;
            s.spawn(move || {
                let t = SENDERS - 1;
                for i in 0..MESSAGES {
                    let mut backoff = crate::Backoff::with_spin_count(1);
                    while tx.try_send(t * MESSAGES + i).is_err() {
                        backoff.backoff();
                    }
                }
            });

            for _ in 0..RECEIVERS - 1 {
                let mut rx = rx.clone().unwrap();
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
            let mut rx = rx;
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
        });

        assert_eq!(total_received.load(Ordering::SeqCst), SENDERS * MESSAGES);
        let n = SENDERS * MESSAGES;
        let expected_sum = n * (n - 1) / 2;
        assert_eq!(total_sum.load(Ordering::SeqCst), expected_sum);
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
                        for i in 0..batch_size {
                            buffer[i].write(thread_id * 10000 + sent + i);
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
                    for i in 0..batch_size {
                        buffer[i].write(thread_id * 10000 + sent + i);
                    }
                    unsafe { tx.commit(batch_size) };
                    sent += batch_size;
                }
            });

            let mut received_counts = vec![0; SHARDS];
            let mut total_received = 0;
            let total_expected = SHARDS * TOTAL_ITEMS_PER_THREAD;

            while total_received < total_expected {
                let mut buffer = rx.read_buffer();
                if buffer.is_empty() {
                    continue;
                }

                for &value in buffer.iter() {
                    let thread_id = value / 10000;
                    let sent_id = value % 10000;
                    assert_eq!(sent_id, received_counts[thread_id]);
                    received_counts[thread_id] += 1;
                    total_received += 1;
                }

                let count = buffer.len();
                buffer.advance(count);
            }

            assert_eq!(total_received, total_expected);
            for count in received_counts {
                assert_eq!(count, TOTAL_ITEMS_PER_THREAD);
            }
        });
    }
}
