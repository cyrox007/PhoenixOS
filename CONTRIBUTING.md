# Contributing to PhoenixOS

## Branches

- `main`: stable integration points
- `develop`: active integration
- `feature/*`: focused changes

Normal work targets `develop` through a pull request.

## Change expectations

A subsystem change should include, where applicable, implementation, tests or a deterministic smoke test, documentation, and a status/roadmap update when milestone state changes.

Avoid exposing internal Rust layout as public ABI.

## Bootstrap checks

```bash
cargo fmt --all -- --check
cargo run --release --quiet
```

CI additionally boots the generated UEFI image under QEMU/OVMF.
