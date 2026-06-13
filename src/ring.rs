//! The SPSC ring core.
//!
//! [`Ring`] is the owning handle to a single-allocation ring buffer (pointer +
//! geometry + refcount/role). [`Producer`] and [`Consumer`] add the local cursors
//! and hold the *only* copy of the wrapping push/pop algorithm. Every SPSC-flavoured
//! endpoint in the crate - spin, parking, and the sharded variants - is built on top
//! of these two types plus a visible wait loop.
//!
//! The cursors are generic over [`RingPtr`] so the same algorithm runs over both
//! ring layouts: the full SPSC layout (async wakers + futex) and the slimmer shard
//! layout (futex only). The layout decides what lives in the header; the cursor
//! types only need the head/tail atomics and the buffer geometry.
//!
//! ## Cursor naming
//!
//! Each endpoint owns one authoritative index and caches the other side's:
//!
//! * [`Producer`] owns `tail` (only it advances) and caches the consumer's head in
//!   `head_cache`, refreshed lazily on the slow branch.
//! * [`Consumer`] owns `head` and caches the producer's tail in `tail_cache`.
//!
//! The cached index only ever *lags* reality, and the opposite side only ever frees
//! more space / publishes more data, so a stale cache is always conservative: if it
//! says "not full"/"not empty" that is definitely true; if it says otherwise we
//! refresh from the atomic and recheck.

use core::{mem::MaybeUninit, ptr::NonNull};

use crate::{
    atomic::{AtomicUsize, Ordering},
    queue::{DropInFlight, Ownership, QueuePtr},
};

/// The default ring handle: the full SPSC layout (head/tail atomics plus optional
/// async wakers and futex) with its geometry and refcount.
pub(crate) type Ring<T> = crate::spsc::queue::QueuePtr<T>;

/// Implemented by header head types: projects out the head index atomic.
pub(crate) trait RingHead {
    fn head(&self) -> &AtomicUsize;
}

/// Implemented by header tail types: projects out the tail index atomic.
pub(crate) trait RingTail {
    fn tail(&self) -> &AtomicUsize;
}

/// What [`Producer`]/[`Consumer`] need from a ring handle: the two index atomics
/// and the buffer geometry/access. Blanket-implemented for every [`QueuePtr`]
/// whose header types implement [`RingHead`]/[`RingTail`].
///
/// Technically we don't really need this trait, the cursors could be generic over
/// [`QueuePtr`] directly but it helps with:
/// 1. keeping [`Producer`]/[`Consumer`] at one type parameter instead of five (plus
///    the four bounds those would carry)
/// 2. keeping slot access (`get`/`set`) out of [`QueuePtr`]'s crate-wide surface.
///    the slot read/write contract only matters to the cursor code in this file
pub(crate) trait RingPtr {
    type Item;

    fn head(&self) -> &AtomicUsize;
    fn tail(&self) -> &AtomicUsize;
    /// Usable capacity (the exact size requested by the user).
    fn size(&self) -> usize;
    /// Index mask; `slots - 1` where `slots` is the allocated power of two.
    fn mask(&self) -> usize;
    /// Allocated buffer length (`size` rounded up to a power of two).
    fn capacity(&self) -> usize;

    /// # Safety
    ///
    /// `index` must be already masked (`< capacity`).
    unsafe fn exact_at(&self, index: usize) -> NonNull<Self::Item>;

    /// Reads the value at `index` (masked here, pass the raw cursor).
    ///
    /// # Safety
    ///
    /// The slot must hold an initialised value that no one else reads.
    unsafe fn get(&self, index: usize) -> Self::Item;

    /// Writes `value` at `index` (masked here, pass the raw cursor).
    ///
    /// # Safety
    ///
    /// The slot must be free (outside the head..tail window).
    unsafe fn set(&self, index: usize, value: Self::Item);
}

impl<H, T, I, G, O> RingPtr for QueuePtr<H, T, I, G, O>
where
    H: RingHead,
    T: RingTail,
    G: DropInFlight<H, T, I>,
    O: Ownership,
{
    type Item = I;

    #[inline(always)]
    fn head(&self) -> &AtomicUsize {
        self.header().head.head()
    }

    #[inline(always)]
    fn tail(&self) -> &AtomicUsize {
        self.header().tail.tail()
    }

    #[inline(always)]
    fn size(&self) -> usize {
        self.size
    }

    #[inline(always)]
    fn mask(&self) -> usize {
        self.mask
    }

    #[inline(always)]
    fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline(always)]
    unsafe fn exact_at(&self, index: usize) -> NonNull<I> {
        unsafe { self.buffer.add(index) }
    }

    #[inline(always)]
    unsafe fn get(&self, index: usize) -> I {
        unsafe { self.buffer.add(index & self.mask).read() }
    }

    #[inline(always)]
    unsafe fn set(&self, index: usize, value: I) {
        unsafe { self.buffer.add(index & self.mask).write(value) }
    }
}

/// The producer cursor over a ring. Holds the one copy of the push algorithm.
pub(crate) struct Producer<R: RingPtr> {
    ring: R,
    /// Authoritative tail - only this endpoint advances it.
    tail: usize,
    /// Stale snapshot of the consumer's head, refreshed lazily.
    head_cache: usize,
}

impl<R: RingPtr> Producer<R> {
    /// Attach to a ring, loading the current head/tail from the atomics. On a fresh
    /// queue these are `0/0`, so this subsumes the from-scratch case; on a reused
    /// shard it picks up where the previous owner left off.
    #[inline(always)]
    pub(crate) fn attach(ring: R) -> Self {
        let tail = ring.tail().load(Ordering::Acquire);
        let head_cache = ring.head().load(Ordering::Acquire);
        Self {
            ring,
            tail,
            head_cache,
        }
    }

    #[cfg(any(feature = "std", feature = "async"))]
    #[inline(always)]
    pub(crate) fn ring(&self) -> &R {
        &self.ring
    }

    /// Full according to the cached head only. no refresh.
    #[inline(always)]
    pub(crate) fn is_full(&self) -> bool {
        self.tail.wrapping_sub(self.head_cache) >= self.ring.size()
    }

    /// Refresh the cached head from the atomic (slow branch only).
    #[inline(always)]
    pub(crate) fn refresh_head(&mut self) {
        self.head_cache = self.ring.head().load(Ordering::Acquire);
    }

    /// Refresh the cached head with `SeqCst`. Used by the async path to close the
    /// lost-wake window after registering a waker.
    #[cfg(feature = "async")]
    #[inline(always)]
    pub(crate) fn refresh_head_seqcst(&mut self) {
        self.head_cache = self.ring.head().load(Ordering::SeqCst);
    }

    /// Write `value` at the tail and publish it. Caller must ensure the ring is not
    /// full (checked via [`is_full`](Self::is_full) after a [`refresh_head`](Self::refresh_head)).
    #[inline(always)]
    pub(crate) fn push(&mut self, value: R::Item) {
        let new_tail = self.tail.wrapping_add(1);
        unsafe { self.ring.set(self.tail, value) };
        self.ring.tail().store(new_tail, Ordering::Release);
        self.tail = new_tail;
    }

    /// The one copy of the non-blocking push. Returns the value back on a full ring.
    #[inline(always)]
    pub(crate) fn try_push(&mut self, value: R::Item) -> Result<(), R::Item> {
        if self.is_full() {
            self.refresh_head();
            if self.is_full() {
                return Err(value);
            }
        }
        self.push(value);
        Ok(())
    }

    /// Contiguous free slots starting at the current tail (uninitialised).
    pub(crate) fn write_buffer(&mut self) -> &mut [MaybeUninit<R::Item>] {
        let mut available = self.ring.size() - self.tail.wrapping_sub(self.head_cache);
        if available == 0 {
            self.refresh_head();
            available = self.ring.size() - self.tail.wrapping_sub(self.head_cache);
        }

        let start = self.tail & self.ring.mask();
        let contiguous = self.ring.capacity() - start;
        let len = available.min(contiguous);

        unsafe {
            let ptr = self.ring.exact_at(start).cast();
            core::slice::from_raw_parts_mut(ptr.as_ptr(), len)
        }
    }

    /// Publish `len` items written via [`write_buffer`](Self::write_buffer).
    ///
    /// # Safety
    ///
    /// `len` must not exceed the most recent `write_buffer` slice length, and all
    /// `len` slots must be initialised.
    #[inline(always)]
    pub(crate) unsafe fn commit(&mut self, len: usize) {
        #[cfg(debug_assertions)]
        {
            let start = self.tail & self.ring.mask();
            let contiguous = self.ring.capacity() - start;
            let available =
                contiguous.min(self.ring.size() - self.tail.wrapping_sub(self.head_cache));
            assert!(
                len <= available,
                "advancing ({len}) more than available space ({available})"
            );
        }

        // the len can be just right at the edge of buffer, so we wrap just in case
        let new_tail = self.tail.wrapping_add(len);
        self.ring.tail().store(new_tail, Ordering::Release);
        self.tail = new_tail;
    }
}

/// The consumer cursor over a ring. Holds the one copy of the pop algorithm.
pub(crate) struct Consumer<R: RingPtr> {
    ring: R,
    /// Authoritative head. Only this endpoint advances it.
    head: usize,
    /// Stale snapshot of the producer's tail, refreshed lazily.
    tail_cache: usize,
}

impl<R: RingPtr> Consumer<R> {
    /// Attach to a ring, loading the current head/tail. See [`Producer::attach`].
    #[inline(always)]
    pub(crate) fn attach(ring: R) -> Self {
        let head = ring.head().load(Ordering::Acquire);
        let tail_cache = ring.tail().load(Ordering::Acquire);
        Self {
            ring,
            head,
            tail_cache,
        }
    }

    #[cfg(any(feature = "std", feature = "async"))]
    #[inline(always)]
    pub(crate) fn ring(&self) -> &R {
        &self.ring
    }

    /// Empty according to the cached tail only. no refresh.
    #[inline(always)]
    pub(crate) fn is_empty(&self) -> bool {
        self.head == self.tail_cache
    }

    /// Refresh the cached tail from the atomic (slow branch only).
    #[inline(always)]
    pub(crate) fn refresh_tail(&mut self) {
        self.tail_cache = self.ring.tail().load(Ordering::Acquire);
    }

    /// Refresh the cached tail with `SeqCst`. See [`Producer::refresh_head_seqcst`].
    #[cfg(feature = "async")]
    #[inline(always)]
    pub(crate) fn refresh_tail_seqcst(&mut self) {
        self.tail_cache = self.ring.tail().load(Ordering::SeqCst);
    }

    /// Read the value at the head and advance. Caller must ensure the ring is not
    /// empty (checked via [`is_empty`](Self::is_empty) after a [`refresh_tail`](Self::refresh_tail)).
    #[inline(always)]
    pub(crate) fn pop(&mut self) -> R::Item {
        // SAFETY: head != tail means the slot at head holds a valid initialised value.
        let value = unsafe { self.ring.get(self.head) };
        let new_head = self.head.wrapping_add(1);
        self.ring.head().store(new_head, Ordering::Release);
        self.head = new_head;
        value
    }

    /// `try_pop`'s emptiness check without the pop: refreshes the cached tail
    /// once on the slow branch. For the sharded receivers, which must *locate* a
    /// non-empty ring inside their round-robin scan and pop after it - popping
    /// inside the scan closure would route the value through an extra
    /// `Option<T>` return, a copy of `T` on the hot path for large payloads.
    #[inline(always)]
    pub(crate) fn has_items(&mut self) -> bool {
        if self.is_empty() {
            self.refresh_tail();
            if self.is_empty() {
                return false;
            }
        }
        true
    }

    /// The one copy of the non-blocking pop.
    #[inline(always)]
    pub(crate) fn try_pop(&mut self) -> Option<R::Item> {
        if !self.has_items() {
            return None;
        }
        Some(self.pop())
    }

    /// Re-sync both cursors from the atomics. Used when the head may have advanced
    /// underneath this cursor i.e. a sharded receiver sharing a shard with peers.
    #[inline(always)]
    pub(crate) fn resync(&mut self) {
        self.head = self.ring.head().load(Ordering::Acquire);
        if self.tail_cache < self.head {
            self.tail_cache = self.ring.tail().load(Ordering::Acquire);
        }
    }

    /// Contiguous available items starting at the current head, as raw parts.
    ///
    /// The sharded receivers use this instead of [`read_buffer`](Self::read_buffer):
    /// their round-robin scan needs to return the found slice from a closure, and a
    /// slice borrowing `&mut self` could not outlive it. The pointer stays valid for
    /// as long as the ring handle does and the items are not advanced past.
    #[inline]
    pub(crate) fn read_buffer_raw(&mut self) -> (NonNull<R::Item>, usize) {
        let mut available = self.tail_cache.wrapping_sub(self.head);
        if available == 0 {
            self.refresh_tail();
            available = self.tail_cache.wrapping_sub(self.head);
        }

        let start = self.head & self.ring.mask();
        let contiguous = self.ring.capacity() - start;
        let len = available.min(contiguous);

        // SAFETY: `start` is masked.
        (unsafe { self.ring.exact_at(start) }, len)
    }

    /// Contiguous available items starting at the current head.
    #[inline]
    pub(crate) fn read_buffer(&mut self) -> &[R::Item] {
        let (ptr, len) = self.read_buffer_raw();
        // SAFETY: the `head..head + len` window holds initialised items that only
        // this consumer reads until it advances past them.
        unsafe { core::slice::from_raw_parts(ptr.as_ptr(), len) }
    }

    /// Advance the head past `n` consumed items.
    ///
    /// # Safety
    ///
    /// `n` must not exceed the most recent [`read_buffer`](Self::read_buffer) length.
    #[inline(always)]
    pub(crate) unsafe fn advance(&mut self, n: usize) {
        #[cfg(debug_assertions)]
        {
            let start = self.head & self.ring.mask();
            let contiguous = self.ring.capacity() - start;
            let available = contiguous.min(self.tail_cache.wrapping_sub(self.head));
            assert!(
                n <= available,
                "advancing ({n}) more than available space ({available})"
            );
        }

        let new_head = self.head.wrapping_add(n);
        self.ring.head().store(new_head, Ordering::Release);
        self.head = new_head;
    }
}
