use core::marker::PhantomData;

use crate::{
    atomic::{AtomicUsize, Ordering},
    cell::{Cell, CellPtr},
    padded::Padded,
};

#[derive(Default)]
#[repr(C)]
pub(crate) struct Head {
    head: Padded<AtomicUsize>,
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct Tail {
    tail: Padded<AtomicUsize>,
}

pub(crate) struct GetInit;
impl<T> crate::DropInitItems<Head, Tail, Cell<T>> for GetInit {
    unsafe fn drop_init_items(
        _head: core::ptr::NonNull<Head>,
        tail: core::ptr::NonNull<Tail>,
        capacity: usize,
        at: impl Fn(usize) -> core::ptr::NonNull<Cell<T>>,
    ) {
        if !core::mem::needs_drop::<T>() {
            return;
        }

        let tail = unsafe { _field!(Tail, tail, tail.value, AtomicUsize).as_ref() }
            .load(Ordering::Relaxed);

        for i in 1..=capacity {
            let idx = tail.wrapping_sub(i);
            let cell = CellPtr::from(at(idx));
            if cell.epoch().load(Ordering::Relaxed) == idx.wrapping_add(1) {
                unsafe { cell.drop_in_place() };
            }
        }
    }
}

pub(crate) struct Initializer<T> {
    _marker: PhantomData<T>,
}

impl<T> crate::Initializer for Initializer<T> {
    type Item = Cell<T>;

    fn initialize(idx: usize, cell: &mut Self::Item) {
        cell.epoch.store(idx, Ordering::Relaxed);
    }
}

pub(crate) type Queue = crate::Queue<Head, Tail>;
pub(crate) type QueuePtr<T> = crate::QueuePtr<Head, Tail, Cell<T>, GetInit>;

impl<T> QueuePtr<T> {
    #[inline(always)]
    pub(crate) fn head(&self) -> &AtomicUsize {
        unsafe { _field!(Queue, self.ptr, head.head.value, AtomicUsize).as_ref() }
    }

    #[inline(always)]
    pub(crate) fn tail(&self) -> &AtomicUsize {
        unsafe { _field!(Queue, self.ptr, tail.tail.value, AtomicUsize).as_ref() }
    }

    #[inline(always)]
    pub(crate) fn cell_at(&self, index: usize) -> CellPtr<T> {
        self.at(index).into()
    }
}
