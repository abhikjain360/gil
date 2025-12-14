use std::num::NonZeroUsize;

pub use self::{receiver::Receiver, sender::Sender};

mod cell;
mod queue;
mod receiver;
mod sender;

pub fn channel<T>(capacity: NonZeroUsize) -> (Sender<T>, Receiver<T>) {
    let queue = queue::QueuePtr::with_size(capacity);
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

        let (tx, mut rx) = channel(NonZeroUsize::new(4).unwrap());

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
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

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
        assert_eq!(count, 4, "Expected 4 items to be dropped, but got {}", count);
    }
}
