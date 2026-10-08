//! The `Platform` trait: everything an M4H application asks of the system.
//!
//! M4H-BEAM and M4H-LB never call the operating system directly. They go
//! through [`Platform`], which has three planned implementations:
//!
//! 1. **Linux hosted** ([`linux::Linux`], feature `linux`): pinned threads,
//!    huge pages, NUMA binding. For development, debugging and as the
//!    baseline every M4H number is compared against.
//! 2. **M4H ring 3**: system calls into the M4H kernel.
//! 3. **M4H ring 0**: direct, inlined calls.
//!
//! The trait is split by concern so that each piece can be implemented,
//! tested and mocked on its own; [`Platform`] is simply all of them together:
//!
//! | Trait | Concern |
//! |---|---|
//! | [`Memory`] | regions with a page size and a NUMA node |
//! | [`Cores`] | topology, current core, pinning, threads on a given core |
//! | [`Rings`] | rings between two cores, in regions both can map |
//! | [`Clock`] | monotonic time and a fast tick counter |
//! | [`Wait`] | how to wait: spin, relax, yield, sleep until a deadline |
//! | [`Storage`] | minimal block devices |
//! | [`Console`] | text in and out |
//!
//! Static dispatch only (generics and associated types, no `dyn`), so the
//! ring 0 backend can inline everything. This is a first draft: the
//! interfaces will change as the VM starts using them.
#![no_std]

#[cfg(all(feature = "linux", target_os = "linux"))]
extern crate std;

mod console;
mod cores;
mod error;
mod memory;
mod rings;
mod storage;
mod time;
mod wait;

#[cfg(all(feature = "linux", target_os = "linux"))]
pub mod linux;

pub use console::Console;
pub use cores::{CoreId, CoreInfo, Cores};
pub use error::{Error, ErrorKind};
pub use memory::{Memory, NumaNode, PageSize, Region, RegionRequest};
pub use rings::{RingRegion, Rings};
pub use storage::{BlockDevice, Storage};
pub use time::{Clock, Instant};
pub use wait::{Wait, WaitHint};

/// Everything an M4H application needs from the system.
///
/// Implemented automatically for any type that implements all the parts.
pub trait Platform: Memory + Cores + Rings + Clock + Wait + Storage + Console {}

impl<T: Memory + Cores + Rings + Clock + Wait + Storage + Console> Platform for T {}
