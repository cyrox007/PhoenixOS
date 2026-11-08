//! Чтение PCIe-профилей графики без обращения к регистрам GPU.
//!
//! Это только выбор целевого оборудования. Обнаруженный контроллер не
//! считается работающим драйвером, и владение экраном остаётся у UEFI GOP.

use crate::{ConfigAccess, PcieFunction};

pub const DISPLAY_CLASS: u8 = 0x03;
pub const AMD_VENDOR_ID: u16 = 0x1002;
pub const AMD_CEZANNE_DEVICE_ID: u16 = 0x1638;
pub const NVIDIA_VENDOR_ID: u16 = 0x10de;
pub const NVIDIA_GTX_1650_DEVICE_ID: u16 = 0x1f99;
pub const VIRTIO_VENDOR_ID: u16 = 0x1af4;
pub const VIRTIO_GPU_DEVICE_ID: u16 = 0x1050;
pub const ASUS_VENDOR_ID: u16 = 0x1043;
pub const ASUS_AMD_SUBSYSTEM_ID: u16 = 0x11d3;
pub const ASUS_NVIDIA_SUBSYSTEM_ID: u16 = 0x14bd;

const TYPE_ZERO_HEADER: u8 = 0;
const SUBSYSTEM_IDS_OFFSET: u64 = 0x2c;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsProfile {
    AmdCezanne,
    NvidiaGtx1650,
    VirtioGpu,
}

impl GraphicsProfile {
    pub const fn name(self) -> &'static str {
        match self {
            Self::AmdCezanne => "amd-cezanne",
            Self::NvidiaGtx1650 => "nvidia-gtx1650",
            Self::VirtioGpu => "virtio-gpu",
        }
    }

    /// Пока собственный драйвер есть только у виртуального профиля QEMU.
    pub const fn driver_ready(self) -> bool {
        matches!(self, Self::VirtioGpu)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsSubsystem {
    pub vendor_id: u16,
    pub device_id: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsController {
    pub function: PcieFunction,
    pub profile: GraphicsProfile,
    pub subsystem: Option<GraphicsSubsystem>,
}

impl GraphicsController {
    /// Точные идентификаторы исходного ноутбука ASUS, включая подсистему.
    pub const fn is_reference_asus(self) -> bool {
        let Some(subsystem) = self.subsystem else {
            return false;
        };
        if subsystem.vendor_id != ASUS_VENDOR_ID {
            return false;
        }
        match self.profile {
            GraphicsProfile::AmdCezanne => subsystem.device_id == ASUS_AMD_SUBSYSTEM_ID,
            GraphicsProfile::NvidiaGtx1650 => subsystem.device_id == ASUS_NVIDIA_SUBSYSTEM_ID,
            GraphicsProfile::VirtioGpu => false,
        }
    }
}

/// Политика владения графическим выводом. Не выдаёт права на физический
/// видеоконтроллер: передача относится только к выбранному виртуальному
/// устройству и прекращает обычную запись в ранний буфер GOP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsOutputPhase {
    FirmwareEarly,
    DriverStarting,
    DriverActive,
    EmergencyFirmware,
    DriverStopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsOutputError {
    UnsupportedProfile,
    InvalidTransition,
    DriverNotReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsOutputHandoff {
    phase: GraphicsOutputPhase,
    target: Option<GraphicsProfile>,
}

impl GraphicsOutputHandoff {
    pub const fn new() -> Self {
        Self {
            phase: GraphicsOutputPhase::FirmwareEarly,
            target: None,
        }
    }

    pub const fn phase(self) -> GraphicsOutputPhase {
        self.phase
    }

    /// Обычный вывод через GOP допустим только до запуска штатного драйвера.
    /// В аварийной фазе разрешена только отдельная диагностическая запись.
    pub const fn firmware_boot_allowed(self) -> bool {
        matches!(self.phase, GraphicsOutputPhase::FirmwareEarly)
    }

    pub const fn firmware_emergency_allowed(self) -> bool {
        matches!(self.phase, GraphicsOutputPhase::EmergencyFirmware)
    }

    pub const fn native_output_allowed(self) -> bool {
        matches!(self.phase, GraphicsOutputPhase::DriverActive)
    }

    pub fn begin(&mut self, profile: GraphicsProfile) -> Result<(), GraphicsOutputError> {
        if self.phase != GraphicsOutputPhase::FirmwareEarly {
            return Err(GraphicsOutputError::InvalidTransition);
        }
        if !profile.driver_ready() {
            return Err(GraphicsOutputError::UnsupportedProfile);
        }
        self.target = Some(profile);
        self.phase = GraphicsOutputPhase::DriverStarting;
        Ok(())
    }

    /// Вызывать исключительно после подтверждения работающего устройства.
    pub fn activate(&mut self, ready: bool) -> Result<(), GraphicsOutputError> {
        if self.phase != GraphicsOutputPhase::DriverStarting {
            return Err(GraphicsOutputError::InvalidTransition);
        }
        if !ready {
            return Err(GraphicsOutputError::DriverNotReady);
        }
        self.phase = GraphicsOutputPhase::DriverActive;
        Ok(())
    }

    /// После отказа GOP разрешён лишь как аварийный экран, а не как
    /// параллельный рабочий вывод. Не переинициализирует физический GPU.
    pub fn emergency(&mut self) -> Result<(), GraphicsOutputError> {
        if !matches!(
            self.phase,
            GraphicsOutputPhase::DriverStarting | GraphicsOutputPhase::DriverActive
        ) {
            return Err(GraphicsOutputError::InvalidTransition);
        }
        self.phase = GraphicsOutputPhase::EmergencyFirmware;
        Ok(())
    }

    /// Подтверждение остановки и возврата ресурсов устройства.
    pub fn stop(&mut self, stopped: bool) -> Result<(), GraphicsOutputError> {
        if self.phase != GraphicsOutputPhase::DriverActive {
            return Err(GraphicsOutputError::InvalidTransition);
        }
        if !stopped {
            return Err(GraphicsOutputError::DriverNotReady);
        }
        self.phase = GraphicsOutputPhase::DriverStopped;
        Ok(())
    }
}

pub const fn is_display_controller(function: PcieFunction) -> bool {
    function.class_code == DISPLAY_CLASS
}

pub const fn classify(function: PcieFunction) -> Option<GraphicsProfile> {
    if !is_display_controller(function) {
        return None;
    }

    match (function.vendor_id, function.device_id) {
        (AMD_VENDOR_ID, AMD_CEZANNE_DEVICE_ID) => Some(GraphicsProfile::AmdCezanne),
        (NVIDIA_VENDOR_ID, NVIDIA_GTX_1650_DEVICE_ID) => Some(GraphicsProfile::NvidiaGtx1650),
        (VIRTIO_VENDOR_ID, VIRTIO_GPU_DEVICE_ID) => Some(GraphicsProfile::VirtioGpu),
        _ => None,
    }
}

/// Сводка найденных графических контроллеров; новые устройства не
/// вытесняют уже выбранные первые экземпляры каждого профиля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsInventory {
    pub display_controllers: u32,
    pub unknown_controllers: u32,
    pub amd_cezanne: Option<GraphicsController>,
    pub nvidia_gtx1650: Option<GraphicsController>,
    pub virtio_gpu: Option<GraphicsController>,
}

impl GraphicsInventory {
    pub const fn empty() -> Self {
        Self {
            display_controllers: 0,
            unknown_controllers: 0,
            amd_cezanne: None,
            nvidia_gtx1650: None,
            virtio_gpu: None,
        }
    }

    /// Обрабатывает одну функцию PCIe; не записывает регистры и не читает BAR.
    pub fn observe<A: ConfigAccess>(&mut self, access: &A, function: PcieFunction) {
        if !is_display_controller(function) {
            return;
        }
        self.display_controllers = self.display_controllers.saturating_add(1);

        let Some(profile) = classify(function) else {
            self.unknown_controllers = self.unknown_controllers.saturating_add(1);
            return;
        };

        let subsystem = read_subsystem(access, function);
        let controller = GraphicsController {
            function,
            profile,
            subsystem,
        };
        match profile {
            GraphicsProfile::AmdCezanne if self.amd_cezanne.is_none() => {
                self.amd_cezanne = Some(controller);
            }
            GraphicsProfile::NvidiaGtx1650 if self.nvidia_gtx1650.is_none() => {
                self.nvidia_gtx1650 = Some(controller);
            }
            GraphicsProfile::VirtioGpu if self.virtio_gpu.is_none() => {
                self.virtio_gpu = Some(controller);
            }
            _ => {}
        }
    }

    /// Приоритет первого физического профиля — встроенная AMD.
    /// NVIDIA обнаруживается, но не выбирается для вывода без драйвера.
    /// В виртуальной машине при отсутствии AMD выбирается VirtIO-GPU.
    pub const fn preferred_target(self) -> Option<GraphicsController> {
        match self.amd_cezanne {
            Some(controller) => Some(controller),
            None => self.virtio_gpu,
        }
    }

    /// Физический профиль без штатного драйвера оставляет ранний GOP.
    pub const fn firmware_output_required(self) -> bool {
        match self.preferred_target() {
            Some(controller) => !controller.profile.driver_ready(),
            None => true,
        }
    }
}

fn read_subsystem<A: ConfigAccess>(
    access: &A,
    function: PcieFunction,
) -> Option<GraphicsSubsystem> {
    if function.header_type & 0x7f != TYPE_ZERO_HEADER {
        return None;
    }
    let address = function.config_address.checked_add(SUBSYSTEM_IDS_OFFSET)?;
    let value = access.read_u32(address);
    if value == u32::MAX {
        return None;
    }
    Some(GraphicsSubsystem {
        vendor_id: value as u16,
        device_id: (value >> 16) as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeConfig {
        subsystem_address: u64,
        subsystem_value: u32,
    }

    impl ConfigAccess for FakeConfig {
        fn read_u32(&self, address: u64) -> u32 {
            if address == self.subsystem_address {
                return self.subsystem_value;
            }
            u32::MAX
        }
    }

    fn pci(vendor_id: u16, device_id: u16, class_code: u8) -> PcieFunction {
        PcieFunction {
            segment_group: 0,
            bus: 0,
            device: 5,
            function: 0,
            vendor_id,
            device_id,
            class_code,
            subclass: 0,
            programming_interface: 0,
            revision_id: 1,
            header_type: 0,
            config_address: 0x8002_8000,
        }
    }

    fn config(subsystem_vendor: u16, subsystem_device: u16) -> FakeConfig {
        FakeConfig {
            subsystem_address: 0x8002_802c,
            subsystem_value: u32::from(subsystem_vendor) | (u32::from(subsystem_device) << 16),
        }
    }

    #[test]
    fn firmware_to_virtual_driver_to_shutdown_is_one_way() {
        let mut handoff = GraphicsOutputHandoff::new();
        assert!(handoff.firmware_boot_allowed());
        assert!(!handoff.native_output_allowed());
        assert_eq!(handoff.activate(true), Err(GraphicsOutputError::InvalidTransition));
        assert_eq!(handoff.begin(GraphicsProfile::AmdCezanne), Err(GraphicsOutputError::UnsupportedProfile));
        handoff.begin(GraphicsProfile::VirtioGpu).unwrap();
        assert!(!handoff.firmware_boot_allowed());
        assert_eq!(handoff.activate(false), Err(GraphicsOutputError::DriverNotReady));
        handoff.activate(true).unwrap();
        assert!(handoff.native_output_allowed());
        assert!(!handoff.firmware_boot_allowed());
        assert!(!handoff.firmware_emergency_allowed());
        assert_eq!(handoff.stop(false), Err(GraphicsOutputError::DriverNotReady));
        handoff.stop(true).unwrap();
        assert_eq!(handoff.phase(), GraphicsOutputPhase::DriverStopped);
        assert!(!handoff.firmware_boot_allowed());
        assert!(!handoff.native_output_allowed());
        assert_eq!(handoff.begin(GraphicsProfile::VirtioGpu), Err(GraphicsOutputError::InvalidTransition));
    }

    #[test]
    fn failure_enters_emergency_without_reopening_normal_gop() {
        let mut handoff = GraphicsOutputHandoff::new();
        handoff.begin(GraphicsProfile::VirtioGpu).unwrap();
        handoff.emergency().unwrap();
        assert!(handoff.firmware_emergency_allowed());
        assert!(!handoff.firmware_boot_allowed());
        assert!(!handoff.native_output_allowed());
        assert_eq!(handoff.activate(true), Err(GraphicsOutputError::InvalidTransition));
        assert_eq!(handoff.stop(true), Err(GraphicsOutputError::InvalidTransition));
    }

    #[test]
    fn running_driver_failure_can_enable_emergency_only() {
        let mut handoff = GraphicsOutputHandoff::new();
        handoff.begin(GraphicsProfile::VirtioGpu).unwrap();
        handoff.activate(true).unwrap();
        handoff.emergency().unwrap();
        assert_eq!(handoff.phase(), GraphicsOutputPhase::EmergencyFirmware);
        assert!(handoff.firmware_emergency_allowed());
    }

    #[test]
    fn recognizes_both_asus_laptop_devices_and_keeps_integrated_first() {
        let mut inventory = GraphicsInventory::empty();
        let amd = pci(AMD_VENDOR_ID, AMD_CEZANNE_DEVICE_ID, DISPLAY_CLASS);
        let mut nvidia = pci(NVIDIA_VENDOR_ID, NVIDIA_GTX_1650_DEVICE_ID, DISPLAY_CLASS);
        nvidia.subclass = 0x02;
        inventory.observe(&config(ASUS_VENDOR_ID, ASUS_NVIDIA_SUBSYSTEM_ID), nvidia);
        inventory.observe(&config(ASUS_VENDOR_ID, ASUS_AMD_SUBSYSTEM_ID), amd);
        assert_eq!(inventory.display_controllers, 2);
        assert_eq!(
            inventory.preferred_target().map(|device| device.profile),
            Some(GraphicsProfile::AmdCezanne)
        );
        assert!(inventory.amd_cezanne.unwrap().is_reference_asus());
        assert!(inventory.nvidia_gtx1650.unwrap().is_reference_asus());
        assert!(inventory.firmware_output_required());
    }

    #[test]
    fn virtual_gpu_is_selected_without_physical_target() {
        let mut inventory = GraphicsInventory::empty();
        let virtio = pci(VIRTIO_VENDOR_ID, VIRTIO_GPU_DEVICE_ID, DISPLAY_CLASS);
        inventory.observe(&config(VIRTIO_VENDOR_ID, 0x1100), virtio);
        assert_eq!(
            inventory.preferred_target().map(|device| device.profile),
            Some(GraphicsProfile::VirtioGpu)
        );
        assert!(!inventory.virtio_gpu.unwrap().is_reference_asus());
        assert!(GraphicsProfile::VirtioGpu.driver_ready());
        assert!(!inventory.firmware_output_required());
    }

    #[test]
    fn unknown_vga_and_wrong_classes_cannot_be_selected() {
        let mut inventory = GraphicsInventory::empty();
        inventory.observe(&config(0, 0), pci(0x1234, 0x1111, DISPLAY_CLASS));
        inventory.observe(
            &config(0, 0),
            pci(AMD_VENDOR_ID, AMD_CEZANNE_DEVICE_ID, 0x01),
        );
        inventory.observe(&config(0, 0), pci(VIRTIO_VENDOR_ID, 0x1041, DISPLAY_CLASS));
        assert_eq!(inventory.display_controllers, 2);
        assert_eq!(inventory.unknown_controllers, 2);
        assert!(inventory.preferred_target().is_none());
    }

    #[test]
    fn unsupported_nvidia_cannot_take_control_of_display() {
        let mut inventory = GraphicsInventory::empty();
        inventory.observe(
            &config(ASUS_VENDOR_ID, ASUS_NVIDIA_SUBSYSTEM_ID),
            pci(NVIDIA_VENDOR_ID, NVIDIA_GTX_1650_DEVICE_ID, DISPLAY_CLASS),
        );
        assert!(inventory.nvidia_gtx1650.is_some());
        assert!(inventory.preferred_target().is_none());
        assert!(inventory.firmware_output_required());
    }

    #[test]
    fn first_matching_gpu_is_stable_and_other_vendor_is_not_reference() {
        let mut inventory = GraphicsInventory::empty();
        let first = pci(AMD_VENDOR_ID, AMD_CEZANNE_DEVICE_ID, DISPLAY_CLASS);
        let mut second = first;
        second.bus = 1;
        inventory.observe(&config(0x1002, 0x11d3), first);
        inventory.observe(&config(ASUS_VENDOR_ID, ASUS_AMD_SUBSYSTEM_ID), second);
        assert_eq!(inventory.amd_cezanne.unwrap().function, first);
        assert!(!inventory.amd_cezanne.unwrap().is_reference_asus());
    }

    #[test]
    fn bridge_header_or_absent_subsystem_does_not_claim_asus_profile() {
        let mut inventory = GraphicsInventory::empty();
        let mut bridge = pci(AMD_VENDOR_ID, AMD_CEZANNE_DEVICE_ID, DISPLAY_CLASS);
        bridge.header_type = 1;
        inventory.observe(&config(ASUS_VENDOR_ID, ASUS_AMD_SUBSYSTEM_ID), bridge);
        assert_eq!(inventory.amd_cezanne.unwrap().subsystem, None);

        let mut empty_subsystem = GraphicsInventory::empty();
        empty_subsystem.observe(
            &FakeConfig {
                subsystem_address: u64::MAX,
                subsystem_value: u32::MAX,
            },
            pci(AMD_VENDOR_ID, AMD_CEZANNE_DEVICE_ID, DISPLAY_CLASS),
        );
        assert!(!empty_subsystem.amd_cezanne.unwrap().is_reference_asus());
    }

    #[test]
    fn physical_target_priority_is_independent_of_discovery_order() {
        let mut inventory = GraphicsInventory::empty();
        inventory.observe(
            &config(0x1af4, 0x1100),
            pci(VIRTIO_VENDOR_ID, VIRTIO_GPU_DEVICE_ID, DISPLAY_CLASS),
        );
        inventory.observe(
            &config(ASUS_VENDOR_ID, ASUS_AMD_SUBSYSTEM_ID),
            pci(AMD_VENDOR_ID, AMD_CEZANNE_DEVICE_ID, DISPLAY_CLASS),
        );
        assert_eq!(
            inventory.preferred_target().unwrap().profile,
            GraphicsProfile::AmdCezanne
        );
    }
}
