use core::{num::NonZeroUsize, ptr::NonNull};

use crate::{
    atomic::{AtomicU32, Ordering},
    read_guard::BatchReader,
    spsc::{self, parking_shards::ParkingShardsPtr},
};

/// The receiving half of a sharded parking SPMC channel.
///
/// Each receiver is bound to a specific shard. When the shard is empty, the
/// receiver parks on a shared futex and is woken by the sender after writing.
pub struct Receiver<T> {
    ptr: spsc::QueuePtr<T>,
    local_head: usize,
    local_tail: usize,
    futex: NonNull<AtomicU32>,
    shards: ParkingShardsPtr<T>,
    max_shards: usize,
    shard: usize,
}

impl<T> Receiver<T> {
    pub(crate) fn new(shards: ParkingShardsPtr<T>, max_shards: NonZeroUsize) -> Self {
        Self::init(shards, max_shards.get(), 0, 2).unwrap()
    }

    /// Attempts to clone the receiver.
    ///
    /// Returns `Some(Receiver)` if there is an available shard, or `None` if
    /// all shards are already occupied.
    ///
    /// This scans the shard table and may touch up to `max_shards` atomics. Prefer
    /// creating long-lived receivers instead of cloning and dropping in a hot path.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> Option<Self> {
        Self::init(
            self.shards.clone(),
            self.max_shards,
            self.shard.wrapping_add(1),
            self.ptr.ref_count() - 1,
        )
    }

    fn init(
        shards: ParkingShardsPtr<T>,
        max_shards: usize,
        start_shard: usize,
        free_ref_count: usize,
    ) -> Option<Self> {
        for offset in 0..max_shards {
            let shard = start_shard.wrapping_add(offset) % max_shards;
            if let Some(shard_ptr) = shards.try_claim_queue_ptr(shard, free_ref_count) {
                let local_head = shard_ptr.head().load(Ordering::Acquire);
                let local_tail = shard_ptr.tail().load(Ordering::Acquire);
                let futex = NonNull::from(shards.futex());

                return Some(Self {
                    ptr: shard_ptr,
                    local_head,
                    local_tail,
                    futex,
                    shards,
                    max_shards,
                    shard,
                });
            }
        }

        None
    }

    /// Receives a value, parking if the shard is empty.
    ///
    /// After a brief spin and yield phase, parks on the shared futex.
    /// The sender wakes parked receivers after writing.
    pub fn recv(&mut self) -> T {
        let mut backoff = crate::ParkingBackoff::new(16, 4);
        while self.local_head == self.local_tail {
            if backoff.backoff() {
                // Announce intent to park (Dekker pattern)
                self.futex().store(1, Ordering::SeqCst);
                // Recheck after announcing
                self.load_tail();
                if self.local_head == self.local_tail {
                    atomic_wait::wait(self.futex(), 1);
                }
            }
            self.load_tail();
        }

        let ret = unsafe { self.ptr.get(self.local_head) };
        let new_head = self.local_head.wrapping_add(1);
        self.store_head(new_head);
        self.local_head = new_head;

        ret
    }

    /// Attempts to receive without blocking.
    pub fn try_recv(&mut self) -> Option<T> {
        if self.local_head == self.local_tail {
            self.load_tail();
            if self.local_head == self.local_tail {
                return None;
            }
        }

        let ret = unsafe { self.ptr.get(self.local_head) };
        let new_head = self.local_head.wrapping_add(1);
        self.store_head(new_head);
        self.local_head = new_head;

        Some(ret)
    }

    /// Returns a [`ReadGuard`](crate::read_guard::ReadGuard) for batch reading.
    pub fn read_guard(&mut self) -> crate::read_guard::ReadGuard<'_, Self> {
        crate::read_guard::ReadGuard::new(self)
    }

    #[inline(always)]
    fn futex(&self) -> &AtomicU32 {
        unsafe { self.futex.as_ref() }
    }

    #[inline(always)]
    fn store_head(&self, value: usize) {
        self.ptr.head().store(value, Ordering::Release);
    }

    #[inline(always)]
    fn load_tail(&mut self) {
        self.local_tail = self.ptr.tail().load(Ordering::Acquire);
    }
}

unsafe impl<T> Send for Receiver<T> {}

/// # Safety
///
/// The implementation delegates to the single shard's SPSC QueuePtr.
unsafe impl<T> BatchReader for Receiver<T> {
    type Item = T;

    fn read_buffer(&mut self) -> &[T] {
        let mut available = self.local_tail.wrapping_sub(self.local_head);

        if available == 0 {
            self.load_tail();
            available = self.local_tail.wrapping_sub(self.local_head);
        }

        let start = self.local_head & self.ptr.mask;
        let contiguous = self.ptr.capacity - start;
        let len = available.min(contiguous);

        unsafe {
            let ptr = self.ptr.exact_at(start);
            core::slice::from_raw_parts(ptr.as_ptr(), len)
        }
    }

    unsafe fn advance(&mut self, n: usize) {
        #[cfg(debug_assertions)]
        {
            let start = self.local_head & self.ptr.mask;
            let contiguous = self.ptr.capacity - start;
            let available = contiguous.min(self.local_tail.wrapping_sub(self.local_head));
            assert!(
                n <= available,
                "advancing ({n}) more than available space ({available})"
            );
        }

        let new_head = self.local_head.wrapping_add(n);
        self.store_head(new_head);
        self.local_head = new_head;
    }
}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use core::num::NonZeroUsize;

    use super::*;

    #[test]
    fn clone_does_not_claim_live_receiver_shard_after_sender_drop() {
        let (tx, rx0) = super::super::channel::<usize>(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(4).unwrap(),
        );
        let rx1 = rx0.clone().unwrap();

        assert!(rx1.clone().is_none());

        let stale_free_ref_count = rx1.ptr.ref_count() - 1;
        let start_shard = rx1.shard.wrapping_add(1);
        let shards = rx1.shards.clone();
        let max_shards = rx1.max_shards;

        drop(tx);

        assert!(
            Receiver::init(shards, max_shards, start_shard, stale_free_ref_count).is_none(),
            "stale refcount let a second receiver claim an already occupied shard"
        );
    }
}
