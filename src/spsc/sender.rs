use core::mem::MaybeUninit;

use crate::ring::{Producer, Ring};

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
        self.producer.try_push(value)?;

        #[cfg(feature = "async")]
        self.producer.ring().wake_receiver();

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
        // Spin until there is space, then move the value straight into the ring.
        // We don't route the value through `try_push` here: its `Result<(), T>`
        // would add a copy of `value` on the hot path for large payloads.
        let mut backoff = crate::Backoff::with_spin_count(spin_count);
        while self.producer.is_full() {
            backoff.backoff();
            self.producer.refresh_head();
        }
        self.producer.push(value);

        #[cfg(feature = "async")]
        self.producer.ring().wake_receiver();
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

        if self.producer.is_full() {
            futures::future::poll_fn(|ctx| {
                self.producer.refresh_head();
                if self.producer.is_full() {
                    self.producer.ring().register_sender_waker(ctx.waker());

                    // prevent lost wake
                    self.producer.refresh_head_seqcst();
                    if self.producer.is_full() {
                        return Poll::Pending;
                    }
                }
                Poll::Ready(())
            })
            .await;
        }

        self.producer.push(value);
        self.producer.ring().wake_receiver();
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
        unsafe { self.producer.commit(len) };

        #[cfg(feature = "async")]
        self.producer.ring().wake_receiver();
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
