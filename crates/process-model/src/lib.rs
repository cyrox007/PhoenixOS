#![no_std]

use phoenix_capability::{Capability, CapabilityError, CapabilityHandle, CapabilityTable, Rights};

pub const PAGE_SIZE: u64 = 4096;

const LOWER_CANONICAL_MAX: u64 = 0x0000_7fff_ffff_ffff;
const UPPER_CANONICAL_MIN: u64 = 0xffff_8000_0000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressSpaceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Created,
    Runnable,
    Blocked,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessError {
    CapacityExceeded,
    IdExhausted,
    ProcessNotFound,
    InvalidTransition {
        from: ProcessState,
        to: ProcessState,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryPermissions(u8);

impl MemoryPermissions {
    const READ: u8 = 1 << 0;
    const WRITE: u8 = 1 << 1;
    const EXECUTE: u8 = 1 << 2;
    const USER: u8 = 1 << 3;

    pub const READ_ONLY: Self = Self(Self::READ);
    pub const READ_WRITE: Self = Self(Self::READ | Self::WRITE);
    pub const READ_EXECUTE: Self = Self(Self::READ | Self::EXECUTE);
    pub const USER_READ_ONLY: Self = Self(Self::READ | Self::USER);
    pub const USER_READ_WRITE: Self = Self(Self::READ | Self::WRITE | Self::USER);
    pub const USER_READ_EXECUTE: Self = Self(Self::READ | Self::EXECUTE | Self::USER);

    pub const fn readable(self) -> bool {
        self.0 & Self::READ != 0
    }

    pub const fn writable(self) -> bool {
        self.0 & Self::WRITE != 0
    }

    pub const fn executable(self) -> bool {
        self.0 & Self::EXECUTE != 0
    }

    pub const fn user_accessible(self) -> bool {
        self.0 & Self::USER != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Program,
    Heap,
    Stack,
    Shared,
    MemoryMapped,
    KernelReserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionError {
    ZeroLength,
    UnalignedStart,
    UnalignedLength,
    AddressOverflow,
    NonCanonical,
    Overlap,
    CapacityExceeded,
    RegionNotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualRegion {
    pub start: u64,
    pub length: u64,
    pub permissions: MemoryPermissions,
    pub kind: RegionKind,
}

impl VirtualRegion {
    pub fn new(
        start: u64,
        length: u64,
        permissions: MemoryPermissions,
        kind: RegionKind,
    ) -> Result<Self, RegionError> {
        if length == 0 {
            return Err(RegionError::ZeroLength);
        }

        if start % PAGE_SIZE != 0 {
            return Err(RegionError::UnalignedStart);
        }

        if length % PAGE_SIZE != 0 {
            return Err(RegionError::UnalignedLength);
        }

        let last = start
            .checked_add(length - 1)
            .ok_or(RegionError::AddressOverflow)?;

        if !is_canonical(start) || !is_canonical(last) {
            return Err(RegionError::NonCanonical);
        }

        Ok(Self {
            start,
            length,
            permissions,
            kind,
        })
    }

    pub fn end_exclusive(self) -> u64 {
        self.start + self.length
    }

    pub fn contains(self, address: u64) -> bool {
        address >= self.start && address < self.end_exclusive()
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end_exclusive() && other.start < self.end_exclusive()
    }
}

pub struct AddressSpace<const CAPACITY: usize> {
    id: AddressSpaceId,
    regions: [Option<VirtualRegion>; CAPACITY],
    len: usize,
}

impl<const CAPACITY: usize> AddressSpace<CAPACITY> {
    pub const fn new(id: AddressSpaceId) -> Self {
        Self {
            id,
            regions: [None; CAPACITY],
            len: 0,
        }
    }

    pub const fn id(&self) -> AddressSpaceId {
        self.id
    }

    pub const fn region_count(&self) -> usize {
        self.len
    }

    pub fn map_region(&mut self, region: VirtualRegion) -> Result<(), RegionError> {
        if self
            .regions
            .iter()
            .flatten()
            .copied()
            .any(|existing| existing.overlaps(region))
        {
            return Err(RegionError::Overlap);
        }

        let Some(index) = self.regions.iter().position(Option::is_none) else {
            return Err(RegionError::CapacityExceeded);
        };

        self.regions[index] = Some(region);
        self.len += 1;

        Ok(())
    }

    pub fn unmap_exact(&mut self, start: u64, length: u64) -> Result<VirtualRegion, RegionError> {
        let index = self
            .regions
            .iter()
            .position(|region| {
                region.is_some_and(|entry| entry.start == start && entry.length == length)
            })
            .ok_or(RegionError::RegionNotFound)?;

        let region = self.regions[index]
            .take()
            .ok_or(RegionError::RegionNotFound)?;
        self.len -= 1;

        Ok(region)
    }

    pub fn find(&self, address: u64) -> Option<VirtualRegion> {
        self.regions
            .iter()
            .flatten()
            .copied()
            .find(|region| region.contains(address))
    }
}

pub struct ProcessCapabilitySet<const CAPACITY: usize> {
    owner: ProcessId,
    table: CapabilityTable<CAPACITY>,
}

impl<const CAPACITY: usize> ProcessCapabilitySet<CAPACITY> {
    pub const fn new(owner: ProcessId) -> Self {
        Self {
            owner,
            table: CapabilityTable::new(),
        }
    }

    pub const fn owner(&self) -> ProcessId {
        self.owner
    }

    pub const fn len(&self) -> usize {
        self.table.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    pub fn insert(&mut self, capability: Capability) -> Result<CapabilityHandle, CapabilityError> {
        self.table.insert(capability)
    }

    pub fn get(&self, handle: CapabilityHandle) -> Result<Capability, CapabilityError> {
        self.table.get(handle)
    }

    pub fn require(
        &self,
        handle: CapabilityHandle,
        rights: Rights,
    ) -> Result<Capability, CapabilityError> {
        self.table.require(handle, rights)
    }

    pub fn derive(
        &mut self,
        handle: CapabilityHandle,
        rights: Rights,
    ) -> Result<CapabilityHandle, CapabilityError> {
        self.table.derive(handle, rights)
    }

    pub fn revoke(&mut self, handle: CapabilityHandle) -> Result<Capability, CapabilityError> {
        self.table.revoke(handle)
    }

    pub fn transfer_to<const TARGET_CAPACITY: usize>(
        &mut self,
        target: &mut ProcessCapabilitySet<TARGET_CAPACITY>,
        handle: CapabilityHandle,
    ) -> Result<CapabilityHandle, CapabilityError> {
        self.table.transfer_to(&mut target.table, handle)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessRecord {
    pub id: ProcessId,
    pub address_space: AddressSpaceId,
    pub state: ProcessState,
    pub exit_status: Option<u64>,
}

pub struct ProcessTable<const CAPACITY: usize> {
    entries: [Option<ProcessRecord>; CAPACITY],
    len: usize,
    next_process_id: u64,
    next_address_space_id: u64,
}

impl<const CAPACITY: usize> ProcessTable<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            entries: [None; CAPACITY],
            len: 0,
            next_process_id: 1,
            next_address_space_id: 1,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn create(&mut self) -> Result<ProcessRecord, ProcessError> {
        let Some(index) = self.entries.iter().position(Option::is_none) else {
            return Err(ProcessError::CapacityExceeded);
        };

        let process_id = ProcessId(self.next_process_id);
        let address_space_id = AddressSpaceId(self.next_address_space_id);

        self.next_process_id = self
            .next_process_id
            .checked_add(1)
            .ok_or(ProcessError::IdExhausted)?;

        self.next_address_space_id = self
            .next_address_space_id
            .checked_add(1)
            .ok_or(ProcessError::IdExhausted)?;

        let record = ProcessRecord {
            id: process_id,
            address_space: address_space_id,
            state: ProcessState::Created,
            exit_status: None,
        };

        self.entries[index] = Some(record);
        self.len += 1;

        Ok(record)
    }

    pub fn state(&self, id: ProcessId) -> Option<ProcessState> {
        self.find(id).map(|index| {
            self.entries[index]
                .expect("найденная запись процесса обязана существовать")
                .state
        })
    }

    pub fn record(&self, id: ProcessId) -> Option<ProcessRecord> {
        self.find(id).and_then(|index| self.entries[index])
    }

    pub fn transition(&mut self, id: ProcessId, target: ProcessState) -> Result<(), ProcessError> {
        let index = self.find(id).ok_or(ProcessError::ProcessNotFound)?;
        let current = self.entries[index].ok_or(ProcessError::ProcessNotFound)?;

        if !valid_transition(current.state, target) {
            return Err(ProcessError::InvalidTransition {
                from: current.state,
                to: target,
            });
        }

        self.entries[index] = Some(ProcessRecord {
            state: target,
            ..current
        });

        Ok(())
    }

    pub fn exit(&mut self, id: ProcessId, status: u64) -> Result<(), ProcessError> {
        let index = self.find(id).ok_or(ProcessError::ProcessNotFound)?;
        let current = self.entries[index].ok_or(ProcessError::ProcessNotFound)?;

        if !matches!(current.state, ProcessState::Runnable | ProcessState::Blocked) {
            return Err(ProcessError::InvalidTransition {
                from: current.state,
                to: ProcessState::Exited,
            });
        }

        self.entries[index] = Some(ProcessRecord {
            state: ProcessState::Exited,
            exit_status: Some(status),
            ..current
        });

        Ok(())
    }

    pub fn remove_exited(&mut self, id: ProcessId) -> Result<ProcessRecord, ProcessError> {
        let index = self.find(id).ok_or(ProcessError::ProcessNotFound)?;
        let record = self.entries[index].ok_or(ProcessError::ProcessNotFound)?;

        if record.state != ProcessState::Exited {
            return Err(ProcessError::InvalidTransition {
                from: record.state,
                to: ProcessState::Exited,
            });
        }

        self.entries[index] = None;
        self.len -= 1;

        Ok(record)
    }

    fn find(&self, id: ProcessId) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.is_some_and(|record| record.id == id))
    }
}

impl<const CAPACITY: usize> Default for ProcessTable<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

fn valid_transition(from: ProcessState, to: ProcessState) -> bool {
    matches!(
        (from, to),
        (ProcessState::Created, ProcessState::Runnable)
            | (ProcessState::Runnable, ProcessState::Blocked)
            | (ProcessState::Blocked, ProcessState::Runnable)
    )
}

fn is_canonical(address: u64) -> bool {
    address <= LOWER_CANONICAL_MAX || address >= UPPER_CANONICAL_MIN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_region(start: u64, pages: u64) -> VirtualRegion {
        VirtualRegion::new(
            start,
            pages * PAGE_SIZE,
            MemoryPermissions::USER_READ_WRITE,
            RegionKind::Program,
        )
        .unwrap()
    }

    #[test]
    fn address_space_rejects_overlaps() {
        let mut space = AddressSpace::<4>::new(AddressSpaceId(7));
        space.map_region(user_region(0x4000, 2)).unwrap();

        assert_eq!(
            space.map_region(user_region(0x5000, 1)),
            Err(RegionError::Overlap)
        );

        space.map_region(user_region(0x6000, 1)).unwrap();
        assert_eq!(space.region_count(), 2);
    }

    #[test]
    fn address_space_finds_and_unmaps_exact_region() {
        let mut space = AddressSpace::<2>::new(AddressSpaceId(1));
        let region = user_region(0x20_0000, 3);

        space.map_region(region).unwrap();

        assert_eq!(space.find(0x20_1000), Some(region));
        assert_eq!(space.find(0x30_0000), None);
        assert_eq!(space.unmap_exact(region.start, region.length), Ok(region));
        assert_eq!(space.region_count(), 0);
    }

    #[test]
    fn region_validation_rejects_bad_ranges() {
        assert_eq!(
            VirtualRegion::new(
                0x1001,
                PAGE_SIZE,
                MemoryPermissions::USER_READ_ONLY,
                RegionKind::Program,
            ),
            Err(RegionError::UnalignedStart)
        );

        assert_eq!(
            VirtualRegion::new(
                0x2000,
                1,
                MemoryPermissions::USER_READ_ONLY,
                RegionKind::Program,
            ),
            Err(RegionError::UnalignedLength)
        );

        assert_eq!(
            VirtualRegion::new(
                0x0000_8000_0000_0000,
                PAGE_SIZE,
                MemoryPermissions::USER_READ_ONLY,
                RegionKind::Program,
            ),
            Err(RegionError::NonCanonical)
        );
    }

    #[test]
    fn process_lifecycle_is_explicit() {
        let mut table = ProcessTable::<2>::new();
        let process = table.create().unwrap();

        assert_eq!(process.state, ProcessState::Created);

        table
            .transition(process.id, ProcessState::Runnable)
            .unwrap();
        table.transition(process.id, ProcessState::Blocked).unwrap();
        table
            .transition(process.id, ProcessState::Runnable)
            .unwrap();
        table.exit(process.id, u64::MAX).unwrap();

        assert_eq!(table.state(process.id), Some(ProcessState::Exited));
        let exited = table.remove_exited(process.id).unwrap();
        assert_eq!(exited.id, process.id);
        assert_eq!(exited.exit_status, Some(u64::MAX));
        assert!(table.is_empty());
    }

    #[test]
    fn process_lifecycle_rejects_invalid_transition() {
        let mut table = ProcessTable::<1>::new();
        let process = table.create().unwrap();

        assert_eq!(
            table.transition(process.id, ProcessState::Blocked),
            Err(ProcessError::InvalidTransition {
                from: ProcessState::Created,
                to: ProcessState::Blocked,
            })
        );
        assert_eq!(
            table.exit(process.id, 1),
            Err(ProcessError::InvalidTransition {
                from: ProcessState::Created,
                to: ProcessState::Exited,
            })
        );
    }

    #[test]
    fn process_table_enforces_capacity() {
        let mut table = ProcessTable::<1>::new();
        table.create().unwrap();

        assert_eq!(table.create(), Err(ProcessError::CapacityExceeded));
    }

    #[test]
    fn process_capabilities_keep_owner_and_rights() {
        let mut set = ProcessCapabilitySet::<2>::new(ProcessId(7));
        let handle = set
            .insert(Capability {
                object: phoenix_capability::ObjectId(11),
                kind: phoenix_capability::ObjectKind::Endpoint,
                rights: Rights::READ.union(Rights::DUPLICATE),
            })
            .unwrap();

        assert_eq!(set.owner(), ProcessId(7));
        assert_eq!(set.require(handle, Rights::READ).unwrap().object.0, 11);
        assert_eq!(
            set.require(handle, Rights::WRITE),
            Err(CapabilityError::MissingRight)
        );
    }

    #[test]
    fn process_capability_transfer_moves_the_handle() {
        let mut source = ProcessCapabilitySet::<1>::new(ProcessId(1));
        let mut target = ProcessCapabilitySet::<1>::new(ProcessId(2));
        let handle = source
            .insert(Capability {
                object: phoenix_capability::ObjectId(90),
                kind: phoenix_capability::ObjectKind::Endpoint,
                rights: Rights::READ.union(Rights::TRANSFER),
            })
            .unwrap();

        let received = source.transfer_to(&mut target, handle).unwrap();

        assert!(source.is_empty());
        assert_eq!(source.get(handle), Err(CapabilityError::InvalidHandle));
        assert_eq!(target.get(received).unwrap().object.0, 90);
    }
}
