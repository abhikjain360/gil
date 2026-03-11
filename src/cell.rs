use core::{mem::MaybeUninit, ptr::NonNull};

use crate::atomic::AtomicUsize;

#[repr(align(64))]
#[cfg_attr(all(target_arch = "aarch64", target_os = "macos"), repr(align(128)))]
pub(crate) struct Cell<T> {
    pub(crate) epoch: AtomicUsize,
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

impl<T> From<NonNull<Cell<T>>> for CellPtr<T> {
    fn from(value: NonNull<Cell<T>>) -> Self {
        Self { ptr: value }
    }
}
