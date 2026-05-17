use crate::{
    Backoff, Box,
    atomic::Ordering,
    read_guard::BatchReader,
    spsc::{self, parking_shards::ParkingShardsPtr},
};

/// The receiving half of a sharded parking MPSC channel.
///
/// The receiver polls all shards in round-robin fashion. After consuming items,
/// it checks that shard's futex and wakes its parked sender, if any.
pub struct Receiver<T> {
    ptrs: Box<[spsc::ShardQueuePtr<T>]>,
    local_heads: Box<[usize]>,
    local_tails: Box<[usize]>,
    max_shards: usize,
    next_shard: usize,
    shards: ParkingShardsPtr<T>,
}

impl<T> Receiver<T> {
    pub(crate) fn new(shards: ParkingShardsPtr<T>, max_shards: usize) -> Self {
        let mut ptrs = Box::new_uninit_slice(max_shards);
        for i in 0..max_shards {
            ptrs[i].write(shards.claim_consumer_queue_ptr(i).unwrap());
        }

        Self {
            ptrs: unsafe { ptrs.assume_init() },
            local_heads: core::iter::repeat_n(0, max_shards).collect(),
            local_tails: core::iter::repeat_n(0, max_shards).collect(),
            max_shards,
            next_shard: 0,
            shards,
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
        let start = self.next_shard;
        loop {
            let shard = self.next_shard;

            if self.local_heads[shard] == self.local_tails[shard] {
                self.load_tail(shard);
            }

            if self.local_heads[shard] != self.local_tails[shard] {
                let ret = unsafe { self.ptrs[shard].get(self.local_heads[shard]) };
                let new_head = self.local_heads[shard].wrapping_add(1);
                self.store_head(shard, new_head);
                self.local_heads[shard] = new_head;

                self.wake_senders(shard);

                return Some(ret);
            }

            self.next_shard += 1;
            if self.next_shard == self.max_shards {
                self.next_shard = 0;
            }

            if self.next_shard == start {
                return None;
            }
        }
    }

    /// Returns a [`ReadGuard`](crate::read_guard::ReadGuard) for batch reading.
    pub fn read_guard(&mut self) -> crate::read_guard::ReadGuard<'_, Self> {
        crate::read_guard::ReadGuard::new(self)
    }

    #[inline(always)]
    fn store_head(&self, shard: usize, value: usize) {
        self.ptrs[shard].head().store(value, Ordering::Release);
    }

    #[inline(always)]
    fn load_tail(&mut self, shard: usize) {
        self.local_tails[shard] = self.ptrs[shard].tail().load(Ordering::Acquire);
    }

    /// Dekker pattern: after store_head(Release), load futex with SeqCst.
    /// If this shard's sender is parked, wake it.
    #[inline(always)]
    fn wake_senders(&self, shard: usize) {
        let futex = self.shards.futex(shard);
        if futex.load(Ordering::SeqCst) != 0 {
            futex.store(0, Ordering::Relaxed);
            atomic_wait::wake_one(futex);
        }
    }
}

/// # Safety
///
/// The implementation delegates to per-shard SPSC QueuePtrs.
/// `read_buffer` polls shards round-robin and returns the first non-empty
/// contiguous slice. `advance` publishes the new head and wakes parked senders.
unsafe impl<T> BatchReader for Receiver<T> {
    type Item = T;

    fn read_buffer(&mut self) -> &[T] {
        let start = self.next_shard;
        loop {
            let shard = self.next_shard;

            let mut available = self.local_tails[shard].wrapping_sub(self.local_heads[shard]);
            if available == 0 {
                self.load_tail(shard);
                available = self.local_tails[shard].wrapping_sub(self.local_heads[shard]);
            }

            if available > 0 {
                let s = self.local_heads[shard] & self.ptrs[shard].mask;
                let contiguous = self.ptrs[shard].capacity - s;
                let len = available.min(contiguous);

                return unsafe {
                    let ptr = self.ptrs[shard].exact_at(s);
                    core::slice::from_raw_parts(ptr.as_ptr(), len)
                };
            }

            self.next_shard += 1;
            if self.next_shard == self.max_shards {
                self.next_shard = 0;
            }

            if self.next_shard == start {
                return &[];
            }
        }
    }

    unsafe fn advance(&mut self, n: usize) {
        let shard = self.next_shard;

        #[cfg(debug_assertions)]
        {
            let s = self.local_heads[shard] & self.ptrs[shard].mask;
            let contiguous = self.ptrs[shard].capacity - s;
            let available =
                contiguous.min(self.local_tails[shard].wrapping_sub(self.local_heads[shard]));
            assert!(
                n <= available,
                "advancing ({n}) more than available space ({available})"
            );
        }

        let new_head = self.local_heads[shard].wrapping_add(n);
        self.store_head(shard, new_head);
        self.local_heads[shard] = new_head;

        self.wake_senders(shard);
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
