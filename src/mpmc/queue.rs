use core::marker::PhantomData;

#[cfg(feature = "std")]
use crate::atomic::AtomicU32;
use crate::{
    atomic::{AtomicUsize, Ordering},
    cell::{Cell, CellPtr},
    padded::Padded,
};

#[derive(Default)]
#[repr(C)]
pub(crate) struct Head {
    head: Padded<AtomicUsize>,
    #[cfg(feature = "std")]
    futex: Padded<AtomicU32>,
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

#[cfg(feature = "std")]
#[derive(Clone, Copy)]
pub(super) enum FutexState {
    ReceiversWaiting = -1,
    Free = 0,
    SendersWaiting = 1,
}

#[cfg(feature = "std")]
impl<T> QueuePtr<T> {
    #[inline(always)]
    pub(super) fn futex(&self) -> &AtomicU32 {
        unsafe { _field!(Queue, self.ptr, head.futex.value, AtomicU32).as_ref() }
    }

    #[inline(always)]
    pub(super) fn prepare_wait(&self, value: FutexState) -> bool {
        // TODO: figure out if we can get-by with cheaper ordering, but is also probably fine as we
        //       were going to wait anyway so what is some extra syncing cost?
        match self.futex().compare_exchange(
            FutexState::Free as u32,
            value as u32,
            Ordering::SeqCst,
            Ordering::Relaxed,
        ) {
            Err(current) => current == value as u32,
            Ok(_) => true,
        }
    }

    #[inline(always)]
    pub(super) fn wait(&self, value: FutexState) {
        atomic_wait::wait(self.futex(), value as u32)
    }

    #[inline(always)]
    pub(super) fn wake(&self) {
        let futex = self.futex();
        if futex.load(Ordering::Relaxed) != FutexState::Free as u32 {
            futex.store(FutexState::Free as u32, Ordering::SeqCst);
            atomic_wait::wake_all(futex);
        }
    }
}
