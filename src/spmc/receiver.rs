use crate::{atomic::Ordering, spmc::queue::QueuePtr};

/// The consumer end of the SPMC queue.
///
/// Unlike SPSC receivers, this struct implements `Clone`, allowing multiple consumers to receive
/// from the same queue. Each consumer competes for items - an item will be received by exactly
/// one consumer.
///
/// This struct is `Send` but not `Sync`. It can be moved to another thread, but cannot be shared
/// across threads without cloning.
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::spmc::channel;
///
/// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
/// let mut rx2 = rx.clone();
///
/// tx.send(1);
/// tx.send(2);
///
/// let a = rx.recv();
/// let b = rx2.recv();
/// assert_eq!(a + b, 3);
/// ```
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
    /// When multiple receivers exist, each call to `recv` competes with other receivers.
    /// Exactly one receiver will get each item.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spmc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send(42);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn recv(&mut self) -> T {
        self.recv_with_spin_count(6, 10)
    }

    /// Receives a value from the queue, blocking if necessary, with custom
    /// backoff limits.
    ///
    /// Retries [`try_recv`](Receiver::try_recv) in a loop with exponential backoff
    /// between attempts. `spin_limit` controls how many doubling spin phases
    /// occur before yielding, and `yield_limit` controls the total number of
    /// backoff steps before the backoff resets.
    ///
    /// When multiple receivers exist, each call competes with other receivers.
    /// Exactly one receiver will get each item.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spmc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send(42);
    /// assert_eq!(rx.recv_with_spin_count(4, 8), 42);
    /// ```
    pub fn recv_with_spin_count(&mut self, spin_limit: u32, yield_limit: u32) -> T {
        let mut backoff = crate::ExponentialBackoff::new(spin_limit, yield_limit);
        loop {
            if let Some(ret) = self.try_recv() {
                return ret;
            }
            if backoff.backoff() {
                backoff.reset();
            }
        }
    }

    /// Attempts to receive a value from the queue without blocking.
    ///
    /// Returns `Some(value)` if a value was successfully received, or `None` if the queue
    /// is empty or another receiver claimed the item.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spmc::channel;
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
            let next_head = self.local_head.wrapping_add(1);

            match epoch.cmp(&next_head) {
                Cmp::Less => return None,
                Cmp::Equal => {
                    match self.ptr.head().compare_exchange_weak(
                        self.local_head,
                        next_head,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            let ret = unsafe { cell.get() };
                            cell.epoch().store(
                                self.local_head.wrapping_add(self.ptr.capacity),
                                Ordering::Release,
                            );
                            self.local_head = next_head;
                            return Some(ret);
                        }
                        Err(cur_head) => self.local_head = cur_head,
                    }
                }
                Cmp::Greater => self.local_head = self.ptr.head().load(Ordering::Relaxed),
            }

            backoff.backoff();
        }
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self {
            ptr: self.ptr.clone(),
            local_head: self.ptr.head().load(Ordering::Relaxed),
        }
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
