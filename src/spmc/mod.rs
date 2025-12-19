use std::num::NonZeroUsize;

pub use self::{receiver::Receiver, sender::Sender};

mod queue;
mod receiver;
mod sender;

/// Creates a new single-producer multi-consumer (SPMC) queue.
///
/// The queue has a fixed capacity and is lock-free.
///
/// # Arguments
///
/// * `capacity` - The capacity of the queue.
///
/// # Returns
///
/// A tuple containing the [`Sender`] and [`Receiver`] handles.
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
