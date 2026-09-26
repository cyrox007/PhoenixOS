# Driver Model and DDK Direction

PhoenixOS intentionally supports a small hardware set first, but the interfaces must make later driver work repeatable.

## Driver lifecycle

A driver has explicit phases:

1. discover/match;
2. validate resources;
3. initialize;
4. publish device interfaces;
5. handle I/O/interrupts;
6. suspend/resume;
7. stop/remove.

## Matching

PCI/PCIe drivers match on class/subclass plus vendor/device IDs where appropriate. USB drivers match on class/interface and VID/PID as needed.

## Required kernel services

The DDK will expose versioned services for MMIO/port I/O, interrupts/MSI/MSI-X, DMA mappings, timers, work queues, synchronization, logging/tracing, PCI configuration, USB transfers, power transitions, and firmware resource queries.

Drivers must not depend on undocumented kernel internals.

## Initial supported classes

- VirtIO devices for QEMU;
- NVMe;
- AHCI;
- USB xHCI + HID;
- Ethernet;
- framebuffer/graphics fallback;
- audio after the basic desktop path.

Wi-Fi is part of the networking roadmap but follows the reusable driver/network abstractions.

## Documentation requirement

Each supported driver class eventually gets an architecture page, minimal reference driver, debugging guide, register/DMA safety checklist, test strategy, and hardware-enablement walkthrough.
