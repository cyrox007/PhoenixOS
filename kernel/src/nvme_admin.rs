//! Административные очереди NVMe через общий механизм закрепления DMA.
//!
//! Только проверочный профиль QEMU: прямые физические адреса допустимы
//! благодаря явно ограниченному окну устройства без IOMMU. Контроллер
//! освобождается до отображений и физических страниц, даже при ошибке команды.

use core::sync::atomic::{Ordering, fence};

use phoenix_dma::DmaLayout;
use phoenix_dma::mapping::{DeviceMapping, DirectDmaAperture, PhysicalAddress, QemuDirectDma};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::nvme::{self, NvmePciController, RegisterAccess};
use phoenix_pcie::nvme_admin::{
    self, AdminCompletion, AdminQueueCursor, AdminQueueError, IdentifyCommand,
};
use phoenix_vm::dma_window::{ActiveDmaPageAccess, DmaCachePolicy};
use phoenix_vm::kernel_reservations::{KernelVirtualRanges, ManagedDmaHandle, ManagedDmaWindows};
use phoenix_vm::{ActivePageTable, PAGE_SIZE};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB},
};

const REGISTERS_VIRTUAL_BASE: u64 = 0xffff_a500_0000_0000;
const DMA_VIRTUAL_BASE: u64 = 0xffff_a600_0000_0000;
const REGISTERS_PAGES: u64 = 2;
const DMA_PAGES: u64 = 3;
const POLL_LIMIT: usize = 5_000_000;
const MAX_DEVICE_ADDRESS_EXCLUSIVE: u64 = 0x1_0000_0000;
const QUEUE_DEPTH: u16 = nvme_admin::ADMIN_QUEUE_DEPTH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminRuntimeError {
    MemoryAccessDisabled,
    BusMasterDisabled,
    InvalidPhysicalRange,
    MmioOccupied,
    MmioMapping,
    MmioUnmapping,
    DmaFramesUnavailable,
    DmaAddressUnavailable,
    DmaReservation,
    DmaMapping,
    DmaUnmapping,
    DmaRelease,
    ControllerFault,
    ControllerTimeout,
    CompletionTimeout,
    InvalidQueueParameters,
    Protocol(AdminQueueError),
    InvalidControllerIdentity(u16),
    InvalidNamespace,
    /// Контроллер не подтвердил остановку. DMA-страницы запрещено возвращать.
    UnsafeShutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminTestReport {
    pub queue_depth: u16,
    pub completed_commands: u16,
    pub controller_vendor: u16,
    pub namespace_blocks: u64,
    pub returned_dma_pages: u64,
}

struct RegisterWindow {
    base: VirtAddr,
    mapped_pages: u64,
}

impl RegisterWindow {
    fn map<const N: usize>(
        controller: NvmePciController,
        table: &mut ActivePageTable,
        frames: &mut SystemFrameAllocator<N>,
    ) -> Result<Self, AdminRuntimeError> {
        if !controller.memory_enabled {
            return Err(AdminRuntimeError::MemoryAccessDisabled);
        }
        if !controller.bus_master_enabled {
            return Err(AdminRuntimeError::BusMasterDisabled);
        }
        let end = controller
            .mmio_base
            .checked_add(REGISTERS_PAGES * PAGE_SIZE - 1)
            .ok_or(AdminRuntimeError::InvalidPhysicalRange)?;
        if controller.mmio_base % PAGE_SIZE != 0
            || PhysAddr::try_new(controller.mmio_base).is_err()
            || PhysAddr::try_new(end).is_err()
        {
            return Err(AdminRuntimeError::InvalidPhysicalRange);
        }
        for index in 0..REGISTERS_PAGES {
            let virtual_address = VirtAddr::new(REGISTERS_VIRTUAL_BASE + index * PAGE_SIZE);
            if table.translate_addr(virtual_address).is_some() {
                return Err(AdminRuntimeError::MmioOccupied);
            }
        }

        let mut window = Self {
            base: VirtAddr::new(REGISTERS_VIRTUAL_BASE),
            mapped_pages: 0,
        };
        let flags = PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE
            | PageTableFlags::WRITE_THROUGH;
        for index in 0..REGISTERS_PAGES {
            let address = window.base + index * PAGE_SIZE;
            let page = Page::<Size4KiB>::containing_address(address);
            let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(
                controller.mmio_base + index * PAGE_SIZE,
            ));
            if unsafe { table.map_4k(page, frame, flags, frames) }.is_err() {
                window.unmap(table)?;
                return Err(AdminRuntimeError::MmioMapping);
            }
            window.mapped_pages += 1;
        }
        Ok(window)
    }

    fn write(&self, offset: u64, value: u32) {
        debug_assert!(offset % 4 == 0 && offset + 4 <= REGISTERS_PAGES * PAGE_SIZE);
        let address = (self.base + offset).as_mut_ptr::<u32>();
        unsafe { address.write_volatile(value) };
    }

    fn write_address(&self, offset: u64, value: u64) {
        self.write(offset, value as u32);
        self.write(offset + 4, (value >> 32) as u32);
    }

    fn unmap(&mut self, table: &mut ActivePageTable) -> Result<(), AdminRuntimeError> {
        while self.mapped_pages > 0 {
            let index = self.mapped_pages - 1;
            let page = Page::<Size4KiB>::containing_address(self.base + index * PAGE_SIZE);
            table
                .unmap_4k(page)
                .map_err(|_| AdminRuntimeError::MmioUnmapping)?;
            self.mapped_pages -= 1;
        }
        Ok(())
    }
}

impl RegisterAccess for RegisterWindow {
    fn read_u32(&self, offset: u64) -> u32 {
        debug_assert!(offset % 4 == 0 && offset + 4 <= REGISTERS_PAGES * PAGE_SIZE);
        let address = (self.base + offset).as_ptr::<u32>();
        unsafe { address.read_volatile() }
    }
}

struct DmaArena {
    first_frame: PhysFrame<Size4KiB>,
    physical_start: u64,
    virtual_start: VirtAddr,
    backend: QemuDirectDma<1>,
    mapping: DeviceMapping,
    ranges: KernelVirtualRanges<1>,
    windows: ManagedDmaWindows<1>,
    handle: ManagedDmaHandle,
}

impl DmaArena {
    fn allocate<const N: usize>(
        table: &mut ActivePageTable,
        frames: &mut SystemFrameAllocator<N>,
    ) -> Result<Self, AdminRuntimeError> {
        let first_frame = frames
            .allocate_contiguous_below_4k(DMA_PAGES, 1, u32::MAX as u64)
            .ok_or(AdminRuntimeError::DmaFramesUnavailable)?;
        let physical_start = first_frame.start_address().as_u64();
        let aperture = DirectDmaAperture::new(0, MAX_DEVICE_ADDRESS_EXCLUSIVE)
            .map_err(|_| AdminRuntimeError::DmaAddressUnavailable)?;
        let mut backend = unsafe { QemuDirectDma::<1>::new(aperture) };
        let layout = DmaLayout::new((DMA_PAGES * PAGE_SIZE) as usize, PAGE_SIZE as usize)
            .map_err(|_| AdminRuntimeError::DmaAddressUnavailable)?;
        let mut mapping = match DeviceMapping::establish(
            &mut backend,
            PhysicalAddress::new(physical_start),
            layout,
        ) {
            Ok(mapping) => mapping,
            Err(_) => {
                frames
                    .release_contiguous_4k(first_frame, DMA_PAGES)
                    .map_err(|_| AdminRuntimeError::DmaRelease)?;
                return Err(AdminRuntimeError::DmaAddressUnavailable);
            }
        };
        let mut ranges = KernelVirtualRanges::<1>::new(DMA_VIRTUAL_BASE, DMA_PAGES)
            .map_err(|_| AdminRuntimeError::DmaReservation)?;
        let mut windows = ManagedDmaWindows::<1>::new();
        let handle = match windows.reserve(&mut ranges, physical_start, DMA_PAGES) {
            Ok(handle) => handle,
            Err(_) => {
                mapping
                    .close(&mut backend)
                    .map_err(|_| AdminRuntimeError::DmaUnmapping)?;
                frames
                    .release_contiguous_4k(first_frame, DMA_PAGES)
                    .map_err(|_| AdminRuntimeError::DmaRelease)?;
                return Err(AdminRuntimeError::DmaReservation);
            }
        };
        let virtual_start = VirtAddr::new(
            windows
                .virtual_start(&ranges, handle)
                .map_err(|_| AdminRuntimeError::DmaReservation)?,
        );
        // Ограниченное окно принадлежит только этой службе, обычная RAM
        // имеет совместимый с прямым отображением тип кэширования WriteBack.
        let result = {
            let mut access = unsafe {
                ActiveDmaPageAccess::with_cache_policy(table, frames, DmaCachePolicy::WriteBack)
            };
            windows.map_all(handle, &mut access)
        };
        if result.is_err() {
            let mut access = unsafe {
                ActiveDmaPageAccess::with_cache_policy(table, frames, DmaCachePolicy::WriteBack)
            };
            windows
                .unmap_all(handle, &mut access)
                .map_err(|_| AdminRuntimeError::DmaUnmapping)?;
            windows
                .release(&mut ranges, handle)
                .map_err(|_| AdminRuntimeError::DmaRelease)?;
            mapping
                .close(&mut backend)
                .map_err(|_| AdminRuntimeError::DmaUnmapping)?;
            frames
                .release_contiguous_4k(first_frame, DMA_PAGES)
                .map_err(|_| AdminRuntimeError::DmaRelease)?;
            return Err(AdminRuntimeError::DmaMapping);
        }

        let area = Self {
            first_frame,
            physical_start,
            virtual_start,
            backend,
            mapping,
            ranges,
            windows,
            handle,
        };
        for word in 0..(DMA_PAGES * PAGE_SIZE / 4) {
            area.write(word as usize * 4, 0);
        }
        Ok(area)
    }

    fn device_start(&self) -> u64 {
        self.mapping.device_address().value()
    }

    fn address(&self, byte_offset: usize) -> *mut u32 {
        debug_assert!(byte_offset % 4 == 0 && byte_offset < (DMA_PAGES * PAGE_SIZE) as usize);
        (self.virtual_start + byte_offset as u64).as_mut_ptr::<u32>()
    }

    fn write(&self, byte_offset: usize, value: u32) {
        unsafe { self.address(byte_offset).write_volatile(value) }
    }

    fn read(&self, byte_offset: usize) -> u32 {
        unsafe { self.address(byte_offset).read_volatile() }
    }

    fn release<const N: usize>(
        &mut self,
        table: &mut ActivePageTable,
        frames: &mut SystemFrameAllocator<N>,
    ) -> Result<(), AdminRuntimeError> {
        {
            let mut access = unsafe {
                ActiveDmaPageAccess::with_cache_policy(table, frames, DmaCachePolicy::WriteBack)
            };
            self.windows
                .unmap_all(self.handle, &mut access)
                .map_err(|_| AdminRuntimeError::DmaUnmapping)?;
        }
        self.windows
            .release(&mut self.ranges, self.handle)
            .map_err(|_| AdminRuntimeError::DmaRelease)?;
        self.mapping
            .close(&mut self.backend)
            .map_err(|_| AdminRuntimeError::DmaUnmapping)?;
        frames
            .release_contiguous_4k(self.first_frame, DMA_PAGES)
            .map_err(|_| AdminRuntimeError::DmaRelease)
    }

    fn physical_start(&self) -> u64 {
        self.physical_start
    }
}

fn wait_ready(regs: &RegisterWindow, expected: bool) -> Result<(), AdminRuntimeError> {
    for _ in 0..POLL_LIMIT {
        let status = regs.read_u32(nvme_admin::REG_CSTS);
        if status & nvme_admin::CONTROLLER_FATAL != 0 {
            return Err(AdminRuntimeError::ControllerFault);
        }
        if (status & nvme_admin::CONTROLLER_READY != 0) == expected {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(AdminRuntimeError::ControllerTimeout)
}

fn disable(regs: &RegisterWindow) -> Result<(), AdminRuntimeError> {
    let config = regs.read_u32(nvme_admin::REG_CC);
    if config & nvme_admin::CONTROLLER_ENABLED != 0 {
        regs.write(nvme_admin::REG_CC, config & !nvme_admin::CONTROLLER_ENABLED);
    }
    wait_ready(regs, false)
}

fn enable(
    regs: &RegisterWindow,
    arena: &DmaArena,
    max_entries: u32,
) -> Result<(), AdminRuntimeError> {
    let aqa = nvme_admin::queue_attributes(QUEUE_DEPTH, max_entries)
        .map_err(AdminRuntimeError::Protocol)?;
    let physical = arena.device_start();
    regs.write(nvme_admin::REG_AQA, aqa);
    regs.write_address(nvme_admin::REG_ASQ, physical);
    regs.write_address(nvme_admin::REG_ACQ, physical + PAGE_SIZE);
    fence(Ordering::SeqCst);
    regs.write(nvme_admin::REG_CC, nvme_admin::enabled_configuration());
    wait_ready(regs, true)
}

fn execute_identify(
    regs: &RegisterWindow,
    arena: &DmaArena,
    cursor: &mut AdminQueueCursor,
    controller: bool,
) -> Result<(), AdminRuntimeError> {
    const SQ_PAGE: usize = 0;
    const CQ_PAGE: usize = PAGE_SIZE as usize;
    const DATA_PAGE: usize = 2 * PAGE_SIZE as usize;

    for word in 0..(PAGE_SIZE as usize / 4) {
        arena.write(DATA_PAGE + word * 4, 0);
    }
    let command_id = cursor.command_id();
    let command = IdentifyCommand::build(
        command_id,
        if controller { 0 } else { 1 },
        controller,
        arena.device_start() + DATA_PAGE as u64,
    )
    .map_err(AdminRuntimeError::Protocol)?;
    let base = SQ_PAGE + usize::from(cursor.submission_tail()) * 64;
    for (index, word) in command.words.iter().enumerate() {
        arena.write(base + index * 4, *word);
    }
    fence(Ordering::SeqCst);
    cursor.submitted();
    regs.write(
        nvme_admin::REG_DOORBELL,
        u32::from(cursor.submission_tail()),
    );

    let complete = CQ_PAGE + usize::from(cursor.completion_head()) * 16;
    let expected_phase = cursor.completion_phase();
    let completion = (0..POLL_LIMIT)
        .find_map(|_| {
            let status = arena.read(complete + 12);
            if (status >> 16) & 1 != u32::from(expected_phase) {
                core::hint::spin_loop();
                return None;
            }
            fence(Ordering::Acquire);
            Some([
                arena.read(complete),
                arena.read(complete + 4),
                arena.read(complete + 8),
                status,
            ])
        })
        .ok_or(AdminRuntimeError::CompletionTimeout)?;
    let result = AdminCompletion::decode(completion, command_id, expected_phase)
        .map_err(AdminRuntimeError::Protocol)?;
    if result.submission_head != cursor.submission_tail() {
        return Err(AdminRuntimeError::Protocol(AdminQueueError::InvalidCompletion));
    }
    // Оповещаем устройство об обработанном завершении до новой команды.
    cursor.completed();
    fence(Ordering::SeqCst);
    regs.write(
        nvme_admin::REG_DOORBELL + 4,
        u32::from(cursor.completion_head()),
    );
    Ok(())
}

fn exercise(
    regs: &RegisterWindow,
    arena: &DmaArena,
    expected_vendor: u16,
    max_entries: u32,
) -> Result<AdminTestReport, AdminRuntimeError> {
    enable(regs, arena, max_entries)?;
    let mut cursor = AdminQueueCursor::new(QUEUE_DEPTH)
        .map_err(AdminRuntimeError::Protocol)?;
    execute_identify(regs, arena, &mut cursor, true)?;
    let vendor = arena.read((2 * PAGE_SIZE) as usize) as u16;
    if vendor != expected_vendor {
        return Err(AdminRuntimeError::InvalidControllerIdentity(vendor));
    }
    execute_identify(regs, arena, &mut cursor, false)?;
    let lower = u64::from(arena.read((2 * PAGE_SIZE) as usize));
    let upper = u64::from(arena.read((2 * PAGE_SIZE) as usize + 4));
    let blocks = lower | (upper << 32);
    if blocks == 0 {
        return Err(AdminRuntimeError::InvalidNamespace);
    }
    Ok(AdminTestReport {
        queue_depth: QUEUE_DEPTH,
        completed_commands: 2,
        controller_vendor: vendor,
        namespace_blocks: blocks,
        returned_dma_pages: DMA_PAGES,
    })
}

/// Полный цикл закрепления, программирования, двух аппаратных команд,
/// подтверждённой остановки NVMe и освобождения физических страниц.
///
/// # Безопасность
///
/// Только проверенный профиль QEMU без IOMMU: BAR устройства эксклюзивен,
/// DMA использует прямую адресацию ниже 4 ГиБ, процессорские отображения
/// обычной RAM используют совместимый тип памяти и другие ядра/драйверы
/// не управляют контроллером во время этого теста.
pub unsafe fn self_test<const N: usize>(
    controller: NvmePciController,
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
) -> Result<AdminTestReport, AdminRuntimeError> {
    let mut registers = RegisterWindow::map(controller, table, frames)?;
    // Предыдущее состояние UEFI не означает, что можно переназначить
    // очереди: контроллер должен подтвердить RDY=0 до выдачи нового DMA.
    if let Err(error) = disable(&registers) {
        // Если контроллер ещё работает, MMIO оставляем действующим.
        if registers.read_u32(nvme_admin::REG_CSTS) & nvme_admin::CONTROLLER_READY != 0 {
            return Err(AdminRuntimeError::UnsafeShutdown);
        }
        registers.unmap(table)?;
        return Err(error);
    }

    let mut arena = match DmaArena::allocate(table, frames) {
        Ok(area) => area,
        Err(error) => {
            registers.unmap(table)?;
            return Err(error);
        }
    };
    let capabilities = nvme::read_capabilities(&registers)
        .map_err(|_| AdminRuntimeError::InvalidQueueParameters);
    let outcome = capabilities.and_then(|cap| {
        if cap.minimum_page_size != PAGE_SIZE
            || cap.doorbell_stride_bytes > (PAGE_SIZE - 4) as u32
            || cap.maximum_queue_entries < u32::from(QUEUE_DEPTH)
        {
            return Err(AdminRuntimeError::InvalidQueueParameters);
        }
        exercise(
            &registers,
            &arena,
            controller.function.vendor_id,
            cap.maximum_queue_entries,
        )
    });

    // В любом случае, включая неверный ответ Identify, проверяем RDY=0.
    // При отказе аппаратной остановки запрещено отпускать память устройства.
    if disable(&registers).is_err() {
        return Err(AdminRuntimeError::UnsafeShutdown);
    }
    let before = frames.free_frames();
    arena.release(table, frames)?;
    if frames.free_frames() != before + DMA_PAGES {
        return Err(AdminRuntimeError::DmaRelease);
    }
    registers.unmap(table)?;
    let report = outcome?;
    assert_eq!(arena.backend.active_count(), 0);
    assert_eq!(arena.physical_start() % PAGE_SIZE, 0);
    Ok(report)
}
