#!/usr/bin/env python3
"""Действительная проверка AMD-Vi и отказа DMA вне домена на QEMU 10.2+."""

import os
import sys
from importlib import util
from pathlib import Path

SCRIPT = Path(__file__).with_name("run-dma-test-qemu.py")
SPEC = util.spec_from_file_location("phoenix_dma_qemu", SCRIPT)
RUNNER = util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)

ORIGINAL_COMMAND = RUNNER.build_command

MARKERS = (
    "DMA separate QEMU profile: entered storage_drivers=none",
    "DMA separate QEMU profile: gated vendor=1234 device=11e8",
    "AMD IOMMU QEMU isolation: OK bytes=256 translated=true forbidden_page_blocked=true shutdown=true physical_asus=denied",
)


def build_command(image, firmware, variables):
    command = ORIGINAL_COMMAND(image, firmware, variables)
    command[0] = os.environ.get("PHOENIX_QEMU_BINARY", "qemu-system-x86_64")
    pos = command.index("-device")
    command[pos:pos] = ["-device", "amd-iommu,dma-remap=on,intremap=off"]
    if command.count("-device") < 3:
        raise RuntimeError("Обязательный виртуальный AMD IOMMU не подключён")
    return command


def execute(image, output):
    RUNNER.MARKERS = MARKERS
    RUNNER.build_command = build_command
    RUNNER.execute(image, output)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("Использование: run-amd-iommu-qemu.py <образ> <журнал>")
    execute(sys.argv[1], sys.argv[2])
