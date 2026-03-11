use core::cmp::Ordering as Cmp;

use crate::{atomic::Ordering, mpmc::queue::QueuePtr};

/// The producer end of the MPMC queue.
///
/// This struct is `Clone` and `Send`. It can be shared across threads by cloning it.
///
/// # Examples
///
/// ```
/// use std::thread;
/// use core::num::NonZeroUsize;
/// use gil::mpmc::channel;
///
/// let (tx, mut rx) = channel::<i32>(NonZeroUsize::new(1024).unwrap());
///
/// let mut tx2 = tx.clone();
/// thread::spawn(move || tx2.send(1));
///
/// let mut tx3 = tx.clone();
/// thread::spawn(move || tx3.send(2));
/// drop(tx);
///
/// let mut values = [rx.recv(), rx.recv()];
/// values.sort();
/// assert_eq!(values, [1, 2]);
/// ```
#[derive(Clone)]
pub struct Sender<T> {
    ptr: QueuePtr<T>,
    local_tail: usize,
}

impl<T> Sender<T> {
    pub(crate) fn new(queue_ptr: QueuePtr<T>) -> Self {
        Self {
            ptr: queue_ptr,
            local_tail: 0,
        }
    }

    /// Sends a value into the queue, blocking if necessary.
    ///
    /// Spins in a loop calling [`try_send`](Sender::try_send) with exponential
    /// backoff (spin limit 6, yield limit 10). For custom limits, use
    /// [`send_with_spin_count`](Sender::send_with_spin_count).
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send(42);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn send(&mut self, value: T) {
        self.send_with_spin_count(value, 6, 10);
    }

    /// Sends a value into the queue, blocking if necessary, with custom backoff limits.
    ///
    /// Retries [`try_send`](Sender::try_send) in a loop with exponential backoff
    /// between attempts. `spin_limit` controls how many doubling spin phases
    /// occur before yielding, and `yield_limit` controls the total number of
    /// backoff steps before the backoff resets.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send_with_spin_count(42, 4, 8);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn send_with_spin_count(&mut self, mut value: T, spin_limit: u32, yield_limit: u32) {
        let mut backoff = crate::ExponentialBackoff::new(spin_limit, yield_limit);
        loop {
            match self.try_send(value) {
                Ok(()) => return,
                Err(ret) => {
                    value = ret;
                    if backoff.backoff() {
                        backoff.reset();
                    }
                }
            }
        }
    }

    /// Attempts to send a value into the queue without blocking.
    ///
    /// Uses exponential backoff (spin limit 6, yield limit 10) to handle CAS
    /// contention between producers. Returns `Ok(())` if the value was
    /// successfully enqueued, or `Err(value)` if the queue is full.
    ///
    /// For custom backoff limits, use
    /// [`try_send_with_spin_count`](Sender::try_send_with_spin_count).
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(2).unwrap());
    ///
    /// assert!(tx.try_send(1).is_ok());
    /// assert!(tx.try_send(2).is_ok());
    /// assert_eq!(tx.try_send(3), Err(3));
    ///
    /// assert_eq!(rx.recv(), 1);
    /// ```
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        let mut backoff = crate::ExponentialBackoff::new(6, 10);

        let cell = loop {
            let cell = self.ptr.cell_at(self.local_tail);
            let epoch = cell.epoch().load(Ordering::Acquire);

            match epoch.cmp(&self.local_tail) {
                // consumer hasn't read the value
                Cmp::Less => return Err(value),

                // consumer has read the value, cell is free
                Cmp::Equal => {
                    let next_epoch = self.local_tail.wrapping_add(1);
                    match self.ptr.tail().compare_exchange_weak(
                        self.local_tail,
                        next_epoch,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            self.local_tail = next_epoch;
                            break cell;
                        }
                        // we weren't fast enough, some other producer wrote to this cell already,
                        // probably
                        Err(cur_tail) => self.local_tail = cur_tail,
                    }
                }

                // some other producer has written to this cell before us
                Cmp::Greater => self.local_tail = self.ptr.tail().load(Ordering::Relaxed),
            };

            backoff.backoff();
        };

        cell.set(value);
        cell.epoch().store(self.local_tail, Ordering::Release);

        Ok(())
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
