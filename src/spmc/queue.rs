use crate::{
    atomic::AtomicUsize,
    cell::{Cell, DropHeadScan},
    padded::Padded,
    ring::RingHead,
};

#[derive(Default)]
#[repr(C)]
pub(crate) struct Head {
    head: Padded<AtomicUsize>,
}

impl RingHead for Head {
    #[inline(always)]
    fn head(&self) -> &AtomicUsize {
        &self.head.value
    }
}

pub(crate) type QueuePtr<T> = crate::QueuePtr<Head, (), Cell<T>, DropHeadScan>;

impl<T> QueuePtr<T> {
    #[inline(always)]
    pub(crate) fn head(&self) -> &AtomicUsize {
        self.header().head.head()
    }

    #[inline(always)]
    pub(crate) fn cell_at(&self, index: usize) -> &Cell<T> {
        // SAFETY: `at` masks the index into the buffer, and every cell is valid
        // for shared access (epoch initialised at construction, payload behind
        // `UnsafeCell<MaybeUninit<_>>`).
        unsafe { self.at(index).as_ref() }
    }
}
