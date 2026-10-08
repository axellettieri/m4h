//! Memory regions.

use crate::Error;
use core::ptr::NonNull;

/// Page size backing a region.
///
/// M4H maps memory statically with large pages and never pages on demand;
/// the base page size exists for small regions and as a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PageSize {
    /// 4 KiB.
    Base,
    /// 2 MiB.
    Huge2M,
    /// 1 GiB.
    Huge1G,
}

impl PageSize {
    /// Size in bytes.
    pub const fn bytes(self) -> usize {
        match self {
            PageSize::Base => 4 << 10,
            PageSize::Huge2M => 2 << 20,
            PageSize::Huge1G => 1 << 30,
        }
    }

    /// The next smaller page size, if any.
    pub const fn smaller(self) -> Option<PageSize> {
        match self {
            PageSize::Huge1G => Some(PageSize::Huge2M),
            PageSize::Huge2M => Some(PageSize::Base),
            PageSize::Base => None,
        }
    }
}

/// A NUMA node, as numbered by the firmware (ACPI SRAT) and the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NumaNode(pub u32);

/// What to allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionRequest {
    /// Minimum length in bytes; rounded up to a multiple of the page size.
    pub len: usize,
    /// Preferred page size.
    pub page_size: PageSize,
    /// NUMA node the memory must come from; `None` for any.
    pub node: Option<NumaNode>,
    /// If `true`, the backend may fall back to smaller pages or to an unbound
    /// node instead of failing. The returned [`Region`] says what was obtained.
    pub fallback: bool,
}

impl RegionRequest {
    /// A request for `len` bytes of base pages on any node, with fallback.
    pub const fn new(len: usize) -> Self {
        Self {
            len,
            page_size: PageSize::Base,
            node: None,
            fallback: true,
        }
    }

    /// Sets the preferred page size.
    pub const fn page_size(mut self, page_size: PageSize) -> Self {
        self.page_size = page_size;
        self
    }

    /// Requires memory from `node`.
    pub const fn node(mut self, node: NumaNode) -> Self {
        self.node = Some(node);
        self
    }

    /// Fails instead of falling back to smaller pages or another node.
    pub const fn strict(mut self) -> Self {
        self.fallback = false;
        self
    }
}

/// A region of zeroed, mapped, pre-faulted memory.
///
/// Returned by [`Memory::alloc_region`] and released by
/// [`Memory::free_region`]; dropping a `Region` without freeing it leaks the
/// memory (regions usually live as long as the application).
#[derive(Debug)]
pub struct Region {
    ptr: NonNull<u8>,
    len: usize,
    page_size: PageSize,
    node: Option<NumaNode>,
}

// SAFETY: a region is exclusive ownership of a mapping; it can move between
// threads like a `Box<[u8]>`.
unsafe impl Send for Region {}
// SAFETY: `&Region` only exposes the address and metadata.
unsafe impl Sync for Region {}

impl Region {
    /// Creates a region descriptor. For backends.
    ///
    /// # Safety
    /// `ptr..ptr + len` is a mapping owned by the caller, valid for reads and
    /// writes, zeroed, aligned to `page_size`, and backed as described.
    pub const unsafe fn from_raw(
        ptr: NonNull<u8>,
        len: usize,
        page_size: PageSize,
        node: Option<NumaNode>,
    ) -> Self {
        Self {
            ptr,
            len,
            page_size,
            node,
        }
    }

    /// Start of the region.
    pub const fn as_ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    /// Length in bytes (a multiple of the page size).
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Always `false`: regions are never empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Page size actually backing the region.
    pub const fn page_size(&self) -> PageSize {
        self.page_size
    }

    /// NUMA node the region is bound to, if it is bound.
    pub const fn node(&self) -> Option<NumaNode> {
        self.node
    }
}

/// Memory allocation.
pub trait Memory {
    /// Allocates a zeroed, pre-faulted region.
    fn alloc_region(&self, request: RegionRequest) -> Result<Region, Error>;

    /// Releases a region.
    ///
    /// # Safety
    /// `region` was returned by this platform and nothing refers to its memory
    /// any more.
    unsafe fn free_region(&self, region: Region);
}
