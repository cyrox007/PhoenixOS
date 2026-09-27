use phoenix_process::{AddressSpace, AddressSpaceId, RegionError, VirtualRegion, PAGE_SIZE};
use phoenix_vm::{InactivePageTable, InactivePageTableError, USER_SPACE_END_EXCLUSIVE};
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, Page, PageTableFlags, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessAddressSpaceError {
    Region(RegionError),
    PageTable(InactivePageTableError),
    NotUserRegion,
}

impl From<RegionError> for ProcessAddressSpaceError {
    fn from(error: RegionError) -> Self {
        Self::Region(error)
    }
}

impl From<InactivePageTableError> for ProcessAddressSpaceError {
    fn from(error: InactivePageTableError) -> Self {
        Self::PageTable(error)
    }
}

pub struct ProcessAddressSpace<const CAPACITY: usize> {
    model: AddressSpace<CAPACITY>,
    page_table: InactivePageTable,
}

impl<const CAPACITY: usize> ProcessAddressSpace<CAPACITY> {
    /// Создаёт отдельный корень процесса и наследует общую верхнюю половину ядра.
    ///
    /// # Безопасность
    ///
    /// `physical_memory_offset` должен быть действующим линейным отображением
    /// физической памяти текущего ядра.
    pub unsafe fn new<A>(
        id: AddressSpaceId,
        physical_memory_offset: VirtAddr,
        allocator: &mut A,
    ) -> Result<Self, ProcessAddressSpaceError>
    where
        A: FrameAllocator<Size4KiB>,
    {
        let mut page_table = unsafe { InactivePageTable::new(physical_memory_offset, allocator)? };
        unsafe {
            page_table.inherit_kernel_half(physical_memory_offset);
        }

        Ok(Self {
            model: AddressSpace::new(id),
            page_table,
        })
    }

    pub fn id(&self) -> AddressSpaceId {
        self.model.id()
    }

    pub fn region_count(&self) -> usize {
        self.model.region_count()
    }

    pub fn map_region<A>(
        &mut self,
        region: VirtualRegion,
        allocator: &mut A,
    ) -> Result<(), ProcessAddressSpaceError>
    where
        A: FrameAllocator<Size4KiB> + FrameDeallocator<Size4KiB>,
    {
        if !region.permissions.user_accessible()
            || region.start >= USER_SPACE_END_EXCLUSIVE
            || region.length > USER_SPACE_END_EXCLUSIVE - region.start
        {
            return Err(ProcessAddressSpaceError::NotUserRegion);
        }

        self.model.map_region(region)?;

        let mut flags = PageTableFlags::empty();
        if region.permissions.writable() {
            flags |= PageTableFlags::WRITABLE;
        }
        if !region.permissions.executable() {
            flags |= PageTableFlags::NO_EXECUTE;
        }

        let page_count = region.length / PAGE_SIZE;
        let mut mapped = 0_u64;

        while mapped < page_count {
            let address = VirtAddr::new(region.start + mapped * PAGE_SIZE);
            let page = Page::<Size4KiB>::containing_address(address);

            if let Err(error) = self.page_table.map_owned_user_4k(page, flags, allocator) {
                while mapped > 0 {
                    mapped -= 1;
                    let rollback_address = VirtAddr::new(region.start + mapped * PAGE_SIZE);
                    let rollback_page = Page::<Size4KiB>::containing_address(rollback_address);
                    self.page_table
                        .unmap_owned_user_4k(rollback_page, allocator)
                        .expect("откат только что созданного отображения обязан быть успешным");
                }

                self.model
                    .unmap_exact(region.start, region.length)
                    .expect("откат зарегистрированного региона обязан быть успешным");
                return Err(ProcessAddressSpaceError::PageTable(error));
            }

            mapped += 1;
        }

        Ok(())
    }

    pub fn unmap_region<A>(
        &mut self,
        start: u64,
        length: u64,
        allocator: &mut A,
    ) -> Result<VirtualRegion, ProcessAddressSpaceError>
    where
        A: FrameDeallocator<Size4KiB>,
    {
        let region = self
            .model
            .find(start)
            .filter(|region| region.start == start && region.length == length)
            .ok_or(ProcessAddressSpaceError::Region(
                RegionError::RegionNotFound,
            ))?;

        let page_count = region.length / PAGE_SIZE;
        for index in 0..page_count {
            let address = VirtAddr::new(region.start + index * PAGE_SIZE);
            let page = Page::<Size4KiB>::containing_address(address);
            self.page_table.unmap_owned_user_4k(page, allocator)?;
        }

        Ok(self.model.unmap_exact(start, length)?)
    }

    pub fn translate_addr(&mut self, address: VirtAddr) -> Option<PhysAddr> {
        self.page_table.translate_user_addr(address)
    }

    pub fn destroy<A>(mut self, allocator: &mut A) -> Result<(), ProcessAddressSpaceError>
    where
        A: FrameDeallocator<Size4KiB>,
    {
        self.page_table.destroy_user_half(allocator)?;
        unsafe {
            self.page_table.clear_kernel_half();
        }
        self.page_table.release_empty(allocator)?;
        Ok(())
    }
}
