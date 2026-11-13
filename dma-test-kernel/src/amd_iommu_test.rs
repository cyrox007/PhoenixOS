//! Настоящая проверка AMD-Vi, но только на виртуальном устройстве QEMU.
//! Физические контроллеры ASUS и любые диски для этого образа запрещены.
//! Проверяются положительное преобразование IOVA и отрицательная передача
//! на соседнюю НЕ отображённую страницу.

use core::sync::atomic::{Ordering, fence};

use phoenix_dma::amd_iommu_v1::{
    AmdIommuIo, AmdIommuPages, AmdV1Engine, StoppedDevice,
};
use phoenix_dma::iommu_domain::IommuEngine;
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::PcieFunction;
use phoenix_reference_edu as edu;
use phoenix_vm::ActivePageTable;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB},
};

const IOMMU_QEMU_BASE: u64 = 0xfed8_0000;
const IOMMU_MAP: u64 = 0xffff_ac00_1000_0000;
const EDU_MAP: u64 = 0xffff_ac00_2000_0000;
const PAGE: u64 = 4096;
const DMA_MASK: u64 = 0x0fff_ffff;
const POLL_COUNT: usize = 10_000_000;
const SENTINEL: u8 = 0xa5;

struct MMIO {
    base: VirtAddr,
}

impl MMIO {
    fn read64(&self, register: u64) -> u64 {
        unsafe { (self.base + register).as_ptr::<u64>().read_volatile() }
    }

    fn write64(&self, register: u64, value: u64) {
        unsafe { (self.base + register).as_mut_ptr::<u64>().write_volatile(value) };
    }

    fn read32(&self, register: usize) -> u32 {
        unsafe { (self.base + register as u64).as_ptr::<u32>().read_volatile() }
    }
}

struct HardwareBus {
    mmio: MMIO,
    physical_memory_offset: VirtAddr,
}

impl AmdIommuIo for HardwareBus {
    fn mmio_read(&self, register: u64) -> u64 {
        self.mmio.read64(register)
    }

    fn mmio_write(&mut self, register: u64, value: u64) {
        self.mmio.write64(register, value);
    }

    fn memory_read(&self, physical: u64) -> u64 {
        unsafe { (self.physical_memory_offset + physical).as_ptr::<u64>().read_volatile() }
    }

    fn memory_write(&mut self, physical: u64, value: u64) {
        unsafe {
            (self.physical_memory_offset + physical)
                .as_mut_ptr::<u64>()
                .write_volatile(value)
        };
    }
}

struct QemuEduStop {
    config_command_address: VirtAddr,
    edu_mmio: MMIO,
}

impl QemuEduStop {
    fn command(&self) -> u16 {
        unsafe { self.config_command_address.as_ptr::<u16>().read_volatile() }
    }

    fn set_command(&self, value: u16) {
        unsafe { self.config_command_address.as_mut_ptr::<u16>().write_volatile(value) };
    }
}

impl StoppedDevice for QemuEduStop {
    fn is_fully_stopped(&mut self) -> bool {
        self.command() & 4 == 0
            && self.edu_mmio.read64(edu::MMIO_DMA_COMMAND as u64) & 1 == 0
    }
}

fn map_mmio<const N: usize>(
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
    virtual_base: u64,
    physical_base: u64,
    pages: u64,
) -> Result<(), &'static str> {
    let flags = PageTableFlags::WRITABLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH;

    for index in 0..pages {
        let virtual_address = VirtAddr::new(virtual_base + index * PAGE);
        let physical_address = PhysAddr::new(physical_base + index * PAGE);
        let page = Page::<Size4KiB>::containing_address(virtual_address);
        let frame = PhysFrame::<Size4KiB>::containing_address(physical_address);
        if table.translate_addr(virtual_address).is_some() {
            return Err("виртуальный MMIO диапазон уже занят");
        }
        unsafe { table.map_4k(page, frame, flags, frames) }
            .map_err(|_| "невозможно отобразить MMIO")?;
    }
    Ok(())
}

fn read_u32(address: VirtAddr) -> u32 {
    unsafe { address.as_ptr::<u32>().read_volatile() }
}

fn transfer(mmio: &MMIO, source: u64, destination: u64, command: u32) -> Result<(), &'static str> {
    mmio.write64(edu::MMIO_DMA_SRC as u64, source);
    mmio.write64(edu::MMIO_DMA_DST as u64, destination);
    mmio.write64(edu::MMIO_DMA_COUNT as u64, edu::EDU_TRANSFER_BYTES as u64);
    fence(Ordering::SeqCst);
    mmio.write64(edu::MMIO_DMA_COMMAND as u64, u64::from(command));
    for _ in 0..POLL_COUNT {
        if mmio.read64(edu::MMIO_DMA_COMMAND as u64) & 1 == 0 {
            fence(Ordering::SeqCst);
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err("виртуальный EDU не завершил передачу")
}

fn fill(address: VirtAddr, byte: u8) {
    for index in 0..PAGE {
        unsafe { (address + index).as_mut_ptr::<u8>().write_volatile(byte) };
    }
    fence(Ordering::SeqCst);
}

fn intact(address: VirtAddr, byte: u8) -> bool {
    (0..PAGE).all(|index| unsafe { (address + index).as_ptr::<u8>().read_volatile() } == byte)
}

fn pattern(address: VirtAddr) {
    for index in 0..edu::EDU_TRANSFER_BYTES {
        let byte = (index as u8).wrapping_mul(7).wrapping_add(11);
        unsafe { (address + index as u64).as_mut_ptr::<u8>().write_volatile(byte) };
    }
    fence(Ordering::SeqCst);
}

fn pattern_matches(address: VirtAddr) -> bool {
    (0..edu::EDU_TRANSFER_BYTES).all(|index| {
        let expected = (index as u8).wrapping_mul(7).wrapping_add(11);
        unsafe { (address + index as u64).as_ptr::<u8>().read_volatile() } == expected
    })
}

/// Устройство EDU обязано иметь единственный точно известный идентификатор.
/// Вызов допускается только ПОСЛЕ проверки виртуальной среды.
///
/// Важная граница: вне QEMU запрещены как запись в PCI, так и запись в IOMMU.
pub unsafe fn verify<const N: usize>(
    function: PcieFunction,
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
    offset: VirtAddr,
) -> Result<(usize, bool), &'static str> {
    if function.vendor_id != edu::EDU_VENDOR_ID
        || function.device_id != edu::EDU_DEVICE_ID
        || function.bus != 0
    {
        return Err("неразрешённый виртуальный контроллер");
    }
    let device_id = (u16::from(function.bus) << 8)
        | (u16::from(function.device) << 3)
        | u16::from(function.function);
    if device_id >= 128 {
        return Err("идентификатор EDU вне подтверждённой таблицы");
    }

    let config = offset + function.config_address;
    let cmd_addr = config + 4;
    let original_command = unsafe { cmd_addr.as_ptr::<u16>().read_volatile() };
    if original_command & 4 != 0 {
        return Err("устройство уже выполняет DMA");
    }
    let bar = read_u32(config + 0x10);
    if bar & 0xf != 0 || bar & !0xfff == 0 {
        return Err("некорректная область EDU");
    }
    let edu_base = u64::from(bar & !0xfff);
    if edu_base == IOMMU_QEMU_BASE {
        return Err("пересечение виртуальных контроллеров");
    }

    map_mmio(table, frames, IOMMU_MAP, IOMMU_QEMU_BASE, 3)?;
    map_mmio(table, frames, EDU_MAP, edu_base, 1)?;
    let edu_mmio = MMIO { base: VirtAddr::new(EDU_MAP) };
    if edu_mmio.read32(edu::MMIO_IDENTIFICATION) & 0xff != 0xed {
        return Err("не распознан виртуальный EDU");
    }
    // Только доступ к регистрам; DMA остаётся выключенным до установки DTE.
    unsafe { cmd_addr.as_mut_ptr::<u16>().write_volatile(original_command | 2) };

    // Восемь страниц таблиц и одна страница данных — все целиком
    // принадлежат этому испытанию и не возвращаются при незавершённой DMA.
    let backing = frames
        .allocate_contiguous_below_4k(7, 1, DMA_MASK)
        .ok_or("не удалось выделить страницы AMD IOMMU")?
        .start_address()
        .as_u64();
    let page_addr = |index: u64| backing + PAGE * index;
    let pages = AmdIommuPages {
        device_table: page_addr(0),
        command_buffer: page_addr(1),
        root: page_addr(2),
        level3: page_addr(3),
        level2: page_addr(4),
        level1: page_addr(5),
        completion: page_addr(6),
    };

    // IOVA физически совпадает с охраняемой областью, которую НЕЛЬЗЯ
    // изменять. Отдельная страница данных находится по другому адресу.
    // Это позволяет выявить как обход IOMMU, так и ошибку изоляции.
    let guards = frames
        .allocate_contiguous_below_4k(2, 512, DMA_MASK)
        .ok_or("не удалось выделить непрерывные страницы защиты")?
        .start_address()
        .as_u64();
    let data = frames
        .allocate_contiguous_below_4k(1, 1, DMA_MASK)
        .ok_or("не удалось выделить буфер DMA")?
        .start_address()
        .as_u64();
    if data == guards || data == guards + PAGE {
        return Err("буфер не отделён от защищённых страниц");
    }
    let guard0 = offset + guards;
    let guard1 = offset + guards + PAGE;
    let buffer = offset + data;
    fill(guard0, SENTINEL);
    fill(guard1, SENTINEL);
    pattern(buffer);

    let device = QemuEduStop {
        config_command_address: cmd_addr,
        edu_mmio,
    };
    let io = HardwareBus {
        mmio: MMIO { base: VirtAddr::new(IOMMU_MAP) },
        physical_memory_offset: offset,
    };
    let mut iommu = AmdV1Engine::new(io, device, pages, device_id, 7, guards)
        .map_err(|_| "проверка исходной конфигурации AMD IOMMU не пройдена")?;
    iommu.initialize().map_err(|_| "аппаратный AMD IOMMU не инициализирован")?;
    iommu.add_page(guards, data).map_err(|_| "не создан аппаратный домен IOVA")?;
    if !iommu.translation_active() || !iommu.device_in_private_domain() {
        return Err("аппаратный домен не подтверждён");
    }

    // Переводим EDU в режим ведущего шины только при действующем домене.
    let (bus, controller) = iommu.into_parts();
    let _bus = bus; // Таблицы остаются закреплены до выключения QEMU.
    controller.set_command(original_command | 2 | 4);
    let edu = controller.edu_mmio;

    transfer(&edu, guards, edu::EDU_INTERNAL_BUFFER, edu::EDU_DMA_START)?;
    fill(buffer, 0);
    transfer(
        &edu,
        edu::EDU_INTERNAL_BUFFER,
        guards,
        edu::EDU_DMA_START | edu::EDU_DMA_FROM_DEVICE,
    )?;
    if !pattern_matches(buffer) || !intact(guard0, SENTINEL) {
        return Err("отсутствует доказательство настоящего преобразования DMA-адреса");
    }

    // Попытка записи в НЕ отображённую страницу должна оставаться
    // без доступа к охраняемой физической странице.
    transfer(
        &edu,
        edu::EDU_INTERNAL_BUFFER,
        guards + PAGE,
        edu::EDU_DMA_START | edu::EDU_DMA_FROM_DEVICE,
    )?;
    if !intact(guard1, SENTINEL) || !intact(guard0, SENTINEL) {
        return Err("виртуальное устройство получило доступ вне домена");
    }

    controller.set_command(original_command & !4);
    if controller.command() & 4 != 0
        || edu.read64(edu::MMIO_DMA_COMMAND as u64) & 1 != 0
    {
        return Err("устройство не остановилось");
    }
    // Сведения остаются в гостевой RAM до выключения машины.
    // Никакие контрольные или DMA-страницы не возвращаются при ошибке.
    Ok((edu::EDU_TRANSFER_BYTES, true))
}
