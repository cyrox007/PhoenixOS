#!/usr/bin/env python3
"""Аппаратная проверка USB-переподключения внутри работающей PhoenixOS/QEMU.

Первый цикл проверяет загрузочное отключение/подключение клавиатуры и ввод.
Второй повторяет переподключение и получение HID во время исполнения службы
под управлением системного планировщика. Все ответы QMP проверяются;
журнал последовательного порта сохраняется.
"""

import json
import os
import selectors
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def verify_gpu_screenshot(path, phase="initial"):
    """Сравнить настоящие пиксели снимка второго видеоконтроллера QEMU."""
    with open(path, "rb") as image:
        if image.readline().strip() != b"P6":
            raise RuntimeError("VirtIO-GPU: снимок не является двоичным PPM")
        geometry = image.readline()
        while geometry.startswith(b"#"):
            geometry = image.readline()
        dimensions = geometry.split()
        if len(dimensions) != 2:
            raise RuntimeError("VirtIO-GPU: неверные размеры снимка")
        width, height = map(int, dimensions)
        if width < 128 or height < 128 or image.readline().strip() != b"255":
            raise RuntimeError("VirtIO-GPU: нет ожидаемой области изображения")
        pixels = image.read()
    if len(pixels) != width * height * 3:
        raise RuntimeError("VirtIO-GPU: неполные данные снимка")

    def pixel(x, y):
        start = (y * width + x) * 3
        return tuple(pixels[start:start + 3])

    expected = [
        (124, 124, (255, 255, 255)),
        (50, 15, (24, 31, 46)),
        (50, 60, (229, 94, 54)),
        (50, 105, (44, 174, 152)),
    ]
    print(
        "Снимок VirtIO-GPU, проверочные цвета: "
        + ", ".join(f"({x},{y})={pixel(x,y)}" for x, y, _ in expected),
        flush=True,
    )
    if phase == "ready":
        expected.append((96, 20, (58, 108, 242)))
    elif phase == "complete":
        expected.append((96, 20, (21, 196, 117)))

    for x, y, color in expected:
        if pixel(x, y) != color:
            raise RuntimeError(
                f"VirtIO-GPU: пиксель ({x},{y}) = {pixel(x, y)}, ожидалось {color}"
            )
    print(f"Снимок VirtIO-GPU: OK {width}x{height}, проверено {len(expected)} пикселя", flush=True)


def verify_firmware_screenshot(path):
    """Проверить, что ранний буфер GOP не изменён при работе VirtIO-GPU."""
    with open(path, "rb") as image:
        if image.readline().strip() != b"P6":
            raise RuntimeError("GOP: снимок не является двоичным PPM")
        geometry = image.readline()
        while geometry.startswith(b"#"):
            geometry = image.readline()
        width, height = map(int, geometry.split())
        if width < 128 or height < 64 or image.readline().strip() != b"255":
            raise RuntimeError("GOP: некорректный размер раннего изображения")
        pixels = image.read()
    if len(pixels) != width * height * 3:
        raise RuntimeError("GOP: неполные пиксели раннего изображения")
    x, y = 16, 12
    start = (y * width + x) * 3
    observed = tuple(pixels[start:start + 3])
    if observed != (24, 31, 46):
        raise RuntimeError(f"GOP: панель была перерисована после передачи: {observed}")
    print(f"Сохранность раннего GOP: OK {width}x{height}", flush=True)


def read_guest_lines(process, total_seconds, idle_seconds):
    """Читать последовательный порт без зависания на неполной строке.

    Оба срока отсчитываются монотонными часами. Отдельное ограничение
    простоя отличает зависшую загрузку от медленного, но живого сценария.
    """
    start = time.monotonic()
    last_output = start
    pending = bytearray()
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        while True:
            now = time.monotonic()
            if now - start >= total_seconds:
                raise TimeoutError(f"QEMU: превышен общий срок {total_seconds} с")
            if now - last_output >= idle_seconds:
                raise TimeoutError(f"QEMU: нет вывода {idle_seconds} с")
            remaining = min(1.0, total_seconds - (now - start),
                            idle_seconds - (now - last_output))
            if not selector.select(remaining):
                continue
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                if pending:
                    yield pending.decode("utf-8", errors="replace")
                return
            last_output = time.monotonic()
            pending.extend(chunk)
            if len(pending) > 1024 * 1024:
                raise RuntimeError("QEMU: строка последовательного порта слишком длинная")
            while b"\n" in pending:
                line, _, rest = pending.partition(b"\n")
                pending = bytearray(rest)
                yield line.decode("utf-8", errors="replace") + "\n"


def positive_timeout(name, default):
    """Проверить ограничение времени из окружения."""
    value = os.environ.get(name, str(default))
    if not value.isdecimal() or int(value) == 0:
        raise ValueError(f"Некорректное ограничение времени {name}: {value!r}")
    return int(value)


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


def build_qemu_command(image, code, vars_path, nvme_image, path):
    """Явный набор UEFI и современных виртуальных устройств."""
    return [
        "qemu-system-x86_64",
        "-machine", "q35,accel=kvm",
        # Не создавать старые устройства по умолчанию: загрузочный диск
        # привязан к отдельному контроллеру NVMe, не к IDE/PATA.
        "-nodefaults",
        "-cpu", "host",
        "-m", "512M",
        "-display", "none",
        "-serial", "stdio",
        "-no-reboot",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        # Второй видеоконтроллер для настоящего перечисления PCIe.
        # Стандартный UEFI-адаптер остаётся основным до запуска
        # собственного графического драйвера.
        "-vga", "std",
        "-device", "virtio-gpu-pci,id=phoenix-gpu",
        "-device", "edu,id=phoenix-edu",
        "-device", "qemu-xhci,id=phoenix-xhci",
        "-device", "usb-kbd,id=phoenix-keyboard,bus=phoenix-xhci.0",
        "-device", "usb-mouse,id=phoenix-mouse,bus=phoenix-xhci.0",
        "-drive", f"file={nvme_image},if=none,id=phoenix-nvm,format=raw",
        "-device", "nvme,id=phoenix-nvme,serial=PhoenixNVM01,drive=phoenix-nvm",
        "-qmp", f"unix:{path},server=on,wait=off",
        "-drive", f"if=pflash,format=raw,unit=0,file={code},readonly=on",
        "-drive", f"if=pflash,format=raw,unit=1,file={vars_path}",
        "-drive", f"if=none,id=phoenix-boot,format=raw,file={image},readonly=on",
        "-device", "nvme,id=phoenix-boot-nvme,serial=PhoenixBoot01,drive=phoenix-boot,bootindex=1",
    ]


def validate_modern_qemu_profile(arguments):
    """Отвергнуть неявный IDE, BIOS/CSM и старые USB-контроллеры.

    Проверяет фактический список параметров, подаваемый QEMU, а не
    комментарии в исходниках. Наследуемый PIC самого чипсета остаётся
    только маскированным и не служит для обработки прерываний.
    """
    if "-nodefaults" not in arguments:
        raise ValueError("Нужен режим QEMU без неявных старых устройств")

    forbidden_options = ("-bios", "-hda", "-hdb", "-hdc", "-hdd", "-fda", "-fdb", "-cdrom")
    if any(option in arguments for option in forbidden_options):
        raise ValueError("Запрещён старый способ загрузки BIOS/IDE")

    values = []
    for index, option in enumerate(arguments[:-1]):
        if option in ("-drive", "-device", "-machine"):
            values.append((option, str(arguments[index + 1]).lower()))

    if not any(key == "-machine" and value.startswith("q35,") for key, value in values):
        raise ValueError("Требуется современная виртуальная плата q35")

    drives = [value for key, value in values if key == "-drive"]
    devices = [value for key, value in values if key == "-device"]
    if not drives or not devices:
        raise ValueError("Не заданы устройства QEMU")

    if any("if=ide" in drive or "if=scsi" in drive or "if=floppy" in drive for drive in drives):
        raise ValueError("Загрузка с IDE/PATA и устаревших шин запрещена")
    if any(not ("if=none" in drive or "if=pflash" in drive) for drive in drives):
        raise ValueError("Каждый накопитель должен иметь явную современную шину")
    if len([drive for drive in drives if "if=pflash" in drive]) != 2:
        raise ValueError("UEFI должен использовать две явные области pflash")

    legacy_names = (
        "ide-hd", "ide-cd", "piix3-ide", "piix4-ide",
        "usb-uhci", "usb-ohci", "usb-ehci",
        "ich9-uhci", "ich9-ehci", "i82801b11-bridge", "i8259",
    )
    if any(name in device for device in devices for name in legacy_names):
        raise ValueError("Обнаружен устаревший контроллер в явной конфигурации")

    required_devices = ("nvme", "qemu-xhci")
    if any(not any(device.startswith(name) for device in devices) for name in required_devices):
        raise ValueError("Требуются NVMe и xHCI")

    if not any("id=phoenix-boot" in drive and "if=none" in drive for drive in drives):
        raise ValueError("Загрузочный образ должен идти через NVMe, не через IDE")
    if not any(device.startswith("nvme") and "drive=phoenix-boot" in device and "bootindex=1" in device for device in devices):
        raise ValueError("Не указан явный загрузочный NVMe-контроллер")
    if not any(device.startswith("nvme") and "drive=phoenix-nvm" in device for device in devices):
        raise ValueError("Нет отдельного NVMe для безопасного испытания PhoenixFS")


def main():
    if len(sys.argv) != 5:
        raise SystemExit("Применение: qemu-hotplug.py <образ> <OVMF_CODE> <OVMF_VARS> <журнал>")
    image, code, vars_path, serial_log = sys.argv[1:]
    with tempfile.TemporaryDirectory(prefix="phoenixos-qmp-") as directory:
        path = str(Path(directory) / "monitor.sock")
        # Отдельный временный разреженный накопитель: его содержимое
        # не связано ни с образом загрузки, ни с файловой системой хоста.
        nvme_image = Path(directory) / "nvme-test.img"
        with nvme_image.open("wb") as disk:
            disk.truncate(16 * 1024 * 1024)
        command = build_qemu_command(image, code, vars_path, nvme_image, path)
        validate_modern_qemu_profile(command)
        overall = positive_timeout("PHOENIXOS_QEMU_TIMEOUT_SECONDS", 180)
        idle = positive_timeout("PHOENIXOS_QEMU_IDLE_TIMEOUT_SECONDS", 60)
        print(f"QEMU: начало испытаний, предел {overall} с, простой {idle} с", flush=True)
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            bufsize=0,
        )
        monitor = Qmp(path)
        gpu_screen_verified = False
        gpu_ready_verified = False
        gpu_complete_verified = False
        firmware_held_verified = False
        ready = False
        detached = False
        attached = False
        hid_ready = False
        key_pressed = False
        key_released = False
        mouse_moved = False
        scheduled_ready = False
        scheduled_detached = False
        scheduled_attached = False
        scheduled_pressed = False
        scheduled_released = False
        scheduled_moved = False
        scheduled_stopped = False
        last_checkpoint = "последовательный порт ещё не ответил"
        try:
            with open(serial_log, "w", encoding="utf-8") as log:
                for line in read_guest_lines(process, overall, idle):
                    if ": OK" in line:
                        last_checkpoint = line.strip()
                    sys.stdout.write(line)
                    sys.stdout.flush()
                    log.write(line)
                    log.flush()
                    if "virtio gpu 2d: OK" in line and not gpu_screen_verified:
                        # GPU остаётся включённым до окончания USB-испытаний.
                        # Снимок строится самим QEMU из вторичного scanout,
                        # а не из буфера гостя или заранее созданного образца.
                        monitor.connect()
                        screenshot = Path(directory) / "virtio-gpu.ppm"
                        monitor.execute("screendump", {
                            "filename": str(screenshot),
                            "device": "phoenix-gpu",
                            "head": 0,
                        })
                        verify_gpu_screenshot(screenshot)
                        gpu_screen_verified = True
                    if "graphics native present: OK phase=ready" in line and not gpu_ready_verified:
                        screenshot = Path(directory) / "virtio-gpu-ready.ppm"
                        monitor.execute("screendump", {
                            "filename": str(screenshot),
                            "device": "phoenix-gpu",
                            "head": 0,
                        })
                        verify_gpu_screenshot(screenshot, phase="ready")
                        gpu_ready_verified = True

                        early_screenshot = Path(directory) / "gop-early.ppm"
                        monitor.execute("screendump", {"filename": str(early_screenshot)})
                        verify_firmware_screenshot(early_screenshot)
                        firmware_held_verified = True

                    if "graphics native present: OK phase=complete" in line and not gpu_complete_verified:
                        screenshot = Path(directory) / "virtio-gpu-complete.ppm"
                        monitor.execute("screendump", {
                            "filename": str(screenshot),
                            "device": "phoenix-gpu",
                            "head": 0,
                        })
                        verify_gpu_screenshot(screenshot, phase="complete")
                        gpu_complete_verified = True

                    if "xhci hotplug ready: OK" in line and not ready:
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
                    if "xhci hid ready: OK" in line and not hid_ready:
                        monitor.execute(
                            "input-send-event",
                            {"events": [
                                {"type": "key", "data": {
                                    "down": True,
                                    "key": {"type": "qcode", "data": "a"},
                                }}
                            ]},
                        )
                        hid_ready = True
                    if "xhci hid key pressed: OK" in line and not key_pressed:
                        monitor.execute(
                            "input-send-event",
                            {"events": [
                                {"type": "key", "data": {
                                    "down": False,
                                    "key": {"type": "qcode", "data": "a"},
                                }}
                            ]},
                        )
                        key_pressed = True
                    if "xhci hid key released: OK" in line and not key_released:
                        monitor.execute(
                            "input-send-event",
                            {"events": [
                                {"type": "rel", "data": {"axis": "x", "value": 12}},
                                {"type": "rel", "data": {"axis": "y", "value": -6}},
                            ]},
                        )
                        key_released = True
                    if "xhci hid mouse moved: OK" in line:
                        mouse_moved = True

                    # Вторая серия выполняется уже не загрузочным циклом, а
                    # двумя аппаратно планируемыми потоками. Тот же QMP-клиент
                    # обеспечивает причинный порядок отключения и ввода.
                    if "xhci scheduled ready: OK" in line and not scheduled_ready:
                        monitor.execute("device_del", {"id": "phoenix-keyboard"})
                        scheduled_ready = True
                    if "xhci scheduled detached: OK" in line and not scheduled_detached:
                        monitor.execute(
                            "device_add",
                            {"driver": "usb-kbd", "id": "phoenix-keyboard", "bus": "phoenix-xhci.0"},
                        )
                        scheduled_detached = True
                    if "xhci scheduled attached: OK" in line and not scheduled_attached:
                        monitor.execute(
                            "input-send-event",
                            {"events": [{
                                "type": "key",
                                "data": {"down": True, "key": {"type": "qcode", "data": "b"}},
                            }]},
                        )
                        scheduled_attached = True
                    if "xhci scheduled key pressed: OK" in line and not scheduled_pressed:
                        monitor.execute(
                            "input-send-event",
                            {"events": [{
                                "type": "key",
                                "data": {"down": False, "key": {"type": "qcode", "data": "b"}},
                            }]},
                        )
                        scheduled_pressed = True
                    if "xhci scheduled key released: OK" in line and not scheduled_released:
                        monitor.execute(
                            "input-send-event",
                            {"events": [
                                {"type": "rel", "data": {"axis": "x", "value": 7}},
                                {"type": "rel", "data": {"axis": "y", "value": 5}},
                            ]},
                        )
                        scheduled_released = True
                    if "xhci scheduled mouse moved: OK" in line:
                        scheduled_moved = True
                    if "xhci scheduled stopped: OK" in line:
                        scheduled_stopped = True
            result = process.wait(timeout=3)
            if not gpu_screen_verified or not gpu_ready_verified or not gpu_complete_verified:
                raise RuntimeError("Отсутствуют снимки двух штатных кадров VirtIO-GPU")
            if not firmware_held_verified:
                raise RuntimeError("Ранний буфер GOP не прошёл проверку сохранности")
            if not (ready and detached and attached):
                raise RuntimeError("Отсутствует подтверждение полного переподключения USB")
            if not (hid_ready and key_pressed and key_released and mouse_moved):
                raise RuntimeError("Отсутствуют отчёты клавиатуры или мыши")
            if not all((
                scheduled_ready,
                scheduled_detached,
                scheduled_attached,
                scheduled_pressed,
                scheduled_released,
                scheduled_moved,
                scheduled_stopped,
            )):
                raise RuntimeError("Постоянная USB-служба не завершила полный жизненный цикл")
            if result != 33:
                raise RuntimeError(f"QEMU завершился с кодом {result}, ожидается 33")
        except Exception as error:
            print(f"QEMU: отказ: {error}; последняя контрольная точка: "
                  f"{last_checkpoint}", file=sys.stderr, flush=True)
            raise
        finally:
            monitor.close()
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)


if __name__ == "__main__":
    main()
