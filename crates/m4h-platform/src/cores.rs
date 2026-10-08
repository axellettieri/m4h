//! Cores and topology.

use crate::{Error, NumaNode};

/// A logical CPU (hardware thread), numbered as by the OS / APIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoreId(pub u32);

/// Where a logical CPU sits: socket → NUMA node → L3 group → core → thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CoreInfo {
    /// The logical CPU.
    pub id: CoreId,
    /// Physical package (socket).
    pub package: u32,
    /// Physical core within the package. Logical CPUs with the same
    /// `(package, core)` are SMT siblings.
    pub core: u32,
    /// NUMA node of the CPU.
    pub node: NumaNode,
    /// Identifier of the L3 cache it shares, if known.
    pub l3: Option<u32>,
}

impl CoreInfo {
    /// `true` if `other` is an SMT sibling (same physical core).
    pub fn is_sibling_of(&self, other: &CoreInfo) -> bool {
        self.id != other.id && self.package == other.package && self.core == other.core
    }
}

/// Cores, topology and threads.
pub trait Cores {
    /// Handle of a thread started with [`Cores::spawn_on`].
    type Thread;

    /// All logical CPUs available to the application, sorted by id.
    fn cores(&self) -> &[CoreInfo];

    /// Topology of one logical CPU.
    fn core_info(&self, id: CoreId) -> Option<&CoreInfo> {
        self.cores().iter().find(|c| c.id == id)
    }

    /// The logical CPU the caller is running on.
    fn current_core(&self) -> CoreId;

    /// Pins the calling thread to `core`.
    fn pin_current(&self, core: CoreId) -> Result<(), Error>;

    /// Starts `entry(arg)` on a new thread pinned to `core`. Returns once the
    /// thread is pinned, or the pinning error.
    fn spawn_on(&self, core: CoreId, entry: fn(usize), arg: usize) -> Result<Self::Thread, Error>;

    /// Waits for a thread started with [`Cores::spawn_on`] to finish.
    fn join(&self, thread: Self::Thread) -> Result<(), Error>;
}
