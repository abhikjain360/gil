use core::ptr::NonNull;

use crate::{
    atomic::{AtomicU32, AtomicUsize, Ordering},
    padded::Padded,
};

#[derive(Default)]
#[repr(C)]
pub(crate) struct Head {
    head: Padded<AtomicUsize>,
    futex: Padded<AtomicU32>,
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct Tail {
    tail: Padded<AtomicUsize>,
}

pub(crate) struct GetInit;

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
    pub(crate) fn head(&self) -> &AtomicUsize {
        unsafe { _field!(Queue, self.ptr, head.head.value, AtomicUsize).as_ref() }
    }

    #[inline(always)]
    pub(crate) fn tail(&self) -> &AtomicUsize {
        unsafe { _field!(Queue, self.ptr, tail.tail.value, AtomicUsize).as_ref() }
    }

    #[inline(always)]
    pub(super) fn futex(&self) -> &AtomicU32 {
        unsafe { _field!(Queue, self.ptr, head.futex.value, AtomicU32).as_ref() }
    }

    #[inline(always)]
    pub(super) fn futex_store(&self, futex_state: FutexState) -> bool {
        self.futex()
            .compare_exchange(
                FutexState::None as u32,
                futex_state as u32,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    #[inline(always)]
    pub(super) fn futex_wait(&self, futex_state: FutexState) {
        atomic_wait::wait(self.futex(), futex_state as u32)
    }

    #[inline(always)]
    pub(super) fn futex_wake(&self) {
        if self.futex().load(Ordering::SeqCst) != FutexState::None as u32 {
            self.futex()
                .store(FutexState::None as u32, Ordering::Relaxed);
            atomic_wait::wake_one(self.futex())
        }
    }
}

pub(super) enum FutexState {
    ReceiverWaiting = -1,
    None = 0,
    SenderWaiting = 1,
}
