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
        cls.registry = (ROOT / "crates/virtual-memory/src/root_registry.rs").read_text()
        cls.kernel = (ROOT / "kernel/src/main.rs").read_text()
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

    def test_both_root_lifetime_paths_are_registered(self):
        self.assertIn("pub mod root_registry;", self.vm)
        self.assertIn("root_registry::observe_current_root(", self.vm)
        self.assertIn("root_registry::register_process_root(", self.vm)
        self.assertIn("root_registry::unregister_process_root(", self.vm)
        self.assertIn("root_registry::contains_root(", self.vm)
        self.assertIn("root_registry::snapshot()", self.kernel)
        self.assertIn("during.roots().contains(&root_address)", self.kernel)
        self.assertIn("after.roots().contains(&root_address)", self.kernel)

    def test_registry_remains_untrusted_for_physical_dma(self):
        self.assertIn("physical_coverage_verified(&self) -> bool", self.registry)
        self.assertIn("return Err(RootError::ConcurrentAccess)", self.registry)
        self.assertIn("self.compromised.store(true, Ordering::Release)", self.registry)
        self.assertIn("RootError::Capacity", self.registry)
        self.assertIn("DifferentBootRoot", self.registry)
        self.assertIn("NotProcessRoot", self.registry)

    def test_no_automatic_all_cores_accepted_by_diagnostic(self):
        self.assertIn("msr_consensus=unverified BSP_only=true", self.diagnostic)
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
