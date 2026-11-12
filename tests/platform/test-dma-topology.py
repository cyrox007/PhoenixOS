#!/usr/bin/env python3
"""Проверки полноты идентификаторов ядер в протоколе допуска DMA."""

from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


def read(relative):
    return (ROOT / relative).read_text(encoding="utf-8")


class CpuTopologyEvidenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.acpi = read("crates/acpi/src/madt_topology.rs")
        cls.ledger = read("crates/virtual-memory/src/dma_evidence.rs")
        cls.gate = read("crates/virtual-memory/src/hardware_gate.rs")
        cls.diagnostic = read("diagnostic-kernel/src/hardware.rs")

    def test_madt_parser_is_bounded_and_rejects_unknown_data(self):
        for marker in (
            "MAX_TOPOLOGY_PROCESSORS: usize = 64",
            "DuplicateApicId",
            "TooManyProcessors",
            "InvalidFlags",
            "InvalidRecord",
            "validate_table(bytes, *b\"APIC\")",
            "enabled_apic_ids",
        ):
            self.assertIn(marker, self.acpi)

    def test_exact_apic_ids_required_not_only_count(self):
        self.assertIn("expected_apic_ids: &[u32]", self.ledger)
        self.assertIn("found.logical_apic_id == *required", self.ledger)
        self.assertIn("DuplicateExpectedProcessor", self.ledger)
        self.assertIn("same_count_with_wrong_apic_ids_must_fail", self.ledger)
        self.assertNotIn("expected_processors: usize", self.ledger)
        self.assertIn("expected_apic_ids: &[u32]", self.gate)

    def test_observed_cpu_ids_are_not_inferred_from_madt(self):
        for marker in (
            "verify_observed_processors(",
            "identifiers_complete",
            "DuplicateObservation",
            "UnknownProcessor",
            "only_executed_processors_count_as_measured",
        ):
            self.assertIn(marker, self.acpi)
        for marker in (
            "running_apic_id()",
            "report_bsp_coverage(",
            "diagnostic CPU coverage:",
            "diagnostic CPU BSP:",
            "cache_measured",
            "msr_consensus=unverified BSP_only=true",
        ):
            self.assertIn(marker, self.diagnostic)

    def test_kernel_trampoline_preserves_parameter_address(self):
        kernel = read("kernel/src/arch/x86_64/smp_probe.rs")
        point = kernel.index('    "phoenix_ap_trampoline_lm64:"')
        entry = kernel.index('    "movq 0xF28(%rsi), %rax"', point)
        kernel_cr3 = kernel.index('    "movq 0xF38(%rsi), %rsi"', point)
        self.assertLess(entry, kernel_cr3)
        self.assertIn('    "pushq $0"', kernel)
        self.assertIn('    "jmpq *%rax"', kernel)
        self.assertIn('    "movq 0xF20(%rsi), %rsp"', kernel)
        self.assertIn("verify_observed_processors(topology,", kernel)
        self.assertIn("kernel AP snapshot: OK", read("kernel/src/main.rs"))
        self.assertIn("-smp", read("scripts/qemu-hotplug.py"))

    def test_diagnostic_is_read_only_and_does_not_claim_cores_measured(self):
        self.assertIn("parse_madt_topology(bytes)", self.diagnostic)
        self.assertIn("measured=false", self.diagnostic)
        self.assertIn("msr_consensus=unverified BSP_only=true", self.diagnostic)
        self.assertNotIn("write_msr(", self.diagnostic)


if __name__ == "__main__":
    unittest.main(verbosity=2)
