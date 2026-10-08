//! Rings between two cores, in memory both sides can map.

use crate::{Error, ErrorKind, Memory, NumaNode, PageSize, Region, RegionRequest};
use core::marker::PhantomData;
use core::mem::{align_of, size_of};
use core::ptr::NonNull;
use m4h_ring::Ring;

/// A ring of `N` slots living in a platform [`Region`].
///
/// On Linux both sides are threads of one process, so the region is ordinary
/// memory. On the M4H kernel it is a region mapped into both address spaces
/// (or both partitions). Either way the sides attach with
/// [`Ring::attach_producer`] / [`Ring::attach_consumer`] on
/// [`RingRegion::ring`].
#[derive(Debug)]
pub struct RingRegion<const N: usize> {
    region: Region,
    _marker: PhantomData<Ring<N>>,
}

impl<const N: usize> RingRegion<N> {
    /// The ring, for attaching handles.
    pub fn ring(&self) -> NonNull<Ring<N>> {
        self.region.as_ptr().cast()
    }

    /// The underlying region.
    pub fn region(&self) -> &Region {
        &self.region
    }

    /// Gives the region back, e.g. to free it with [`Memory::free_region`].
    pub fn into_region(self) -> Region {
        self.region
    }
}

/// Creation of rings shared between two cores.
pub trait Rings: Memory {
    /// Allocates and publishes a ring of `N` slots on `node`.
    ///
    /// The default implementation allocates a region (2 MiB pages if the ring
    /// needs at least one, base pages otherwise, with fallback) and
    /// initializes it with [`Ring::init_shared`] at generation `generation`.
    fn create_ring<const N: usize>(
        &self,
        node: Option<NumaNode>,
        generation: u32,
    ) -> Result<RingRegion<N>, Error> {
        let len = size_of::<Ring<N>>();
        let page_size = if len >= PageSize::Huge2M.bytes() {
            PageSize::Huge2M
        } else {
            PageSize::Base
        };
        let mut request = RegionRequest::new(len).page_size(page_size);
        request.node = node;
        let region = self.alloc_region(request)?;
        let ptr = region.as_ptr();
        if region.len() < len || ptr.as_ptr() as usize % align_of::<Ring<N>>() != 0 {
            // SAFETY: freshly allocated, nothing refers to it.
            unsafe { self.free_region(region) };
            return Err(Error::new(ErrorKind::Other));
        }
        // SAFETY: the region is large enough, aligned (pages are aligned far
        // beyond the ring's 128 bytes), zeroed, and no handle exists yet.
        unsafe { Ring::<N>::init_shared(ptr.cast(), generation) };
        Ok(RingRegion {
            region,
            _marker: PhantomData,
        })
    }
}
