#!/usr/bin/env python3
"""Единый образ для аппаратной приёмки не должен превращать пассивную диагностику в активную."""

from pathlib import Path
import importlib.util
import unittest

ROOT = Path(__file__).resolve().parents[2]


class AsusAcceptanceImageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.kernel = (ROOT / "diagnostic-kernel/src/main.rs").read_text()
        cls.smp = (ROOT / "diagnostic-kernel/src/smp_probe.rs").read_text()
        cls.manifest = (ROOT / "diagnostic-kernel/Cargo.toml").read_text()
        cls.image = (ROOT / "tools/asus-acceptance-image/Cargo.toml").read_text()
        cls.builder = (ROOT / "tools/asus-acceptance-image/build.rs").read_text()
        cls.ci = (ROOT / ".github/workflows/ci.yml").read_text()
        spec = importlib.util.spec_from_file_location(
            "diagnostic_runner", ROOT / "scripts/run-diagnostic-qemu.py"
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        cls.runner = module

    def test_separate_passive_and_active_images(self):
        self.assertIn('asus-acceptance = ["dep:phoenix-memory"]', self.manifest)
        self.assertIn('#[cfg(feature = "asus-acceptance")]\nmod smp_probe;', self.kernel)
        self.assertIn('#[cfg(feature = "asus-acceptance")]\n    run_processor_acceptance', self.kernel)
        self.assertIn('features = ["asus-acceptance"]', self.image)
        self.assertIn("phoenixos-asus-acceptance-uefi.img", self.builder)
        self.assertIn("phoenixos-asus-acceptance-uefi", self.ci)

    def test_no_dma_and_protected_storage(self):
        for forbidden in ("phoenix-dma =", "phoenix-reference-edu =", "phoenix-fs =",
                          "nvme_admin", "nvme_fs", "xhci_driver"):
            self.assertNotIn(forbidden, self.manifest)
            self.assertNotIn(forbidden, self.image)
        self.assertIn("dma_authorized=false", self.kernel)
        self.assertIn("no-verified-physical-controller-and-iommu-domain", self.kernel)
        self.assertNotIn("dma_authorized=true", self.kernel)
        self.assertNotIn("NVMe", self.smp)

    def test_multiprocessor_readout_not_simulated(self):
        self.assertIn("send_ipi(*id, 0xc500)", self.smp)
        self.assertIn("send_ipi(*id, 0x4600", self.smp)
        self.assertIn("read_msr(0x277)", self.smp)
        self.assertIn("read_msr(0xc001_0010)", self.smp)
        self.assertIn("read_msr(0xc001_001d)", self.smp)
        self.assertIn("sample.amd_observed = true", self.smp)
        self.assertIn("supported_platform()", self.smp)
        self.assertIn("InvalidTopology", self.smp)
        self.assertIn("ApDidNotStart", self.smp)

    def test_qemu_requires_all_cores_and_dma_block(self):
        required = set(self.runner.ACCEPTANCE_MARKERS)
        self.assertIn(
            "diagnostic AP snapshot: OK measured=4 expected=4 distinct=true cache=true cr3=true dma_authorized=false",
            required,
        )
        self.assertTrue(any("acceptance 16/17: PARTIAL" in marker for marker in required))
        self.assertTrue(any("acceptance 17/17: BLOCKED" in marker for marker in required))
        command = " ".join(self.runner.build_command("test.img", "code.fd", "vars.fd"))
        self.assertIn("file=test.img,readonly=on", command)
        self.assertNotIn("edu,", command)
        self.assertNotIn("usb-storage", command)


if __name__ == "__main__":
    unittest.main(verbosity=2)
