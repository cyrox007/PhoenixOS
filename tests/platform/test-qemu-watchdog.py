#!/usr/bin/env python3
"""Проверки ограничений ожидания вывода QEMU без запуска виртуальной машины."""

import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("phoenix_qemu_hotplug", ROOT / "scripts/qemu-hotplug.py")
QEMU = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(QEMU)


class QemuOutputWatchdogTests(unittest.TestCase):
    def run_child(self, source, *, total=2, idle=1):
        child = subprocess.Popen(
            [sys.executable, "-u", "-c", source],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            bufsize=0,
        )
        try:
            return list(QEMU.read_guest_lines(child, total, idle))
        finally:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=5)
            child.stdout.close()

    def test_reads_complete_and_partial_lines(self):
        lines = self.run_child("import sys; sys.stdout.write('один\\nдва\\nхвост'); sys.stdout.flush()")
        self.assertEqual(lines, ["один\n", "два\n", "хвост"])

    def test_reports_idle_before_global_deadline(self):
        with self.assertRaisesRegex(TimeoutError, "нет вывода"):
            self.run_child("import time; time.sleep(5)", total=3, idle=0.3)

    def test_reports_global_deadline_when_guest_keeps_talking(self):
        program = "import time\nwhile True:\n print('жив', flush=True)\n time.sleep(0.05)"
        with self.assertRaisesRegex(TimeoutError, "общий срок"):
            self.run_child(program, total=0.5, idle=0.3)

    def test_rejects_invalid_timeout_configuration(self):
        with patch.dict("os.environ", {"PHOENIXOS_QEMU_TIMEOUT_SECONDS": "0"}):
            with self.assertRaisesRegex(ValueError, "Некорректное"):
                QEMU.positive_timeout("PHOENIXOS_QEMU_TIMEOUT_SECONDS", 180)


if __name__ == "__main__":
    unittest.main(verbosity=2)
