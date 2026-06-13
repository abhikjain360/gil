#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc as alloc_crate;
#[cfg(any(test, feature = "std"))]
extern crate std;

pub(crate) use alloc_crate::boxed::Box;
#[cfg(not(feature = "loom"))]
pub(crate) use alloc_crate::{alloc, sync::Arc};

#[expect(unused_imports)]
#[cfg(not(feature = "loom"))]
pub(crate) use core::{
    cell as std_cell, hint,
    sync::{self, atomic},
};
#[cfg(all(not(feature = "loom"), any(test, feature = "std")))]
use std::thread;

#[cfg(not(any(test, feature = "std", feature = "loom")))]
pub(crate) mod thread {
    pub(crate) use core::hint::spin_loop as yield_now;
}

#[cfg(feature = "loom")]
pub(crate) use loom::sync::Arc;
#[expect(unused_imports)]
#[cfg(feature = "loom")]
pub(crate) use loom::{
    cell as std_cell, hint,
    sync::{self, atomic},
    thread,
};
#[cfg(feature = "loom")]
pub(crate) mod alloc {
    pub use alloc_crate::alloc::handle_alloc_error;
    pub use loom::alloc::Layout;
    pub use loom::alloc::alloc;
    pub use loom::alloc::dealloc;
}

mod backoff;
mod cell;
#[cfg(feature = "std")]
pub(crate) mod futex;
pub mod mpmc;
pub mod mpsc;
mod padded;
pub(crate) mod queue;
pub mod read_guard;
pub(crate) mod ring;
pub(crate) mod shard_table;
pub mod spmc;
pub mod spsc;

pub use backoff::*;
pub(crate) use queue::*;
