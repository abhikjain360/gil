use core::mem::MaybeUninit;

use crate::{
    ring::Producer,
    shard_table::{Shard, ShardTable},
};

/// The sending half of a sharded MPMC channel.
///
/// Each sender is bound to a specific shard. Cloning a sender will attempt to bind the new
/// instance to a different, unused shard.
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::mpmc::sharded::channel;
///
/// let (mut tx, mut rx) = channel::<i32>(
///     NonZeroUsize::new(2).unwrap(),
///     NonZeroUsize::new(16).unwrap(),
/// );
///
/// let mut tx2 = tx.try_clone().expect("shard available");
/// tx.send(1);
/// tx2.send(2);
///
/// let mut values = [rx.recv(), rx.recv()];
/// values.sort();
/// assert_eq!(values, [1, 2]);
/// ```
pub struct Sender<T> {
    producer: Producer<Shard<T>>,
    table: ShardTable<T>,
    shard_idx: usize,
}

impl<T> Sender<T> {
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
    /// use gil::mpmc::sharded::channel;
    ///
    /// let (tx, rx) = channel::<i32>(
    ///     NonZeroUsize::new(2).unwrap(),
    ///     NonZeroUsize::new(16).unwrap(),
    /// );
    ///
    /// let tx2 = tx.try_clone().expect("shard available");
    ///
    /// // Only 2 shards, so the third clone fails
    /// assert!(tx.try_clone().is_none());
    /// ```
    pub fn try_clone(&self) -> Option<Self> {
        Self::init(self.table.clone(), self.shard_idx.wrapping_add(1))
    }

    pub(super) fn new(table: ShardTable<T>) -> Self {
        Self::init(table, 0).unwrap()
    }

    fn init(table: ShardTable<T>, start: usize) -> Option<Self> {
        let (shard_idx, shard) = table.claim_producer(start)?;
        Some(Self {
            producer: Producer::attach(shard),
            table,
            shard_idx,
        })
    }

    /// Sends a value into the channel.
    ///
    /// This method will block (spin) until there is space in the shard's queue.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(16).unwrap(),
    /// );
    /// tx.send(42);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn send(&mut self, value: T) {
        // Spin until the shard has space, then move the value straight into the ring.
        let mut backoff = crate::Backoff::with_spin_count(128);
        while self.producer.is_full() {
            backoff.backoff();
            self.producer.refresh_head();
        }
        self.producer.push(value);
    }

    /// Attempts to send a value into the channel without blocking.
    ///
    /// Returns `Ok(())` if the value was sent, or `Err(value)` if the shard's queue is full.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
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
        self.producer.try_push(value)
    }

    /// Returns a mutable slice of the internal write buffer for batched sending.
    ///
    /// After writing to the buffer, call [`commit`](Sender::commit) to make the items
    /// visible to receivers.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
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
        self.producer.write_buffer()
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
    /// use gil::mpmc::sharded::channel;
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
        unsafe { self.producer.commit(len) }
    }
}

unsafe impl<T: Send> Send for Sender<T> {}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use core::num::NonZeroUsize;

    #[test]
    fn try_clone_does_not_claim_live_sender_shard_after_receiver_drop() {
        let (tx0, rx) = super::super::channel::<usize>(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(4).unwrap(),
        );
        let tx1 = tx0.try_clone().unwrap();

        assert!(tx1.try_clone().is_none());

        drop(rx);
        assert!(tx1.try_clone().is_none());
    }
}
