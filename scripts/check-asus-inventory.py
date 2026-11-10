#!/usr/bin/env python3
"""Консервативная проверка аппаратного перечня, без доступа к устройствам."""

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "config" / "asus-m6500qh-hardware.json"
PCI = re.compile(r"^PCI VEN=([0-9a-fA-F]{4}) DEV=([0-9a-fA-F]{4})\\b", re.MULTILINE)


def load_profile(path=PROFILE):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def analyze(report, profile):
    lines = report.splitlines()
    devices = [
        f"{vendor.upper()}:{product.upper()}"
        for vendor, product in PCI.findall(report)
    ]
    present = set(devices)
    expected = profile["pci_required"]
    missing = [pci for pci in expected if pci not in present]
    names = [line for line in lines if line.startswith(("Computer:", "Ноутбук:"))]
    uefi = [line for line in lines if line.startswith(("UEFI:", "Версия UEFI:"))]
    cpu = [line for line in lines if line.startswith(("CPU:", "Процессор:"))]
    disks = [line for line in lines if line.startswith(("Storage:", "Накопитель:"))]
    model_matches = len(names) == 1 and profile["computer_model"] in names[0]
    cpu_matches = len(cpu) == 1 and profile["processor"] in cpu[0]
    core_matches = cpu_matches and (
        f"cores={profile['cores']}" in cpu[0] or f"ядер={profile['cores']}" in cpu[0]
    ) and (
        f"threads={profile['threads']}" in cpu[0]
        or f"потоков={profile['threads']}" in cpu[0]
    )
    firmware_matches = len(uefi) == 1 and profile["uefi_observed"] in uefi[0]
    disk_matches = sum(profile["disk_model_protected"] in line for line in disks) == 1
    storage_visible = "1C5C:174A" in present and disk_matches

    complete = (
        model_matches and core_matches and firmware_matches
        and storage_visible and not missing and bool(devices)
    )
    return {
        "profile": profile["profile"],
        "inventory_complete": bool(complete),
        "model_matches": bool(model_matches),
        "cpu_and_cores_match": bool(core_matches),
        "uefi_matches": bool(firmware_matches),
        "protected_nvme_present": bool(storage_visible),
        "pci_entries": len(devices),
        "missing_expected_pci": missing,
        "blocked_pci": sorted(set(expected).intersection(present)),
        "internal_storage_controller_blocked": True,
        "physical_dma_permitted": False,
        "verified_iommu_domain": False,
        "safe_physical_dma_device_selected": False,
        "acceptance_16": False,
        "acceptance_17": False,
        "hardware_progress": "15/17",
        "reason": (
            "Аппаратный перечень подтверждён, но доказательства IOMMU/CR3/ядер отсутствуют."
            if complete else
            "Отчёт неполон либо устройство/прошивка отличаются; испытание запрещено."
        ),
    }


def main():
    parser = argparse.ArgumentParser(description="Сверка пассивного перечня ASUS")
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    data = analyze(
        args.report.read_text(encoding="utf-8-sig", errors="replace"),
        load_profile(),
    )
    print(json.dumps(data, indent=2, ensure_ascii=False))
    return 0 if data["inventory_complete"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
