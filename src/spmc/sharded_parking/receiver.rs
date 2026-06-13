use crate::{
    futex::RECEIVER_WAITING,
    read_guard::BatchReader,
    ring::Consumer,
    shard_table::{Shard, ShardTable},
};

/// The receiving half of a sharded parking SPMC channel.
///
/// Each receiver is bound to a specific shard. When the shard is empty, the
/// receiver parks on the futex embedded in that shard's header and is woken by
/// the sender after writing.
pub struct Receiver<T> {
    consumer: Consumer<Shard<T>>,
    table: ShardTable<T>,
    shard_idx: usize,
}

impl<T> Receiver<T> {
    pub(crate) fn new(table: ShardTable<T>) -> Self {
        Self::init(table, 0).unwrap()
    }

    /// Attempts to clone the receiver.
    ///
    /// Returns `Some(Receiver)` if there is an available shard, or `None` if
    /// all shards are already occupied.
    ///
    /// This scans the shard table and may touch up to `max_shards` atomics. Prefer
    /// creating long-lived receivers instead of cloning and dropping in a hot path.
    pub fn try_clone(&self) -> Option<Self> {
        Self::init(self.table.clone(), self.shard_idx.wrapping_add(1))
    }

    fn init(table: ShardTable<T>, start: usize) -> Option<Self> {
        let (shard_idx, shard) = table.claim_consumer(start)?;
        Some(Self {
            consumer: Consumer::attach(shard),
            table,
            shard_idx,
        })
    }

    /// Receives a value, parking if the shard is empty.
    ///
    /// After a brief spin and yield phase, parks on the shard's futex.
    /// The sender wakes parked receivers after writing.
    pub fn recv(&mut self) -> T {
        let mut backoff = crate::ParkingBackoff::new(16, 4);
        while self.consumer.is_empty() {
            if backoff.backoff() {
                let futex = self.consumer.ring().futex();
                if futex.announce(RECEIVER_WAITING) {
                    // catch lost wakes: recheck against a fresh tail before parking
                    self.consumer.refresh_tail();
                    if self.consumer.is_empty() {
                        futex.sleep(RECEIVER_WAITING);
                    }
                }
            }
            self.consumer.refresh_tail();
        }
        self.consumer.pop()
    }

    /// Attempts to receive without blocking.
    pub fn try_recv(&mut self) -> Option<T> {
        self.consumer.try_pop()
    }

    /// Returns a [`ReadGuard`](crate::read_guard::ReadGuard) for batch reading.
    pub fn read_guard(&mut self) -> crate::read_guard::ReadGuard<'_, Self> {
        crate::read_guard::ReadGuard::new(self)
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}

/// # Safety
///
/// The implementation delegates to this receiver's single shard consumer.
unsafe impl<T> BatchReader for Receiver<T> {
    type Item = T;

    fn read_buffer(&mut self) -> &[T] {
        self.consumer.read_buffer()
    }

    unsafe fn advance(&mut self, n: usize) {
        unsafe { self.consumer.advance(n) };
    }
}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use core::num::NonZeroUsize;

    #[test]
    fn try_clone_does_not_claim_live_receiver_shard_after_sender_drop() {
        let (tx, rx0) = super::super::channel::<usize>(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(4).unwrap(),
        );
        let rx1 = rx0.try_clone().unwrap();

        assert!(rx1.try_clone().is_none());

        drop(tx);
        assert!(rx1.try_clone().is_none());
    }
}
