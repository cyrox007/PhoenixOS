#![no_std]

use x86_64::registers::control::Cr3;
use x86_64::structures::paging::mapper::{MapToError, UnmapError};
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags,
    PhysFrame, Size4KiB, Translate,
};
use x86_64::{PhysAddr, VirtAddr};

pub const PAGE_SIZE: u64 = 4096;
pub const KERNEL_P4_START_INDEX: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSpan {
    pub first: Page<Size4KiB>,
    pub count: u64,
}

impl PageSpan {
    pub fn for_bytes(start: VirtAddr, len: u64) -> Option<Self> {
        if len == 0 {
            return None;
        }

        let last_raw = start.as_u64().checked_add(len - 1)?;
        let last_addr = VirtAddr::try_new(last_raw).ok()?;
        let first = Page::<Size4KiB>::containing_address(start);
        let last = Page::<Size4KiB>::containing_address(last_addr);
        let first_number = first.start_address().as_u64() / PAGE_SIZE;
        let last_number = last.start_address().as_u64() / PAGE_SIZE;
        let count = last_number.checked_sub(first_number)?.checked_add(1)?;

        Some(Self { first, count })
    }

    pub fn page(self, index: u64) -> Option<Page<Size4KiB>> {
        if index >= self.count {
            return None;
        }

        let offset = index.checked_mul(PAGE_SIZE)?;
        let address = self.first.start_address().as_u64().checked_add(offset)?;
        let address = VirtAddr::try_new(address).ok()?;
        Some(Page::<Size4KiB>::containing_address(address))
    }
}

/// Обёртка над активной четырёхуровневой таблицей страниц x86-64.
///
/// Объект получает доступ к таблицам через линейное отображение всей физической
/// памяти. Он не владеет этим отображением: его создаёт ранний загрузочный путь.
pub struct ActivePageTable {
    mapper: OffsetPageTable<'static>,
    physical_memory_offset: VirtAddr,
}

impl ActivePageTable {
    /// Создаёт доступ к текущей таблице P4.
    ///
    /// # Безопасность
    ///
    /// Вызывающая сторона обязана гарантировать, что:
    ///
    /// - `physical_memory_offset` действительно указывает на линейное отображение
    ///   всей физической памяти;
    /// - активная CR3 относится к текущему адресному пространству ядра;
    /// - для этой P4 не существует другого одновременного изменяемого владельца.
    pub unsafe fn from_current(physical_memory_offset: VirtAddr) -> Self {
        let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
        let mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };

        Self {
            mapper,
            physical_memory_offset,
        }
    }

    pub fn physical_memory_offset(&self) -> VirtAddr {
        self.physical_memory_offset
    }

    /// Отображает одну страницу 4 КиБ на заданный физический кадр.
    ///
    /// Флаг `PRESENT` добавляется автоматически.
    ///
    /// # Безопасность
    ///
    /// Вызывающая сторона должна обеспечить корректность прав доступа, отсутствие
    /// конфликтующего отображения и допустимость доступа к переданному физическому
    /// кадру с указанными флагами.
    pub unsafe fn map_4k<A>(
        &mut self,
        page: Page<Size4KiB>,
        frame: PhysFrame<Size4KiB>,
        flags: PageTableFlags,
        allocator: &mut A,
    ) -> Result<(), MapToError<Size4KiB>>
    where
        A: FrameAllocator<Size4KiB>,
    {
        let flags = flags | PageTableFlags::PRESENT;
        let flush = unsafe { self.mapper.map_to(page, frame, flags, allocator)? };
        flush.flush();
        Ok(())
    }

    pub fn unmap_4k(&mut self, page: Page<Size4KiB>) -> Result<PhysFrame<Size4KiB>, UnmapError> {
        let (frame, flush) = self.mapper.unmap(page)?;
        flush.flush();
        Ok(frame)
    }

    pub fn translate_addr(&self, address: VirtAddr) -> Option<PhysAddr> {
        self.mapper.translate_addr(address)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InactivePageTableError {
    NoPhysicalFrame,
    NotEmpty,
}

/// Отдельный неактивный корень таблиц страниц.
///
/// На текущем этапе объект владеет только физической страницей P4. Он не
/// активируется автоматически и не наследует отображения ядра. Это позволяет
/// безопасно проверить создание отдельного корня до фиксации постоянной схемы
/// виртуальных адресов процессов.
pub struct InactivePageTable {
    root_frame: PhysFrame<Size4KiB>,
    root_table: *mut PageTable,
}

impl InactivePageTable {
    /// Выделяет и обнуляет отдельную таблицу P4.
    ///
    /// # Безопасность
    ///
    /// `physical_memory_offset` должен указывать на действующее линейное
    /// отображение всей физической памяти, доступное текущему ядру.
    pub unsafe fn new<A>(
        physical_memory_offset: VirtAddr,
        allocator: &mut A,
    ) -> Result<Self, InactivePageTableError>
    where
        A: FrameAllocator<Size4KiB>,
    {
        let root_frame = allocator
            .allocate_frame()
            .ok_or(InactivePageTableError::NoPhysicalFrame)?;

        let virtual_address = physical_memory_offset + root_frame.start_address().as_u64();
        let root_table = virtual_address.as_mut_ptr::<PageTable>();

        unsafe {
            root_table.write(PageTable::new());
        }

        Ok(Self {
            root_frame,
            root_table,
        })
    }

    pub fn root_frame(&self) -> PhysFrame<Size4KiB> {
        self.root_frame
    }

    /// Копирует верхнюю каноническую половину активной P4 в корень процесса.
    ///
    /// Дочерние таблицы разделяются с ядром и остаются собственностью
    /// системного адресного пространства.
    ///
    /// # Безопасность
    ///
    /// Активная P4 должна содержать постоянные ядерные отображения только
    /// в верхней канонической половине.
    pub unsafe fn inherit_kernel_half(&mut self, physical_memory_offset: VirtAddr) {
        let active = unsafe { active_level_4_table(physical_memory_offset) };
        let target = unsafe { &mut *self.root_table };

        for index in KERNEL_P4_START_INDEX..512 {
            target[index] = active[index].clone();
        }
    }

    pub fn kernel_half_matches_active(&self, physical_memory_offset: VirtAddr) -> bool {
        let active = unsafe { active_level_4_table(physical_memory_offset) };
        let target = unsafe { &*self.root_table };

        (KERNEL_P4_START_INDEX..512).all(|index| {
            target[index].addr() == active[index].addr()
                && target[index].flags() == active[index].flags()
        })
    }

    pub fn user_half_is_empty(&self) -> bool {
        let table = unsafe { &*self.root_table };
        (0..KERNEL_P4_START_INDEX).all(|index| table[index].is_unused())
    }

    pub fn is_active(&self) -> bool {
        let (active, _) = Cr3::read();
        active == self.root_frame
    }

    pub fn is_empty(&self) -> bool {
        let table = unsafe { &*self.root_table };

        (0..512).all(|index| table[index].is_unused())
    }

    /// Возвращает физическую страницу пустого корня распределителю.
    ///
    /// Непустой корень освобождать этим методом нельзя: сначала должны быть
    /// уничтожены его дочерние таблицы и отображения.
    pub fn release_empty<A>(self, allocator: &mut A) -> Result<(), InactivePageTableError>
    where
        A: FrameDeallocator<Size4KiB>,
    {
        if !self.is_empty() {
            return Err(InactivePageTableError::NotEmpty);
        }

        unsafe {
            allocator.deallocate_frame(self.root_frame);
        }

        Ok(())
    }
}

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_frame, _) = Cr3::read();
    let physical = level_4_frame.start_address();
    let virtual_address = physical_memory_offset + physical.as_u64();
    let table_ptr: *mut PageTable = virtual_address.as_mut_ptr();

    unsafe { &mut *table_ptr }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_span_covers_unaligned_edges() {
        let span = PageSpan::for_bytes(VirtAddr::new(0x1801), 0x2000).unwrap();

        assert_eq!(span.first.start_address().as_u64(), 0x1000);
        assert_eq!(span.count, 3);
        assert_eq!(span.page(0).unwrap().start_address().as_u64(), 0x1000);
        assert_eq!(span.page(2).unwrap().start_address().as_u64(), 0x3000);
        assert!(span.page(3).is_none());
    }

    #[test]
    fn zero_length_span_is_empty() {
        assert!(PageSpan::for_bytes(VirtAddr::new(0x1000), 0).is_none());
    }

    #[test]
    fn page_span_stays_canonical() {
        let start = VirtAddr::new(0x0000_7fff_ffff_f000);
        assert!(PageSpan::for_bytes(start, 0x2000).is_none());
    }
}
