/// A spinning backoff strategy that spins for a configurable number of iterations
/// before yielding the thread.
///
/// This is used internally by the blocking `send` and `recv` methods to wait for
/// queue space or data to become available. It can also be used directly for custom
/// retry loops with [`try_send`](crate::spsc::Sender::try_send) and
/// [`try_recv`](crate::spsc::Receiver::try_recv).
///
/// The strategy works as follows:
/// 1. For the first `spin_count` calls to [`backoff`](Backoff::backoff), the CPU
///    spins via [`core::hint::spin_loop`].
/// 2. After `spin_count` spins, the counter resets and the thread yields
///    (via [`std::thread::yield_now`] when `std` is available, or
///    [`core::hint::spin_loop`] in `no_std`).
///
/// # Examples
///
/// ```
/// use core::num::NonZeroUsize;
/// use gil::Backoff;
/// use gil::spsc::channel;
///
/// let (mut tx, mut rx) = channel::<i32>(NonZeroUsize::new(16).unwrap());
/// tx.send(42);
///
/// // Custom retry loop using Backoff
/// let mut backoff = Backoff::with_spin_count(64);
/// loop {
///     match rx.try_recv() {
///         Some(val) => {
///             assert_eq!(val, 42);
///             break;
///         }
///         None => backoff.backoff(),
///     }
/// }
/// ```
pub struct Backoff {
    max_spin: u32,
    spin_count: u32,
}

impl Backoff {
    /// Creates a new `Backoff` with the given spin count.
    ///
    /// The backoff will spin for `spin_count` iterations before yielding the
    /// thread on each cycle.
    ///
    /// # Examples
    ///
    /// ```
    /// use gil::Backoff;
    ///
    /// let backoff = Backoff::with_spin_count(128);
    /// ```
    #[inline(always)]
    pub fn with_spin_count(max_spin: u32) -> Self {
        Self {
            max_spin,
            spin_count: 0,
        }
    }

    /// Updates the spin count.
    ///
    /// This does not reset the current spin counter. Call [`reset`](Backoff::reset)
    /// to restart the cycle.
    ///
    /// # Examples
    ///
    /// ```
    /// use gil::Backoff;
    ///
    /// let mut backoff = Backoff::with_spin_count(64);
    /// backoff.set_spin_count(256);
    /// ```
    #[inline(always)]
    pub fn set_spin_count(&mut self, max_spin: u32) {
        self.max_spin = max_spin;
    }

    /// Performs one backoff step.
    ///
    /// If fewer than `spin_count` spins have occurred since the last reset,
    /// this spins the CPU. Once `spin_count` is reached, the counter resets
    /// and the thread yields.
    ///
    /// # Examples
    ///
    /// ```
    /// use gil::Backoff;
    ///
    /// let mut backoff = Backoff::with_spin_count(4);
    /// // First 4 calls spin, the 5th yields and resets
    /// for _ in 0..5 {
    ///     backoff.backoff();
    /// }
    /// ```
    #[inline(always)]
    pub fn backoff(&mut self) {
        if self.spin_count < self.max_spin {
            crate::hint::spin_loop();
            self.spin_count += 1;
        } else {
            self.spin_count = 0;
            crate::thread::yield_now();
        }
    }

    /// Resets the spin counter to zero.
    ///
    /// The next call to [`backoff`](Backoff::backoff) will start spinning from
    /// the beginning.
    ///
    /// # Examples
    ///
    /// ```
    /// use gil::Backoff;
    ///
    /// let mut backoff = Backoff::with_spin_count(4);
    /// for _ in 0..3 {
    ///     backoff.backoff();
    /// }
    /// backoff.reset();
    /// // Starts spinning again from the beginning
    /// backoff.backoff();
    /// ```
    #[inline(always)]
    pub fn reset(&mut self) {
        self.spin_count = 0;
    }
}

/// A three-phase backoff strategy used by the parking queue variants.
///
/// This backoff progresses through three phases before signalling the caller
/// to park:
///
/// 1. **Spin** — calls [`core::hint::spin_loop`] up to `max_spin` times.
/// 2. **Yield** — calls [`std::thread::yield_now`] (or `spin_loop` in `no_std`)
///    up to `max_yield` times, resetting the spin counter each time.
/// 3. **Park** — once both budgets are exhausted, [`backoff`](ParkingBackoff::backoff)
///    returns `true` on every subsequent call, indicating the caller should park
///    on a futex or other blocking primitive.
///
/// This is used internally by [`spsc::parking`](crate::spsc::parking) to decide
/// when to transition from spinning to futex-based sleeping.
///
/// # Examples
///
/// ```
/// use gil::ParkingBackoff;
///
/// let mut backoff = ParkingBackoff::new(4, 2);
///
/// // First 4 calls spin (returns false)
/// for _ in 0..4 {
///     assert!(!backoff.backoff());
/// }
///
/// // Next 2 × (4+1) calls yield then spin (returns false)
/// // After that, returns true forever
/// loop {
///     if backoff.backoff() {
///         // Time to park on a futex
///         break;
///     }
/// }
/// ```
pub struct ParkingBackoff {
    max_yield: u32,
    yield_count: u32,
    max_spin: u32,
    spin_count: u32,
}

impl ParkingBackoff {
    /// Creates a new `ParkingBackoff` with the given spin and yield budgets.
    ///
    /// * `max_spin` — number of [`spin_loop`](core::hint::spin_loop) iterations
    ///   per spin phase.
    /// * `max_yield` — number of yield phases before the backoff signals to park.
    pub fn new(max_spin: u32, max_yield: u32) -> Self {
        Self {
            max_yield,
            yield_count: 0,
            max_spin,
            spin_count: 0,
        }
    }

    /// Performs one backoff step, returning `true` when the caller should park.
    ///
    /// Returns `false` while still in the spin or yield phases, and `true`
    /// once both budgets are exhausted (and on every subsequent call).
    #[inline(always)]
    pub fn backoff(&mut self) -> bool {
        if self.spin_count < self.max_spin {
            crate::hint::spin_loop();
            self.spin_count += 1;
        } else if self.yield_count < self.max_yield {
            crate::thread::yield_now();
            self.spin_count = 0;
            self.yield_count += 1;
        } else {
            return true;
        }

        false
    }

    /// Resets the spin counter to zero.
    ///
    /// The next call to [`backoff`](ParkingBackoff::backoff) will start spinning from
    /// the beginning.
    #[inline(always)]
    pub fn reset(&mut self) {
        self.spin_count = 0;
        self.yield_count = 0;
    }
}
