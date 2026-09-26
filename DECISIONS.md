# Architecture Decisions

This file records decisions that should not silently drift.

## D-001: x86-64 + UEFI first

**Status:** accepted.

The first supported architecture is x86-64 and the first firmware path is UEFI. Legacy BIOS is not a release requirement.

## D-002: Rust kernel, language-neutral public ABI

**Status:** accepted.

The kernel and new first-party low-level components prefer Rust. Small assembly sections and unsafe Rust are allowed where the hardware boundary requires them.

Applications are not required to use Rust. Public OS ABI surfaces must remain C-compatible and language-neutral.

## D-003: modular hybrid kernel

**Status:** accepted.

PhoenixOS is not committed to a pure microkernel. Performance-sensitive primitives may remain in kernel space while drivers/services use narrow, versioned interfaces that can later support stronger isolation.

## D-004: controlled hardware matrix before broad compatibility

**Status:** accepted.

Early releases guarantee QEMU plus a small list of real machines/controllers. The project must provide DDK documentation so additional hardware can be enabled without redesigning the kernel.

## D-005: networking is a core platform feature

**Status:** accepted.

Networking is required for the desktop target and browser. IPv4 and IPv6, sockets, DNS, automatic address configuration, TCP/UDP, TLS, HTTP(S), proxy support, and certificate management are roadmap requirements. Ethernet is enabled before Wi-Fi; Wi-Fi enters through the same device/network abstractions.

## D-006: port a browser engine

**Status:** accepted.

PhoenixOS will not build a modern HTML/CSS/JavaScript engine from scratch. The browser shell is first-party, while the standards engine is ported behind a platform abstraction. Servo is the primary research/porting candidate because its Rust implementation and embedding direction align well with PhoenixOS, but the browser/platform boundary must not make the OS permanently dependent on one engine.

## D-007: native API first, POSIX compatibility second

**Status:** accepted.

New PhoenixOS software should use the native asynchronous and handle-based API. POSIX compatibility is implemented as a portability layer for existing software.

## D-008: documentation and reference code are release artifacts

**Status:** accepted.

Every stable public subsystem requires documentation plus at least one buildable reference example. Driver, CLI, GUI, networking, and packaging examples are part of the SDK deliverable.

## D-009: OS updates are signed and transactional

**Status:** accepted.

System updates must not modify the currently booted system in a way that can leave it half-updated after a power loss or failed reboot.

PhoenixOS will use an A/B-style or equivalent generation-based system-update model with:

- cryptographically signed metadata and payloads;
- download verification before activation;
- installation into an inactive system generation/slot;
- atomic activation for the next boot;
- boot-success confirmation;
- automatic rollback after failed boot/health checks;
- recovery support independent of the active system;
- separate release channels and staged rollout metadata;
- resumable downloads;
- update history and user-visible rollback controls.

Application/package updates may use finer-grained transactional package operations but must use the same trust/signature infrastructure.

Bootloader, kernel, firmware-facing components, base userspace, drivers, applications, and configuration migrations must each have explicit compatibility/version rules.
