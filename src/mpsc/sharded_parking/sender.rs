use core::{mem::MaybeUninit, num::NonZeroUsize, ptr::NonNull};

use crate::{
    atomic::{AtomicU32, Ordering},
    spsc::{self, parking_shards::ParkingShardsPtr},
};

/// The sending half of a sharded parking MPSC channel.
///
/// Each sender is bound to a specific shard. Cloning a sender will attempt to bind the new
/// instance to a different, unused shard.
///
/// When the sender's shard is full, it parks on that shard's futex and is woken by the
/// receiver after it drains items.
pub struct Sender<T> {
    ptr: spsc::ShardQueuePtr<T>,
    local_head: usize,
    local_tail: usize,
    futex: NonNull<AtomicU32>,
    shards: ParkingShardsPtr<T>,
    max_shards: usize,
    shard: usize,
}

impl<T> Sender<T> {
    pub(crate) fn new(shards: ParkingShardsPtr<T>, max_shards: NonZeroUsize) -> Self {
        Self::init(shards, max_shards.get(), 0).unwrap()
    }

    /// Attempts to clone the sender.
    ///
    /// Returns `Some(Sender)` if there is an available shard to bind to, or `None` if
    /// all shards are already occupied.
    ///
    /// This scans the shard table and may touch up to `max_shards` atomics. Prefer
    /// creating long-lived senders instead of cloning and dropping in a hot path.
    #[expect(clippy::should_implement_trait)]
    pub fn clone(&self) -> Option<Self> {
        Self::init(
            self.shards.clone(),
            self.max_shards,
            self.shard.wrapping_add(1),
        )
    }

    pub(crate) fn init(
        shards: ParkingShardsPtr<T>,
        max_shards: usize,
        start_shard: usize,
    ) -> Option<Self> {
        for offset in 0..max_shards {
            let shard = start_shard.wrapping_add(offset) % max_shards;
            if let Some(shard_ptr) = shards.claim_producer_queue_ptr(shard) {
                let local_head = shard_ptr.head().load(Ordering::Acquire);
                let local_tail = shard_ptr.tail().load(Ordering::Acquire);
                let futex = NonNull::from(shards.futex(shard));

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

    /// Sends a value into the channel, parking if the shard is full.
    ///
    /// After a brief spin and yield phase, the sender parks on its shard's futex.
    /// The receiver wakes parked senders after draining items.
    pub fn send(&mut self, value: T) {
        let mut backoff = crate::ParkingBackoff::new(16, 4);
        while self.is_full() {
            if backoff.backoff() {
                // Announce intent to park (Dekker pattern)
                self.futex().store(1, Ordering::SeqCst);
                // Recheck after announcing
                self.load_head();
                if self.is_full() {
                    atomic_wait::wait(self.futex(), 1);
                }
            }
            self.load_head();
        }

        let new_tail = self.local_tail.wrapping_add(1);
        unsafe { self.ptr.set(self.local_tail, value) };
        self.store_tail(new_tail);
        self.local_tail = new_tail;
    }

    /// Attempts to send a value into the channel without blocking.
    ///
    /// Returns `Ok(())` if the value was sent, or `Err(value)` if the shard's queue is full.
    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        if self.is_full() {
            self.load_head();
            if self.is_full() {
                return Err(value);
            }
        }

        let new_tail = self.local_tail.wrapping_add(1);
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
    fn is_full(&self) -> bool {
        self.local_tail.wrapping_sub(self.local_head) >= self.ptr.size
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

unsafe impl<T: Send> Send for Sender<T> {}

#[cfg(all(test, not(feature = "loom")))]
mod test {
    use core::num::NonZeroUsize;

    #[test]
    fn clone_does_not_claim_live_sender_shard_after_receiver_drop() {
        let (tx0, rx) = super::super::channel::<usize>(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(4).unwrap(),
        );
        let tx1 = tx0.clone().unwrap();

        assert!(tx1.clone().is_none());

        drop(rx);
        assert!(tx1.clone().is_none());
    }
}
