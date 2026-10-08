//! Unit tests. They also run under Miri (`cargo +nightly miri test -p m4h-ring`).

use crate::*;
use core::mem::MaybeUninit;
use core::ptr::NonNull;
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::boxed::Box;
use std::vec::Vec;

/// Number of messages for the threaded tests: small under Miri, which is slow.
const MESSAGES: u64 = if cfg!(miri) { 300 } else { 1_000_000 };

/// A zeroed, correctly aligned heap region holding a `T`, standing in for a
/// shared-memory mapping.
struct Zeroed<T> {
    ptr: NonNull<T>,
}

impl<T> Zeroed<T> {
    fn new() -> Self {
        let layout = Layout::new::<T>();
        // SAFETY: `T` is not zero-sized in these tests.
        let ptr = unsafe { alloc_zeroed(layout) }.cast::<T>();
        Self {
            ptr: NonNull::new(ptr).expect("allocation failed"),
        }
    }
}

impl<T> Drop for Zeroed<T> {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with the same layout.
        unsafe { dealloc(self.ptr.as_ptr().cast(), Layout::new::<T>()) }
    }
}

fn slot(v: u64) -> Slot {
    Slot::from_pod(v)
}

fn value(s: &Slot) -> u64 {
    s.to_pod::<u64>()
}

fn private<const N: usize>() -> Box<MaybeUninit<Ring<N>>> {
    Box::new(MaybeUninit::uninit())
}

#[test]
fn push_pop_in_order() {
    let mut place = private::<4>();
    let ring = Ring::new_in(&mut place, 0);
    let (mut tx, mut rx) = ring.split();
    assert_eq!(rx.try_pop(), Err(PopError::Empty));
    for i in 0..4 {
        tx.try_push(&slot(i)).unwrap();
    }
    assert_eq!(tx.try_push(&slot(99)), Err(PushError::Full));
    for i in 0..4 {
        assert_eq!(value(&rx.try_pop().unwrap()), i);
    }
    assert_eq!(rx.try_pop(), Err(PopError::Empty));
}

#[test]
fn capacity_one() {
    let mut place = private::<1>();
    let ring = Ring::new_in(&mut place, 0);
    let (mut tx, mut rx) = ring.split();
    for i in 0..10 {
        tx.try_push(&slot(i)).unwrap();
        assert_eq!(tx.try_push(&slot(i)), Err(PushError::Full));
        assert_eq!(value(&rx.try_pop().unwrap()), i);
    }
}

#[test]
fn indices_wrap_around_u32() {
    let region = Zeroed::<Ring<4>>::new();
    // SAFETY: zeroed region, no handles yet.
    unsafe { Ring::init_shared(region.ptr, 7) };
    let start = u32::MAX - 5;
    // SAFETY: the region is valid; no handles attached.
    unsafe { region.ptr.as_ref() }.set_indices(start, start);
    // SAFETY: valid mapping for the test's duration, one handle per side.
    let mut tx = unsafe { Ring::attach_producer::<Trusted>(region.ptr, Some(7)) }.unwrap();
    // SAFETY: as above.
    let mut rx = unsafe { Ring::attach_consumer::<Trusted>(region.ptr, Some(7)) }.unwrap();
    for i in 0..100u64 {
        tx.try_push(&slot(i)).unwrap();
        if i % 3 == 0 {
            tx.try_push(&slot(1000 + i)).unwrap();
            assert_eq!(value(&rx.try_pop().unwrap()), i);
            assert_eq!(value(&rx.try_pop().unwrap()), 1000 + i);
        } else {
            assert_eq!(value(&rx.try_pop().unwrap()), i);
        }
    }
}

#[test]
fn push_slice_and_pop_into_cross_the_wrap() {
    let mut place = private::<8>();
    let ring = Ring::new_in(&mut place, 0);
    let (mut tx, mut rx) = ring.split();
    let mut next = 0u64;
    let mut expect = 0u64;
    let mut buf = [Slot::ZERO; 5];
    for _ in 0..50 {
        let batch: Vec<Slot> = (next..next + 5).map(slot).collect();
        let pushed = tx.push_slice(&batch).unwrap();
        next += pushed as u64;
        let popped = rx.pop_into(&mut buf[..3]).unwrap();
        for s in &buf[..popped] {
            assert_eq!(value(s), expect);
            expect += 1;
        }
    }
    let popped = rx.pop_into(&mut buf).unwrap();
    for s in &buf[..popped] {
        assert_eq!(value(s), expect);
        expect += 1;
    }
    assert!(expect > 100);
}

#[test]
fn reserve_and_peek_stop_at_the_wrap() {
    let mut place = private::<8>();
    let ring = Ring::new_in(&mut place, 0);
    let (mut tx, mut rx) = ring.split();

    // Move both indices to 6: the next reservation can only be 2 contiguous slots.
    for i in 0..6 {
        tx.try_push(&slot(i)).unwrap();
        rx.try_pop().unwrap();
    }
    let r = tx.reserve(8).unwrap();
    assert_eq!(r.len(), 2);
    r[0] = slot(6);
    r[1] = slot(7);
    tx.commit(2);
    let r = tx.reserve(8).unwrap();
    assert_eq!(r.len(), 6);
    for (i, s) in r.iter_mut().enumerate() {
        *s = slot(8 + i as u64);
    }
    tx.commit(6);
    assert_eq!(tx.reserve(1).unwrap().len(), 0);

    let p = rx.peek(8).unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(value(&p[0]), 6);
    assert_eq!(value(&p[1]), 7);
    rx.release(2);
    let p = rx.peek(8).unwrap();
    assert_eq!(p.len(), 6);
    assert_eq!(value(&p[5]), 13);
    rx.release(3);
    assert_eq!(value(&rx.try_pop().unwrap()), 11);
}

#[test]
fn partial_commit() {
    let mut place = private::<4>();
    let ring = Ring::new_in(&mut place, 0);
    let (mut tx, mut rx) = ring.split();
    let r = tx.reserve(4).unwrap();
    r[0] = slot(1);
    tx.commit(1);
    assert_eq!(value(&rx.try_pop().unwrap()), 1);
    assert_eq!(rx.try_pop(), Err(PopError::Empty));
}

#[test]
#[should_panic(expected = "exceeds the reservation")]
fn commit_beyond_reservation_panics() {
    let mut place = private::<4>();
    let ring = Ring::new_in(&mut place, 0);
    let (mut tx, _rx) = ring.split();
    let _ = tx.reserve(2).unwrap();
    tx.commit(3);
}

#[test]
#[should_panic(expected = "exceeds the last peek")]
fn release_beyond_peek_panics() {
    let mut place = private::<4>();
    let ring = Ring::new_in(&mut place, 0);
    let (mut tx, mut rx) = ring.split();
    tx.try_push(&slot(1)).unwrap();
    let _ = rx.peek(4).unwrap();
    rx.release(2);
}

#[test]
fn corrupted_indices_are_detected() {
    let region = Zeroed::<Ring<4>>::new();
    // SAFETY: zeroed region, no handles yet.
    unsafe { Ring::init_shared(region.ptr, 0) };
    // SAFETY: valid mapping, one handle per side.
    let mut tx = unsafe { Ring::attach_producer::<Untrusted>(region.ptr, None) }.unwrap();
    // SAFETY: as above.
    let mut rx = unsafe { Ring::attach_consumer::<Untrusted>(region.ptr, None) }.unwrap();
    // SAFETY: the region is valid.
    let raw = unsafe { region.ptr.as_ref() };

    // Fill the ring so the producer must re-read head on the next push.
    for i in 0..4 {
        tx.try_push(&slot(i)).unwrap();
    }
    // A malicious consumer moves head past tail.
    raw.set_indices(4, 9);
    assert_eq!(tx.try_push(&slot(0)), Err(PushError::Corrupted));
    assert_eq!(tx.push_slice(&[Slot::ZERO]), Err(Corrupted));

    // A malicious producer publishes a tail more than N ahead of head.
    raw.set_indices(100, 0);
    assert_eq!(rx.try_pop(), Err(PopError::Corrupted));
    let mut buf = [Slot::ZERO; 8];
    assert_eq!(rx.pop_into(&mut buf), Err(Corrupted));

    // A tail behind head is just as invalid.
    raw.set_indices(0, 0);
    // SAFETY: valid mapping; the previous consumer handle is no longer used.
    let mut fresh_rx = unsafe { Ring::attach_consumer::<Untrusted>(region.ptr, None) }.unwrap();
    raw.set_indices(u32::MAX, 0);
    assert_eq!(fresh_rx.try_pop(), Err(PopError::Corrupted));

    // Attaching to a ring with inconsistent indices fails too.
    raw.set_indices(10, 0);
    // SAFETY: valid mapping.
    let r = unsafe { Ring::attach_consumer::<Untrusted>(region.ptr, None) };
    assert_eq!(r.err(), Some(AttachError::Corrupted));
}

#[test]
fn attach_validates_the_header() {
    let region = Zeroed::<Ring<4>>::new();

    // Zeroed memory is not a published ring.
    // SAFETY: valid mapping.
    let r = unsafe { Ring::attach_producer::<Trusted>(region.ptr, None) };
    assert_eq!(r.err(), Some(AttachError::BadMagic { found: 0 }));

    // SAFETY: zeroed region, no handles yet.
    unsafe { Ring::init_shared(region.ptr, 3) };

    // Wrong capacity: view the same memory as a ring of 2 slots.
    let small = region.ptr.cast::<Ring<2>>();
    // SAFETY: `Ring<2>` is smaller than `Ring<4>`, same alignment.
    let r = unsafe { Ring::attach_producer::<Trusted>(small, None) };
    assert_eq!(
        r.err(),
        Some(AttachError::CapacityMismatch {
            expected: 2,
            found: 4
        })
    );

    // Wrong generation.
    // SAFETY: valid mapping.
    let r = unsafe { Ring::attach_producer::<Trusted>(region.ptr, Some(2)) };
    assert_eq!(
        r.err(),
        Some(AttachError::GenerationMismatch {
            expected: 2,
            found: 3
        })
    );

    // Wrong format version.
    // SAFETY: the region is valid.
    unsafe { region.ptr.as_ref() }.set_version(FORMAT_VERSION + 1);
    // SAFETY: valid mapping.
    let r = unsafe { Ring::attach_producer::<Trusted>(region.ptr, None) };
    assert_eq!(
        r.err(),
        Some(AttachError::VersionMismatch {
            found: FORMAT_VERSION + 1
        })
    );
}

#[test]
fn reinitialize_bumps_the_generation_and_empties_the_ring() {
    let region = Zeroed::<Ring<4>>::new();
    // SAFETY: zeroed region, no handles yet.
    unsafe { Ring::init_shared(region.ptr, u32::MAX) };
    {
        // SAFETY: valid mapping.
        let mut tx = unsafe { Ring::attach_producer::<Trusted>(region.ptr, None) }.unwrap();
        tx.try_push(&slot(1)).unwrap();
    }
    // SAFETY: no handle attached any more.
    let generation = unsafe { Ring::reinitialize(region.ptr) };
    assert_eq!(generation, 0, "generation wraps around");
    // SAFETY: the region is valid.
    assert_eq!(unsafe { region.ptr.as_ref() }.generation(), 0);
    // SAFETY: valid mapping.
    let r = unsafe { Ring::attach_consumer::<Trusted>(region.ptr, Some(u32::MAX)) };
    assert!(matches!(r, Err(AttachError::GenerationMismatch { .. })));
    // SAFETY: valid mapping.
    let mut rx = unsafe { Ring::attach_consumer::<Trusted>(region.ptr, Some(0)) }.unwrap();
    assert_eq!(rx.try_pop(), Err(PopError::Empty));
}

#[test]
fn untrusted_handles_round_trip() {
    let region = Zeroed::<Ring<8>>::new();
    // SAFETY: zeroed region, no handles yet.
    unsafe { Ring::init_shared(region.ptr, 0) };
    // SAFETY: valid mapping, one handle per side.
    let mut tx = unsafe { Ring::attach_producer::<Untrusted>(region.ptr, Some(0)) }.unwrap();
    // SAFETY: as above.
    let mut rx = unsafe { Ring::attach_consumer::<Untrusted>(region.ptr, Some(0)) }.unwrap();
    let batch: Vec<Slot> = (0..6).map(slot).collect();
    assert_eq!(tx.push_slice(&batch), Ok(6));
    let mut out = [Slot::ZERO; 4];
    assert_eq!(rx.pop_into(&mut out), Ok(4));
    assert_eq!(out.map(|s| value(&s)), [0, 1, 2, 3]);
    assert_eq!(value(&rx.try_pop().unwrap()), 4);
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
struct Packet {
    conn: u64,
    offset: u32,
    len: u32,
    tag: [u8; 16],
}

// SAFETY: repr(C), integers and a byte array, 8 + 4 + 4 + 16 = 32 bytes, no padding.
unsafe impl Pod for Packet {}

fn packet(i: u64) -> Packet {
    Packet {
        conn: i,
        offset: i as u32 * 2,
        len: 3,
        tag: [i as u8; 16],
    }
}

#[test]
fn typed_ring_in_private_memory() {
    // `&'static str` is Copy + Send but not Pod: fine within one program.
    let mut place = Box::new(MaybeUninit::<TypedRing<&'static str, 4>>::uninit());
    let ring = TypedRing::new_in(&mut place, 0);
    let (mut tx, mut rx) = ring.split();
    tx.try_push("hello").unwrap();
    tx.try_push("world").unwrap();
    assert_eq!(rx.try_pop(), Ok("hello"));
    let mut it = ["a", "b", "c", "d", "e"].into_iter();
    assert_eq!(tx.push_iter(&mut it), Ok(3));
    assert_eq!(
        it.next(),
        Some("d"),
        "items that do not fit stay in the iterator"
    );
    let mut out = [""; 8];
    assert_eq!(rx.pop_into(&mut out), Ok(4));
    assert_eq!(&out[..4], &["world", "a", "b", "c"]);
}

#[test]
fn typed_ring_across_address_spaces() {
    let region = Zeroed::<Ring<4>>::new();
    // SAFETY: zeroed region, no handles yet.
    unsafe { Ring::init_shared(region.ptr, 0) };
    // SAFETY: valid mapping, one handle per side, both sides use `Packet`.
    let mut tx =
        unsafe { TypedProducer::<Packet, 4, Untrusted>::attach(region.ptr, Some(0)) }.unwrap();
    // SAFETY: as above.
    let mut rx =
        unsafe { TypedConsumer::<Packet, 4, Untrusted>::attach(region.ptr, Some(0)) }.unwrap();
    for i in 0..10 {
        tx.try_push(packet(i)).unwrap();
        assert_eq!(rx.try_pop(), Ok(packet(i)));
    }
    let mut it = (0..6).map(packet);
    assert_eq!(tx.push_iter(&mut it), Ok(4));
    let mut out = [packet(0); 4];
    assert_eq!(rx.pop_into(&mut out), Ok(4));
    assert_eq!(out, [packet(0), packet(1), packet(2), packet(3)]);
    assert_eq!(it.next(), Some(packet(4)));
}

#[test]
fn untrusted_typed_writes_zero_padding() {
    // The untrusted producer writes the whole slot, zero-padded, so nothing
    // stale reaches the peer. Observe it through a raw consumer.
    let region = Zeroed::<Ring<2>>::new();
    // SAFETY: zeroed region, no handles yet.
    unsafe { Ring::init_shared(region.ptr, 0) };
    {
        // SAFETY: valid mapping, single producer.
        let mut raw_tx = unsafe { Ring::attach_producer::<Trusted>(region.ptr, None) }.unwrap();
        raw_tx.try_push(&Slot::new([0xAA; SLOT_SIZE])).unwrap();
    }
    // SAFETY: valid mapping, single consumer.
    let mut rx = unsafe { Ring::attach_consumer::<Trusted>(region.ptr, None) }.unwrap();
    rx.try_pop().unwrap();
    // SAFETY: the previous producer handle is gone; single producer again.
    let mut tx = unsafe { TypedProducer::<u32, 2, Untrusted>::attach(region.ptr, None) }.unwrap();
    tx.try_push(0x1122_3344).unwrap();
    tx.try_push(7).unwrap();
    let a = rx.try_pop().unwrap();
    assert_eq!(a.to_pod::<u32>(), 0x1122_3344);
    assert!(a.bytes[4..].iter().all(|&b| b == 0));
    let b = rx.try_pop().unwrap();
    assert_eq!(b.to_pod::<u32>(), 7);
    assert!(b.bytes[4..].iter().all(|&b| b == 0));
}

#[test]
fn slot_pod_round_trip() {
    let s = Slot::from_pod([1u32, 2, 3]);
    assert_eq!(s.to_pod::<[u32; 3]>(), [1, 2, 3]);
    assert!(s.bytes[12..].iter().all(|&b| b == 0));
}

#[test]
fn threads_trusted() {
    let mut place = private::<64>();
    let ring = Ring::new_in(&mut place, 0);
    let (mut tx, mut rx) = ring.split();
    std::thread::scope(|s| {
        s.spawn(move || {
            let mut i = 0;
            while i < MESSAGES {
                match tx.try_push(&slot(i)) {
                    Ok(()) => i += 1,
                    Err(PushError::Full) => std::thread::yield_now(),
                    Err(PushError::Corrupted) => panic!("corrupted"),
                }
            }
        });
        s.spawn(move || {
            let mut expect = 0;
            while expect < MESSAGES {
                match rx.try_pop() {
                    Ok(s) => {
                        assert_eq!(value(&s), expect);
                        expect += 1;
                    }
                    Err(PopError::Empty) => std::thread::yield_now(),
                    Err(PopError::Corrupted) => panic!("corrupted"),
                }
            }
        });
    });
}

#[test]
fn threads_batched_in_place() {
    let mut place = private::<64>();
    let ring = Ring::new_in(&mut place, 0);
    let (mut tx, mut rx) = ring.split();
    std::thread::scope(|s| {
        s.spawn(move || {
            let mut i = 0;
            while i < MESSAGES {
                let want = (MESSAGES - i).min(16) as usize;
                let r = tx.reserve(want).unwrap();
                let n = r.len();
                for s in r.iter_mut() {
                    *s = slot(i);
                    i += 1;
                }
                tx.commit(n);
                if n == 0 {
                    std::thread::yield_now();
                }
            }
        });
        s.spawn(move || {
            let mut expect = 0;
            while expect < MESSAGES {
                let p = rx.peek(16).unwrap();
                let n = p.len();
                for s in p {
                    assert_eq!(value(s), expect);
                    expect += 1;
                }
                rx.release(n);
                if n == 0 {
                    std::thread::yield_now();
                }
            }
        });
    });
}

#[test]
fn threads_untrusted_typed() {
    let region = Zeroed::<Ring<16>>::new();
    // SAFETY: zeroed region, no handles yet.
    unsafe { Ring::init_shared(region.ptr, 0) };
    // SAFETY: the region outlives the scope below; one handle per side.
    let mut tx =
        unsafe { TypedProducer::<Packet, 16, Untrusted>::attach(region.ptr, None) }.unwrap();
    // SAFETY: as above.
    let mut rx =
        unsafe { TypedConsumer::<Packet, 16, Untrusted>::attach(region.ptr, None) }.unwrap();
    let n = MESSAGES / 10;
    std::thread::scope(|s| {
        s.spawn(move || {
            let mut it = (0..n).map(packet).peekable();
            while it.peek().is_some() {
                if tx.push_iter(&mut it).unwrap() == 0 {
                    std::thread::yield_now();
                }
            }
        });
        s.spawn(move || {
            let mut expect = 0;
            let mut buf = [packet(0); 8];
            while expect < n {
                let got = rx.pop_into(&mut buf).unwrap();
                for p in &buf[..got] {
                    assert_eq!(*p, packet(expect));
                    expect += 1;
                }
                if got == 0 {
                    std::thread::yield_now();
                }
            }
        });
    });
}
