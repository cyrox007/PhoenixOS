//! Запуск xHCI через общий реестр драйверов с ресурсами ядра.
//!
//! Контроллер определяется по классу PCIe. Физические страницы и таблицы
//! отображений остаются собственностью ядра и передаются обработчику явно.

use crate::xhci_mmio::{self, XhciMmioError};
use phoenix_driver_registry::{
    ContextRegisteredDriver, DriverDescriptor, DriverLaunch, DriverRegistry, DriverRegistryError,
    DriverStartError, DriverVersion, PcieMatchRule,
};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::PcieFunction;
use phoenix_pcie::xhci::{self, XhciPciController};
use phoenix_pcie::xhci_capabilities::XhciCapabilities;
use phoenix_pcie::xhci_control::ResetResult;
use phoenix_pcie::xhci_ports::PortSummary;
use phoenix_vm::ActivePageTable;
use x86_64::VirtAddr;

use crate::xhci_command::{CommandReport, RetainedSlotsReport};

pub type ProbeReport = (
    XhciCapabilities,
    ResetResult,
    CommandReport,
    CommandReport,
    PortSummary,
    RetainedSlotsReport,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XhciStartError {
    Registry(DriverRegistryError),
    Hardware(XhciMmioError),
    MissingReport,
}

struct XhciDriverContext<'a, const N: usize> {
    controller: XhciPciController,
    table: &'a mut ActivePageTable,
    frames: &'a mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
    report: Option<ProbeReport>,
    hardware_error: Option<XhciMmioError>,
}

fn start_xhci<const N: usize>(
    function: PcieFunction,
    context: &mut XhciDriverContext<'_, N>,
) -> Result<(), DriverStartError> {
    if !xhci::is_xhci(function) || function != context.controller.function {
        return Err(DriverStartError::UnsupportedDevice);
    }
    if !context.controller.memory_enabled || !context.controller.bus_master_enabled {
        return Err(DriverStartError::ResourceUnavailable);
    }

    // Безопасность: вызывающая сторона владеет эксклюзивным отображением
    // регистров QEMU xHCI и передаёт единственные изменяемые ссылки на память.
    let result = unsafe {
        xhci_mmio::probe(
            context.controller,
            context.table,
            context.frames,
            context.physical_memory_offset,
        )
    };
    match result {
        Ok(report) => {
            context.report = Some(report);
            Ok(())
        }
        Err(error) => {
            context.hardware_error = Some(error);
            Err(DriverStartError::InitializationFailed)
        }
    }
}

/// Находит и действительно запускает xHCI через тот же механизм выбора,
/// что и остальные драйверы PCIe. Возвращает результат лишь после проверки
/// регистров, перечисления двух USB-устройств за один запуск и
/// повторной передачи к первому устройству без сброса xHCI.
///
/// # Безопасность
///
/// Вызывающий код должен выполнять требования к эксклюзивному MMIO-окну
/// и типам памяти, перечисленные в `xhci_mmio::probe`.
pub unsafe fn start<const N: usize>(
    controller: XhciPciController,
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
) -> Result<(DriverLaunch, ProbeReport), XhciStartError> {
    let mut registry =
        DriverRegistry::<1, ContextRegisteredDriver<XhciDriverContext<'_, N>>>::new();
    registry
        .register(ContextRegisteredDriver {
            descriptor: DriverDescriptor::new(
                "xhci",
                DriverVersion::new(0, 3, 0),
                100,
                PcieMatchRule {
                    class_code: Some(xhci::USB_CLASS),
                    subclass: Some(xhci::USB_SUBCLASS),
                    programming_interface: Some(xhci::XHCI_INTERFACE),
                    ..PcieMatchRule::any()
                },
            ),
            start: start_xhci::<N>,
        })
        .map_err(XhciStartError::Registry)?;

    let mut context = XhciDriverContext {
        controller,
        table,
        frames,
        physical_memory_offset,
        report: None,
        hardware_error: None,
    };
    let launch = registry.start_best_with_context(controller.function, &mut context);
    if let Some(error) = context.hardware_error {
        return Err(XhciStartError::Hardware(error));
    }
    let launch = launch.map_err(XhciStartError::Registry)?;
    let report = context.report.take().ok_or(XhciStartError::MissingReport)?;
    Ok((launch, report))
}
