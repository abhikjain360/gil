use core::num::NonZeroUsize;

use crate::{
    atomic::Ordering,
    read_guard::BatchReader,
    spsc::{self, shards::ShardsPtr},
};

/// The receiving half of a sharded SPMC channel.
///
/// Each receiver is bound to a specific shard. Cloning a receiver will attempt to bind
/// the new instance to a different, unused shard.
pub struct Receiver<T> {
    ptr: spsc::ShardQueuePtr<T>,
    local_head: usize,
    local_tail: usize,
    shards: ShardsPtr<T>,
    max_shards: usize,
    shard: usize,
}

impl<T> Receiver<T> {
    pub(crate) fn new(shards: ShardsPtr<T>, max_shards: NonZeroUsize) -> Self {
        Self::init(shards, max_shards.get(), 0).unwrap()
    }

    /// Attempts to clone the receiver.
    ///
    /// Returns `Some(Receiver)` if there is an available shard to bind to, or `None` if
    /// all shards are already occupied.
    ///
    /// This scans the shard table and may touch up to `max_shards` atomics. Prefer
    /// creating long-lived receivers instead of cloning and dropping in a hot path.
    #[expect(clippy::should_implement_trait)]
    pub fn clone(&self) -> Option<Self> {
        Self::init(
            self.shards.clone(),
            self.max_shards,
            self.shard.wrapping_add(1),
        )
    }

    fn init(shards: ShardsPtr<T>, max_shards: usize, start_shard: usize) -> Option<Self> {
        for offset in 0..max_shards {
            let shard = start_shard.wrapping_add(offset) % max_shards;
            if let Some(shard_ptr) = shards.claim_consumer_queue_ptr(shard) {
                let local_head = shard_ptr.head().load(Ordering::Acquire);
                let local_tail = shard_ptr.tail().load(Ordering::Acquire);

                return Some(Self {
                    ptr: shard_ptr,
                    local_head,
                    local_tail,
                    shards,
                    max_shards,
                    shard,
                });
            }
        }

        None
    }

    /// Receives a value, spinning/yielding until one is available.
    pub fn recv(&mut self) -> T {
        let mut backoff = crate::Backoff::with_spin_count(128);
        loop {
            match self.try_recv() {
                None => backoff.backoff(),
                Some(ret) => return ret,
            }
        }
    }

    /// Attempts to receive a value from this receiver's shard without blocking.
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

    #[test]
    fn clone_does_not_claim_live_receiver_shard_after_sender_drop() {
        let (tx, rx0) = super::super::channel::<usize>(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(4).unwrap(),
        );
        let rx1 = rx0.clone().unwrap();

        assert!(rx1.clone().is_none());

        drop(tx);
        assert!(rx1.clone().is_none());
    }
}
