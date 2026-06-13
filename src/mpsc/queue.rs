use crate::{
    atomic::AtomicUsize,
    cell::{Cell, DropTailScan},
    padded::Padded,
    ring::RingTail,
};

#[derive(Default)]
#[repr(C)]
pub(crate) struct Tail {
    tail: Padded<AtomicUsize>,
}

impl RingTail for Tail {
    #[inline(always)]
    fn tail(&self) -> &AtomicUsize {
        &self.tail.value
    }
}

pub(crate) type QueuePtr<T> = crate::QueuePtr<(), Tail, Cell<T>, DropTailScan>;

impl<T> QueuePtr<T> {
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
