use core::ptr::NonNull;

use crate::{Backoff, spsc};

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
            let ret = self.receivers[self.next_shard].try_recv();

            self.next_shard += 1;
            if self.next_shard == self.max_shards {
                self.next_shard = 0;
            }

            if ret.is_some() {
                return ret;
            }

            if self.next_shard == start {
                return None;
            }
        }
    }

    pub fn read_buffer(&mut self) -> &[T] {
        let start = self.next_shard;
        loop {
            let ret = self.receivers[self.next_shard].read_buffer();

            self.next_shard += 1;
            if self.next_shard == self.max_shards {
                self.next_shard = 0;
            }

            if !ret.is_empty() {
                return unsafe { std::mem::transmute::<&[T], &[T]>(ret) };
            }

            if self.next_shard == start {
                return &[];
            }
        }
    }

    pub unsafe fn advance(&mut self, len: usize) {
        let prev = if self.next_shard == 0 {
            self.max_shards - 1
        } else {
            self.next_shard - 1
        };

        unsafe { self.receivers[prev].advance(len) };
    }
}

unsafe impl<T> Send for Receiver<T> {}
