# Supported Hardware Policy

PhoenixOS does not claim universal PC support in early releases.

## Tier 0 — virtual reference

QEMU + OVMF is the deterministic reference platform used by CI. Planned virtual devices favor VirtIO where practical.

## Tier 1 — physical reference machine(s)

Before hardware enablement begins, each reference machine will be documented with exact CPU/platform, firmware behavior, PCI IDs, storage controller, USB controller, Ethernet controller, Wi-Fi controller, graphics device, and audio controller/codec.

Only explicitly tested configurations are called supported.

## Additional hardware

New devices are added through the DDK and hardware-enablement documentation. Unsupported hardware should fail cleanly and report actionable diagnostics rather than silently corrupt state.
