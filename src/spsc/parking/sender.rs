use core::mem::MaybeUninit;

use crate::{
    futex::SENDER_WAITING,
    ring::{Producer, Ring},
};

/// The producer end of the parking SPSC queue.
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
pub struct Sender<T> {
    producer: Producer<Ring<T>>,
}

impl<T> Sender<T> {
    pub(crate) fn new(ring: Ring<T>) -> Self {
        Self {
            producer: Producer::attach(ring),
        }
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
    /// use gil::spsc::parking::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(2).unwrap());
    ///
    /// assert!(tx.try_send(1).is_ok());
    /// assert!(tx.try_send(2).is_ok());
    ///
    /// // Queue is full
    /// assert_eq!(tx.try_send(3), Err(3));
    ///
    /// // After consuming, we can send again
    /// assert_eq!(rx.try_recv(), Some(1));
    /// assert!(tx.try_send(3).is_ok());
    /// ```
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        self.producer.try_push(value)?;
        self.producer.ring().futex().wake();
        Ok(())
    }

    /// Sends a value into the queue, blocking if necessary.
    ///
    /// This method spins briefly, then yields, and finally parks the thread via
    /// a futex if the queue remains full. The receiver will wake the sender when
    /// space becomes available. For a non-blocking alternative, use
    /// [`Sender::try_send`].
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
    pub fn send(&mut self, value: T) {
        // Wait for space, then move the value straight into the ring. We don't
        // route the value through `try_push` here: its `Result<(), T>` would add
        // a copy of `value` on the hot path for large payloads.
        let mut backoff = crate::ParkingBackoff::new(16, 4);
        while self.producer.is_full() {
            if backoff.backoff() {
                let futex = self.producer.ring().futex();
                if futex.announce(SENDER_WAITING) {
                    // catch lost wakes: recheck against a fresh head before parking
                    self.producer.refresh_head();
                    if self.producer.is_full() {
                        futex.sleep(SENDER_WAITING);
                    }
                }
            }
            self.producer.refresh_head();
        }
        self.producer.push(value);

        self.producer.ring().futex().wake();
    }

    /// Returns a mutable slice to the available write buffer in the queue.
    ///
    /// This allows writing multiple items directly into the queue's memory (zero-copy),
    /// bypassing the per-item overhead of [`send`](Sender::send).
    ///
    /// After writing to the buffer, you must call [`commit`](Sender::commit) to make
    /// the items visible to the receiver.
    ///
    /// The returned slice represents contiguous free space starting from the current tail.
    /// It may not represent *all* free space if the buffer wraps around; call `write_buffer`
    /// again after committing to get the next contiguous chunk.
    ///
    /// The slice contains [`MaybeUninit<T>`] values. You can initialize them with
    /// [`MaybeUninit::write`] or use [`copy_nonoverlapping`](core::ptr::copy_nonoverlapping)
    /// for bulk copies.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spsc::parking::channel;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(128).unwrap());
    ///
    /// // Write a batch of items
    /// let buf = tx.write_buffer();
    /// let count = buf.len().min(5);
    /// for i in 0..count {
    ///     buf[i].write(i + 1);
    /// }
    /// unsafe { tx.commit(count) };
    ///
    /// // Read them back
    /// for i in 0..count {
    ///     assert_eq!(rx.recv(), i + 1);
    /// }
    /// ```
    pub fn write_buffer(&mut self) -> &mut [MaybeUninit<T>] {
        self.producer.write_buffer()
    }

    /// Commits items written to the buffer obtained via [`write_buffer`](Sender::write_buffer).
    ///
    /// This makes `len` items visible to the receiver.
    ///
    /// # Safety
    ///
    /// * `len` must be less than or equal to the length of the slice returned by the
    ///   most recent call to [`write_buffer`](Sender::write_buffer).
    /// * All `len` items in the buffer must have been initialized before calling this.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spsc::parking::channel;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(NonZeroUsize::new(128).unwrap());
    ///
    /// let buf = tx.write_buffer();
    /// buf[0].write(10);
    /// buf[1].write(20);
    /// unsafe { tx.commit(2) };
    ///
    /// assert_eq!(rx.recv(), 10);
    /// assert_eq!(rx.recv(), 20);
    /// ```
    #[inline(always)]
    pub unsafe fn commit(&mut self, len: usize) {
        unsafe { self.producer.commit(len) };
        self.producer.ring().futex().wake();
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
