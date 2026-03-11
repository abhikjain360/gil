use crate::{atomic::Ordering, mpmc::queue::QueuePtr};

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
    /// This method uses a spin loop with a default spin count of 128 to wait
    /// for available data in the queue. For control over the spin count, use
    /// [`Receiver::recv_with_spin_count`]. For a non-blocking alternative, use
    /// [`Receiver::try_recv`].
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
        self.recv_with_spin_count(6)
    }

    /// Receives a value from the queue, blocking if necessary, using a custom spin count.
    ///
    /// The `spin_count` controls how many times the backoff spins before yielding
    /// the thread. A higher value keeps the thread spinning longer, which can reduce
    /// latency when the queue is expected to fill quickly, at the cost of higher CPU
    /// usage. A lower value yields sooner, reducing CPU usage but potentially
    /// increasing latency.
    ///
    /// For a non-blocking alternative, use [`Receiver::try_recv`].
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send(42);
    /// assert_eq!(rx.recv_with_spin_count(32), 42);
    /// ```
    pub fn recv_with_spin_count(&mut self, spin_count: u32) -> T {
        let head = self.ptr.head().fetch_add(1, Ordering::Relaxed);
        let next = head.wrapping_add(1);
        self.local_head = next;

        let cell = self.ptr.cell_at(head);
        let mut backoff = crate::ExponentialBackoff::new(spin_count, spin_count + 4);
        while cell.epoch().load(Ordering::Acquire) != next {
            backoff.backoff();
        }

        let ret = unsafe { cell.get() };
        cell.epoch()
            .store(head.wrapping_add(self.ptr.capacity), Ordering::Release);

        ret
    }

    /// Attempts to receive a value from the queue without blocking.
    ///
    /// Returns `Some(value)` if a value is available, or `None` if the queue is empty.
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
