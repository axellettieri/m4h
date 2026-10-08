//! Typed rings: one value of type `T` per slot.
//!
//! Two ways in, with different bounds:
//!
//! - [`TypedRing::split`], for a ring in the program's own memory: `T: Copy +
//!   Send` is enough, because both sides are the same program.
//! - [`TypedProducer::attach`] / [`TypedConsumer::attach`], for a ring in
//!   shared memory that crosses address spaces or partitions: `T: Pod`
//!   ([`Pod`]), because the bytes travel to a place where pointers mean
//!   nothing and, with an untrusted peer, may come back as any bit pattern.
//!
//! Invariant used below: a handle in [`Untrusted`](crate::Untrusted) mode can
//! only be created through `attach`, so its `T` is always `Pod`.

use crate::error::{AttachError, Corrupted, PopError, PushError};
use crate::mode::{Mode, Trusted};
use crate::pod::Pod;
use crate::ring::{Consumer, Producer, Ring};
use crate::slot::{self, Slot, assert_fits};
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ptr::NonNull;

/// A ring carrying one `T` per slot, in the program's own memory.
///
/// `T` must fit in a slot (size and alignment at most [`SLOT_SIZE`]); this is
/// checked at compile time.
///
/// [`SLOT_SIZE`]: crate::SLOT_SIZE
#[repr(transparent)]
pub struct TypedRing<T, const N: usize> {
    raw: Ring<N>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Copy + Send, const N: usize> TypedRing<T, N> {
    /// Initializes a typed ring in caller-provided memory.
    pub fn new_in(place: &mut MaybeUninit<Self>, generation: u32) -> &mut Self {
        const { assert_fits::<T>() };
        // SAFETY: `TypedRing` is `repr(transparent)` over `Ring<N>` (the other
        // field is a ZST), so the two `MaybeUninit`s have the same layout.
        let raw = unsafe { &mut *place.as_mut_ptr().cast::<MaybeUninit<Ring<N>>>() };
        Ring::new_in(raw, generation);
        // SAFETY: the only non-ZST field is now initialized.
        unsafe { place.assume_init_mut() }
    }

    /// Splits the ring into its two trusted handles.
    pub fn split(
        &mut self,
    ) -> (
        TypedProducer<'_, T, N, Trusted>,
        TypedConsumer<'_, T, N, Trusted>,
    ) {
        let (p, c) = self.raw.split();
        (
            TypedProducer {
                raw: p,
                _marker: PhantomData,
            },
            TypedConsumer {
                raw: c,
                _marker: PhantomData,
            },
        )
    }

    /// The underlying raw ring.
    pub fn as_raw(&self) -> &Ring<N> {
        &self.raw
    }
}

/// Writes `value` into a slot according to the handle's mode.
///
/// # Safety
/// `p` points to a free slot owned by the producer; `T` fits in a slot; if
/// `M` is untrusted, `T: Pod` (handle invariant).
#[inline(always)]
unsafe fn write_value<T: Copy, M: Mode>(p: *mut Slot, value: T) {
    if M::UNTRUSTED {
        // Build the slot locally (zero-padded, so nothing stale leaks to the
        // peer), then copy it out with volatile stores.
        let mut local = Slot::ZERO;
        // SAFETY: `T` fits in a slot and is no more aligned than it.
        unsafe { local.bytes.as_mut_ptr().cast::<T>().write(value) };
        // SAFETY: `p` is a valid, aligned slot (caller contract). `T: Pod` has
        // no padding, so every byte of `local` is initialized.
        unsafe { slot::write_volatile(p, &local) };
    } else {
        // SAFETY: valid, aligned, exclusively owned slot (caller contract).
        unsafe { p.cast::<T>().write(value) };
    }
}

/// Reads a `T` from a slot according to the handle's mode.
///
/// # Safety
/// `p` points to a published slot owned by the consumer; `T` fits in a slot;
/// with a trusted peer the slot holds a `T` written by the producer; if `M` is
/// untrusted, `T: Pod` (handle invariant).
#[inline(always)]
unsafe fn read_value<T: Copy, M: Mode>(p: *const Slot) -> T {
    if M::UNTRUSTED {
        // Copy first, then interpret the local copy: the shared slot is read
        // exactly once.
        // SAFETY: `p` is a valid, aligned slot (caller contract).
        let local = unsafe { slot::read_volatile(p) };
        // SAFETY: `T: Pod` accepts any bit pattern; size and alignment fit.
        unsafe { local.bytes.as_ptr().cast::<T>().read() }
    } else {
        // SAFETY: the trusted producer wrote a `T` at offset 0 of this slot.
        unsafe { p.cast::<T>().read() }
    }
}

/// Producer handle of a typed ring.
pub struct TypedProducer<'r, T, const N: usize, M: Mode = Trusted> {
    raw: Producer<'r, N, M>,
    _marker: PhantomData<fn(T)>,
}

// SAFETY: values of `T` are moved to the consumer's thread, so `T: Send`; the
// raw handle is `Send`.
unsafe impl<T: Send, const N: usize, M: Mode> Send for TypedProducer<'_, T, N, M> {}

impl<'r, T: Pod, const N: usize, M: Mode> TypedProducer<'r, T, N, M> {
    /// Attaches the producer side of a typed ring in shared memory.
    ///
    /// # Safety
    ///
    /// Same contract as [`Ring::attach_producer`]. With a trusted peer, the
    /// consumer must read the slots as `T`.
    pub unsafe fn attach(
        ring: NonNull<Ring<N>>,
        expected_generation: Option<u32>,
    ) -> Result<Self, AttachError> {
        const { assert_fits::<T>() };
        // SAFETY: caller contract, forwarded.
        let raw = unsafe { Ring::attach_producer(ring, expected_generation) }?;
        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }
}

impl<'r, T: Copy, const N: usize, M: Mode> TypedProducer<'r, T, N, M> {
    /// Pushes one value. Fails with [`PushError::Full`] if the ring is full.
    #[inline]
    pub fn try_push(&mut self, value: T) -> Result<(), PushError> {
        self.raw.push_with(|p| {
            // SAFETY: `push_with` hands us the next free slot; `T` fits (checked
            // at construction); untrusted handles have `T: Pod`.
            unsafe { write_value::<T, M>(p, value) }
        })
    }

    /// Pushes values from `iter` until the ring is full or the iterator ends,
    /// and publishes them with a single store. Items that do not fit stay in
    /// the iterator. Returns how many were pushed.
    #[inline]
    pub fn push_iter<I: Iterator<Item = T>>(&mut self, iter: &mut I) -> Result<usize, Corrupted> {
        self.raw.push_many_with(usize::MAX, |p| match iter.next() {
            Some(value) => {
                // SAFETY: as in `try_push`.
                unsafe { write_value::<T, M>(p, value) };
                true
            }
            None => false,
        })
    }

    /// Number of slots.
    pub const fn capacity(&self) -> usize {
        N
    }
}

/// Consumer handle of a typed ring.
pub struct TypedConsumer<'r, T, const N: usize, M: Mode = Trusted> {
    raw: Consumer<'r, N, M>,
    _marker: PhantomData<fn() -> T>,
}

// SAFETY: see `TypedProducer`.
unsafe impl<T: Send, const N: usize, M: Mode> Send for TypedConsumer<'_, T, N, M> {}

impl<'r, T: Pod, const N: usize, M: Mode> TypedConsumer<'r, T, N, M> {
    /// Attaches the consumer side of a typed ring in shared memory.
    ///
    /// # Safety
    ///
    /// Same contract as [`Ring::attach_consumer`]. With a trusted peer, the
    /// producer must write the slots as `T`.
    pub unsafe fn attach(
        ring: NonNull<Ring<N>>,
        expected_generation: Option<u32>,
    ) -> Result<Self, AttachError> {
        const { assert_fits::<T>() };
        // SAFETY: caller contract, forwarded.
        let raw = unsafe { Ring::attach_consumer(ring, expected_generation) }?;
        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }
}

impl<'r, T: Copy, const N: usize, M: Mode> TypedConsumer<'r, T, N, M> {
    /// Pops one value (always by copy). Fails with [`PopError::Empty`] if the
    /// ring is empty.
    #[inline]
    pub fn try_pop(&mut self) -> Result<T, PopError> {
        self.raw.pop_with(|p| {
            // SAFETY: `pop_with` hands us the next published slot.
            unsafe { read_value::<T, M>(p) }
        })
    }

    /// Pops up to `dst.len()` values and releases them with a single store.
    /// Returns how many were popped.
    #[inline]
    pub fn pop_into(&mut self, dst: &mut [T]) -> Result<usize, Corrupted> {
        self.raw.pop_many_with(dst.len(), |i, p| {
            // SAFETY: `pop_many_with` hands us published slots, `i < dst.len()`.
            dst[i] = unsafe { read_value::<T, M>(p) };
        })
    }

    /// Number of slots.
    pub const fn capacity(&self) -> usize {
        N
    }
}
