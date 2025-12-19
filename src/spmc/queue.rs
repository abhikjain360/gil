use std::{
    marker::PhantomData,
    mem::{align_of, offset_of, size_of},
    num::NonZeroUsize,
    ptr::NonNull,
};

use crate::{
    alloc,
    atomic::{AtomicUsize, Ordering},
    cell::{Cell, CellPtr},
    padded::Padded,
};

/// # Invariants
/// - head should always point to the place where we can read next from.
// avoid re-ordering fields
#[repr(C)]
struct Queue {
    head: Padded<AtomicUsize>,
    rc: AtomicUsize,
}

pub(crate) struct QueuePtr<T> {
    ptr: NonNull<Queue>,
    buffer: NonNull<Cell<T>>,
    pub(crate) mask: usize,
    pub(crate) capacity: usize,
    _marker: PhantomData<T>,
}

impl<T> Clone for QueuePtr<T> {
    fn clone(&self) -> Self {
        let rc = unsafe { _field!(Queue, self.ptr, rc, AtomicUsize).as_ref() };
        rc.fetch_add(1, Ordering::AcqRel);
        Self {
            ptr: self.ptr,
            buffer: self.buffer,
            mask: self.mask,
            capacity: self.capacity,
            _marker: PhantomData,
        }
    }
}

impl<T> QueuePtr<T> {
    pub(crate) fn with_size(size: NonZeroUsize) -> Self {
        let capacity = size.get().next_power_of_two();

        let (layout, buffer_offset) = Self::layout(capacity);

        let ptr = unsafe { alloc::alloc(layout) } as *mut Queue;
        let Some(ptr) = NonNull::new(ptr) else {
            std::alloc::handle_alloc_error(layout);
        };

        let buffer = unsafe {
            NonNull::new_unchecked(ptr.as_ptr().byte_add(buffer_offset).cast::<Cell<T>>())
        };

        unsafe {
            ptr.write(Queue {
                head: Padded::new(AtomicUsize::new(0)),
                rc: AtomicUsize::new(1),
            });
        };

        let buffer_slice = unsafe { std::slice::from_raw_parts_mut(buffer.as_ptr(), capacity) };
        for (idx, cell) in buffer_slice.iter_mut().enumerate() {
            cell.epoch.store(idx, Ordering::Relaxed);
        }

        Self {
            ptr,
            buffer,
            _marker: PhantomData,
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
    pub(crate) fn head(&self) -> &AtomicUsize {
        unsafe { _field!(Queue, self.ptr, head.value, AtomicUsize).as_ref() }
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
        let rc = unsafe { _field!(Queue, self.ptr, rc, AtomicUsize).as_ref() };
        if rc.fetch_sub(1, Ordering::AcqRel) == 1 {
            let (layout, _) = Self::layout(self.capacity);

            let head = self.head().load(Ordering::Relaxed);

            if std::mem::needs_drop::<T>() {
                for idx in head..head.wrapping_add(self.capacity) {
                    let cell = self.at(idx);
                    let epoch = cell.epoch().load(Ordering::Relaxed);
                    if epoch > idx && (epoch & self.mask) == (idx & self.mask) {
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
