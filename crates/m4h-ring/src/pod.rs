//! The [`Pod`] marker trait.

/// Types that can cross address spaces and partitions through a ring.
///
/// A `Pod` value is copied byte for byte into a slot and read back, possibly by
/// another address space or by an untrusted peer, so the type must be
/// meaningful as raw bytes. `T: Copy` alone is not enough: it admits
/// references and raw pointers, which mean nothing in another address space.
///
/// # Safety
///
/// Implementing `Pod` for `T` asserts that:
///
/// 1. **no pointers**: `T` contains no references, raw pointers, function
///    pointers or other address-space-dependent values;
/// 2. **no padding**: every byte of `T` is part of a field (`#[repr(C)]` or
///    `#[repr(transparent)]` with fields laid out without gaps), so copying a
///    `T` never copies uninitialized bytes;
/// 3. **every bit pattern is valid**: any sequence of `size_of::<T>()` bytes is
///    a valid `T`. This excludes `bool`, `char`, enums, `NonZero*` and
///    references;
/// 4. **fits a slot**: `align_of::<T>() <= 64` (checked again at compile time
///    by the typed ring, together with `size_of::<T>() <= SLOT_SIZE`).
///
/// ```
/// use m4h_ring::Pod;
///
/// #[derive(Clone, Copy)]
/// #[repr(C)]
/// struct Packet {
///     conn: u64,
///     offset: u32,
///     len: u32,
/// }
///
/// // SAFETY: repr(C), integer fields only, no padding (8 + 4 + 4 bytes).
/// unsafe impl Pod for Packet {}
/// ```
pub unsafe trait Pod: Copy + 'static {}

macro_rules! impl_pod {
    ($($t:ty),* $(,)?) => {
        // SAFETY: primitive integers and floats have no padding, no pointers,
        // and every bit pattern is a valid value.
        $(unsafe impl Pod for $t {})*
    };
}

impl_pod!(
    u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64
);

// SAFETY: an array of `Pod` has no padding between elements (size is a multiple
// of alignment), no pointers, and every bit pattern is valid element-wise.
unsafe impl<T: Pod, const N: usize> Pod for [T; N] {}

// SAFETY: a slot is 64 plain bytes, aligned to 64, with no padding.
unsafe impl Pod for crate::Slot {}
