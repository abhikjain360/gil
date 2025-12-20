use std::{num::NonZeroUsize, ptr::NonNull};

use crate::spsc;

mod receiver;
mod sender;

pub fn channel<T>(
    max_shards: NonZeroUsize,
    capacity_per_shard: NonZeroUsize,
) -> (sender::Sender<T>, receiver::Receiver<T>) {
    debug_assert_ne!(max_shards.get(), 0, "number of shards must be > 0");
    debug_assert!(
        max_shards.is_power_of_two(),
        "number of shards must be a power of 2"
    );

    let mut shards = Box::new_uninit_slice(max_shards.get());
    for i in 0..max_shards.get() {
        shards[i].write(spsc::QueuePtr::<T>::with_size(capacity_per_shard));
    }
    // SAFETY: Box::new was valid
    let shards = unsafe { NonNull::new_unchecked(Box::into_raw(shards)).cast() };

    // SAFETY: Sender::init(..) will clone, while Receiver::new(..) will move
    (sender::Sender::new(shards, max_shards), unsafe {
        receiver::Receiver::new(shards, max_shards.get())
    })
}
