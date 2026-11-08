#!/usr/bin/env python3
"""Аппаратная проверка USB-переподключения внутри работающей PhoenixOS/QEMU.

После готовности службы удаляет и снова добавляет USB-клавиатуру по QMP.
Все ответы монитора проверяются, а вывод последовательного порта сохраняется.
"""

import json
import os
import socket
import subprocess
import sys
import tempfile
from pathlib import Path


class Qmp:
    def __init__(self, path):
        self.path = path
        self.connection = None
        self.channel = None
        self.counter = 0

    def connect(self):
        self.connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.connection.settimeout(5)
        self.connection.connect(self.path)
        self.channel = self.connection.makefile("rwb", buffering=0)
        greeting = self.receive()
        if "QMP" not in greeting:
            raise RuntimeError("Монитор QEMU не прислал приветствие")
        self.execute("qmp_capabilities")

    def receive(self):
        line = self.channel.readline()
        if not line:
            raise RuntimeError("Монитор QEMU закрыл соединение")
        return json.loads(line.decode("utf-8"))

    def execute(self, command, arguments=None):
        self.counter += 1
        request = {"execute": command, "id": self.counter}
        if arguments is not None:
            request["arguments"] = arguments
        self.channel.write((json.dumps(request) + "\r\n").encode("utf-8"))
        for _ in range(50):
            response = self.receive()
            if response.get("id") == self.counter:
                if "error" in response:
                    raise RuntimeError(f"QMP {command}: {response['error']}")
                if "return" not in response:
                    raise RuntimeError(f"QMP {command}: нет подтверждения")
                return
        raise RuntimeError(f"QMP {command}: слишком много посторонних событий")

    def close(self):
        if self.channel is not None:
            self.channel.close()
        if self.connection is not None:
            self.connection.close()


def main():
    if len(sys.argv) != 5:
        raise SystemExit("Применение: qemu-hotplug.py <образ> <OVMF_CODE> <OVMF_VARS> <журнал>")
    image, code, vars_path, serial_log = sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix="phoenixos-qmp-") as directory:
        path = str(Path(directory) / "monitor.sock")
        command = [
            "qemu-system-x86_64",
            "-machine", "q35,accel=kvm",
            "-cpu", "host",
            "-m", "512M",
            "-display", "none",
            "-serial", "stdio",
            "-no-reboot",
            "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
            "-device", "qemu-xhci,id=phoenix-xhci",
            "-device", "usb-kbd,id=phoenix-keyboard,bus=phoenix-xhci.0",
            "-device", "usb-mouse,id=phoenix-mouse,bus=phoenix-xhci.0",
            "-qmp", f"unix:{path},server=on,wait=off",
            "-drive", f"if=pflash,format=raw,unit=0,file={code},readonly=on",
            "-drive", f"if=pflash,format=raw,unit=1,file={vars_path}",
            "-drive", f"format=raw,file={image}",
        ]
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        monitor = Qmp(path)
        ready = False
        detached = False
        attached = False
        try:
            with open(serial_log, "w", encoding="utf-8") as log:
                for line in process.stdout:
                    sys.stdout.write(line)
                    sys.stdout.flush()
                    log.write(line)
                    log.flush()
                    if "xhci hotplug ready: OK" in line and not ready:
                        monitor.connect()
                        monitor.execute("device_del", {"id": "phoenix-keyboard"})
                        ready = True
                    if "xhci hotplug detached: OK" in line and not detached:
                        monitor.execute(
                            "device_add",
                            {"driver": "usb-kbd", "id": "phoenix-keyboard", "bus": "phoenix-xhci.0"},
                        )
                        detached = True
                    if "xhci hotplug attached: OK" in line:
                        attached = True
            result = process.wait(timeout=3)
            if not (ready and detached and attached):
                raise RuntimeError("Отсутствует подтверждение полного переподключения USB")
            if result != 33:
                raise RuntimeError(f"QEMU завершился с кодом {result}, ожидается 33")
        finally:
            monitor.close()
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)


if __name__ == "__main__":
    main()
