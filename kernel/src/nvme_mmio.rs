//! Безопасная начальная проверка регистров NVMe в реальном MMIO QEMU.
//!
//! Временно отображается только первая страница BAR0, включающая CAP,
//! VS, CC и CSTS. Контроллер не сбрасывается и не получает команд.
//! Это ещё не работающий блочный драйвер: очереди и DMA появятся отдельно.

use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::nvme::{
    self, NvmeCapabilities, NvmeCapabilityError, NvmePciController, RegisterAccess,
};
use phoenix_vm::{ActivePageTable, PAGE_SIZE};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB},
};

const NVME_MMIO_VIRTUAL_START: u64 = 0xffff_a200_0000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvmeMmioError {
    MemoryAccessDisabled,
    InvalidPhysicalRange,
    VirtualRangeOccupied,
    MappingFailed,
    UnmappingFailed,
    InvalidCapabilities(NvmeCapabilityError),
}

struct MappedRegisters {
    base: VirtAddr,
}

impl RegisterAccess for MappedRegisters {
    fn read_u32(&self, offset: u64) -> u32 {
        debug_assert!(offset + 4 <= PAGE_SIZE && offset % 4 == 0);
        let pointer = (self.base + offset).as_ptr::<u32>();
        unsafe { pointer.read_volatile() }
    }
}

/// Только чтение стандартных регистров первой страницы BAR0.
///
/// # Безопасность
///
/// Вызывающий код удостоверяет, что BAR относится к найденному NVMe,
/// физический диапазон не перекрывает память других устройств, а политика
/// кэширования совместима с остальными физическими отображениями.
/// Устройства с отличающимся размером BAR требуют отдельной проверки.
pub unsafe fn probe<const N: usize>(
    controller: NvmePciController,
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
) -> Result<NvmeCapabilities, NvmeMmioError> {
    if !controller.memory_enabled {
        return Err(NvmeMmioError::MemoryAccessDisabled);
    }
    let last = controller
        .mmio_base
        .checked_add(PAGE_SIZE - 1)
        .ok_or(NvmeMmioError::InvalidPhysicalRange)?;
    if controller.mmio_base % PAGE_SIZE != 0
        || PhysAddr::try_new(controller.mmio_base).is_err()
        || PhysAddr::try_new(last).is_err()
    {
        return Err(NvmeMmioError::InvalidPhysicalRange);
    }

    let virtual_address = VirtAddr::new(NVME_MMIO_VIRTUAL_START);
    if table.translate_addr(virtual_address).is_some() {
        return Err(NvmeMmioError::VirtualRangeOccupied);
    }
    let page = Page::<Size4KiB>::containing_address(virtual_address);
    let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(controller.mmio_base));
    let flags = PageTableFlags::WRITABLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH;

    unsafe { table.map_4k(page, frame, flags, frames) }
        .map_err(|_| NvmeMmioError::MappingFailed)?;
    let registers = MappedRegisters {
        base: virtual_address,
    };
    let capabilities = nvme::read_capabilities(&registers);
    // Даже при некорректном содержимом аппаратных регистров отображение
    // снимается; драйвер не удерживает MMIO до запуска очередей.
    table
        .unmap_4k(page)
        .map_err(|_| NvmeMmioError::UnmappingFailed)?;
    capabilities.map_err(NvmeMmioError::InvalidCapabilities)
}
