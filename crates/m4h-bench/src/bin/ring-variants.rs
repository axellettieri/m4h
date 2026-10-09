//! Investigation of the SMT-sibling throughput gap between m4h-ring and rtrb.
//!
//! ```text
//! cargo run --release -p m4h-bench --bin ring-variants -- A,B [messages] [runs]
//! ```
//!
//! Single-slot throughput only, same loops as `m4h-bench throughput`, with the
//! m4h ring placed in different ways to isolate the cause:
//!
//! - `m4h mmap`: ring in a page-aligned mmap region (what m4h-bench does)
//! - `m4h mmap +2K`: same, base shifted by 2048 bytes (tests 4K aliasing)
//! - `m4h heap`: ring in a Box (malloc'd, 128-byte aligned)
//! - `m4h mmap N=256`: smaller ring (16 KiB of slots)
//! - `m4h mmap N=4096`: bigger ring (256 KiB of slots)
//! - `rtrb`            : baseline, capacity 1024
//!
//! Prints the median and min/max over `runs` runs, in million messages/s.

use m4h_platform::linux::Linux;
use m4h_platform::{CoreId, Cores, Memory, PageSize, RegionRequest};
use m4h_ring::{Consumer, Producer, Ring, Slot, Trusted};
use std::hint::spin_loop;
use std::mem::{MaybeUninit, size_of};
use std::ptr::NonNull;
use std::sync::Barrier;
use std::thread;
use std::time::Instant;

fn pair<'s>(
    p: &'s Linux,
    a: CoreId,
    b: CoreId,
    prod: impl FnOnce() + Send + 's,
    cons: impl FnOnce() + Send + 's,
) -> f64 {
    let barrier = Barrier::new(3);
    let start = thread::scope(|s| {
        let bar = &barrier;
        s.spawn(move || {
            p.pin_current(a).unwrap();
            bar.wait();
            prod();
        });
        s.spawn(move || {
            p.pin_current(b).unwrap();
            bar.wait();
            cons();
        });
        barrier.wait();
        Instant::now()
    });
    start.elapsed().as_secs_f64()
}

fn run_m4h<const N: usize>(
    p: &Linux,
    a: CoreId,
    b: CoreId,
    n: u64,
    mut tx: Producer<'_, N, Trusted>,
    mut rx: Consumer<'_, N, Trusted>,
) -> f64 {
    let secs = pair(
        p,
        a,
        b,
        move || {
            for i in 0..n {
                let s = Slot::from_pod(i);
                while tx.try_push(&s).is_err() {
                    spin_loop();
                }
            }
        },
        move || {
            let mut i = 0;
            while i < n {
                match rx.try_pop() {
                    Ok(s) => {
                        assert_eq!(s.to_pod::<u64>(), i);
                        i += 1;
                    }
                    Err(_) => spin_loop(),
                }
            }
        },
    );
    n as f64 / secs / 1e6
}

/// m4h ring in an mmap region, at byte offset `offset` from the region start.
fn m4h_mmap<const N: usize>(p: &Linux, a: CoreId, b: CoreId, n: u64, offset: usize) -> f64 {
    let len = size_of::<Ring<N>>() + offset;
    let region = p
        .alloc_region(RegionRequest::new(len).page_size(PageSize::Base))
        .unwrap();
    // SAFETY: `offset` keeps 128-byte alignment and the ring inside the region.
    let ring = unsafe { NonNull::new_unchecked(region.as_ptr().as_ptr().add(offset)) }.cast();
    // SAFETY: zeroed region, no handles yet.
    unsafe { Ring::<N>::init_shared(ring, 0) };
    // SAFETY: the region outlives the run; one handle per side.
    let tx = unsafe { Ring::attach_producer::<Trusted>(ring, None) }.unwrap();
    // SAFETY: as above.
    let rx = unsafe { Ring::attach_consumer::<Trusted>(ring, None) }.unwrap();
    let r = run_m4h(p, a, b, n, tx, rx);
    // SAFETY: the handles were consumed by the joined threads.
    unsafe { p.free_region(region) };
    r
}

fn m4h_heap(p: &Linux, a: CoreId, b: CoreId, n: u64) -> f64 {
    let mut place = Box::new(MaybeUninit::<Ring<1024>>::uninit());
    let ring = Ring::new_in(&mut place, 0);
    let (tx, rx) = ring.split();
    run_m4h(p, a, b, n, tx, rx)
}

fn rtrb(p: &Linux, a: CoreId, b: CoreId, n: u64) -> f64 {
    let (mut tx, mut rx) = rtrb::RingBuffer::<Slot>::new(1024);
    let secs = pair(
        p,
        a,
        b,
        move || {
            for i in 0..n {
                let mut s = Slot::from_pod(i);
                while let Err(rtrb::PushError::Full(back)) = tx.push(s) {
                    s = back;
                    spin_loop();
                }
            }
        },
        move || {
            let mut i = 0;
            while i < n {
                match rx.pop() {
                    Ok(s) => {
                        assert_eq!(s.to_pod::<u64>(), i);
                        i += 1;
                    }
                    Err(_) => spin_loop(),
                }
            }
        },
    );
    n as f64 / secs / 1e6
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cores = args.next().unwrap_or_else(|| "0,1".into());
    let (a, b) = cores.split_once(',').expect("cores as A,B");
    let (a, b) = (CoreId(a.parse().unwrap()), CoreId(b.parse().unwrap()));
    let n: u64 = args.next().map_or(10_000_000, |s| s.parse().unwrap());
    let runs: usize = args.next().map_or(7, |s| s.parse().unwrap());
    let p = Linux::new().unwrap();

    type Variant = (&'static str, fn(&Linux, CoreId, CoreId, u64) -> f64);
    let variants: [Variant; 6] = [
        ("m4h mmap", |p, a, b, n| m4h_mmap::<1024>(p, a, b, n, 0)),
        ("m4h mmap +2K", |p, a, b, n| {
            m4h_mmap::<1024>(p, a, b, n, 2048)
        }),
        ("m4h heap", m4h_heap),
        ("m4h mmap N=256", |p, a, b, n| {
            m4h_mmap::<256>(p, a, b, n, 0)
        }),
        ("m4h mmap N=4096", |p, a, b, n| {
            m4h_mmap::<4096>(p, a, b, n, 0)
        }),
        ("rtrb", rtrb),
    ];

    println!(
        "cpu {} -> cpu {}, {n} messages, {runs} runs (Mmsg/s)",
        a.0, b.0
    );
    println!(
        "{:<18} {:>8} {:>8} {:>8}",
        "variant", "median", "min", "max"
    );
    let mut results: Vec<Vec<f64>> = vec![Vec::new(); variants.len()];
    // Interleave variants run by run so that drift affects all of them alike.
    for _ in 0..runs {
        for (i, (_, f)) in variants.iter().enumerate() {
            results[i].push(f(&p, a, b, n));
        }
    }
    for ((name, _), r) in variants.iter().zip(&mut results) {
        r.sort_by(|x, y| x.partial_cmp(y).unwrap());
        println!(
            "{:<18} {:>8.1} {:>8.1} {:>8.1}",
            name,
            r[r.len() / 2],
            r[0],
            r[r.len() - 1]
        );
    }
}
