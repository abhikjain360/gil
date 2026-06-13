use core::cell::UnsafeCell;

use crate::{
    Arc, Backoff, Box,
    padded::Padded,
    read_guard::BatchReader,
    ring::Consumer,
    shard_table::{Cursor, Shard, ShardTable},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Shared<T> {
    consumers: Box<[UnsafeCell<Consumer<Shard<T>>>]>,
    locks: Box<[Padded<AtomicBool>]>,
    /// Live receiver count; only bounds `try_clone` at one receiver per shard —
    /// the `Arc` holding this struct owns the memory.
    alive_receivers: AtomicUsize,
}

impl<T> Shared<T> {
    #[inline(always)]
    fn max_shards(&self) -> usize {
        self.consumers.len()
    }

    #[inline(always)]
    fn try_lock(&self, shard_idx: usize) -> bool {
        self.locks[shard_idx]
            .value
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// # Safety
    /// Only call this if `try_lock` returned `true` earlier, and this is the only `unlock` after
    /// that.
    #[inline(always)]
    unsafe fn unlock(&self, shard_idx: usize) {
        self.locks[shard_idx].value.store(false, Ordering::Release);
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
    shared: Arc<Shared<T>>,
    cursor: Cursor,
}

impl<T> Receiver<T> {
    pub(super) fn new(table: &ShardTable<T>) -> Self {
        let consumers = table
            .claim_all_consumers()
            .map(|shard| UnsafeCell::new(Consumer::attach(shard)))
            .collect();
        let locks = (0..table.len())
            .map(|_| Padded::new(AtomicBool::new(false)))
            .collect();

        Self {
            shared: Arc::new(Shared {
                consumers,
                locks,
                alive_receivers: AtomicUsize::new(1),
            }),
            cursor: Cursor::new(table.len()),
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
        let shared = &self.shared;
        let mut live = shared.alive_receivers.load(Ordering::Acquire);
        loop {
            if live >= shared.max_shards() {
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
            shared: Arc::clone(&self.shared),
            cursor: Cursor::new(self.shared.max_shards()),
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
        // Locate a non-empty shard in the scan (keeping its lock), then pop
        // outside it: a pop inside the closure would route the value through an
        // extra `Option<T>` return — a copy of `T` on the hot path for large
        // payloads.
        let shared = &*self.shared;
        let shard_idx = self.cursor.find(|shard_idx| {
            if !shared.try_lock(shard_idx) {
                return None;
            }

            // SAFETY: we hold this shard's lock.
            let consumer = unsafe { &mut *shared.consumers[shard_idx].get() };
            // a peer may have advanced the head since we last held this shard
            consumer.resync();
            if !consumer.has_items() {
                // SAFETY: locked above; single unlock.
                unsafe { shared.unlock(shard_idx) };
                return None;
            }

            // still locked — the pop below is exclusive
            Some(shard_idx)
        })?;

        // SAFETY: the scan left this shard locked for us.
        let consumer = unsafe { &mut *shared.consumers[shard_idx].get() };
        let value = consumer.pop();
        // SAFETY: locked in the scan; single unlock.
        unsafe { shared.unlock(shard_idx) };

        Some(value)
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
        let shared = &*self.shared;
        let found = self.cursor.find(|shard_idx| {
            if !shared.try_lock(shard_idx) {
                return None;
            }

            // SAFETY: we hold this shard's lock.
            let consumer = unsafe { &mut *shared.consumers[shard_idx].get() };
            consumer.resync();
            let (ptr, len) = consumer.read_buffer_raw();
            if len == 0 {
                // SAFETY: locked above; single unlock.
                unsafe { shared.unlock(shard_idx) };
                return None;
            }

            // keep the lock; `release` (via ReadGuard drop) unlocks
            Some((ptr, len))
        });

        match found {
            // SAFETY: raw parts of the locked shard's ring, which `self` keeps
            // alive; rebuilt here only so the slice outlives the scan closure.
            Some((ptr, len)) => unsafe { core::slice::from_raw_parts(ptr.as_ptr(), len) },
            None => &[],
        }
    }

    unsafe fn advance(&mut self, n: usize) {
        // SAFETY (deref): `read_buffer` left this shard locked for us.
        let consumer = unsafe { &mut *self.shared.consumers[self.cursor.index()].get() };
        unsafe { consumer.advance(n) };
    }

    /// Releases the shard spinlock acquired by
    /// [`read_buffer`](BatchReader::read_buffer).
    ///
    /// # Safety
    ///
    /// Must only be called after [`read_buffer`](BatchReader::read_buffer)
    /// returned a **non-empty** slice (i.e., a lock is held).
    unsafe fn release(&mut self) {
        unsafe { self.shared.unlock(self.cursor.index()) };
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        // the Arc owns the memory; this only maintains the clone-bound count
        self.shared.alive_receivers.fetch_sub(1, Ordering::AcqRel);
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
