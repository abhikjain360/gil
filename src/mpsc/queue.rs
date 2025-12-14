use std::{
    marker::PhantomData,
    mem::{align_of, offset_of, size_of},
    num::NonZeroUsize,
    ptr::NonNull,
};

use crate::{
    alloc,
    atomic::{AtomicUsize, Ordering},
    mpsc::cell::{Cell, CellPtr},
    padded::Padded,
};

macro_rules! _field {
    ($ptr:expr, $($path:tt).+) => {
        $ptr.byte_add(offset_of!(Queue, $($path).+))
    };

    ($ptr:expr, $($path:tt).+, $field_ty:ty) => {
        $ptr.byte_add(offset_of!(Queue, $($path).+)).cast::<$field_ty>()
    };
}

/// # Invariants
/// - tail should always point to the place where we can write next to.
// avoid re-ordering fields
#[repr(C)]
struct Queue {
    tail: Padded<AtomicUsize>,
    rc: AtomicUsize,
}

pub(crate) struct QueuePtr<T> {
    ptr: NonNull<Queue>,
    buffer: NonNull<Cell<T>>,
    pub(crate) size: usize,
    pub(crate) mask: usize,
    pub(crate) capacity: usize,
    _marker: PhantomData<T>,
}

impl<T> Clone for QueuePtr<T> {
    fn clone(&self) -> Self {
        let rc = unsafe { _field!(self.ptr, rc, AtomicUsize).as_ref() };
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

impl<T> QueuePtr<T> {
    pub(crate) fn with_size(size: NonZeroUsize) -> Self {
        // Allocate exactly capacity + 1 slots (one slot is always empty to distinguish full from empty)
        let size = size.get();
        let capacity = size.next_power_of_two();

        let (layout, buffer_offset) = Self::layout(capacity);

        // SAFETY: capacity > 0, so layout is non-zero too
        let ptr = unsafe { alloc::alloc(layout) } as *mut Queue;
        let Some(ptr) = NonNull::new(ptr) else {
            std::alloc::handle_alloc_error(layout);
        };

        // calculate buffer pointer
        // SAFETY: `ptr` is already checked by NonNull::new above, so this is guaranteed to be
        // valid ptr too
        let buffer = unsafe {
            NonNull::new_unchecked(ptr.as_ptr().byte_add(buffer_offset).cast::<Cell<T>>())
        };

        // SAFETY: just allocated it and checked for NonNull
        unsafe {
            ptr.write(Queue {
                tail: Padded::new(AtomicUsize::new(0)),
                rc: AtomicUsize::new(1),
            });
        };

        // SAFETY: we just allocated it, and atomics are safe to access without initialisation
        let buffer_slice = unsafe { std::slice::from_raw_parts_mut(buffer.as_ptr(), capacity) };
        for (idx, cell) in buffer_slice.iter_mut().enumerate() {
            cell.epoch.store(idx, Ordering::Relaxed);
        }

        Self {
            ptr,
            buffer,
            _marker: PhantomData,
            size,
            capacity,
            mask: capacity - 1,
        }
    }

    fn layout(capacity: usize) -> (alloc::Layout, usize) {
        let header_layout =
            alloc::Layout::from_size_align(size_of::<Queue>(), align_of::<Queue>()).unwrap();
        let buffer_layout = alloc::Layout::array::<Cell<T>>(capacity).unwrap();
        header_layout.extend(buffer_layout).unwrap()
    }

    #[inline(always)]
    pub(crate) fn tail(&self) -> &AtomicUsize {
        unsafe { _field!(self.ptr, tail.value, AtomicUsize).as_ref() }
    }

    #[inline(always)]
    pub(crate) fn exact_at(&self, index: usize) -> CellPtr<T> {
        debug_assert!(index < self.capacity);

        unsafe { self.buffer.add(index) }.into()
    }

    #[inline(always)]
    pub(crate) fn at(&self, index: usize) -> CellPtr<T> {
        self.exact_at(index & self.mask)
    }
}

impl<T> Drop for QueuePtr<T> {
    fn drop(&mut self) {
        let rc = unsafe { _field!(self.ptr, rc, AtomicUsize).as_ref() };
        if rc.fetch_sub(1, Ordering::AcqRel) == 1 {
            let (layout, _) = Self::layout(self.capacity);

            let tail = self.tail().load(Ordering::Relaxed);

            if std::mem::needs_drop::<T>() {
                for idx in tail.saturating_sub(self.capacity)..tail {
                    let cell = self.at(idx);
                    if cell.epoch().load(Ordering::Relaxed) == idx.wrapping_add(1) {
                        unsafe { cell.drop_in_place() };
                    }
                }
            }

            unsafe {
                self.ptr.drop_in_place();
                alloc::dealloc(self.ptr.cast().as_ptr(), layout);
            }
        }
    }
}
