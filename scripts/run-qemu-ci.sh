#!/usr/bin/env bash
set -euo pipefail

IMAGE="${1:?usage: run-qemu-ci.sh <uefi-image>}"

OVMF_CODE="${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE.fd}"
OVMF_VARS="${OVMF_VARS:-/usr/share/OVMF/OVMF_VARS.fd}"

if [[ ! -f "$OVMF_CODE" || ! -f "$OVMF_VARS" ]]; then
  echo "OVMF firmware not found" >&2
  exit 2
fi

vars_copy="$(mktemp)"
trap 'rm -f "$vars_copy"' EXIT
cp "$OVMF_VARS" "$vars_copy"

set +e
timeout 30s qemu-system-x86_64   -machine q35   -m 512M   -display none   -serial stdio   -no-reboot   -device isa-debug-exit,iobase=0xf4,iosize=0x04   -drive "if=pflash,format=raw,unit=0,file=$OVMF_CODE,readonly=on"   -drive "if=pflash,format=raw,unit=1,file=$vars_copy"   -drive "format=raw,file=$IMAGE"
status=$?
set -e

if [[ "$status" -eq 33 ]]; then
  echo "PhoenixOS UEFI boot smoke test passed"
  exit 0
fi

echo "PhoenixOS UEFI boot smoke test failed with qemu status $status" >&2
exit 1
