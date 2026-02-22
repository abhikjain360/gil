use core::{num::NonZeroUsize, ptr::NonNull};

use crate::{
    Box,
    atomic::{AtomicUsize, Ordering},
    read_guard::BatchReader,
    spsc::{self, shards::ShardsPtr},
};

/// The receiving half of a sharded SPMC channel.
///
/// Each receiver is bound to a specific shard. Cloning a receiver will attempt to bind
/// the new instance to a different, unused shard.
pub struct Receiver<T> {
    ptr: spsc::QueuePtr<T>,
    local_head: usize,
    local_tail: usize,
    shards: ShardsPtr<T>,
    num_receivers: NonNull<AtomicUsize>,
    max_shards: usize,
}

impl<T> Receiver<T> {
    pub(crate) fn new(shards: ShardsPtr<T>, max_shards: NonZeroUsize) -> Self {
        let num_receivers_ptr = Box::into_raw(Box::new(AtomicUsize::new(0)));
        unsafe {
            let num_receivers = NonNull::new_unchecked(num_receivers_ptr);
            Self::init(shards, max_shards.get(), num_receivers).unwrap_unchecked()
        }
    }

    /// Attempts to clone the receiver.
    ///
    /// Returns `Some(Receiver)` if there is an available shard to bind to, or `None` if
    /// all shards are already occupied.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> Option<Self> {
        unsafe { Self::init(self.shards.clone(), self.max_shards, self.num_receivers) }
    }

    unsafe fn init(
        shards: ShardsPtr<T>,
        max_shards: usize,
        num_receivers: NonNull<AtomicUsize>,
    ) -> Option<Self> {
        let num_receivers_ref = unsafe { num_receivers.as_ref() };
        let next_shard = num_receivers_ref.fetch_add(1, Ordering::AcqRel);
        if next_shard >= max_shards {
            num_receivers_ref.fetch_sub(1, Ordering::AcqRel);
            return None;
        }

        let shard_ptr = shards.clone_queue_ptr(next_shard);

        Some(Self {
            ptr: shard_ptr,
            local_head: 0,
            local_tail: 0,
            shards,
            num_receivers,
            max_shards,
        })
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

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let num_receivers_ref = unsafe { self.num_receivers.as_ref() };
        if num_receivers_ref.fetch_sub(1, Ordering::AcqRel) == 1 {
            _ = unsafe { Box::from_raw(self.num_receivers.as_ptr()) };
        }
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
