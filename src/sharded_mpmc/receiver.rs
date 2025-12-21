use core::ptr::{self, NonNull};

use crate::{
    Backoff,
    padded::Padded,
    spsc,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

type Lock = Padded<AtomicBool>;

pub struct ReadGuard<'a, T> {
    receiver: &'a mut Receiver<T>,
    data: *const [T],
    consumed: usize,
}

impl<'a, T> std::ops::Deref for ReadGuard<'a, T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.data }
    }
}

impl<'a, T> Drop for ReadGuard<'a, T> {
    fn drop(&mut self) {
        if self.len() > 0 {
            unsafe {
                self.receiver.advance(self.consumed);
            }
        }
    }
}

impl<'a, T> ReadGuard<'a, T> {
    pub fn advance(&mut self, len: usize) {
        debug_assert!(self.consumed + len <= self.len(), "advancing beyond buffer length");
        self.consumed += len;
    }
}

pub struct Receiver<T> {
    receivers: Box<[spsc::Receiver<T>]>,
    locks: NonNull<Lock>,
    num_receivers: NonNull<AtomicUsize>,
    max_shards: usize,
    next_shard: usize,
}

impl<T> Receiver<T> {
    /// # Safety/Warning
    /// This **does not** clone the shard's QueuePtr, instead reads them.
    pub(crate) unsafe fn new(shards: NonNull<spsc::QueuePtr<T>>, max_shards: usize) -> Self {
        let mut locks = Box::<[Lock]>::new_uninit_slice(max_shards);
        let mut receivers = Box::new_uninit_slice(max_shards);

        for i in 0..max_shards {
            let shard = unsafe { shards.add(i).read() };
            receivers[i].write(spsc::Receiver::new(shard));

            locks[i].write(Padded::new(AtomicBool::new(false)));
        }

        let locks = unsafe { NonNull::new_unchecked(Box::into_raw(locks.assume_init())) }.cast();

        let num_receivers_ptr = Box::into_raw(Box::new(AtomicUsize::new(1)));
        let num_receivers = unsafe { NonNull::new_unchecked(num_receivers_ptr) };

        Self {
            receivers: unsafe { receivers.assume_init() },
            locks,
            num_receivers,
            max_shards,
            next_shard: 0,
        }
    }

    pub fn clone(&self) -> Option<Self> {
        let num_receivers_ref = unsafe { self.num_receivers.as_ref() };
        if num_receivers_ref.fetch_add(1, Ordering::AcqRel) == self.max_shards {
            num_receivers_ref.fetch_sub(1, Ordering::AcqRel);
            return None;
        }

        let mut receivers = Box::new_uninit_slice(self.max_shards);
        for i in 0..self.max_shards {
            receivers[i].write(unsafe { self.receivers[i].clone_via_ptr() });
        }

        Some(Self {
            receivers: unsafe { receivers.assume_init() },
            num_receivers: self.num_receivers,
            locks: self.locks,
            max_shards: self.max_shards,
            next_shard: 0,
        })
    }

    pub fn recv(&mut self) -> T {
        let mut backoff = Backoff::with_spin_count(128);
        loop {
            match self.try_recv() {
                None => backoff.backoff(),
                Some(ret) => return ret,
            }
        }
    }

    pub fn try_recv(&mut self) -> Option<T> {
        let start = self.next_shard;
        loop {
            let idx = self.next_shard;
            self.next_shard += 1;
            if self.next_shard == self.max_shards {
                self.next_shard = 0;
            }

            if self.try_lock(idx) {
                // SAFETY: We hold the lock for this shard.
                self.receivers[idx].refresh_head();
                let ret = self.receivers[idx].try_recv();
                if let Some(v) = ret {
                    unsafe { self.unlock(idx) };
                    return Some(v);
                } else {
                    unsafe { self.unlock(idx) };
                }
            }

            if self.next_shard == start {
                return None;
            }
        }
    }

    pub fn read_buffer(&mut self) -> ReadGuard<'_, T> {
        let start = self.next_shard;
        loop {
            let idx = self.next_shard;
            self.next_shard += 1;
            if self.next_shard == self.max_shards {
                self.next_shard = 0;
            }

            if self.try_lock(idx) {
                let receiver_ptr = &mut self.receivers[idx] as *mut spsc::Receiver<T>;
                // SAFETY: We have &mut self, so accessing the receiver via raw pointer is safe.
                // We use raw pointer to avoid multiple mutable borrows of self.
                unsafe { (*receiver_ptr).refresh_head() };
                let slice = unsafe { (*receiver_ptr).read_buffer() };

                if !slice.is_empty() {
                    return ReadGuard {
                        receiver: self,
                        data: slice as *const [T],
                        consumed: 0,
                    };
                } else {
                    unsafe { self.unlock(idx) };
                }
            }

            if self.next_shard == start {
                return ReadGuard {
                    receiver: self,
                    data: &[] as *const [T],
                    consumed: 0,
                };
            }
        }
    }

    unsafe fn advance(&mut self, len: usize) {
        let prev = if self.next_shard == 0 {
            self.max_shards - 1
        } else {
            self.next_shard - 1
        };

        unsafe {
            self.receivers[prev].advance(len);
            self.unlock(prev);
        }
    }

    #[inline(always)]
    fn shard_lock(&self, shard: usize) -> &AtomicBool {
        unsafe { self.locks.add(shard).cast::<AtomicBool>().as_ref() }
    }

    #[inline(always)]
    fn try_lock(&self, shard: usize) -> bool {
        self.shard_lock(shard)
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// # Safety
    /// Only call this if `try_lock` returned `true` earlier, and this is the only `unlock` after
    /// that.
    #[inline(always)]
    unsafe fn unlock(&self, shard: usize) {
        self.shard_lock(shard).store(false, Ordering::Relaxed);
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let num_receivers_ref = unsafe { self.num_receivers.as_ref() };
        if num_receivers_ref.fetch_sub(1, Ordering::AcqRel) == 1 {
            unsafe {
                let slice_ptr = ptr::slice_from_raw_parts_mut(self.locks.as_ptr(), self.max_shards);

                // creating `Box`s so that heap allocation is also freed
                _ = Box::from_raw(self.num_receivers.as_ptr());
                _ = Box::from_raw(slice_ptr);
            }
        }
    }
}

unsafe impl<T> Send for Receiver<T> {}
