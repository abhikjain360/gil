use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

use crate::{
    alloc,
    atomic::{AtomicUsize, Ordering},
};

pub trait Ownership {
    type State;
    type Handle: Copy;

    fn initial_state() -> Self::State;
    fn initial_handle() -> Self::Handle;
    fn try_acquire(state: &Self::State, handle: Self::Handle) -> bool;
    fn release(state: &Self::State, handle: Self::Handle) -> bool;
}

pub struct RefCounted;

impl Ownership for RefCounted {
    type State = AtomicUsize;
    type Handle = ();

    fn initial_state() -> Self::State {
        AtomicUsize::new(1)
    }

    fn initial_handle() -> Self::Handle {}

    fn try_acquire(state: &Self::State, _handle: Self::Handle) -> bool {
        state.fetch_add(1, Ordering::AcqRel);
        true
    }

    fn release(state: &Self::State, _handle: Self::Handle) -> bool {
        state.fetch_sub(1, Ordering::AcqRel) == 1
    }
}

pub struct ShardOwnership;

impl ShardOwnership {
    // Sharded SPSC queues have at most one table owner, one producer, and one consumer.
    // Keeping owner identity in the same atomic as lifetime avoids inferring occupancy
    // from refcount values that change when the opposite endpoint drops.
    pub(crate) const TABLE: usize = 1 << 0;
    pub(crate) const PRODUCER: usize = 1 << 1;
    pub(crate) const CONSUMER: usize = 1 << 2;
}

impl Ownership for ShardOwnership {
    type State = AtomicUsize;
    type Handle = usize;

    fn initial_state() -> Self::State {
        AtomicUsize::new(Self::TABLE)
    }

    fn initial_handle() -> Self::Handle {
        Self::TABLE
    }

    fn try_acquire(state: &Self::State, handle: Self::Handle) -> bool {
        let mut current = state.load(Ordering::Acquire);
        loop {
            if current & handle != 0 {
                return false;
            }

            match state.compare_exchange_weak(
                current,
                current | handle,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    fn release(state: &Self::State, handle: Self::Handle) -> bool {
        let previous = state.fetch_and(!handle, Ordering::AcqRel);
        debug_assert_ne!(previous & handle, 0);
        previous == handle
    }
}

#[repr(C)]
pub(crate) struct Queue<H, T, O: Ownership = RefCounted> {
    pub(crate) head: H,
    pub(crate) tail: T,
    ownership: O::State,
}

/// Teardown policy: drops the in-flight items (sent but not yet received) when
/// the last handle to a queue goes away.
pub trait DropInFlight<H, T, I> {
    /// # Safety
    ///
    /// Must only be called once, on teardown, when no endpoint can touch the
    /// buffer anymore. `at` must return valid (possibly uninitialised) slots for
    /// any index.
    unsafe fn drop_in_flight(head: &H, tail: &T, capacity: usize, at: impl Fn(usize) -> NonNull<I>);
}

pub struct QueuePtr<H, T, I, G: DropInFlight<H, T, I>, O: Ownership = RefCounted> {
    pub(crate) ptr: NonNull<Queue<H, T, O>>,
    pub(crate) buffer: NonNull<I>,
    pub(crate) size: usize,
    pub(crate) mask: usize,
    pub(crate) capacity: usize,
    owner: O::Handle,
    _marker: PhantomData<G>,
}

impl<H, T, I, G: DropInFlight<H, T, I>> Clone for QueuePtr<H, T, I, G, RefCounted> {
    fn clone(&self) -> Self {
        self.try_clone_as(()).unwrap()
    }
}

impl<H, T, I, G, O> QueuePtr<H, T, I, G, O>
where
    H: Default,
    T: Default,
    G: DropInFlight<H, T, I>,
    O: Ownership,
{
    pub(crate) fn with_size(size: NonZeroUsize) -> Self {
        // Round up to power of 2 so we can use mask
        let size = size.get();
        let capacity = size.next_power_of_two();

        let (layout, buffer_offset) = Self::layout(capacity);

        // SAFETY: capacity > 0, so layout is non-zero too
        let Some(ptr) = NonNull::new(unsafe { alloc::alloc(layout) }) else {
            alloc::handle_alloc_error(layout);
        };
        let ptr = ptr.cast::<Queue<H, T, O>>();

        // calculate buffer pointer
        // SAFETY: `ptr` is already checked by NonNull::new above, so this is guaranteed to be
        // valid ptr too
        let buffer =
            unsafe { NonNull::new_unchecked(ptr.as_ptr().byte_add(buffer_offset).cast::<I>()) };

        unsafe {
            ptr.write(Queue {
                head: H::default(),
                tail: T::default(),

                ownership: O::initial_state(),
            });
        };

        Self {
            ptr,
            buffer,
            size,
            capacity,
            mask: capacity - 1,
            owner: O::initial_handle(),
            _marker: PhantomData,
        }
    }
}

pub(crate) trait Initializer {
    type Item;

    fn initialize(idx: usize, item: &mut Self::Item);
}

impl<H, T, I, G, O> QueuePtr<H, T, I, G, O>
where
    G: DropInFlight<H, T, I>,
    O: Ownership,
{
    /// Shared reference to the queue header.
    ///
    /// Sound because the header is initialised at allocation, outlives every
    /// handle, and all of its fields are interior-mutable (atomics/wakers).
    #[inline(always)]
    pub(crate) fn header(&self) -> &Queue<H, T, O> {
        unsafe { self.ptr.as_ref() }
    }

    #[inline(always)]
    fn ownership(&self) -> &O::State {
        &self.header().ownership
    }

    pub(crate) fn try_clone_as(&self, owner: O::Handle) -> Option<Self> {
        if O::try_acquire(self.ownership(), owner) {
            Some(Self {
                ptr: self.ptr,
                buffer: self.buffer,
                size: self.size,
                mask: self.mask,
                capacity: self.capacity,
                owner,
                _marker: PhantomData,
            })
        } else {
            None
        }
    }

    fn layout(capacity: usize) -> (alloc::Layout, usize) {
        let header_layout = alloc::Layout::from_size_align(
            size_of::<Queue<H, T, O>>(),
            align_of::<Queue<H, T, O>>(),
        )
        .unwrap();
        let buffer_layout = alloc::Layout::array::<I>(capacity).unwrap();
        let (layout, offset) = header_layout.extend(buffer_layout).unwrap();
        (layout.pad_to_align(), offset)
    }

    pub(crate) fn initialize<Z: Initializer<Item = I>>(&self) {
        for i in 0..self.capacity {
            Z::initialize(i, unsafe { self.exact_at(i).as_mut() });
        }
    }

    #[inline(always)]
    pub(crate) unsafe fn exact_at(&self, index: usize) -> NonNull<I> {
        unsafe { NonNull::new_unchecked(self.buffer.as_ptr().add(index)) }
    }

    #[inline(always)]
    pub(crate) fn at(&self, index: usize) -> NonNull<I> {
        unsafe { self.exact_at(index & self.mask) }
    }
}

impl<H, T, I, G, O> Drop for QueuePtr<H, T, I, G, O>
where
    G: DropInFlight<H, T, I>,
    O: Ownership,
{
    fn drop(&mut self) {
        if O::release(self.ownership(), self.owner) {
            let (layout, _) = Self::layout(self.capacity);

            let header = self.header();
            unsafe { G::drop_in_flight(&header.head, &header.tail, self.capacity, |i| self.at(i)) };

            unsafe {
                self.ptr.drop_in_place();
                alloc::dealloc(self.ptr.cast().as_ptr(), layout);
            }
        }
    }
}
