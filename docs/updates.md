# System Update Architecture

PhoenixOS treats system updates as a core reliability and security subsystem.

## Goals

An interrupted update must not brick a supported machine. The currently working system remains bootable until a new generation has been fully downloaded, verified, installed, and selected for boot.

The update subsystem must support:

- signed metadata and payload verification;
- resumable background downloads;
- system generations or A/B slots;
- atomic activation;
- automatic rollback;
- offline/recovery updates;
- release channels;
- staged rollout metadata;
- update history;
- explicit compatibility checks;
- deterministic fault-injection tests.

## Proposed disk/update model

The first implementation will use an A/B-style layout because it is straightforward to reason about and test:

```text
EFI System Partition
  +-- Phoenix boot manager
  +-- boot metadata

System A
  +-- kernel
  +-- base userspace
  +-- system drivers
  +-- immutable/versioned base

System B
  +-- kernel
  +-- base userspace
  +-- system drivers
  +-- immutable/versioned base

Data
  +-- users
  +-- applications/data
  +-- mutable system state

Recovery
  +-- recovery environment
  +-- updater/repair tools
```

One system slot is active. The updater writes only to the inactive slot.

## Update transaction

1. Fetch signed update metadata.
2. Verify metadata chain, version policy, channel, target architecture, and hardware constraints.
3. Download payloads into a resumable staging area.
4. Verify cryptographic hashes/signatures before installation.
5. Install the complete new system into the inactive slot/generation.
6. Run offline consistency checks.
7. Write new boot metadata atomically, marking the new slot as **pending**.
8. Reboot into the pending slot.
9. Early userspace and system services run health checks.
10. Mark the boot **successful** only after the required health gate passes.
11. If the new slot fails repeatedly or never confirms success, the boot manager returns to the previous known-good slot automatically.

Power loss before step 7 leaves the old system active. Power loss after step 7 still leaves the previous slot intact for rollback.

## Trust model

Update trust is separate from transport security. HTTPS protects transport, but every update is also authenticated by PhoenixOS update-signing keys.

The metadata design must allow:

- offline root key;
- delegated online signing keys;
- expiry;
- key rotation;
- revocation/recovery;
- rollback/freeze protection;
- channel-specific signing policy.

The exact metadata format will be selected/design-reviewed before implementation; novel cryptography is out of scope.

## Components updated

The updater must model compatibility explicitly for:

- boot manager;
- kernel;
- base userspace;
- core libraries;
- system drivers;
- firmware-facing support data;
- bundled applications;
- SDK/runtime packages;
- configuration/data migrations.

A system update may refuse activation when a required compatibility contract cannot be satisfied.

## Applications

Ordinary applications do not need a whole-system A/B slot. The package manager can update them transactionally using versioned package objects and an atomic active-version switch.

Shared trust/signature infrastructure should be reused for both system and application packages.

## Channels

Planned channels:

- `stable`
- `beta`
- `development`

Moving to a less stable channel is allowed explicitly. Moving back to a stable channel must validate data/config compatibility rather than blindly downgrading.

## Staged rollouts

Metadata may define a rollout percentage or cohort policy so a defective release can be stopped before reaching every installation.

The client must never silently change channels to participate in a rollout.

## Delta updates

Delta payloads are an optimization, not a correctness requirement.

The first implementation may ship full system payloads. Delta updates are introduced only after full-image/generation updates, rollback, and recovery are proven reliable.

## Recovery

Recovery must be independently bootable and able to:

- inspect both system slots;
- choose a known-good generation;
- repair boot metadata;
- verify installed system hashes;
- apply an offline signed update;
- reinstall the base system without destroying user data where possible;
- export diagnostics.

## Testing

The update subsystem is not considered production-ready without automated failure injection covering:

- power loss during download;
- power loss during inactive-slot write;
- corrupt payload;
- bad signature;
- expired metadata;
- missing dependency;
- full disk;
- failed configuration migration;
- kernel panic on first boot;
- userspace health-check failure;
- repeated failed boots;
- rollback to previous slot;
- interrupted rollback;
- recovery-mode update.

The release gate should eventually boot old releases, update them to the candidate release in QEMU, intentionally fail selected update phases, and verify that at least one known-good boot path always remains.
