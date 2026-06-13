//! Regression test for the futex parking lost-wake bug.
//!
//! The parking protocol publishes a new head/tail (a `Release` store) and then
//! loads the futex word to decide whether to wake a parked peer. Those are a
//! store and a load to *different* locations, so ordering them requires a
//! `SeqCst` fence between them — a `SeqCst` load alone is **not** ordered after
//! an earlier `Release` store elsewhere. With the fence missing, x86 lets the
//! word load execute ahead of the still-store-buffered index publish, read a
//! stale `FREE`, and skip the wake; the peer that already announced `WAITING`
//! then parks forever (`atomic_wait` is untimed) and the channel deadlocks.
//!
//! Capacity 1 forces both endpoints to park on nearly every message, maximizing
//! announce/wake traffic and so the odds of hitting the race. The work runs
//! under a wall-clock budget on a worker thread; if it ever stops making
//! progress, the deadline below turns the deadlock into a test failure instead
//! of a hung CI job.
//!
//! The race is probabilistic and ISA-dependent. On x86 (e.g. the GitHub
//! `ubuntu-latest` runners) a regression has a real chance of tripping this; on
//! AArch64 it passes unconditionally, because `ldar` cannot be reordered before
//! an earlier `stlr`. Set `GIL_LOST_WAKE_SECS` to soak it harder.

#![cfg(feature = "std")]

use std::num::NonZeroUsize;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use gil::spsc::parking::channel;

/// Ping-pong round trips per channel pair before tearing it down and starting
/// a fresh pair (fresh pairs also exercise the park state from a clean slate).
const ROUNDS: u64 = 1000;

fn budget() -> Duration {
    let secs = std::env::var("GIL_LOST_WAKE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    Duration::from_secs(secs)
}

#[test]
fn parking_pingpong_no_lost_wake() {
    let budget = budget();
    let (done_tx, done_rx) = mpsc::channel();

    let worker = thread::spawn(move || {
        let deadline = Instant::now() + budget;
        let mut attempts = 0u64;
        while Instant::now() < deadline {
            let cap = NonZeroUsize::new(1).unwrap();
            let (mut tx1, mut rx1) = channel::<u8>(cap);
            let (mut tx2, mut rx2) = channel::<u8>(cap);

            let echo = thread::spawn(move || {
                for i in 0..ROUNDS {
                    let _ = rx1.recv();
                    tx2.send(i as u8);
                }
            });
            for i in 0..ROUNDS {
                tx1.send(i as u8);
                let _ = rx2.recv();
            }
            echo.join().unwrap();
            attempts += 1;
        }
        let _ = done_tx.send(attempts);
    });

    // `recv_timeout` returns the moment the worker signals done, so a healthy
    // run pays only its real runtime (~`budget`); the grace window is reached
    // only if a lost wake has actually parked a thread forever.
    match done_rx.recv_timeout(budget + Duration::from_secs(30)) {
        Ok(_attempts) => worker.join().unwrap(),
        Err(_) => panic!(
            "futex parking regression: ping-pong deadlocked (lost wake). A peer \
             announced WAITING, the waker read a stale futex word and skipped \
             the wake, and the peer parked forever. Check the SeqCst fences in \
             src/futex.rs."
        ),
    }
}
