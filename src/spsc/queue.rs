use core::ptr::NonNull;
#[cfg(feature = "async")]
use core::task::Waker;

#[cfg(feature = "async")]
use futures::task::AtomicWaker;

use crate::{
    atomic::{AtomicUsize, Ordering},
    padded::Padded,
};

#[derive(Default)]
#[repr(C)]
pub struct Head {
    head: Padded<AtomicUsize>,
    #[cfg(feature = "async")]
    receiver_waker: Padded<AtomicWaker>,
}

#[derive(Default)]
#[repr(C)]
pub struct Tail {
    tail: Padded<AtomicUsize>,
    #[cfg(feature = "async")]
    sender_waker: Padded<AtomicWaker>,
}

pub struct GetInit;

impl<T> crate::DropInitItems<Head, Tail, T> for GetInit {
    unsafe fn drop_init_items(
        head: NonNull<Head>,
        tail: NonNull<Tail>,
        _capaity: usize,
        at: impl Fn(usize) -> NonNull<T>,
    ) {
        if !core::mem::needs_drop::<T>() {
            return;
        }

        let (head, tail) = unsafe {
            let head = _field!(Head, head, head.value, AtomicUsize)
                .as_ref()
                .load(Ordering::Relaxed);
            let tail = _field!(Tail, tail, tail.value, AtomicUsize)
                .as_ref()
                .load(Ordering::Relaxed);
            (head, tail)
        };
        let len = tail.wrapping_sub(head);

        for i in 0..len {
            let idx = head.wrapping_add(i);
            unsafe { at(idx).drop_in_place() };
        }
    }
}

pub(crate) type QueuePtr<T> = crate::QueuePtr<Head, Tail, T, GetInit>;
type Queue = crate::Queue<Head, Tail>;

impl<T> QueuePtr<T> {
    #[inline(always)]
    pub fn head(&self) -> &AtomicUsize {
        unsafe { _field!(Queue, self.ptr, head.head.value, AtomicUsize).as_ref() }
    }

    #[inline(always)]
    pub fn tail(&self) -> &AtomicUsize {
        unsafe { _field!(Queue, self.ptr, tail.tail.value, AtomicUsize).as_ref() }
    }
}

#[cfg(feature = "async")]
impl<T> QueuePtr<T> {
    #[inline(always)]
    pub fn register_sender_waker(&self, waker: &Waker) {
        unsafe {
            _field!(Queue, self.ptr, tail.sender_waker.value, AtomicWaker)
                .as_ref()
                .register(waker);
        }
    }

    #[inline(always)]
    pub fn register_receiver_waker(&self, waker: &Waker) {
        unsafe {
            _field!(Queue, self.ptr, head.receiver_waker.value, AtomicWaker)
                .as_ref()
                .register(waker);
        }
    }

    #[inline(always)]
    pub(crate) fn wake_sender(&self) {
        unsafe {
            _field!(Queue, self.ptr, tail.sender_waker.value, AtomicWaker)
                .as_ref()
                .wake();
        }
    }

    #[inline(always)]
    pub(crate) fn wake_receiver(&self) {
        unsafe {
            _field!(Queue, self.ptr, head.receiver_waker.value, AtomicWaker)
                .as_ref()
                .wake();
        }
    }
}
