use core::{
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

use crate::atomic::AtomicUsize;

#[repr(align(64))]
#[cfg_attr(all(target_arch = "aarch64", target_os = "macos"), repr(align(128)))]
pub(crate) struct Cell<T> {
    pub(crate) epoch: AtomicUsize,
    #[cfg(feature = "std")]
    pub(crate) futex: AtomicU32,
    pub(crate) data: MaybeUninit<T>,
}

pub(crate) struct CellPtr<T> {
    ptr: NonNull<Cell<T>>,
}

impl<T> CellPtr<T> {
    /// # Safety
    /// The value must be initialised correctly at this `index`
    #[inline(always)]
    pub(crate) unsafe fn get(&self) -> T {
        unsafe { _field!(Cell<T>, self.ptr, data, T).read() }
    }

    #[inline(always)]
    pub(crate) fn set(&self, value: T) {
        unsafe { _field!(Cell<T>, self.ptr, data, T).write(value) }
    }

    #[inline(always)]
    pub(crate) fn epoch(&self) -> &AtomicUsize {
        unsafe { _field!(Cell<T>, self.ptr, epoch, AtomicUsize).as_ref() }
    }

    #[inline(always)]
    pub(crate) unsafe fn drop_in_place(&self) {
        if core::mem::needs_drop::<T>() {
            unsafe {
                core::ptr::drop_in_place(_field!(Cell<T>, self.ptr, data, T).as_ptr());
            }
        }
    }
}

#[cfg(feature = "std")]
impl<T> CellPtr<T> {
    #[inline(always)]
    pub(crate) fn futex(&self) -> &AtomicU32 {
        unsafe { _field!(Cell<T>, self.ptr, futex, AtomicU32).as_ref() }
    }

    #[inline(always)]
    pub(crate) fn prepare_wait(&self) -> bool {
        self.futex()
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
    }

    #[inline(always)]
    pub(crate) fn wait(&self) {
        atomic_wait::wait(self.futex(), 1);
    }

    #[inline(always)]
    pub(crate) fn wake(&self) {
        let futex = self.futex();
        if futex.load(Ordering::SeqCst) == 1 {
            futex.store(0, Ordering::Relaxed);
            atomic_wait::wake_all(futex);
        }
    }
}

impl<T> From<NonNull<Cell<T>>> for CellPtr<T> {
    fn from(value: NonNull<Cell<T>>) -> Self {
        Self { ptr: value }
    }
}
