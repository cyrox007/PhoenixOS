# Early Diagnostics

PhoenixOS keeps its earliest diagnostics path deliberately independent from the future userspace logging stack.

## Serial console

COM1 is the reference early-debug channel under QEMU and on compatible x86-64 hardware.

The kernel keeps a normal serial console handle during boot for structured messages. Panic handling does not depend on that handle: it attempts to initialize an emergency serial writer independently so a panic can still be reported if the regular boot path has been disrupted.

## Current format

Early log lines use a compact level prefix:

```text
[INFO] PhoenixOS bootstrap kernel
[INFO] architecture: x86_64
[INFO] firmware target: UEFI
[INFO] memory regions: ...
[INFO] bootstrap: OK
```

Panics use:

```text
[PANIC] ...
```

## Future path

This early channel will later feed or coexist with:

- exception reports;
- crash dumps;
- in-memory ring buffers;
- framebuffer panic output;
- userspace log collection;
- persistent diagnostic export from recovery.

The early logger must stay allocation-free and usable before the heap, scheduler, filesystem, and userspace exist.
