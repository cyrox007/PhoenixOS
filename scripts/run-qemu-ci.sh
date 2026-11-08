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
serial_log="$(mktemp)"
trap 'rm -f "$vars_copy" "$serial_log"' EXIT
cp "$OVMF_VARS" "$vars_copy"

if [[ ! -c /dev/kvm ]]; then
  echo "Проверка современного x86-64 профиля требует KVM с x2APIC; /dev/kvm недоступен" >&2
  exit 2
fi

# Управляемая проверка: QEMU отключает и повторно подключает клавиатуру
# после сигнала готовности из ядра. Монитор работает отдельным UNIX-сокетом.
set +e
timeout -k 5s 30s sudo python3 scripts/qemu-hotplug.py "$IMAGE" "$OVMF_CODE" "$vars_copy" "$serial_log"
status=$?
set -e

if [[ "$status" -ne 0 ]]; then
  echo "Проверка USB-переподключения QEMU завершилась с кодом $status" >&2
  exit 1
fi

bash scripts/check-serial-markers.sh "$MARKERS_FILE" "$serial_log"

echo "Проверочная UEFI-загрузка PhoenixOS успешно завершена"
