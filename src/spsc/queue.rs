//! Ring layouts for the SPSC family.
//!
//! Two header layouts share the one ring algorithm in [`crate::ring`]:
//!
//! * [`Head`]/[`Tail`] — the full SPSC layout, used by both the spin and parking
//!   SPSC queues. Carries the futex word under `std` and the async wakers under
//!   `async`. Spin-only users pay one untouched cache line for the futex; that
//!   cost is memory only, never on the hot path.
//! * [`ShardHead`]/[`ShardTail`] — the shard layout used by the sharded channels.
//!   Shards have no async API, so this layout carries only what shards use: the
//!   indices and (under `std`) the futex the parking variants park on.

use core::ptr::NonNull;
#[cfg(feature = "async")]
use core::task::Waker;

#[cfg(feature = "async")]
use futures::task::AtomicWaker;

#[cfg(feature = "std")]
use crate::{atomic::AtomicU32, futex::HasFutex};
use crate::{
    atomic::{AtomicUsize, Ordering},
    padded::Padded,
    queue::ShardOwnership,
    ring::{RingHead, RingTail},
};

#[derive(Default)]
#[repr(C)]
pub struct Head {
    head: Padded<AtomicUsize>,
    #[cfg(feature = "std")]
    futex: Padded<AtomicU32>,
    #[cfg(feature = "async")]
    receiver_waker: Padded<AtomicWaker>,
}

#[derive(Default)]
#[repr(C)]
pub struct Tail {
    tail: Padded<AtomicUsize>,
    #[cfg(feature = "async")]
    sender_waker: Padded<AtomicWaker>,
}

#[derive(Default)]
#[repr(C)]
pub struct ShardHead {
    head: Padded<AtomicUsize>,
    #[cfg(feature = "std")]
    futex: Padded<AtomicU32>,
}

#[derive(Default)]
#[repr(C)]
pub struct ShardTail {
    tail: Padded<AtomicUsize>,
}

impl RingHead for Head {
    #[inline(always)]
    fn head(&self) -> &AtomicUsize {
        &self.head.value
    }
}

impl RingTail for Tail {
    #[inline(always)]
    fn tail(&self) -> &AtomicUsize {
        &self.tail.value
    }
}

impl RingHead for ShardHead {
    #[inline(always)]
    fn head(&self) -> &AtomicUsize {
        &self.head.value
    }
}

impl RingTail for ShardTail {
    #[inline(always)]
    fn tail(&self) -> &AtomicUsize {
        &self.tail.value
    }
}

#[cfg(feature = "std")]
impl HasFutex for Head {
    #[inline(always)]
    fn futex(&self) -> &AtomicU32 {
        &self.futex.value
    }
}

#[cfg(feature = "std")]
impl HasFutex for ShardHead {
    #[inline(always)]
    fn futex(&self) -> &AtomicU32 {
        &self.futex.value
    }
}

/// Drops the in-flight items of a ring (the `head..tail` window) on teardown.
/// One impl serves every layout whose header implements [`RingHead`]/[`RingTail`].
pub struct DropWindow;

impl<H: RingHead, T: RingTail, I> crate::DropInFlight<H, T, I> for DropWindow {
    unsafe fn drop_in_flight(
        head: &H,
        tail: &T,
        _capacity: usize,
        at: impl Fn(usize) -> NonNull<I>,
    ) {
        if !core::mem::needs_drop::<I>() {
            return;
        }

        let head = head.head().load(Ordering::Relaxed);
        let tail = tail.tail().load(Ordering::Relaxed);
        let len = tail.wrapping_sub(head);

        for i in 0..len {
            let idx = head.wrapping_add(i);
            unsafe { at(idx).drop_in_place() };
        }
    }
}

pub(crate) type QueuePtr<T> = crate::QueuePtr<Head, Tail, T, DropWindow>;
/// One shard of a sharded channel: an SPSC ring with role-claimed ownership.
pub(crate) type Shard<T> = crate::QueuePtr<ShardHead, ShardTail, T, DropWindow, ShardOwnership>;

#[cfg(feature = "async")]
impl<T> QueuePtr<T> {
    #[inline(always)]
    pub fn register_sender_waker(&self, waker: &Waker) {
        self.header().tail.sender_waker.value.register(waker);
    }

    #[inline(always)]
    pub fn register_receiver_waker(&self, waker: &Waker) {
        self.header().head.receiver_waker.value.register(waker);
    }

    #[inline(always)]
    pub(crate) fn wake_sender(&self) {
        self.header().tail.sender_waker.value.wake();
    }

    #[inline(always)]
    pub(crate) fn wake_receiver(&self) {
        self.header().head.receiver_waker.value.wake();
    }
}
