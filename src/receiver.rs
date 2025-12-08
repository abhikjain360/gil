use crate::{atomic::Ordering, hint, queue::QueuePtr};

/// The consumer end of the queue.
///
/// This struct is `Send` but not `Sync`. It can be moved to another thread, but cannot be shared
/// across threads.
pub struct Receiver<T> {
    ptr: QueuePtr<T>,
    local_tail: usize,
    mask: usize,
}

impl<T> Receiver<T> {
    pub(crate) fn new(queue_ptr: QueuePtr<T>, mask: usize) -> Self {
        Self {
            ptr: queue_ptr,
            local_tail: 0,
            mask,
        }
    }

    /// Attempts to receive a value from the queue without blocking.
    ///
    /// # Returns
    ///
    /// * `Some(value)` if a value is available.
    /// * `None` if the queue is empty.
    pub fn try_recv(&mut self) -> Option<T> {
        let current_head = self.load_head();
        let new_head = (current_head + 1) & self.mask;

        if current_head == self.local_tail {
            self.load_tail();
            if self.local_tail == current_head {
                return None;
            }
        }

        // SAFETY: head != tail which means queue is not empty and head has valid initialised
        //         value
        let ret = unsafe { self.ptr.get(current_head) };
        self.store_head(new_head);

        Some(ret)
    }

    /// Receives a value from the queue, blocking if necessary.
    ///
    /// This method uses a spin loop to wait for available data in the queue.
    /// For a non-blocking alternative, use [`Receiver::try_recv`].
    pub fn recv(&mut self) -> T {
        let current_head = self.load_head();
        let new_head = (current_head + 1) & self.mask;

        while current_head == self.local_tail {
            hint::spin_loop();
            self.load_tail();
        }

        // SAFETY: head != tail which means queue is not empty and head has valid initialised
        //         value
        let ret = unsafe { self.ptr.get(current_head) };
        self.store_head(new_head);

        ret
    }

    /// Receives a value from the queue asynchronously.
    ///
    /// This method yields the current task if the queue is empty.
    #[cfg(feature = "async")]
    pub async fn recv_async(&mut self) -> T {
        use std::task::Poll;

        futures::future::poll_fn(|ctx| {
            let current_head = self.load_head();

            if current_head == self.local_tail {
                self.load_tail();
                if current_head == self.local_tail {
                    self.ptr.register_receiver_waker(ctx.waker());
                    self.ptr.receiver_sleeping().store(true, Ordering::SeqCst);

                    // prevent lost wake
                    self.local_tail = self.ptr.tail().load(Ordering::SeqCst);
                    if current_head == self.local_tail {
                        return Poll::Pending;
                    }

                    // not sleeping anymore
                    self.ptr.receiver_sleeping().store(false, Ordering::Relaxed);
                }
            }

            let new_head = (current_head + 1) & self.mask;
            // SAFETY: head != tail which means queue is not empty and head has valid initialised
            //         value
            let ret = unsafe { self.ptr.get(current_head) };
            self.store_head(new_head);

            if self.ptr.sender_sleeping().load(Ordering::SeqCst) {
                self.ptr.sender_sleeping().store(false, Ordering::Relaxed);
                self.ptr.wake_sender();
            }
            Poll::Ready(ret)
        })
        .await
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
        let start = self.load_head();

        if start == self.local_tail {
            self.load_tail();
        }

        let end = if self.local_tail >= start {
            self.local_tail
        } else {
            self.ptr.capacity
        };

        unsafe {
            let ptr = self.ptr.at(start);
            std::slice::from_raw_parts(ptr.as_ptr(), end - start)
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
        // the len can be just right at the edge of buffer, so we need to wrap just in case
        let new_head = (self.load_head() + len) & self.mask;
        self.store_head(new_head);
    }

    #[inline(always)]
    fn load_head(&self) -> usize {
        self.ptr.head().load(Ordering::Relaxed)
    }

    #[inline(always)]
    fn store_head(&self, value: usize) {
        self.ptr.head().store(value, Ordering::Release);
    }

    #[inline(always)]
    fn load_tail(&mut self) {
        self.local_tail = self.ptr.tail().load(Ordering::Acquire);
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
