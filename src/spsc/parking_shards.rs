use core::{num::NonZeroUsize, ptr::NonNull};

use crate::{
    alloc,
    atomic::{AtomicU32, AtomicUsize, Ordering},
    padded::Padded,
    spsc,
};

#[repr(C)]
pub(crate) struct ParkingShards<T> {
    rc: Padded<AtomicUsize>,
    futex: Padded<AtomicU32>,
    queue_ptrs: spsc::QueuePtr<T>,
}

impl<T> ParkingShards<T> {
    pub fn new(max_shards: NonZeroUsize) -> NonNull<Self> {
        let layout = Self::layout(max_shards.get());
        let Some(ptr) = NonNull::new(unsafe { alloc::alloc(layout) }) else {
            alloc::handle_alloc_error(layout)
        };

        unsafe {
            // Initialize rc to 1
            _field!(ParkingShards<T>, ptr, rc.value, AtomicUsize)
                .write(AtomicUsize::new(1));
            // Initialize futex to 0
            _field!(ParkingShards<T>, ptr, futex.value, AtomicU32)
                .write(AtomicU32::new(0));
        }

        ptr.cast()
    }

    fn layout(max_shards: usize) -> alloc::Layout {
        let base = alloc::Layout::new::<Padded<AtomicUsize>>();
        let (layout, _) = base
            .extend(alloc::Layout::new::<Padded<AtomicU32>>())
            .unwrap();
        let (layout, _) = layout
            .extend(alloc::Layout::array::<spsc::QueuePtr<T>>(max_shards).unwrap())
            .unwrap();

        layout.pad_to_align()
    }

    #[inline(always)]
    pub(crate) fn at(ptr: NonNull<Self>, shard: usize) -> NonNull<spsc::QueuePtr<T>> {
        unsafe { _field!(ParkingShards<T>, ptr, queue_ptrs).cast().add(shard) }
    }
}

pub(crate) struct ParkingShardsPtr<T> {
    ptr: NonNull<ParkingShards<T>>,
    max_shards: usize,
}

impl<T> Clone for ParkingShardsPtr<T> {
    fn clone(&self) -> Self {
        self.rc().fetch_add(1, Ordering::AcqRel);

        Self {
            ptr: self.ptr,
            max_shards: self.max_shards,
        }
    }
}

impl<T> ParkingShardsPtr<T> {
    pub fn new(max_shards: NonZeroUsize, capacity_per_shard: NonZeroUsize) -> Self {
        let ptr = ParkingShards::new(max_shards);

        for i in 0..max_shards.get() {
            let ptr = ParkingShards::at(ptr, i);
            unsafe { ptr.write(spsc::QueuePtr::with_size(capacity_per_shard)) };
        }

        Self {
            ptr,
            max_shards: max_shards.get(),
        }
    }

    pub(crate) fn clone_queue_ptr(&self, shard: usize) -> spsc::QueuePtr<T> {
        unsafe { ParkingShards::at(self.ptr, shard).as_ref() }.clone()
    }

    pub(crate) fn futex(&self) -> &AtomicU32 {
        unsafe {
            _field!(ParkingShards<T>, self.ptr, futex.value, AtomicU32).as_ref()
        }
    }

    fn rc(&self) -> &AtomicUsize {
        unsafe { _field!(ParkingShards<T>, self.ptr, rc.value, AtomicUsize).as_ref() }
    }
}

impl<T> Drop for ParkingShardsPtr<T> {
    fn drop(&mut self) {
        if self.rc().fetch_sub(1, Ordering::AcqRel) == 1 {
            unsafe {
                _field!(ParkingShards<T>, self.ptr, rc.value, AtomicUsize).drop_in_place();
                _field!(ParkingShards<T>, self.ptr, futex.value, AtomicU32).drop_in_place();
                for i in 0..self.max_shards {
                    ParkingShards::at(self.ptr, i).drop_in_place();
                }
                alloc::dealloc(
                    self.ptr.as_ptr().cast(),
                    ParkingShards::<T>::layout(self.max_shards),
                );
            }
        }
    }
}
