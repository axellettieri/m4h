//! Error types.

use core::fmt;

/// The peer published an index that is impossible for a well-behaved peer
/// (more than the ring capacity apart from ours).
///
/// The handle did not touch any slot. With an untrusted peer this means the
/// peer is crashed or malicious; with a trusted peer it is a bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corrupted;

/// Error returned by single-slot pushes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushError {
    /// The ring is full.
    Full,
    /// The consumer published an invalid index. See [`Corrupted`].
    Corrupted,
}

/// Error returned by single-slot pops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopError {
    /// The ring is empty.
    Empty,
    /// The producer published an invalid index. See [`Corrupted`].
    Corrupted,
}

/// Error returned when attaching to a ring in shared memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachError {
    /// The region does not hold a published ring (magic word missing).
    BadMagic {
        /// Magic word found in the header.
        found: u32,
    },
    /// The ring was created by an incompatible version of the format.
    VersionMismatch {
        /// Format version found in the header.
        found: u32,
    },
    /// The ring has a different capacity than the handle type.
    CapacityMismatch {
        /// Capacity of the handle type.
        expected: u32,
        /// Capacity found in the header.
        found: u32,
    },
    /// The ring was built for a different slot size (different architecture or build).
    SlotSizeMismatch {
        /// Slot size of this build.
        expected: u32,
        /// Slot size found in the header.
        found: u32,
    },
    /// The ring was re-initialized since the caller last saw it.
    GenerationMismatch {
        /// Generation the caller expected.
        expected: u32,
        /// Generation found in the header.
        found: u32,
    },
    /// The published indices are inconsistent.
    Corrupted,
}

impl From<Corrupted> for PushError {
    fn from(_: Corrupted) -> Self {
        PushError::Corrupted
    }
}

impl From<Corrupted> for PopError {
    fn from(_: Corrupted) -> Self {
        PopError::Corrupted
    }
}

impl From<Corrupted> for AttachError {
    fn from(_: Corrupted) -> Self {
        AttachError::Corrupted
    }
}

impl fmt::Display for Corrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ring peer published an invalid index")
    }
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PushError::Full => f.write_str("ring is full"),
            PushError::Corrupted => fmt::Display::fmt(&Corrupted, f),
        }
    }
}

impl fmt::Display for PopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PopError::Empty => f.write_str("ring is empty"),
            PopError::Corrupted => fmt::Display::fmt(&Corrupted, f),
        }
    }
}

impl fmt::Display for AttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttachError::BadMagic { found } => write!(f, "no ring here (magic {found:#010x})"),
            AttachError::VersionMismatch { found } => {
                write!(f, "unsupported ring format version {found}")
            }
            AttachError::CapacityMismatch { expected, found } => {
                write!(f, "ring capacity {found}, expected {expected}")
            }
            AttachError::SlotSizeMismatch { expected, found } => {
                write!(f, "ring slot size {found}, expected {expected}")
            }
            AttachError::GenerationMismatch { expected, found } => {
                write!(f, "ring generation {found}, expected {expected}")
            }
            AttachError::Corrupted => fmt::Display::fmt(&Corrupted, f),
        }
    }
}

impl core::error::Error for Corrupted {}
impl core::error::Error for PushError {}
impl core::error::Error for PopError {}
impl core::error::Error for AttachError {}
