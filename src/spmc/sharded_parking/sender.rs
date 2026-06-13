use core::mem::MaybeUninit;

use crate::{
    Backoff, Box,
    ring::Producer,
    shard_table::{Cursor, Shard, ShardTable},
};

/// The sending half of a sharded parking SPMC channel.
///
/// The sender writes to shards in strict round-robin fashion. After writing,
/// it checks that shard's futex and wakes its parked receiver, if any.
pub struct Sender<T> {
    producers: Box<[Producer<Shard<T>>]>,
    cursor: Cursor,
}

impl<T> Sender<T> {
    pub(crate) fn new(table: &ShardTable<T>) -> Self {
        Self {
            producers: table.claim_all_producers().map(Producer::attach).collect(),
            cursor: Cursor::new(table.len()),
        }
    }

    /// Sends a value to the next shard in round-robin order, blocking if full.
    ///
    /// After writing, wakes any parked receivers.
    pub fn send(&mut self, value: T) {
        let producer = &mut self.producers[self.cursor.index()];

        let mut backoff = Backoff::with_spin_count(128);
        while producer.is_full() {
            backoff.backoff();
            producer.refresh_head();
        }
        producer.push(value);
        // the push published the new tail; wake this shard's parked receiver, if
        // any — see `Futex::wake` for the ordering reasoning
        producer.ring().futex().wake();

        self.cursor.step();
    }

    /// Attempts to send to the next shard without blocking.
    ///
    /// Returns `Err(value)` if the current target shard is full.
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        let producer = &mut self.producers[self.cursor.index()];

        producer.try_push(value)?;
        producer.ring().futex().wake();

        self.cursor.step();
        Ok(())
    }

    /// Returns a mutable slice of the write buffer for the current shard.
    pub fn write_buffer(&mut self) -> &mut [MaybeUninit<T>] {
        self.producers[self.cursor.index()].write_buffer()
    }

    /// Commits `len` elements and wakes any parked receivers.
    ///
    /// # Safety
    ///
    /// The caller must ensure that at least `len` elements have been initialized.
    pub unsafe fn commit(&mut self, len: usize) {
        let producer = &mut self.producers[self.cursor.index()];

        unsafe { producer.commit(len) };
        producer.ring().futex().wake();

        self.cursor.step();
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
