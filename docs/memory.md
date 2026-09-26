# Memory Management

PhoenixOS brings memory management up in layers so each stage can be tested before it becomes responsible for more of the machine.

## Bootstrap allocator

The first allocator consumes only regions marked `Usable` by the bootloader memory map.

Properties:

- no heap dependency;
- 4 KiB frame granularity;
- monotonic allocation;
- skips non-usable regions;
- aligns region starts upward to frame boundaries;
- never frees;
- intended only for early page-table/heap bring-up.

This is deliberately not the final physical-memory manager.

## Safety rule

There must be exactly one live owner allocating from a given set of firmware-provided usable regions. Creating independent allocators over the same map can hand the same physical frame to two subsystems.

The bootstrap constructor is therefore `unsafe` and documents this invariant.

## Next physical-memory manager

The system allocator will eventually add:

- frame reclamation;
- contiguous allocations where DMA requires them;
- allocation zones/constraints;
- accounting and diagnostics;
- reserved-frame tracking;
- per-CPU or batched fast paths where measurements justify them;
- integration with IOMMU/DMA mapping;
- memory-pressure reporting.

The implementation choice (bitmap, buddy allocator, segregated free lists, or a hybrid) will be made from measured workload needs instead of being fixed prematurely.

## Virtual memory follow-up

After physical-frame allocation is proven, the next steps are:

1. inspect the active page tables supplied by the boot path;
2. create safe mapping/unmapping helpers;
3. reserve a kernel virtual-memory layout;
4. map a kernel heap;
5. add guard pages;
6. build address-space creation for processes;
7. add copy-on-write and shared-memory primitives later.

## Tests

The bootstrap allocator has host-side unit tests for:

- memory summary accounting;
- reserved-region skipping;
- unaligned usable-region starts;
- exhaustion;
- transitions between separated usable regions.

The QEMU boot test additionally allocates real frames from the firmware map and logs their physical addresses.
