use core::mem::MaybeUninit;

use crate::{atomic::Ordering, spsc::queue::QueuePtr};

/// The producer end of the SPSC queue.
///
/// This struct is `Send` but not `Sync` or `Clone`. It can be moved to another thread, but cannot be shared
/// across threads.
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::spsc::channel;
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
    /// use gil::spsc::channel;
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
        if self.is_full() {
            self.load_head();
            if self.is_full() {
                return Err(value);
            }
        }

        let new_tail = self.local_tail.wrapping_add(1);
        unsafe { self.ptr.set(self.local_tail, value) };
        self.store_tail(new_tail);
        self.local_tail = new_tail;

        #[cfg(feature = "async")]
        self.ptr.wake_receiver();

        Ok(())
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
    /// use gil::spsc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send(42);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn send(&mut self, value: T) {
        self.send_with_spin_count(value, 128);
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
    /// use gil::spsc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    ///
    /// // Use a lower spin count to yield sooner under contention
    /// tx.send_with_spin_count(42, 32);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn send_with_spin_count(&mut self, value: T, spin_count: u32) {
        let mut backoff = crate::Backoff::with_spin_count(spin_count);
        while self.is_full() {
            backoff.backoff();
            self.load_head();
        }

        let new_tail = self.local_tail.wrapping_add(1);
        unsafe { self.ptr.set(self.local_tail, value) };
        self.store_tail(new_tail);
        self.local_tail = new_tail;

        #[cfg(feature = "async")]
        self.ptr.wake_receiver();
    }

    /// Sends a value into the queue asynchronously.
    ///
    /// This method yields the current task if the queue is full, and resumes
    /// when space becomes available.
    ///
    /// Requires the `async` feature.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use core::num::NonZeroUsize;
    /// use gil::spsc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send_async(42).await;
    /// assert_eq!(rx.recv_async().await, 42);
    /// ```
    #[cfg(feature = "async")]
    pub async fn send_async(&mut self, value: T) {
        use core::task::Poll;

        if self.is_full() {
            futures::future::poll_fn(|ctx| {
                self.load_head();
                if self.is_full() {
                    self.ptr.register_sender_waker(ctx.waker());

                    // prevent lost wake
                    self.local_head = self.ptr.head().load(Ordering::SeqCst);
                    if self.is_full() {
                        return Poll::Pending;
                    }
                }
                Poll::Ready(())
            })
            .await;
        }

        let new_tail = self.local_tail.wrapping_add(1);
        unsafe { self.ptr.set(self.local_tail, value) };
        self.store_tail(new_tail);
        self.local_tail = new_tail;

        self.ptr.wake_receiver();
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
    /// use gil::spsc::channel;
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
    /// use gil::spsc::channel;
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

        #[cfg(feature = "async")]
        self.ptr.wake_receiver();
    }

    #[inline(always)]
    fn is_full(&self) -> bool {
        self.local_tail.wrapping_sub(self.local_head) >= self.ptr.size
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
