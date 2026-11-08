//! Проверяемая политика устройств этапа 0.3.
//!
//! Устаревшее устройство может присутствовать в конфигурации PCI,
//! но ядро не создаёт для него рабочий драйвер и не включает Bus Master.
//! Контроллеры обнаруживаются без записи в BAR или регистры устройства.

use crate::{PcieFunction, nvme, xhci};

const STORAGE_CLASS: u8 = 0x01;
const IDE_SUBCLASS: u8 = 0x01;
const USB_CLASS: u8 = 0x0c;
const USB_SUBCLASS: u8 = 0x03;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyController {
    IdePata,
    Uhci,
    Ohci,
    Ehci,
}

impl LegacyController {
    pub const fn name(self) -> &'static str {
        match self {
            Self::IdePata => "ide-pata",
            Self::Uhci => "uhci",
            Self::Ohci => "ohci",
            Self::Ehci => "ehci",
        }
    }
}

/// Контроллер может быть перечислен PCIe, но не должен получать
/// рабочий путь в ядре, ни доступ к DMA, ни обработчик прерывания.
pub const fn legacy_controller(function: PcieFunction) -> Option<LegacyController> {
    match (
        function.class_code,
        function.subclass,
        function.programming_interface,
    ) {
        (STORAGE_CLASS, IDE_SUBCLASS, _) => Some(LegacyController::IdePata),
        (USB_CLASS, USB_SUBCLASS, 0x00) => Some(LegacyController::Uhci),
        (USB_CLASS, USB_SUBCLASS, 0x10) => Some(LegacyController::Ohci),
        (USB_CLASS, USB_SUBCLASS, 0x20) => Some(LegacyController::Ehci),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingController {
    Nvme,
    Xhci,
}

/// Единственный поддерживаемый выбор среди контроллеров хранения
/// и USB для этапа 0.3. Не означает, что любой другой класс PCI
/// запрещён (например, VirtIO-GPU и EDU остаются независимыми).
pub const fn supported_storage_or_usb(function: PcieFunction) -> Option<WorkingController> {
    if legacy_controller(function).is_some() {
        return None;
    }

    if nvme::is_nvme(function) {
        return Some(WorkingController::Nvme);
    }

    if xhci::is_xhci(function) {
        return Some(WorkingController::Xhci);
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyPciAudit {
    pub ide_pata: u32,
    pub uhci: u32,
    pub ohci: u32,
    pub ehci: u32,
    pub nvme: u32,
    pub xhci: u32,
}

impl LegacyPciAudit {
    pub const fn new() -> Self {
        Self {
            ide_pata: 0,
            uhci: 0,
            ohci: 0,
            ehci: 0,
            nvme: 0,
            xhci: 0,
        }
    }

    pub fn observe(&mut self, function: PcieFunction) {
        let selected = legacy_controller(function);
        match selected {
            Some(LegacyController::IdePata) => {
                self.ide_pata = self.ide_pata.saturating_add(1);
            }
            Some(LegacyController::Uhci) => {
                self.uhci = self.uhci.saturating_add(1);
            }
            Some(LegacyController::Ohci) => {
                self.ohci = self.ohci.saturating_add(1);
            }
            Some(LegacyController::Ehci) => {
                self.ehci = self.ehci.saturating_add(1);
            }
            None => {}
        }
        match supported_storage_or_usb(function) {
            Some(WorkingController::Nvme) => self.nvme = self.nvme.saturating_add(1),
            Some(WorkingController::Xhci) => self.xhci = self.xhci.saturating_add(1),
            None => {}
        }
    }

    pub const fn excluded_legacy_total(self) -> u32 {
        self.ide_pata
            .saturating_add(self.uhci)
            .saturating_add(self.ohci)
            .saturating_add(self.ehci)
    }
}

impl Default for LegacyPciAudit {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pci(class: u8, subclass: u8, interface: u8) -> PcieFunction {
        PcieFunction {
            segment_group: 0,
            bus: 0,
            device: 3,
            function: 0,
            vendor_id: 0x8086,
            device_id: 0x1234,
            class_code: class,
            subclass,
            programming_interface: interface,
            revision_id: 1,
            header_type: 0,
            config_address: 0x8000_0000,
        }
    }

    #[test]
    fn rejects_ide_pata_in_all_pci_programming_modes() {
        let mut audit = LegacyPciAudit::new();
        for interface in [0x00, 0x05, 0x8a, 0xff] {
            let function = pci(0x01, 0x01, interface);
            assert_eq!(legacy_controller(function), Some(LegacyController::IdePata));
            assert_eq!(supported_storage_or_usb(function), None);
            assert!(!nvme::is_nvme(function));
            audit.observe(function);
        }
        assert_eq!(audit.ide_pata, 4);
        assert_eq!(audit.excluded_legacy_total(), 4);
    }

    #[test]
    fn excludes_uhci_ohci_ehci_and_preserves_xhci() {
        let mut audit = LegacyPciAudit::new();
        for (interface, kind) in [
            (0x00, LegacyController::Uhci),
            (0x10, LegacyController::Ohci),
            (0x20, LegacyController::Ehci),
        ] {
            let function = pci(0x0c, 0x03, interface);
            assert_eq!(legacy_controller(function), Some(kind));
            assert_eq!(supported_storage_or_usb(function), None);
            assert!(!xhci::is_xhci(function));
            audit.observe(function);
        }
        let xhci_controller = pci(0x0c, 0x03, 0x30);
        assert_eq!(legacy_controller(xhci_controller), None);
        assert_eq!(
            supported_storage_or_usb(xhci_controller),
            Some(WorkingController::Xhci)
        );
        audit.observe(xhci_controller);
        assert_eq!(
            (audit.uhci, audit.ohci, audit.ehci, audit.xhci),
            (1, 1, 1, 1)
        );
        assert_eq!(audit.excluded_legacy_total(), 3);
    }

    #[test]
    fn nvme_remains_supported_and_sata_is_not_claimed_as_nvme() {
        let modern = pci(0x01, 0x08, 0x02);
        let sata = pci(0x01, 0x06, 0x01);
        assert_eq!(
            supported_storage_or_usb(modern),
            Some(WorkingController::Nvme)
        );
        assert_eq!(supported_storage_or_usb(sata), None);
        let mut audit = LegacyPciAudit::new();
        audit.observe(modern);
        audit.observe(sata);
        assert_eq!(audit.nvme, 1);
        assert_eq!(audit.ide_pata, 0);
    }

    #[test]
    fn unrelated_devices_are_not_misidentified() {
        let mut audit = LegacyPciAudit::new();
        for function in [
            pci(0x03, 0x00, 0x00),
            pci(0x06, 0x01, 0x00),
            pci(0x01, 0x00, 0x00),
        ] {
            assert_eq!(legacy_controller(function), None);
            assert_eq!(supported_storage_or_usb(function), None);
            audit.observe(function);
        }
        assert_eq!(audit.excluded_legacy_total(), 0);
        assert_eq!(audit.nvme, 0);
        assert_eq!(audit.xhci, 0);
    }
}
