use core::{mem::MaybeUninit, num::NonZeroUsize};

use crate::spsc::{self, shards::ShardsPtr};

/// The sending half of a sharded MPSC channel.
///
/// Each sender is bound to a specific shard. Cloning a sender will attempt to bind the new
/// instance to a different, unused shard.
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::mpsc::sharded::channel;
///
/// let (mut tx, mut rx) = channel::<i32>(
///     NonZeroUsize::new(2).unwrap(),
///     NonZeroUsize::new(16).unwrap(),
/// );
///
/// let mut tx2 = tx.clone().expect("shard available");
/// tx.send(1);
/// tx2.send(2);
///
/// let mut values = [rx.recv(), rx.recv()];
/// values.sort();
/// assert_eq!(values, [1, 2]);
/// ```
pub struct Sender<T> {
    inner: spsc::Sender<T>,
    shards: ShardsPtr<T>,
    max_shards: usize,
    shard: usize,
}

impl<T> Sender<T> {
    pub(crate) fn new(shards: ShardsPtr<T>, max_shards: NonZeroUsize) -> Self {
        Self::init(shards, max_shards.get(), 0, 2).unwrap()
    }

    /// Attempts to clone the sender.
    ///
    /// Returns `Some(Sender)` if there is an available shard to bind to, or `None` if
    /// all shards are already occupied.
    ///
    /// This scans the shard table and may touch up to `max_shards` atomics. Prefer
    /// creating long-lived senders instead of cloning and dropping in a hot path.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpsc::sharded::channel;
    ///
    /// let (tx, rx) = channel::<i32>(
    ///     NonZeroUsize::new(2).unwrap(),
    ///     NonZeroUsize::new(16).unwrap(),
    /// );
    ///
    /// let tx2 = tx.clone().expect("shard available");
    ///
    /// // Only 2 shards, so the third clone fails
    /// assert!(tx.clone().is_none());
    /// ```
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> Option<Self> {
        Self::init(
            self.shards.clone(),
            self.max_shards,
            self.shard.wrapping_add(1),
            self.inner.ref_count() - 1,
        )
    }

    pub(crate) fn init(
        shards: ShardsPtr<T>,
        max_shards: usize,
        start_shard: usize,
        free_ref_count: usize,
    ) -> Option<Self> {
        for offset in 0..max_shards {
            let shard = start_shard.wrapping_add(offset) % max_shards;
            if let Some(shard_ptr) = shards.try_claim_queue_ptr(shard, free_ref_count) {
                let inner = spsc::Sender::from_current(shard_ptr);

                return Some(Self {
                    inner,
                    shards,
                    max_shards,
                    shard,
                });
            }
        }

        None
    }

    /// Sends a value into the channel.
    ///
    /// This method will block (spin) until there is space in the shard's queue.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpsc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(16).unwrap(),
    /// );
    /// tx.send(42);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn send(&mut self, value: T) {
        self.inner.send(value)
    }

    /// Attempts to send a value into the channel without blocking.
    ///
    /// Returns `Ok(())` if the value was sent, or `Err(value)` if the shard's queue is full.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpsc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(2).unwrap(),
    /// );
    ///
    /// assert!(tx.try_send(1).is_ok());
    /// assert!(tx.try_send(2).is_ok());
    /// assert_eq!(tx.try_send(3), Err(3));
    /// ```
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        self.inner.try_send(value)
    }

    /// Returns a mutable slice of the internal write buffer for batched sending.
    ///
    /// After writing to the buffer, call [`commit`](Sender::commit) to make the items
    /// visible to the receiver.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpsc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(128).unwrap(),
    /// );
    ///
    /// let buf = tx.write_buffer();
    /// buf[0].write(10);
    /// buf[1].write(20);
    /// unsafe { tx.commit(2) };
    ///
    /// assert_eq!(rx.recv(), 10);
    /// assert_eq!(rx.recv(), 20);
    /// ```
    pub fn write_buffer(&mut self) -> &mut [MaybeUninit<T>] {
        self.inner.write_buffer()
    }

    /// Commits `len` elements from the write buffer to the channel.
    ///
    /// # Safety
    ///
    /// The caller must ensure that at least `len` elements in the write buffer have been initialized.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpsc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(128).unwrap(),
    /// );
    ///
    /// let buf = tx.write_buffer();
    /// buf[0].write(42);
    /// unsafe { tx.commit(1) };
    ///
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub unsafe fn commit(&mut self, len: usize) {
        unsafe { self.inner.commit(len) }
    }
}

unsafe impl<T> Send for Sender<T> {}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use core::num::NonZeroUsize;

    use super::*;

    #[test]
    fn clone_does_not_claim_live_sender_shard_after_receiver_drop() {
        let (tx0, rx) = super::super::channel::<usize>(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(4).unwrap(),
        );
        let tx1 = tx0.clone().unwrap();

        assert!(tx1.clone().is_none());

        let stale_free_ref_count = tx1.inner.ref_count() - 1;
        let start_shard = tx1.shard.wrapping_add(1);
        let shards = tx1.shards.clone();
        let max_shards = tx1.max_shards;

        drop(rx);

        assert!(
            Sender::init(shards, max_shards, start_shard, stale_free_ref_count).is_none(),
            "stale refcount let a second sender claim an already occupied shard"
        );
    }
}
