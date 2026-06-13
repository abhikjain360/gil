use crate::{
    Backoff, Box,
    read_guard::BatchReader,
    ring::Consumer,
    shard_table::{Cursor, Shard, ShardTable},
};

/// The receiving half of a sharded MPSC channel.
///
/// The receiver polls all shards in round-robin fashion, returning the first available item.
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::mpsc::sharded::channel;
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
    consumers: Box<[Consumer<Shard<T>>]>,
    cursor: Cursor,
}

impl<T> Receiver<T> {
    pub(crate) fn new(table: &ShardTable<T>) -> Self {
        Self {
            consumers: table.claim_all_consumers().map(Consumer::attach).collect(),
            cursor: Cursor::new(table.len()),
        }
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
    /// use gil::mpsc::sharded::channel;
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
    /// use gil::mpsc::sharded::channel;
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
    /// Returns `Some(value)` if a value was received from any shard, or `None` if all
    /// shards are empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpsc::sharded::channel;
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
        // Locate a non-empty shard in the scan, then pop outside it: a pop
        // inside the closure would route the value through an extra `Option<T>`
        // return — a copy of `T` on the hot path for large payloads.
        let consumers = &mut self.consumers;
        let shard_idx = self
            .cursor
            .find(|shard_idx| consumers[shard_idx].has_items().then_some(shard_idx))?;

        Some(self.consumers[shard_idx].pop())
    }

    /// Returns a [`ReadGuard`](crate::read_guard::ReadGuard) that provides
    /// batch read access to available items across shards.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpsc::sharded::channel;
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
    /// assert_eq!(guard.as_slice(), &[10, 20]);
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
/// The implementation delegates to the per-shard SPSC receivers.
/// `read_buffer` polls shards round-robin and returns the first non-empty
/// contiguous slice. `advance` publishes the new head on the active shard.
///
/// Items are returned by shared reference — ownership is **not** transferred.
/// See [`BatchReader`](crate::read_guard::BatchReader#ownership) for details.
unsafe impl<T> BatchReader for Receiver<T> {
    type Item = T;

    /// Returns a slice of available items from one of the shards.
    ///
    /// Items are returned by shared reference — ownership is **not**
    /// transferred. See [`BatchReader`](crate::read_guard::BatchReader#ownership).
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpsc::sharded::channel;
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
    /// assert_eq!(buf.len(), 2);
    /// let count = buf.len();
    /// unsafe { rx.advance(count) };
    /// ```
    fn read_buffer(&mut self) -> &[T] {
        let consumers = &mut self.consumers;
        let found = self.cursor.find(|shard_idx| {
            let (ptr, len) = consumers[shard_idx].read_buffer_raw();
            (len > 0).then_some((ptr, len))
        });

        match found {
            // SAFETY: raw parts of the found shard's ring, which `self` keeps
            // alive; rebuilt here only so the slice outlives the scan closure.
            Some((ptr, len)) => unsafe { core::slice::from_raw_parts(ptr.as_ptr(), len) },
            None => &[],
        }
    }

    /// Advances the read pointer of the last shard accessed by
    /// [`read_buffer`](BatchReader::read_buffer).
    ///
    /// # Safety
    ///
    /// `len` must not exceed the length of the slice returned by the last
    /// call to [`read_buffer`](BatchReader::read_buffer).
    ///
    /// # Examples
    ///
    /// ```
    /// use core::num::NonZeroUsize;
    /// use gil::mpsc::sharded::channel;
    /// use gil::read_guard::BatchReader;
    ///
    /// let (mut tx, mut rx) = channel::<usize>(
    ///     NonZeroUsize::new(1).unwrap(),
    ///     NonZeroUsize::new(128).unwrap(),
    /// );
    ///
    /// tx.send(10);
    /// let buf = rx.read_buffer();
    /// assert_eq!(buf, &[10]);
    /// unsafe { rx.advance(1) };
    ///
    /// assert_eq!(rx.try_recv(), None);
    /// ```
    unsafe fn advance(&mut self, len: usize) {
        unsafe { self.consumers[self.cursor.index()].advance(len) };
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
