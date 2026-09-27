#![no_std]

use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::mapper::{MapToError, UnmapError};
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags,
    PhysFrame, Size4KiB, Translate,
};
use x86_64::{PhysAddr, VirtAddr};

pub const PAGE_SIZE: u64 = 4096;
/// На раннем этапе процессу выделяется первый P4-слот: 0..512 ГиБ.
pub const USER_P4_ENTRY_COUNT: usize = 1;
pub const USER_SPACE_END_EXCLUSIVE: u64 = 0x0000_0080_0000_0000;

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
    NotUserAddress,
    UnmappedUserAddress,
    MappingFailed,
    UnmappingFailed,
    CorruptHierarchy,
    UnsupportedHugePage,
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
    physical_memory_offset: VirtAddr,
}

pub struct AddressSpaceActivation<'a> {
    previous_root: PhysFrame<Size4KiB>,
    previous_flags: Cr3Flags,
    active_root: &'a InactivePageTable,
}

impl AddressSpaceActivation<'_> {
    pub fn active_root_frame(&self) -> PhysFrame<Size4KiB> {
        self.active_root.root_frame
    }
}

impl Drop for AddressSpaceActivation<'_> {
    fn drop(&mut self) {
        unsafe {
            Cr3::write(self.previous_root, self.previous_flags);
        }
    }
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
            physical_memory_offset,
        })
    }

    pub fn root_frame(&self) -> PhysFrame<Size4KiB> {
        self.root_frame
    }

    /// Временно активирует этот корень таблиц страниц.
    ///
    /// Возвращаемый guard автоматически восстанавливает предыдущий CR3.
    ///
    /// # Безопасность
    ///
    /// Код, текущий стек и все данные, к которым выполняется доступ до
    /// уничтожения guard, обязаны быть отображены в новом адресном пространстве.
    pub unsafe fn activate(&self) -> AddressSpaceActivation<'_> {
        let (previous_root, previous_flags) = Cr3::read();
        unsafe {
            Cr3::write(self.root_frame, previous_flags);
        }

        AddressSpaceActivation {
            previous_root,
            previous_flags,
            active_root: self,
        }
    }

    /// Копирует системные P4-записи из активного корня, кроме пользовательского
    /// окна P4[0].
    ///
    /// На текущем загрузочном макете ядро, стек, physical-memory map и служебные
    /// отображения находятся за пределами первых 512 ГиБ. Дочерние таблицы
    /// разделяются с ядром и остаются собственностью системного пространства.
    ///
    /// # Безопасность
    ///
    /// Вызывающая сторона должна гарантировать, что P4[0] не содержит системных
    /// отображений, необходимых во время работы под процессным CR3.
    pub unsafe fn inherit_kernel_mappings(&mut self, physical_memory_offset: VirtAddr) {
        let active = unsafe { active_level_4_table(physical_memory_offset) };
        let target = unsafe { &mut *self.root_table };

        for index in USER_P4_ENTRY_COUNT..512 {
            target[index] = active[index].clone();
        }
    }

    pub fn shared_kernel_mappings_match_active(&self, physical_memory_offset: VirtAddr) -> bool {
        let active = unsafe { active_level_4_table(physical_memory_offset) };
        let target = unsafe { &*self.root_table };

        (USER_P4_ENTRY_COUNT..512).all(|index| {
            target[index].addr() == active[index].addr()
                && target[index].flags() == active[index].flags()
        })
    }

    pub fn user_space_is_empty(&self) -> bool {
        let table = unsafe { &*self.root_table };
        (0..USER_P4_ENTRY_COUNT).all(|index| table[index].is_unused())
    }

    /// Выделяет обнулённую физическую страницу и отображает её в нижней
    /// пользовательской половине адресного пространства.
    ///
    /// Все пользовательские листовые страницы, созданные этим методом,
    /// принадлежат данному адресному пространству и освобождаются
    /// `destroy_user_space`.
    pub fn map_owned_user_4k<A>(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
        allocator: &mut A,
    ) -> Result<PhysFrame<Size4KiB>, InactivePageTableError>
    where
        A: FrameAllocator<Size4KiB> + FrameDeallocator<Size4KiB>,
    {
        if page.start_address().as_u64() >= USER_SPACE_END_EXCLUSIVE {
            return Err(InactivePageTableError::NotUserAddress);
        }

        let frame = allocator
            .allocate_frame()
            .ok_or(InactivePageTableError::NoPhysicalFrame)?;

        let frame_ptr =
            (self.physical_memory_offset + frame.start_address().as_u64()).as_mut_ptr::<u8>();
        unsafe {
            core::ptr::write_bytes(frame_ptr, 0, PAGE_SIZE as usize);
        }

        let allowed = PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        let mapping_flags =
            (flags & allowed) | PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;

        let result = unsafe { self.mapper().map_to(page, frame, mapping_flags, allocator) };

        match result {
            Ok(flush) => {
                flush.ignore();
                Ok(frame)
            }
            Err(_) => {
                unsafe {
                    allocator.deallocate_frame(frame);
                }
                Err(InactivePageTableError::MappingFailed)
            }
        }
    }

    pub fn translate_user_addr(&mut self, address: VirtAddr) -> Option<PhysAddr> {
        if address.as_u64() >= USER_SPACE_END_EXCLUSIVE {
            return None;
        }

        unsafe { self.mapper() }.translate_addr(address)
    }

    pub fn write_user_bytes(
        &mut self,
        start: VirtAddr,
        bytes: &[u8],
    ) -> Result<(), InactivePageTableError> {
        if bytes.is_empty() {
            return Ok(());
        }

        let start_raw = start.as_u64();
        let length =
            u64::try_from(bytes.len()).map_err(|_| InactivePageTableError::NotUserAddress)?;
        let end = start_raw
            .checked_add(length)
            .ok_or(InactivePageTableError::NotUserAddress)?;

        if start_raw >= USER_SPACE_END_EXCLUSIVE || end > USER_SPACE_END_EXCLUSIVE {
            return Err(InactivePageTableError::NotUserAddress);
        }

        let mut copied = 0_usize;
        let mut virtual_address = start_raw;

        while copied < bytes.len() {
            let physical = self
                .translate_user_addr(VirtAddr::new(virtual_address))
                .ok_or(InactivePageTableError::UnmappedUserAddress)?;
            let page_offset = virtual_address % PAGE_SIZE;
            let page_remaining = usize::try_from(PAGE_SIZE - page_offset)
                .map_err(|_| InactivePageTableError::NotUserAddress)?;
            let remaining = bytes.len() - copied;
            let chunk = core::cmp::min(page_remaining, remaining);
            let target = (self.physical_memory_offset + physical.as_u64()).as_mut_ptr::<u8>();

            unsafe {
                core::ptr::copy_nonoverlapping(bytes[copied..].as_ptr(), target, chunk);
            }

            copied += chunk;
            virtual_address += chunk as u64;
        }

        Ok(())
    }

    /// Снимает одно пользовательское отображение и возвращает принадлежащий
    /// процессу физический кадр системному распределителю.
    pub fn unmap_owned_user_4k<A>(
        &mut self,
        page: Page<Size4KiB>,
        allocator: &mut A,
    ) -> Result<(), InactivePageTableError>
    where
        A: FrameDeallocator<Size4KiB>,
    {
        if page.start_address().as_u64() >= USER_SPACE_END_EXCLUSIVE {
            return Err(InactivePageTableError::NotUserAddress);
        }

        let (frame, flush) = unsafe { self.mapper() }
            .unmap(page)
            .map_err(|_| InactivePageTableError::UnmappingFailed)?;
        flush.ignore();

        unsafe {
            allocator.deallocate_frame(frame);
        }

        Ok(())
    }

    /// Освобождает пользовательские листовые страницы и принадлежащие процессу
    /// таблицы P1/P2/P3 внутри выделенного пользовательского P4-окна.
    /// Системные P4-записи за пределами окна не затрагиваются.
    pub fn destroy_user_space<A>(
        &mut self,
        allocator: &mut A,
    ) -> Result<u64, InactivePageTableError>
    where
        A: FrameDeallocator<Size4KiB>,
    {
        let mut released = 0_u64;
        let p4 = unsafe { &mut *self.root_table };

        for p4_index in 0..USER_P4_ENTRY_COUNT {
            if p4[p4_index].is_unused() {
                continue;
            }

            let p3_frame = p4[p4_index]
                .frame()
                .map_err(|_| InactivePageTableError::CorruptHierarchy)?;
            let p3 = unsafe { &mut *self.table_ptr(p3_frame) };

            for p3_index in 0..512 {
                if p3[p3_index].is_unused() {
                    continue;
                }
                if p3[p3_index].flags().contains(PageTableFlags::HUGE_PAGE) {
                    return Err(InactivePageTableError::UnsupportedHugePage);
                }

                let p2_frame = p3[p3_index]
                    .frame()
                    .map_err(|_| InactivePageTableError::CorruptHierarchy)?;
                let p2 = unsafe { &mut *self.table_ptr(p2_frame) };

                for p2_index in 0..512 {
                    if p2[p2_index].is_unused() {
                        continue;
                    }
                    if p2[p2_index].flags().contains(PageTableFlags::HUGE_PAGE) {
                        return Err(InactivePageTableError::UnsupportedHugePage);
                    }

                    let p1_frame = p2[p2_index]
                        .frame()
                        .map_err(|_| InactivePageTableError::CorruptHierarchy)?;
                    let p1 = unsafe { &mut *self.table_ptr(p1_frame) };

                    for p1_index in 0..512 {
                        if p1[p1_index].is_unused() {
                            continue;
                        }

                        let data_frame = p1[p1_index]
                            .frame()
                            .map_err(|_| InactivePageTableError::CorruptHierarchy)?;
                        p1[p1_index].set_unused();
                        unsafe {
                            allocator.deallocate_frame(data_frame);
                        }
                        released += 1;
                    }

                    p2[p2_index].set_unused();
                    unsafe {
                        allocator.deallocate_frame(p1_frame);
                    }
                    released += 1;
                }

                p3[p3_index].set_unused();
                unsafe {
                    allocator.deallocate_frame(p2_frame);
                }
                released += 1;
            }

            p4[p4_index].set_unused();
            unsafe {
                allocator.deallocate_frame(p3_frame);
            }
            released += 1;
        }

        Ok(released)
    }

    unsafe fn mapper(&mut self) -> OffsetPageTable<'_> {
        let root = unsafe { &mut *self.root_table };
        unsafe { OffsetPageTable::new(root, self.physical_memory_offset) }
    }

    unsafe fn table_ptr(&self, frame: PhysFrame<Size4KiB>) -> *mut PageTable {
        (self.physical_memory_offset + frame.start_address().as_u64()).as_mut_ptr::<PageTable>()
    }

    /// Удаляет только разделяемые системные ссылки верхнего уровня.
    ///
    /// Дочерние таблицы не освобождаются: ими владеет системное адресное
    /// пространство.
    pub unsafe fn clear_shared_kernel_mappings(&mut self) {
        let table = unsafe { &mut *self.root_table };
        for index in USER_P4_ENTRY_COUNT..512 {
            table[index].set_unused();
        }
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
