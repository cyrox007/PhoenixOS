#!/usr/bin/env bash
set -euo pipefail

IMAGE="${1:?usage: run-qemu-ci.sh <uefi-image>}"
MARKERS_FILE="${PHOENIXOS_MARKERS_FILE:-tests/qemu/bootstrap.markers}"

find_ovmf_file() {
  local explicit="${1:-}"
  shift || true

  if [[ -n "$explicit" && -f "$explicit" ]]; then
    printf '%s\n' "$explicit"
    return 0
  fi

  local candidate
  for candidate in "$@"; do
    if [[ -f "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  return 1
}

OVMF_CODE="$(find_ovmf_file "${OVMF_CODE:-}"   /usr/share/OVMF/OVMF_CODE.fd   /usr/share/OVMF/OVMF_CODE_4M.fd   /usr/share/edk2/x64/OVMF_CODE.fd   /usr/share/edk2/ovmf/OVMF_CODE.fd || true)"

OVMF_VARS="$(find_ovmf_file "${OVMF_VARS:-}"   /usr/share/OVMF/OVMF_VARS.fd   /usr/share/OVMF/OVMF_VARS_4M.fd   /usr/share/edk2/x64/OVMF_VARS.fd   /usr/share/edk2/ovmf/OVMF_VARS.fd || true)"

if [[ -z "$OVMF_CODE" || -z "$OVMF_VARS" ]]; then
  echo "Не найдены файлы прошивки OVMF" >&2
  find /usr/share -maxdepth 3 -type f     \( -name 'OVMF_CODE*.fd' -o -name 'OVMF_VARS*.fd' \)     -print >&2 || true
  exit 2
fi

vars_copy="$(mktemp)"
mkdir -p artifacts
serial_log="$PWD/artifacts/qemu-serial.log"
: > "$serial_log"
trap 'rm -f "$vars_copy"' EXIT
cp "$OVMF_VARS" "$vars_copy"

if [[ ! -c /dev/kvm ]]; then
  echo "Проверка современного x86-64 профиля требует KVM с x2APIC; /dev/kvm недоступен" >&2
  exit 2
fi

# Управляемая проверка: QEMU отключает и повторно подключает клавиатуру
# после сигнала готовности из ядра. Монитор работает отдельным UNIX-сокетом.
set +e
# Виртуальная машина загружает несколько устройств и проверяет два цикла
# USB-переподключения, DMA, NVMe и снимки графики. 30 секунд недостаточно
# на нагруженных исполнителях CI; таймаут остаётся конечным и настраиваемым.
QEMU_TIMEOUT_SECONDS="${PHOENIXOS_QEMU_TIMEOUT_SECONDS:-180}"
QEMU_IDLE_TIMEOUT_SECONDS="${PHOENIXOS_QEMU_IDLE_TIMEOUT_SECONDS:-60}"
if [[ ! "$QEMU_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "Некорректное время ожидания QEMU: $QEMU_TIMEOUT_SECONDS" >&2
  exit 2
fi
if [[ ! "$QEMU_IDLE_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "Некорректное время простоя QEMU: $QEMU_IDLE_TIMEOUT_SECONDS" >&2
  exit 2
fi
# Внутренний сторожевой механизм сообщает последнюю контрольную точку;
# внешний предел на 15 секунд больше, чтобы успели завершиться очистка и вывод.
timeout -k 5s "$(( QEMU_TIMEOUT_SECONDS + 15 ))s" sudo env \
  "PHOENIXOS_QEMU_TIMEOUT_SECONDS=$QEMU_TIMEOUT_SECONDS" \
  "PHOENIXOS_QEMU_IDLE_TIMEOUT_SECONDS=$QEMU_IDLE_TIMEOUT_SECONDS" \
  "PHOENIXOS_QEMU_FAULT_LOG=${PHOENIXOS_QEMU_FAULT_LOG:-}" \
  python3 scripts/qemu-hotplug.py "$IMAGE" "$OVMF_CODE" "$vars_copy" "$serial_log"
status=$?
set -e

if [[ "$status" -ne 0 ]]; then
  echo "Проверка USB-переподключения QEMU завершилась с кодом $status" >&2
  echo "Последние сообщения последовательного порта:" >&2
  tail -n 40 "$serial_log" >&2 || true
  exit 1
fi

bash scripts/check-serial-markers.sh "$MARKERS_FILE" "$serial_log"

echo "Проверочная UEFI-загрузка PhoenixOS успешно завершена"
