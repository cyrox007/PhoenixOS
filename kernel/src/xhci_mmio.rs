//! Диагностическое чтение MMIO-регистров xHCI из проверочного профиля QEMU.
//!
//! Не запускает контроллер и не разрешает DMA. Фиксированный диапазон
//! зарезервирован исключительно для ранней диагностики ядра в QEMU.

use crate::xhci_command::{self, CommandError, CommandReport};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::xhci::XhciPciController;
use phoenix_pcie::xhci_capabilities::{CapabilityError, RegisterAccess, XhciCapabilities};
use phoenix_pcie::xhci_control::{self, OperationalRegisters, ResetError, ResetResult};
use phoenix_vm::{ActivePageTable, PAGE_SIZE};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB},
};

const XHCI_MMIO_VIRTUAL_START: u64 = 0xffff_a100_0000_0000;
/// Размер окна только для текущего QEMU-профиля. Это не определение размера BAR.
const QEMU_XHCI_MMIO_BYTES: u64 = 0x4000;
const XHCI_MMIO_PAGE_COUNT: u64 = QEMU_XHCI_MMIO_BYTES / PAGE_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XhciMmioError {
    MemoryAccessDisabled,
    InvalidPhysicalRange,
    VirtualRangeOccupied,
    MappingFailed,
    UnmappingFailed,
    InvalidCapabilities(CapabilityError),
    ControllerReset(ResetError),
    Command(CommandError),
}

struct MappedRegisters {
    base: VirtAddr,
}

impl RegisterAccess for MappedRegisters {
    fn read_u32(&self, offset: u64) -> u32 {
        // Диапазон временного окна включает возможности и рабочие регистры.
        debug_assert!(offset < QEMU_XHCI_MMIO_BYTES && offset % 4 == 0);
        let pointer = (self.base + offset).as_ptr::<u32>();
        unsafe { pointer.read_volatile() }
    }
}

impl OperationalRegisters for MappedRegisters {
    fn read_u32(&self, offset: u64) -> u32 {
        RegisterAccess::read_u32(self, offset)
    }

    fn write_u32(&self, offset: u64, value: u32) {
        debug_assert!(offset < QEMU_XHCI_MMIO_BYTES && offset % 4 == 0);
        let pointer = (self.base + offset).as_mut_ptr::<u32>();
        unsafe { pointer.write_volatile(value) };
    }
}

/// Читает регистры действующего контроллера QEMU через отдельное отображение.
///
/// # Безопасность
///
/// Вызывающий код гарантирует, что BAR принадлежит обнаруженному xHCI, весь
/// диапазон окна соответствует устройству, регион виртуальных адресов
/// эксклюзивно выделен ядром, а текущий тип памяти не конфликтует с другими
/// отображениями этого же физического диапазона. Флаги PCD/PWT лишь задают
/// некэшируемое отображение с учётом конфигурации PAT/MTRR.
pub unsafe fn probe<const N: usize>(
    controller: XhciPciController,
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
) -> Result<(XhciCapabilities, ResetResult, CommandReport), XhciMmioError> {
    if !controller.memory_enabled {
        return Err(XhciMmioError::MemoryAccessDisabled);
    }

    let physical_end = controller
        .mmio_base
        .checked_add(QEMU_XHCI_MMIO_BYTES - 1)
        .ok_or(XhciMmioError::InvalidPhysicalRange)?;
    if controller.mmio_base % PAGE_SIZE != 0
        || PhysAddr::try_new(controller.mmio_base).is_err()
        || PhysAddr::try_new(physical_end).is_err()
    {
        return Err(XhciMmioError::InvalidPhysicalRange);
    }

    for index in 0..XHCI_MMIO_PAGE_COUNT {
        let address = VirtAddr::new(XHCI_MMIO_VIRTUAL_START + index * PAGE_SIZE);
        if table.translate_addr(address).is_some() {
            return Err(XhciMmioError::VirtualRangeOccupied);
        }
    }

    let mut mapped = 0;
    while mapped < XHCI_MMIO_PAGE_COUNT {
        let virtual_addr = VirtAddr::new(XHCI_MMIO_VIRTUAL_START + mapped * PAGE_SIZE);
        let physical_addr = PhysAddr::new(controller.mmio_base + mapped * PAGE_SIZE);
        let page = Page::<Size4KiB>::containing_address(virtual_addr);
        let frame = PhysFrame::<Size4KiB>::containing_address(physical_addr);
        let flags = PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE
            | PageTableFlags::WRITE_THROUGH;
        if unsafe { table.map_4k(page, frame, flags, frames) }.is_err() {
            unmap_pages(table, mapped)?;
            return Err(XhciMmioError::MappingFailed);
        }
        mapped += 1;
    }

    let registers = MappedRegisters {
        base: VirtAddr::new(XHCI_MMIO_VIRTUAL_START),
    };
    let result = (|| {
        let capabilities = XhciCapabilities::probe(&registers, QEMU_XHCI_MMIO_BYTES)
            .map_err(XhciMmioError::InvalidCapabilities)?;
        let reset = xhci_control::stop_and_reset(
            &registers,
            capabilities.operational_offset,
            xhci_control::MAX_POLLS,
        )
        .map_err(XhciMmioError::ControllerReset)?;
        let report = unsafe {
            xhci_command::first_command(&registers, capabilities, frames, physical_memory_offset)
        }
        .map_err(XhciMmioError::Command)?;
        // Второй независимый цикл проверяет повторное выделение страниц,
        // отсутствие оставшихся отображений и повторный запуск устройства.
        let repeated = unsafe {
            xhci_command::first_command(&registers, capabilities, frames, physical_memory_offset)
        }
        .map_err(XhciMmioError::Command)?;
        assert_eq!(report, repeated, "повторный обмен xHCI дал другой результат");
        Ok((capabilities, reset, report))
    })();
    unmap_pages(table, mapped)?;
    result
}

fn unmap_pages(table: &mut ActivePageTable, mapped: u64) -> Result<(), XhciMmioError> {
    for index in (0..mapped).rev() {
        let virtual_addr = VirtAddr::new(XHCI_MMIO_VIRTUAL_START + index * PAGE_SIZE);
        let page = Page::<Size4KiB>::containing_address(virtual_addr);
        table
            .unmap_4k(page)
            .map_err(|_| XhciMmioError::UnmappingFailed)?;
    }
    Ok(())
}
