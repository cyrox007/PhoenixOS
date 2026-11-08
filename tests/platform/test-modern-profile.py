#!/usr/bin/env python3
"""Проверки запрета возврата BIOS/IDE/PATA и старого USB в QEMU."""

import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
QEMU_SCRIPT = ROOT / "scripts" / "qemu-hotplug.py"
BUILD_SCRIPT = ROOT / "build.rs"


def load_qemu():
    specification = importlib.util.spec_from_file_location("phoenix_qemu_hotplug", QEMU_SCRIPT)
    if specification is None or specification.loader is None:
        raise RuntimeError("Нельзя загрузить сценарий проверки QEMU")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


QEMU = load_qemu()


class ModernProfileTests(unittest.TestCase):
    def setUp(self):
        self.arguments = QEMU.build_qemu_command(
            "phoenixos-uefi.img",
            "OVMF_CODE.fd",
            "OVMF_VARS.fd",
            "phoenix-nvme.img",
            "monitor.sock",
        )

    def check_rejected(self, arguments):
        with self.assertRaises(ValueError):
            QEMU.validate_modern_qemu_profile(arguments)

    def without_device(self, device):
        arguments = list(self.arguments)
        index = arguments.index(device)
        self.assertEqual(arguments[index - 1], "-device")
        del arguments[index - 1:index + 1]
        return arguments

    def test_uefi_boot_and_modern_devices(self):
        QEMU.validate_modern_qemu_profile(self.arguments)
        text = BUILD_SCRIPT.read_text(encoding="utf-8")
        self.assertIn("bootloader::UefiBoot::new(&kernel)", text)
        self.assertNotIn("bootloader::BiosBoot", text)
        self.assertTrue(any("drive=phoenix-boot" in item and "bootindex=1" in item for item in self.arguments))

    def test_implicit_legacy_devices_are_forbidden(self):
        self.check_rejected([part for part in self.arguments if part != "-nodefaults"])
        self.check_rejected(self.arguments + ["-hda", "old-disk.img"])
        self.check_rejected(self.arguments + ["-bios", "legacy.rom"])

    def test_ide_and_pata_are_not_reintroduced(self):
        self.check_rejected(self.arguments + ["-drive", "if=ide,format=raw,file=old.img"])
        self.check_rejected(self.arguments + ["-device", "ide-hd,drive=old"])
        self.check_rejected(self.arguments + ["-device", "piix3-ide"])
        self.check_rejected(self.arguments + ["-drive", "file=old.img,format=raw"])

    def test_uhci_ohci_and_ehci_are_not_reintroduced(self):
        for name in ("usb-uhci", "usb-ohci", "usb-ehci", "ich9-ehci1", "ich9-uhci1"):
            with self.subTest(controller=name):
                self.check_rejected(self.arguments + ["-device", name + ",id=old-usb"])

    def test_requires_modern_q35_nvme_xhci_and_nvme_boot(self):
        modern = list(self.arguments)
        machine = modern.index("-machine") + 1
        modern[machine] = "pc,accel=kvm"
        self.check_rejected(modern)

        self.check_rejected(
            self.without_device("qemu-xhci,id=phoenix-xhci")
        )
        self.check_rejected(
            self.without_device("nvme,id=phoenix-boot-nvme,serial=PhoenixBoot01,drive=phoenix-boot,bootindex=1")
        )
        self.check_rejected(
            self.without_device("nvme,id=phoenix-nvme,serial=PhoenixNVM01,drive=phoenix-nvm")
        )

    def test_pflash_required_for_both_firmware_regions(self):
        missing = list(self.arguments)
        index = next(
            i for i, value in enumerate(missing) if value.startswith("if=pflash") and "unit=1" in value
        )
        del missing[index - 1:index + 1]
        self.check_rejected(missing)


if __name__ == "__main__":
    unittest.main(verbosity=2)
