//! Обнаружение NVMe через PCIe и проверка регистров возможностей.
//!
//! Начальный этап исключительно читает PCIe и MMIO. Контроллер не
//! включается, очереди DMA не создаются и данные накопителя не изменяются.

use crate::{ConfigAccess, PcieFunction};

pub const STORAGE_CLASS: u8 = 0x01;
pub const NVM_SUBCLASS: u8 = 0x08;
pub const NVME_INTERFACE: u8 = 0x02;

const PCI_COMMAND_OFFSET: u64 = 0x04;
const PCI_BAR0_OFFSET: u64 = 0x10;
const PCI_BAR1_OFFSET: u64 = 0x14;
const PCI_COMMAND_MEMORY_ENABLE: u32 = 1 << 1;
const PCI_COMMAND_BUS_MASTER: u32 = 1 << 2;

pub const REGISTER_CAP_LOW: u64 = 0x00;
pub const REGISTER_CAP_HIGH: u64 = 0x04;
pub const REGISTER_VERSION: u64 = 0x08;
pub const REGISTER_CONFIGURATION: u64 = 0x14;
pub const REGISTER_STATUS: u64 = 0x1c;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvmePciError {
    NotNvme,
    IoBar,
    UnsupportedBarType,
    MissingBar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NvmePciController {
    pub function: PcieFunction,
    pub mmio_base: u64,
    pub uses_64_bit_bar: bool,
    pub memory_enabled: bool,
    pub bus_master_enabled: bool,
}

pub const fn is_nvme(function: PcieFunction) -> bool {
    function.class_code == STORAGE_CLASS
        && function.subclass == NVM_SUBCLASS
        && function.programming_interface == NVME_INTERFACE
}

/// Чтение BAR без изменения конфигурации устройства. Для проверки MMIO
/// ядро обязано отдельно создать некэшируемое отображение адреса BAR.
pub fn probe<A: ConfigAccess>(
    access: &A,
    function: PcieFunction,
) -> Result<NvmePciController, NvmePciError> {
    if !is_nvme(function) {
        return Err(NvmePciError::NotNvme);
    }

    let base = function.config_address;
    let command = access.read_u32(base + PCI_COMMAND_OFFSET);
    let bar0 = access.read_u32(base + PCI_BAR0_OFFSET);
    if bar0 & 1 != 0 {
        return Err(NvmePciError::IoBar);
    }

    let uses_64_bit_bar = match (bar0 >> 1) & 3 {
        0 => false,
        2 => true,
        _ => return Err(NvmePciError::UnsupportedBarType),
    };
    let upper = if uses_64_bit_bar {
        u64::from(access.read_u32(base + PCI_BAR1_OFFSET)) << 32
    } else {
        0
    };
    let mmio_base = upper | u64::from(bar0 & !0xf);
    if mmio_base == 0 {
        return Err(NvmePciError::MissingBar);
    }

    Ok(NvmePciController {
        function,
        mmio_base,
        uses_64_bit_bar,
        memory_enabled: command & PCI_COMMAND_MEMORY_ENABLE != 0,
        bus_master_enabled: command & PCI_COMMAND_BUS_MASTER != 0,
    })
}

/// Чтение 32-разрядных аппаратных регистров по смещениям относительно BAR0.
pub trait RegisterAccess {
    fn read_u32(&self, offset: u64) -> u32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvmeCapabilityError {
    InvalidCapabilities,
    InvalidVersion,
    NoNvmCommandSet,
    UnsupportedPageSize,
    ControllerFault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NvmeCapabilities {
    pub version_major: u16,
    pub version_minor: u8,
    pub version_tertiary: u8,
    pub maximum_queue_entries: u32,
    pub contiguous_queues_required: bool,
    pub timeout_half_seconds: u8,
    pub doorbell_stride_bytes: u32,
    pub minimum_page_size: u64,
    pub maximum_page_size: u64,
    pub controller_enabled: bool,
    pub controller_ready: bool,
}

/// CAP, VS, CC и CSTS — только чтение. Не требует предварительного
/// сброса и не изменяет состояние прошивки или содержимое накопителя.
pub fn read_capabilities<A: RegisterAccess>(
    registers: &A,
) -> Result<NvmeCapabilities, NvmeCapabilityError> {
    let cap_low = u64::from(registers.read_u32(REGISTER_CAP_LOW));
    let cap_high = u64::from(registers.read_u32(REGISTER_CAP_HIGH));
    let cap = cap_low | (cap_high << 32);
    let version = registers.read_u32(REGISTER_VERSION);
    let configuration = registers.read_u32(REGISTER_CONFIGURATION);
    let status = registers.read_u32(REGISTER_STATUS);

    let queue_max_zero_based = (cap & 0xffff) as u32;
    if cap == 0 || cap == u64::MAX || queue_max_zero_based == 0 {
        return Err(NvmeCapabilityError::InvalidCapabilities);
    }
    let major = (version >> 16) as u16;
    if major == 0 || major == u16::MAX {
        return Err(NvmeCapabilityError::InvalidVersion);
    }
    if cap & (1 << 37) == 0 {
        return Err(NvmeCapabilityError::NoNvmCommandSet);
    }

    let min_page_shift = ((cap >> 48) & 0xf) as u32;
    let max_page_shift = ((cap >> 52) & 0xf) as u32;
    if min_page_shift > max_page_shift || min_page_shift != 0 {
        return Err(NvmeCapabilityError::UnsupportedPageSize);
    }
    if status & (1 << 1) != 0 {
        return Err(NvmeCapabilityError::ControllerFault);
    }

    Ok(NvmeCapabilities {
        version_major: major,
        version_minor: (version >> 8) as u8,
        version_tertiary: version as u8,
        maximum_queue_entries: queue_max_zero_based + 1,
        contiguous_queues_required: cap & (1 << 16) != 0,
        timeout_half_seconds: (cap >> 24) as u8,
        doorbell_stride_bytes: 4 << ((cap >> 32) & 0xf),
        minimum_page_size: 4096 << min_page_shift,
        maximum_page_size: 4096 << max_page_shift,
        controller_enabled: configuration & 1 != 0,
        controller_ready: status & 1 != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeConfig {
        command: u32,
        bar0: u32,
        bar1: u32,
    }

    impl ConfigAccess for FakeConfig {
        fn read_u32(&self, offset: u64) -> u32 {
            match offset {
                0x1004 => self.command,
                0x1010 => self.bar0,
                0x1014 => self.bar1,
                _ => u32::MAX,
            }
        }
    }

    fn function() -> PcieFunction {
        PcieFunction {
            segment_group: 0,
            bus: 0,
            device: 4,
            function: 0,
            vendor_id: 0x1b36,
            device_id: 0x0010,
            class_code: STORAGE_CLASS,
            subclass: NVM_SUBCLASS,
            programming_interface: NVME_INTERFACE,
            revision_id: 1,
            header_type: 0,
            config_address: 0x1000,
        }
    }

    #[test]
    fn reads_64_bit_bar_and_pci_command() {
        let result = probe(
            &FakeConfig {
                command: 0x0006,
                bar0: 0x3800_0004,
                bar1: 0x0000_0001,
            },
            function(),
        )
        .unwrap();
        assert_eq!(result.mmio_base, 0x0000_0001_3800_0000);
        assert!(result.uses_64_bit_bar);
        assert!(result.memory_enabled);
        assert!(result.bus_master_enabled);
    }

    #[test]
    fn reads_32_bit_bar_and_disabled_bus_master() {
        let result = probe(
            &FakeConfig {
                command: 0x0002,
                bar0: 0xfeb0_0000,
                bar1: u32::MAX,
            },
            function(),
        )
        .unwrap();
        assert_eq!(result.mmio_base, 0xfeb0_0000);
        assert!(!result.uses_64_bit_bar);
        assert!(!result.bus_master_enabled);
    }

    #[test]
    fn rejects_unexpected_class_and_bars() {
        let mut wrong = function();
        wrong.programming_interface = 1;
        let access = FakeConfig {
            command: 0,
            bar0: 0,
            bar1: 0,
        };
        assert_eq!(probe(&access, wrong), Err(NvmePciError::NotNvme));
        assert_eq!(probe(&access, function()), Err(NvmePciError::MissingBar));
        assert_eq!(
            probe(
                &FakeConfig {
                    bar0: 0xf000_0001,
                    ..access
                },
                function()
            ),
            Err(NvmePciError::IoBar)
        );
        assert_eq!(
            probe(
                &FakeConfig {
                    bar0: 0xf000_0002,
                    ..access
                },
                function()
            ),
            Err(NvmePciError::UnsupportedBarType)
        );
    }

    #[derive(Clone, Copy)]
    struct FakeRegisters {
        cap: u64,
        version: u32,
        configuration: u32,
        status: u32,
    }

    impl RegisterAccess for FakeRegisters {
        fn read_u32(&self, offset: u64) -> u32 {
            match offset {
                REGISTER_CAP_LOW => self.cap as u32,
                REGISTER_CAP_HIGH => (self.cap >> 32) as u32,
                REGISTER_VERSION => self.version,
                REGISTER_CONFIGURATION => self.configuration,
                REGISTER_STATUS => self.status,
                _ => u32::MAX,
            }
        }
    }

    fn supported() -> FakeRegisters {
        FakeRegisters {
            cap: (1 << 37) | (1 << 16) | (10 << 24) | (2 << 32) | 63,
            version: 0x0001_0400,
            configuration: 1,
            status: 1,
        }
    }

    #[test]
    fn decodes_registers_without_writing_to_controller() {
        let parsed = read_capabilities(&supported()).unwrap();
        assert_eq!(parsed.version_major, 1);
        assert_eq!(parsed.version_minor, 4);
        assert_eq!(parsed.maximum_queue_entries, 64);
        assert!(parsed.contiguous_queues_required);
        assert_eq!(parsed.timeout_half_seconds, 10);
        assert_eq!(parsed.doorbell_stride_bytes, 16);
        assert_eq!(parsed.minimum_page_size, 4096);
        assert_eq!(parsed.maximum_page_size, 4096);
        assert!(parsed.controller_enabled);
        assert!(parsed.controller_ready);
    }

    #[test]
    fn rejects_invalid_or_unsupported_capabilities() {
        let baseline = supported();
        assert_eq!(
            read_capabilities(&FakeRegisters { cap: 0, ..baseline }),
            Err(NvmeCapabilityError::InvalidCapabilities)
        );
        assert_eq!(
            read_capabilities(&FakeRegisters { version: 0, ..baseline }),
            Err(NvmeCapabilityError::InvalidVersion)
        );
        assert_eq!(
            read_capabilities(&FakeRegisters {
                cap: 63,
                ..baseline
            }),
            Err(NvmeCapabilityError::NoNvmCommandSet)
        );
        assert_eq!(
            read_capabilities(&FakeRegisters {
                cap: (1 << 37) | (1 << 48) | (1 << 52) | 63,
                ..baseline
            }),
            Err(NvmeCapabilityError::UnsupportedPageSize)
        );
        assert_eq!(
            read_capabilities(&FakeRegisters { status: 2, ..baseline }),
            Err(NvmeCapabilityError::ControllerFault)
        );
    }
}
