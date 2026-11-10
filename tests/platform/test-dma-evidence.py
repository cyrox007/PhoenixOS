#!/usr/bin/env python3
"""Регрессионные испытания разборщика одного аппаратного протокола."""

import importlib.util
from pathlib import Path
import unittest

path = Path(__file__).resolve().parents[2] / "scripts" / "evaluate-dma-evidence.py"
spec = importlib.util.spec_from_file_location("dma_evidence", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

GOOD = """[INFO] PhoenixOS physical diagnostic
[INFO] diagnostic memory map: entries=5 RAM=3 other=2 RAM_MiB=4096 high_MiB=0 invalid=false
[INFO] diagnostic RAM audit: complete=true pages=1234 WB=1234 blocked=0 invalid=0 scope=direct-only
[INFO] diagnostic aliases: complete=true leaves=1300 other_pages=1 conflicts=0 unknown=0 scope=current-cr3
[INFO] diagnostic IOMMU: ivhd=1 structural_ok=true runtime=unverified domain=unverified
[INFO] diagnostic IOMMU MMIO: observed=false reason=no-ivhd domain=unverified
[INFO] diagnostic CPU APIC IDs: enabled=12 online_capable=0 ids=[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11] measured=false
[INFO] diagnostic CPU cores: MADT=12 msr_consensus=unverified BSP_only=true
[INFO] diagnostic DMA gate: authorized=false reason=Some(OtherAddressSpaces) readonly=1
[INFO] diagnostic complete: OK
"""


class EvidenceTests(unittest.TestCase):
    def test_complete_passive_report_never_approves_dma(self):
        result = module.evaluate(GOOD)
        self.assertTrue(result["complete_passive_log"])
        self.assertTrue(result["direct_ram_checked"])
        self.assertTrue(result["current_cr3_checked"])
        self.assertTrue(result["diagnostic_dma_blocked"])
        self.assertTrue(result["cpu_topology_consistent"])
        self.assertEqual(result["enabled_apic_ids"], list(range(12)))
        self.assertFalse(result["cpu_other_cores_checked"])
        self.assertFalse(result["criterion_16_accepted"])
        self.assertFalse(result["criterion_17_accepted"])
        self.assertFalse(result["source_is_real_hardware_verified"])

    def test_topology_with_duplicate_or_missing_apic_id_is_rejected(self):
        duplicated = GOOD.replace(
            "ids=[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]",
            "ids=[0, 0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]",
        )
        self.assertFalse(module.evaluate(duplicated)["cpu_topology_consistent"])
        self.assertFalse(module.evaluate(GOOD.replace(
            "enabled=12 online_capable=0", "enabled=11 online_capable=0"
        ))["cpu_topology_consistent"])
        self.assertFalse(module.evaluate(GOOD.replace(
            "measured=false", "measured=true"
        ))["cpu_topology_consistent"])
        self.assertFalse(module.evaluate(GOOD.replace(
            "[INFO] diagnostic CPU APIC IDs:", "[WARN] diagnostic CPU APIC IDs:"
        ))["cpu_topology_consistent"])
        self.assertFalse(module.evaluate(GOOD.replace(
            "MADT=12", "MADT=11"
        ))["cpu_topology_consistent"])

    def test_control_register_observation_never_proves_iommu_domain(self):
        transcript = GOOD.replace(
            "observed=false reason=no-ivhd domain=unverified",
            "observed=true enabled_bit=true device_table_nonzero=true domain=unverified readonly=1",
        )
        result = module.evaluate(transcript)
        self.assertTrue(result["iommu_control_observed"])
        self.assertTrue(result["iommu_enable_bit_seen"])
        self.assertFalse(result["iommu_runtime_verified"])
        self.assertFalse(result["criterion_17_accepted"])

    def test_control_register_not_read_keeps_unknown(self):
        result = module.evaluate(GOOD)
        self.assertFalse(result["iommu_control_observed"])
        self.assertFalse(result["iommu_enable_bit_seen"])

    def test_does_not_merge_multiple_boots(self):
        result = module.evaluate(GOOD + "[INFO] PhoenixOS physical diagnostic\n")
        self.assertFalse(result["complete_passive_log"])
        self.assertFalse(result["direct_ram_checked"])

    def test_rejects_conflicting_aliases(self):
        result = module.evaluate(GOOD.replace("conflicts=0", "conflicts=1"))
        self.assertFalse(result["current_cr3_checked"])

    def test_rejects_truncated_log(self):
        text = GOOD.replace("[INFO] diagnostic complete: OK", "[WARN] journal-overflow")
        self.assertFalse(module.evaluate(text)["complete_passive_log"])

    def test_rejects_unapproved_gate(self):
        result = module.evaluate(GOOD.replace("authorized=false", "authorized=true"))
        self.assertFalse(result["diagnostic_dma_blocked"])

    def test_rejects_no_ram(self):
        result = module.evaluate(GOOD.replace("pages=1234", "pages=0"))
        self.assertFalse(result["direct_ram_checked"])

    def test_rejects_no_start(self):
        result = module.evaluate("RAM audit: complete=true")
        self.assertFalse(result["complete_passive_log"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
