# PhoenixOS

PhoenixOS is a from-scratch operating system project targeting modern x86-64 PCs.

The long-term goal is a complete general-purpose desktop platform: kernel, drivers, networking, graphical desktop, SDK, application ecosystem, package/update system, browser, documentation, and a small set of bundled games.

## Current targets

- Architecture: x86-64
- Firmware: UEFI
- Reference platform: QEMU + OVMF
- Kernel implementation: Rust (`no_std`) with small, explicit low-level/assembly escape hatches
- Public application ABI: stable C-compatible ABI; language bindings sit above it
- Kernel shape: modular hybrid kernel
- Hardware strategy: a deliberately small supported-hardware matrix first, with a documented DDK/porting path for additional devices
- Networking target: IPv4 + IPv6, sockets, DHCP/SLAAC, DNS, TCP/UDP, TLS, HTTP(S), proxy support, Ethernet first and Wi-Fi through the driver framework
- Browser target: a real standards engine port; PhoenixOS will not attempt to write HTML/CSS/JS engines from scratch

## Repository flow

- `main`: stable integration points
- `develop`: active integrated development
- `feature/*`: isolated implementation increments

See `ROADMAP.md`, `ARCHITECTURE.md`, `DECISIONS.md`, and `STATUS.md`.

## Bootstrap

The first executable target is intentionally tiny: build a UEFI disk image, enter the Rust kernel in QEMU, write a serial boot banner, and exit QEMU through the debug port. This gives CI a deterministic proof that the toolchain, boot image, firmware path, and kernel entry point all work before the real kernel grows.
