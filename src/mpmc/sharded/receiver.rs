use core::ptr::{self, NonNull};

use crate::{
    Backoff, Box,
    padded::Padded,
    read_guard::BatchReader,
    spsc::{self, shards::ShardsPtr},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

type Lock = Padded<AtomicBool>;

/// A guard that provides read access to a batch of elements from the channel.
///
/// When the guard is dropped, the elements are marked as consumed in the channel.
/// Use [`advance`](ReadGuard::advance) to mark specific elements as consumed before dropping.
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::mpmc::sharded::channel;
///
/// let (mut tx, mut rx) = channel::<usize>(
///     NonZeroUsize::new(1).unwrap(),
///     NonZeroUsize::new(128).unwrap(),
/// );
///
/// tx.send(10);
/// tx.send(20);
///
/// let mut guard = rx.read_buffer();
/// assert_eq!(guard.len(), 2);
/// assert_eq!(guard[0], 10);
/// guard.advance(guard.len());
/// // Elements are consumed when the guard is dropped
/// ```
pub struct ReadGuard<'a, T> {
    receiver: &'a mut Receiver<T>,
    data: NonNull<[T]>,
    consumed: usize,
}

impl<'a, T> core::ops::Deref for ReadGuard<'a, T> {
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
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(128).unwrap(),
    /// );
    ///
    /// tx.send(1);
    /// tx.send(2);
    /// tx.send(3);
    ///
    /// let mut guard = rx.read_buffer();
    /// // Only consume the first 2 elements
    /// guard.advance(2);
    /// drop(guard);
    ///
    /// // Third element is still available
    /// assert_eq!(rx.recv(), 3);
    /// ```
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
/// The receiver polls shards in round-robin fashion, returning the first available item.
/// Multiple receivers can coexist via [`try_clone`](Receiver::try_clone), each competing for
/// items across shards.
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::mpmc::sharded::channel;
///
/// let (mut tx, mut rx) = channel::<i32>(
///     NonZeroUsize::new(1).unwrap(),
///     NonZeroUsize::new(16).unwrap(),
/// );
/// tx.send(1);
/// tx.send(2);
/// assert_eq!(rx.recv(), 1);
/// assert_eq!(rx.recv(), 2);
/// ```
pub struct Receiver<T> {
    receivers: Box<[spsc::Receiver<T>]>,
    locks: NonNull<Lock>,
    alive_receivers: NonNull<AtomicUsize>,
    shards: ShardsPtr<T>,
    max_shards: usize,
    next_shard: usize,
}

impl<T> Receiver<T> {
    pub(super) fn new(shards: ShardsPtr<T>, max_shards: usize) -> Self {
        let mut locks = Box::<[Lock]>::new_uninit_slice(max_shards);
        let mut receivers = Box::new_uninit_slice(max_shards);

        for i in 0..max_shards {
            let shard = shards.clone_queue_ptr(i);
            receivers[i].write(spsc::Receiver::new(shard));

            locks[i].write(Padded::new(AtomicBool::new(false)));
        }

        let locks = unsafe { NonNull::new_unchecked(Box::into_raw(locks.assume_init())) }.cast();

        let alive_receivers_ptr = Box::into_raw(Box::new(AtomicUsize::new(1)));
        let alive_receivers = unsafe { NonNull::new_unchecked(alive_receivers_ptr) };

        Self {
            receivers: unsafe { receivers.assume_init() },
            locks,
            alive_receivers,
            shards,
            max_shards,
            next_shard: 0,
        }
    }

    /// Attempts to clone the receiver.
    ///
    /// Returns `Some(Receiver)` if there is an available slot for a new receiver,
    /// or `None` if the maximum number of receivers has been reached.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
    ///
    /// let (tx, rx) = channel::<i32>(
    ///     NonZeroUsize::new(2).unwrap(),
    ///     NonZeroUsize::new(16).unwrap(),
    /// );
    ///
    /// let rx2 = rx.try_clone().expect("slot available");
    /// ```
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
            shards: self.shards.clone(),
            locks: self.locks,
            max_shards: self.max_shards,
            next_shard: 0,
        })
    }

    /// Receives a value from the channel.
    ///
    /// This method will block (spin) with a default spin count of 128 until a
    /// value is available in any of the shards. For control over the spin count,
    /// use [`Receiver::recv_with_spin_count`].
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(16).unwrap(),
    /// );
    /// tx.send(42);
    /// assert_eq!(rx.recv(), 42);
    /// ```
    pub fn recv(&mut self) -> T {
        self.recv_with_spin_count(128)
    }

    /// Receives a value from the channel, using a custom spin count.
    ///
    /// The `spin_count` controls how many times the backoff spins before yielding
    /// the thread. A higher value keeps the thread spinning longer, which can reduce
    /// latency when the queue is expected to fill quickly, at the cost of higher CPU
    /// usage. A lower value yields sooner, reducing CPU usage but potentially
    /// increasing latency.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(16).unwrap(),
    /// );
    /// tx.send(42);
    /// assert_eq!(rx.recv_with_spin_count(32), 42);
    /// ```
    pub fn recv_with_spin_count(&mut self, spin_count: u32) -> T {
        let mut backoff = Backoff::with_spin_count(spin_count);
        loop {
            match self.try_recv() {
                None => backoff.backoff(),
                Some(ret) => return ret,
            }
        }
    }

    /// Attempts to receive a value from the channel without blocking.
    ///
    /// Returns `Some(value)` if a value was received, or `None` if all shards are empty
    /// or locked by other receivers.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<i32>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(16).unwrap(),
    /// );
    ///
    /// assert_eq!(rx.try_recv(), None);
    ///
    /// tx.send(42);
    /// assert_eq!(rx.try_recv(), Some(42));
    /// ```
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
    /// If no elements are available, an empty [`ReadGuard`] is returned. Use
    /// [`ReadGuard::advance`] to mark elements as consumed before dropping the guard.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(128).unwrap(),
    /// );
    ///
    /// tx.send(10);
    /// tx.send(20);
    ///
    /// let mut guard = rx.read_buffer();
    /// assert_eq!(guard[0], 10);
    /// assert_eq!(guard[1], 20);
    /// guard.advance(guard.len());
    /// drop(guard);
    ///
    /// assert_eq!(rx.try_recv(), None);
    /// ```
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
    ///   slice
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
            if self.alive_receivers.as_ref().fetch_sub(1, Ordering::AcqRel) == 1 {
                let slice_ptr = ptr::slice_from_raw_parts_mut(self.locks.as_ptr(), self.max_shards);
                _ = Box::from_raw(slice_ptr);
                _ = Box::from_raw(self.alive_receivers.as_ptr());
            }
        }
    }
}

unsafe impl<T> Send for Receiver<T> {}
