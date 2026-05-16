use core::{num::NonZeroUsize, ptr::NonNull};

use crate::{
    alloc,
    atomic::{AtomicUsize, Ordering},
    padded::Padded,
    spsc,
};

#[repr(C)]
pub(crate) struct Shards<T> {
    rc: Padded<AtomicUsize>,
    queue_ptrs: spsc::QueuePtr<T>,
}

impl<T> Shards<T> {
    pub fn new(max_shards: NonZeroUsize) -> NonNull<Self> {
        let layout = Self::layout(max_shards.get());
        let Some(ptr) = NonNull::new(unsafe { alloc::alloc(layout) }) else {
            alloc::handle_alloc_error(layout)
        };

        unsafe { ptr.cast::<AtomicUsize>().write(AtomicUsize::new(1)) };

        ptr.cast()
    }

    fn layout(max_shards: usize) -> alloc::Layout {
        let (layout, _offset) = alloc::Layout::new::<Padded<AtomicUsize>>()
            .extend(alloc::Layout::array::<spsc::QueuePtr<T>>(max_shards).unwrap())
            .unwrap();

        layout.pad_to_align()
    }

    #[inline(always)]
    pub(crate) fn at(ptr: NonNull<Self>, shard: usize) -> NonNull<spsc::QueuePtr<T>> {
        unsafe { _field!(Shards<T>, ptr, queue_ptrs).cast().add(shard) }
    }
}

pub(crate) struct ShardsPtr<T> {
    ptr: NonNull<Shards<T>>,
    max_shards: usize,
}

impl<T> Clone for ShardsPtr<T> {
    fn clone(&self) -> Self {
        self.rc().fetch_add(1, Ordering::AcqRel);

        Self {
            ptr: self.ptr,
            max_shards: self.max_shards,
        }
    }
}

impl<T> ShardsPtr<T> {
    pub fn new(max_shards: NonZeroUsize, capacity_per_shard: NonZeroUsize) -> Self {
        let ptr = Shards::new(max_shards);

        for i in 0..max_shards.get() {
            let ptr = Shards::at(ptr, i);
            unsafe { ptr.write(spsc::QueuePtr::with_size(capacity_per_shard)) };
        }

        Self {
            ptr,
            max_shards: max_shards.get(),
        }
    }

    pub(crate) fn clone_queue_ptr(&self, shard: usize) -> spsc::QueuePtr<T> {
        unsafe { Shards::at(self.ptr, shard).as_ref() }.clone()
    }

    pub(crate) fn try_claim_queue_ptr(
        &self,
        shard: usize,
        ref_count: usize,
    ) -> Option<spsc::QueuePtr<T>> {
        unsafe { Shards::at(self.ptr, shard).as_ref() }.try_clone_from_count(ref_count)
    }

    fn rc(&self) -> &AtomicUsize {
        unsafe { _field!(Shards<T>, self.ptr, rc, AtomicUsize).as_ref() }
    }
}

impl<T> Drop for ShardsPtr<T> {
    fn drop(&mut self) {
        if self.rc().fetch_sub(1, Ordering::AcqRel) == 1 {
            unsafe {
                _field!(Shards<T>, self.ptr, rc, AtomicUsize).drop_in_place();
                for i in 0..self.max_shards {
                    Shards::at(self.ptr, i).drop_in_place();
                }
                alloc::dealloc(
                    self.ptr.as_ptr().cast(),
                    Shards::<T>::layout(self.max_shards),
                );
            }
        }
    }
}
