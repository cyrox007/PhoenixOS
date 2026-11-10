#!/usr/bin/env python3
"""Не допускать обхода поколения CR3 и выдачи мнимых аппаратных доказательств."""

from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


class MappingCoverageSourceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.vm = (ROOT / "crates/virtual-memory/src/lib.rs").read_text()
        cls.ledger = (ROOT / "crates/virtual-memory/src/dma_evidence.rs").read_text()
        cls.gate = (ROOT / "crates/virtual-memory/src/hardware_gate.rs").read_text()
        cls.diagnostic = (ROOT / "diagnostic-kernel/src/hardware.rs").read_text()

    def test_all_mapping_paths_invalidate_before_write(self):
        for marker in (
            "map_4k", "unmap_4k", "impl Drop for AddressSpaceActivation",
            "pub unsafe fn new<A>", "pub unsafe fn activate(",
            "inherit_kernel_mappings(", "map_owned_user_4k",
            "unmap_owned_user_4k", "destroy_user_space",
            "clear_shared_kernel_mappings",
        ):
            self.assertIn(marker, self.vm)
        self.assertGreaterEqual(self.vm.count("dma_evidence::invalidate_mappings();"), 10)
        self.assertIn("pub mod dma_evidence;", self.vm)

    def test_no_automatic_all_cores_accepted_by_diagnostic(self):
        self.assertIn("msr_consensus=unverified BSP_only=true", self.diagnostic)
        self.assertIn("all_processors_checked: false", self.ledger if "all_processors_checked: false" in self.ledger else self.gate)
        self.assertIn("evidence.all_processors_checked = false;", self.gate)
        self.assertIn("evidence.all_address_spaces_checked = false;", self.gate)

    def test_epoch_and_hardware_reports_are_strict(self):
        for token in (
            "AtomicU64", "EPOCH_EXHAUSTED", "InconsistentRegisters",
            "MissingProcessor", "MissingRoot", "StaleGeneration",
            "DuplicateCpu", "DuplicateRoot", "aliases_clear",
            "amd_syscfg", "amd_top_mem2",
        ):
            self.assertIn(token, self.ledger)


if __name__ == "__main__":
    unittest.main(verbosity=2)
