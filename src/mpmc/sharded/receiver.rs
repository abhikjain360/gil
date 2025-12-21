use core::ptr::{self, NonNull};

use crate::{
    Backoff,
    padded::Padded,
    spsc,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

type Lock = Padded<AtomicBool>;

/// A guard that provides read access to a batch of elements from the channel.
///
/// When the guard is dropped, the elements are marked as consumed in the channel.
pub struct ReadGuard<'a, T> {
    receiver: &'a mut Receiver<T>,
    data: NonNull<[T]>,
    consumed: usize,
}

impl<'a, T> std::ops::Deref for ReadGuard<'a, T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        unsafe { self.data.as_ref() }
    }
}

impl<'a, T> Drop for ReadGuard<'a, T> {
    fn drop(&mut self) {
        let slice = unsafe { self.data.as_ref() };
        if !slice.is_empty() {
            unsafe {
                self.receiver.advance(self.consumed);
            }
        }
    }
}

impl<'a, T> ReadGuard<'a, T> {
    /// Marks `len` elements as consumed.
    ///
    /// These elements will be removed from the channel when the guard is dropped.
    pub fn advance(&mut self, len: usize) {
        debug_assert!(
            self.consumed + len <= self.len(),
            "advancing beyond buffer length"
        );
        self.consumed += len;
    }
}

/// The receiving half of a sharded MPMC channel.
///
/// The receiver attempts to read from shards in a round-robin fashion.
pub struct Receiver<T> {
    receivers: Box<[spsc::Receiver<T>]>,
    locks: NonNull<Lock>,
    alive_receivers: NonNull<AtomicUsize>,
    alive_senders: NonNull<AtomicUsize>,
    shards: NonNull<spsc::QueuePtr<T>>,
    max_shards: usize,
    next_shard: usize,
}

impl<T> Receiver<T> {
    pub(crate) fn new(
        shards: NonNull<spsc::QueuePtr<T>>,
        max_shards: usize,
        alive_senders: NonNull<AtomicUsize>,
        alive_receivers: NonNull<AtomicUsize>,
    ) -> Self {
        let mut locks = Box::<[Lock]>::new_uninit_slice(max_shards);
        let mut receivers = Box::new_uninit_slice(max_shards);

        for i in 0..max_shards {
            let shard = unsafe { shards.add(i).as_ref() }.clone();
            receivers[i].write(spsc::Receiver::new(shard));

            locks[i].write(Padded::new(AtomicBool::new(false)));
        }

        let locks = unsafe { NonNull::new_unchecked(Box::into_raw(locks.assume_init())) }.cast();

        unsafe {
            alive_receivers.as_ref().fetch_add(1, Ordering::AcqRel);
        }

        Self {
            receivers: unsafe { receivers.assume_init() },
            locks,
            alive_receivers,
            alive_senders,
            shards,
            max_shards,
            next_shard: 0,
        }
    }

    /// Attempts to clone the receiver.
    ///
    /// Returns `Some(Receiver)` if there is an available slot for a new receiver, otherwise returns `None`.
    pub fn try_clone(&self) -> Option<Self> {
        let num_receivers_ref = unsafe { self.alive_receivers.as_ref() };
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
            alive_receivers: self.alive_receivers,
            alive_senders: self.alive_senders,
            shards: self.shards,
            locks: self.locks,
            max_shards: self.max_shards,
            next_shard: 0,
        })
    }

    /// Receives a value from the channel.
    ///
    /// This method will block (spin) until a value is available in any of the shards.
    pub fn recv(&mut self) -> T {
        let mut backoff = Backoff::with_spin_count(128);
        loop {
            match self.try_recv() {
                None => backoff.backoff(),
                Some(ret) => return ret,
            }
        }
    }

    /// Attempts to receive a value from the channel without blocking.
    ///
    /// Returns `Some(value)` if a value was received, or `None` if all shards are empty or locked.
    pub fn try_recv(&mut self) -> Option<T> {
        let start = self.next_shard;
        loop {
            let idx = self.next_shard;

            if !self.receivers[idx].is_empty() && self.try_lock(idx) {
                self.receivers[idx].refresh_head();
                let ret = self.receivers[idx].try_recv();
                unsafe { self.unlock(idx) };

                if let Some(v) = ret {
                    return Some(v);
                }
            }

            self.next_shard += 1;
            if self.next_shard == self.max_shards {
                self.next_shard = 0;
            }

            if self.next_shard == start {
                return None;
            }
        }
    }

    /// Returns a [`ReadGuard`] providing read access to a batch of elements from the channel.
    ///
    /// If no elements are available, an empty [`ReadGuard`] is returned.
    pub fn read_buffer(&mut self) -> ReadGuard<'_, T> {
        let start = self.next_shard;
        loop {
            let idx = self.next_shard;

            if !self.receivers[idx].is_empty() && self.try_lock(idx) {
                let receiver_ptr = &mut self.receivers[idx] as *mut spsc::Receiver<T>;
                unsafe { (*receiver_ptr).refresh_head() };
                let slice = unsafe { (*receiver_ptr).read_buffer() };

                if !slice.is_empty() {
                    return ReadGuard {
                        receiver: self,
                        data: NonNull::from_ref(slice),
                        consumed: 0,
                    };
                } else {
                    unsafe { self.unlock(idx) };
                }
            }

            self.next_shard += 1;
            if self.next_shard == self.max_shards {
                self.next_shard = 0;
            }

            if self.next_shard == start {
                return ReadGuard {
                    receiver: self,
                    data: NonNull::from_ref(&[]),
                    consumed: 0,
                };
            }
        }
    }

    /// # Safety
    ///
    /// - `self.read_buffer` must've been called before this, and it should've returned a non-empty
    ///    slice
    /// - this should be called **only once** after `self.read_buffer` returned a non-empty slice.
    unsafe fn advance(&mut self, len: usize) {
        // SAFETY: caller guarantees that read_buffer was called before, and it returned a buffer
        //         which was NOT empty
        unsafe {
            self.receivers[self.next_shard].advance(len);
            self.unlock(self.next_shard);
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
        self.shard_lock(shard).store(false, Ordering::Release);
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        unsafe {
            let num_receivers_ref = self.alive_receivers.as_ref();
            if num_receivers_ref.fetch_sub(1, Ordering::AcqRel) == 1 {
                let slice_ptr = ptr::slice_from_raw_parts_mut(self.locks.as_ptr(), self.max_shards);
                _ = Box::from_raw(slice_ptr);

                if self.alive_senders.as_ref().load(Ordering::Acquire) == 0 {
                    _ = Box::from_raw(self.alive_senders.as_ptr());
                    _ = Box::from_raw(self.alive_receivers.as_ptr());
                    _ = Box::from_raw(ptr::slice_from_raw_parts_mut(
                        self.shards.as_ptr(),
                        self.max_shards,
                    ));
                }
            }
        }
    }
}

unsafe impl<T> Send for Receiver<T> {}
