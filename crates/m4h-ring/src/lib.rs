//! Lock-free single-producer/single-consumer rings with cache-line-sized slots.
//!
//! This is the inter-core and inter-partition communication primitive of M4H:
//! cores never share mutable state, they exchange 64-byte messages through
//! these rings.
//!
//! # Overview
//!
//! - [`Ring<N>`]: `N` slots of [`SLOT_SIZE`] bytes (64 on x86_64 and aarch64),
//!   each aligned to a cache line; `N` is a power of two fixed at compile
//!   time. `#[repr(C)]`, no pointers inside: the same ring can be mapped at
//!   different addresses by the two sides.
//! - [`Producer`] / [`Consumer`]: the two handles. A ring has exactly one of
//!   each. Created by [`Ring::split`] (private memory) or
//!   [`Ring::attach_producer`] / [`Ring::attach_consumer`] (shared memory,
//!   with header validation).
//! - [`TypedRing`], [`TypedProducer`], [`TypedConsumer`]: one value of type
//!   `T` per slot. Across address spaces `T` must implement [`Pod`].
//! - [`Trusted`] / [`Untrusted`]: what a handle assumes about its peer.
//!
//! No allocation, no dependencies, `no_std`.
//!
//! # Protocol
//!
//! The ring has two free-running `u32` indices that only grow (wrapping at
//! 2^32): `tail`, written only by the producer, and `head`, written only by the
//! consumer. Index `i` lives in slot `i & (N - 1)`. Slots `head..tail` are
//! filled and owned by the consumer; slots `tail..head + N` are free and owned
//! by the producer. `tail - head` (wrapping) is always between `0` and `N`.
//!
//! Each index sits alone on its own cache line (two lines on x86_64, see
//! [`INDEX_ALIGN`]). Each handle keeps a private copy of its own index (it is
//! the only writer) and a cached copy of the peer's index, and re-reads the
//! peer's index only when the cached value says the ring is full (producer)
//! or empty (consumer). In steady state each side touches only its own index
//! line plus the slots.
//!
//! Batches cost one atomic store: [`Producer::push_slice`],
//! [`Producer::reserve`] + [`Producer::commit`], [`Consumer::pop_into`] and
//! [`Consumer::peek`] + [`Consumer::release`] publish or release `n` slots at
//! once. `reserve` and `peek` return only the contiguous part of the ring, up
//! to the wrap point; call them again for the rest.
//!
//! # Memory ordering
//!
//! Each index has a single writer and there are exactly two parties, so
//! pairwise acquire/release synchronization is sufficient; no global order
//! (`SeqCst`) is needed.
//!
//! | Operation | Ordering | Why |
//! |---|---|---|
//! | Producer reads its own `tail` | none (private copy) | It is the only writer, so its copy is always current. |
//! | Producer re-reads `head` (ring looks full) | `Acquire` | Pairs with the consumer's `Release` store of `head`: the consumer's *reads* of a slot happen-before the producer's *overwrite* of it. Without it the new write could be observed by the old read (write-after-read race). |
//! | Producer writes slot data | plain store | The slot is owned exclusively by the producer at that point. |
//! | Producer publishes `tail + n` | `Release` | Makes every slot write before it visible to whoever acquires this value: this is the store that publishes the data. |
//! | Consumer re-reads `tail` (ring looks empty) | `Acquire` | The other half of the previous pair: after this load the slot data is visible. |
//! | Consumer reads slot data | plain load | The slot is owned exclusively by the consumer at that point. |
//! | Consumer releases `head + n` | `Release` | Its reads of the slots complete before the producer can see them as free. |
//! | Consumer reads its own `head` | none (private copy) | Single writer. |
//! | Header `magic` at init / attach | `Release` / `Acquire` | Publishes the rest of the header and the zeroed indices to the attaching peer. |
//!
//! **Why not `SeqCst`.** `SeqCst` adds a single total order over all `SeqCst`
//! accesses. It is needed only when a thread stores to one location and then
//! loads a *different* one and the two must not be reordered (store→load, the
//! Dekker pattern). That pattern does not occur in the ring. On x86_64 (TSO)
//! acquire loads and release stores are plain `mov`s and the orderings only
//! constrain the compiler, while a `SeqCst` store is an `xchg` (or `mov` +
//! `mfence`), tens of cycles. On aarch64 acquire/release become `ldar`/`stlr`,
//! cheaper than a full `dmb`.
//!
//! **Where `SeqCst` will be needed.** Once sleeping is added (MWAIT, IPI,
//! futex), the consumer will set a "sleeping" flag and then re-check `tail`,
//! while the producer stores `tail` and then checks the flag. That is exactly
//! store→load on both sides: each needs a `fence(SeqCst)` between the store and
//! the load, or a wake-up can be lost. Sleeping is outside this crate for now:
//! every operation is non-blocking and the caller polls.
//!
//! **Verification.** The protocol is model-checked with
//! [loom](https://docs.rs/loom) (`RUSTFLAGS="--cfg loom" cargo test -p
//! m4h-ring --test loom --release`), which explores all interleavings and
//! reports any slot access not ordered by happens-before. A negative test
//! weakens the publishing store to `Relaxed` (`--cfg m4h_ring_broken_release`)
//! and expects loom to report the race. The unit tests also run under Miri.
//!
//! # Trust model
//!
//! Every index read from the peer is validated, in both modes: if the peer
//! publishes an index more than `N` away from ours, the operation fails with
//! [`Corrupted`] and touches no slot. Slot accesses are always masked, so no
//! index can reach outside the ring. The cost is one comparison per re-read.
//!
//! The modes differ in how slot contents are accessed:
//!
//! - [`Trusted`]: in place ([`Producer::reserve`], [`Consumer::peek`]) or by
//!   copy.
//! - [`Untrusted`]: only by copy, with volatile accesses. A malicious peer can
//!   change a slot at any time, so the handle never creates a Rust reference
//!   to slot memory (that would be undefined behaviour) and reads each slot
//!   exactly once. **Copy first, then validate the local copy, never the
//!   reverse.** The typed consumer always copies.
//!
//! # Example
//!
//! ```
//! use core::mem::MaybeUninit;
//! use m4h_ring::{Ring, Slot};
//!
//! let mut place = Box::new(MaybeUninit::<Ring<8>>::uninit());
//! let ring = Ring::new_in(&mut place, 0);
//! let (mut tx, mut rx) = ring.split();
//!
//! let mut msg = Slot::ZERO;
//! msg.bytes[0] = 42;
//! tx.try_push(&msg).unwrap();
//! assert_eq!(rx.try_pop().unwrap().bytes[0], 42);
//! ```
#![no_std]

#[cfg(test)]
extern crate std;

mod error;
mod mode;
mod pod;
mod ring;
mod slot;
mod sync;
mod typed;

pub use error::{AttachError, Corrupted, PopError, PushError};
pub use mode::{Mode, Trusted, Untrusted};
pub use pod::Pod;
pub use ring::{Consumer, FORMAT_VERSION, MAGIC, Producer, Ring};
pub use slot::{INDEX_ALIGN, SLOT_SIZE, Slot};
pub use typed::{TypedConsumer, TypedProducer, TypedRing};

#[cfg(all(test, not(loom)))]
mod tests;
