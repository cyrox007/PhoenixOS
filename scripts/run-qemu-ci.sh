#!/usr/bin/env bash
set -euo pipefail

IMAGE="${1:?usage: run-qemu-ci.sh <uefi-image>}"

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
  echo "OVMF firmware not found" >&2
  find /usr/share -maxdepth 3 -type f     \( -name 'OVMF_CODE*.fd' -o -name 'OVMF_VARS*.fd' \)     -print >&2 || true
  exit 2
fi

vars_copy="$(mktemp)"
serial_log="$(mktemp)"
trap 'rm -f "$vars_copy" "$serial_log"' EXIT
cp "$OVMF_VARS" "$vars_copy"

set +e
timeout 30s qemu-system-x86_64   -machine q35   -m 512M   -display none   -serial stdio   -no-reboot   -device isa-debug-exit,iobase=0xf4,iosize=0x04   -drive "if=pflash,format=raw,unit=0,file=$OVMF_CODE,readonly=on"   -drive "if=pflash,format=raw,unit=1,file=$vars_copy"   -drive "format=raw,file=$IMAGE" 2>&1 | tee "$serial_log"
status=${PIPESTATUS[0]}
set -e

if [[ "$status" -ne 33 ]]; then
  echo "PhoenixOS UEFI boot smoke test failed with qemu status $status" >&2
  exit 1
fi

if ! grep -Fq "bootstrap: OK" "$serial_log"; then
  echo "PhoenixOS bootstrap completion marker not found" >&2
  exit 1
fi

if [[ -n "${PHOENIXOS_EXPECT_SERIAL:-}" ]] &&
   ! grep -Fq "$PHOENIXOS_EXPECT_SERIAL" "$serial_log"; then
  echo "Expected serial marker not found: $PHOENIXOS_EXPECT_SERIAL" >&2
  exit 1
fi

echo "PhoenixOS UEFI boot smoke test passed"
