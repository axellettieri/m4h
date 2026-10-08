//! Time.

use core::ops::{Add, Sub};
use core::time::Duration;

/// A point on the platform's monotonic clock, in nanoseconds since an
/// unspecified origin (boot, on M4H).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Instant(pub u64);

impl Instant {
    /// Nanoseconds since the clock's origin.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Time elapsed from `earlier` to `self`, zero if `earlier` is later.
    pub const fn saturating_duration_since(self, earlier: Instant) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, d: Duration) -> Instant {
        Instant(
            self.0
                .saturating_add(d.as_nanos().min(u64::MAX as u128) as u64),
        )
    }
}

impl Sub for Instant {
    type Output = Duration;
    fn sub(self, earlier: Instant) -> Duration {
        self.saturating_duration_since(earlier)
    }
}

/// Clocks.
pub trait Clock {
    /// Monotonic time, nanosecond resolution.
    fn now(&self) -> Instant;

    /// A fast, monotonic, per-core-consistent tick counter (TSC on x86_64).
    /// Use it for measurements; convert with [`Clock::tick_hz`].
    fn ticks(&self) -> u64;

    /// Frequency of [`Clock::ticks`], in ticks per second.
    fn tick_hz(&self) -> u64;
}
