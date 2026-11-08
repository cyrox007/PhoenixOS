//! Рабочий двухмерный драйвер VirtIO-GPU для проверочного QEMU.
//!
//! Реально создаёт ресурс, передаёт пиксели устройству и обновляет scanout.
//! Никаких операций на физической AMD/NVIDIA. После проверки контроллер
//! возвращается в сброшенное состояние перед освобождением DMA-памяти.

use core::sync::atomic::{Ordering, fence};

use phoenix_dma::DmaLayout;
use phoenix_dma::mapping::{DeviceMapping, DirectDmaAperture, PhysicalAddress, QemuDirectDma};
use phoenix_driver_registry::{
    ContextRegisteredDriver, DriverDescriptor, DriverLaunch, DriverRegistry, DriverRegistryError,
    DriverStartError, DriverVersion, PcieMatchRule,
};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::graphics::{GraphicsController, GraphicsProfile, VIRTIO_GPU_DEVICE_ID, VIRTIO_VENDOR_ID};
use phoenix_pcie::virtio_gpu_2d::{
    self as gpu, GpuProtocolError, GpuRequest, RESP_OK_DISPLAY_INFO, RESP_OK_NODATA,
};
use phoenix_pcie::virtio_pci::{self, VirtioPciError, VirtioPciLayout};
use phoenix_pcie::{ConfigAccess, PcieFunction};
use phoenix_vm::dma_window::{ActiveDmaPageAccess, DmaCachePolicy};
use phoenix_vm::kernel_reservations::{KernelVirtualRanges, ManagedDmaHandle, ManagedDmaWindows};
use phoenix_vm::{ActivePageTable, PAGE_SIZE};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB},
};

const MMIO_VIRTUAL_BASE: u64 = 0xffff_ac00_0000_0000;
const DMA_VIRTUAL_BASE: u64 = 0xffff_ad00_0000_0000;
const MMIO_PAGES: u64 = 4;
const IMAGE_WIDTH: u32 = 128;
const IMAGE_HEIGHT: u32 = 128;
const IMAGE_BYTES: usize = IMAGE_WIDTH as usize * IMAGE_HEIGHT as usize * 4;
const DMA_PAGES: u64 = 4 + (IMAGE_BYTES as u64 / PAGE_SIZE);
const IMAGE_OFFSET: usize = 4 * 4096;
const COMMAND_OFFSET: usize = 3 * 4096;
const RESPONSE_OFFSET: usize = COMMAND_OFFSET + 1024;
const QUEUE_DEPTH: u16 = 8;
const POLL_LIMIT: usize = 5_000_000;
const DEVICE_MAX_ADDRESS: u64 = 0x1_0000_0000;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuRuntimeError {
    UnsafeDevice,
    Pci(VirtioPciError),
    Registry(DriverRegistryError),
    MissingReport,
    InvalidMmio,
    MmioOccupied,
    MmioMapping,
    MmioUnmapping,
    DmaUnavailable,
    DmaMapping,
    DmaRelease,
    DeviceMapping,
    InvalidFeature,
    InvalidQueue,
    InvalidStatus,
    DeviceTimedOut,
    InvalidCompletion,
    DeviceCommand(GpuProtocolError),
    InvalidDisplayMode,
    InvalidPixels,
    UnsafeShutdown,
}

impl From<VirtioPciError> for GpuRuntimeError {
    fn from(error: VirtioPciError) -> Self {
        Self::Pci(error)
    }
}

impl From<GpuProtocolError> for GpuRuntimeError {
    fn from(error: GpuProtocolError) -> Self {
        Self::DeviceCommand(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuReport {
    pub width: u32,
    pub height: u32,
    pub display_width: u32,
    pub display_height: u32,
    pub completed_commands: u16,
    pub returned_dma_pages: u64,
    pub pixel_checksum: u64,
}

struct PciRegisters {
    physical_memory_offset: VirtAddr,
}

impl ConfigAccess for PciRegisters {
    fn read_u32(&self, address: u64) -> u32 {
        let pointer = (self.physical_memory_offset + address).as_ptr::<u32>();
        unsafe { pointer.read_volatile() }
    }
}

impl PciRegisters {
    fn command(&self, function: PcieFunction) -> u16 {
        self.read_u32(function.config_address + 4) as u16
    }

    fn write_command(&self, function: PcieFunction, command: u16) {
        let address = self.physical_memory_offset + function.config_address + 4;
        unsafe { address.as_mut_ptr::<u16>().write_volatile(command) };
    }
}

struct MmioWindow {
    base: VirtAddr,
    pages: u64,
}

impl MmioWindow {
    fn map<const N: usize>(
        physical: u64,
        table: &mut ActivePageTable,
        frames: &mut SystemFrameAllocator<N>,
    ) -> Result<Self, GpuRuntimeError> {
        let end = physical
            .checked_add(MMIO_PAGES * PAGE_SIZE - 1)
            .ok_or(GpuRuntimeError::InvalidMmio)?;
        if physical & (PAGE_SIZE - 1) != 0
            || PhysAddr::try_new(physical).is_err()
            || PhysAddr::try_new(end).is_err()
        {
            return Err(GpuRuntimeError::InvalidMmio);
        }
        let base = VirtAddr::new(MMIO_VIRTUAL_BASE);
        for index in 0..MMIO_PAGES {
            if table.translate_addr(base + index * PAGE_SIZE).is_some() {
                return Err(GpuRuntimeError::MmioOccupied);
            }
        }
        let mut window = Self { base, pages: 0 };
        let flags = PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE
            | PageTableFlags::WRITE_THROUGH;
        for index in 0..MMIO_PAGES {
            let virtual_page = Page::<Size4KiB>::containing_address(base + index * PAGE_SIZE);
            let physical_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(
                physical + index * PAGE_SIZE,
            ));
            if unsafe { table.map_4k(virtual_page, physical_frame, flags, frames) }.is_err() {
                window.unmap(table)?;
                return Err(GpuRuntimeError::MmioMapping);
            }
            window.pages += 1;
        }
        Ok(window)
    }

    fn read_u8(&self, offset: u64) -> u8 {
        unsafe { (self.base + offset).as_ptr::<u8>().read_volatile() }
    }

    fn read_u16(&self, offset: u64) -> u16 {
        unsafe { (self.base + offset).as_ptr::<u16>().read_volatile() }
    }

    fn read_u32(&self, offset: u64) -> u32 {
        unsafe { (self.base + offset).as_ptr::<u32>().read_volatile() }
    }

    fn write_u8(&self, offset: u64, value: u8) {
        unsafe { (self.base + offset).as_mut_ptr::<u8>().write_volatile(value) };
    }

    fn write_u16(&self, offset: u64, value: u16) {
        unsafe { (self.base + offset).as_mut_ptr::<u16>().write_volatile(value) };
    }

    fn write_u32(&self, offset: u64, value: u32) {
        unsafe { (self.base + offset).as_mut_ptr::<u32>().write_volatile(value) };
    }

    fn write_u64(&self, offset: u64, value: u64) {
        self.write_u32(offset, value as u32);
        self.write_u32(offset + 4, (value >> 32) as u32);
    }

    fn unmap(&mut self, table: &mut ActivePageTable) -> Result<(), GpuRuntimeError> {
        while self.pages > 0 {
            let index = self.pages - 1;
            let page = Page::<Size4KiB>::containing_address(self.base + index * PAGE_SIZE);
            table.unmap_4k(page).map_err(|_| GpuRuntimeError::MmioUnmapping)?;
            self.pages -= 1;
        }
        Ok(())
    }
}

struct DmaArena {
    first_frame: PhysFrame<Size4KiB>,
    physical: u64,
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
    ) -> Result<Self, GpuRuntimeError> {
        let first_frame = frames
            .allocate_contiguous_below_4k(DMA_PAGES, 1, u32::MAX as u64)
            .ok_or(GpuRuntimeError::DmaUnavailable)?;
        let physical = first_frame.start_address().as_u64();
        let aperture = DirectDmaAperture::new(0, DEVICE_MAX_ADDRESS)
            .map_err(|_| GpuRuntimeError::DeviceMapping)?;
        let mut backend = unsafe { QemuDirectDma::<1>::new(aperture) };
        let layout = DmaLayout::new((DMA_PAGES * PAGE_SIZE) as usize, PAGE_SIZE as usize)
            .map_err(|_| GpuRuntimeError::DeviceMapping)?;
        let mut mapping = match DeviceMapping::establish(
            &mut backend,
            PhysicalAddress::new(physical),
            layout,
        ) {
            Ok(mapping) => mapping,
            Err(_) => {
                frames
                    .release_contiguous_4k(first_frame, DMA_PAGES)
                    .map_err(|_| GpuRuntimeError::DmaRelease)?;
                return Err(GpuRuntimeError::DeviceMapping);
            }
        };
        let mut ranges = KernelVirtualRanges::<1>::new(DMA_VIRTUAL_BASE, DMA_PAGES)
            .map_err(|_| GpuRuntimeError::DmaMapping)?;
        let mut windows = ManagedDmaWindows::<1>::new();
        let handle = match windows.reserve(&mut ranges, physical, DMA_PAGES) {
            Ok(handle) => handle,
            Err(_) => {
                mapping.close(&mut backend).map_err(|_| GpuRuntimeError::DeviceMapping)?;
                frames
                    .release_contiguous_4k(first_frame, DMA_PAGES)
                    .map_err(|_| GpuRuntimeError::DmaRelease)?;
                return Err(GpuRuntimeError::DmaMapping);
            }
        };
        let virtual_start = VirtAddr::new(
            windows.virtual_start(&ranges, handle).map_err(|_| GpuRuntimeError::DmaMapping)?,
        );
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
            windows.unmap_all(handle, &mut access).map_err(|_| GpuRuntimeError::DmaMapping)?;
            windows.release(&mut ranges, handle).map_err(|_| GpuRuntimeError::DmaRelease)?;
            mapping.close(&mut backend).map_err(|_| GpuRuntimeError::DeviceMapping)?;
            frames
                .release_contiguous_4k(first_frame, DMA_PAGES)
                .map_err(|_| GpuRuntimeError::DmaRelease)?;
            return Err(GpuRuntimeError::DmaMapping);
        }
        let arena = Self {
            first_frame,
            physical,
            virtual_start,
            backend,
            mapping,
            ranges,
            windows,
            handle,
        };
        for index in 0..DMA_PAGES * PAGE_SIZE / 4 {
            arena.write_u32(index as usize * 4, 0);
        }
        Ok(arena)
    }

    fn device_address(&self, offset: usize) -> u64 {
        self.mapping.device_address().value() + offset as u64
    }

    fn ptr<T>(&self, offset: usize) -> *mut T {
        (self.virtual_start + offset as u64).as_mut_ptr::<T>()
    }

    fn write_u8(&self, offset: usize, value: u8) {
        unsafe { self.ptr::<u8>(offset).write_volatile(value) }
    }

    fn read_u8(&self, offset: usize) -> u8 {
        unsafe { self.ptr::<u8>(offset).read_volatile() }
    }

    fn write_u16(&self, offset: usize, value: u16) {
        unsafe { self.ptr::<u16>(offset).write_volatile(value.to_le()) }
    }

    fn read_u16(&self, offset: usize) -> u16 {
        u16::from_le(unsafe { self.ptr::<u16>(offset).read_volatile() })
    }

    fn write_u32(&self, offset: usize, value: u32) {
        unsafe { self.ptr::<u32>(offset).write_volatile(value.to_le()) }
    }

    fn read_u32(&self, offset: usize) -> u32 {
        u32::from_le(unsafe { self.ptr::<u32>(offset).read_volatile() })
    }

    fn write_u64(&self, offset: usize, value: u64) {
        self.write_u32(offset, value as u32);
        self.write_u32(offset + 4, (value >> 32) as u32);
    }

    fn release<const N: usize>(
        &mut self,
        table: &mut ActivePageTable,
        frames: &mut SystemFrameAllocator<N>,
    ) -> Result<(), GpuRuntimeError> {
        {
            let mut access = unsafe {
                ActiveDmaPageAccess::with_cache_policy(table, frames, DmaCachePolicy::WriteBack)
            };
            self.windows
                .unmap_all(self.handle, &mut access)
                .map_err(|_| GpuRuntimeError::DmaMapping)?;
        }
        self.windows
            .release(&mut self.ranges, self.handle)
            .map_err(|_| GpuRuntimeError::DmaRelease)?;
        self.mapping
            .close(&mut self.backend)
            .map_err(|_| GpuRuntimeError::DeviceMapping)?;
        frames
            .release_contiguous_4k(self.first_frame, DMA_PAGES)
            .map_err(|_| GpuRuntimeError::DmaRelease)
    }
}

struct ControlQueue {
    used_index: u16,
    submitted: u16,
    notify_offset: u64,
}

impl ControlQueue {
    fn start(
        registers: &MmioWindow,
        layout: VirtioPciLayout,
        arena: &DmaArena,
    ) -> Result<Self, GpuRuntimeError> {
        let common = u64::from(layout.common.offset);

        // Сброс не должен оставлять висящие ссылки устройства на память.
        registers.write_u8(common + 0x14, 0);
        wait_reset(registers, common)?;
        registers.write_u8(common + 0x14, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        registers.write_u32(common, 1);
        if registers.read_u32(common + 4) & 1 == 0 {
            return Err(GpuRuntimeError::InvalidFeature);
        }
        registers.write_u32(common, 0);
        registers.write_u32(common + 8, 0);
        registers.write_u32(common + 12, 0);
        registers.write_u32(common + 8, 1);
        registers.write_u32(common + 12, 1);
        registers.write_u8(
            common + 0x14,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );
        if registers.read_u8(common + 0x14) & STATUS_FEATURES_OK == 0 {
            return Err(GpuRuntimeError::InvalidFeature);
        }

        let scanouts = registers.read_u32(u64::from(layout.device.offset) + 8);
        if scanouts == 0 {
            return Err(GpuRuntimeError::InvalidDisplayMode);
        }

        registers.write_u16(common + 0x16, 0);
        let max_queue = registers.read_u16(common + 0x18);
        if max_queue < QUEUE_DEPTH || registers.read_u16(common + 0x1c) != 0 {
            return Err(GpuRuntimeError::InvalidQueue);
        }
        registers.write_u16(common + 0x18, QUEUE_DEPTH);
        registers.write_u16(common + 0x1a, u16::MAX);
        let notify = registers.read_u16(common + 0x1e);
        let notify_offset = layout.notify_address(notify)?;
        // Три независимо выровненные страницы: дескрипторы / доступные / завершённые.
        registers.write_u64(common + 0x20, arena.device_address(0));
        registers.write_u64(common + 0x28, arena.device_address(4096));
        registers.write_u64(common + 0x30, arena.device_address(8192));
        registers.write_u16(common + 0x1c, 1);
        registers.write_u8(
            common + 0x14,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );
        if registers.read_u8(common + 0x14) & STATUS_FAILED != 0 {
            return Err(GpuRuntimeError::InvalidStatus);
        }
        arena.write_u16(4096, 1); // VIRTQ_AVAIL_F_NO_INTERRUPT
        fence(Ordering::SeqCst);
        Ok(Self {
            used_index: 0,
            submitted: 0,
            notify_offset,
        })
    }

    fn submit(
        &mut self,
        registers: &MmioWindow,
        arena: &DmaArena,
        request: GpuRequest,
        expected_response: u32,
    ) -> Result<usize, GpuRuntimeError> {
        for (index, byte) in request.bytes().iter().enumerate() {
            arena.write_u8(COMMAND_OFFSET + index, *byte);
        }
        // Не принимаем повторно прежний ответ.
        for index in 0..1024 {
            arena.write_u8(RESPONSE_OFFSET + index, 0);
        }

        // Пара дескрипторов: запрос гостя, затем буфер записи устройством.
        arena.write_u64(0, arena.device_address(COMMAND_OFFSET));
        arena.write_u32(8, request.bytes().len() as u32);
        arena.write_u16(12, 1);
        arena.write_u16(14, 1);
        arena.write_u64(16, arena.device_address(RESPONSE_OFFSET));
        arena.write_u32(24, 1024);
        arena.write_u16(28, 2);
        arena.write_u16(30, 0);

        let slot = usize::from(self.submitted % QUEUE_DEPTH);
        arena.write_u16(4096 + 4 + slot * 2, 0);
        fence(Ordering::SeqCst);
        self.submitted = self.submitted.wrapping_add(1);
        arena.write_u16(4096 + 2, self.submitted);
        fence(Ordering::SeqCst);
        registers.write_u16(self.notify_offset, 0);

        let expected_index = self.used_index.wrapping_add(1);
        for _ in 0..POLL_LIMIT {
            fence(Ordering::Acquire);
            if arena.read_u16(8192 + 2) == expected_index {
                let slot = usize::from(self.used_index % QUEUE_DEPTH);
                let id = arena.read_u32(8192 + 4 + slot * 8);
                let length = arena.read_u32(8192 + 8 + slot * 8) as usize;
                if id != 0 || length < gpu::CONTROL_HEADER_LEN || length > 1024 {
                    return Err(GpuRuntimeError::InvalidCompletion);
                }
                self.used_index = expected_index;
                let mut response = [0_u8; 48];
                for (index, byte) in response.iter_mut().enumerate() {
                    *byte = arena.read_u8(RESPONSE_OFFSET + index);
                }
                gpu::check_response(&response, length, expected_response)?;
                return Ok(length);
            }
            core::hint::spin_loop();
        }
        Err(GpuRuntimeError::DeviceTimedOut)
    }
}

fn wait_reset(registers: &MmioWindow, common: u64) -> Result<(), GpuRuntimeError> {
    for _ in 0..POLL_LIMIT {
        if registers.read_u8(common + 0x14) == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(GpuRuntimeError::UnsafeShutdown)
}

/// Только проверочный профиль виртуального видеоконтроллера QEMU.
#[inline(never)]
fn exercise(
    registers: &MmioWindow,
    layout: VirtioPciLayout,
    arena: &DmaArena,
) -> Result<GpuReport, GpuRuntimeError> {
    let mut queue = ControlQueue::start(registers, layout, arena)?;

    let used_length = queue.submit(
        registers,
        arena,
        GpuRequest::display_info(),
        RESP_OK_DISPLAY_INFO,
    )?;
    if used_length < 48 {
        return Err(GpuRuntimeError::InvalidDisplayMode);
    }
    let mut response = [0_u8; 48];
    for (index, byte) in response.iter_mut().enumerate() {
        *byte = arena.read_u8(RESPONSE_OFFSET + index);
    }
    let display = gpu::first_display_mode(&response)?;
    if display.width < IMAGE_WIDTH || display.height < IMAGE_HEIGHT {
        return Err(GpuRuntimeError::InvalidDisplayMode);
    }

    queue.submit(
        registers,
        arena,
        GpuRequest::create_2d(IMAGE_WIDTH, IMAGE_HEIGHT)?,
        RESP_OK_NODATA,
    )?;
    queue.submit(
        registers,
        arena,
        GpuRequest::attach_backing(arena.device_address(IMAGE_OFFSET), IMAGE_BYTES as u32)?,
        RESP_OK_NODATA,
    )?;

    let checksum = paint_image(arena);
    queue.submit(
        registers,
        arena,
        GpuRequest::scanout(IMAGE_WIDTH, IMAGE_HEIGHT, true),
        RESP_OK_NODATA,
    )?;
    queue.submit(
        registers,
        arena,
        GpuRequest::transfer(IMAGE_WIDTH, IMAGE_HEIGHT),
        RESP_OK_NODATA,
    )?;
    queue.submit(
        registers,
        arena,
        GpuRequest::flush(IMAGE_WIDTH, IMAGE_HEIGHT),
        RESP_OK_NODATA,
    )?;

    // Временно удерживаем отрисованный ресурс в устройстве во время
    // подтверждения вывода; перед возвратом DMA удаляем его явно.
    queue.submit(
        registers,
        arena,
        GpuRequest::scanout(IMAGE_WIDTH, IMAGE_HEIGHT, false),
        RESP_OK_NODATA,
    )?;
    queue.submit(
        registers,
        arena,
        GpuRequest::detach_backing(),
        RESP_OK_NODATA,
    )?;
    queue.submit(registers, arena, GpuRequest::unref(), RESP_OK_NODATA)?;

    Ok(GpuReport {
        width: IMAGE_WIDTH,
        height: IMAGE_HEIGHT,
        display_width: display.width,
        display_height: display.height,
        completed_commands: queue.submitted,
        returned_dma_pages: DMA_PAGES,
        pixel_checksum: checksum,
    })
}

fn paint_image(arena: &DmaArena) -> u64 {
    let mut checksum = 0_u64;
    for y in 0..IMAGE_HEIGHT as usize {
        for x in 0..IMAGE_WIDTH as usize {
            let border = x < 4 || y < 4 || x >= IMAGE_WIDTH as usize - 4
                || y >= IMAGE_HEIGHT as usize - 4;
            let diagonal = x.abs_diff(y) < 3;
            let color = if border || diagonal {
                0x00ff_ffff
            } else if y < IMAGE_HEIGHT as usize / 3 {
                0x0018_1f2e
            } else if y < IMAGE_HEIGHT as usize * 2 / 3 {
                0x00e5_5e36
            } else {
                0x002c_ae98
            };
            arena.write_u32(IMAGE_OFFSET + (y * IMAGE_WIDTH as usize + x) * 4, color);
            checksum = checksum.wrapping_add(u64::from(color));
        }
    }
    fence(Ordering::SeqCst);
    checksum
}

#[inline(never)]
fn run<const N: usize>(
    controller: GraphicsController,
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
) -> Result<GpuReport, GpuRuntimeError> {
    if controller.profile != GraphicsProfile::VirtioGpu
        || controller.function.vendor_id != VIRTIO_VENDOR_ID
        || controller.function.device_id != VIRTIO_GPU_DEVICE_ID
    {
        return Err(GpuRuntimeError::UnsafeDevice);
    }
    let pci = PciRegisters {
        physical_memory_offset,
    };
    let layout = virtio_pci::discover(&pci, controller.function)?;
    let original_command = pci.command(controller.function);
    pci.write_command(controller.function, original_command | 6);
    if pci.command(controller.function) & 6 != 6 {
        return Err(GpuRuntimeError::UnsafeDevice);
    }

    let mut registers = match MmioWindow::map(layout.mmio_base, table, frames) {
        Ok(registers) => registers,
        Err(error) => {
            pci.write_command(controller.function, original_command);
            return Err(error);
        }
    };
    let mut arena = match DmaArena::allocate(table, frames) {
        Ok(arena) => arena,
        Err(error) => {
            registers.unmap(table)?;
            pci.write_command(controller.function, original_command);
            return Err(error);
        }
    };

    let result = exercise(&registers, layout, &arena);
    // Сброс обязан завершиться до снятия любого отображения и освобождения
    // физических страниц, даже если устройство вернуло ошибку команды.
    let common = u64::from(layout.common.offset);
    registers.write_u8(common + 0x14, 0);
    if wait_reset(&registers, common).is_err() {
        return Err(GpuRuntimeError::UnsafeShutdown);
    }
    let before = frames.free_frames();
    arena.release(table, frames)?;
    if frames.free_frames() != before + DMA_PAGES {
        return Err(GpuRuntimeError::DmaRelease);
    }
    registers.unmap(table)?;
    pci.write_command(controller.function, original_command);
    result
}

struct StartContext<'a, const N: usize> {
    controller: GraphicsController,
    table: &'a mut ActivePageTable,
    frames: &'a mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
    report: Option<GpuReport>,
    failure: Option<GpuRuntimeError>,
}

fn start_driver<const N: usize>(
    function: PcieFunction,
    context: &mut StartContext<'_, N>,
) -> Result<(), DriverStartError> {
    if function != context.controller.function {
        return Err(DriverStartError::UnsupportedDevice);
    }
    match run(
        context.controller,
        context.table,
        context.frames,
        context.physical_memory_offset,
    ) {
        Ok(report) => {
            context.report = Some(report);
            Ok(())
        }
        Err(error) => {
            context.failure = Some(error);
            Err(DriverStartError::InitializationFailed)
        }
    }
}

/// Запускает действительный графический драйвер через единый реестр.
/// После полной проверки возвращает управление UEFI (переход к постоянному
/// владению экраном и компоновщику — отдельные пункты плана).
///
/// # Безопасность
///
/// Только профиль QEMU/KVM с непосредственной адресацией DMA без IOMMU.
/// Доступ к виртуальному MMIO эксклюзивен; реальное оборудование запрещено.
pub unsafe fn self_test<const N: usize>(
    controller: GraphicsController,
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
) -> Result<(DriverLaunch, GpuReport), GpuRuntimeError> {
    let mut registry =
        DriverRegistry::<1, ContextRegisteredDriver<StartContext<'_, N>>>::new();
    registry
        .register(ContextRegisteredDriver {
            descriptor: DriverDescriptor::new(
                "virtio-gpu-2d",
                DriverVersion::new(0, 3, 0),
                100,
                PcieMatchRule {
                    vendor_id: Some(VIRTIO_VENDOR_ID),
                    device_id: Some(VIRTIO_GPU_DEVICE_ID),
                    class_code: Some(0x03),
                    ..PcieMatchRule::any()
                },
            ),
            start: start_driver::<N>,
        })
        .map_err(GpuRuntimeError::Registry)?;
    let mut context = StartContext {
        controller,
        table,
        frames,
        physical_memory_offset,
        report: None,
        failure: None,
    };
    let launch = registry.start_best_with_context(controller.function, &mut context);
    if let Some(error) = context.failure {
        return Err(error);
    }
    let launch = launch.map_err(GpuRuntimeError::Registry)?;
    let report = context.report.ok_or(GpuRuntimeError::MissingReport)?;
    Ok((launch, report))
}
