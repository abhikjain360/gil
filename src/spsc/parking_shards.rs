use core::{num::NonZeroUsize, ptr::NonNull};

use crate::{
    Box,
    atomic::{AtomicU32, AtomicUsize, Ordering},
    padded::Padded,
    spsc::{self, shards::ShardsPtr},
};

struct ParkingShards<T> {
    rc: Padded<AtomicUsize>,
    futexes: Box<[Padded<AtomicU32>]>,
    shards: ShardsPtr<T>,
}

pub(crate) struct ParkingShardsPtr<T> {
    ptr: NonNull<ParkingShards<T>>,
}

impl<T> Clone for ParkingShardsPtr<T> {
    fn clone(&self) -> Self {
        self.shared().rc.value.fetch_add(1, Ordering::AcqRel);

        Self { ptr: self.ptr }
    }
}

impl<T> ParkingShardsPtr<T> {
    pub fn new(max_shards: NonZeroUsize, capacity_per_shard: NonZeroUsize) -> Self {
        let shared = Box::new(ParkingShards {
            rc: Padded::new(AtomicUsize::new(1)),
            futexes: (0..max_shards.get())
                .map(|_| Padded::new(AtomicU32::new(0)))
                .collect(),
            shards: ShardsPtr::new(max_shards, capacity_per_shard),
        });

        Self {
            ptr: unsafe { NonNull::new_unchecked(Box::into_raw(shared)) },
        }
    }

    pub(crate) fn claim_producer_queue_ptr(
        &self,
        shard: usize,
    ) -> Option<spsc::queue::ShardQueuePtr<T>> {
        self.shared().shards.claim_producer_queue_ptr(shard)
    }

    pub(crate) fn claim_consumer_queue_ptr(
        &self,
        shard: usize,
    ) -> Option<spsc::queue::ShardQueuePtr<T>> {
        self.shared().shards.claim_consumer_queue_ptr(shard)
    }

    #[inline(always)]
    pub(crate) fn futex(&self, shard: usize) -> &AtomicU32 {
        &self.shared().futexes[shard].value
    }

    #[inline(always)]
    fn shared(&self) -> &ParkingShards<T> {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> Drop for ParkingShardsPtr<T> {
    fn drop(&mut self) {
        if self.shared().rc.value.fetch_sub(1, Ordering::AcqRel) == 1 {
            unsafe { _ = Box::from_raw(self.ptr.as_ptr()) };
        }
    }
}
