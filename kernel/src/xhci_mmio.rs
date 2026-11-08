//! Управление регистровым окном живого контроллера xHCI (QEMU).
//!
//! MMIO-отображение и DMA-страницы остаются закреплёнными до явного
//! завершения работы драйвера. После запуска можно проверять состояние
//! контроллера и опрашивать кольцо событий без повторного сброса.

use crate::xhci_command::{
    self, ActiveDma, CommandError, CommandReport, RetainedSlotsReport, RuntimePortEvent,
};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::xhci::XhciPciController;
use phoenix_pcie::xhci_capabilities::{CapabilityError, RegisterAccess, XhciCapabilities};
use phoenix_pcie::xhci_control::{self, OperationalRegisters, ResetError, ResetResult};
use phoenix_pcie::xhci_ports::{self, PortError, PortSummary};
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
    Ports(PortError),
    MissingTestDevices,
    ControllerNotRunning,
}

struct MappedRegisters {
    base: VirtAddr,
}

/// Контекст работающего контроллера. Владеет действующим DMA-отображением
/// и зарезервированным окном MMIO; освободить их можно только через shutdown.
pub struct ActiveXhci {
    registers: MappedRegisters,
    capabilities: XhciCapabilities,
    dma: ActiveDma,
    closed: bool,
}

impl ActiveXhci {
    pub fn is_running(&self) -> bool {
        !self.closed
            && OperationalRegisters::read_u32(
                &self.registers,
                self.capabilities.operational_offset + 4,
            ) & 1
                == 0
            && self.dma.is_mapped()
    }

    pub fn poll_port_event(&mut self) -> Result<Option<RuntimePortEvent>, XhciMmioError> {
        if !self.is_running() {
            return Err(XhciMmioError::ControllerNotRunning);
        }
        self.dma
            .poll_port_event(&self.registers, self.capabilities)
            .map_err(XhciMmioError::Command)
    }

    /// Снятие DMA всегда выполняется после аппаратной остановки xHCI.
    /// Ошибка остановки сохраняет и MMIO, и DMA для повторной попытки.
    pub fn shutdown<const N: usize>(
        &mut self,
        table: &mut ActivePageTable,
        frames: &mut SystemFrameAllocator<N>,
    ) -> Result<(), XhciMmioError> {
        if self.closed {
            return Err(XhciMmioError::ControllerNotRunning);
        }
        self.dma
            .shutdown(&self.registers, self.capabilities, frames)
            .map_err(XhciMmioError::Command)?;
        unmap_pages(table, XHCI_MMIO_PAGE_COUNT)?;
        self.closed = true;
        Ok(())
    }
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
) -> Result<
    (
        XhciCapabilities,
        ResetResult,
        CommandReport,
        CommandReport,
        PortSummary,
        RetainedSlotsReport,
        ActiveXhci,
    ),
    XhciMmioError,
> {
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
        let ports = xhci_ports::scan_ports(
            &registers,
            capabilities.operational_offset,
            capabilities.max_ports,
            QEMU_XHCI_MMIO_BYTES,
            xhci_control::MAX_POLLS,
        )
        .map_err(XhciMmioError::Ports)?;
        if ports.connected < 2 || ports.enabled < 2 {
            return Err(XhciMmioError::MissingTestDevices);
        }
        // Один живой сеанс: два слота сохраняются за пределами загрузочных
        // проверок; DMA принадлежит ActiveXhci до безопасного завершения.
        let (report, second, retained, dma) = unsafe {
            xhci_command::start_pair(
                &registers,
                capabilities,
                frames,
                physical_memory_offset,
                ports.first_connected_port,
                ports.first_speed,
                ports.second_connected_port,
                ports.second_speed,
            )
        }
        .map_err(XhciMmioError::Command)?;
        Ok((
            capabilities,
            reset,
            report,
            second,
            ports,
            retained,
            ActiveXhci {
                registers,
                capabilities,
                dma,
                closed: false,
            },
        ))
    })();

    // При успешном запуске окно остаётся отображённым: теперь им
    // владеет ActiveXhci. При отказе инициализации старый путь
    // восстановления уже остановил DMA, можно убрать MMIO.
    if result.is_err() {
        unmap_pages(table, mapped)?;
    }
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
