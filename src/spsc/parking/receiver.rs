use crate::{
    futex::RECEIVER_WAITING,
    read_guard::BatchReader,
    ring::{Consumer, Ring},
};

/// The consumer end of the parking SPSC queue.
///
/// This struct is `Send` but not `Sync` or `Clone`. It can be moved to another thread, but cannot
/// be shared across threads.
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::spsc::parking::channel;
///
/// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
/// tx.send(1);
/// tx.send(2);
/// assert_eq!(rx.recv(), 1);
/// assert_eq!(rx.recv(), 2);
/// ```
pub struct Receiver<T> {
    consumer: Consumer<Ring<T>>,
}

impl<T> Receiver<T> {
    pub(crate) fn new(ring: Ring<T>) -> Self {
        Self {
            consumer: Consumer::attach(ring),
        }
    }

    /// Attempts to receive a value from the queue without blocking.
    ///
    /// Returns `Some(value)` if a value is available, or `None` if the queue is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spsc::parking::channel;
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
        let value = self.consumer.try_pop()?;
        self.consumer.ring().futex().wake();
        Some(value)
    }

    /// Receives a value from the queue, blocking if necessary.
    ///
    /// This method spins briefly, then yields, and finally parks the thread via
    /// a futex if the queue remains empty. The sender will wake the receiver when
    /// data becomes available. For a non-blocking alternative, use
    /// [`Receiver::try_recv`].
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spsc::parking::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send(42);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn recv(&mut self) -> T {
        // Wait for data, then move the value straight out of the ring. We don't
        // route the value through `try_pop` here: its `Option<T>` would add a
        // copy of the value on the hot path for large payloads.
        let mut backoff = crate::ParkingBackoff::new(16, 4);
        while self.consumer.is_empty() {
            if backoff.backoff() {
                let futex = self.consumer.ring().futex();
                if futex.announce(RECEIVER_WAITING) {
                    // catch lost wakes: recheck against a fresh tail before parking
                    self.consumer.refresh_tail();
                    if self.consumer.is_empty() {
                        futex.sleep(RECEIVER_WAITING);
                    }
                }
            }
            self.consumer.refresh_tail();
        }
        let value = self.consumer.pop();

        self.consumer.ring().futex().wake();

        value
    }

    /// Returns a [`ReadGuard`](crate::read_guard::ReadGuard) that provides
    /// batch read access to available items in the queue.
    ///
    /// The guard tracks how many items have been consumed via
    /// [`ReadGuard::advance`](crate::read_guard::ReadGuard::advance) and
    /// publishes the new head in a single atomic store when dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spsc::parking::channel;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(128).unwrap());
    ///
    /// for i in 0..5 {
    ///     tx.send(i);
    /// }
    ///
    /// let mut guard = rx.read_guard();
    /// assert_eq!(guard.as_slice(), &[0, 1, 2, 3, 4]);
    /// guard.advance(guard.len());
    /// drop(guard);
    ///
    /// assert_eq!(rx.try_recv(), None);
    /// ```
    pub fn read_guard(&mut self) -> crate::read_guard::ReadGuard<'_, Self> {
        crate::read_guard::ReadGuard::new(self)
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}

/// # Safety
///
/// The implementation delegates to the queue's atomic head/tail for synchronisation.
/// `read_buffer` refreshes the cached tail and returns a contiguous slice from
/// the ring buffer.  `advance` publishes the new head via a `Release` store and
/// wakes the sender via futex.
unsafe impl<T> BatchReader for Receiver<T> {
    type Item = T;

    /// Returns a slice of the available read buffer in the queue.
    ///
    /// This allows reading multiple items directly from the queue's memory (zero-copy),
    /// bypassing the per-item overhead of [`recv`](Receiver::recv).
    ///
    /// The returned slice contains contiguous available items starting from the current head.
    /// It may not represent *all* available items if the buffer wraps around; call
    /// `read_buffer` again after advancing to get the next contiguous chunk.
    ///
    /// Items are returned by shared reference -- ownership is **not** transferred.
    /// See [`BatchReader`](crate::read_guard::BatchReader#ownership) for details.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spsc::parking::channel;
    /// use gil::read_guard::BatchReader;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(128).unwrap());
    ///
    /// for i in 0..5 {
    ///     tx.send(i);
    /// }
    ///
    /// let buf = rx.read_buffer();
    /// assert_eq!(buf.len(), 5);
    /// assert_eq!(buf[0], 0);
    /// assert_eq!(buf[4], 4);
    ///
    /// let count = buf.len();
    /// unsafe { rx.advance(count) };
    /// ```
    #[inline]
    fn read_buffer(&mut self) -> &[T] {
        self.consumer.read_buffer()
    }

    /// Advances the consumer head by `n` items.
    ///
    /// This marks items previously obtained via [`read_buffer`](BatchReader::read_buffer)
    /// as consumed, freeing space for the producer.
    ///
    /// # Safety
    ///
    /// * `n` must be less than or equal to the length of the slice returned by the
    ///   most recent call to [`read_buffer`](BatchReader::read_buffer).
    /// * Advancing past the available data results in undefined behavior.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spsc::parking::channel;
    /// use gil::read_guard::BatchReader;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(128).unwrap());
    ///
    /// tx.send(10);
    /// tx.send(20);
    ///
    /// let buf = rx.read_buffer();
    /// assert_eq!(buf, &[10, 20]);
    /// let len = buf.len();
    /// unsafe { rx.advance(len) };
    ///
    /// // Buffer is now empty
    /// assert_eq!(rx.try_recv(), None);
    /// ```
    #[inline(always)]
    unsafe fn advance(&mut self, n: usize) {
        unsafe { self.consumer.advance(n) };
        self.consumer.ring().futex().wake();
    }
}
