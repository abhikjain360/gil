use core::mem::MaybeUninit;

use crate::{
    Backoff, Box,
    ring::Producer,
    shard_table::{Cursor, Shard, ShardTable},
};

/// The sending half of a sharded SPMC channel.
///
/// The sender writes to shards in strict round-robin fashion, ensuring even
/// distribution across all receivers.
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

    /// Sends a value to the next shard in round-robin order, blocking if that shard is full.
    pub fn send(&mut self, value: T) {
        let producer = &mut self.producers[self.cursor.index()];

        let mut backoff = Backoff::with_spin_count(128);
        while producer.is_full() {
            backoff.backoff();
            producer.refresh_head();
        }
        producer.push(value);

        self.cursor.step();
    }

    /// Attempts to send to the next shard without blocking.
    ///
    /// Returns `Err(value)` if the current target shard is full.
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        self.producers[self.cursor.index()].try_push(value)?;
        self.cursor.step();
        Ok(())
    }

    /// Returns a mutable slice of the write buffer for the current shard.
    ///
    /// After writing, call [`commit`](Sender::commit).
    pub fn write_buffer(&mut self) -> &mut [MaybeUninit<T>] {
        self.producers[self.cursor.index()].write_buffer()
    }

    /// Commits `len` elements from the write buffer of the current shard.
    ///
    /// # Safety
    ///
    /// The caller must ensure that at least `len` elements have been initialized.
    pub unsafe fn commit(&mut self, len: usize) {
        unsafe { self.producers[self.cursor.index()].commit(len) };
        self.cursor.step();
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
