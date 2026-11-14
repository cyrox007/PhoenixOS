#!/usr/bin/env python3
"""Проверки запрета физического DMA и полноты профиля виртуального AMD-Vi."""

import unittest
from importlib import util
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/run-amd-iommu-qemu.py"
SPEC = util.spec_from_file_location("isolated_amd_iommu_runner", SCRIPT)
RUNNER = util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class AmdIommuIsolationTests(unittest.TestCase):
    def test_virtual_machine_has_no_host_disk_and_uses_amd_vi(self):
        command = RUNNER.build_command("readonly.img", "OVMF_CODE.fd", "OVMF_VARS.fd")
        joined = " ".join(command)
        self.assertIn("amd-iommu,dma-remap=on,intremap=off", joined)
        self.assertIn("edu,id=phoenix-dma-only", joined)
        self.assertIn("file=readonly.img,readonly=on", joined)
        self.assertIn("file=OVMF_CODE.fd,readonly=on", joined)
        self.assertIn("accel=kvm", joined)
        self.assertNotIn("file=/dev/nvme", joined)
        self.assertNotIn("file=/dev/sda", joined)
        self.assertNotIn("vfio-pci", joined)

    def test_required_markers_prove_isolation_and_cleanup(self):
        self.assertTrue(any("translated=true" in line for line in RUNNER.MARKERS))
        self.assertTrue(any("forbidden_page_blocked=true" in line for line in RUNNER.MARKERS))
        self.assertTrue(any("shutdown=true" in line for line in RUNNER.MARKERS))
        self.assertTrue(any("physical_asus=denied" in line for line in RUNNER.MARKERS))

    def test_real_hardware_driver_is_not_linked_into_physical_diagnostics(self):
        diagnostic = (ROOT / "diagnostic-kernel/Cargo.toml").read_text(encoding="utf-8")
        probe = (ROOT / "diagnostic-kernel/src/hardware.rs").read_text(encoding="utf-8")
        self.assertNotIn("amd_iommu_test", diagnostic)
        self.assertNotIn("amd_iommu_test", probe)
        self.assertIn("diagnostic_dma_authorized", probe)

    def test_amd_driver_requires_real_hardware_queue_completion(self):
        module = (ROOT / "crates/dma/src/amd_iommu_v1.rs").read_text(encoding="utf-8")
        for expected in (
            "trait AmdIommuIo",
            "trait StoppedDevice",
            "fn issue_commands(",
            "fn device_in_private_domain(",
            "fn hardware_active(",
            "pub fn shutdown(",
            "HardwareError::CommandTimeout",
            "HardwareError::UnsafeExistingConfiguration",
            "HardwareError::MappingAlreadyPresent",
        ):
            self.assertIn(expected, module)
        self.assertNotIn("write_volatile", module)
        # Аппаратный драйвер должен иметь явную проверку QEMU при вызове.
        test = (ROOT / "dma-test-kernel/src/main.rs").read_text(encoding="utf-8")
        self.assertLess(test.index("if !is_test_hypervisor()"), test.index("amd_iommu_test::verify("))


if __name__ == "__main__":
    unittest.main(verbosity=2)
