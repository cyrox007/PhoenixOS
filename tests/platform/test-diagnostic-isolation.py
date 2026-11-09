#!/usr/bin/env python3
"""Запрет опасных действий в независимом диагностическом профиле."""

import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]


class DiagnosticIsolationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.kernel = (ROOT / "diagnostic-kernel/src/main.rs").read_text()
        cls.hardware = (ROOT / "diagnostic-kernel/src/hardware.rs").read_text()
        cls.dependencies = (ROOT / "diagnostic-kernel/Cargo.toml").read_text()
        path = ROOT / "scripts/run-diagnostic-qemu.py"
        spec = importlib.util.spec_from_file_location("diagnostic_qemu", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        cls.runner = module

    def test_diagnostic_kernel_is_independent_of_working_drivers(self):
        for forbidden in ("nvme_admin", "nvme_fs", "xhci_driver", "virtio_gpu",
                          "phoenix-fs", "phoenix-dma", "phoenix-reference-edu"):
            self.assertNotIn(forbidden, self.kernel)
            self.assertNotIn(forbidden, self.dependencies)
        self.assertIn("diagnostic complete: OK", self.kernel)

    def test_no_write_to_device_config_or_control_registers(self):
        # Проверка помогает обнаружить случайное подключение опасного кода,
        # но не заменяет аудит всех сторонних зависимостей и загрузчика.
        for forbidden in ("wrmsr", "write_volatile", "write_u32", "out dx",
                          "io::", "DmaBuffer::reserve", "NVMe"):
            self.assertNotIn(forbidden, self.hardware)
        self.assertIn("read_volatile", self.hardware)
        self.assertIn("read_msr", self.hardware)
        self.assertIn("read_msr(AMD_SYSCFG)", self.hardware)
        self.assertIn("read_msr(AMD_TOP_MEM2)", self.hardware)
        self.assertIn("ActivePageTable::from_current", self.hardware)
        self.assertIn("table.translate_addr", self.hardware)
        self.assertIn("table.pat_index", self.hardware)
        self.assertNotIn("map_4k(", self.hardware)
        self.assertNotIn("unmap_4k(", self.hardware)
        self.assertIn("audit_ram_regions", self.hardware)
        self.assertIn("RamAuditReport::default()", self.hardware)
        self.assertIn("ObservedMapping::Mapped", self.hardware)
        self.assertIn("scope=direct-only", self.hardware)
        self.assertIn("walk_current_cr3", self.hardware)
        self.assertIn("AliasAuditReport::default()", self.hardware)
        self.assertIn("blocking_reason(&evidence)", self.hardware)
        self.assertIn("diagnostic_dma_authorized(&evidence)", self.hardware)
        self.assertIn("parse_ivrs(bytes)", self.hardware)
        self.assertIn("busmaster_enabled=", self.hardware)
        self.assertNotIn("write_msr(", self.hardware)
        self.assertNotIn("Cr3::write", self.hardware)

    def test_boot_medium_is_read_only_and_no_test_disk(self):
        command = self.runner.build_command("diag.img", "code.fd", "vars.fd")
        joined = " ".join(command)
        self.assertIn("file=diag.img,readonly=on", joined)
        self.assertIn("file=code.fd,readonly=on", joined)
        self.assertNotIn("file=diag.img,readonly=off", joined)
        self.assertNotIn("virtio-blk", joined)
        self.assertNotIn("usb-storage", joined)
        self.assertNotIn("qemu-xhci", joined)
        self.assertNotIn("edu,", joined)
        self.assertEqual(joined.count("file=diag.img"), 1)

    def test_report_is_required_for_success(self):
        required = set(self.runner.MARKERS)
        self.assertIn("diagnostic mode: OK readonly=1", required)
        self.assertIn("device DMA: disabled", required)
        self.assertIn("diagnostic CPU: OK", required)
        self.assertIn("diagnostic cache: OK", required)
        self.assertIn("diagnostic cache sample: phys=", required)
        self.assertIn("WB-candidate=true", required)
        self.assertIn("diagnostic RAM audit: complete=true", required)
        self.assertIn("scope=direct-only", required)
        self.assertIn("diagnostic ACPI: OK", required)
        self.assertIn("diagnostic PCI: OK", required)
        self.assertIn("diagnostic complete: OK", required)


if __name__ == "__main__":
    unittest.main(verbosity=2)
