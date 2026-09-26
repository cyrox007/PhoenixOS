# PhoenixOS Architecture

## Product boundary

PhoenixOS is designed as a complete operating-system platform, not only a kernel. The platform boundary includes firmware/boot, kernel/HAL, drivers, userspace, storage, networking, security, graphics/audio/desktop, SDK/package/update systems, bundled applications, browser, games, and developer documentation.

## Kernel model

PhoenixOS uses a **modular hybrid-kernel** direction.

Performance-sensitive primitives live in kernel space: virtual memory, scheduler, interrupts, IPC primitives, VFS core, socket fast path, and the minimum device plumbing required to make hardware useful.

The architecture must still allow components with larger attack surfaces to move out of the kernel when isolation is worth the boundary crossing. Drivers therefore use explicit interfaces rather than arbitrary kernel internals.

## Stable boundaries

Internal Rust types are not ABI.

Public boundaries use fixed-layout, versioned, C-compatible types: fixed-width integers, handles instead of raw cross-boundary pointers, explicit buffer lengths, versioned structures, capability/rights masks, and stable syscall numbers once promoted from experimental status.

Language-specific SDKs wrap this ABI.

## Planned layout

```text
UEFI
  |
boot image / loader
  |
kernel
  +-- HAL / CPU / ACPI / APIC
  +-- memory manager
  +-- scheduler / processes / threads
  +-- IPC / handles / capabilities
  +-- VFS / page cache
  +-- socket and packet fast path
  +-- driver framework
  |
drivers
  +-- PCIe
  +-- NVMe / AHCI
  +-- USB / HID
  +-- Ethernet / Wi-Fi
  +-- graphics
  +-- audio
  |
userspace
  +-- init / service manager
  +-- network manager
  +-- DNS / TLS / HTTP libraries
  +-- package/update services
  +-- compositor / desktop
  +-- SDK runtimes
  |
applications
  +-- terminal / editor / files / settings
  +-- browser
  +-- developer tools
  +-- games
```

## Performance principles

- no garbage collector in the kernel;
- avoid copying large IPC/network payloads where shared memory or page ownership transfer is appropriate;
- batch interrupts and network work under load;
- keep hot-path allocations bounded and observable;
- asynchronous I/O at the native API level;
- page cache shared by filesystems and executable loading;
- damage-tracked compositor;
- hardware acceleration behind a graphics abstraction, with UEFI GOP/framebuffer as the initial fallback;
- benchmarks become release gates once subsystems stabilize.

## Compatibility

PhoenixOS native APIs are primary. A POSIX compatibility layer is planned to reduce the cost of porting existing C/C++ and Unix-oriented software. POSIX compatibility does not dictate all kernel internals.
