use core::{mem::MaybeUninit, num::NonZeroUsize, ptr::NonNull};

use crate::{
    Box,
    atomic::{AtomicU32, AtomicUsize, Ordering},
    spsc::{self, parking_shards::ParkingShardsPtr},
};

/// The sending half of a sharded parking MPSC channel.
///
/// Each sender is bound to a specific shard. Cloning a sender will attempt to bind the new
/// instance to a different, unused shard.
///
/// When the sender's shard is full, it parks on a shared futex and is woken by the
/// receiver after it drains items.
pub struct Sender<T> {
    ptr: spsc::QueuePtr<T>,
    local_head: usize,
    local_tail: usize,
    futex: NonNull<AtomicU32>,
    shards: ParkingShardsPtr<T>,
    num_senders: NonNull<AtomicUsize>,
    max_shards: usize,
}

impl<T> Sender<T> {
    pub(crate) fn new(shards: ParkingShardsPtr<T>, max_shards: NonZeroUsize) -> Self {
        let num_senders_ptr = Box::into_raw(Box::new(AtomicUsize::new(0)));
        unsafe {
            let num_senders = NonNull::new_unchecked(num_senders_ptr);
            Self::init(shards, max_shards.get(), num_senders).unwrap_unchecked()
        }
    }

    /// Attempts to clone the sender.
    ///
    /// Returns `Some(Sender)` if there is an available shard to bind to, or `None` if
    /// all shards are already occupied.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> Option<Self> {
        unsafe { Self::init(self.shards.clone(), self.max_shards, self.num_senders) }
    }

    pub(crate) unsafe fn init(
        shards: ParkingShardsPtr<T>,
        max_shards: usize,
        num_senders: NonNull<AtomicUsize>,
    ) -> Option<Self> {
        let num_senders_ref = unsafe { num_senders.as_ref() };
        let next_shard = num_senders_ref.fetch_add(1, Ordering::AcqRel);
        if next_shard >= max_shards {
            num_senders_ref.fetch_sub(1, Ordering::AcqRel);
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
            num_senders,
            max_shards,
        })
    }

    /// Sends a value into the channel, parking if the shard is full.
    ///
    /// After a brief spin and yield phase, the sender parks on a shared futex.
    /// The receiver wakes parked senders after draining items.
    pub fn send(&mut self, value: T) {
        let new_tail = self.local_tail.wrapping_add(1);

        let mut backoff = crate::ParkingBackoff::new(16, 4);
        while new_tail > self.max_tail() {
            if backoff.backoff() {
                // Announce intent to park (Dekker pattern)
                self.futex().store(1, Ordering::SeqCst);
                // Recheck after announcing
                self.load_head();
                if new_tail > self.max_tail() {
                    atomic_wait::wait(self.futex(), 1);
                }
            }
            self.load_head();
        }

        unsafe { self.ptr.set(self.local_tail, value) };
        self.store_tail(new_tail);
        self.local_tail = new_tail;
    }

    /// Attempts to send a value into the channel without blocking.
    ///
    /// Returns `Ok(())` if the value was sent, or `Err(value)` if the shard's queue is full.
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        let new_tail = self.local_tail.wrapping_add(1);

        if new_tail > self.max_tail() {
            self.load_head();
            if new_tail > self.max_tail() {
                return Err(value);
            }
        }

        unsafe { self.ptr.set(self.local_tail, value) };
        self.store_tail(new_tail);
        self.local_tail = new_tail;

        Ok(())
    }

    /// Returns a mutable slice of the internal write buffer for batched sending.
    ///
    /// After writing to the buffer, call [`commit`](Sender::commit) to make the items
    /// visible to the receiver.
    pub fn write_buffer(&mut self) -> &mut [MaybeUninit<T>] {
        let mut available = self.ptr.size - self.local_tail.wrapping_sub(self.local_head);

        if available == 0 {
            self.load_head();
            available = self.ptr.size - self.local_tail.wrapping_sub(self.local_head);
        }

        let start = self.local_tail & self.ptr.mask;
        let contiguous = self.ptr.capacity - start;
        let len = available.min(contiguous);

        unsafe {
            let ptr = self.ptr.exact_at(start).cast();
            core::slice::from_raw_parts_mut(ptr.as_ptr(), len)
        }
    }

    /// Commits `len` elements from the write buffer to the channel.
    ///
    /// # Safety
    ///
    /// The caller must ensure that at least `len` elements in the write buffer have been initialized.
    pub unsafe fn commit(&mut self, len: usize) {
        #[cfg(debug_assertions)]
        {
            let start = self.local_tail & self.ptr.mask;
            let contiguous = self.ptr.capacity - start;
            let available =
                contiguous.min(self.ptr.size - self.local_tail.wrapping_sub(self.local_head));
            assert!(
                len <= available,
                "advancing ({len}) more than available space ({available})"
            );
        }

        let new_tail = self.local_tail.wrapping_add(len);
        self.store_tail(new_tail);
        self.local_tail = new_tail;
    }

    #[inline(always)]
    fn futex(&self) -> &AtomicU32 {
        unsafe { self.futex.as_ref() }
    }

    #[inline(always)]
    fn max_tail(&self) -> usize {
        self.local_head.wrapping_add(self.ptr.size)
    }

    #[inline(always)]
    fn store_tail(&self, value: usize) {
        self.ptr.tail().store(value, Ordering::Release);
    }

    #[inline(always)]
    fn load_head(&mut self) {
        self.local_head = self.ptr.head().load(Ordering::Acquire);
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let num_senders_ref = unsafe { self.num_senders.as_ref() };
        if num_senders_ref.fetch_sub(1, Ordering::AcqRel) == 1 {
            _ = unsafe { Box::from_raw(self.num_senders.as_ptr()) };
        }
    }
}

unsafe impl<T> Send for Sender<T> {}
