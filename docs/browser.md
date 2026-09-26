# Browser Strategy

A usable modern browser is an explicit PhoenixOS milestone.

## What PhoenixOS will build

PhoenixOS should own the browser application shell and OS integration:

- tabs/windows;
- address/search UI;
- history/bookmarks;
- downloads;
- permissions;
- certificate UI;
- proxy settings;
- file picker;
- notifications;
- password/credential integration when the security model is ready;
- process sandbox integration.

## What PhoenixOS will not build from scratch

The project will not initially implement its own HTML parser, CSS layout engine, JavaScript VM, DOM, Web APIs, or WebGL/WebGPU stack.

Reimplementing the modern web platform would delay the OS by years and create a separate standards-compatibility project.

## Engine boundary

```text
Phoenix Browser
      |
browser-engine adapter
      |
+---------------------------+
| Servo (primary candidate) |
| future alternative engine |
+---------------------------+
      |
PhoenixOS platform adapter
      +-- sockets/TLS
      +-- windows/surfaces
      +-- graphics/GPU
      +-- fonts
      +-- input
      +-- audio/video
      +-- files
      +-- clipboard
      +-- timers/threads
```

The adapter boundary keeps PhoenixOS able to replace or add an engine without changing its kernel ABI.

## Primary candidate: Servo

Servo is the initial porting target because its Rust codebase, modular design, embedding work, and WebView-oriented API align with PhoenixOS.

This is a porting candidate, not a permanent architectural dependency. A feasibility checkpoint occurs only after PhoenixOS has processes/threads, virtual memory, filesystem, sockets, DNS/TLS, window surfaces, fonts, input, shared libraries or an equivalent linking strategy, and enough POSIX compatibility for dependencies.

## Browser milestone definition

The browser milestone is not complete merely because a blank page renders.

Minimum useful target:

- multiple tabs;
- DNS and HTTPS;
- certificate validation;
- common HTML/CSS rendering;
- JavaScript;
- persistent cookies/cache;
- downloads;
- text input, selection, copy/paste;
- basic audio/video;
- crash isolation;
- browser-process sandboxing;
- reproducible compatibility tests against a curated real-site list.

## Porting preparation

The POSIX layer, libc surface, threading, files, sockets, mmap, clocks, and process APIs are strategic because they reduce the cost of bringing large third-party codebases such as a browser engine to PhoenixOS.
