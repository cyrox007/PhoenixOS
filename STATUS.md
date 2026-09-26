# PhoenixOS Status

## Current phase

**Milestone 0.0 — Bootstrap**

Completed in the bootstrap feature branch:

- repository initialized;
- `main`, `develop`, and feature-flow established;
- architecture and long-term scope documented;
- networking and browser promoted to first-class roadmap requirements;
- Rust x86-64 `no_std` kernel entry point added;
- UEFI image builder added;
- QEMU/OVMF boot smoke-test script added;
- CI workflow added.

## Immediate verification gate

The next gate is the first GitHub Actions run. It must:

1. format-check the workspace;
2. build the UEFI image;
3. boot it under QEMU + OVMF;
4. observe the kernel serial banner;
5. receive the expected debug-exit code;
6. publish the UEFI image as a CI artifact.

If this gate fails, the bootstrap task remains open until the concrete toolchain/image/QEMU failure is fixed.

## Next implementation after bootstrap

Kernel diagnostics, framebuffer initialization, memory-map normalization, physical page allocation, exception handling, and the first kernel test harness.
