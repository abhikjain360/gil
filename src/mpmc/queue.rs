#[cfg(feature = "std")]
use crate::{atomic::AtomicU32, futex::HasFutex};
use crate::{
    atomic::AtomicUsize,
    cell::{Cell, DropTailScan},
    padded::Padded,
    ring::{RingHead, RingTail},
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

impl RingHead for Head {
    #[inline(always)]
    fn head(&self) -> &AtomicUsize {
        &self.head.value
    }
}

impl RingTail for Tail {
    #[inline(always)]
    fn tail(&self) -> &AtomicUsize {
        &self.tail.value
    }
}

#[cfg(feature = "std")]
impl HasFutex for Head {
    #[inline(always)]
    fn futex(&self) -> &AtomicU32 {
        &self.futex.value
    }
}

pub(crate) type QueuePtr<T> = crate::QueuePtr<Head, Tail, Cell<T>, DropTailScan>;

impl<T> QueuePtr<T> {
    #[inline(always)]
    pub(crate) fn head(&self) -> &AtomicUsize {
        self.header().head.head()
    }

    #[inline(always)]
    pub(crate) fn tail(&self) -> &AtomicUsize {
        self.header().tail.tail()
    }

    #[inline(always)]
    pub(crate) fn cell_at(&self, index: usize) -> &Cell<T> {
        // SAFETY: `at` masks the index into the buffer, and every cell is valid
        // for shared access (epoch initialised at construction, payload behind
        // `UnsafeCell<MaybeUninit<_>>`).
        unsafe { self.at(index).as_ref() }
    }
}
