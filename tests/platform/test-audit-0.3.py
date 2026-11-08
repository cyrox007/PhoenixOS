#!/usr/bin/env python3
"""Регрессионная проверка согласованности приёмки этапа 0.3.

Проверяет текущий итог, а не исторические промежуточные отчёты.
Не подменяет аппаратные испытания виртуальной машины или ноутбука.
"""

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def read(path):
    return (ROOT / path).read_text(encoding="utf-8")


def check_progress():
    report = read("docs/progress-0.3.md")
    current = report.split("## Выполнение по направлениям", 1)[1].split(
        "## Подзадачи DMA", 1
    )[0]
    rows = re.findall(r"^\| ([^|]+) \| (\d+) \| (\d+) \|", current, re.MULTILINE)
    assert len(rows) == 10, "В сводной таблице должны быть все 10 направлений"
    completed = sum(int(done) for _, done, _ in rows)
    remaining = sum(int(left) for _, _, left in rows)
    assert (completed, remaining) == (43, 0), (
        f"Сводка этапа 0.3 не совпадает с планом: {completed}/43, осталось {remaining}"
    )
    assert "43 из 43" in current and "100,0 %" in current
    assert "15 выполненных подпунктов из 17" in report
    assert "реализация аппаратного IOMMU остаются открытыми" in report


def check_plan():
    tasks = read("TASKS.md")
    dma = tasks.split("### 1. Интерфейс DMA", 1)[1].split(
        "**Дополнительные завершённые меры безопасности:**", 1
    )[0]
    criteria = re.findall(r"^- \[[ x]\] ", dma, re.MULTILINE)
    assert len(criteria) == 8 and all(item == "- [x] " for item in criteria), (
        "Основные критерии DMA должны быть отдельно закрыты (8/8)"
    )
    assert "реальный IOMMU не реализован" in tasks


def check_public_state():
    readme = read("README.md")
    modern = read("docs/modern-platform.md")
    assert "43 из 43 критериев" in readme
    assert "42 из 43 критериев" not in readme
    assert "DMA 7/8" not in modern
    assert "42/43 (97,7 %)" not in modern


def check_qemu_evidence():
    markers = read("tests/qemu/bootstrap.markers")
    for requirement in (
        "modern interrupt mode: OK apic=x2APIC",
        "nvme phoenixfs: OK",
        "xhci scheduled dma shutdown: OK",
        "virtio gpu shutdown: OK",
        "reference edu driver: OK",
        "reference edu irq dma: OK",
    ):
        assert requirement in markers, f"Потерян обязательный признак QEMU: {requirement}"


def main():
    check_progress()
    check_plan()
    check_public_state()
    check_qemu_evidence()
    print("Приёмка 0.3: сводка, основной план и признаки QEMU согласованы")


if __name__ == "__main__":
    main()
