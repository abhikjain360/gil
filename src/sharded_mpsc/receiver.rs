use core::ptr::NonNull;

use crate::{Backoff, spsc};

pub struct Receiver<T> {
    receivers: Box<[spsc::Receiver<T>]>,
    max_shards: usize,
    next_receiver: usize,
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
            next_receiver: 0,
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
        let start = self.next_receiver;
        loop {
            let ret = self.receivers[self.next_receiver].try_recv();
            if ret.is_some() {
                return ret;
            }

            self.next_receiver += 1;
            if self.next_receiver == self.max_shards {
                self.next_receiver = 0;
            }

            if self.next_receiver == start {
                return None;
            }
        }
    }
}
