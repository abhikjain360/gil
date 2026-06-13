//! Futex-based parking, written once for every parking variant.
//!
//! All parking in this crate follows the same Dekker-style protocol between a
//! *waiter* (a blocked sender or receiver) and a *waker* (the opposite endpoint):
//!
//! * **Waiter:** [`announce`](Futex::announce) intent to park, **recheck** the
//!   queue against fresh indices, and only then [`sleep`](Futex::sleep).
//! * **Waker:** publish the new head/tail, then check the futex word and wake
//!   ([`wake`](Futex::wake) / [`wake_all`](Futex::wake_all)) if anyone announced.
//!
//! The recheck-after-announce is what prevents lost wakes, and it only works if
//! the announce store cannot be reordered after the recheck load — hence the
//! `SeqCst` CAS in `announce`. Symmetrically the waker's futex load must not be
//! reordered before its index store, hence the `SeqCst` accesses in `wake` and
//! `wake_all`. With that ordering, either the waiter's recheck sees the waker's
//! published index (and skips sleeping), or the waker's load sees the announce
//! (and wakes).
//!
//! Under `loom` the futex word is still modelled, but the actual OS park/wake is
//! compiled out: `sleep` degrades to a yield (loom cannot model `atomic_wait`,
//! and a real futex wait would block the model scheduler). This turns parking
//! into spinning inside loom models, which preserves the data-path orderings
//! loom is checking.

use core::ptr::NonNull;

use crate::{
    atomic::{AtomicU32, Ordering},
    queue::{DropInFlight, Ownership, QueuePtr},
};

pub(crate) const FREE: u32 = 0;
pub(crate) const SENDER_WAITING: u32 = 1;
pub(crate) const RECEIVER_WAITING: u32 = 2;

/// A futex word embedded in a queue header.
///
/// Holds a raw pointer rather than a borrow so wait loops can keep a copy while
/// mutating their cursors. The word lives in the queue allocation, which every
/// holder of a `Futex` keeps alive via its own queue handle.
#[derive(Clone, Copy)]
pub(crate) struct Futex(NonNull<AtomicU32>);

impl Futex {
    #[inline(always)]
    fn word(&self) -> &AtomicU32 {
        // SAFETY: see the struct docs — the queue allocation outlives this handle.
        unsafe { self.0.as_ref() }
    }
    /// Announces intent to park as `who` (Dekker step 1).
    ///
    /// Returns `true` if `who` is now announced — either this call claimed the
    /// word, or it already held `who` (a leftover from an aborted park, or a
    /// fellow waiter of the same class on a shared futex). Returns `false` if
    /// the opposite class is announced; the caller should skip parking and
    /// retry its operation instead.
    #[inline(always)]
    pub(crate) fn announce(self, who: u32) -> bool {
        match self
            .word()
            .compare_exchange(FREE, who, Ordering::SeqCst, Ordering::Relaxed)
        {
            Ok(_) => true,
            Err(current) => current == who,
        }
    }

    /// Parks the thread while the futex word still reads `who` (Dekker step 3).
    ///
    /// Must be preceded by a successful [`announce`](Self::announce) and a
    /// recheck of the queue against fresh indices.
    #[inline(always)]
    pub(crate) fn sleep(self, who: u32) {
        #[cfg(not(feature = "loom"))]
        atomic_wait::wait(self.word(), who);

        #[cfg(feature = "loom")]
        {
            _ = who;
            crate::thread::yield_now();
        }
    }

    /// Wakes the parked waiter, if any. Call after publishing the new index.
    #[inline(always)]
    pub(crate) fn wake(self) {
        let word = self.word();
        if word.load(Ordering::SeqCst) != FREE {
            word.store(FREE, Ordering::Relaxed);
            #[cfg(not(feature = "loom"))]
            atomic_wait::wake_one(word);
        }
    }

    /// Wakes every parked waiter. Used by the MPMC core, where one futex word
    /// is shared by all senders and all receivers.
    // TODO(deferred): wake_all causes a thundering herd; side-specific futexes
    // with wake_one are tracked in TODO.md.
    #[inline(always)]
    pub(crate) fn wake_all(self) {
        let word = self.word();
        if word.load(Ordering::Relaxed) != FREE {
            word.store(FREE, Ordering::SeqCst);
            #[cfg(not(feature = "loom"))]
            atomic_wait::wake_all(word);
        }
    }
}

/// Implemented by queue-header head types that embed a futex word.
// `pub` rather than `pub(crate)` only to satisfy `private_bounds` on the
// accessor below; the module itself is crate-private.
pub trait HasFutex {
    fn futex(&self) -> &AtomicU32;
}

impl<H, T, I, G, O> QueuePtr<H, T, I, G, O>
where
    H: HasFutex,
    G: DropInFlight<H, T, I>,
    O: Ownership,
{
    #[inline(always)]
    pub(crate) fn futex(&self) -> Futex {
        Futex(NonNull::from(self.header().head.futex()))
    }
}
