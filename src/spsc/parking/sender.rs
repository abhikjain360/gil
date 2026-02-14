use core::mem::MaybeUninit;

use crate::{
    atomic::Ordering,
    spsc::parking::queue::{FutexState, QueuePtr},
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
    ptr: QueuePtr<T>,
    local_head: usize,
    local_tail: usize,
}

impl<T> Sender<T> {
    pub(crate) fn new(queue_ptr: QueuePtr<T>) -> Self {
        Self {
            ptr: queue_ptr,
            local_head: 0,
            local_tail: 0,
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
        let new_tail = self.local_tail.wrapping_add(1);

        if new_tail > self.max_tail() {
            self.load_head();
            if new_tail > self.max_tail() {
                return Err(value);
            }
        }

        unsafe { self.ptr.set(self.local_tail, value) };
        self.store_tail(new_tail);
        self.local_tail = new_tail;

        self.ptr.futex_wake();

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
        let new_tail = self.local_tail.wrapping_add(1);

        let mut backoff = crate::ParkingBackoff::new(16, 4);
        while new_tail > self.max_tail() {
            if backoff.backoff() && self.ptr.futex_store(FutexState::SenderWaiting) {
                // catch lost unparks first before trying to park self
                self.load_head();
                if new_tail > self.max_tail() {
                    self.ptr.futex_wait(FutexState::SenderWaiting);
                }
            };
            self.load_head();
        }

        unsafe { self.ptr.set(self.local_tail, value) };
        self.store_tail(new_tail);
        self.local_tail = new_tail;

        self.ptr.futex_wake();
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
        let mut available = self.ptr.size - self.local_tail.wrapping_sub(self.local_head);

        if available == 0 {
            self.load_head();
            available = self.ptr.size - self.local_tail.wrapping_sub(self.local_head);
        }

        let start = self.local_tail & self.ptr.mask;
        let contiguous = self.ptr.capacity - start;
        let len = available.min(contiguous);

        unsafe {
            let ptr = self.ptr.exact_at(start).cast();
            core::slice::from_raw_parts_mut(ptr.as_ptr(), len)
        }
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
        #[cfg(debug_assertions)]
        {
            let start = self.local_tail & self.ptr.mask;
            let contiguous = self.ptr.capacity - start;
            let available =
                contiguous.min(self.ptr.size - self.local_tail.wrapping_sub(self.local_head));
            assert!(
                len <= available,
                "advancing ({len}) more than available space ({available})"
            );
        }

        // the len can be just right at the edge of buffer, so we need to wrap just in case
        let new_tail = self.local_tail.wrapping_add(len);
        self.store_tail(new_tail);
        self.local_tail = new_tail;

        self.ptr.futex_wake();
    }

    #[inline(always)]
    fn max_tail(&self) -> usize {
        self.local_head.wrapping_add(self.ptr.size)
    }

    #[inline(always)]
    fn store_tail(&self, value: usize) {
        self.ptr.tail().store(value, Ordering::Release);
    }

    #[inline(always)]
    fn load_head(&mut self) {
        self.local_head = self.ptr.head().load(Ordering::Acquire);
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
