# SDK and Porting Strategy

PhoenixOS applications should not need to know that the kernel is written in Rust.

## Stable ABI

The stable application boundary is C-compatible. Language SDKs wrap it.

Planned SDKs/examples:

- C;
- C++;
- Rust;
- Zig;
- additional languages as runtimes are ported.

## Native API themes

- handles/capabilities;
- asynchronous I/O;
- processes/threads;
- virtual memory;
- filesystem;
- sockets;
- GUI surfaces and event loop;
- timers;
- shared memory;
- package/application metadata.

## POSIX compatibility

A compatibility layer is a strategic porting tool.

Initial priority APIs include file descriptors, open/read/write/close, directories and stat-like metadata, mmap, pthread-style threading subset, clocks/sleep, pipes, sockets, poll/select compatibility, process environment, and only as much signal behavior as useful ports require.

The goal is practical source compatibility, not blindly reproducing every historical Unix behavior.

## Reference projects

The SDK is not considered complete without buildable examples:

1. hello-world CLI;
2. file I/O CLI;
3. TCP/HTTPS client;
4. GUI hello-world;
5. packaged GUI application;
6. minimal device driver;
7. simple 2D game.

## Porting guide template

Each substantial port should document upstream version/commit, build-system changes, missing APIs, PhoenixOS patches, runtime assumptions, test coverage, and upgrade procedure.
