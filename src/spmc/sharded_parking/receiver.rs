use core::{num::NonZeroUsize, ptr::NonNull};

use crate::{
    Box,
    atomic::{AtomicU32, AtomicUsize, Ordering},
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
    num_receivers: NonNull<AtomicUsize>,
    max_shards: usize,
}

impl<T> Receiver<T> {
    pub(crate) fn new(shards: ParkingShardsPtr<T>, max_shards: NonZeroUsize) -> Self {
        let num_receivers_ptr = Box::into_raw(Box::new(AtomicUsize::new(0)));
        unsafe {
            let num_receivers = NonNull::new_unchecked(num_receivers_ptr);
            Self::init(shards, max_shards.get(), num_receivers).unwrap_unchecked()
        }
    }

    /// Attempts to clone the receiver.
    ///
    /// Returns `Some(Receiver)` if there is an available shard, or `None` if
    /// all shards are already occupied.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> Option<Self> {
        unsafe { Self::init(self.shards.clone(), self.max_shards, self.num_receivers) }
    }

    unsafe fn init(
        shards: ParkingShardsPtr<T>,
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
        let futex = NonNull::from(shards.futex());

        Some(Self {
            ptr: shard_ptr,
            local_head: 0,
            local_tail: 0,
            futex,
            shards,
            num_receivers,
            max_shards,
        })
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
