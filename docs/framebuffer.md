# Early Framebuffer Output

PhoenixOS uses the UEFI-provided framebuffer as the first graphical output path.

The early framebuffer layer is intentionally small. It is not the future compositor or graphics driver API.

## Responsibilities

- validate framebuffer geometry;
- handle RGB, BGR, and simple grayscale layouts;
- perform bounds-checked pixel writes;
- render an early PhoenixOS boot banner;
- provide deterministic host-side tests for pixel addressing and channel order;
- fail cleanly when a firmware pixel format is not yet supported.

## Why keep it separate

The kernel should not spread raw framebuffer offsets and pixel-format branching across unrelated subsystems.

This module creates an early boundary:

```text
bootloader framebuffer
        |
phoenix-framebuffer
        |
boot diagnostics today
        |
future early console / panic screen
```

The desktop compositor and accelerated graphics stack will later use a separate graphics abstraction.

## Font

The boot banner uses the no-std Noto Sans Mono bitmap crate with only the required ASCII/regular/16px feature set enabled.

Third-party font/library licensing will be tracked as part of release packaging.

## Next steps

- add an early graphical panic screen;
- add a small framebuffer console for diagnostics;
- exercise multiple framebuffer formats in tests;
- later hand display ownership to the real graphics stack rather than keeping firmware framebuffer access globally available.
