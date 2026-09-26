# Networking Architecture

Networking is a required PhoenixOS platform subsystem because the browser, package manager, updater, remote development, and ordinary desktop use all depend on it.

## Layering

```text
applications
  |
native socket API / POSIX socket compatibility
  |
TLS / HTTP / DNS libraries and system services
  |
TCP / UDP / ICMP
  |
IPv4 / IPv6 / routing
  |
ARP / NDP
  |
Ethernet / Wi-Fi link layer
  |
NIC driver API
  |
PCIe / USB device
```

## Kernel/user split

The packet fast path, routing primitives, socket buffers, wait/wakeup integration, and NIC queue interaction may live in kernel space for low overhead.

Configuration and policy belong in userspace where possible:

- network manager;
- DHCP clients;
- DNS policy/cache;
- certificate management;
- proxy policy;
- Wi-Fi connection policy;
- UI.

The boundary is allowed to evolve as profiling data becomes available.

## Native sockets

The native API must support:

- IPv4 and IPv6;
- TCP and UDP;
- nonblocking/asynchronous operation;
- scatter/gather I/O;
- poll/wait integration;
- address reuse and common socket options;
- interface binding;
- dual-stack behavior;
- zero-copy or page-sharing extensions where safe and useful.

A POSIX socket compatibility layer maps common BSD socket calls onto the native API.

## Addressing and discovery

Required:

- loopback;
- static addresses;
- DHCPv4;
- IPv6 link-local addresses;
- NDP;
- SLAAC;
- DHCPv6;
- configurable routing table;
- DNS A/AAAA/CNAME handling;
- search domains and resolver ordering.

## Security and web prerequisites

Required before the browser milestone can be called usable:

- system trust/certificate store;
- secure random source;
- wall-clock/time synchronization path;
- TLS 1.2 and TLS 1.3;
- SNI and ALPN;
- HTTP/1.1;
- HTTP/2;
- HTTPS certificate validation;
- HTTP/SOCKS proxy support;
- revocation/update strategy for trust roots.

Cryptographic primitives should come from reviewed libraries rather than novel in-house cryptography.

## Driver milestones

1. VirtIO-net under QEMU.
2. One real wired Ethernet controller used by the supported reference PC.
3. Generalized NIC queue/checksum/offload interfaces.
4. Wi-Fi framework.
5. Driver for the first explicitly supported Wi-Fi controller.
6. Additional drivers according to hardware demand.

## Test strategy

Automated tests should cover packet encode/decode, malformed input, TCP state transitions, loopback sockets, QEMU multi-node connectivity, DHCP/DNS, IPv4/IPv6, TLS validation, HTTP integration, and throughput/latency regressions.
