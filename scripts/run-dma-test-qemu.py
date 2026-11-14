#!/usr/bin/env python3
"""Одноразовая реальная DMA-передача виртуального EDU в отдельном образе QEMU."""

import os
import selectors
import subprocess
import sys
import tempfile
import time
from pathlib import Path

MARKERS = (
    "DMA separate QEMU profile: entered storage_drivers=none",
    "DMA separate QEMU profile: gated vendor=1234 device=11e8",
    "DMA QEMU EDU real transfer: OK bytes=256 returned_pages=1",
    "DMA QEMU-only complete: OK physical-asus=denied iommu-domain=unverified",
)


def build_command(image, firmware, variables):
    """Нет доступа к дискам хоста и нет дополнительного NVMe-накопителя."""
    return [
        "qemu-system-x86_64",
        "-machine", "q35,accel=kvm",
        "-nodefaults",
        "-cpu", "host",
        "-smp", "1",
        "-m", "512M",
        "-display", "none",
        "-serial", "stdio",
        "-no-reboot",
        "-vga", "std",
        "-device", "edu,id=phoenix-dma-only",
        "-drive", f"if=pflash,format=raw,unit=0,file={firmware},readonly=on",
        "-drive", f"if=pflash,format=raw,unit=1,file={variables}",
        "-drive", f"if=none,id=dma-boot,format=raw,file={image},readonly=on",
        "-device", "nvme,id=dma-boot-pci,serial=DMAOnly01,drive=dma-boot,bootindex=1",
    ]


def find_firmware(name, options):
    value = os.environ.get(name)
    for path in ([value] if value else []) + options:
        if path and Path(path).is_file():
            return Path(path)
    raise RuntimeError(f"Не найден файл прошивки {name}")


def execute(image, output):
    firmware = find_firmware("OVMF_CODE", [
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
    ])
    variables_template = find_firmware("OVMF_VARS", [
        "/usr/share/OVMF/OVMF_VARS.fd",
        "/usr/share/OVMF/OVMF_VARS_4M.fd",
    ])
    if not Path("/dev/kvm").exists():
        raise RuntimeError("Испытание требует /dev/kvm")

    with tempfile.TemporaryDirectory(prefix="phoenix-dma-qemu-") as directory:
        variables = Path(directory) / "OVMF_VARS.fd"
        variables.write_bytes(variables_template.read_bytes())
        cmd = build_command(image, firmware, variables)
        started = time.monotonic()
        last_output = started
        buffer = bytearray()
        seen = set()
        with open(output, "wb") as log:
            proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, bufsize=0)
            try:
                with selectors.DefaultSelector() as events:
                    events.register(proc.stdout, selectors.EVENT_READ)
                    while time.monotonic() - started < 150:
                        if time.monotonic() - last_output > 65:
                            raise TimeoutError("Нет вывода QEMU DMA более 65 секунд")
                        if not events.select(0.5):
                            if proc.poll() is not None:
                                raise RuntimeError(f"QEMU завершился до подтверждения с кодом {proc.returncode}")
                            continue
                        data = os.read(proc.stdout.fileno(), 65536)
                        if not data:
                            raise RuntimeError("Последовательный порт закрыт")
                        log.write(data)
                        log.flush()
                        last_output = time.monotonic()
                        buffer.extend(data)
                        if len(buffer) > 1024 * 1024:
                            raise RuntimeError("Слишком длинная строка QEMU DMA")
                        while b"\n" in buffer:
                            raw, _, remaining = buffer.partition(b"\n")
                            buffer = bytearray(remaining)
                            line = raw.decode("utf-8", errors="replace")
                            print(line, flush=True)
                            if ("[PANIC]" in line or "DMA separate QEMU profile: denied" in line
                                    or "AMD IOMMU QEMU stage failure:" in line):
                                raise RuntimeError(f"Испытательный образ отклонил запуск: {line}")
                            for marker in MARKERS:
                                if marker in line:
                                    seen.add(marker)
                            if MARKERS[-1] in seen:
                                missing = set(MARKERS) - seen
                                if missing:
                                    raise RuntimeError(f"Неполный протокол DMA: {sorted(missing)}")
                                print("Изолированный образ DMA: реальная передача QEMU успешна", flush=True)
                                return
                raise TimeoutError("Превышено полное время проверки QEMU DMA")
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("Использование: run-dma-test-qemu.py <образ> <журнал>")
    try:
        execute(sys.argv[1], sys.argv[2])
    except Exception as error:
        print(f"Ошибка испытания DMA: {error}", file=sys.stderr, flush=True)
        raise
