use crate::{atomic::Ordering, read_guard::BatchReader, spsc::queue::QueuePtr};

/// The consumer end of the SPSC queue.
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
pub struct Receiver<T> {
    ptr: QueuePtr<T>,
    local_tail: usize,
    local_head: usize,
}

impl<T> Receiver<T> {
    pub(crate) fn new(queue_ptr: QueuePtr<T>) -> Self {
        Self {
            ptr: queue_ptr,
            local_tail: 0,
            local_head: 0,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.local_head == self.local_tail && {
            // relaxed load is enough as we are only checking for emptiness hint
            // to avoid expensive locking in sharded implementation
            let tail = self.ptr.tail().load(Ordering::Relaxed);
            self.local_head == tail
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
    /// use gil::spsc::channel;
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
        if self.local_head == self.local_tail {
            self.load_tail();
            if self.local_head == self.local_tail {
                return None;
            }
        }

        // SAFETY: head != tail which means queue is not empty and head has valid initialised
        //         value
        let ret = unsafe { self.ptr.get(self.local_head) };
        let new_head = self.local_head.wrapping_add(1);
        self.store_head(new_head);
        self.local_head = new_head;

        #[cfg(feature = "async")]
        self.ptr.wake_sender();

        Some(ret)
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
    /// use gil::spsc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send(42);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn recv(&mut self) -> T {
        self.recv_with_spin_count(128)
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
    /// use gil::spsc::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
    /// tx.send(42);
    ///
    /// // Use a lower spin count to yield sooner under contention
    /// assert_eq!(rx.recv_with_spin_count(32), 42);
    /// ```
    pub fn recv_with_spin_count(&mut self, spin_count: u32) -> T {
        let mut backoff = crate::Backoff::with_spin_count(spin_count);
        while self.local_head == self.local_tail {
            backoff.backoff();
            self.load_tail();
        }

        // SAFETY: head != tail which means queue is not empty and head has valid initialised
        //         value
        let ret = unsafe { self.ptr.get(self.local_head) };
        let new_head = self.local_head.wrapping_add(1);
        self.store_head(new_head);
        self.local_head = new_head;

        #[cfg(feature = "async")]
        self.ptr.wake_sender();

        ret
    }

    /// Receives a value from the queue asynchronously.
    ///
    /// This method yields the current task if the queue is empty, and resumes
    /// when data becomes available.
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
    pub async fn recv_async(&mut self) -> T {
        use core::task::Poll;

        if self.local_head == self.local_tail {
            futures::future::poll_fn(|ctx| {
                self.load_tail();
                if self.local_head == self.local_tail {
                    self.ptr.register_receiver_waker(ctx.waker());

                    // prevent lost wake
                    self.local_tail = self.ptr.tail().load(Ordering::SeqCst);
                    if self.local_head == self.local_tail {
                        return Poll::Pending;
                    }
                }
                Poll::Ready(())
            })
            .await;
        }

        // SAFETY: head != tail which means queue is not empty and head has valid initialised
        //         value
        let ret = unsafe { self.ptr.get(self.local_head) };
        let new_head = self.local_head.wrapping_add(1);
        self.store_head(new_head);
        self.local_head = new_head;

        self.ptr.wake_sender();

        ret
    }

    #[inline(always)]
    fn store_head(&self, value: usize) {
        self.ptr.head().store(value, Ordering::Release);
    }

    #[inline(always)]
    fn load_tail(&mut self) {
        self.local_tail = self.ptr.tail().load(Ordering::Acquire);
    }

    #[inline(always)]
    pub(crate) fn refresh_head(&mut self) {
        self.local_head = self.ptr.head().load(Ordering::Acquire);
        if self.local_tail < self.local_head {
            self.local_tail = self.ptr.tail().load(Ordering::Acquire);
        }
    }

    /// # Safety
    /// Caller needs to ensure that only one receiver ever access the the `self.ptr` at any time.
    #[inline(always)]
    pub(crate) unsafe fn clone_via_ptr(&self) -> Self {
        Self {
            ptr: self.ptr.clone(),
            local_tail: self.local_tail,
            local_head: self.local_head,
        }
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}

/// # Safety
///
/// The implementation delegates to the queue's atomic head/tail for synchronisation.
/// `read_buffer` refreshes the cached tail and returns a contiguous slice from
/// the ring buffer.  `advance` publishes the new head via a `Release` store and
/// wakes the sender when the `async` feature is enabled.
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
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::spsc::channel;
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
        let mut available = self.local_tail.wrapping_sub(self.local_head);

        if available == 0 {
            self.load_tail();
            available = self.local_tail.wrapping_sub(self.local_head);
        }

        let start = self.local_head & self.ptr.mask;
        let contiguous = self.ptr.capacity - start;
        let len = available.min(contiguous);

        unsafe {
            let ptr = self.ptr.exact_at(start);
            core::slice::from_raw_parts(ptr.as_ptr(), len)
        }
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
    /// use gil::spsc::channel;
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
        #[cfg(debug_assertions)]
        {
            let start = self.local_head & self.ptr.mask;
            let contiguous = self.ptr.capacity - start;
            let available = contiguous.min(self.local_tail.wrapping_sub(self.local_head));
            assert!(
                n <= available,
                "advancing ({n}) more than available space ({available})"
            );
        }

        let new_head = self.local_head.wrapping_add(n);
        self.store_head(new_head);
        self.local_head = new_head;

        #[cfg(feature = "async")]
        self.ptr.wake_sender();
    }
}
