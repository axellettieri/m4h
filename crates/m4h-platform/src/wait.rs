//! Waiting.

use crate::Instant;

/// How to wait.
///
/// The hot path of M4H polls and never blocks. These hints cover the cases in
/// between; waking a core that waits on a ring (MWAIT, IPI, futex) is not part
/// of this draft yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitHint {
    /// Return immediately; the caller spins.
    Spin,
    /// One CPU relax hint (`pause` on x86_64, `yield` on aarch64): lets the
    /// SMT sibling run and saves power while spinning.
    Relax,
    /// Give the core to another runnable thread, if the platform has any.
    Yield,
    /// Sleep until the given instant.
    Until(Instant),
}

/// Waiting.
pub trait Wait {
    /// Waits as described by `hint`.
    fn wait(&self, hint: WaitHint);
}
