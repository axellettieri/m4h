//! Trust modes of a ring handle.

mod sealed {
    pub trait Sealed {}
}

/// How much a handle trusts the peer on the other side of the ring.
///
/// The mode is a type parameter of [`Producer`](crate::Producer) and
/// [`Consumer`](crate::Consumer), fixed when the handle is created, so the
/// choice costs nothing at run time. Both modes validate every index read from
/// the peer; they differ in how slot contents are accessed.
pub trait Mode: sealed::Sealed + Send + 'static {
    /// `true` if the peer may be crashed or malicious.
    const UNTRUSTED: bool;
}

/// The peer is trusted: same program, or same kernel and trusted code.
///
/// Slot contents can be accessed in place ([`Producer::reserve`],
/// [`Consumer::peek`]) with no copy.
///
/// [`Producer::reserve`]: crate::Producer::reserve
/// [`Consumer::peek`]: crate::Consumer::peek
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trusted {}

/// The peer may be crashed or malicious (another partition or application).
///
/// Slot contents are only ever copied, with volatile accesses: the handle never
/// creates a Rust reference to memory the peer can write. A reference to
/// memory that changes underneath it is undefined behaviour, and validating
/// shared memory in place is a time-of-check/time-of-use bug. Copy first,
/// then validate the local copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Untrusted {}

impl sealed::Sealed for Trusted {}
impl sealed::Sealed for Untrusted {}

impl Mode for Trusted {
    const UNTRUSTED: bool = false;
}

impl Mode for Untrusted {
    const UNTRUSTED: bool = true;
}
