//! Проверенный выбор NVMe-драйвера через общий реестр PCIe.
//!
//! На данном этапе инициализация ограничена чтением CAP/VS/CC/CSTS:
//! очереди, прерывания и команды NVM ещё не подаются.

use crate::nvme_mmio::{self, NvmeMmioError};
use phoenix_driver_registry::{
    ContextRegisteredDriver, DriverDescriptor, DriverLaunch, DriverRegistry, DriverRegistryError,
    DriverStartError, DriverVersion, PcieMatchRule,
};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::PcieFunction;
use phoenix_pcie::nvme::{self, NvmeCapabilities, NvmePciController};
use phoenix_vm::ActivePageTable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvmeStartError {
    Registry(DriverRegistryError),
    Hardware(NvmeMmioError),
    MissingReport,
}

struct NvmeDriverContext<'a, const N: usize> {
    controller: NvmePciController,
    table: &'a mut ActivePageTable,
    frames: &'a mut SystemFrameAllocator<N>,
    capabilities: Option<NvmeCapabilities>,
    hardware_error: Option<NvmeMmioError>,
}

fn probe_nvme<const N: usize>(
    function: PcieFunction,
    context: &mut NvmeDriverContext<'_, N>,
) -> Result<(), DriverStartError> {
    if !nvme::is_nvme(function) || function != context.controller.function {
        return Err(DriverStartError::UnsupportedDevice);
    }
    if !context.controller.memory_enabled || !context.controller.bus_master_enabled {
        return Err(DriverStartError::ResourceUnavailable);
    }
    let result = unsafe { nvme_mmio::probe(context.controller, context.table, context.frames) };
    match result {
        Ok(capabilities) => {
            context.capabilities = Some(capabilities);
            Ok(())
        }
        Err(error) => {
            context.hardware_error = Some(error);
            Err(DriverStartError::InitializationFailed)
        }
    }
}

/// Выбор NVMe по классу PCIe и аппаратная проверка регистров.
/// Не выдаёт свидетельство работающих очередей DMA или блочного ввода-вывода.
///
/// # Безопасность
///
/// Вызывающая сторона обязана обеспечить корректное эксклюзивное
/// некэшируемое отображение BAR, как указано в nvme_mmio::probe.
pub unsafe fn probe<const N: usize>(
    controller: NvmePciController,
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
) -> Result<(DriverLaunch, NvmeCapabilities), NvmeStartError> {
    let mut registry =
        DriverRegistry::<1, ContextRegisteredDriver<NvmeDriverContext<'_, N>>>::new();
    registry
        .register(ContextRegisteredDriver {
            descriptor: DriverDescriptor::new(
                "nvme",
                DriverVersion::new(0, 3, 0),
                100,
                PcieMatchRule {
                    class_code: Some(nvme::STORAGE_CLASS),
                    subclass: Some(nvme::NVM_SUBCLASS),
                    programming_interface: Some(nvme::NVME_INTERFACE),
                    ..PcieMatchRule::any()
                },
            ),
            start: probe_nvme::<N>,
        })
        .map_err(NvmeStartError::Registry)?;

    let mut context = NvmeDriverContext {
        controller,
        table,
        frames,
        capabilities: None,
        hardware_error: None,
    };
    let launch = registry.start_best_with_context(controller.function, &mut context);
    if let Some(error) = context.hardware_error {
        return Err(NvmeStartError::Hardware(error));
    }
    let launch = launch.map_err(NvmeStartError::Registry)?;
    let capabilities = context.capabilities.ok_or(NvmeStartError::MissingReport)?;
    Ok((launch, capabilities))
}
