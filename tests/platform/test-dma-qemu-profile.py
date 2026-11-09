#!/usr/bin/env python3
"""Не допускать возвращения штатных дисков и опасного физического профиля."""

import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
path = ROOT / "scripts/run-dma-test-qemu.py"
spec = importlib.util.spec_from_file_location("dma_qemu", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class SeparateDmaImageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.kernel = (ROOT / "dma-test-kernel/src/main.rs").read_text()
        cls.toml = (ROOT / "dma-test-kernel/Cargo.toml").read_text()
        cls.image = (ROOT / "tools/dma-test-image/build.rs").read_text()

    def test_single_read_only_boot_device_and_one_edu(self):
        command = module.build_command("dma-only.img", "OVMF_CODE.fd", "OVMF_VARS.fd")
        values = [
            command[index + 1]
            for index, key in enumerate(command[:-1])
            if key == "-drive"
        ]
        self.assertEqual(len(values), 3)
        self.assertEqual(sum("if=none" in value for value in values), 1)
        self.assertIn("file=dma-only.img,readonly=on", values[-1])
        self.assertTrue(values[0].endswith("readonly=on"))
        self.assertEqual(sum("edu," in value for value in command), 1)
        self.assertIn("q35,accel=kvm", command)
        self.assertIn("-nodefaults", command)
        self.assertIn("-smp", command)
        self.assertEqual(command[command.index("-smp") + 1], "1")
        self.assertFalse(any("usb" in value or "virtio-blk" in value for value in command))

    def test_dedicated_kernel_not_main_image_or_driver_stack(self):
        self.assertIn('path = "src/main.rs"', self.toml)
        self.assertIn("CARGO_BIN_FILE_PHOENIX_DMA_TEST_KERNEL_", self.image)
        self.assertNotIn("CARGO_BIN_FILE_KERNEL_kernel", self.image)
        for forbidden in ("nvme_admin", "nvme_fs", "xhci_driver", "phoenix-fs", "nvme_driver"):
            self.assertNotIn(forbidden, self.kernel)
            self.assertNotIn(forbidden, self.toml)

    def test_hypervisor_and_exact_edu_gate_before_hardware_writes(self):
        source = self.kernel
        self.assertIn('b"KVMKVMKVM', source)
        self.assertIn("leaf.ecx & (1 << 31)", source)
        self.assertLess(source.index("if !is_test_hypervisor()"), source.index("initialize_edu_interrupt().expect"))
        self.assertLess(source.index("find_unique_edu(&acpi.mcfg"), source.index("initialize_edu_interrupt().expect"))
        self.assertIn("matches != 1", source)
        self.assertIn("EDU_VENDOR_ID", source)
        self.assertIn("EDU_DEVICE_ID", source)

    def test_actual_dma_not_modelled_success(self):
        self.assertIn("reference_edu::self_test", self.kernel)
        self.assertIn("result.verified_bytes", self.kernel)
        self.assertIn("result.returned_dma_pages", self.kernel)
        self.assertIn("irq_mask", self.kernel)
        self.assertIn("physical-asus=denied iommu-domain=unverified", self.kernel)
        self.assertEqual(len(module.MARKERS), 4)


if __name__ == "__main__":
    unittest.main(verbosity=2)
