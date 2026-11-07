//! Определение контроллера xHCI через стандартные регистры PCIe.
//!
//! Модуль только читает конфигурацию: он не обращается к регистрам MMIO,
//! не включает DMA и не изменяет состояние контроллера.

use crate::{ConfigAccess, PcieFunction};

pub const USB_CLASS: u8 = 0x0c;
pub const USB_SUBCLASS: u8 = 0x03;
pub const XHCI_INTERFACE: u8 = 0x30;

const COMMAND_OFFSET: u64 = 0x04;
const BAR0_OFFSET: u64 = 0x10;
const BAR1_OFFSET: u64 = 0x14;
const PCI_COMMAND_MEMORY_ENABLE: u32 = 1 << 1;
const PCI_COMMAND_BUS_MASTER: u32 = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XhciPciError {
    NotXhci,
    IoBar,
    UnsupportedBarType,
    MissingBar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XhciPciController {
    pub function: PcieFunction,
    pub mmio_base: u64,
    pub uses_64_bit_bar: bool,
    pub memory_enabled: bool,
    pub bus_master_enabled: bool,
}

pub const fn is_xhci(function: PcieFunction) -> bool {
    function.class_code == USB_CLASS
        && function.subclass == USB_SUBCLASS
        && function.programming_interface == XHCI_INTERFACE
}

/// Читает BAR0 и регистр PCI Command без пробных записей в BAR.
///
/// Адрес BAR — физический. Перед чтением MMIO ядро обязано создать
/// корректное некэшируемое отображение и проверить его размер.
pub fn probe<A: ConfigAccess>(
    access: &A,
    function: PcieFunction,
) -> Result<XhciPciController, XhciPciError> {
    if !is_xhci(function) {
        return Err(XhciPciError::NotXhci);
    }

    let base = function.config_address;
    let command = access.read_u32(base + COMMAND_OFFSET);
    let bar0 = access.read_u32(base + BAR0_OFFSET);
    if bar0 & 1 != 0 {
        return Err(XhciPciError::IoBar);
    }

    let uses_64_bit_bar = match (bar0 >> 1) & 3 {
        0 => false,
        2 => true,
        _ => return Err(XhciPciError::UnsupportedBarType),
    };
    let upper = if uses_64_bit_bar {
        u64::from(access.read_u32(base + BAR1_OFFSET)) << 32
    } else {
        0
    };
    let mmio_base = upper | u64::from(bar0 & !0xf);
    if mmio_base == 0 {
        return Err(XhciPciError::MissingBar);
    }

    Ok(XhciPciController {
        function,
        mmio_base,
        uses_64_bit_bar,
        memory_enabled: command & PCI_COMMAND_MEMORY_ENABLE != 0,
        bus_master_enabled: command & PCI_COMMAND_BUS_MASTER != 0,
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
        fn read_u32(&self, address: u64) -> u32 {
            match address {
                0x1004 => self.command,
                0x1010 => self.bar0,
                0x1014 => self.bar1,
                _ => u32::MAX,
            }
        }
    }

    fn controller() -> PcieFunction {
        PcieFunction {
            segment_group: 0,
            bus: 0,
            device: 4,
            function: 0,
            vendor_id: 0x1b36,
            device_id: 0x000d,
            class_code: USB_CLASS,
            subclass: USB_SUBCLASS,
            programming_interface: XHCI_INTERFACE,
            revision_id: 1,
            header_type: 0,
            config_address: 0x1000,
        }
    }

    #[test]
    fn reads_64_bit_bar_and_pci_command() {
        let config = FakeConfig {
            command: 0x0006,
            bar0: 0xfee0_0004,
            bar1: 0x0000_0001,
        };
        let result = probe(&config, controller()).unwrap();
        assert_eq!(result.mmio_base, 0x0000_0001_fee0_0000);
        assert!(result.uses_64_bit_bar);
        assert!(result.memory_enabled);
        assert!(result.bus_master_enabled);
    }

    #[test]
    fn accepts_32_bit_bar_without_mastering() {
        let config = FakeConfig {
            command: 0x0002,
            bar0: 0xfebf_0000,
            bar1: u32::MAX,
        };
        let result = probe(&config, controller()).unwrap();
        assert_eq!(result.mmio_base, 0xfebf_0000);
        assert!(!result.uses_64_bit_bar);
        assert!(result.memory_enabled);
        assert!(!result.bus_master_enabled);
    }

    #[test]
    fn rejects_invalid_registers_and_non_xhci() {
        let mut function = controller();
        function.programming_interface = 0x20;
        let config = FakeConfig {
            command: 0,
            bar0: 0,
            bar1: 0,
        };
        assert_eq!(probe(&config, function), Err(XhciPciError::NotXhci));
        function.programming_interface = XHCI_INTERFACE;
        assert_eq!(probe(&config, function), Err(XhciPciError::MissingBar));

        let io_bar = FakeConfig {
            bar0: 0xf000_1001,
            ..config
        };
        assert_eq!(probe(&io_bar, function), Err(XhciPciError::IoBar));

        let reserved_bar = FakeConfig {
            bar0: 0xf000_1002,
            ..config
        };
        assert_eq!(
            probe(&reserved_bar, function),
            Err(XhciPciError::UnsupportedBarType)
        );
    }
}
