//! Поэтапное отображение уже зарезервированных физических страниц DMA.
//! Ошибка не теряет сведения об успешно отображённых страницах.

use crate::{ActivePageTable, PAGE_SIZE, USER_SPACE_END_EXCLUSIVE};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, Page, PageTableFlags, PhysFrame, Size4KiB,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaWindowError {
    InvalidRange,
    PartialMapping,
    MappingFailed,
    UnmappingFailed,
    WrongPhysicalFrame,
}

/// Преобразователь одной страницы. Возврат ошибки не должен менять отображение.
pub trait DmaPageAccess {
    fn map_page(&mut self, virtual_addr: u64, physical_addr: u64) -> Result<(), DmaWindowError>;
    fn unmap_page(&mut self, virtual_addr: u64, physical_addr: u64)
        -> Result<(), DmaWindowError>;
}

/// Не владеет физическими страницами. Они остаются закреплёнными у владельца,
/// пока mapped_pages не станет нулём. Не требует кучи.
pub struct DmaKernelWindow {
    virtual_start: u64,
    physical_start: u64,
    page_count: u64,
    mapped_pages: u64,
}

impl DmaKernelWindow {
    pub fn new(
        virtual_start: u64,
        physical_start: u64,
        page_count: u64,
    ) -> Result<Self, DmaWindowError> {
        let bytes = page_count
            .checked_mul(PAGE_SIZE)
            .ok_or(DmaWindowError::InvalidRange)?;
        let last_virtual = virtual_start
            .checked_add(bytes.checked_sub(1).ok_or(DmaWindowError::InvalidRange)?)
            .ok_or(DmaWindowError::InvalidRange)?;
        let _last_physical = physical_start
            .checked_add(bytes - 1)
            .ok_or(DmaWindowError::InvalidRange)?;
        if virtual_start % PAGE_SIZE != 0
            || physical_start % PAGE_SIZE != 0
            || virtual_start < USER_SPACE_END_EXCLUSIVE
            || VirtAddr::try_new(virtual_start).is_err()
            || VirtAddr::try_new(last_virtual).is_err()
            || PhysAddr::try_new(physical_start).is_err()
            || PhysAddr::try_new(_last_physical).is_err()
        {
            return Err(DmaWindowError::InvalidRange);
        }
        Ok(Self {
            virtual_start,
            physical_start,
            page_count,
            mapped_pages: 0,
        })
    }

    /// При ошибке сохраняет количество уже отображённых страниц.
    /// Владелец обязан выполнить unmap_all до освобождения физических кадров.
    pub fn map_all<A: DmaPageAccess>(&mut self, access: &mut A) -> Result<(), DmaWindowError> {
        if self.mapped_pages != 0 {
            return Err(DmaWindowError::PartialMapping);
        }
        while self.mapped_pages < self.page_count {
            let offset = self.mapped_pages * PAGE_SIZE;
            access.map_page(self.virtual_start + offset, self.physical_start + offset)?;
            self.mapped_pages += 1;
        }
        Ok(())
    }

    /// Возвращает ошибки без потери учёта ещё отображённых страниц.
    pub fn unmap_all<A: DmaPageAccess>(&mut self, access: &mut A) -> Result<(), DmaWindowError> {
        while self.mapped_pages > 0 {
            let index = self.mapped_pages - 1;
            let offset = index * PAGE_SIZE;
            access.unmap_page(self.virtual_start + offset, self.physical_start + offset)?;
            self.mapped_pages -= 1;
        }
        Ok(())
    }

    pub const fn mapped_pages(&self) -> u64 {
        self.mapped_pages
    }

    pub const fn page_count(&self) -> u64 {
        self.page_count
    }

    pub const fn is_clear(&self) -> bool {
        self.mapped_pages == 0
    }
}

/// Подключение к существующим активным таблицам ядра x86-64.
/// Внутренняя память для таблиц выделяется из переданного распределителя.
/// Права доступа и политику кэширования задаёт вызывающая сторона.
pub struct ActiveDmaPageAccess<'a, A> {
    table: &'a mut ActivePageTable,
    allocator: &'a mut A,
    flags: PageTableFlags,
}

impl<'a, A: FrameAllocator<Size4KiB>> ActiveDmaPageAccess<'a, A> {
    /// # Безопасность
    ///
    /// Вызывающая сторона гарантирует эксклюзивное владение зарезервированным
    /// виртуальным диапазоном, владение физическими кадрами, корректные
    /// флаги кэширования и отсутствие конфликтующих отображений/потоков.
    pub unsafe fn new(
        table: &'a mut ActivePageTable,
        allocator: &'a mut A,
        flags: PageTableFlags,
    ) -> Self {
        Self {
            table,
            allocator,
            flags: (flags | PageTableFlags::NO_EXECUTE)
                & !PageTableFlags::USER_ACCESSIBLE,
        }
    }
}

impl<A: FrameAllocator<Size4KiB>> DmaPageAccess for ActiveDmaPageAccess<'_, A> {
    fn map_page(&mut self, virtual_addr: u64, physical_addr: u64) -> Result<(), DmaWindowError> {
        let virtual_addr = VirtAddr::try_new(virtual_addr)
            .map_err(|_| DmaWindowError::InvalidRange)?;
        let physical_addr = PhysAddr::try_new(physical_addr)
            .map_err(|_| DmaWindowError::InvalidRange)?;
        let page = Page::<Size4KiB>::from_start_address(virtual_addr)
            .map_err(|_| DmaWindowError::InvalidRange)?;
        let frame = PhysFrame::<Size4KiB>::from_start_address(physical_addr)
            .map_err(|_| DmaWindowError::InvalidRange)?;
        unsafe {
            self.table.map_4k(page, frame, self.flags, self.allocator)
        }
        .map_err(|_| DmaWindowError::MappingFailed)
    }

    fn unmap_page(
        &mut self,
        virtual_addr: u64,
        physical_addr: u64,
    ) -> Result<(), DmaWindowError> {
        let virtual_addr = VirtAddr::try_new(virtual_addr)
            .map_err(|_| DmaWindowError::InvalidRange)?;
        let page = Page::<Size4KiB>::from_start_address(virtual_addr)
            .map_err(|_| DmaWindowError::InvalidRange)?;
        if self.table.translate_addr(virtual_addr).map(|p| p.as_u64())
            != Some(physical_addr)
        {
            return Err(DmaWindowError::WrongPhysicalFrame);
        }
        self.table
            .unmap_4k(page)
            .map_err(|_| DmaWindowError::UnmappingFailed)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakePages {
        mappings: [bool; 4],
        fail_map: Option<usize>,
        fail_unmap: Option<usize>,
        start: u64,
    }

    impl DmaPageAccess for FakePages {
        fn map_page(&mut self, virtual_addr: u64, _physical_addr: u64) -> Result<(), DmaWindowError> {
            let index = ((virtual_addr - self.start) / PAGE_SIZE) as usize;
            if self.fail_map == Some(index) {
                return Err(DmaWindowError::MappingFailed);
            }
            assert!(!self.mappings[index]);
            self.mappings[index] = true;
            Ok(())
        }

        fn unmap_page(
            &mut self,
            virtual_addr: u64,
            _physical_addr: u64,
        ) -> Result<(), DmaWindowError> {
            let index = ((virtual_addr - self.start) / PAGE_SIZE) as usize;
            if self.fail_unmap == Some(index) {
                return Err(DmaWindowError::UnmappingFailed);
            }
            assert!(self.mappings[index]);
            self.mappings[index] = false;
            Ok(())
        }
    }

    const BASE: u64 = 0xffff_9000_0000_0000;

    #[test]
    fn incomplete_mapping_remains_owned_until_rollback() {
        let mut window = DmaKernelWindow::new(BASE, 0x4000, 3).unwrap();
        let mut pages = FakePages {
            mappings: [false; 4],
            fail_map: Some(2),
            fail_unmap: Some(1),
            start: BASE,
        };
        assert_eq!(window.map_all(&mut pages), Err(DmaWindowError::MappingFailed));
        assert_eq!(window.mapped_pages(), 2);
        assert_eq!(window.map_all(&mut pages), Err(DmaWindowError::PartialMapping));
        assert_eq!(window.unmap_all(&mut pages), Err(DmaWindowError::UnmappingFailed));
        assert_eq!(window.mapped_pages(), 2);
        pages.fail_unmap = None;
        window.unmap_all(&mut pages).unwrap();
        assert!(window.is_clear());
        assert_eq!(pages.mappings, [false; 4]);
        pages.fail_map = None;
        window.map_all(&mut pages).unwrap();
        assert_eq!(window.mapped_pages(), 3);
        window.unmap_all(&mut pages).unwrap();
    }

    #[test]
    fn invalid_windows_cannot_touch_page_tables() {
        assert!(DmaKernelWindow::new(BASE, 0, 0).is_err());
        assert!(DmaKernelWindow::new(BASE + 1, 0x1000, 1).is_err());
        assert!(DmaKernelWindow::new(BASE, 0x1001, 1).is_err());
        assert!(DmaKernelWindow::new(0x1000, 0x1000, 1).is_err());
        assert!(DmaKernelWindow::new(u64::MAX - 4095, 0x1000, 2).is_err());
        assert!(DmaKernelWindow::new(BASE, u64::MAX - 4095, 2).is_err());
    }
}
