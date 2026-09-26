# PhoenixOS Roadmap

The roadmap is milestone-oriented. A milestone is complete only when the relevant code, tests, documentation, and reference examples agree.

## 0.0 — Bootstrap

- [x] repository and branch strategy
- [x] architecture/decision baseline
- [x] Rust `no_std` kernel skeleton
- [x] UEFI disk-image build path
- [x] QEMU/OVMF smoke-test workflow
- [ ] green CI on `develop`
- [ ] persistent serial logger and panic diagnostics
- [ ] framebuffer boot banner

## 0.1 — Kernel foundations

- physical memory manager
- virtual memory manager
- heap allocator
- GDT/IDT and exception handling
- APIC/timers
- scheduler
- kernel threads
- process/address-space model
- syscall entry path
- handles/capabilities
- initial IPC
- boot diagnostics and kernel test harness

## 0.2 — Storage and userspace

- ELF loader
- init/service manager
- userspace runtime
- VFS
- initramfs
- FAT read/write
- native filesystem design/prototype
- file descriptors/handles and async I/O
- libc compatibility baseline
- shell and core CLI tools

## 0.3 — Device framework

- ACPI
- PCI/PCIe enumeration
- driver registry, matching, lifecycle, and versioning
- DMA API and IOMMU-ready design
- USB host baseline
- HID keyboard/mouse
- NVMe
- AHCI/SATA fallback
- UEFI GOP/framebuffer graphics fallback
- DDK reference driver

## 0.4 — Networking foundation

- NIC abstraction
- VirtIO-net reference driver
- one real Ethernet controller driver
- Ethernet II
- ARP
- IPv4
- ICMPv4
- UDP
- TCP
- socket ABI
- DHCPv4
- DNS resolver
- loopback
- network configuration CLI
- packet/socket tests under QEMU

## 0.5 — Modern networking

- IPv6
- ICMPv6/NDP
- SLAAC
- DHCPv6
- dual-stack socket behavior
- routing tables
- local firewall hooks
- certificate store
- TLS 1.2/1.3 through a ported/audited crypto stack
- HTTP/1.1
- HTTP/2
- proxy support
- system network manager
- Wi-Fi framework and first supported Wi-Fi device
- network diagnostics and capture hooks

## 0.6 — Desktop

- graphics abstraction
- compositor
- windows/surfaces
- input routing
- fonts/text shaping
- clipboard
- notifications
- audio stack
- desktop shell
- Settings
- File Manager
- Terminal
- Text Editor
- System Monitor
- image/document viewers

## 0.7 — SDK and application platform

- stable C ABI v1 candidate
- C SDK
- Rust SDK
- C++ convenience layer
- Zig examples
- GUI toolkit
- sockets/network API documentation
- package format
- package manager
- application permissions/sandboxing
- debugger/profiler integration
- CLI, GUI, network-client, and driver reference projects

## 0.8 — Browser

- browser platform abstraction
- browser shell (tabs, address bar, downloads, history, settings)
- port selected web engine, initially targeting Servo
- networking/TLS integration
- fonts/input/clipboard integration
- GPU/rendering integration
- cookies/cache/storage
- downloads and file picker
- proxy and certificate UI
- audio/video integration
- sandbox browser processes
- Web Platform Test subset in CI
- real-site compatibility matrix

## 0.9 — Productization and updates

- installer and disk-layout tooling
- boot manager
- signed update metadata format
- trusted root/update signing keys and key rotation
- A/B or generation-based system slots
- inactive-slot update installation
- atomic next-boot activation
- boot-success health marker
- automatic rollback on failed boot/health check
- resumable background downloads
- stable/beta/development update channels
- staged rollout metadata
- delta update support after correctness baseline
- driver/base-system compatibility gates
- configuration/data migration framework
- update history and manual rollback UI
- offline/recovery update path
- recovery environment
- package signatures
- user accounts and permissions
- encrypted-storage path
- crash dumps
- telemetry only if explicitly opt-in
- Secure Boot path
- hardware-support matrix
- performance/power profiling
- upgrade compatibility tests
- power-loss/fault-injection update tests

## 1.0 — Supported daily-use release

1.0 means PhoenixOS can be installed and used on the documented supported hardware without another OS for routine supported workflows.

Required deliverables include:

- stable installer plus signed transactional update and recovery path;
- automatic rollback from an unbootable/failed update;
- desktop and core applications;
- working wired networking and at least one documented Wi-Fi path;
- real browser with HTTPS;
- SDK and porting documentation;
- DDK and hardware-enablement guide;
- package manager;
- minimum two bundled games;
- security baseline and reproducible release checks.

Broad support for all PC hardware is explicitly not a 1.0 requirement.
