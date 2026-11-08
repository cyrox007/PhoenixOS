//! Административные очереди NVMe через общий механизм закрепления DMA.
//!
//! Только проверочный профиль QEMU: прямые физические адреса допустимы
//! благодаря явно ограниченному окну устройства без IOMMU. Контроллер
//! освобождается до отображений и физических страниц, даже при ошибке команды.

use core::sync::atomic::{Ordering, fence};

use phoenix_block::{BlockDevice, BlockError, BlockGeometry, validate_transfer};
use phoenix_dma::DmaLayout;
use phoenix_dma::mapping::{DeviceMapping, DirectDmaAperture, PhysicalAddress, QemuDirectDma};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::nvme::{self, NvmePciController, RegisterAccess};
use phoenix_pcie::nvme_admin::{
    self, AdminCompletion, AdminQueueCursor, AdminQueueError, IdentifyCommand,
};
use phoenix_pcie::nvme_io::{self, IoCommand, IoError, IoQueueState};
use phoenix_pcie::nvme_wait::{self, WaitError};
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
const DMA_PAGES: u64 = 6;
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
    ControllerNotReady,
    ControllerTimeout,
    CompletionTimeout,
    InvalidQueueParameters,
    Protocol(AdminQueueError),
    InvalidControllerIdentity(u16),
    InvalidNamespace,
    InvalidBlockFormat,
    UnsafeTestDevice,
    DataMismatch,
    Io(IoError),
    Block(BlockError),
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
    pub io_commands: u16,
    pub verified_blocks: u16,
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

fn map_ready_wait(error: WaitError) -> AdminRuntimeError {
    match error {
        WaitError::ControllerFault => AdminRuntimeError::ControllerFault,
        WaitError::ControllerNotReady => AdminRuntimeError::ControllerNotReady,
        WaitError::TimedOut => AdminRuntimeError::ControllerTimeout,
    }
}

fn map_completion_wait(error: WaitError) -> AdminRuntimeError {
    match error {
        WaitError::ControllerFault => AdminRuntimeError::ControllerFault,
        WaitError::ControllerNotReady => AdminRuntimeError::ControllerNotReady,
        WaitError::TimedOut => AdminRuntimeError::CompletionTimeout,
    }
}

fn wait_ready(regs: &RegisterWindow, expected: bool) -> Result<(), AdminRuntimeError> {
    nvme_wait::wait_ready(|| regs.read_u32(nvme_admin::REG_CSTS), expected, POLL_LIMIT)
        .map_err(map_ready_wait)
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

fn submit_admin(
    regs: &RegisterWindow,
    arena: &DmaArena,
    cursor: &mut AdminQueueCursor,
    words: [u32; 16],
) -> Result<(), AdminRuntimeError> {
    const CQ_PAGE: usize = PAGE_SIZE as usize;
    let command_id = cursor.command_id();
    let base = usize::from(cursor.submission_tail()) * 64;
    for (index, word) in words.iter().enumerate() {
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
    let status = nvme_wait::wait_completion_phase(
        || regs.read_u32(nvme_admin::REG_CSTS),
        || arena.read(complete + 12),
        expected_phase,
        POLL_LIMIT,
    )
    .map_err(map_completion_wait)?;
    fence(Ordering::Acquire);
    let completion = [
        arena.read(complete),
        arena.read(complete + 4),
        arena.read(complete + 8),
        status,
    ];
    let result = AdminCompletion::decode(completion, command_id, expected_phase)
        .map_err(AdminRuntimeError::Protocol)?;
    if result.submission_head != cursor.submission_tail() {
        return Err(AdminRuntimeError::Protocol(
            AdminQueueError::InvalidCompletion,
        ));
    }
    cursor.completed();
    fence(Ordering::SeqCst);
    regs.write(
        nvme_admin::REG_DOORBELL + 4,
        u32::from(cursor.completion_head()),
    );
    Ok(())
}

fn execute_identify(
    regs: &RegisterWindow,
    arena: &DmaArena,
    cursor: &mut AdminQueueCursor,
    controller: bool,
) -> Result<(), AdminRuntimeError> {
    const DATA_PAGE: usize = 2 * PAGE_SIZE as usize;
    for word in 0..(PAGE_SIZE as usize / 4) {
        arena.write(DATA_PAGE + word * 4, 0);
    }
    let command = IdentifyCommand::build(
        cursor.command_id(),
        if controller { 0 } else { 1 },
        controller,
        arena.device_start() + DATA_PAGE as u64,
    )
    .map_err(AdminRuntimeError::Protocol)?;
    submit_admin(regs, arena, cursor, command.words)
}

const IO_DATA_PAGE: usize = 5 * PAGE_SIZE as usize;
const BLOCK_BYTES: usize = nvme_io::LOGICAL_BLOCK_BYTES;
const MAX_IO_BYTES: usize = PAGE_SIZE as usize;

/// Отправка команды в уже созданную рабочую очередь NVMe.
/// Вызов допустим только при живых MMIO и DMA-отображениях.
fn submit_io(
    regs: &RegisterWindow,
    arena: &DmaArena,
    cursor: &mut AdminQueueCursor,
    stride: u32,
    command: IoCommand,
) -> Result<(), AdminRuntimeError> {
    const IO_SQ_PAGE: usize = 3 * PAGE_SIZE as usize;
    const IO_CQ_PAGE: usize = 4 * PAGE_SIZE as usize;
    let base = IO_SQ_PAGE + usize::from(cursor.submission_tail()) * 64;
    for (index, word) in command.words.iter().enumerate() {
        arena.write(base + index * 4, *word);
    }
    fence(Ordering::SeqCst);
    cursor.submitted();
    regs.write(
        nvme_admin::REG_DOORBELL + u64::from(stride) * 2,
        u32::from(cursor.submission_tail()),
    );
    let complete = IO_CQ_PAGE + usize::from(cursor.completion_head()) * 16;
    let expected_phase = cursor.completion_phase();
    let status = nvme_wait::wait_completion_phase(
        || regs.read_u32(nvme_admin::REG_CSTS),
        || arena.read(complete + 12),
        expected_phase,
        POLL_LIMIT,
    )
    .map_err(map_completion_wait)?;
    fence(Ordering::Acquire);
    let completion = [
        arena.read(complete),
        arena.read(complete + 4),
        arena.read(complete + 8),
        status,
    ];
    let head = nvme_io::check_completion(completion, cursor.command_id(), expected_phase)
        .map_err(AdminRuntimeError::Io)?;
    if head != cursor.submission_tail() {
        return Err(AdminRuntimeError::Io(IoError::InvalidCompletion));
    }
    cursor.completed();
    fence(Ordering::SeqCst);
    regs.write(
        nvme_admin::REG_DOORBELL + u64::from(stride) * 3,
        u32::from(cursor.completion_head()),
    );
    Ok(())
}

/// NVMe-адаптер к общему блочному контракту. Не владеет DMA/MMIO:
/// его жизнь ограничена работающим контроллером и очередями.
struct NvmeBlockDevice<'a> {
    registers: &'a RegisterWindow,
    dma: &'a DmaArena,
    cursor: AdminQueueCursor,
    geometry: BlockGeometry,
    doorbell_stride: u32,
    queue_state: IoQueueState,
    completed_commands: u16,
}

impl<'a> NvmeBlockDevice<'a> {
    fn new(
        registers: &'a RegisterWindow,
        dma: &'a DmaArena,
        block_count: u64,
        doorbell_stride: u32,
    ) -> Result<Self, AdminRuntimeError> {
        if block_count == 0 {
            return Err(AdminRuntimeError::InvalidNamespace);
        }
        Ok(Self {
            registers,
            dma,
            cursor: AdminQueueCursor::new(nvme_io::IO_QUEUE_DEPTH)
                .map_err(AdminRuntimeError::Protocol)?,
            geometry: BlockGeometry {
                block_size: BLOCK_BYTES,
                block_count,
            },
            doorbell_stride,
            queue_state: IoQueueState::Ready,
            completed_commands: 0,
        })
    }

    fn issue(&mut self, command: IoCommand) -> Result<(), BlockError> {
        // Отказ от повторной команды выполняется самим охранным состоянием:
        // даже при ошибке проверки завершения очередь остаётся изолированной.
        self.queue_state
            .submit(|| {
                submit_io(
                    self.registers,
                    self.dma,
                    &mut self.cursor,
                    self.doorbell_stride,
                    command,
                )
            })
            .map_err(|_| BlockError::BackendFailure)?;
        self.completed_commands = self.completed_commands.wrapping_add(1);
        Ok(())
    }

    fn transfer(&mut self, lba: u64, count: u16, write: bool) -> Result<(), BlockError> {
        let command = IoCommand::transfer(
            self.cursor.command_id(),
            write,
            1,
            lba,
            count,
            self.geometry.block_count,
            self.dma.device_start() + IO_DATA_PAGE as u64,
        )
        .map_err(|_| BlockError::BackendFailure)?;
        self.issue(command)
    }

    fn completed_commands(&self) -> u16 {
        self.completed_commands
    }
}

impl BlockDevice for NvmeBlockDevice<'_> {
    fn geometry(&self) -> BlockGeometry {
        self.geometry
    }

    fn read_blocks(&mut self, first_block: u64, destination: &mut [u8]) -> Result<(), BlockError> {
        validate_transfer(self.geometry, first_block, destination.len())?;
        if !self.queue_state.can_submit() {
            return Err(BlockError::BackendFailure);
        }
        for (offset, chunk) in destination.chunks_mut(MAX_IO_BYTES).enumerate() {
            let block = first_block
                .checked_add((offset * MAX_IO_BYTES / BLOCK_BYTES) as u64)
                .ok_or(BlockError::ArithmeticOverflow)?;
            self.transfer(block, (chunk.len() / BLOCK_BYTES) as u16, false)?;
            for (index, bytes) in chunk.chunks_exact_mut(4).enumerate() {
                bytes.copy_from_slice(&self.dma.read(IO_DATA_PAGE + index * 4).to_le_bytes());
            }
        }
        Ok(())
    }

    fn write_blocks(&mut self, first_block: u64, source: &[u8]) -> Result<(), BlockError> {
        validate_transfer(self.geometry, first_block, source.len())?;
        if !self.queue_state.can_submit() {
            return Err(BlockError::BackendFailure);
        }
        for (offset, chunk) in source.chunks(MAX_IO_BYTES).enumerate() {
            let block = first_block
                .checked_add((offset * MAX_IO_BYTES / BLOCK_BYTES) as u64)
                .ok_or(BlockError::ArithmeticOverflow)?;
            for (index, bytes) in chunk.chunks_exact(4).enumerate() {
                let word =
                    u32::from_le_bytes(bytes.try_into().map_err(|_| BlockError::BackendFailure)?);
                self.dma.write(IO_DATA_PAGE + index * 4, word);
            }
            self.transfer(block, (chunk.len() / BLOCK_BYTES) as u16, true)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        let command = IoCommand::flush(self.cursor.command_id(), 1)
            .map_err(|_| BlockError::BackendFailure)?;
        self.issue(command)
    }
}

/// Проверяем именно общую границу BlockDevice, а не особый путь к DMA.
fn verify_block_device<D: BlockDevice>(
    device: &mut D,
    first_block: u64,
) -> Result<u16, AdminRuntimeError> {
    const TEST_BLOCKS: usize = 10;
    const TEST_BYTES: usize = TEST_BLOCKS * BLOCK_BYTES;

    if device.geometry().block_size != BLOCK_BYTES {
        return Err(AdminRuntimeError::InvalidBlockFormat);
    }
    let mut original = [0_u8; TEST_BYTES];
    let mut observed = [0_u8; TEST_BYTES];

    // Ошибочные запросы должны отклоняться до обращения к контроллеру.
    if device.read_blocks(first_block, &mut [0_u8; BLOCK_BYTES + 1])
        != Err(BlockError::InvalidBufferLength)
        || device.write_blocks(device.geometry().block_count - 1, &[0_u8; BLOCK_BYTES * 2])
            != Err(BlockError::OutOfRange)
        || device.read_blocks(u64::MAX, &mut [0_u8; BLOCK_BYTES])
            != Err(BlockError::ArithmeticOverflow)
    {
        return Err(AdminRuntimeError::DataMismatch);
    }

    device
        .read_blocks(first_block, &mut original)
        .map_err(AdminRuntimeError::Block)?;
    for (index, byte) in observed.iter_mut().enumerate() {
        let block = (index / BLOCK_BYTES) as u8;
        *byte = (index as u8)
            .wrapping_mul(37)
            .wrapping_add(block.wrapping_mul(23))
            .wrapping_add(11);
    }
    device
        .write_blocks(first_block, &observed)
        .map_err(AdminRuntimeError::Block)?;
    device.flush().map_err(AdminRuntimeError::Block)?;
    observed.fill(0);
    device
        .read_blocks(first_block, &mut observed)
        .map_err(AdminRuntimeError::Block)?;
    let mut correct = observed.iter().enumerate().all(|(index, byte)| {
        let block = (index / BLOCK_BYTES) as u8;
        *byte
            == (index as u8)
                .wrapping_mul(37)
                .wrapping_add(block.wrapping_mul(23))
                .wrapping_add(11)
    });

    // Восстановление выполняется даже при ошибке сверки образца.
    device
        .write_blocks(first_block, &original)
        .map_err(AdminRuntimeError::Block)?;
    device.flush().map_err(AdminRuntimeError::Block)?;
    observed.fill(0);
    device
        .read_blocks(first_block, &mut observed)
        .map_err(AdminRuntimeError::Block)?;
    correct &= observed == original;

    // Два дополнительных чтения проверяют повторный оборот колец
    // с изменением фазового бита на 16-м завершении.
    for _ in 0..2 {
        observed.fill(0);
        device
            .read_blocks(first_block, &mut observed)
            .map_err(AdminRuntimeError::Block)?;
        correct &= observed == original;
    }
    device.flush().map_err(AdminRuntimeError::Block)?;
    if !correct {
        return Err(AdminRuntimeError::DataMismatch);
    }
    Ok((4 * TEST_BLOCKS) as u16)
}

fn exercise_blocks(
    regs: &RegisterWindow,
    arena: &DmaArena,
    admin: &mut AdminQueueCursor,
    blocks: u64,
    stride: u32,
) -> Result<(u16, u16), AdminRuntimeError> {
    const TEST_BLOCKS: u64 = 10;
    if blocks < TEST_BLOCKS {
        return Err(AdminRuntimeError::InvalidNamespace);
    }
    let cq = IoCommand::create_completion(admin.command_id(), arena.device_start() + 4 * PAGE_SIZE)
        .map_err(AdminRuntimeError::Io)?;
    submit_admin(regs, arena, admin, cq.words)?;
    let sq = IoCommand::create_submission(admin.command_id(), arena.device_start() + 3 * PAGE_SIZE)
        .map_err(AdminRuntimeError::Io)?;
    submit_admin(regs, arena, admin, sq.words)?;

    let mut device = NvmeBlockDevice::new(regs, arena, blocks, stride)?;
    let verified = verify_block_device(&mut device, blocks - TEST_BLOCKS)?;
    Ok((device.completed_commands(), verified))
}

fn exercise(
    regs: &RegisterWindow,
    arena: &DmaArena,
    expected_vendor: u16,
    max_entries: u32,
    stride: u32,
) -> Result<AdminTestReport, AdminRuntimeError> {
    enable(regs, arena, max_entries)?;
    let mut cursor = AdminQueueCursor::new(QUEUE_DEPTH).map_err(AdminRuntimeError::Protocol)?;
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
    let format_index = (arena.read((2 * PAGE_SIZE + 24) as usize) >> 16) & 0xf;
    let format = arena.read((2 * PAGE_SIZE + 128 + 4 * u64::from(format_index)) as usize);
    if format & 0xffff != 0 || (format >> 16) & 0xff != 9 {
        return Err(AdminRuntimeError::InvalidBlockFormat);
    }
    // Самопроверка с записью допускается только для временного тома QEMU 16 МиБ.
    if blocks != 32_768 {
        return Err(AdminRuntimeError::UnsafeTestDevice);
    }
    let (io_commands, verified_blocks) = exercise_blocks(regs, arena, &mut cursor, blocks, stride)?;
    Ok(AdminTestReport {
        queue_depth: QUEUE_DEPTH,
        completed_commands: 2,
        controller_vendor: vendor,
        namespace_blocks: blocks,
        returned_dma_pages: DMA_PAGES,
        io_commands,
        verified_blocks,
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
    // Запрещаем изменять данные физического SSD и любого другого контроллера.
    if controller.function.vendor_id != 0x1b36 || controller.function.device_id != 0x0010 {
        return Err(AdminRuntimeError::UnsafeTestDevice);
    }
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
    let capabilities =
        nvme::read_capabilities(&registers).map_err(|_| AdminRuntimeError::InvalidQueueParameters);
    let outcome = capabilities.and_then(|cap| {
        if cap.minimum_page_size != PAGE_SIZE
            || cap.doorbell_stride_bytes
                > ((REGISTERS_PAGES * PAGE_SIZE - nvme_admin::REG_DOORBELL - 4) / 3) as u32
            || cap.maximum_queue_entries < u32::from(QUEUE_DEPTH)
        {
            return Err(AdminRuntimeError::InvalidQueueParameters);
        }
        exercise(
            &registers,
            &arena,
            controller.function.vendor_id,
            cap.maximum_queue_entries,
            cap.doorbell_stride_bytes,
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
