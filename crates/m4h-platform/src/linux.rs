//! Linux hosted backend.
//!
//! - **Memory**: anonymous `mmap`, with `MAP_HUGETLB` for 2 MiB / 1 GiB pages
//!   (falling back to smaller pages if allowed, with a transparent-huge-page
//!   hint), bound to a NUMA node with `mbind(MPOL_BIND)` *before* the pages are
//!   touched, then pre-faulted so the hot path never takes a page fault.
//! - **Cores**: the CPUs in the process affinity mask; topology from sysfs;
//!   pinning with `sched_setaffinity`.
//! - **Clock**: `CLOCK_MONOTONIC`; ticks from the TSC on x86_64, calibrated
//!   against the monotonic clock at start-up.
//! - **Wait**: `pause`, `sched_yield`, `clock_nanosleep(TIMER_ABSTIME)`.
//! - **Storage**: one regular file per device under a root directory.
//! - **Console**: stdin and stdout.

use crate::{
    BlockDevice, Clock, Console, CoreId, CoreInfo, Cores, Error, ErrorKind, Instant, Memory,
    NumaNode, PageSize, Region, RegionRequest, Rings, Storage, Wait, WaitHint,
};
use core::ptr::NonNull;
use std::format;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::string::String;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::vec::Vec;

/// `MAP_HUGE_SHIFT` from `<linux/mman.h>`: the huge page size is encoded as
/// `log2(size) << 26` in the `mmap` flags.
const MAP_HUGE_SHIFT: i32 = 26;
/// `MPOL_BIND` from `<linux/mempolicy.h>`.
const MPOL_BIND: i32 = 2;
/// Bits in the node mask passed to `mbind` (supports nodes 0..1024).
const NODE_MASK_WORDS: usize = 16;

/// The Linux hosted platform.
#[derive(Debug)]
pub struct Linux {
    cores: Vec<CoreInfo>,
    tick_hz: u64,
    storage_root: PathBuf,
}

impl Linux {
    /// Reads the topology of the CPUs this process may run on and calibrates
    /// the tick counter (about 20 ms).
    ///
    /// Storage devices live in the current directory; see
    /// [`Linux::with_storage_root`].
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            cores: read_topology()?,
            tick_hz: calibrate_ticks(),
            storage_root: PathBuf::from("."),
        })
    }

    /// Sets the directory holding the storage devices.
    pub fn with_storage_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.storage_root = root.into();
        self
    }

    /// The NUMA nodes that have at least one available CPU, sorted.
    pub fn nodes(&self) -> Vec<NumaNode> {
        let mut nodes: Vec<NumaNode> = self.cores.iter().map(|c| c.node).collect();
        nodes.sort();
        nodes.dedup();
        nodes
    }

    fn map(len: usize, page_size: PageSize) -> Result<(NonNull<u8>, usize), Error> {
        let len = len.div_ceil(page_size.bytes()) * page_size.bytes();
        let mut flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
        match page_size {
            PageSize::Base => {}
            PageSize::Huge2M => flags |= libc::MAP_HUGETLB | (21 << MAP_HUGE_SHIFT),
            PageSize::Huge1G => flags |= libc::MAP_HUGETLB | (30 << MAP_HUGE_SHIFT),
        }
        // SAFETY: anonymous mapping at an address chosen by the kernel; no
        // existing memory is affected.
        let ptr = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                flags,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(last_os_error());
        }
        let ptr = NonNull::new(ptr.cast::<u8>()).ok_or(Error::new(ErrorKind::Other))?;
        Ok((ptr, len))
    }

    fn bind(ptr: NonNull<u8>, len: usize, node: NumaNode) -> Result<(), Error> {
        let index = node.0 as usize;
        if index >= NODE_MASK_WORDS * 64 {
            return Err(ErrorKind::InvalidArgument.into());
        }
        let mut mask = [0u64; NODE_MASK_WORDS];
        mask[index / 64] |= 1 << (index % 64);
        // SAFETY: `ptr..ptr + len` is a mapping we own; the mask is a valid
        // buffer of `NODE_MASK_WORDS * 64` bits (the kernel reads maxnode - 1
        // bits, hence the + 1).
        let rc = unsafe {
            libc::syscall(
                libc::SYS_mbind,
                ptr.as_ptr(),
                len,
                MPOL_BIND,
                mask.as_ptr(),
                NODE_MASK_WORDS * 64 + 1,
                0,
            )
        };
        if rc != 0 {
            return Err(last_os_error());
        }
        Ok(())
    }

    fn cpu_set(core: CoreId) -> Result<libc::cpu_set_t, Error> {
        if core.0 as usize >= libc::CPU_SETSIZE as usize {
            return Err(ErrorKind::InvalidArgument.into());
        }
        // SAFETY: `cpu_set_t` is a plain bit array; all-zero is the empty set.
        let mut set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
        // SAFETY: the index is below CPU_SETSIZE (checked above).
        unsafe { libc::CPU_SET(core.0 as usize, &mut set) };
        Ok(set)
    }
}

impl Memory for Linux {
    fn alloc_region(&self, request: RegionRequest) -> Result<Region, Error> {
        if request.len == 0 {
            return Err(ErrorKind::InvalidArgument.into());
        }
        let mut page_size = request.page_size;
        let (ptr, len) = loop {
            match Self::map(request.len, page_size) {
                Ok(mapping) => break mapping,
                Err(e) => match page_size.smaller() {
                    Some(smaller) if request.fallback => page_size = smaller,
                    _ => return Err(e),
                },
            }
        };
        if page_size == PageSize::Base && request.page_size > PageSize::Base {
            // Fell back to base pages: ask for transparent huge pages instead.
            // Best effort; the region is valid either way.
            // SAFETY: advisory call on a mapping we own.
            unsafe { libc::madvise(ptr.as_ptr().cast(), len, libc::MADV_HUGEPAGE) };
        }

        let mut node = None;
        if let Some(wanted) = request.node {
            match Self::bind(ptr, len, wanted) {
                Ok(()) => node = Some(wanted),
                Err(_) if request.fallback => {}
                Err(e) => {
                    // SAFETY: we own the mapping and nothing refers to it.
                    unsafe { libc::munmap(ptr.as_ptr().cast(), len) };
                    return Err(e);
                }
            }
        }

        // Pre-fault every page now (after binding, so pages come from the
        // right node): no page faults on the hot path.
        let step = page_size.bytes();
        let mut offset = 0;
        while offset < len {
            // SAFETY: `offset < len`, inside the mapping; anonymous memory is
            // already zero, so writing zero keeps the contents.
            unsafe { ptr.as_ptr().add(offset).write_volatile(0) };
            offset += step;
        }

        // SAFETY: we own the mapping; it is zeroed, page-aligned, of `len`
        // bytes and backed by `page_size` pages bound to `node`.
        Ok(unsafe { Region::from_raw(ptr, len, page_size, node) })
    }

    unsafe fn free_region(&self, region: Region) {
        // SAFETY: the region came from `alloc_region` (caller contract), so it
        // is exactly one mapping of `len` bytes.
        unsafe { libc::munmap(region.as_ptr().as_ptr().cast(), region.len()) };
    }
}

impl Rings for Linux {}

impl Cores for Linux {
    type Thread = JoinHandle<()>;

    fn cores(&self) -> &[CoreInfo] {
        &self.cores
    }

    fn current_core(&self) -> CoreId {
        // SAFETY: no arguments, no memory access.
        let cpu = unsafe { libc::sched_getcpu() };
        CoreId(cpu.max(0) as u32)
    }

    fn pin_current(&self, core: CoreId) -> Result<(), Error> {
        if self.core_info(core).is_none() {
            return Err(ErrorKind::NotFound.into());
        }
        let set = Self::cpu_set(core)?;
        // SAFETY: `set` is a valid `cpu_set_t` of the size we pass.
        let rc =
            unsafe { libc::sched_setaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &set) };
        if rc != 0 {
            return Err(last_os_error());
        }
        Ok(())
    }

    fn spawn_on(&self, core: CoreId, entry: fn(usize), arg: usize) -> Result<Self::Thread, Error> {
        if self.core_info(core).is_none() {
            return Err(ErrorKind::NotFound.into());
        }
        let set = Self::cpu_set(core)?;
        let (ack_tx, ack_rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name(format!("m4h-core-{}", core.0))
            .spawn(move || {
                // SAFETY: `set` is a valid `cpu_set_t` of the size we pass.
                let rc = unsafe {
                    libc::sched_setaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &set)
                };
                let pinned = if rc == 0 {
                    Ok(())
                } else {
                    Err(last_os_error())
                };
                let ok = pinned.is_ok();
                let _ = ack_tx.send(pinned);
                if ok {
                    entry(arg);
                }
            })
            .map_err(Error::from)?;
        match ack_rx.recv() {
            Ok(Ok(())) => Ok(handle),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => Err(ErrorKind::Other.into()),
        }
    }

    fn join(&self, thread: Self::Thread) -> Result<(), Error> {
        thread.join().map_err(|_| Error::new(ErrorKind::Other))
    }
}

impl Clock for Linux {
    fn now(&self) -> Instant {
        Instant(monotonic_ns())
    }

    fn ticks(&self) -> u64 {
        ticks()
    }

    fn tick_hz(&self) -> u64 {
        self.tick_hz
    }
}

impl Wait for Linux {
    fn wait(&self, hint: WaitHint) {
        match hint {
            WaitHint::Spin => {}
            WaitHint::Relax => core::hint::spin_loop(),
            WaitHint::Yield => thread::yield_now(),
            WaitHint::Until(deadline) => {
                let ts = libc::timespec {
                    tv_sec: (deadline.0 / 1_000_000_000) as libc::time_t,
                    tv_nsec: (deadline.0 % 1_000_000_000) as libc::c_long,
                };
                // Retry on EINTR until the deadline has passed.
                // SAFETY: `ts` is a valid timespec; no remaining-time output.
                while unsafe {
                    libc::clock_nanosleep(
                        libc::CLOCK_MONOTONIC,
                        libc::TIMER_ABSTIME,
                        &ts,
                        core::ptr::null_mut(),
                    )
                } == libc::EINTR
                {}
            }
        }
    }
}

/// A storage device backed by a regular file.
#[derive(Debug)]
pub struct FileDevice {
    file: File,
}

impl BlockDevice for FileDevice {
    fn block_size(&self) -> usize {
        4096
    }

    fn len(&self) -> Result<u64, Error> {
        Ok(self.file.metadata()?.len())
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Error> {
        let mut done = 0;
        while done < buf.len() {
            match self.file.read_at(&mut buf[done..], offset + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(done)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, Error> {
        self.file.write_all_at(buf, offset)?;
        Ok(buf.len())
    }

    fn flush(&self) -> Result<(), Error> {
        Ok(self.file.sync_data()?)
    }
}

impl Storage for Linux {
    type Device = FileDevice;

    fn open_device(&self, name: &str, create: Option<u64>) -> Result<FileDevice, Error> {
        let valid = !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.contains('\0');
        if !valid {
            return Err(ErrorKind::InvalidArgument.into());
        }
        let path = self.storage_root.join(name);
        let file = match create {
            Some(len) => match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    file.set_len(len)?;
                    file
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    OpenOptions::new().read(true).write(true).open(&path)?
                }
                Err(e) => return Err(e.into()),
            },
            None => OpenOptions::new().read(true).write(true).open(&path)?,
        };
        Ok(FileDevice { file })
    }
}

impl Console for Linux {
    fn write_str(&self, s: &str) -> Result<(), Error> {
        let mut out = io::stdout().lock();
        out.write_all(s.as_bytes())?;
        Ok(out.flush()?)
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, Error> {
        Ok(io::stdin().lock().read(buf)?)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        match e.raw_os_error() {
            Some(code) => from_errno(code),
            None => Error::new(match e.kind() {
                io::ErrorKind::NotFound => ErrorKind::NotFound,
                io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
                io::ErrorKind::InvalidInput => ErrorKind::InvalidArgument,
                io::ErrorKind::OutOfMemory => ErrorKind::OutOfMemory,
                io::ErrorKind::Unsupported => ErrorKind::Unsupported,
                _ => ErrorKind::Other,
            }),
        }
    }
}

fn from_errno(code: i32) -> Error {
    let kind = match code {
        libc::ENOMEM => ErrorKind::OutOfMemory,
        libc::EPERM | libc::EACCES => ErrorKind::PermissionDenied,
        libc::EINVAL | libc::EFAULT => ErrorKind::InvalidArgument,
        libc::ENOENT | libc::ENODEV | libc::ESRCH => ErrorKind::NotFound,
        libc::ENOSYS | libc::EOPNOTSUPP => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    };
    Error { kind, code }
}

fn last_os_error() -> Error {
    from_errno(io::Error::last_os_error().raw_os_error().unwrap_or(0))
}

fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid output buffer; CLOCK_MONOTONIC always exists.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(target_arch = "x86_64")]
// `_rdtsc` is safe on recent toolchains and `unsafe` on the MSRV.
#[allow(unused_unsafe)]
fn ticks() -> u64 {
    // SAFETY: RDTSC is available on every x86_64 CPU and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

#[cfg(not(target_arch = "x86_64"))]
fn ticks() -> u64 {
    monotonic_ns()
}

#[cfg(target_arch = "x86_64")]
fn calibrate_ticks() -> u64 {
    let (t0, c0) = (monotonic_ns(), ticks());
    thread::sleep(core::time::Duration::from_millis(20));
    let (t1, c1) = (monotonic_ns(), ticks());
    let ns = (t1 - t0).max(1) as u128;
    ((c1.wrapping_sub(c0) as u128 * 1_000_000_000) / ns) as u64
}

#[cfg(not(target_arch = "x86_64"))]
fn calibrate_ticks() -> u64 {
    1_000_000_000
}

/// CPUs in the process affinity mask, with their topology from sysfs.
fn read_topology() -> Result<Vec<CoreInfo>, Error> {
    // SAFETY: all-zero is the empty set.
    let mut set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
    // SAFETY: `set` is a valid output buffer of the size we pass.
    let rc =
        unsafe { libc::sched_getaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &mut set) };
    if rc != 0 {
        return Err(last_os_error());
    }
    let mut cores = Vec::new();
    for cpu in 0..libc::CPU_SETSIZE as usize {
        // SAFETY: `cpu` is below CPU_SETSIZE.
        if !unsafe { libc::CPU_ISSET(cpu, &set) } {
            continue;
        }
        let base = format!("/sys/devices/system/cpu/cpu{cpu}");
        cores.push(CoreInfo {
            id: CoreId(cpu as u32),
            package: read_u32(&format!("{base}/topology/physical_package_id")).unwrap_or(0),
            core: read_u32(&format!("{base}/topology/core_id")).unwrap_or(cpu as u32),
            node: NumaNode(cpu_node(&base).unwrap_or(0)),
            l3: read_u32(&format!("{base}/cache/index3/id")),
        });
    }
    if cores.is_empty() {
        return Err(ErrorKind::NotFound.into());
    }
    Ok(cores)
}

fn read_u32(path: &str) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The NUMA node of a CPU: sysfs has a `nodeN` link in the CPU directory.
fn cpu_node(cpu_dir: &str) -> Option<u32> {
    fs::read_dir(cpu_dir).ok()?.find_map(|entry| {
        let name: String = entry.ok()?.file_name().into_string().ok()?;
        name.strip_prefix("node")?.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicU32, Ordering};
    use core::time::Duration;
    use m4h_ring::{PopError, PushError, Ring, Slot, Trusted};

    fn platform() -> Linux {
        Linux::new().expect("Linux platform")
    }

    #[test]
    fn topology_is_consistent() {
        let p = platform();
        let cores = p.cores();
        assert!(!cores.is_empty());
        assert!(cores.windows(2).all(|w| w[0].id < w[1].id));
        assert!(!p.nodes().is_empty());
        assert!(p.core_info(p.current_core()).is_some());
    }

    #[test]
    fn pin_current_moves_the_thread() {
        let p = platform();
        let target = p.cores().last().unwrap().id;
        thread::scope(|s| {
            s.spawn(|| {
                p.pin_current(target).unwrap();
                assert_eq!(p.current_core(), target);
            });
        });
        assert_eq!(
            p.pin_current(CoreId(u32::MAX)).unwrap_err().kind,
            ErrorKind::NotFound
        );
    }

    static SEEN_CORE: AtomicU32 = AtomicU32::new(u32::MAX);

    fn record_core(expected: usize) {
        // SAFETY: no arguments.
        let cpu = unsafe { libc::sched_getcpu() } as u32;
        assert_eq!(cpu as usize, expected);
        SEEN_CORE.store(cpu, Ordering::SeqCst);
    }

    #[test]
    fn spawn_on_pins_before_running() {
        let p = platform();
        let core = p.cores()[0].id;
        let t = p.spawn_on(core, record_core, core.0 as usize).unwrap();
        p.join(t).unwrap();
        assert_eq!(SEEN_CORE.load(Ordering::SeqCst), core.0);
    }

    #[test]
    fn regions_are_zeroed_and_writable() {
        let p = platform();
        let r = p.alloc_region(RegionRequest::new(10_000)).unwrap();
        assert_eq!(r.len(), 12 * 1024);
        assert_eq!(r.page_size(), PageSize::Base);
        // SAFETY: the region is ours, `len` bytes long.
        let bytes = unsafe { core::slice::from_raw_parts_mut(r.as_ptr().as_ptr(), r.len()) };
        assert!(bytes.iter().all(|&b| b == 0));
        bytes.fill(0xA5);
        // SAFETY: nothing refers to the region any more.
        unsafe { p.free_region(r) };
    }

    #[test]
    fn huge_pages_fall_back_when_allowed() {
        let p = platform();
        let r = p
            .alloc_region(RegionRequest::new(1).page_size(PageSize::Huge2M))
            .unwrap();
        assert_eq!(r.len() % r.page_size().bytes(), 0);
        assert_eq!(r.as_ptr().as_ptr() as usize % r.page_size().bytes(), 0);
        // SAFETY: nothing refers to the region.
        unsafe { p.free_region(r) };

        // Strict 1 GiB: either we really get one, or a clean error.
        match p.alloc_region(RegionRequest::new(1).page_size(PageSize::Huge1G).strict()) {
            Ok(r) => {
                assert_eq!(r.page_size(), PageSize::Huge1G);
                // SAFETY: nothing refers to the region.
                unsafe { p.free_region(r) };
            }
            Err(e) => assert_ne!(e.kind, ErrorKind::InvalidArgument),
        }
    }

    #[test]
    fn regions_bind_to_a_node() {
        let p = platform();
        let node = p.nodes()[0];
        let r = p
            .alloc_region(RegionRequest::new(1 << 20).node(node))
            .unwrap();
        // With fallback, binding may be refused (e.g. in a container), but
        // a bound region must report the requested node.
        assert!(r.node().is_none() || r.node() == Some(node));
        // SAFETY: nothing refers to the region.
        unsafe { p.free_region(r) };
        assert_eq!(
            p.alloc_region(RegionRequest::new(0)).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );
    }

    #[test]
    fn clock_and_wait() {
        let p = platform();
        assert!(p.tick_hz() > 1_000_000, "tick_hz = {}", p.tick_hz());
        let (t0, c0) = (p.now(), p.ticks());
        let deadline = t0 + Duration::from_millis(5);
        p.wait(WaitHint::Until(deadline));
        let (t1, c1) = (p.now(), p.ticks());
        assert!(t1 >= deadline);
        assert!(c1 > c0);
        p.wait(WaitHint::Relax);
        p.wait(WaitHint::Yield);
        p.wait(WaitHint::Spin);
    }

    #[test]
    fn storage_round_trip() {
        let root = std::env::temp_dir().join(format!("m4h-storage-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let p = platform().with_storage_root(&root);
        assert_eq!(
            p.open_device("missing", None).unwrap_err().kind,
            ErrorKind::NotFound
        );
        assert_eq!(
            p.open_device("../escape", Some(1)).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );
        let dev = p.open_device("disk0", Some(8192)).unwrap();
        assert_eq!(dev.len().unwrap(), 8192);
        assert_eq!(dev.write_at(4000, b"ad astra").unwrap(), 8);
        dev.flush().unwrap();
        let again = p.open_device("disk0", Some(1)).unwrap();
        assert_eq!(again.len().unwrap(), 8192, "existing device is not resized");
        let mut buf = [0u8; 8];
        assert_eq!(again.read_at(4000, &mut buf).unwrap(), 8);
        assert_eq!(&buf, b"ad astra");
        let mut tail = [0u8; 16];
        assert_eq!(again.read_at(8190, &mut tail).unwrap(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn console_writes() {
        platform().write_str("m4h-platform console test\n").unwrap();
    }

    #[test]
    fn ring_between_two_pinned_threads() {
        const N: usize = 256;
        const MESSAGES: u64 = 200_000;
        let p = platform();
        let rr = p.create_ring::<N>(None, 1).unwrap();
        let ring = rr.ring();
        let cores = p.cores();
        let (a, b) = (cores[0].id, cores[cores.len() - 1].id);
        // SAFETY: the region outlives the scope; one handle per side.
        let mut tx = unsafe { Ring::attach_producer::<Trusted>(ring, Some(1)) }.unwrap();
        // SAFETY: as above.
        let mut rx = unsafe { Ring::attach_consumer::<Trusted>(ring, Some(1)) }.unwrap();
        thread::scope(|s| {
            let p = &p;
            s.spawn(move || {
                p.pin_current(a).unwrap();
                let mut i = 0;
                while i < MESSAGES {
                    match tx.try_push(&Slot::from_pod(i)) {
                        Ok(()) => i += 1,
                        Err(PushError::Full) => p.wait(WaitHint::Relax),
                        Err(e) => panic!("{e}"),
                    }
                }
            });
            s.spawn(move || {
                p.pin_current(b).unwrap();
                let mut expect = 0;
                while expect < MESSAGES {
                    match rx.try_pop() {
                        Ok(slot) => {
                            assert_eq!(slot.to_pod::<u64>(), expect);
                            expect += 1;
                        }
                        Err(PopError::Empty) => p.wait(WaitHint::Relax),
                        Err(e) => panic!("{e}"),
                    }
                }
            });
        });
        // SAFETY: both handles are gone.
        unsafe { p.free_region(rr.into_region()) };
    }
}
