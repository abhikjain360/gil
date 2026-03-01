use core::ptr::NonNull;

use alloc_crate::vec::Vec;

/// Trait for queue receivers that support batch read operations.
///
/// # Safety
///
/// Implementations must ensure:
/// - `read_buffer` returns a valid slice of initialized items that remain
///   valid until `advance` is called
/// - `advance` publishes the new head to the producer (atomic store + wake)
/// - `release` releases any held resources (e.g., shard locks),
///   and is a no-op when no resources are held
///
/// # Ownership
///
/// Items are returned by **shared reference** — ownership is not transferred
/// to the caller. [`advance`](BatchReader::advance) makes slots reusable by
/// the producer but does **not** drop the previous values. For types that
/// implement [`Drop`], use [`ReadGuard::drain_into`] or the [`ReadGuard`]
/// [iterator](ReadGuard#impl-Iterator) to take ownership; plain
/// [`ReadGuard::as_slice`] combined with [`ReadGuard::advance`] will skip
/// the items' destructors.
pub unsafe trait BatchReader {
    type Item;

    /// Returns a slice of available contiguous items.
    ///
    /// Items are returned by shared reference — ownership is **not**
    /// transferred. See the [trait-level ownership note](BatchReader#ownership).
    fn read_buffer(&mut self) -> &[Self::Item];

    /// Advance head by `n` and publish to the producer (atomic store + wake).
    ///
    /// Does **not** drop the items being advanced past. The producer may
    /// immediately overwrite those slots.
    ///
    /// # Safety
    ///
    /// `n` must not exceed available unconsumed items.
    unsafe fn advance(&mut self, n: usize);

    /// Release held resources (e.g., shard lock).
    ///
    /// Default is a no-op for queue types that don't hold resources.
    ///
    /// # Safety
    ///
    /// Must only be called after all consumed items have been advanced past
    /// via [`advance`](BatchReader::advance).
    unsafe fn release(&mut self) {}
}

/// RAII guard for batch read operations on a queue receiver.
///
/// Accumulates consumed items and advances the head in a single atomic
/// store on drop.
pub struct ReadGuard<'a, R: BatchReader> {
    receiver: &'a mut R,
    data: NonNull<[R::Item]>,
    consumed: usize,
}

impl<'a, R: BatchReader> ReadGuard<'a, R> {
    /// Calls the receiver's [`read_buffer`](BatchReader::read_buffer) and
    /// converts the returned slice to [`NonNull`]. This is safe because the
    /// receiver keeps the underlying queue allocation alive.
    pub(crate) fn new(receiver: &'a mut R) -> Self {
        let slice = receiver.read_buffer();
        let data = NonNull::from_ref(slice);
        Self {
            receiver,
            data,
            consumed: 0,
        }
    }

    /// Number of remaining unconsumed items.
    #[inline]
    pub fn len(&self) -> usize {
        unsafe { self.data.as_ref() }.len() - self.consumed
    }

    /// Returns `true` if no items remain.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Zero-copy view of remaining unconsumed items.
    #[inline]
    pub fn as_slice(&self) -> &[R::Item] {
        unsafe { &self.data.as_ref()[self.consumed..] }
    }

    /// Mark `n` items as consumed.
    ///
    /// Does not publish to the producer until the guard is dropped.
    ///
    /// # Panics
    ///
    /// Panics if `n > self.len()`.
    pub fn advance(&mut self, n: usize) {
        assert!(n <= self.len(), "advancing beyond available items");
        self.consumed += n;
    }

    /// Move remaining items into the user's vec.
    ///
    /// Copies items in bulk via `memcpy`. After this call the guard's view
    /// is empty.
    pub fn drain_into(&mut self, dst: &mut Vec<R::Item>) -> usize {
        let slice = self.as_slice();
        let len = slice.len();
        if len == 0 {
            return 0;
        }
        dst.reserve(len);
        let dst_len = dst.len();
        unsafe {
            core::ptr::copy_nonoverlapping(slice.as_ptr(), dst.as_mut_ptr().add(dst_len), len);
            dst.set_len(dst_len + len);
        }
        self.consumed += len;
        len
    }

    /// Move remaining items into the user's raw pointer. This allows writing
    /// to any container not limited to Vec.
    ///
    /// Copies all unconsumed items (i.e. [`len`](Self::len) items) in bulk via
    /// `memcpy`. After this call the guard's view is empty.
    ///
    /// # Safety
    ///
    /// * `dst` must be non-null, properly aligned for `R::Item`, and valid for
    ///   writes of at least [`len`](Self::len) items. Writing fewer allocated
    ///   items is undefined behavior.
    /// * `dst` must not overlap with the queue's internal ring buffer.
    /// * The caller is responsible for any post-processing required on their
    ///   container, e.g. calling `set_len` or similar.
    pub unsafe fn drain_into_ptr(&mut self, dst: *mut R::Item) -> usize {
        let slice = self.as_slice();
        let len = slice.len();
        if len == 0 {
            return 0;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(slice.as_ptr(), dst, len);
        }
        self.consumed += len;
        len
    }

    /// Copy remaining items into the user's slice.
    ///
    /// Returns how many items were copied (min of available and `dst.len()`).
    pub fn copy_into(&mut self, dst: &mut [R::Item]) -> usize
    where
        R::Item: Copy,
    {
        let slice = self.as_slice();
        let n = slice.len().min(dst.len());
        if n > 0 {
            dst[..n].copy_from_slice(&slice[..n]);
            self.consumed += n;
        }
        n
    }

    /// Advance without bounds checking.
    ///
    /// # Safety
    ///
    /// `n` must not exceed [`len`](Self::len).
    pub unsafe fn advance_unchecked(&mut self, n: usize) {
        self.consumed += n;
    }
}

/// Yields items by copying them out of the buffer one at a time via
/// [`ptr::read`](core::ptr::read).
///
/// **Not zero-copy** — each [`next`](Iterator::next) performs a bitwise copy
/// of the item. For bulk operations, prefer [`drain_into`](ReadGuard::drain_into)
/// or [`copy_into`](ReadGuard::copy_into) which use `memcpy` and are
/// significantly more efficient. For zero-copy access, use
/// [`as_slice`](ReadGuard::as_slice).
impl<R: BatchReader> Iterator for ReadGuard<'_, R> {
    type Item = R::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let slice = unsafe { self.data.as_ref() };
        if self.consumed >= slice.len() {
            return None;
        }
        // SAFETY: consumed < slice.len(), pointer is valid and initialized.
        // The ring buffer does not drop items on advance, so this ptr::read
        // is the sole owner of the value.
        let item = unsafe { core::ptr::read(&slice[self.consumed]) };
        self.consumed += 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.len();
        (remaining, Some(remaining))
    }
}

impl<R: BatchReader> ExactSizeIterator for ReadGuard<'_, R> {}

impl<R: BatchReader> Drop for ReadGuard<'_, R> {
    fn drop(&mut self) {
        // If the original buffer was empty, no resources were acquired
        // (e.g., no shard lock for MPMC). Skip advance and release.
        if unsafe { self.data.as_ref() }.is_empty() {
            return;
        }
        unsafe {
            if self.consumed > 0 {
                self.receiver.advance(self.consumed);
            }
            self.receiver.release();
        }
    }
}
