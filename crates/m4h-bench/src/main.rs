//! Benchmarks for M4H components.
//!
//! ```text
//! m4h-bench topology
//! m4h-bench throughput --cores A,B [--impl LIST] [--messages M] [--batch B]
//! m4h-bench latency    --cores A,B [--impl LIST] [--rounds R]
//! m4h-bench suite      [--messages M] [--rounds R] [--batch B]
//! ```
//!
//! Every run pins the producer (or pinger) to core `A` and the consumer (or
//! ponger) to core `B`, and allocates the m4h rings on the NUMA node of the
//! receiving core. Messages are 64-byte slots carrying a sequence number that
//! the receiver checks, in every implementation.
//!
//! `suite` picks the core pairs from the topology: SMT siblings, two physical
//! cores of the same NUMA node, and two cores on different NUMA nodes
//! (cross-socket on a dual-socket server), and runs everything on each.
//!
//! Implementations: `m4h` (single slot), `m4h-batch` (reserve/commit,
//! peek/release), `rtrb` (single), `rtrb-chunk` (chunk API), `crossbeam`
//! (`ArrayQueue`, an MPMC queue: the price of generality). Ring capacity is
//! 1024 slots for all.

use crossbeam_queue::ArrayQueue;
use m4h_platform::linux::Linux;
use m4h_platform::{Clock, CoreId, CoreInfo, Cores, Memory, NumaNode, Rings};
use m4h_ring::{Ring, Slot, Trusted};
use std::hint::spin_loop;
use std::process::ExitCode;
use std::sync::Barrier;
use std::thread;
use std::time::{Duration, Instant};

/// Capacity of every ring under test.
const CAPACITY: usize = 1024;
/// Latency rounds discarded before measuring.
const WARMUP_ROUNDS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Impl {
    M4h,
    M4hBatch,
    Rtrb,
    RtrbChunk,
    Crossbeam,
}

impl Impl {
    const ALL: [Impl; 5] = [
        Impl::M4h,
        Impl::M4hBatch,
        Impl::Rtrb,
        Impl::RtrbChunk,
        Impl::Crossbeam,
    ];

    fn name(self) -> &'static str {
        match self {
            Impl::M4h => "m4h",
            Impl::M4hBatch => "m4h-batch",
            Impl::Rtrb => "rtrb",
            Impl::RtrbChunk => "rtrb-chunk",
            Impl::Crossbeam => "crossbeam",
        }
    }

    fn parse(s: &str) -> Option<Impl> {
        Impl::ALL.into_iter().find(|i| i.name() == s)
    }

    /// Batching only changes throughput; latency is measured one message at a time.
    fn has_latency(self) -> bool {
        matches!(self, Impl::M4h | Impl::Rtrb | Impl::Crossbeam)
    }
}

struct Options {
    command: String,
    cores: Option<(CoreId, CoreId)>,
    impls: Vec<Impl>,
    messages: u64,
    rounds: u64,
    batch: usize,
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    let mut o = Options {
        command,
        cores: None,
        impls: Impl::ALL.to_vec(),
        messages: 10_000_000,
        rounds: 1_000_000,
        batch: 32,
    };
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--cores" => {
                let (a, b) = value
                    .split_once(',')
                    .ok_or("--cores takes A,B (two logical CPU ids)")?;
                let parse = |s: &str| s.trim().parse().map(CoreId).map_err(|e| format!("{e}"));
                o.cores = Some((parse(a)?, parse(b)?));
            }
            "--impl" => {
                o.impls = value
                    .split(',')
                    .map(|s| Impl::parse(s.trim()).ok_or(format!("unknown implementation {s}")))
                    .collect::<Result<_, _>>()?;
            }
            "--messages" => o.messages = value.parse().map_err(|e| format!("{e}"))?,
            "--rounds" => o.rounds = value.parse().map_err(|e| format!("{e}"))?,
            "--batch" => o.batch = value.parse().map_err(|e| format!("{e}"))?,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    if o.messages == 0 || o.rounds == 0 {
        return Err("--messages and --rounds must be at least 1".into());
    }
    if o.batch == 0 || o.batch > CAPACITY {
        return Err(format!("--batch must be between 1 and {CAPACITY}"));
    }
    Ok(o)
}

const USAGE: &str = "\
usage:
  m4h-bench topology
  m4h-bench throughput --cores A,B [--impl LIST] [--messages M] [--batch B]
  m4h-bench latency    --cores A,B [--impl LIST] [--rounds R]
  m4h-bench suite      [--impl LIST] [--messages M] [--rounds R] [--batch B]

implementations: m4h, m4h-batch, rtrb, rtrb-chunk, crossbeam (default: all)";

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let p = match Linux::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot initialize the Linux platform: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match opts.command.as_str() {
        "topology" => {
            print_topology(&p);
            Ok(())
        }
        "throughput" | "latency" => match opts.cores {
            Some((a, b)) => run_pair(
                &p,
                &opts,
                a,
                b,
                opts.command == "throughput",
                opts.command == "latency",
            ),
            None => Err("--cores A,B is required".into()),
        },
        "suite" => run_suite(&p, &opts),
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_topology(p: &Linux) {
    println!(
        "{} logical CPUs, NUMA nodes {:?}, tick {:.3} GHz",
        p.cores().len(),
        p.nodes().iter().map(|n| n.0).collect::<Vec<_>>(),
        p.tick_hz() as f64 / 1e9
    );
    println!(
        "{:>5} {:>7} {:>5} {:>5} {:>5}  smt siblings",
        "cpu", "package", "core", "node", "l3"
    );
    for c in p.cores() {
        let siblings: Vec<u32> = p
            .cores()
            .iter()
            .filter(|o| o.is_sibling_of(c))
            .map(|o| o.id.0)
            .collect();
        println!(
            "{:>5} {:>7} {:>5} {:>5} {:>5}  {:?}",
            c.id.0,
            c.package,
            c.core,
            c.node.0,
            c.l3.map_or("-".to_string(), |l| l.to_string()),
            siblings
        );
    }
}

/// Core pairs for the suite: (label, a, b).
fn suite_pairs(p: &Linux) -> Vec<(&'static str, CoreInfo, CoreInfo)> {
    let cores = p.cores();
    let mut pairs = Vec::new();
    let find = |pred: &dyn Fn(&CoreInfo, &CoreInfo) -> bool| {
        cores
            .iter()
            .flat_map(|a| cores.iter().map(move |b| (a, b)))
            .find(|(a, b)| a.id != b.id && pred(a, b))
            .map(|(a, b)| (*a, *b))
    };
    if let Some((a, b)) = find(&|a, b| a.is_sibling_of(b)) {
        pairs.push(("SMT siblings", a, b));
    }
    if let Some((a, b)) =
        find(&|a, b| a.node == b.node && a.package == b.package && !a.is_sibling_of(b))
    {
        pairs.push(("same NUMA node", a, b));
    }
    if let Some((a, b)) = find(&|a, b| a.node != b.node) {
        pairs.push(("cross NUMA node", a, b));
    }
    pairs
}

fn run_suite(p: &Linux, opts: &Options) -> Result<(), String> {
    print_topology(p);
    let pairs = suite_pairs(p);
    if pairs.is_empty() {
        return Err("need at least two logical CPUs".into());
    }
    if !pairs.iter().any(|(label, ..)| *label == "cross NUMA node") {
        println!("\nnote: a single NUMA node is visible; the cross-node case is skipped");
    }
    for (label, a, b) in pairs {
        println!(
            "\n=== {label}: cpu {} (node {}) -> cpu {} (node {}) ===",
            a.id.0, a.node.0, b.id.0, b.node.0
        );
        run_pair(p, opts, a.id, b.id, true, true)?;
    }
    Ok(())
}

fn run_pair(
    p: &Linux,
    opts: &Options,
    a: CoreId,
    b: CoreId,
    tput: bool,
    lat: bool,
) -> Result<(), String> {
    let info = |c: CoreId| {
        p.core_info(c)
            .copied()
            .ok_or(format!("cpu {} not available", c.0))
    };
    let (ia, ib) = (info(a)?, info(b)?);
    if a == b {
        return Err("the two cores must differ".into());
    }
    if tput {
        println!(
            "\nthroughput: {} messages of 64 B, capacity {CAPACITY}, batch {}",
            opts.messages, opts.batch
        );
        println!(
            "{:<12} {:>12} {:>10} {:>12}",
            "impl", "Mmsg/s", "GB/s", "ns/msg"
        );
        for &imp in &opts.impls {
            let d = throughput(p, imp, ia, ib, opts.messages, opts.batch)?;
            let rate = opts.messages as f64 / d.as_secs_f64();
            println!(
                "{:<12} {:>12.2} {:>10.2} {:>12.2}",
                imp.name(),
                rate / 1e6,
                rate * 64.0 / 1e9,
                1e9 / rate
            );
        }
    }
    if lat {
        println!(
            "\nlatency: ping-pong round trip, {} rounds after {WARMUP_ROUNDS} warm-up (ns)",
            opts.rounds
        );
        println!(
            "{:<12} {:>8} {:>8} {:>8} {:>9} {:>9} {:>10}",
            "impl", "min", "p50", "p99", "p99.9", "max", "one-way*"
        );
        for &imp in opts.impls.iter().filter(|i| i.has_latency()) {
            let mut samples = latency(p, imp, ia, ib, opts.rounds)?;
            let s = Stats::new(&mut samples, p.tick_hz());
            println!(
                "{:<12} {:>8.0} {:>8.0} {:>8.0} {:>9.0} {:>9.0} {:>10.0}",
                imp.name(),
                s.min,
                s.p50,
                s.p99,
                s.p999,
                s.max,
                s.p50 / 2.0
            );
        }
        println!("* one-way = p50 / 2");
    }
    Ok(())
}

fn slot(i: u64) -> Slot {
    Slot::from_pod(i)
}

#[inline(always)]
fn check(got: u64, expect: u64) {
    if got != expect {
        panic!("sequence error: got {got}, expected {expect}");
    }
}

/// Runs `producer` pinned to `a` and `consumer` pinned to `b`, released
/// together; returns the wall time from release to both finishing.
fn timed_pair<'s>(
    p: &'s Linux,
    a: CoreId,
    b: CoreId,
    producer: impl FnOnce() + Send + 's,
    consumer: impl FnOnce() + Send + 's,
) -> Result<Duration, String> {
    let barrier = Barrier::new(3);
    thread::scope(|s| {
        let barrier = &barrier;
        let tx = s.spawn(move || {
            p.pin_current(a)
                .map_err(|e| format!("pin cpu {}: {e}", a.0))?;
            barrier.wait();
            producer();
            Ok::<(), String>(())
        });
        let rx = s.spawn(move || {
            p.pin_current(b)
                .map_err(|e| format!("pin cpu {}: {e}", b.0))?;
            barrier.wait();
            consumer();
            Ok::<(), String>(())
        });
        barrier.wait();
        let start = Instant::now();
        let r1 = tx.join().map_err(|_| "producer panicked".to_string())?;
        let r2 = rx.join().map_err(|_| "consumer panicked".to_string())?;
        let elapsed = start.elapsed();
        r1.and(r2).map(|()| elapsed)
    })
}

/// Allocates a ring on `node` and returns the region with its two handles.
fn m4h_ring<'r>(
    p: &Linux,
    node: NumaNode,
) -> Result<
    (
        m4h_platform::RingRegion<CAPACITY>,
        m4h_ring::Producer<'r, CAPACITY, Trusted>,
        m4h_ring::Consumer<'r, CAPACITY, Trusted>,
    ),
    String,
> {
    let rr = p
        .create_ring::<CAPACITY>(Some(node), 0)
        .map_err(|e| format!("ring allocation: {e}"))?;
    // SAFETY: the region outlives the handles (callers free it after the
    // threads using them have joined); one handle per side.
    let tx = unsafe { Ring::attach_producer::<Trusted>(rr.ring(), Some(0)) }
        .map_err(|e| e.to_string())?;
    // SAFETY: as above.
    let rx = unsafe { Ring::attach_consumer::<Trusted>(rr.ring(), Some(0)) }
        .map_err(|e| e.to_string())?;
    Ok((rr, tx, rx))
}

fn throughput(
    p: &Linux,
    imp: Impl,
    a: CoreInfo,
    b: CoreInfo,
    n: u64,
    batch: usize,
) -> Result<Duration, String> {
    match imp {
        Impl::M4h => {
            let (rr, mut tx, mut rx) = m4h_ring(p, b.node)?;
            let d = timed_pair(
                p,
                a.id,
                b.id,
                move || {
                    for i in 0..n {
                        let s = slot(i);
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
                                check(s.to_pod(), i);
                                i += 1;
                            }
                            Err(_) => spin_loop(),
                        }
                    }
                },
            );
            // SAFETY: both handles were moved into the joined threads.
            unsafe { p.free_region(rr.into_region()) };
            d
        }
        Impl::M4hBatch => {
            let (rr, mut tx, mut rx) = m4h_ring(p, b.node)?;
            let d = timed_pair(
                p,
                a.id,
                b.id,
                move || {
                    let mut i = 0;
                    while i < n {
                        let want = (n - i).min(batch as u64) as usize;
                        let r = tx.reserve(want).expect("corrupted");
                        let k = r.len();
                        for s in r.iter_mut() {
                            *s = slot(i);
                            i += 1;
                        }
                        tx.commit(k);
                        if k == 0 {
                            spin_loop();
                        }
                    }
                },
                move || {
                    let mut i = 0;
                    while i < n {
                        let r = rx.peek(batch).expect("corrupted");
                        let k = r.len();
                        for s in r {
                            check(s.to_pod(), i);
                            i += 1;
                        }
                        rx.release(k);
                        if k == 0 {
                            spin_loop();
                        }
                    }
                },
            );
            // SAFETY: both handles were moved into the joined threads.
            unsafe { p.free_region(rr.into_region()) };
            d
        }
        Impl::Rtrb => {
            let (mut tx, mut rx) = rtrb::RingBuffer::<Slot>::new(CAPACITY);
            timed_pair(
                p,
                a.id,
                b.id,
                move || {
                    for i in 0..n {
                        let mut s = slot(i);
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
                                check(s.to_pod(), i);
                                i += 1;
                            }
                            Err(_) => spin_loop(),
                        }
                    }
                },
            )
        }
        Impl::RtrbChunk => {
            let (mut tx, mut rx) = rtrb::RingBuffer::<Slot>::new(CAPACITY);
            timed_pair(
                p,
                a.id,
                b.id,
                move || {
                    let mut i = 0;
                    while i < n {
                        let k = ((n - i) as usize).min(batch).min(tx.slots());
                        if k == 0 {
                            spin_loop();
                            continue;
                        }
                        let chunk = tx.write_chunk_uninit(k).expect("slots checked");
                        let written = chunk.fill_from_iter((i..).map(slot));
                        i += written as u64;
                    }
                },
                move || {
                    let mut i = 0;
                    while i < n {
                        let k = batch.min(rx.slots());
                        if k == 0 {
                            spin_loop();
                            continue;
                        }
                        let chunk = rx.read_chunk(k).expect("slots checked");
                        let (x, y) = chunk.as_slices();
                        for s in x.iter().chain(y) {
                            check(s.to_pod(), i);
                            i += 1;
                        }
                        chunk.commit_all();
                    }
                },
            )
        }
        Impl::Crossbeam => {
            let q = ArrayQueue::<Slot>::new(CAPACITY);
            let q = &q;
            timed_pair(
                p,
                a.id,
                b.id,
                move || {
                    for i in 0..n {
                        let mut s = slot(i);
                        while let Err(back) = q.push(s) {
                            s = back;
                            spin_loop();
                        }
                    }
                },
                move || {
                    let mut i = 0;
                    while i < n {
                        match q.pop() {
                            Some(s) => {
                                check(s.to_pod(), i);
                                i += 1;
                            }
                            None => spin_loop(),
                        }
                    }
                },
            )
        }
    }
}

/// A one-message channel for the ping-pong, abstracting the implementation.
trait Pipe: Send {
    fn send(&mut self, s: Slot);
    fn recv(&mut self) -> Slot;
}

struct M4hPipe<'r> {
    tx: m4h_ring::Producer<'r, CAPACITY, Trusted>,
    rx: m4h_ring::Consumer<'r, CAPACITY, Trusted>,
}

impl Pipe for M4hPipe<'_> {
    #[inline(always)]
    fn send(&mut self, s: Slot) {
        while self.tx.try_push(&s).is_err() {
            spin_loop();
        }
    }
    #[inline(always)]
    fn recv(&mut self) -> Slot {
        loop {
            if let Ok(s) = self.rx.try_pop() {
                return s;
            }
            spin_loop();
        }
    }
}

struct RtrbPipe {
    tx: rtrb::Producer<Slot>,
    rx: rtrb::Consumer<Slot>,
}

impl Pipe for RtrbPipe {
    #[inline(always)]
    fn send(&mut self, mut s: Slot) {
        while let Err(rtrb::PushError::Full(back)) = self.tx.push(s) {
            s = back;
            spin_loop();
        }
    }
    #[inline(always)]
    fn recv(&mut self) -> Slot {
        loop {
            if let Ok(s) = self.rx.pop() {
                return s;
            }
            spin_loop();
        }
    }
}

struct CrossbeamPipe<'q> {
    tx: &'q ArrayQueue<Slot>,
    rx: &'q ArrayQueue<Slot>,
}

impl Pipe for CrossbeamPipe<'_> {
    #[inline(always)]
    fn send(&mut self, mut s: Slot) {
        while let Err(back) = self.tx.push(s) {
            s = back;
            spin_loop();
        }
    }
    #[inline(always)]
    fn recv(&mut self) -> Slot {
        loop {
            if let Some(s) = self.rx.pop() {
                return s;
            }
            spin_loop();
        }
    }
}

/// Ping-pong between `a` (pinger, measures) and `b` (ponger, echoes).
/// Returns one round-trip sample per measured round, in ticks.
fn ping_pong<'s>(
    p: &'s Linux,
    a: CoreId,
    b: CoreId,
    rounds: u64,
    mut ping: impl Pipe + 's,
    mut pong: impl Pipe + 's,
) -> Result<Vec<u64>, String> {
    let total = WARMUP_ROUNDS + rounds;
    let mut samples = Vec::with_capacity(rounds as usize);
    let out = &mut samples;
    timed_pair(
        p,
        a,
        b,
        move || {
            for r in 0..total {
                let t0 = p.ticks();
                ping.send(slot(r));
                let back = ping.recv();
                let t1 = p.ticks();
                check(back.to_pod(), r);
                if r >= WARMUP_ROUNDS {
                    out.push(t1.wrapping_sub(t0));
                }
            }
        },
        move || {
            for _ in 0..total {
                let s = pong.recv();
                pong.send(s);
            }
        },
    )?;
    Ok(samples)
}

fn latency(
    p: &Linux,
    imp: Impl,
    a: CoreInfo,
    b: CoreInfo,
    rounds: u64,
) -> Result<Vec<u64>, String> {
    match imp {
        Impl::M4h | Impl::M4hBatch => {
            let (fwd, fwd_tx, fwd_rx) = m4h_ring(p, b.node)?;
            let (back, back_tx, back_rx) = m4h_ring(p, a.node)?;
            let samples = ping_pong(
                p,
                a.id,
                b.id,
                rounds,
                M4hPipe {
                    tx: fwd_tx,
                    rx: back_rx,
                },
                M4hPipe {
                    tx: back_tx,
                    rx: fwd_rx,
                },
            );
            // SAFETY: all handles were moved into the joined threads.
            unsafe {
                p.free_region(fwd.into_region());
                p.free_region(back.into_region());
            }
            samples
        }
        Impl::Rtrb | Impl::RtrbChunk => {
            let (fwd_tx, fwd_rx) = rtrb::RingBuffer::<Slot>::new(CAPACITY);
            let (back_tx, back_rx) = rtrb::RingBuffer::<Slot>::new(CAPACITY);
            ping_pong(
                p,
                a.id,
                b.id,
                rounds,
                RtrbPipe {
                    tx: fwd_tx,
                    rx: back_rx,
                },
                RtrbPipe {
                    tx: back_tx,
                    rx: fwd_rx,
                },
            )
        }
        Impl::Crossbeam => {
            let fwd = ArrayQueue::<Slot>::new(CAPACITY);
            let back = ArrayQueue::<Slot>::new(CAPACITY);
            ping_pong(
                p,
                a.id,
                b.id,
                rounds,
                CrossbeamPipe {
                    tx: &fwd,
                    rx: &back,
                },
                CrossbeamPipe {
                    tx: &back,
                    rx: &fwd,
                },
            )
        }
    }
}

struct Stats {
    min: f64,
    p50: f64,
    p99: f64,
    p999: f64,
    max: f64,
}

impl Stats {
    /// Percentiles of `samples` (ticks), converted to nanoseconds.
    fn new(samples: &mut [u64], tick_hz: u64) -> Stats {
        samples.sort_unstable();
        let ns = |t: u64| t as f64 * 1e9 / tick_hz as f64;
        let at = |q: f64| {
            let i = ((samples.len() as f64 * q) as usize).min(samples.len() - 1);
            ns(samples[i])
        };
        Stats {
            min: ns(samples[0]),
            p50: at(0.50),
            p99: at(0.99),
            p999: at(0.999),
            max: ns(samples[samples.len() - 1]),
        }
    }
}
