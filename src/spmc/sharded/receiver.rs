use crate::{
    Backoff,
    read_guard::BatchReader,
    ring::Consumer,
    shard_table::{Shard, ShardTable},
};

/// The receiving half of a sharded SPMC channel.
///
/// Each receiver is bound to a specific shard. Cloning a receiver will attempt to bind
/// the new instance to a different, unused shard.
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
    /// Returns `Some(Receiver)` if there is an available shard to bind to, or `None` if
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

    /// Receives a value, spinning/yielding until one is available.
    pub fn recv(&mut self) -> T {
        let mut backoff = Backoff::with_spin_count(128);
        loop {
            match self.try_recv() {
                None => backoff.backoff(),
                Some(ret) => return ret,
            }
        }
    }

    /// Attempts to receive a value from this receiver's shard without blocking.
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
