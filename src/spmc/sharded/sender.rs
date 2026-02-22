use core::mem::MaybeUninit;

use crate::{
    Backoff, Box,
    atomic::Ordering,
    spsc::{self, shards::ShardsPtr},
};

/// The sending half of a sharded SPMC channel.
///
/// The sender writes to shards in strict round-robin fashion, ensuring even
/// distribution across all receivers.
pub struct Sender<T> {
    ptrs: Box<[spsc::QueuePtr<T>]>,
    local_heads: Box<[usize]>,
    local_tails: Box<[usize]>,
    max_shards: usize,
    next_shard: usize,
    _shards: ShardsPtr<T>,
}

impl<T> Sender<T> {
    pub(crate) fn new(shards: ShardsPtr<T>, max_shards: usize) -> Self {
        let mut ptrs = Box::new_uninit_slice(max_shards);
        for i in 0..max_shards {
            ptrs[i].write(shards.clone_queue_ptr(i));
        }

        Self {
            ptrs: unsafe { ptrs.assume_init() },
            local_heads: core::iter::repeat_n(0, max_shards).collect(),
            local_tails: core::iter::repeat_n(0, max_shards).collect(),
            max_shards,
            next_shard: 0,
            _shards: shards,
        }
    }

    /// Sends a value to the next shard in round-robin order, blocking if that shard is full.
    pub fn send(&mut self, value: T) {
        let shard = self.next_shard;
        let new_tail = self.local_tails[shard].wrapping_add(1);

        let mut backoff = Backoff::with_spin_count(128);
        while new_tail > self.max_tail(shard) {
            backoff.backoff();
            self.load_head(shard);
        }

        unsafe { self.ptrs[shard].set(self.local_tails[shard], value) };
        self.store_tail(shard, new_tail);
        self.local_tails[shard] = new_tail;

        self.next_shard += 1;
        if self.next_shard == self.max_shards {
            self.next_shard = 0;
        }
    }

    /// Attempts to send to the next shard without blocking.
    ///
    /// Returns `Err(value)` if the current target shard is full.
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        let shard = self.next_shard;
        let new_tail = self.local_tails[shard].wrapping_add(1);

        if new_tail > self.max_tail(shard) {
            self.load_head(shard);
            if new_tail > self.max_tail(shard) {
                return Err(value);
            }
        }

        unsafe { self.ptrs[shard].set(self.local_tails[shard], value) };
        self.store_tail(shard, new_tail);
        self.local_tails[shard] = new_tail;

        self.next_shard += 1;
        if self.next_shard == self.max_shards {
            self.next_shard = 0;
        }

        Ok(())
    }

    /// Returns a mutable slice of the write buffer for the current shard.
    ///
    /// After writing, call [`commit`](Sender::commit).
    pub fn write_buffer(&mut self) -> &mut [MaybeUninit<T>] {
        let shard = self.next_shard;
        let mut available =
            self.ptrs[shard].size - self.local_tails[shard].wrapping_sub(self.local_heads[shard]);

        if available == 0 {
            self.load_head(shard);
            available = self.ptrs[shard].size
                - self.local_tails[shard].wrapping_sub(self.local_heads[shard]);
        }

        let start = self.local_tails[shard] & self.ptrs[shard].mask;
        let contiguous = self.ptrs[shard].capacity - start;
        let len = available.min(contiguous);

        unsafe {
            let ptr = self.ptrs[shard].exact_at(start).cast();
            core::slice::from_raw_parts_mut(ptr.as_ptr(), len)
        }
    }

    /// Commits `len` elements from the write buffer of the current shard.
    ///
    /// # Safety
    ///
    /// The caller must ensure that at least `len` elements have been initialized.
    pub unsafe fn commit(&mut self, len: usize) {
        let shard = self.next_shard;

        #[cfg(debug_assertions)]
        {
            let start = self.local_tails[shard] & self.ptrs[shard].mask;
            let contiguous = self.ptrs[shard].capacity - start;
            let available = contiguous.min(
                self.ptrs[shard].size
                    - self.local_tails[shard].wrapping_sub(self.local_heads[shard]),
            );
            assert!(
                len <= available,
                "advancing ({len}) more than available space ({available})"
            );
        }

        let new_tail = self.local_tails[shard].wrapping_add(len);
        self.store_tail(shard, new_tail);
        self.local_tails[shard] = new_tail;

        // Advance to next shard
        self.next_shard += 1;
        if self.next_shard == self.max_shards {
            self.next_shard = 0;
        }
    }

    #[inline(always)]
    fn max_tail(&self, shard: usize) -> usize {
        self.local_heads[shard].wrapping_add(self.ptrs[shard].size)
    }

    #[inline(always)]
    fn store_tail(&self, shard: usize, value: usize) {
        self.ptrs[shard].tail().store(value, Ordering::Release);
    }

    #[inline(always)]
    fn load_head(&mut self, shard: usize) {
        self.local_heads[shard] = self.ptrs[shard].head().load(Ordering::Acquire);
    }
}

unsafe impl<T> Send for Sender<T> {}
