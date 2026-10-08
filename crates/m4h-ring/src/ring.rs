//! The raw ring: layout, initialization, attachment and the two handles.

use crate::error::{AttachError, Corrupted, PopError, PushError};
use crate::mode::{Mode, Trusted};
use crate::slot::{self, SLOT_SIZE, Slot};
use crate::sync::{AtomicU32, Cell, Ordering};
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ptr::{NonNull, addr_of_mut};

/// Magic word of a published ring header: `"M4HR"` in little-endian order.
pub const MAGIC: u32 = u32::from_le_bytes(*b"M4HR");

/// Version of the in-memory ring format.
pub const FORMAT_VERSION: u32 = 1;

/// Ordering of the producer's store that publishes new slots.
///
/// `Release` in every real build. The `m4h_ring_broken_release` cfg weakens it
/// to `Relaxed` so that a loom test can prove the model checker catches the
/// resulting data race; it must never be enabled outside that test.
#[cfg(not(m4h_ring_broken_release))]
const PUBLISH: Ordering = Ordering::Release;
#[cfg(m4h_ring_broken_release)]
const PUBLISH: Ordering = Ordering::Relaxed;

/// Ring header: one cache line, written once at initialization.
///
/// `magic` is stored last with `Release`; attachers load it with `Acquire`, so
/// every other header field written before it is visible to them. After
/// publication the header is read-only, so sharing its line is free.
#[repr(C, align(64))]
struct Header {
    magic: AtomicU32,
    version: AtomicU32,
    capacity: AtomicU32,
    slot_size: AtomicU32,
    /// Incremented by every re-initialization; lets a peer that survived a
    /// partition restart detect that the ring it held was reset.
    generation: AtomicU32,
}

/// One ring index on its own (pair of) cache line(s). See [`INDEX_ALIGN`].
///
/// [`INDEX_ALIGN`]: crate::INDEX_ALIGN
#[cfg_attr(target_arch = "x86_64", repr(C, align(128)))]
#[cfg_attr(not(target_arch = "x86_64"), repr(C, align(64)))]
struct IndexLine {
    value: AtomicU32,
}

/// A single-producer, single-consumer ring of `N` [`Slot`]s.
///
/// `N` must be a power of two between 1 and 2^31; this is checked at compile
/// time. The layout is `#[repr(C)]` and position-independent (no pointers
/// inside), so the same ring can be mapped at different virtual addresses by
/// the producer and the consumer:
///
/// ```text
/// offset 0          header   (64 B: magic, version, capacity, slot size, generation)
/// INDEX_ALIGN       tail     (written only by the producer)
/// 2 * INDEX_ALIGN   head     (written only by the consumer)
/// 3 * INDEX_ALIGN   slots    (N * 64 B)
/// ```
///
/// Indices are free-running `u32` counters that wrap around; the slot of index
/// `i` is `i & (N - 1)`, and `tail - head` (wrapping) is the number of filled
/// slots. See the [crate documentation](crate) for the memory-ordering
/// argument.
#[repr(C)]
pub struct Ring<const N: usize> {
    header: Header,
    tail: IndexLine,
    head: IndexLine,
    slots: [Cell<Slot>; N],
}

// SAFETY: all shared state is atomic or accessed through `Cell` under the
// SPSC protocol: a slot is written only by the producer while it owns it and
// read only by the consumer after the producer published it (and vice versa).
// The handles, not the ring, enforce single producer and single consumer.
unsafe impl<const N: usize> Sync for Ring<N> {}
// SAFETY: the ring owns plain data and atomics; moving it moves no references.
unsafe impl<const N: usize> Send for Ring<N> {}

impl<const N: usize> Ring<N> {
    const CHECK: () = {
        assert!(N.is_power_of_two(), "ring capacity must be a power of two");
        assert!(N <= 1 << 31, "ring capacity must be at most 2^31");
    };
    const CAP: u32 = N as u32;
    const MASK: u32 = (N - 1) as u32;

    /// Number of slots.
    pub const CAPACITY: usize = N;

    /// Initializes a ring in caller-provided memory and returns it.
    ///
    /// Use this for rings in private memory (a `static`, a `Box`, a stack
    /// frame). The ring is published with generation `generation`.
    pub fn new_in(place: &mut MaybeUninit<Self>, generation: u32) -> &mut Self {
        let () = Self::CHECK;
        let ring = place.as_mut_ptr();
        // SAFETY: `ring` points to memory we exclusively own (`&mut`), valid
        // and aligned for `Self`. We initialize every field before creating a
        // reference to the whole ring.
        unsafe {
            addr_of_mut!((*ring).header).write(Header {
                magic: AtomicU32::new(0),
                version: AtomicU32::new(FORMAT_VERSION),
                capacity: AtomicU32::new(Self::CAP),
                slot_size: AtomicU32::new(SLOT_SIZE as u32),
                generation: AtomicU32::new(generation),
            });
            addr_of_mut!((*ring).tail).write(IndexLine {
                value: AtomicU32::new(0),
            });
            addr_of_mut!((*ring).head).write(IndexLine {
                value: AtomicU32::new(0),
            });
            let slots = addr_of_mut!((*ring).slots).cast::<Cell<Slot>>();
            for i in 0..N {
                slots.add(i).write(Cell::new(Slot::ZERO));
            }
        }
        // SAFETY: every field is initialized above.
        let ring = unsafe { place.assume_init_mut() };
        ring.header.magic.store(MAGIC, Ordering::Release);
        ring
    }

    /// Initializes a ring in a shared-memory region and publishes it.
    ///
    /// All header fields and both indices are written with atomic stores, and
    /// the magic word is stored last with `Release`: a peer may poll
    /// [`Ring::attach_producer`] / [`Ring::attach_consumer`] on the region
    /// concurrently and will attach only once the ring is complete.
    ///
    /// # Safety
    ///
    /// - `ring` is valid for reads and writes of `Self`, aligned, and stays
    ///   mapped while any handle uses it;
    /// - the region is either all zero bytes (fresh anonymous or shared
    ///   mapping) or already holds a `Ring<N>`;
    /// - no handle is attached to the ring while it is (re)initialized.
    #[cfg(not(loom))]
    pub unsafe fn init_shared(ring: NonNull<Self>, generation: u32) {
        let () = Self::CHECK;
        // SAFETY: all-zero bytes are a valid `Ring<N>` (atomics and plain
        // bytes), as is an existing ring; the caller guarantees validity.
        let r = unsafe { ring.as_ref() };
        let h = &r.header;
        h.magic.store(0, Ordering::Relaxed);
        h.version.store(FORMAT_VERSION, Ordering::Relaxed);
        h.capacity.store(Self::CAP, Ordering::Relaxed);
        h.slot_size.store(SLOT_SIZE as u32, Ordering::Relaxed);
        h.generation.store(generation, Ordering::Relaxed);
        r.tail.value.store(0, Ordering::Relaxed);
        r.head.value.store(0, Ordering::Relaxed);
        // Release: everything above happens-before any attacher that reads
        // the magic word with Acquire.
        h.magic.store(MAGIC, Ordering::Release);
    }

    /// Re-initializes a published ring: empties it and increments its
    /// generation. Returns the new generation.
    ///
    /// Only the generation bump is defined here; the protocol for resetting a
    /// ring while the surviving peer is alive is future work.
    ///
    /// # Safety
    ///
    /// Same contract as [`Ring::init_shared`].
    #[cfg(not(loom))]
    pub unsafe fn reinitialize(ring: NonNull<Self>) -> u32 {
        // SAFETY: caller contract.
        let next = unsafe { ring.as_ref() }
            .header
            .generation
            .load(Ordering::Relaxed)
            .wrapping_add(1);
        // SAFETY: caller contract, forwarded.
        unsafe { Self::init_shared(ring, next) };
        next
    }

    /// Generation of the ring, as stored in its header.
    pub fn generation(&self) -> u32 {
        self.header.generation.load(Ordering::Acquire)
    }

    /// Splits a ring in private memory into its two trusted handles.
    ///
    /// The `&mut` borrow guarantees that no other handle exists.
    pub fn split(&mut self) -> (Producer<'_, N, Trusted>, Consumer<'_, N, Trusted>) {
        let () = Self::CHECK;
        // Relaxed: `&mut self` gives exclusive access; nothing to synchronize.
        let tail = self.tail.value.load(Ordering::Relaxed);
        let head = self.head.value.load(Ordering::Relaxed);
        let ring = NonNull::from(&*self);
        (
            Producer {
                ring,
                tail,
                cached_head: head,
                reserved: 0,
                _marker: PhantomData,
            },
            Consumer {
                ring,
                head,
                cached_tail: tail,
                peeked: 0,
                _marker: PhantomData,
            },
        )
    }

    /// Validates the header and returns the generation and current indices.
    fn validate(&self, expected_generation: Option<u32>) -> Result<(u32, u32, u32), AttachError> {
        let () = Self::CHECK;
        // Acquire: pairs with the Release store of the magic word at
        // initialization; the loads below then see the initialized values.
        let magic = self.header.magic.load(Ordering::Acquire);
        if magic != MAGIC {
            return Err(AttachError::BadMagic { found: magic });
        }
        let version = self.header.version.load(Ordering::Relaxed);
        if version != FORMAT_VERSION {
            return Err(AttachError::VersionMismatch { found: version });
        }
        let capacity = self.header.capacity.load(Ordering::Relaxed);
        if capacity != Self::CAP {
            return Err(AttachError::CapacityMismatch {
                expected: Self::CAP,
                found: capacity,
            });
        }
        let slot_size = self.header.slot_size.load(Ordering::Relaxed);
        if slot_size != SLOT_SIZE as u32 {
            return Err(AttachError::SlotSizeMismatch {
                expected: SLOT_SIZE as u32,
                found: slot_size,
            });
        }
        let generation = self.header.generation.load(Ordering::Relaxed);
        if let Some(expected) = expected_generation {
            if expected != generation {
                return Err(AttachError::GenerationMismatch {
                    expected,
                    found: generation,
                });
            }
        }
        // Acquire on both indices: a handle that re-attaches after a previous
        // handle of the same side must see everything that handle published.
        let tail = self.tail.value.load(Ordering::Acquire);
        let head = self.head.value.load(Ordering::Acquire);
        if tail.wrapping_sub(head) > Self::CAP {
            return Err(AttachError::Corrupted);
        }
        Ok((generation, tail, head))
    }

    /// Attaches the producer side to a ring published in shared memory.
    ///
    /// Checks magic, format version, capacity, slot size and, if
    /// `expected_generation` is `Some`, the generation.
    ///
    /// # Safety
    ///
    /// - `ring` points to a mapping valid for reads and writes of `Self`,
    ///   aligned, for the whole lifetime `'r`;
    /// - at most one producer handle is attached to the ring at a time;
    /// - with [`Trusted`], the peer follows the protocol. With
    ///   [`Untrusted`](crate::Untrusted) the peer may do anything to the
    ///   mapping without making this handle unsound.
    pub unsafe fn attach_producer<'r, M: Mode>(
        ring: NonNull<Self>,
        expected_generation: Option<u32>,
    ) -> Result<Producer<'r, N, M>, AttachError> {
        // SAFETY: caller contract.
        let (_, tail, head) = unsafe { ring.as_ref() }.validate(expected_generation)?;
        Ok(Producer {
            ring,
            tail,
            cached_head: head,
            reserved: 0,
            _marker: PhantomData,
        })
    }

    /// Attaches the consumer side to a ring published in shared memory.
    ///
    /// # Safety
    ///
    /// Same contract as [`Ring::attach_producer`], for the consumer side.
    pub unsafe fn attach_consumer<'r, M: Mode>(
        ring: NonNull<Self>,
        expected_generation: Option<u32>,
    ) -> Result<Consumer<'r, N, M>, AttachError> {
        // SAFETY: caller contract.
        let (_, tail, head) = unsafe { ring.as_ref() }.validate(expected_generation)?;
        Ok(Consumer {
            ring,
            head,
            cached_tail: tail,
            peeked: 0,
            _marker: PhantomData,
        })
    }

    #[inline(always)]
    fn cell(&self, index: u32) -> &Cell<Slot> {
        // The mask keeps the access in bounds whatever the index, including
        // indices published by an untrusted peer.
        &self.slots[(index & Self::MASK) as usize]
    }

    /// Raw pointer to the slot of index `index`, with provenance over the
    /// whole slot array, for the in-place slice APIs.
    ///
    /// The pointer must be derived from the array, not from a reference to a
    /// single cell: a slice built from `&slots[i]` would only be allowed to
    /// touch that one slot (Stacked Borrows), even though the memory is there.
    #[cfg(not(loom))]
    #[inline(always)]
    fn slot_ptr(&self, index: u32) -> *mut Slot {
        let base = self.slots.as_ptr().cast::<Slot>().cast_mut();
        // SAFETY: the masked index is in bounds of the array. `Cell<Slot>` is
        // `repr(transparent)` over `UnsafeCell<Slot>`, so the cast is
        // layout-correct and writes through it are allowed.
        unsafe { base.add((index & Self::MASK) as usize) }
    }

    /// Test hook: overwrites both published indices.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn set_indices(&self, tail: u32, head: u32) {
        self.tail.value.store(tail, Ordering::Relaxed);
        self.head.value.store(head, Ordering::Relaxed);
    }

    /// Test hook: overwrites the format version.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn set_version(&self, version: u32) {
        self.header.version.store(version, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Producer
// ---------------------------------------------------------------------------

/// The producer handle of a ring.
///
/// Only one producer exists per ring. It keeps a private copy of the tail (it
/// is the only writer) and a cached copy of the consumer's head, re-read only
/// when the cached value says the ring is full: in steady state the producer
/// never touches the consumer's cache line.
pub struct Producer<'r, const N: usize, M: Mode = Trusted> {
    ring: NonNull<Ring<N>>,
    tail: u32,
    cached_head: u32,
    /// Length of the last in-place reservation (in-place API, not under loom).
    #[cfg_attr(loom, allow(dead_code))]
    reserved: u32,
    _marker: PhantomData<(&'r Ring<N>, M)>,
}

// SAFETY: the handle is the unique producer of the ring; moving it to another
// thread moves that role. The ring itself is `Sync`.
unsafe impl<const N: usize, M: Mode> Send for Producer<'_, N, M> {}

impl<'r, const N: usize, M: Mode> Producer<'r, N, M> {
    #[inline(always)]
    fn ring(&self) -> &Ring<N> {
        // SAFETY: the ring outlives `'r` (split borrow or attach contract).
        unsafe { self.ring.as_ref() }
    }

    /// Number of slots.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Returns how many of `want` slots are free, re-reading the consumer's
    /// head only if the cached value does not show enough free space.
    #[inline]
    fn free(&mut self, want: u32) -> Result<u32, Corrupted> {
        let want = want.min(Ring::<N>::CAP);
        let mut free = Ring::<N>::CAP - self.tail.wrapping_sub(self.cached_head);
        if free < want {
            // Acquire: pairs with the consumer's Release store of `head`. The
            // consumer's reads of the slots it released happen-before our
            // upcoming writes to them (no write-after-read race).
            let head = self.ring().head.value.load(Ordering::Acquire);
            let used = self.tail.wrapping_sub(head);
            if used > Ring::<N>::CAP {
                return Err(Corrupted);
            }
            self.cached_head = head;
            free = Ring::<N>::CAP - used;
        }
        Ok(free.min(want))
    }

    /// Makes `n` written slots visible to the consumer.
    #[inline]
    fn publish(&mut self, n: u32) {
        self.tail = self.tail.wrapping_add(n);
        // Release: every slot write above happens-before the consumer's
        // Acquire load that observes this tail.
        self.ring().tail.value.store(self.tail, PUBLISH);
    }

    #[inline(always)]
    fn write_slot(&self, index: u32, src: &Slot) {
        self.ring().cell(index).with_mut(|p| {
            if M::UNTRUSTED {
                // SAFETY: `p` points to a slot of the ring, valid and aligned.
                unsafe { slot::write_volatile(p, src) }
            } else {
                // SAFETY: the slot is free (between head and tail + free) so
                // the consumer does not access it until we publish it.
                unsafe { p.write(*src) }
            }
        });
    }

    /// Writes into the next free slot through `f` and publishes it.
    ///
    /// Used by the typed ring.
    #[inline]
    pub(crate) fn push_with(&mut self, f: impl FnOnce(*mut Slot)) -> Result<(), PushError> {
        if self.free(1)? == 0 {
            return Err(PushError::Full);
        }
        self.ring().cell(self.tail).with_mut(f);
        self.publish(1);
        Ok(())
    }

    /// Writes up to `max` slots through `f(i, slot)` and publishes them with a
    /// single store. `f` returns `false` to stop early. Used by the typed ring.
    #[inline]
    pub(crate) fn push_many_with(
        &mut self,
        max: usize,
        mut f: impl FnMut(*mut Slot) -> bool,
    ) -> Result<usize, Corrupted> {
        let free = self.free(u32::try_from(max).unwrap_or(u32::MAX))?;
        let mut n = 0;
        while n < free {
            let index = self.tail.wrapping_add(n);
            if !self.ring().cell(index).with_mut(&mut f) {
                break;
            }
            n += 1;
        }
        if n > 0 {
            self.publish(n);
        }
        Ok(n as usize)
    }

    /// Pushes one slot. Fails with [`PushError::Full`] if the ring is full.
    #[inline]
    pub fn try_push(&mut self, slot: &Slot) -> Result<(), PushError> {
        if self.free(1)? == 0 {
            return Err(PushError::Full);
        }
        self.write_slot(self.tail, slot);
        self.publish(1);
        Ok(())
    }

    /// Copies as many slots of `src` as fit into the ring, in order, and
    /// publishes them with a single store. Returns how many were pushed
    /// (`0` if the ring is full).
    #[inline]
    pub fn push_slice(&mut self, src: &[Slot]) -> Result<usize, Corrupted> {
        let n = self.free(u32::try_from(src.len()).unwrap_or(u32::MAX))?;
        for (i, s) in src[..n as usize].iter().enumerate() {
            self.write_slot(self.tail.wrapping_add(i as u32), s);
        }
        if n > 0 {
            self.publish(n);
        }
        Ok(n as usize)
    }
}

#[cfg(not(loom))]
impl<const N: usize> Producer<'_, N, Trusted> {
    /// Reserves up to `n` free slots for writing in place.
    ///
    /// Returns only the contiguous part, up to the point where the ring wraps
    /// around: the slice may be shorter than `n` even if more slots are free.
    /// Write the slots, then call [`Producer::commit`]; to fill the rest, call
    /// `reserve` again. An empty slice means the ring is full.
    ///
    /// The slots hold whatever was last written to them.
    ///
    /// Only trusted handles can write in place: see [`Untrusted`](crate::Untrusted).
    #[inline]
    pub fn reserve(&mut self, n: usize) -> Result<&mut [Slot], Corrupted> {
        let free = self.free(u32::try_from(n).unwrap_or(u32::MAX))?;
        let start = self.tail & Ring::<N>::MASK;
        let len = free.min(Ring::<N>::CAP - start);
        self.reserved = len;
        let first = self.ring().slot_ptr(start);
        // SAFETY: slots `start..start + len` are within the array (no wrap),
        // are free (the consumer does not access them until we publish), and
        // `Cell<Slot>` is `repr(transparent)` over `UnsafeCell<Slot>`, so the
        // cells are laid out as `[Slot]`. The `&mut self` borrow prevents a
        // second overlapping reservation.
        Ok(unsafe { core::slice::from_raw_parts_mut(first, len as usize) })
    }

    /// Publishes the first `n` slots of the last reservation.
    ///
    /// # Panics
    ///
    /// If `n` exceeds the length of the last reservation.
    #[inline]
    pub fn commit(&mut self, n: usize) {
        assert!(
            n <= self.reserved as usize,
            "commit({n}) exceeds the reservation of {} slots",
            self.reserved
        );
        self.reserved = 0;
        if n > 0 {
            self.publish(n as u32);
        }
    }
}

// ---------------------------------------------------------------------------
// Consumer
// ---------------------------------------------------------------------------

/// The consumer handle of a ring.
///
/// Mirror image of [`Producer`]: a private copy of the head, a cached copy of
/// the producer's tail re-read only when the cached value says the ring is
/// empty.
pub struct Consumer<'r, const N: usize, M: Mode = Trusted> {
    ring: NonNull<Ring<N>>,
    head: u32,
    cached_tail: u32,
    /// Length of the last in-place peek (in-place API, not under loom).
    #[cfg_attr(loom, allow(dead_code))]
    peeked: u32,
    _marker: PhantomData<(&'r Ring<N>, M)>,
}

// SAFETY: the handle is the unique consumer of the ring; see `Producer`.
unsafe impl<const N: usize, M: Mode> Send for Consumer<'_, N, M> {}

impl<'r, const N: usize, M: Mode> Consumer<'r, N, M> {
    #[inline(always)]
    fn ring(&self) -> &Ring<N> {
        // SAFETY: the ring outlives `'r` (split borrow or attach contract).
        unsafe { self.ring.as_ref() }
    }

    /// Number of slots.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Returns how many of `want` slots are filled, re-reading the producer's
    /// tail only if the cached value does not show enough.
    #[inline]
    fn filled(&mut self, want: u32) -> Result<u32, Corrupted> {
        let want = want.min(Ring::<N>::CAP);
        let mut filled = self.cached_tail.wrapping_sub(self.head);
        if filled < want {
            // Acquire: pairs with the producer's Release store of `tail`; the
            // producer's slot writes happen-before our reads of those slots.
            let tail = self.ring().tail.value.load(Ordering::Acquire);
            filled = tail.wrapping_sub(self.head);
            if filled > Ring::<N>::CAP {
                return Err(Corrupted);
            }
            self.cached_tail = tail;
        }
        Ok(filled.min(want))
    }

    /// Hands `n` read slots back to the producer.
    #[inline]
    fn release_slots(&mut self, n: u32) {
        self.head = self.head.wrapping_add(n);
        // Release: our reads of the released slots happen-before the
        // producer's Acquire load that observes this head, hence before it
        // overwrites them.
        self.ring().head.value.store(self.head, Ordering::Release);
    }

    #[inline(always)]
    fn read_slot(&self, index: u32) -> Slot {
        self.ring().cell(index).with(|p| {
            if M::UNTRUSTED {
                // SAFETY: `p` points to a slot of the ring, valid and aligned.
                unsafe { slot::read_volatile(p) }
            } else {
                // SAFETY: the slot was published by the producer and is not
                // written again until we release it.
                unsafe { p.read() }
            }
        })
    }

    /// Reads the next filled slot through `f` and releases it. Used by the
    /// typed ring.
    #[inline]
    pub(crate) fn pop_with<R>(&mut self, f: impl FnOnce(*const Slot) -> R) -> Result<R, PopError> {
        if self.filled(1)? == 0 {
            return Err(PopError::Empty);
        }
        let out = self.ring().cell(self.head).with(f);
        self.release_slots(1);
        Ok(out)
    }

    /// Reads up to `max` slots through `f` and releases them with a single
    /// store. Used by the typed ring.
    #[inline]
    pub(crate) fn pop_many_with(
        &mut self,
        max: usize,
        mut f: impl FnMut(usize, *const Slot),
    ) -> Result<usize, Corrupted> {
        let n = self.filled(u32::try_from(max).unwrap_or(u32::MAX))?;
        for i in 0..n {
            let index = self.head.wrapping_add(i);
            self.ring().cell(index).with(|p| f(i as usize, p));
        }
        if n > 0 {
            self.release_slots(n);
        }
        Ok(n as usize)
    }

    /// Pops one slot (by copy). Fails with [`PopError::Empty`] if the ring is
    /// empty.
    #[inline]
    pub fn try_pop(&mut self) -> Result<Slot, PopError> {
        if self.filled(1)? == 0 {
            return Err(PopError::Empty);
        }
        let slot = self.read_slot(self.head);
        self.release_slots(1);
        Ok(slot)
    }

    /// Copies up to `dst.len()` slots out of the ring, in order, and releases
    /// them with a single store. Returns how many were popped (`0` if the ring
    /// is empty).
    ///
    /// With an [`Untrusted`](crate::Untrusted) peer this is the way to read:
    /// validate the copies in `dst`, never the shared memory.
    #[inline]
    pub fn pop_into(&mut self, dst: &mut [Slot]) -> Result<usize, Corrupted> {
        let n = self.filled(u32::try_from(dst.len()).unwrap_or(u32::MAX))?;
        for (i, d) in dst[..n as usize].iter_mut().enumerate() {
            *d = self.read_slot(self.head.wrapping_add(i as u32));
        }
        if n > 0 {
            self.release_slots(n);
        }
        Ok(n as usize)
    }
}

#[cfg(not(loom))]
impl<const N: usize> Consumer<'_, N, Trusted> {
    /// Returns up to `n` filled slots to read in place.
    ///
    /// Returns only the contiguous part, up to the point where the ring wraps
    /// around; call [`Consumer::release`] and `peek` again for the rest. An
    /// empty slice means the ring is empty.
    ///
    /// Only trusted handles can read in place. With an untrusted peer the
    /// slots could change underneath the reader: holding a Rust reference to
    /// them would be undefined behaviour, and validating them in place would
    /// be a time-of-check/time-of-use bug. Untrusted handles copy instead
    /// ([`Consumer::pop_into`]); validate the copy.
    #[inline]
    pub fn peek(&mut self, n: usize) -> Result<&[Slot], Corrupted> {
        let filled = self.filled(u32::try_from(n).unwrap_or(u32::MAX))?;
        let start = self.head & Ring::<N>::MASK;
        let len = filled.min(Ring::<N>::CAP - start);
        self.peeked = len;
        let first = self.ring().slot_ptr(start).cast_const();
        // SAFETY: slots `start..start + len` are in bounds (no wrap), were
        // published by the trusted producer and are not written until we
        // release them; the layout argument is the same as in `reserve`.
        Ok(unsafe { core::slice::from_raw_parts(first, len as usize) })
    }

    /// Releases the first `n` slots of the last [`Consumer::peek`].
    ///
    /// # Panics
    ///
    /// If `n` exceeds the length of the last peek.
    #[inline]
    pub fn release(&mut self, n: usize) {
        assert!(
            n <= self.peeked as usize,
            "release({n}) exceeds the last peek of {} slots",
            self.peeked
        );
        self.peeked = 0;
        if n > 0 {
            self.release_slots(n as u32);
        }
    }
}

#[cfg(all(test, not(loom)))]
mod layout {
    use super::*;
    use crate::INDEX_ALIGN;
    use core::mem::{offset_of, size_of};

    #[test]
    fn layout_is_fixed() {
        assert_eq!(size_of::<Header>(), 64);
        assert_eq!(offset_of!(Ring<4>, header), 0);
        assert_eq!(offset_of!(Ring<4>, tail), INDEX_ALIGN);
        assert_eq!(offset_of!(Ring<4>, head), 2 * INDEX_ALIGN);
        assert_eq!(offset_of!(Ring<4>, slots), 3 * INDEX_ALIGN);
        assert_eq!(size_of::<Ring<4>>(), 3 * INDEX_ALIGN + 4 * SLOT_SIZE);
        assert_eq!(size_of::<Cell<Slot>>(), SLOT_SIZE);
    }
}
