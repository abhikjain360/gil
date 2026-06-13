use crate::{
    Backoff, Box,
    read_guard::BatchReader,
    ring::Consumer,
    shard_table::{Cursor, Shard, ShardTable},
};

/// The receiving half of a sharded parking MPSC channel.
///
/// The receiver polls all shards in round-robin fashion. After consuming items,
/// it checks that shard's futex and wakes its parked sender, if any.
pub struct Receiver<T> {
    consumers: Box<[Consumer<Shard<T>>]>,
    cursor: Cursor,
}

impl<T> Receiver<T> {
    pub(crate) fn new(table: &ShardTable<T>) -> Self {
        Self {
            consumers: table.claim_all_consumers().map(Consumer::attach).collect(),
            cursor: Cursor::new(table.len()),
        }
    }

    /// Receives a value from the channel, spinning/yielding until one is available.
    pub fn recv(&mut self) -> T {
        let mut backoff = Backoff::with_spin_count(128);
        loop {
            match self.try_recv() {
                None => backoff.backoff(),
                Some(ret) => return ret,
            }
        }
    }

    /// Attempts to receive a value from any shard without blocking.
    ///
    /// Returns `Some(value)` if a value was received, or `None` if all shards are empty.
    pub fn try_recv(&mut self) -> Option<T> {
        // Locate a non-empty shard in the scan, then pop outside it: a pop
        // inside the closure would route the value through an extra `Option<T>`
        // return — a copy of `T` on the hot path for large payloads.
        let consumers = &mut self.consumers;
        let shard_idx = self
            .cursor
            .find(|shard_idx| consumers[shard_idx].has_items().then_some(shard_idx))?;

        let consumer = &mut self.consumers[shard_idx];
        let value = consumer.pop();
        // the pop published the new head; wake this shard's parked sender, if
        // any — see `Futex::wake` for the ordering reasoning
        consumer.ring().futex().wake();
        Some(value)
    }

    /// Returns a [`ReadGuard`](crate::read_guard::ReadGuard) for batch reading.
    pub fn read_guard(&mut self) -> crate::read_guard::ReadGuard<'_, Self> {
        crate::read_guard::ReadGuard::new(self)
    }
}

/// # Safety
///
/// The implementation delegates to the per-shard SPSC consumers.
/// `read_buffer` polls shards round-robin and returns the first non-empty
/// contiguous slice. `advance` publishes the new head on the active shard and
/// wakes its parked sender, if any.
unsafe impl<T> BatchReader for Receiver<T> {
    type Item = T;

    fn read_buffer(&mut self) -> &[T] {
        let consumers = &mut self.consumers;
        let found = self.cursor.find(|shard_idx| {
            let (ptr, len) = consumers[shard_idx].read_buffer_raw();
            (len > 0).then_some((ptr, len))
        });

        match found {
            // SAFETY: raw parts of the found shard's ring, which `self` keeps
            // alive; rebuilt here only so the slice outlives the scan closure.
            Some((ptr, len)) => unsafe { core::slice::from_raw_parts(ptr.as_ptr(), len) },
            None => &[],
        }
    }

    unsafe fn advance(&mut self, n: usize) {
        let consumer = &mut self.consumers[self.cursor.index()];
        unsafe { consumer.advance(n) };
        consumer.ring().futex().wake();
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
