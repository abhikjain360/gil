use core::ptr::NonNull;

use crate::{Backoff, spsc};

/// The receiving half of a sharded MPSC channel.
///
/// The receiver attempts to read from shards in a round-robin fashion.
pub struct Receiver<T> {
    receivers: Box<[spsc::Receiver<T>]>,
    max_shards: usize,
    next_shard: usize,
}

impl<T> Receiver<T> {
    /// # Safety/Warning
    /// This **does not** clone the shard's QueuePtr, instead reads them.
    pub(crate) unsafe fn new(shards: NonNull<spsc::QueuePtr<T>>, max_shards: usize) -> Self {
        let mut receivers = Box::new_uninit_slice(max_shards);

        for i in 0..max_shards {
            let shard = unsafe { shards.add(i).read() };
            receivers[i].write(spsc::Receiver::new(shard));
        }

        Self {
            receivers: unsafe { receivers.assume_init() },
            max_shards,
            next_shard: 0,
        }
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
    /// Returns `Some(value)` if a value was received, or `None` if all shards are empty.
    pub fn try_recv(&mut self) -> Option<T> {
        let start = self.next_shard;
        loop {
            let ret = self.receivers[self.next_shard].try_recv();

            if ret.is_some() {
                return ret;
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

    /// Returns a slice of the internal read buffer from one of the shards.
    ///
    /// If no elements are available in any shard, an empty slice is returned.
    pub fn read_buffer(&mut self) -> &[T] {
        let start = self.next_shard;
        loop {
            let ret = self.receivers[self.next_shard].read_buffer();

            if !ret.is_empty() {
                return unsafe { std::mem::transmute::<&[T], &[T]>(ret) };
            }

            self.next_shard += 1;
            if self.next_shard == self.max_shards {
                self.next_shard = 0;
            }

            if self.next_shard == start {
                return &[];
            }
        }
    }

    /// Advances the read pointer of the last shard accessed by `read_buffer`.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `len` is less than or equal to the length of the slice
    /// returned by the last call to `read_buffer`.
    pub unsafe fn advance(&mut self, len: usize) {
        unsafe { self.receivers[self.next_shard].advance(len) };
    }
}

unsafe impl<T> Send for Receiver<T> {}
