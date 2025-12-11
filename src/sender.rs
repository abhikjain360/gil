use std::mem::MaybeUninit;

use crate::{atomic::Ordering, hint, queue::QueuePtr};

/// The producer end of the queue.
///
/// This struct is `Send` but not `Sync`. It can be moved to another thread, but cannot be shared
/// across threads.
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

    #[inline(always)]
    fn next_tail(&self) -> usize {
        let next = self.local_tail + 1;
        if next == self.ptr.capacity { 0 } else { next }
    }

    /// Attempts to send a value into the queue without blocking.
    ///
    /// # Returns
    ///
    /// * `true` if the value was successfully sent.
    /// * `false` if the queue is full.
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        let new_tail = self.next_tail();

        if new_tail == self.local_head {
            self.load_head();
            if new_tail == self.local_head {
                return Err(value);
            }
        }

        self.ptr.set(self.local_tail, value);
        self.store_tail(new_tail);
        self.local_tail = new_tail;

        Ok(())
    }

    /// Sends a value into the queue, blocking if necessary.
    ///
    /// This method uses a spin loop to wait for available space in the queue.
    /// For a non-blocking alternative, use [`Sender::try_send`].
    pub fn send(&mut self, value: T) {
        let new_tail = self.next_tail();

        while new_tail == self.local_head {
            hint::spin_loop();
            self.load_head();
        }

        self.ptr.set(self.local_tail, value);
        self.store_tail(new_tail);
        self.local_tail = new_tail;
    }

    /// Sends a value into the queue asynchronously.
    ///
    /// This method yields the current task if the queue is full.
    #[cfg(feature = "async")]
    pub async fn send_async(&mut self, value: T) {
        use std::task::Poll;

        // We need to store the value in an Option so we can move it into the queue
        // when space becomes available
        let mut value = Some(value);

        futures::future::poll_fn(|ctx| {
            let new_tail = self.next_tail();

            if new_tail == self.local_head {
                self.load_head();
                if new_tail == self.local_head {
                    self.ptr.register_sender_waker(ctx.waker());
                    self.ptr.sender_sleeping().store(true, Ordering::SeqCst);

                    // prevent lost wake
                    self.local_head = self.ptr.head().load(Ordering::SeqCst);
                    if new_tail == self.local_head {
                        return Poll::Pending;
                    }

                    // not sleeping anymore
                    self.ptr.sender_sleeping().store(false, Ordering::Relaxed);
                }
            }

            self.ptr.set(self.local_tail, value.take().unwrap());
            self.store_tail(new_tail);
            self.local_tail = new_tail;

            if self.ptr.receiver_sleeping().load(Ordering::SeqCst) {
                self.ptr.receiver_sleeping().store(false, Ordering::Relaxed);
                self.ptr.wake_receiver();
            }
            Poll::Ready(())
        })
        .await
    }

    /// Returns a mutable slice to the available write buffer in the queue.
    ///
    /// This allows writing multiple items directly into the queue's memory (zero-copy),
    /// bypassing the per-item overhead of `send`.
    ///
    /// After writing to the buffer, you must call [`Sender::commit`] to make the items visible
    /// to the receiver.
    ///
    /// # Returns
    ///
    /// A mutable slice representing the contiguous free space starting from the current tail.
    /// Note that this might not represent *all* free space if the buffer wraps around.
    ///
    /// # Usage
    ///
    /// It returns a slice of [`MaybeUninit`](std::mem::MaybeUninit) to prevent UB, you can use
    /// [`copy_nonoverlapping`](std::ptr::copy_nonoverlapping) if you want fast copying between
    /// this and your own data.
    pub fn write_buffer(&mut self) -> &mut [MaybeUninit<T>] {
        let start = self.local_tail;
        let next = self.next_tail();

        if next == self.local_head {
            self.load_head();
        }

        let end = if self.local_head > start {
            self.local_head - 1
        } else if self.local_head == 0 {
            self.ptr.capacity - 1
        } else {
            self.ptr.capacity
        };

        unsafe {
            let ptr = self.ptr.at(start).cast();
            std::slice::from_raw_parts_mut(ptr.as_ptr(), end - start)
        }
    }

    /// Commits items written to the buffer obtained via [`Sender::write_buffer`].
    ///
    /// # Safety
    ///
    /// * This function must only be called after writing data to the slice returned by [`Sender::write_buffer`].
    /// * `len` must be less than or equal to the length of the slice returned by the most recent call to [`Sender::write_buffer`].
    /// * Committing more items than available in the buffer slice will result in undefined behavior.
    #[inline(always)]
    pub unsafe fn commit(&mut self, len: usize) {
        // the len can be just right at the edge of buffer, so we need to wrap just in case
        let mut new_tail = self.local_tail + len;
        if new_tail >= self.ptr.capacity {
            new_tail -= self.ptr.capacity;
        }
        self.store_tail(new_tail);
        self.local_tail = new_tail;
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
