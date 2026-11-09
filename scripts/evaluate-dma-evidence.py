#!/usr/bin/env python3
"""Строгая проверка полноты пассивного протокола; не выдаёт допуск DMA."""

import argparse
import json
import re
from pathlib import Path

START = "[INFO] PhoenixOS physical diagnostic"
TOKENS = re.compile(r"([A-Za-z_][A-Za-z_0-9]*)=([^\s]+)")


def record(lines, marker):
    line = next((line for line in reversed(lines) if line.startswith(marker)), "")
    return dict(TOKENS.findall(line))


def evaluate(text):
    # Никогда не объединяем успехи разных запусков.
    pos = text.rfind(START)
    lines = text[pos:].splitlines() if pos >= 0 else []
    complete = any(line.strip() == "[INFO] diagnostic complete: OK" for line in lines)
    complete = pos >= 0 and complete and "journal-overflow" not in "\n".join(lines)
    memory = record(lines, "[INFO] diagnostic memory map:")
    ram = record(lines, "[INFO] diagnostic RAM audit:")
    aliases = record(lines, "[INFO] diagnostic aliases:")
    iommu = record(lines, "[INFO] diagnostic IOMMU: ivhd=")
    iommu_control = record(lines, "[INFO] diagnostic IOMMU MMIO:")
    cpus = record(lines, "[INFO] diagnostic CPU cores:")
    gate = record(lines, "[INFO] diagnostic DMA gate:")

    ram_ok = (
        complete
        and memory.get("invalid") == "false"
        and ram.get("complete") == "true"
        and ram.get("scope") == "direct-only"
        and ram.get("blocked") == "0"
        and ram.get("invalid") == "0"
        and ram.get("pages", "").isdigit()
        and int(ram.get("pages", "0")) > 0
    )
    alias_ok = (
        complete
        and aliases.get("complete") == "true"
        and aliases.get("scope") == "current-cr3"
        and aliases.get("conflicts") == "0"
        and aliases.get("unknown") == "0"
    )
    gate_off = complete and gate.get("authorized") == "false" and gate.get("readonly") == "1"

    return {
        "complete_passive_log": complete,
        "direct_ram_checked": ram_ok,
        "current_cr3_checked": alias_ok,
        "ivrs_structure_checked": complete and iommu.get("structural_ok") == "true",
        "diagnostic_dma_blocked": gate_off,
        "cpu_other_cores_checked": False,
        "all_address_spaces_checked": False,
        "iommu_control_observed": complete and iommu_control.get("observed") == "true",
        "iommu_enable_bit_seen": (
            complete
            and iommu_control.get("observed") == "true"
            and iommu_control.get("enabled_bit") == "true"
        ),
        "iommu_runtime_verified": False,
        "device_isolation_verified": False,
        "physical_dma_verified": False,
        "reported_cpu_consensus": cpus.get("msr_consensus", "unknown"),
        "reported_iommu_runtime": iommu.get("runtime", "unknown"),
        "criterion_16_accepted": False,
        "criterion_17_accepted": False,
        "source_is_real_hardware_verified": False,
        "hardware_progress": "15/17",
        "missing": [
            "Измерение MSR всех процессорных ядер",
            "Проверка всех иных адресных пространств",
            "Действующее преобразование IOMMU и выделенный домен",
            "Выбор безопасного контроллера и реальная DMA-передача",
            "Аппаратно подтверждённые остановка и освобождение",
        ],
    }


def main():
    parser = argparse.ArgumentParser(description="Пассивная приёмка DMA PhoenixOS")
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    data = evaluate(args.report.read_text(encoding="utf-8", errors="replace"))
    print(json.dumps(data, ensure_ascii=False, indent=2))
    return 0 if data["complete_passive_log"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
