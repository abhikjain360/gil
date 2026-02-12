#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc as alloc_crate;
#[cfg(any(test, feature = "std"))]
extern crate std;

#[cfg(not(feature = "loom"))]
pub(crate) use alloc_crate::alloc;
pub(crate) use alloc_crate::boxed::Box;

#[allow(unused_imports)]
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

#[allow(unused_imports)]
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

#[allow(unused_macros)]
macro_rules! _field {
    ($ty:ty, $ptr:expr, $($path:tt).+) => {
        $ptr.byte_add(core::mem::offset_of!($ty, $($path).+))
    };

    ($ty:ty, $ptr:expr, $($path:tt).+, $field_ty:ty) => {
        $ptr.byte_add(core::mem::offset_of!($ty, $($path).+)).cast::<$field_ty>()
    };
}

mod backoff;
mod cell;
pub mod mpmc;
pub mod mpsc;
mod padded;
pub mod queue;
pub mod read_guard;
pub mod spmc;
pub mod spsc;

pub use backoff::Backoff;
pub(crate) use queue::*;
