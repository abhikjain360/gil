use core::{mem::MaybeUninit, num::NonZeroUsize, ptr::NonNull};

use crate::{
    spsc,
    sync::atomic::{AtomicUsize, Ordering},
};

pub struct Sender<T> {
    inner: spsc::Sender<T>,
    shards: NonNull<spsc::QueuePtr<T>>,
    num_senders: NonNull<AtomicUsize>,
    max_shards: usize,
}

impl<T> Sender<T> {
    pub fn clone(&self) -> Option<Self> {
        unsafe { Self::init(self.shards, self.max_shards, self.num_senders) }
    }

    pub(crate) fn new(shards: NonNull<spsc::QueuePtr<T>>, max_shards: NonZeroUsize) -> Self {
        let num_senders_ptr = Box::into_raw(Box::new(AtomicUsize::new(0)));
        unsafe {
            let num_senders = NonNull::new_unchecked(num_senders_ptr);
            Self::init(shards, max_shards.get(), num_senders).unwrap_unchecked()
        }
    }

    pub(crate) unsafe fn init(
        shards: NonNull<spsc::QueuePtr<T>>,
        max_shards: usize,
        num_senders: NonNull<AtomicUsize>,
    ) -> Option<Self> {
        let num_senders_ref = unsafe { num_senders.as_ref() };
        let next_shard = num_senders_ref.fetch_add(1, Ordering::Relaxed);
        if next_shard >= max_shards {
            num_senders_ref.store(max_shards, Ordering::Relaxed);
            return None;
        }

        let shard_ptr = unsafe { shards.add(next_shard).as_ref() }.clone();
        let inner = spsc::Sender::new(shard_ptr);

        Some(Self {
            inner,
            shards,
            num_senders,
            max_shards,
        })
    }

    pub fn send(&mut self, value: T) {
        self.inner.send(value)
    }

    pub fn try_send(&mut self, value: T) -> Result<(), T> {
        self.inner.try_send(value)
    }
    pub fn write_buffer(&mut self) -> &mut [MaybeUninit<T>] {
        self.inner.write_buffer()
    }

    pub unsafe fn commit(&mut self, len: usize) {
        unsafe { self.inner.commit(len) }
    }
}

unsafe impl<T> Send for Sender<T> {}
