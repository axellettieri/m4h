//! Synchronization primitives, swapped for their `loom` models under `cfg(loom)`.
//!
//! Every atomic and every slot access in the crate goes through this module, so
//! the same code is model-checked by loom and compiled to plain loads/stores in
//! production builds.

#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicU32, Ordering};

#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{AtomicU32, Ordering};

/// Interior-mutable cell holding one slot.
///
/// In production builds it is a `#[repr(transparent)]` wrapper around
/// [`core::cell::UnsafeCell`], so `[Cell<Slot>; N]` has exactly the layout of
/// `[Slot; N]` and can be viewed as a slice of slots. Under loom it wraps
/// `loom::cell::UnsafeCell`, which records every access and reports reads and
/// writes that are not ordered by a happens-before relation.
#[cfg(not(loom))]
#[repr(transparent)]
pub(crate) struct Cell<T>(core::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> Cell<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(core::cell::UnsafeCell::new(value))
    }

    #[inline(always)]
    pub(crate) fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        f(self.0.get())
    }

    #[inline(always)]
    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }

    /// Raw pointer to the contents. Only used by the in-place (slice) APIs,
    /// which do not exist under loom.
    #[inline(always)]
    pub(crate) const fn get(&self) -> *mut T {
        self.0.get()
    }
}

#[cfg(loom)]
pub(crate) struct Cell<T>(loom::cell::UnsafeCell<T>);

#[cfg(loom)]
impl<T> Cell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(loom::cell::UnsafeCell::new(value))
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        self.0.with(f)
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        self.0.with_mut(f)
    }
}
