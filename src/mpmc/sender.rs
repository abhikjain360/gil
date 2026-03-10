use core::cmp::Ordering as Cmp;

use crate::{
    atomic::Ordering,
    mpmc::queue::{FutexState, QueuePtr},
};

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
    /// This method uses a spin loop with a default spin count of 128 to wait
    /// for available space in the queue. For control over the spin count, use
    /// [`Sender::send_with_spin_count`]. For a non-blocking alternative, use
    /// [`Sender::try_send`].
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
    pub fn send2(&mut self, value: T) {
        self.send_with_spin_count(value, 16);
    }

    /// Sends a value into the queue, blocking if necessary, using a custom spin count.
    ///
    /// The `spin_count` controls how many times the backoff spins before yielding
    /// the thread. A higher value keeps the thread spinning longer, which can reduce
    /// latency when the queue is expected to drain quickly, at the cost of higher CPU
    /// usage. A lower value yields sooner, reducing CPU usage but potentially
    /// increasing latency.
    ///
    /// For a non-blocking alternative, use [`Sender::try_send`].
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send_with_spin_count(42, 32);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn send_with_spin_count(&mut self, value: T, spin_count: u32) {
        // fetch_add means we are the only ones who can access the cell at this idx
        let tail = self.ptr.tail().fetch_add(1, Ordering::Relaxed);
        let next = tail.wrapping_add(1);

        let cell = self.ptr.cell_at(tail);
        let mut backoff = crate::ParkingBackoff::new(spin_count, 1);
        while cell.epoch().load(Ordering::Acquire) != tail {
            if backoff.backoff() && cell.prepare_wait() {
                // don't lose wakes
                if cell.epoch().load(Ordering::Acquire) == tail {
                    break;
                }
                cell.wait();
            }
        }

        cell.set(value);
        cell.epoch().store(next, Ordering::Release);
        cell.wake();

        self.local_tail = next;
    }

    pub fn send(&mut self, mut value: T) {
        while let Err(ret) = self.try_send(value) {
            value = ret;
            if self.ptr.prepare_wait(FutexState::SendersWaiting) {
                // catch lost wakeups
                if let Err(ret) = self.try_send(value) {
                    value = ret;
                    self.ptr.wait(FutexState::SendersWaiting);
                    continue;
                }
                break;
            }
        }
        self.ptr.wake();
    }

    /// Attempts to send a value into the queue without blocking.
    ///
    /// Returns `Ok(())` if the value was successfully enqueued, or `Err(value)` if the
    /// queue is full, returning the original value.
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
        let mut backoff = crate::Backoff::with_spin_count(16);

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
        // cell.wake();

        Ok(())
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
