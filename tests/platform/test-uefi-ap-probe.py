#!/usr/bin/env python3
"""Отказные проверки независимой UEFI-диагностики процессоров."""

from importlib import util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/run-uefi-ap-probe.py"
SPEC = util.spec_from_file_location("uefi_ap_probe", SCRIPT)
MODULE = util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def successful_trace(count=4):
    lines = ["AP_PROBE mode=UEFI-QEMU readonly=true hardware_dma=false"]
    for cpu in range(count):
        lines.append(
            f"AP_PROBE cpu={cpu} apic={cpu} cr3=0x1000 "
            "pat=0x7040600070406 mtrr_default=0xc00 "
            "mtrr_count=8 amd_observed=false valid=true"
        )
    lines.append(
        f"AP_PROBE result=OK total={count} enabled={count} captured={count} "
        "unique=true cache_consensus=true phase=uefi-before-exit "
        "kernel_cr3_verified=false dma_authorized=false"
    )
    return lines


class FirmwareProbeSafetyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.source = (ROOT / "tools/uefi-ap-probe/src/main.rs").read_text(encoding="utf-8")
        cls.manifest = (ROOT / "tools/uefi-ap-probe/Cargo.toml").read_text(encoding="utf-8")

    def test_only_qemu_may_run_mp_services(self):
        self.assertIn("if !known_qemu()", self.source)
        self.assertLess(self.source.index("if !known_qemu()"), self.source.index("    execute()"))
        self.assertIn("KVMKVMKVM", self.source)
        self.assertIn("TCGTCGTCGTCG", self.source)
        self.assertIn("startup_all_aps(", self.source)
        self.assertIn("get_processor_info(", self.source)
        self.assertIn("read_msr(", self.source)
        self.assertIn("kernel_cr3_verified=false dma_authorized=false", self.source)
        self.assertNotIn("wrmsr", self.source)
        self.assertNotIn("Cr3::write", self.source)
        self.assertNotIn("DmaBuffer", self.source)
        self.assertNotIn("write_volatile", self.source)

    def test_uefi_dependencies_are_separate_and_fixed(self):
        self.assertIn("[workspace]", self.manifest)
        self.assertIn('uefi = { version = "=0.41.0"', self.manifest)
        self.assertTrue((ROOT / "tools/uefi-ap-probe/Cargo.lock").exists())

    def test_qemu_has_four_cores_without_writable_disks(self):
        command = MODULE.qemu_command("probe.img", "code.fd", "vars.fd")
        joined = " ".join(command)
        self.assertIn("-smp 4", joined)
        self.assertIn("-smp 12", " ".join(MODULE.qemu_command("probe.img", "code.fd", "vars.fd", 12)))
        self.assertIn("file=probe.img,readonly=on", joined)
        self.assertIn("file=code.fd,readonly=on", joined)
        self.assertNotIn("file=probe.img,readonly=off", joined)
        self.assertNotIn("virtio-blk", joined)
        self.assertNotIn("qemu-xhci", joined)
        self.assertNotIn("edu,", joined)

    def test_all_four_real_samples_are_required(self):
        MODULE.validate(successful_trace())
        for index in range(1, 5):
            trace = successful_trace()
            del trace[index]
            with self.assertRaises(AssertionError):
                MODULE.validate(trace)

    def test_twelve_distinct_executed_processors_are_required(self):
        trace = successful_trace(12)
        MODULE.validate(trace, 12)
        for index in (1, 6, 12):
            incomplete = trace.copy()
            del incomplete[index]
            with self.assertRaises(AssertionError):
                MODULE.validate(incomplete, 12)

    def test_duplicate_or_invalid_processor_is_rejected(self):
        trace = successful_trace()
        trace[2] = trace[2].replace("apic=1", "apic=0")
        with self.assertRaises(AssertionError):
            MODULE.validate(trace)
        trace = successful_trace()
        trace[1] = trace[1].replace("valid=true", "valid=false")
        with self.assertRaises(AssertionError):
            MODULE.validate(trace)
        trace = successful_trace()
        trace[3] = trace[3].replace("cr3=0x1000", "cr3=0x1001")
        with self.assertRaises(AssertionError):
            MODULE.validate(trace)

    def test_dma_authorization_or_kernel_claim_is_rejected(self):
        trace = successful_trace()
        trace[-1] = trace[-1].replace("dma_authorized=false", "dma_authorized=true")
        with self.assertRaises(AssertionError):
            MODULE.validate(trace)
        trace = successful_trace()
        trace[-1] = trace[-1].replace("kernel_cr3_verified=false", "kernel_cr3_verified=true")
        with self.assertRaises(AssertionError):
            MODULE.validate(trace)


if __name__ == "__main__":
    unittest.main(verbosity=2)
