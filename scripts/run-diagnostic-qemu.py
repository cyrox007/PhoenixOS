#!/usr/bin/env python3
"""Изолированная проверка диагностического UEFI-образа без рабочих дисков."""

import os
import selectors
import subprocess
import sys
import tempfile
import time
from pathlib import Path


MARKERS = (
    "diagnostic mode: OK readonly=1",
    "storage controllers: not initialized",
    "device DMA: disabled",
    "diagnostic CPU: OK",
    "diagnostic cache: OK",
    "diagnostic ACPI: OK",
    "diagnostic PCI: OK",
    "diagnostic complete: OK",
)


def build_command(image, code, vars_file):
    return [
        "qemu-system-x86_64",
        "-machine", "q35,accel=kvm",
        "-nodefaults",
        "-cpu", "host",
        "-m", "512M",
        "-display", "none",
        "-serial", "stdio",
        "-no-reboot",
        "-vga", "std",
        "-drive", f"if=pflash,format=raw,unit=0,file={code},readonly=on",
        "-drive", f"if=pflash,format=raw,unit=1,file={vars_file}",
        # Только сам загрузочный носитель: чтение без права записи.
        "-drive", f"if=none,id=diagnostic-boot,format=raw,file={image},readonly=on",
        "-device", "nvme,id=diagnostic-boot-controller,serial=DiagBoot01,drive=diagnostic-boot,bootindex=1",
    ]


def find_ovmf(names):
    for path in names:
        if path and Path(path).is_file():
            return str(path)
    raise RuntimeError("Не найден файл OVMF")


def execute(image, output):
    code = find_ovmf([
        os.environ.get("OVMF_CODE"),
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
    ])
    template = find_ovmf([
        os.environ.get("OVMF_VARS"),
        "/usr/share/OVMF/OVMF_VARS.fd",
        "/usr/share/OVMF/OVMF_VARS_4M.fd",
    ])
    if not Path("/dev/kvm").exists():
        raise RuntimeError("Для проверочного окружения требуется KVM")
    with tempfile.TemporaryDirectory(prefix="phoenix-diag-") as tmp:
        variables = Path(tmp) / "OVMF_VARS.fd"
        variables.write_bytes(Path(template).read_bytes())
        command = build_command(image, code, str(variables))
        start = time.monotonic()
        last_output = start
        seen = set()
        pending = bytearray()
        with open(output, "wb") as log:
            proc = subprocess.Popen(
                command,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                bufsize=0,
            )
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(proc.stdout, selectors.EVENT_READ)
                    while time.monotonic() - start < 120:
                        if time.monotonic() - last_output > 60:
                            raise TimeoutError("Нет последовательного вывода QEMU в течение 60 с")
                        if not selector.select(0.5):
                            if proc.poll() is not None:
                                raise RuntimeError(f"QEMU завершился с кодом {proc.returncode}")
                            continue
                        chunk = os.read(proc.stdout.fileno(), 65536)
                        if not chunk:
                            raise RuntimeError("QEMU закрыл последовательный канал")
                        log.write(chunk)
                        log.flush()
                        last_output = time.monotonic()
                        pending.extend(chunk)
                        if len(pending) > 1024 * 1024:
                            raise RuntimeError("Вывод QEMU превышает допустимый размер строки")
                        while b"\n" in pending:
                            raw, _, rest = pending.partition(b"\n")
                            pending = bytearray(rest)
                            line = raw.decode("utf-8", errors="replace")
                            print(line, flush=True)
                            for marker in MARKERS:
                                if marker in line:
                                    seen.add(marker)
                            if MARKERS[-1] in seen:
                                missing = set(MARKERS) - seen
                                if missing:
                                    raise RuntimeError(f"Не найдены признаки диагностики: {sorted(missing)}")
                                print("Диагностический образ: проверка QEMU успешна", flush=True)
                                return
                raise TimeoutError("Общий срок диагностики QEMU превышен")
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("Использование: run-diagnostic-qemu.py <образ> <журнал>")
    try:
        execute(sys.argv[1], sys.argv[2])
    except Exception as error:
        print(f"Диагностический образ: ошибка: {error}", file=sys.stderr, flush=True)
        raise
