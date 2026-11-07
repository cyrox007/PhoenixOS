//! Поэтапное отображение уже зарезервированных физических страниц DMA.
//! Ошибка не теряет сведения об успешно отображённых страницах.

use crate::{ActivePageTable, PAGE_SIZE, USER_SPACE_END_EXCLUSIVE};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{FrameAllocator, Page, PageTableFlags, PhysFrame, Size4KiB},
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
    fn unmap_page(&mut self, virtual_addr: u64, physical_addr: u64) -> Result<(), DmaWindowError>;
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

/// Выбор флагов кэширования для уже проверенного физического диапазона.
///
/// Итоговый аппаратный тип памяти зависит также от PAT и MTRR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaCachePolicy {
    WriteBack,
    Uncacheable,
}

impl DmaCachePolicy {
    pub fn page_flags(self) -> PageTableFlags {
        let base = PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        match self {
            Self::WriteBack => base,
            Self::Uncacheable => base | PageTableFlags::NO_CACHE | PageTableFlags::WRITE_THROUGH,
        }
    }
}

/// Идентификатор резервирования с защитой от повторного использования записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaWindowHandle {
    slot: usize,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaReservationError {
    InvalidWindow(DmaWindowError),
    Overlap,
    PhysicalAlias,
    Exhausted,
    StaleHandle,
    StillMapped,
}

struct ReservationSlot {
    window: Option<DmaKernelWindow>,
    generation: u64,
}

/// Реестр непересекающихся виртуальных окон с фиксированной вместимостью.
///
/// Проверяет пересечения только в управляемом им диапазоне. Владелец ядра
/// обязан предварительно исключить этот диапазон из прочих назначений.
pub struct DmaWindowRegistry<const N: usize> {
    slots: [ReservationSlot; N],
}

impl<const N: usize> DmaWindowRegistry<N> {
    pub const fn new() -> Self {
        Self {
            slots: [const {
                ReservationSlot {
                    window: None,
                    generation: 0,
                }
            }; N],
        }
    }

    pub fn reserve(
        &mut self,
        virtual_start: u64,
        physical_start: u64,
        page_count: u64,
    ) -> Result<DmaWindowHandle, DmaReservationError> {
        let window = DmaKernelWindow::new(virtual_start, physical_start, page_count)
            .map_err(DmaReservationError::InvalidWindow)?;
        let last = virtual_start + (page_count * PAGE_SIZE - 1);
        let last_physical = physical_start + (page_count * PAGE_SIZE - 1);
        for slot in &self.slots {
            let Some(existing) = slot.window.as_ref() else {
                continue;
            };
            let existing_last = existing.virtual_start + (existing.page_count * PAGE_SIZE - 1);
            if virtual_start <= existing_last && existing.virtual_start <= last {
                return Err(DmaReservationError::Overlap);
            }
            let physical_end = existing.physical_start + (existing.page_count * PAGE_SIZE - 1);
            if physical_start <= physical_end && existing.physical_start <= last_physical {
                return Err(DmaReservationError::PhysicalAlias);
            }
        }

        let index = self
            .slots
            .iter()
            .position(|slot| slot.window.is_none() && slot.generation < u64::MAX)
            .ok_or(DmaReservationError::Exhausted)?;
        let slot = &mut self.slots[index];
        slot.generation += 1;
        slot.window = Some(window);
        Ok(DmaWindowHandle {
            slot: index,
            generation: slot.generation,
        })
    }

    fn checked_slot(&self, handle: DmaWindowHandle) -> Result<usize, DmaReservationError> {
        let slot = self
            .slots
            .get(handle.slot)
            .ok_or(DmaReservationError::StaleHandle)?;
        if slot.generation != handle.generation || slot.window.is_none() {
            return Err(DmaReservationError::StaleHandle);
        }
        Ok(handle.slot)
    }

    pub fn map_all<A: DmaPageAccess>(
        &mut self,
        handle: DmaWindowHandle,
        access: &mut A,
    ) -> Result<(), DmaReservationError> {
        let index = self.checked_slot(handle)?;
        self.slots[index]
            .window
            .as_mut()
            .expect("проверенное резервирование")
            .map_all(access)
            .map_err(DmaReservationError::InvalidWindow)
    }

    pub fn unmap_all<A: DmaPageAccess>(
        &mut self,
        handle: DmaWindowHandle,
        access: &mut A,
    ) -> Result<(), DmaReservationError> {
        let index = self.checked_slot(handle)?;
        self.slots[index]
            .window
            .as_mut()
            .expect("проверенное резервирование")
            .unmap_all(access)
            .map_err(DmaReservationError::InvalidWindow)
    }

    pub fn mapped_pages(&self, handle: DmaWindowHandle) -> Result<u64, DmaReservationError> {
        let index = self.checked_slot(handle)?;
        Ok(self.slots[index]
            .window
            .as_ref()
            .expect("проверенное резервирование")
            .mapped_pages())
    }

    /// Запрещает повторную выдачу окна до полного снятия отображений.
    pub fn release(&mut self, handle: DmaWindowHandle) -> Result<(), DmaReservationError> {
        let index = self.checked_slot(handle)?;
        let slot = &mut self.slots[index];
        if !slot
            .window
            .as_ref()
            .expect("проверенное резервирование")
            .is_clear()
        {
            return Err(DmaReservationError::StillMapped);
        }
        slot.window = None;
        Ok(())
    }
}

impl<const N: usize> Default for DmaWindowRegistry<N> {
    fn default() -> Self {
        Self::new()
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
    /// Создаёт отображатель с явно выбранной политикой кэширования.
    ///
    /// # Безопасность
    ///
    /// Вызывающий код обязан проверить PAT/MTRR, свойства физической памяти,
    /// отсутствие несовместимых псевдонимов и владение виртуальным окном.
    pub unsafe fn with_cache_policy(
        table: &'a mut ActivePageTable,
        allocator: &'a mut A,
        policy: DmaCachePolicy,
    ) -> Self {
        unsafe { Self::new(table, allocator, policy.page_flags()) }
    }
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
            flags: (flags | PageTableFlags::NO_EXECUTE) & !PageTableFlags::USER_ACCESSIBLE,
        }
    }
}

impl<A: FrameAllocator<Size4KiB>> DmaPageAccess for ActiveDmaPageAccess<'_, A> {
    fn map_page(&mut self, virtual_addr: u64, physical_addr: u64) -> Result<(), DmaWindowError> {
        let virtual_addr =
            VirtAddr::try_new(virtual_addr).map_err(|_| DmaWindowError::InvalidRange)?;
        let physical_addr =
            PhysAddr::try_new(physical_addr).map_err(|_| DmaWindowError::InvalidRange)?;
        let page = Page::<Size4KiB>::from_start_address(virtual_addr)
            .map_err(|_| DmaWindowError::InvalidRange)?;
        let frame = PhysFrame::<Size4KiB>::from_start_address(physical_addr)
            .map_err(|_| DmaWindowError::InvalidRange)?;
        unsafe { self.table.map_4k(page, frame, self.flags, self.allocator) }
            .map_err(|_| DmaWindowError::MappingFailed)
    }

    fn unmap_page(&mut self, virtual_addr: u64, physical_addr: u64) -> Result<(), DmaWindowError> {
        let virtual_addr =
            VirtAddr::try_new(virtual_addr).map_err(|_| DmaWindowError::InvalidRange)?;
        let page = Page::<Size4KiB>::from_start_address(virtual_addr)
            .map_err(|_| DmaWindowError::InvalidRange)?;
        if self.table.translate_addr(virtual_addr).map(|p| p.as_u64()) != Some(physical_addr) {
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
        fn map_page(
            &mut self,
            virtual_addr: u64,
            _physical_addr: u64,
        ) -> Result<(), DmaWindowError> {
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
        assert_eq!(
            window.map_all(&mut pages),
            Err(DmaWindowError::MappingFailed)
        );
        assert_eq!(window.mapped_pages(), 2);
        assert_eq!(
            window.map_all(&mut pages),
            Err(DmaWindowError::PartialMapping)
        );
        assert_eq!(
            window.unmap_all(&mut pages),
            Err(DmaWindowError::UnmappingFailed)
        );
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
    fn reservation_prevents_overlap_and_reuse_until_unmapped() {
        let mut registry = DmaWindowRegistry::<2>::new();
        let first = registry.reserve(BASE, 0x4000, 2).unwrap();
        assert_eq!(
            registry.reserve(BASE + PAGE_SIZE, 0x8000, 1),
            Err(DmaReservationError::Overlap)
        );
        assert_eq!(
            registry.reserve(BASE + 2 * PAGE_SIZE, 0x5000, 1),
            Err(DmaReservationError::PhysicalAlias)
        );
        let second = registry.reserve(BASE + 2 * PAGE_SIZE, 0x8000, 1).unwrap();
        let mut pages = FakePages {
            mappings: [false; 4],
            fail_map: None,
            fail_unmap: Some(1),
            start: BASE,
        };
        registry.map_all(first, &mut pages).unwrap();
        assert_eq!(
            registry.release(first),
            Err(DmaReservationError::StillMapped)
        );
        assert_eq!(
            registry.reserve(BASE, 0x4000, 1),
            Err(DmaReservationError::Overlap)
        );
        assert!(registry.unmap_all(first, &mut pages).is_err());
        assert_eq!(registry.mapped_pages(first), Ok(2));
        pages.fail_unmap = None;
        registry.unmap_all(first, &mut pages).unwrap();
        registry.release(first).unwrap();
        assert_eq!(
            registry.release(first),
            Err(DmaReservationError::StaleHandle)
        );
        let third = registry.reserve(BASE, 0xa000, 1).unwrap();
        assert_ne!(first, third);
        assert_eq!(
            registry.mapped_pages(first),
            Err(DmaReservationError::StaleHandle)
        );
        registry.release(third).unwrap();
        registry.release(second).unwrap();
    }

    #[test]
    fn highest_canonical_page_can_be_reserved_without_overflow() {
        let mut registry = DmaWindowRegistry::<1>::new();
        let last_page = 0xffff_ffff_ffff_f000;
        let handle = registry.reserve(last_page, 0x1000, 1).unwrap();
        assert_eq!(registry.mapped_pages(handle), Ok(0));
        registry.release(handle).unwrap();
    }

    #[test]
    fn reservations_and_cache_flags_are_explicit() {
        let mut registry = DmaWindowRegistry::<0>::new();
        assert_eq!(
            registry.reserve(BASE, 0x1000, 1),
            Err(DmaReservationError::Exhausted)
        );
        let writable = PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        assert_eq!(DmaCachePolicy::WriteBack.page_flags(), writable);
        assert_eq!(
            DmaCachePolicy::Uncacheable.page_flags(),
            writable | PageTableFlags::NO_CACHE | PageTableFlags::WRITE_THROUGH
        );
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
