//! The Vyukov queue slot and its shared teardown/initialisation policies.

use core::{cell::UnsafeCell, marker::PhantomData, mem::MaybeUninit, ptr::NonNull};

use crate::{
    atomic::{AtomicUsize, Ordering},
    ring::{RingHead, RingTail},
};

/// One slot of a Vyukov queue: a cache-line-aligned epoch plus the payload.
///
/// The epoch is the per-cell state machine the mpsc/mpmc/spmc cores synchronise
/// on. `data` is interior-mutable so an endpoint can write the payload through a
/// shared reference once it has claimed the cell via the epoch protocol.
#[repr(align(64))]
#[cfg_attr(all(target_arch = "aarch64", target_os = "macos"), repr(align(128)))]
pub(crate) struct Cell<T> {
    epoch: AtomicUsize,
    data: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Cell<T> {
    #[inline(always)]
    pub(crate) fn epoch(&self) -> &AtomicUsize {
        &self.epoch
    }

    /// Moves the value out of the cell.
    ///
    /// # Safety
    /// The cell must hold an initialised value that no other endpoint accesses
    /// (guaranteed by the epoch protocol), and it must not be read again.
    #[inline(always)]
    pub(crate) unsafe fn get(&self) -> T {
        unsafe { (*self.data.get()).assume_init_read() }
    }

    /// Writes `value` into the cell. The caller must have claimed the cell via
    /// the epoch protocol, which is what makes this shared-reference write
    /// race-free.
    #[inline(always)]
    pub(crate) fn set(&self, value: T) {
        unsafe { (*self.data.get()).write(value) };
    }

    /// Drops the value in place.
    ///
    /// # Safety
    /// The cell must hold an initialised value that is never accessed again.
    #[inline(always)]
    pub(crate) unsafe fn drop_in_place(&self) {
        if core::mem::needs_drop::<T>() {
            unsafe { (*self.data.get()).assume_init_drop() };
        }
    }
}

/// Sets each cell's epoch to its index — Vyukov's initial "free, awaiting the
/// first producer lap" state. Shared by the mpsc/mpmc/spmc constructors.
pub(crate) struct CellInit<T> {
    _marker: PhantomData<T>,
}

impl<T> crate::Initializer for CellInit<T> {
    type Item = Cell<T>;

    fn initialize(idx: usize, cell: &mut Self::Item) {
        cell.epoch().store(idx, Ordering::Relaxed);
    }
}

/// Teardown for the mpsc/mpmc cores: scan one lap back from the tail; a cell
/// whose epoch reads `idx + 1` was written by a producer and not yet consumed.
pub(crate) struct DropTailScan;

impl<H, T: RingTail, I> crate::DropInFlight<H, T, Cell<I>> for DropTailScan {
    unsafe fn drop_in_flight(
        _head: &H,
        tail: &T,
        capacity: usize,
        at: impl Fn(usize) -> NonNull<Cell<I>>,
    ) {
        if !core::mem::needs_drop::<I>() {
            return;
        }

        let tail = tail.tail().load(Ordering::Relaxed);

        for i in 1..=capacity {
            let idx = tail.wrapping_sub(i);
            let cell = unsafe { at(idx).as_ref() };
            if cell.epoch().load(Ordering::Relaxed) == idx.wrapping_add(1) {
                unsafe { cell.drop_in_place() };
            }
        }
    }
}

/// Teardown for the spmc core: scan one lap forward from the head; a cell whose
/// epoch is ahead of its index but on the same lap holds an unconsumed value.
pub(crate) struct DropHeadScan;

impl<H: RingHead, T, I> crate::DropInFlight<H, T, Cell<I>> for DropHeadScan {
    unsafe fn drop_in_flight(
        head: &H,
        _tail: &T,
        capacity: usize,
        at: impl Fn(usize) -> NonNull<Cell<I>>,
    ) {
        if !core::mem::needs_drop::<I>() {
            return;
        }

        let head = head.head().load(Ordering::Relaxed);

        for i in 0..capacity {
            let idx = head.wrapping_add(i);
            let cell = unsafe { at(idx).as_ref() };
            let epoch = cell.epoch().load(Ordering::Relaxed);
            if epoch > idx && (epoch & (capacity - 1)) == (idx & (capacity - 1)) {
                unsafe { cell.drop_in_place() };
            }
        }
    }
}
