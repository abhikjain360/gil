use core::mem::MaybeUninit;

use crate::{
    futex::SENDER_WAITING,
    ring::Producer,
    shard_table::{Shard, ShardTable},
};

/// The sending half of a sharded parking MPSC channel.
///
/// Each sender is bound to a specific shard. Cloning a sender will attempt to bind the new
/// instance to a different, unused shard.
///
/// When the sender's shard is full, it parks on the futex embedded in that shard's
/// header and is woken by the receiver after it drains items.
pub struct Sender<T> {
    producer: Producer<Shard<T>>,
    table: ShardTable<T>,
    shard_idx: usize,
}

impl<T> Sender<T> {
    pub(crate) fn new(table: ShardTable<T>) -> Self {
        Self::init(table, 0).unwrap()
    }

    /// Attempts to clone the sender.
    ///
    /// Returns `Some(Sender)` if there is an available shard to bind to, or `None` if
    /// all shards are already occupied.
    ///
    /// This scans the shard table and may touch up to `max_shards` atomics. Prefer
    /// creating long-lived senders instead of cloning and dropping in a hot path.
    pub fn try_clone(&self) -> Option<Self> {
        Self::init(self.table.clone(), self.shard_idx.wrapping_add(1))
    }

    fn init(table: ShardTable<T>, start: usize) -> Option<Self> {
        let (shard_idx, shard) = table.claim_producer(start)?;
        Some(Self {
            producer: Producer::attach(shard),
            table,
            shard_idx,
        })
    }

    /// Sends a value into the channel, parking if the shard is full.
    ///
    /// After a brief spin and yield phase, the sender parks on its shard's futex.
    /// The receiver wakes parked senders after draining items.
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
    }

    /// Attempts to send a value into the channel without blocking.
    ///
    /// Returns `Ok(())` if the value was sent, or `Err(value)` if the shard's queue is full.
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        self.producer.try_push(value)
    }

    /// Returns a mutable slice of the internal write buffer for batched sending.
    ///
    /// After writing to the buffer, call [`commit`](Sender::commit) to make the items
    /// visible to the receiver.
    pub fn write_buffer(&mut self) -> &mut [MaybeUninit<T>] {
        self.producer.write_buffer()
    }

    /// Commits `len` elements from the write buffer to the channel.
    ///
    /// # Safety
    ///
    /// The caller must ensure that at least `len` elements in the write buffer have been initialized.
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
