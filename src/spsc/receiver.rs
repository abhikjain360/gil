use crate::{atomic::Ordering, spsc::queue::QueuePtr};

/// The consumer end of the SPSC queue.
///
/// This struct is `Send` but not `Sync` or `Clone`. It can be moved to another thread, but cannot be shared
/// across threads.
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

    /// Attempts to receive a value from the queue without blocking.
    ///
    /// # Returns
    ///
    /// * `Some(value)` if a value is available.
    /// * `None` if the queue is empty.
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

        Some(ret)
    }

    /// Receives a value from the queue, blocking if necessary.
    ///
    /// This method uses a spin loop to wait for available data in the queue.
    /// For a non-blocking alternative, use [`Receiver::try_recv`].
    pub fn recv(&mut self) -> T {
        let mut backoff = crate::Backoff::with_spin_count(128);
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

        ret
    }

    /// Receives a value from the queue asynchronously.
    ///
    /// This method yields the current task if the queue is empty.
    #[cfg(feature = "async")]
    pub async fn recv_async(&mut self) -> T {
        use std::task::Poll;

        if self.local_head == self.local_tail {
            futures::future::poll_fn(|ctx| {
                self.load_tail();
                if self.local_head == self.local_tail {
                    self.ptr.register_receiver_waker(ctx.waker());
                    self.ptr.receiver_sleeping().store(true, Ordering::SeqCst);

                    // prevent lost wake
                    self.local_tail = self.ptr.tail().load(Ordering::SeqCst);
                    if self.local_head == self.local_tail {
                        return Poll::Pending;
                    }

                    // not sleeping anymore
                    self.ptr.receiver_sleeping().store(false, Ordering::Relaxed);
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

        if self.ptr.sender_sleeping().load(Ordering::SeqCst) {
            self.ptr.wake_sender();
        }

        ret
    }

    /// Returns a slice to the available read buffer in the queue.
    ///
    /// This allows reading multiple items directly from the queue's memory (zero-copy),
    /// bypassing the per-item overhead of `recv`.
    ///
    /// After reading from the buffer, you must call [`Receiver::advance`] to mark the items as consumed.
    ///
    /// # Returns
    ///
    /// A slice containing available items starting from the current head.
    /// Note that this might not represent *all* available items if the buffer wraps around.
    pub fn read_buffer(&mut self) -> &[T] {
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
            std::slice::from_raw_parts(ptr.as_ptr(), len)
        }
    }

    /// Advances the consumer head by `len` items.
    ///
    /// This should be called after processing items obtained via [`Receiver::read_buffer`].
    ///
    /// # Safety
    ///
    /// * This function must only be called after reading data from the slice returned by [`Receiver::read_buffer`].
    /// * `len` must be less than or equal to the length of the slice returned by the most recent call to [`Receiver::read_buffer`].
    /// * Advancing past the available data in the buffer results in undefined behavior.
    #[inline(always)]
    pub unsafe fn advance(&mut self, len: usize) {
        #[cfg(debug_assertions)]
        {
            let start = self.local_head & self.ptr.mask;
            let contiguous = self.ptr.capacity - start;
            let available = contiguous.min(self.local_tail.wrapping_sub(self.local_head));
            assert!(
                len <= available,
                "advancing ({len}) more than available space ({available})"
            );
        }

        // the len can be just right at the edge of buffer, so we need to wrap just in case
        let new_head = self.local_head.wrapping_add(len);
        self.store_head(new_head);
        self.local_head = new_head;
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
