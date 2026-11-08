//! Управление регистровым окном живого контроллера xHCI (QEMU).
//!
//! MMIO-отображение и DMA-страницы остаются закреплёнными до явного
//! завершения работы драйвера. После запуска можно проверять состояние
//! контроллера и опрашивать кольцо событий без повторного сброса.

use crate::xhci_command::{
    self, ActiveDma, CommandError, CommandReport, RetainedSlotsReport, RuntimeEvent,
    RuntimePortEvent,
};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::usb_descriptors::HidConfiguration;
use phoenix_pcie::usb_hid::{HidError, InputEvent, KeyboardReport, KeyboardState, MouseReport};
use phoenix_pcie::xhci::XhciPciController;
use phoenix_pcie::xhci_capabilities::{CapabilityError, RegisterAccess, XhciCapabilities};
use phoenix_pcie::xhci_control::{self, OperationalRegisters, ResetError, ResetResult};
use phoenix_pcie::xhci_ports::{self, PortError, PortSummary};
use phoenix_pcie::xhci_usb_service::{UsbBinding, UsbService, UsbServiceError};
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
    InvalidPortEventCompletion(u8),
    UsbService(UsbServiceError),
    HidReport(HidError),
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
    service: UsbService,
    keyboard: KeyboardState,
    closed: bool,
}

/// Итог одной итерации обслуживания USB-портов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceResult {
    Idle,
    Unchanged(u8),
    Detached(UsbBinding),
    Attached(UsbBinding),
    Hid(InputEvent),
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

    pub fn connected_count(&self) -> usize {
        self.service.active_count()
    }

    pub fn binding_for_port(&self, port: u8) -> Option<UsbBinding> {
        self.service.for_port(port)
    }

    /// Подготавливает USB HID для ранее адресованного устройства.
    /// Не меняет учёт слотов до подтверждения команд контроллера.
    pub fn configure_hid_for_port(&mut self, port: u8) -> Result<HidConfiguration, XhciMmioError> {
        if !self.is_running() {
            return Err(XhciMmioError::ControllerNotRunning);
        }
        let Some(binding) = self.service.for_port(port) else {
            return Err(XhciMmioError::UsbService(UsbServiceError::InvalidBinding));
        };
        let Some(index) = (0..2).find(|&index| self.service.for_index(index) == Some(binding))
        else {
            return Err(XhciMmioError::UsbService(UsbServiceError::InvalidBinding));
        };
        self.dma
            .configure_hid(&self.registers, self.capabilities, index, binding.slot)
            .map_err(XhciMmioError::Command)
    }

    fn handle_hid(
        &mut self,
        report: xhci_command::HidRawReport,
    ) -> Result<ServiceResult, XhciMmioError> {
        if report.protocol == 1 {
            let parsed = KeyboardReport::parse(&report.bytes[..usize::from(report.length)])
                .map_err(XhciMmioError::HidReport)?;
            return Ok(match self.keyboard.update(parsed) {
                Some(change) => ServiceResult::Hid(InputEvent::Key(change)),
                None => ServiceResult::Idle,
            });
        }
        if report.protocol == 2 {
            let parsed = MouseReport::parse(&report.bytes[..usize::from(report.length)])
                .map_err(XhciMmioError::HidReport)?;
            if parsed.buttons == 0
                && parsed.delta_x == 0
                && parsed.delta_y == 0
                && parsed.wheel == 0
            {
                return Ok(ServiceResult::Idle);
            }
            return Ok(ServiceResult::Hid(InputEvent::Mouse(parsed)));
        }
        Ok(ServiceResult::Idle)
    }

    /// Одна итерация постоянной службы: извещение из кольца xHCI,
    /// подтверждение изменения PORTSC и точечное обслуживание USB-слота.
    /// Не сбрасывает контроллер и не освобождает общую DMA-память.
    pub fn service_once(&mut self) -> Result<ServiceResult, XhciMmioError> {
        if !self.is_running() {
            return Err(XhciMmioError::ControllerNotRunning);
        }
        let next = self
            .dma
            .poll_runtime_event(&self.registers, self.capabilities)
            .map_err(XhciMmioError::Command)?;
        let event = match next {
            None => return Ok(ServiceResult::Idle),
            Some(RuntimeEvent::Hid(report)) => return self.handle_hid(report),
            Some(RuntimeEvent::Port(port)) => port,
        };
        if event.completion_code != 1 {
            return Err(XhciMmioError::InvalidPortEventCompletion(
                event.completion_code,
            ));
        }
        let snapshot = xhci_ports::acknowledge_change(
            &self.registers,
            self.capabilities.operational_offset,
            event.port,
            QEMU_XHCI_MMIO_BYTES,
            xhci_control::MAX_POLLS,
        )
        .map_err(XhciMmioError::Ports)?;

        if !snapshot.connected {
            let Some(binding) = self.service.for_port(event.port) else {
                return Ok(ServiceResult::Unchanged(event.port));
            };
            self.dma
                .disable_slot(&self.registers, self.capabilities, binding.slot)
                .map_err(XhciMmioError::Command)?;
            self.service.detach(event.port);
            if binding.protocol == 1 {
                self.keyboard = KeyboardState::new();
            }
            return Ok(ServiceResult::Detached(binding));
        }

        if self.service.for_port(event.port).is_some() {
            return Ok(ServiceResult::Unchanged(event.port));
        }

        let index = self
            .service
            .vacant_index()
            .ok_or(XhciMmioError::UsbService(UsbServiceError::NoFreeContext))?;
        let descriptor = self
            .dma
            .attach_device(
                &self.registers,
                self.capabilities,
                index,
                event.port,
                snapshot.speed,
            )
            .map_err(XhciMmioError::Command)?;
        let binding = UsbBinding {
            port: event.port,
            slot: descriptor.slot_id,
            protocol: descriptor.interface_protocol,
        };
        self.service
            .attach(binding)
            .map_err(XhciMmioError::UsbService)?;
        Ok(ServiceResult::Attached(binding))
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
        let (report, second, retained, mut dma) = unsafe {
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
        let mut service = UsbService::new();
        for binding in [
            UsbBinding {
                port: ports.first_connected_port,
                slot: report.slot_id,
                protocol: report.interface_protocol,
            },
            UsbBinding {
                port: ports.second_connected_port,
                slot: second.slot_id,
                protocol: second.interface_protocol,
            },
        ] {
            if let Err(error) = service.attach(binding) {
                // Если сведения о двух устройствах противоречивы, нельзя
                // снимать MMIO, пока аппаратный DMA не остановлен.
                dma.shutdown(&registers, capabilities, frames)
                    .map_err(XhciMmioError::Command)?;
                return Err(XhciMmioError::UsbService(error));
            }
        }
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
                service,
                keyboard: KeyboardState::new(),
                closed: false,
            },
        ))
    })();

    // При успешном запуске окно остаётся отображённым: теперь им
    // владеет ActiveXhci. При отказе инициализации старый путь
    // восстановления уже остановил DMA, можно убрать MMIO.
    if matches!(
        &result,
        Err(XhciMmioError::Command(CommandError::UnsafeShutdown))
    ) {
        // Контроллер мог продолжить DMA. Запрещено снимать рабочее
        // регистровое окно до подтверждённой остановки устройства.
        return result;
    }
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
