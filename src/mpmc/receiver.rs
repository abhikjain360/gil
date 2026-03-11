#[cfg(feature = "std")]
use crate::mpmc::queue::FutexState;
use crate::{
    atomic::Ordering,
    mpmc::queue::QueuePtr,
};

/// The consumer end of the MPMC queue.
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
/// let mut tx3 = tx.clone();
/// thread::spawn(move || tx3.send(2));
/// drop(tx);
///
/// let mut rx2 = rx.clone();
/// let a = rx.recv();
/// let b = rx2.recv();
/// assert_eq!(a + b, 3);
/// ```
#[derive(Clone)]
pub struct Receiver<T> {
    ptr: QueuePtr<T>,
    local_head: usize,
}

impl<T> Receiver<T> {
    pub(crate) fn new(queue_ptr: QueuePtr<T>) -> Self {
        Self {
            ptr: queue_ptr,
            local_head: 0,
        }
    }

    /// Receives a value from the queue, blocking if necessary.
    ///
    /// Spins in a loop calling [`try_recv`](Receiver::try_recv) with exponential
    /// backoff (spin limit 6, yield limit 10). For custom limits, use
    /// [`recv_with_spin_count`](Receiver::recv_with_spin_count).
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
    pub fn recv(&mut self) -> T {
        self.recv_with_spin_count(128, 1)
    }

    /// Receives a value from the queue, blocking if necessary, with custom
    /// backoff limits.
    ///
    /// Retries [`try_recv`](Receiver::try_recv) in a loop with exponential backoff
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
    /// tx.send(42);
    /// assert_eq!(rx.recv_with_spin_count(4, 8), 42);
    /// ```
    pub fn recv_with_spin_count(&mut self, spin_limit: u32, yield_limit: u32) -> T {
        let mut backoff = crate::ParkingBackoff::new(spin_limit, yield_limit);
        loop {
            if let Some(ret) = self.try_recv() {
                return ret;
            }
            #[cfg(feature = "std")]
            if backoff.backoff() && self.ptr.prepare_wait(FutexState::ReceiversWaiting) {
                // catch lost wakes
                if let Some(ret) = self.try_recv() {
                    return ret;
                }
                self.ptr.wait(FutexState::ReceiversWaiting);
            }
            #[cfg(not(feature = "std"))]
            backoff.backoff();
        }
    }

    /// Attempts to receive a value from the queue without blocking.
    ///
    /// Uses exponential backoff (spin limit 6, yield limit 10) to handle CAS
    /// contention between consumers. Returns `Some(value)` if a value is
    /// available, or `None` if the queue is empty.
    ///
    /// For custom backoff limits, use
    /// [`try_recv_with_spin_count`](Receiver::try_recv_with_spin_count).
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    ///
    /// assert_eq!(rx.try_recv(), None);
    ///
    /// tx.send(42);
    /// assert_eq!(rx.try_recv(), Some(42));
    /// assert_eq!(rx.try_recv(), None);
    /// ```
    pub fn try_recv(&mut self) -> Option<T> {
        use core::cmp::Ordering as Cmp;

        let mut backoff = crate::ExponentialBackoff::new(6, 10);

        loop {
            let cell = self.ptr.cell_at(self.local_head);
            let epoch = cell.epoch().load(Ordering::Acquire);
            let next_epoch = self.local_head.wrapping_add(1);

            match epoch.cmp(&next_epoch) {
                // producer hasn't written a new value since last read
                Cmp::Less => return None,

                // producer wrote a value
                Cmp::Equal => {
                    match self.ptr.head().compare_exchange_weak(
                        self.local_head,
                        next_epoch,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            let ret = unsafe { cell.get() };
                            cell.epoch().store(
                                self.local_head.wrapping_add(self.ptr.capacity),
                                Ordering::Release,
                            );

                            #[cfg(feature = "std")]
                            self.ptr.wake();

                            self.local_head = next_epoch;
                            return Some(ret);
                        }
                        // we weren't fast enough, some other consumer read from this cell already,
                        // probably
                        Err(cur_head) => self.local_head = cur_head,
                    }
                }

                // some other consumer has read from this cell before us
                Cmp::Greater => self.local_head = self.ptr.head().load(Ordering::Relaxed),
            }

            backoff.backoff();
        }
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
