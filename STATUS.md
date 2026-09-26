# PhoenixOS Status

## Current phase

**Milestone 0.0 — Bootstrap**

Completed in the bootstrap feature branch:

- repository initialized;
- `main`, `develop`, and feature-flow established;
- architecture and long-term scope documented;
- networking and browser promoted to first-class roadmap requirements;
- production-grade transactional updates specified as a first-class platform subsystem;
- Rust x86-64 `no_std` kernel entry point added;
- UEFI image builder added;
- QEMU/OVMF boot smoke-test script added;
- CI workflow added.

## Update-system baseline

The project now requires signed, transactional system updates with an A/B or equivalent generation-based model, pending-boot health confirmation, automatic rollback, independent recovery, resumable downloads, release channels, staged rollout support, and explicit compatibility/migration rules.

The detailed baseline is in `docs/updates.md`.

## Immediate verification gate

The current CI sequence must:

1. format-check the workspace;
2. build the UEFI image;
3. boot it under QEMU + OVMF;
4. observe the kernel serial banner;
5. receive the expected debug-exit code;
6. publish the UEFI image as a CI artifact.

The first CI failure was a formatting mismatch and was fixed. The next build failure identified the missing Rust `rust-src` component required by the bootloader build; the toolchain definition now includes it and the corrected run is being verified.

If the gate still fails, the bootstrap task remains open until the concrete toolchain/image/QEMU failure is fixed.

## Next implementation after bootstrap

Kernel diagnostics, framebuffer initialization, memory-map normalization, physical page allocation, exception handling, and the first kernel test harness.

A separate stacked memory-foundation branch already contains the first heap-free 4 KiB boot-frame allocator plus host-side tests; it will only be integrated after the bootstrap gate is healthy.
