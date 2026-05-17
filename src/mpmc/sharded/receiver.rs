use core::{cell::UnsafeCell, ptr::NonNull};

use crate::{
    Backoff, Box,
    padded::Padded,
    queue::ShardOwnership,
    read_guard::BatchReader,
    spsc::{self, shards::ShardsPtr},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Shared<T> {
    receivers: Box<[UnsafeCell<spsc::Receiver<T, ShardOwnership>>]>,
    locks: Box<[Padded<AtomicBool>]>,
    alive_receivers: AtomicUsize,
    max_shards: usize,
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
    shared: NonNull<Shared<T>>,
    next_shard: usize,
}

impl<T> Receiver<T> {
    pub(super) fn new(shards: ShardsPtr<T>, max_shards: usize) -> Self {
        let mut locks = Box::<[Padded<AtomicBool>]>::new_uninit_slice(max_shards);
        let mut receivers = Box::new_uninit_slice(max_shards);

        for i in 0..max_shards {
            let shard = shards.claim_consumer_queue_ptr(i).unwrap();
            receivers[i].write(UnsafeCell::new(spsc::Receiver::new(shard)));

            locks[i].write(Padded::new(AtomicBool::new(false)));
        }

        let shared = Box::new(Shared {
            receivers: unsafe { receivers.assume_init() },
            locks: unsafe { locks.assume_init() },
            alive_receivers: AtomicUsize::new(1),
            max_shards,
        });

        Self {
            shared: unsafe { NonNull::new_unchecked(Box::into_raw(shared)) },
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
        let shared = self.shared();
        let mut live = shared.alive_receivers.load(Ordering::Acquire);
        loop {
            if live >= shared.max_shards {
                return None;
            }

            // need cas instead of fetch_add to accidentally avoid creating more than N clones
            // TODO: do we really need to limit receivers to N? makes sense for senders but
            //       receivers to round-robin so more than N can work just fine
            match shared.alive_receivers.compare_exchange(
                live,
                live + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => live = actual,
            }
        }

        Some(Self {
            shared: self.shared,
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

            if self.try_lock(idx) {
                let receiver = unsafe { self.receiver_mut(idx) };
                receiver.refresh_head();
                let ret = receiver.try_recv();
                unsafe { self.unlock(idx) };

                if let Some(v) = ret {
                    return Some(v);
                }
            }

            self.next_shard += 1;
            if self.next_shard == self.shared().max_shards {
                self.next_shard = 0;
            }

            if self.next_shard == start {
                return None;
            }
        }
    }

    /// Returns a [`ReadGuard`](crate::read_guard::ReadGuard) providing read
    /// access to a batch of elements from the channel.
    ///
    /// The guard holds a shard lock internally. The lock is released when the
    /// guard is dropped. If no elements are available, an empty guard is
    /// returned (no lock held).
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
    /// let mut guard = rx.read_guard();
    /// assert_eq!(guard.as_slice()[0], 10);
    /// assert_eq!(guard.as_slice()[1], 20);
    /// guard.advance(guard.len());
    /// drop(guard);
    ///
    /// assert_eq!(rx.try_recv(), None);
    /// ```
    pub fn read_guard(&mut self) -> crate::read_guard::ReadGuard<'_, Self> {
        crate::read_guard::ReadGuard::new(self)
    }

    #[inline(always)]
    fn shared(&self) -> &Shared<T> {
        unsafe { self.shared.as_ref() }
    }

    /// # Safety
    ///
    /// The caller must hold this shard's lock.
    #[inline(always)]
    unsafe fn receiver_mut(&mut self, shard: usize) -> &mut spsc::Receiver<T, ShardOwnership> {
        unsafe { &mut *self.shared().receivers[shard].get() }
    }

    #[inline(always)]
    fn shard_lock(&self, shard: usize) -> &AtomicBool {
        &self.shared().locks[shard].value
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

/// # Safety
///
/// The implementation locks a shard spinlock in [`read_buffer`](BatchReader::read_buffer)
/// and releases it in [`release`](BatchReader::release). Between these two
/// calls, no other receiver can access the locked shard.
///
/// Items are returned by shared reference — ownership is **not** transferred.
/// See [`BatchReader`](crate::read_guard::BatchReader#ownership) for details.
unsafe impl<T> BatchReader for Receiver<T> {
    type Item = T;

    /// Returns a slice of available items from a locked shard.
    ///
    /// Acquires a shard spinlock internally. The lock is released by
    /// [`release`](BatchReader::release) (called automatically by
    /// [`ReadGuard`](crate::read_guard::ReadGuard) on drop).
    ///
    /// If no items are available, returns an empty slice (no lock held).
    ///
    /// Items are returned by shared reference — ownership is **not**
    /// transferred. See [`BatchReader`](crate::read_guard::BatchReader#ownership).
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpmc::sharded::channel;
    /// use gil::read_guard::BatchReader;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(128).unwrap(),
    /// );
    ///
    /// tx.send(10);
    /// tx.send(20);
    ///
    /// let buf = rx.read_buffer();
    /// assert_eq!(buf[0], 10);
    /// assert_eq!(buf[1], 20);
    /// let count = buf.len();
    /// unsafe {
    ///     rx.advance(count);
    ///     rx.release();
    /// };
    ///
    /// assert_eq!(rx.try_recv(), None);
    /// ```
    fn read_buffer(&mut self) -> &[T] {
        let start = self.next_shard;
        loop {
            let idx = self.next_shard;

            if self.try_lock(idx) {
                let receiver = unsafe { self.receiver_mut(idx) };
                receiver.refresh_head();
                let ret = receiver.read_buffer();

                if !ret.is_empty() {
                    return unsafe { core::mem::transmute::<&[T], &[T]>(ret) };
                } else {
                    unsafe { self.unlock(idx) };
                }
            }

            self.next_shard += 1;
            if self.next_shard == self.shared().max_shards {
                self.next_shard = 0;
            }

            if self.next_shard == start {
                return &[];
            }
        }
    }

    unsafe fn advance(&mut self, n: usize) {
        let receiver = unsafe { self.receiver_mut(self.next_shard) };
        unsafe { receiver.advance(n) };
    }

    /// Releases the shard spinlock acquired by
    /// [`read_buffer`](BatchReader::read_buffer).
    ///
    /// # Safety
    ///
    /// Must only be called after [`read_buffer`](BatchReader::read_buffer)
    /// returned a **non-empty** slice (i.e., a lock is held).
    unsafe fn release(&mut self) {
        unsafe { self.unlock(self.next_shard) };
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        unsafe {
            if self.shared().alive_receivers.fetch_sub(1, Ordering::AcqRel) == 1 {
                _ = Box::from_raw(self.shared.as_ptr());
            }
        }
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
