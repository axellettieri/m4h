//! The 64-byte, cache-line-aligned slot and the per-architecture layout constants.

use crate::Pod;
use core::fmt;

/// Size in bytes of one ring slot: one cache line of the target architecture.
///
/// x86_64 and the server aarch64 parts we target (Ampere Altra/AmpereOne) both
/// use 64-byte cache lines. A port to an architecture with a different line
/// size changes this constant and the `align` of [`Slot`] together; the
/// compile-time assertions below keep them in sync.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub const SLOT_SIZE: usize = 64;

/// Size in bytes of one ring slot (fallback for other architectures).
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub const SLOT_SIZE: usize = 64;

/// Alignment and padding of each ring index (`head`, `tail`).
///
/// On x86_64 this is 128 bytes, not 64: Intel's L2 spatial prefetcher fetches
/// cache lines in 128-byte-aligned pairs, so two indices 64 bytes apart would
/// still bounce between cores (false sharing). On aarch64 it is one line.
#[cfg(target_arch = "x86_64")]
pub const INDEX_ALIGN: usize = 128;

/// Alignment and padding of each ring index (`head`, `tail`).
#[cfg(not(target_arch = "x86_64"))]
pub const INDEX_ALIGN: usize = 64;

/// One ring slot: [`SLOT_SIZE`] bytes, aligned to a cache line.
///
/// A slot is plain bytes; every bit pattern is valid. Messages that do not fit
/// in a slot are passed by reference (offset and length into a shared buffer
/// pool), never split across slots.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C, align(64))]
pub struct Slot {
    /// The slot contents.
    pub bytes: [u8; SLOT_SIZE],
}

const _: () = {
    assert!(core::mem::size_of::<Slot>() == SLOT_SIZE);
    assert!(core::mem::align_of::<Slot>() == SLOT_SIZE);
    assert!(SLOT_SIZE % 8 == 0);
};

impl Slot {
    /// A slot with every byte set to zero.
    pub const ZERO: Slot = Slot {
        bytes: [0; SLOT_SIZE],
    };

    /// Creates a slot from its bytes.
    #[inline]
    pub const fn new(bytes: [u8; SLOT_SIZE]) -> Self {
        Self { bytes }
    }

    /// Creates a zero-padded slot holding `value` at offset 0.
    ///
    /// Fails to compile if `T` is larger or more aligned than a slot.
    #[inline]
    pub fn from_pod<T: Pod>(value: T) -> Self {
        const { assert_fits::<T>() };
        let mut slot = Self::ZERO;
        // SAFETY: `T` fits in the slot and is no more aligned than it (checked
        // at compile time above); the slot is valid for writes.
        unsafe { slot.bytes.as_mut_ptr().cast::<T>().write(value) };
        slot
    }

    /// Reads a `T` from offset 0 of the slot.
    ///
    /// Fails to compile if `T` is larger or more aligned than a slot. Any bit
    /// pattern is a valid `T` because `T: Pod`.
    #[inline]
    pub fn to_pod<T: Pod>(&self) -> T {
        const { assert_fits::<T>() };
        // SAFETY: size and alignment checked at compile time; `T: Pod` makes
        // every initialized bit pattern a valid `T`.
        unsafe { self.bytes.as_ptr().cast::<T>().read() }
    }
}

impl Default for Slot {
    fn default() -> Self {
        Self::ZERO
    }
}

impl fmt::Debug for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Slot(")?;
        for b in &self.bytes {
            write!(f, "{b:02x}")?;
        }
        f.write_str(")")
    }
}

/// Compile-time check that `T` fits in a slot.
pub(crate) const fn assert_fits<T>() {
    assert!(
        core::mem::size_of::<T>() <= SLOT_SIZE,
        "type is larger than a ring slot"
    );
    assert!(
        core::mem::align_of::<T>() <= SLOT_SIZE,
        "type is more aligned than a ring slot"
    );
}

/// Copies a slot out of memory the peer may write concurrently.
///
/// Used by untrusted handles. Volatile word loads keep the compiler from
/// re-reading shared memory after validation (no double fetch) and from
/// assuming the contents are stable. The copy may be torn if a misbehaving
/// peer writes concurrently; callers validate the local copy.
///
/// # Safety
/// `src` must be valid for reads of a `Slot` and aligned.
#[inline]
pub(crate) unsafe fn read_volatile(src: *const Slot) -> Slot {
    let mut out = Slot::ZERO;
    let words = src.cast::<u64>();
    let dst = out.bytes.as_mut_ptr().cast::<u64>();
    for i in 0..SLOT_SIZE / 8 {
        // SAFETY: `src` is valid and 64-byte aligned (caller contract), so each
        // 8-byte word inside it is valid and aligned; `dst` is a local slot.
        unsafe { dst.add(i).write(words.add(i).read_volatile()) };
    }
    out
}

/// Copies a slot into memory the peer may read concurrently.
///
/// # Safety
/// `dst` must be valid for writes of a `Slot` and aligned.
#[inline]
pub(crate) unsafe fn write_volatile(dst: *mut Slot, src: &Slot) {
    let words = dst.cast::<u64>();
    let from = src.bytes.as_ptr().cast::<u64>();
    for i in 0..SLOT_SIZE / 8 {
        // SAFETY: `dst` is valid and aligned (caller contract); `src` is a
        // 64-byte aligned slot, so every word read is aligned and in bounds.
        unsafe { words.add(i).write_volatile(from.add(i).read()) };
    }
}
