#!/usr/bin/env python3
"""Защита от запуска DMA на другом компьютере и ошибка кодировки PowerShell."""

import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "inventory", ROOT / "scripts" / "check-asus-inventory.py"
)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

SAMPLE = """PhoenixOS: read-only Windows hardware inventory
Computer: ASUSTeK COMPUTER INC. Vivobook_ASUSLaptop M6500QH_M6500QH
UEFI: M6500QH.311
CPU: AMD Ryzen 5 5600H with Radeon Graphics, cores=6, threads=12
PCI VEN=1C5C DEV=174A status=OK name=NVM Express controller
PCI VEN=1022 DEV=7901 status=OK name=AHCI controller
PCI VEN=10DE DEV=1F9D status=OK name=NVIDIA GTX 1650
PCI VEN=1002 DEV=1638 status=OK name=AMD Radeon Graphics
PCI VEN=14C3 DEV=7961 status=OK name=MediaTek Wi-Fi
Storage: HFM512GD3JX013N, interface=SCSI
BitLocker: C:, protection=Off
"""


class HardwareInventoryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profile = mod.load_profile()

    def test_observed_asus_never_authorizes_dma(self):
        data = mod.analyze(SAMPLE, self.profile)
        self.assertTrue(data["inventory_complete"])
        self.assertTrue(data["protected_nvme_present"])
        self.assertFalse(data["physical_dma_permitted"])
        self.assertFalse(data["acceptance_16"])
        self.assertFalse(data["acceptance_17"])
        self.assertIn("1C5C:174A", data["blocked_pci"])

    def test_legacy_russian_report_format(self):
        old = SAMPLE.replace("Computer:", "Ноутбук:").replace(
            "UEFI:", "Версия UEFI:"
        ).replace("CPU:", "Процессор:").replace(
            "cores=6, threads=12", "ядер=6, потоков=12"
        ).replace("Storage:", "Накопитель:").replace(
            ", interface=", ", интерфейс="
        )
        self.assertTrue(mod.analyze(old, self.profile)["inventory_complete"])

    def test_wrong_machine_fails_closed(self):
        report = SAMPLE.replace("M6500QH_M6500QH", "M7500_M7500")
        self.assertFalse(mod.analyze(report, self.profile)["inventory_complete"])

    def test_missing_nvme_fails_closed(self):
        report = SAMPLE.replace("PCI VEN=1C5C DEV=174A", "PCI VEN=1234 DEV=5678")
        result = mod.analyze(report, self.profile)
        self.assertFalse(result["inventory_complete"])
        self.assertFalse(result["protected_nvme_present"])
        self.assertFalse(result["physical_dma_permitted"])

    def test_wrong_firmware_or_core_count_fails_closed(self):
        for report in (
            SAMPLE.replace("M6500QH.311", "M6500QH.999"),
            SAMPLE.replace("cores=6", "cores=4"),
        ):
            self.assertFalse(mod.analyze(report, self.profile)["inventory_complete"])

    def test_powershell_5_1_script_is_ascii_safe(self):
        source = (ROOT / "scripts/collect-asus-evidence.ps1").read_bytes()
        self.assertTrue(source.isascii(), "Windows PowerShell 5.1 requires BOM for non-ASCII scripts")
        self.assertIn(b"Get-CimInstance", source)
        self.assertIn(b"Out-File", source)
        self.assertNotIn(b"Set-PnpDevice", source)
        self.assertNotIn(b"Set-Disk", source)


if __name__ == "__main__":
    unittest.main(verbosity=2)
