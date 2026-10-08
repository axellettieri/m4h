//! The platform error type, shared by every part of the trait.

use core::fmt;

/// What went wrong, independently of the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Not enough memory (or not enough pages of the requested size).
    OutOfMemory,
    /// The backend or the hardware does not support the request.
    Unsupported,
    /// An argument is out of range (unknown core, zero length, ...).
    InvalidArgument,
    /// The named object (core, device, node) does not exist.
    NotFound,
    /// The caller lacks the permission.
    PermissionDenied,
    /// Any other failure reported by the system.
    Other,
}

/// A platform error: a portable kind plus the backend's own code, if any
/// (`errno` on Linux).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    /// Portable classification.
    pub kind: ErrorKind,
    /// Backend-specific code (`errno` on Linux), `0` if none.
    pub code: i32,
}

impl Error {
    /// An error with no backend-specific code.
    pub const fn new(kind: ErrorKind) -> Self {
        Self { kind, code: 0 }
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self::new(kind)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.code == 0 {
            write!(f, "{:?}", self.kind)
        } else {
            write!(f, "{:?} (code {})", self.kind, self.code)
        }
    }
}

impl core::error::Error for Error {}
