#[cfg(feature = "async")]
use std::task::Waker;
use std::{
    marker::PhantomData,
    mem::{align_of, offset_of, size_of},
    num::NonZeroUsize,
    ptr::NonNull,
};

#[cfg(feature = "async")]
use futures::task::AtomicWaker;

#[cfg(feature = "async")]
use crate::atomic::AtomicBool;
use crate::{
    alloc,
    atomic::{AtomicUsize, Ordering},
    padded::Padded,
};

/// # Invariants
/// - tail should always point to the place where we can write next to.
// avoid re-ordering fields
#[repr(C)]
struct Queue {
    head: Padded<AtomicUsize>,
    #[cfg(feature = "async")]
    sender_sleeping: Padded<AtomicBool>,
    #[cfg(feature = "async")]
    receiver_waker: Padded<AtomicWaker>,

    tail: Padded<AtomicUsize>,
    #[cfg(feature = "async")]
    receiver_sleeping: Padded<AtomicBool>,
    #[cfg(feature = "async")]
    sender_waker: Padded<AtomicWaker>,

    rc: AtomicUsize,
}

pub(crate) struct QueuePtr<T> {
    ptr: NonNull<Queue>,
    buffer: NonNull<T>,
    pub(crate) capacity: usize,
    _marker: PhantomData<T>,
}

impl<T> Clone for QueuePtr<T> {
    fn clone(&self) -> Self {
        Self {
            ptr: self.ptr,
            buffer: self.buffer,
            capacity: self.capacity,
            _marker: PhantomData,
        }
    }
}

macro_rules! _field {
    ($ptr:expr, $($path:tt).+) => {
        $ptr.byte_add(offset_of!(Queue, $($path).+))
    };

    ($ptr:expr, $($path:tt).+, $ty:ty) => {
        $ptr.byte_add(offset_of!(Queue, $($path).+)).cast::<$ty>()
    };
}

impl<T> QueuePtr<T> {
    pub(crate) fn with_capacity(capacity: NonZeroUsize) -> (Self, usize) {
        let capacity = capacity.saturating_add(1).get().next_power_of_two();

        let (layout, buffer_offset) = Self::layout(capacity);

        // SAFETY: capacity > 0, so layout is non-zero too
        let ptr = unsafe { alloc::alloc(layout) } as *mut Queue;
        let Some(ptr) = NonNull::new(ptr) else {
            std::alloc::handle_alloc_error(layout);
        };

        let mask = capacity - 1;

        // Calculate buffer pointer
        let buffer =
            unsafe { NonNull::new_unchecked(ptr.as_ptr().byte_add(buffer_offset).cast::<T>()) };

        unsafe {
            ptr.write(Queue {
                head: Padded::new(AtomicUsize::new(0)),
                tail: Padded::new(AtomicUsize::new(0)),

                #[cfg(feature = "async")]
                sender_sleeping: Padded::new(AtomicBool::new(false)),

                #[cfg(feature = "async")]
                receiver_sleeping: Padded::new(AtomicBool::new(false)),

                #[cfg(feature = "async")]
                sender_waker: Padded::new(AtomicWaker::new()),

                #[cfg(feature = "async")]
                receiver_waker: Padded::new(AtomicWaker::new()),

                rc: AtomicUsize::new(2),
            });
        };

        (
            Self {
                ptr,
                buffer,
                _marker: PhantomData,
                capacity,
            },
            mask,
        )
    }

    fn layout(capacity: usize) -> (alloc::Layout, usize) {
        let header_layout =
            alloc::Layout::from_size_align(size_of::<Queue>(), align_of::<Queue>()).unwrap();
        let buffer_layout = alloc::Layout::array::<T>(capacity).unwrap();
        header_layout.extend(buffer_layout).unwrap()
    }

    #[inline(always)]
    pub(crate) fn head(&self) -> &AtomicUsize {
        unsafe { _field!(self.ptr, head.value, AtomicUsize).as_ref() }
    }

    #[inline(always)]
    pub(crate) fn tail(&self) -> &AtomicUsize {
        unsafe { _field!(self.ptr, tail.value, AtomicUsize).as_ref() }
    }

    #[inline(always)]
    pub(crate) unsafe fn at(&self, index: usize) -> NonNull<T> {
        unsafe { NonNull::new_unchecked(self.buffer.as_ptr().add(index)) }
    }

    #[inline(always)]
    pub(crate) unsafe fn get(&self, index: usize) -> T {
        unsafe { self.at(index).read() }
    }

    #[inline(always)]
    pub(crate) fn set(&self, index: usize, value: T) {
        unsafe { self.at(index).write(value) }
    }
}

#[cfg(feature = "async")]
impl<T> QueuePtr<T> {
    #[inline(always)]
    pub(crate) fn register_sender_waker(&self, waker: &Waker) {
        unsafe {
            _field!(self.ptr, sender_waker.value, AtomicWaker)
                .as_ref()
                .register(waker);
        }
    }

    #[inline(always)]
    pub(crate) fn register_receiver_waker(&self, waker: &Waker) {
        unsafe {
            _field!(self.ptr, receiver_waker.value, AtomicWaker)
                .as_ref()
                .register(waker);
        }
    }

    #[inline(always)]
    pub(crate) fn wake_sender(&self) {
        unsafe {
            _field!(self.ptr, sender_waker.value, AtomicWaker)
                .as_ref()
                .wake();
        }
    }

    #[inline(always)]
    pub(crate) fn wake_receiver(&self) {
        unsafe {
            _field!(self.ptr, receiver_waker.value, AtomicWaker)
                .as_ref()
                .wake();
        }
    }

    #[inline(always)]
    pub(crate) fn sender_sleeping(&self) -> &AtomicBool {
        unsafe { _field!(self.ptr, sender_sleeping.value, AtomicBool).as_ref() }
    }

    #[inline(always)]
    pub(crate) fn receiver_sleeping(&self) -> &AtomicBool {
        unsafe { _field!(self.ptr, receiver_sleeping.value, AtomicBool).as_ref() }
    }
}

impl<T> Drop for QueuePtr<T> {
    fn drop(&mut self) {
        let rc = unsafe { _field!(self.ptr, rc, AtomicUsize).as_ref() };
        if rc.fetch_sub(1, Ordering::AcqRel) == 1 {
            let (layout, _) = Self::layout(self.capacity);

            let head = self.head().load(Ordering::Relaxed);
            let tail = self.tail().load(Ordering::Relaxed);
            let mask = self.capacity - 1;

            if std::mem::needs_drop::<T>() {
                let mut idx = head;
                while idx != tail {
                    unsafe {
                        std::ptr::drop_in_place(self.at(idx).as_ptr());
                    }
                    idx = (idx + 1) & mask;
                }
            }

            unsafe {
                self.ptr.drop_in_place();
                alloc::dealloc(self.ptr.cast().as_ptr(), layout);
            }
        }
    }
}
