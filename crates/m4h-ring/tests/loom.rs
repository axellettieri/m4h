//! Model checking of the ring protocol with loom.
//!
//! ```sh
//! RUSTFLAGS="--cfg loom" cargo test -p m4h-ring --test loom --release
//! ```
//!
//! Loom runs the closure under every interleaving of the two threads allowed
//! by the C++/Rust memory model (bounded by the preemption bound) and panics
//! if a slot is read and written without a happens-before relation between
//! the two accesses.
//!
//! The negative test needs the ring compiled with the publishing store
//! weakened to `Relaxed`:
//!
//! ```sh
//! RUSTFLAGS="--cfg loom --cfg m4h_ring_broken_release" \
//!     cargo test -p m4h-ring --test loom --release
//! ```
#![cfg(loom)]

use loom::thread;
use m4h_ring::{PopError, PushError, Ring, Slot, Trusted, TypedConsumer, TypedProducer};
use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::sync::Arc;

/// A published ring in private memory, shared by the two loom threads.
fn ring<const N: usize>() -> Arc<Ring<N>> {
    let mut place = Box::new(MaybeUninit::<Ring<N>>::uninit());
    Ring::new_in(&mut place, 0);
    // SAFETY: `new_in` initialized the ring.
    let ring: Box<Ring<N>> = unsafe { Box::from_raw(Box::into_raw(place).cast()) };
    Arc::from(ring)
}

fn model(f: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(f);
}

fn slot(v: u8) -> Slot {
    let mut s = Slot::ZERO;
    s.bytes[0] = v;
    s.bytes[63] = v;
    s
}

/// Pushes `values` one by one, spinning while the ring is full.
fn producer<const N: usize>(ring: Arc<Ring<N>>, values: Vec<u8>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // SAFETY: the Arc keeps the ring alive; single producer.
        let mut tx =
            unsafe { Ring::attach_producer::<Trusted>(NonNull::from(&*ring), Some(0)) }.unwrap();
        for v in values {
            loop {
                match tx.try_push(&slot(v)) {
                    Ok(()) => break,
                    Err(PushError::Full) => thread::yield_now(),
                    Err(PushError::Corrupted) => panic!("corrupted"),
                }
            }
        }
    })
}

/// Single slots, ring of 2, three messages: covers empty, full and wrap.
#[test]
fn single_slots() {
    model(|| {
        let ring = ring::<2>();
        let p = producer(ring.clone(), vec![1, 2, 3]);
        // SAFETY: the Arc keeps the ring alive; single consumer.
        let mut rx =
            unsafe { Ring::attach_consumer::<Trusted>(NonNull::from(&*ring), Some(0)) }.unwrap();
        for expect in 1..=3u8 {
            let got = loop {
                match rx.try_pop() {
                    Ok(s) => break s,
                    Err(PopError::Empty) => thread::yield_now(),
                    Err(PopError::Corrupted) => panic!("corrupted"),
                }
            };
            assert_eq!(got, slot(expect));
        }
        p.join().unwrap();
    });
}

/// Batches: one store publishes or releases several slots.
#[test]
fn batches() {
    model(|| {
        let ring = ring::<4>();
        let r = ring.clone();
        let p = thread::spawn(move || {
            // SAFETY: the Arc keeps the ring alive; single producer.
            let mut tx =
                unsafe { Ring::attach_producer::<Trusted>(NonNull::from(&*r), Some(0)) }.unwrap();
            let all = [slot(1), slot(2), slot(3), slot(4), slot(5)];
            let mut sent = 0;
            while sent < all.len() {
                let n = tx.push_slice(&all[sent..]).unwrap();
                if n == 0 {
                    thread::yield_now();
                }
                sent += n;
            }
        });
        // SAFETY: the Arc keeps the ring alive; single consumer.
        let mut rx =
            unsafe { Ring::attach_consumer::<Trusted>(NonNull::from(&*ring), Some(0)) }.unwrap();
        let mut buf = [Slot::ZERO; 3];
        let mut expect = 1u8;
        while expect <= 5 {
            let n = rx.pop_into(&mut buf).unwrap();
            if n == 0 {
                thread::yield_now();
            }
            for s in &buf[..n] {
                assert_eq!(*s, slot(expect));
                expect += 1;
            }
        }
        p.join().unwrap();
    });
}

/// Typed handles over the same protocol.
#[test]
fn typed() {
    model(|| {
        let ring = ring::<2>();
        let r = ring.clone();
        let p = thread::spawn(move || {
            // SAFETY: the Arc keeps the ring alive; single producer.
            let mut tx = unsafe {
                TypedProducer::<[u64; 2], 2, Trusted>::attach(NonNull::from(&*r), Some(0))
            }
            .unwrap();
            for v in 1..=3u64 {
                while tx.try_push([v, !v]).is_err() {
                    thread::yield_now();
                }
            }
        });
        // SAFETY: the Arc keeps the ring alive; single consumer.
        let mut rx = unsafe {
            TypedConsumer::<[u64; 2], 2, Trusted>::attach(NonNull::from(&*ring), Some(0))
        }
        .unwrap();
        for v in 1..=3u64 {
            let got = loop {
                match rx.try_pop() {
                    Ok(x) => break x,
                    Err(_) => thread::yield_now(),
                }
            };
            assert_eq!(got, [v, !v]);
        }
        p.join().unwrap();
    });
}

/// Negative test: with the publishing store weakened to `Relaxed`, loom must
/// find an interleaving in which the consumer reads a slot concurrently with
/// the producer's write. If this test ever stops panicking, the model checker
/// has lost its teeth.
#[test]
#[cfg(m4h_ring_broken_release)]
#[should_panic]
fn relaxed_publish_is_caught() {
    model(|| {
        let ring = ring::<2>();
        let p = producer(ring.clone(), vec![1, 2]);
        // SAFETY: the Arc keeps the ring alive; single consumer.
        let mut rx =
            unsafe { Ring::attach_consumer::<Trusted>(NonNull::from(&*ring), Some(0)) }.unwrap();
        for _ in 0..2 {
            while rx.try_pop().is_err() {
                thread::yield_now();
            }
        }
        p.join().unwrap();
    });
}
