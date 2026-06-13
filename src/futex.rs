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
//! Both sides are a store-then-load on *different* locations (waiter: store
//! word, load index; waker: store index, load word). That shape needs a
//! `SeqCst` **fence** between the store and the load on both sides — `SeqCst`
//! on the accesses themselves is not enough. In particular, a `SeqCst` load is
//! not ordered after an earlier `Release` store to another location: on x86
//! both compile to plain `mov`s, and the store can sit in the store buffer
//! while the load executes, exactly the store-buffer litmus that loses the
//! wake. (AArch64 happens to forbid it — `ldar` cannot pass an earlier `stlr`
//! — which is why the weaker form never misbehaved on Apple Silicon.)
//!
//! The two fences are totally ordered. If the waiter's fence comes first, the
//! waker's post-fence load sees the announce and wakes; if the waker's fence
//! comes first, the waiter's post-fence recheck sees the published index and
//! skips sleeping. Either way the wake cannot be lost. The fences live in
//! [`announce`](Futex::announce) and [`wake`](Futex::wake) /
//! [`wake_all`](Futex::wake_all), so callers only have to keep the protocol
//! order: announce → recheck → sleep, and publish → wake.
//!
//! Under `loom` the futex word is still modelled, but the actual OS park/wake is
//! compiled out: `sleep` degrades to a yield (loom cannot model `atomic_wait`,
//! and a real futex wait would block the model scheduler). This turns parking
//! into spinning inside loom models, which preserves the data-path orderings
//! loom is checking.

use core::ptr::NonNull;

use crate::{
    atomic::{AtomicU32, Ordering, fence},
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
        // `Relaxed` is enough on the CAS itself: no data is published through
        // the word (RMWs read the latest value regardless of ordering), and
        // all cross-location ordering comes from the fence below.
        let announced =
            match self
                .word()
                .compare_exchange(FREE, who, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => true,
                Err(current) => current == who,
            };
        // Order the announce store before the caller's recheck loads:
        // store-then-load on different locations is only ordered through a
        // fence. Pairs with the fence in `wake`/`wake_all`; see module docs.
        fence(Ordering::SeqCst);
        announced
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
        // Order the caller's index publish before the announce-word load.
        // Pairs with the fence in `announce`; see the module docs.
        fence(Ordering::SeqCst);
        if word.load(Ordering::Relaxed) != FREE {
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
        // Same fence pairing as `wake`; see the module docs.
        fence(Ordering::SeqCst);
        if word.load(Ordering::Relaxed) != FREE {
            word.store(FREE, Ordering::Relaxed);
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
