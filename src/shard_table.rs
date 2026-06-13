//! The shard table — the refcounted slice of SPSC rings every sharded channel
//! is built from, plus the two pieces of shard bookkeeping the endpoints share:
//! the role-claim scan ([`ShardTable::claim_producer`]/[`claim_consumer`](ShardTable::claim_consumer))
//! and the round-robin [`Cursor`].
//!
//! Each [`Shard`] is an SPSC ring whose header tracks role occupancy (table /
//! producer / consumer) in a single atomic — see
//! [`ShardOwnership`](crate::queue::ShardOwnership). The table holds the
//! table-owner handle of every shard; endpoints claim the producer or consumer
//! slot of individual shards and keep the rings alive through their own handles,
//! so the table allocation itself only needs to live while someone may still
//! claim from it (i.e. while a cloneable endpoint exists).

use core::num::NonZeroUsize;

use crate::queue::ShardOwnership;
pub(crate) use crate::spsc::queue::Shard;

#[cfg(not(feature = "loom"))]
type Table<T> = crate::Arc<[Shard<T>]>;
// loom's `Arc` cannot hold unsized slices; box the slice under loom.
#[cfg(feature = "loom")]
type Table<T> = crate::Arc<crate::Box<[Shard<T>]>>;

/// A shared, refcounted table of [`Shard`]s. Cloning shares the same table;
/// the final drop releases every shard's table-owner role.
pub(crate) struct ShardTable<T> {
    shards: Table<T>,
}

impl<T> Clone for ShardTable<T> {
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
        }
    }
}

impl<T> ShardTable<T> {
    pub(crate) fn new(max_shards: NonZeroUsize, capacity_per_shard: NonZeroUsize) -> Self {
        let shards = (0..max_shards.get()).map(|_| Shard::with_size(capacity_per_shard));

        #[cfg(not(feature = "loom"))]
        let shards = shards.collect();
        #[cfg(feature = "loom")]
        let shards = crate::Arc::new(shards.collect());

        Self { shards }
    }

    pub(crate) fn len(&self) -> usize {
        self.shards.len()
    }

    /// Scans every shard once, starting at `start` (wrapping), and claims the
    /// first whose `role` slot is free. Returns the claimed handle and its index.
    fn claim(&self, role: usize, start: usize) -> Option<(usize, Shard<T>)> {
        let len = self.shards.len();
        for offset in 0..len {
            let index = start.wrapping_add(offset) % len;
            if let Some(shard) = self.shards[index].try_clone_as(role) {
                return Some((index, shard));
            }
        }

        None
    }

    /// Claims `role` on every shard, in index order. Only valid while no other
    /// claimant of `role` exists (channel construction); panics on an occupied
    /// slot.
    fn claim_all(&self, role: usize) -> impl Iterator<Item = Shard<T>> + '_ {
        self.shards.iter().map(move |shard| {
            shard
                .try_clone_as(role)
                .expect("shard role already claimed")
        })
    }

    pub(crate) fn claim_producer(&self, start: usize) -> Option<(usize, Shard<T>)> {
        self.claim(ShardOwnership::PRODUCER, start)
    }

    pub(crate) fn claim_consumer(&self, start: usize) -> Option<(usize, Shard<T>)> {
        self.claim(ShardOwnership::CONSUMER, start)
    }

    pub(crate) fn claim_all_producers(&self) -> impl Iterator<Item = Shard<T>> + '_ {
        self.claim_all(ShardOwnership::PRODUCER)
    }

    pub(crate) fn claim_all_consumers(&self) -> impl Iterator<Item = Shard<T>> + '_ {
        self.claim_all(ShardOwnership::CONSUMER)
    }
}

/// Round-robin cursor over the shard indices `0..len`.
pub(crate) struct Cursor {
    next: usize,
    len: usize,
}

impl Cursor {
    pub(crate) fn new(len: usize) -> Self {
        Self { next: 0, len }
    }

    /// The index the cursor points at.
    #[inline(always)]
    pub(crate) fn index(&self) -> usize {
        self.next
    }

    /// Advances one position, wrapping at the end of the table.
    #[inline(always)]
    pub(crate) fn step(&mut self) {
        self.next += 1;
        if self.next == self.len {
            self.next = 0;
        }
    }

    /// Visits every index once, starting at the current position, until `f`
    /// returns `Some`. The cursor stays on the index that produced the value,
    /// so follow-ups like `advance`/`release` target the same shard; after a
    /// full empty sweep it is back where it started.
    #[inline]
    pub(crate) fn find<U>(&mut self, mut f: impl FnMut(usize) -> Option<U>) -> Option<U> {
        let start = self.next;
        loop {
            if let Some(found) = f(self.next) {
                return Some(found);
            }

            self.step();
            if self.next == start {
                return None;
            }
        }
    }
}
