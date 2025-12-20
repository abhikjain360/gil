#![doc = include_str!("../README.md")]

#[allow(unused_imports)]
#[cfg(not(feature = "loom"))]
pub(crate) use std::{
    alloc, cell as std_cell, hint,
    sync::{self, atomic},
    thread,
};

#[allow(unused_imports)]
#[cfg(feature = "loom")]
pub(crate) use loom::{
    alloc, cell as std_cell, hint,
    sync::{self, atomic},
    thread,
};

#[allow(unused_macros)]
macro_rules! _field {
    ($ty:ty, $ptr:expr, $($path:tt).+) => {
        $ptr.byte_add(offset_of!($ty, $($path).+))
    };

    ($ty:ty, $ptr:expr, $($path:tt).+, $field_ty:ty) => {
        $ptr.byte_add(offset_of!($ty, $($path).+)).cast::<$field_ty>()
    };
}

mod backoff;
mod cell;
pub mod mpmc;
pub mod mpsc;
mod padded;
pub mod sharded_mpsc;
pub mod spmc;
pub mod spsc;

pub use backoff::Backoff;
