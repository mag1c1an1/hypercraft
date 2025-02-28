use bitflags;
use core::marker::PhantomData;
use core::ptr::{slice_from_raw_parts, slice_from_raw_parts_mut};

use crate::{HyperCraftHal, HostPhysAddr, GuestPhysAddr};
use crate::{HyperResult, HyperError};

use page_table::MappingFlags;

/// Information about nested page faults.
#[derive(Debug)]
pub struct NestedPageFaultInfo {
    /// Access type that caused the nested page fault.
    pub access_flags: MappingFlags,
    /// Guest physical address that caused the nested page fault.
    pub fault_guest_paddr: GuestPhysAddr,
}

/// A 4K-sized contiguous physical memory page, it will deallocate the page
/// automatically on drop.
#[derive(Debug)]
pub struct PhysFrame<H: HyperCraftHal> {
    start_paddr: HostPhysAddr,
    _phantom: PhantomData<H>,
}

impl<H: HyperCraftHal> PhysFrame<H> {
    pub fn alloc() -> HyperResult<Self> {
        let start_paddr = H::alloc_page()
            .map(|va| H::virt_to_phys(va))
            .ok_or_else(|| HyperError::NoMemory)?;
        trace!("allocated physframe {:#018x}", start_paddr);
        assert_ne!(start_paddr, 0);
        Ok(Self {
            start_paddr,
            _phantom: PhantomData,
        })
    }

    pub fn alloc_zero() -> HyperResult<Self> {
        let mut f = Self::alloc()?;
        f.fill(0);
        Ok(f)
    }

    pub const unsafe fn uninit() -> Self {
        Self {
            start_paddr: 0,
            _phantom: PhantomData,
        }
    }

    pub fn start_paddr(&self) -> HostPhysAddr {
        self.start_paddr
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        H::phys_to_virt(self.start_paddr) as *mut u8
    }

    pub fn fill(&mut self, byte: u8) {
        unsafe { core::ptr::write_bytes(self.as_mut_ptr(), byte, H::PAGE_SIZE) }
    }
}

impl<H: HyperCraftHal> Clone for PhysFrame<H> {
    fn clone(&self) -> Self {
        let mut other = Self::alloc_zero().unwrap();
        let src = unsafe { slice_from_raw_parts(self.as_mut_ptr(), H::PAGE_SIZE).as_ref() }.unwrap();
        let mut dst = unsafe { slice_from_raw_parts_mut(other.as_mut_ptr(), H::PAGE_SIZE).as_mut().unwrap() };
        unsafe { dst.copy_from_slice(&*src); }
        other
    }
}

impl<H: HyperCraftHal> Drop for PhysFrame<H> {
    fn drop(&mut self) {
        if self.start_paddr > 0 {
            trace!("dropping physframe {:#018x}", self.start_paddr);
            H::dealloc_page(H::phys_to_virt(self.start_paddr));
            trace!("dropped physframe {:#018x}", self.start_paddr);
        }
    }
}