use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

use crate::{
    alloc,
    atomic::{AtomicUsize, Ordering},
};

#[repr(C)]
pub(crate) struct Queue<H, T> {
    pub(crate) head: H,
    pub(crate) tail: T,
    rc: AtomicUsize,
}

pub trait DropInitItems<H, T, I> {
    #[allow(clippy::missing_safety_doc)]
    unsafe fn drop_init_items(
        head: NonNull<H>,
        tail: NonNull<T>,
        capacity: usize,
        at: impl Fn(usize) -> NonNull<I>,
    );
}

pub struct QueuePtr<H, T, I, G: DropInitItems<H, T, I>> {
    pub(crate) ptr: NonNull<Queue<H, T>>,
    pub(crate) buffer: NonNull<I>,
    pub(crate) size: usize,
    pub(crate) mask: usize,
    pub(crate) capacity: usize,
    _marker: PhantomData<G>,
}

impl<H, T, I, G: DropInitItems<H, T, I>> Clone for QueuePtr<H, T, I, G> {
    fn clone(&self) -> Self {
        let rc = unsafe { _field!(Queue<H, T>, self.ptr, rc, AtomicUsize).as_ref() };
        rc.fetch_add(1, Ordering::AcqRel);
        Self {
            ptr: self.ptr,
            buffer: self.buffer,
            size: self.size,
            mask: self.mask,
            capacity: self.capacity,
            _marker: PhantomData,
        }
    }
}

impl<H, T, I, G> QueuePtr<H, T, I, G>
where
    H: Default,
    T: Default,
    G: DropInitItems<H, T, I>,
{
    pub(crate) fn with_size(size: NonZeroUsize) -> Self {
        // Allocate exactly capacity + 1 slots (one slot is always empty to distinguish full from empty)
        let size = size.get();
        let capacity = size.next_power_of_two();

        let (layout, buffer_offset) = Self::layout(capacity);

        // SAFETY: capacity > 0, so layout is non-zero too
        let Some(ptr) = NonNull::new(unsafe { alloc::alloc(layout) }) else {
            alloc::handle_alloc_error(layout);
        };
        let ptr = ptr.cast::<Queue<H, T>>();

        // calculate buffer pointer
        // SAFETY: `ptr` is already checked by NonNull::new above, so this is guaranteed to be
        // valid ptr too
        let buffer =
            unsafe { NonNull::new_unchecked(ptr.as_ptr().byte_add(buffer_offset).cast::<I>()) };

        unsafe {
            ptr.write(Queue {
                head: H::default(),
                tail: T::default(),

                rc: AtomicUsize::new(1),
            });
        };

        Self {
            ptr,
            buffer,
            size,
            capacity,
            mask: capacity - 1,
            _marker: PhantomData,
        }
    }
}

pub(crate) trait Initializer {
    type Item;

    fn initialize(idx: usize, item: &mut Self::Item);
}

impl<H, T, I, G: DropInitItems<H, T, I>> QueuePtr<H, T, I, G> {
    fn layout(capacity: usize) -> (alloc::Layout, usize) {
        let header_layout =
            alloc::Layout::from_size_align(size_of::<Queue<H, T>>(), align_of::<Queue<H, T>>())
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

    #[inline(always)]
    pub(crate) unsafe fn get(&self, index: usize) -> I {
        unsafe { self.at(index & self.mask).read() }
    }

    #[inline(always)]
    pub(crate) unsafe fn set(&self, index: usize, value: I) {
        unsafe { self.at(index & self.mask).write(value) }
    }
}

impl<H, T, I, G: DropInitItems<H, T, I>> Drop for QueuePtr<H, T, I, G> {
    fn drop(&mut self) {
        let rc = unsafe { _field!(Queue<H, T>, self.ptr, rc, AtomicUsize).as_ref() };
        if rc.fetch_sub(1, Ordering::AcqRel) == 1 {
            let (layout, _) = Self::layout(self.capacity);

            unsafe {
                G::drop_init_items(
                    _field!(Queue<H, T>, self.ptr, head, H),
                    _field!(Queue<H, T>, self.ptr, tail, T),
                    self.capacity,
                    |i| self.at(i),
                )
            };

            unsafe {
                self.ptr.drop_in_place();
                alloc::dealloc(self.ptr.cast().as_ptr(), layout);
            }
        }
    }
}
