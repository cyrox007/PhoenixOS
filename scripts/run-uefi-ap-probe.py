#!/usr/bin/env python3
"""Изолированная загрузка предзагрузочного UEFI-сборщика регистров ЦП в QEMU."""

import os
import selectors
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def find_firmware(candidates):
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            return str(candidate)
    raise FileNotFoundError("Не найдена прошивка OVMF")


def build_image(binary, image):
    with open(image, "wb") as destination:
        destination.truncate(64 * 1024 * 1024)
    subprocess.run(["mkfs.vfat", "-F", "32", str(image)], check=True)
    subprocess.run(["mmd", "-i", str(image), "::/EFI"], check=True)
    subprocess.run(["mmd", "-i", str(image), "::/EFI/BOOT"], check=True)
    subprocess.run(
        ["mcopy", "-i", str(image), str(binary), "::/EFI/BOOT/BOOTX64.EFI"],
        check=True,
    )


def qemu_command(image, code, variables):
    return [
        "qemu-system-x86_64",
        "-machine", "q35,accel=kvm",
        "-cpu", "host",
        "-smp", "4",
        "-m", "512M",
        "-nodefaults",
        "-display", "none",
        "-vga", "std",
        "-serial", "stdio",
        "-no-reboot",
        "-drive", f"if=pflash,format=raw,unit=0,file={code},readonly=on",
        "-drive", f"if=pflash,format=raw,unit=1,file={variables}",
        # Только изолированный образ FAT, открытый строго на чтение.
        "-drive", f"if=none,id=ap-probe,format=raw,file={image},readonly=on",
        "-device", "nvme,id=ap-probe-controller,serial=APProbe01,drive=ap-probe,bootindex=1",
    ]


def validate(lines):
    records = [item for item in lines if item.startswith("AP_PROBE cpu=")]
    result = [item for item in lines if item.startswith("AP_PROBE result=")]
    assert any(
        "AP_PROBE mode=UEFI-QEMU readonly=true hardware_dma=false" in line
        for line in lines
    ), "Отсутствует безопасный признак предзагрузочной диагностики"
    assert len(records) == 4, f"Ожидалось четыре реальных снимка ЦП, получено {len(records)}"
    ids = []
    for index, record in enumerate(records):
        parts = dict(token.split("=", 1) for token in record.split()[1:])
        assert parts["cpu"] == str(index), record
        assert parts["valid"] == "true", record
        assert int(parts["cr3"], 16) != 0, record
        assert int(parts["cr3"], 16) % 4096 == 0, record
        ids.append(int(parts["apic"]))
    assert len(set(ids)) == 4, f"Идентификаторы процессоров повторяются: {ids}"
    assert len(result) == 1, "Не найден единственный итоговый протокол"
    summary = result[0]
    for expected in (
        "result=OK", "total=4", "enabled=4", "captured=4",
        "unique=true", "cache_consensus=true",
        "phase=uefi-before-exit", "kernel_cr3_verified=false",
        "dma_authorized=false",
    ):
        assert expected in summary, f"Недостающий признак {expected}: {summary}"


def execute(binary, output):
    if not Path("/dev/kvm").exists():
        raise RuntimeError("Для изолированного испытания требуется KVM")
    code = find_firmware([
        os.environ.get("OVMF_CODE"),
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
    ])
    firmware_vars = find_firmware([
        os.environ.get("OVMF_VARS"),
        "/usr/share/OVMF/OVMF_VARS.fd",
        "/usr/share/OVMF/OVMF_VARS_4M.fd",
    ])
    with tempfile.TemporaryDirectory(prefix="phoenix-ap-uefi-") as directory:
        root = Path(directory)
        image = root / "ap-probe.img"
        vars_copy = root / "OVMF_VARS.fd"
        shutil.copyfile(firmware_vars, vars_copy)
        build_image(binary, image)
        command = qemu_command(image, code, vars_copy)
        started = time.monotonic()
        lines = []
        pending = bytearray()
        with open(output, "wb") as log:
            process = subprocess.Popen(
                command,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                bufsize=0,
            )
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    while time.monotonic() - started < 120:
                        if not selector.select(0.5):
                            if process.poll() is not None:
                                raise RuntimeError(f"QEMU завершился: {process.returncode}")
                            continue
                        data = os.read(process.stdout.fileno(), 65536)
                        if not data:
                            raise RuntimeError("Последовательный канал закрыт")
                        log.write(data)
                        log.flush()
                        pending.extend(data)
                        if len(pending) > 256 * 1024:
                            raise RuntimeError("Превышен допустимый размер строки")
                        while b"\n" in pending:
                            raw, _, rest = pending.partition(b"\n")
                            pending = bytearray(rest)
                            line = raw.decode("utf-8", errors="replace").strip()
                            if line.startswith("AP_PROBE"):
                                print(line, flush=True)
                                lines.append(line)
                            if line.startswith("AP_PROBE result="):
                                validate(lines)
                                print("UEFI: все четыре процессора реально измерены", flush=True)
                                return
                raise TimeoutError("Превышен общий срок испытания UEFI")
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("Использование: run-uefi-ap-probe.py <файл EFI> <журнал>")
    execute(Path(sys.argv[1]), Path(sys.argv[2]))
