#![no_std]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Process,
    Thread,
    AddressSpace,
    Memory,
    Endpoint,
    File,
    Device,
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(u32);

impl Rights {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const MAP: Self = Self(1 << 3);
    pub const WAIT: Self = Self(1 << 4);
    pub const SIGNAL: Self = Self(1 << 5);
    pub const TRANSFER: Self = Self(1 << 6);
    pub const DUPLICATE: Self = Self(1 << 7);
    pub const MANAGE: Self = Self(1 << 8);

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    pub const fn is_subset_of(self, parent: Self) -> bool {
        parent.contains(self)
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityHandle {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub object: ObjectId,
    pub kind: ObjectKind,
    pub rights: Rights,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityError {
    CapacityExceeded,
    InvalidHandle,
    MissingRight,
    RightsEscalation,
    GenerationExhausted,
}

#[derive(Clone, Copy)]
struct Slot {
    generation: u32,
    capability: Option<Capability>,
}

impl Slot {
    const EMPTY: Self = Self {
        generation: 1,
        capability: None,
    };
}

pub struct CapabilityTable<const CAPACITY: usize> {
    slots: [Slot; CAPACITY],
    len: usize,
}

impl<const CAPACITY: usize> CapabilityTable<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            slots: [Slot::EMPTY; CAPACITY],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn insert(&mut self, capability: Capability) -> Result<CapabilityHandle, CapabilityError> {
        let Some(index) = self.slots.iter().position(|slot| slot.capability.is_none()) else {
            return Err(CapabilityError::CapacityExceeded);
        };

        let slot = &mut self.slots[index];
        slot.capability = Some(capability);
        self.len += 1;

        Ok(CapabilityHandle {
            slot: index as u32,
            generation: slot.generation,
        })
    }

    pub fn get(&self, handle: CapabilityHandle) -> Result<Capability, CapabilityError> {
        let slot = self.slot(handle)?;
        slot.capability.ok_or(CapabilityError::InvalidHandle)
    }

    pub fn require(
        &self,
        handle: CapabilityHandle,
        rights: Rights,
    ) -> Result<Capability, CapabilityError> {
        let capability = self.get(handle)?;
        if !capability.rights.contains(rights) {
            return Err(CapabilityError::MissingRight);
        }

        Ok(capability)
    }

    pub fn derive(
        &mut self,
        source: CapabilityHandle,
        rights: Rights,
    ) -> Result<CapabilityHandle, CapabilityError> {
        let capability = self.require(source, Rights::DUPLICATE)?;
        if !rights.is_subset_of(capability.rights) {
            return Err(CapabilityError::RightsEscalation);
        }

        self.insert(Capability {
            rights,
            ..capability
        })
    }

    pub fn revoke(&mut self, handle: CapabilityHandle) -> Result<Capability, CapabilityError> {
        let index = self.index(handle)?;
        let slot = &mut self.slots[index];

        if slot.generation != handle.generation {
            return Err(CapabilityError::InvalidHandle);
        }

        let capability = slot
            .capability
            .take()
            .ok_or(CapabilityError::InvalidHandle)?;
        slot.generation = slot
            .generation
            .checked_add(1)
            .ok_or(CapabilityError::GenerationExhausted)?;
        self.len -= 1;

        Ok(capability)
    }

    pub fn take_for_transfer(
        &mut self,
        handle: CapabilityHandle,
    ) -> Result<Capability, CapabilityError> {
        self.require(handle, Rights::TRANSFER)?;
        self.revoke(handle)
    }

    fn slot(&self, handle: CapabilityHandle) -> Result<&Slot, CapabilityError> {
        let index = self.index(handle)?;
        let slot = &self.slots[index];

        if slot.generation != handle.generation {
            return Err(CapabilityError::InvalidHandle);
        }

        Ok(slot)
    }

    fn index(&self, handle: CapabilityHandle) -> Result<usize, CapabilityError> {
        let index = handle.slot as usize;
        if index >= CAPACITY {
            return Err(CapabilityError::InvalidHandle);
        }

        Ok(index)
    }
}

impl<const CAPACITY: usize> Default for CapabilityTable<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rights() -> Rights {
        Rights::READ
            .union(Rights::WRITE)
            .union(Rights::TRANSFER)
            .union(Rights::DUPLICATE)
    }

    fn capability(object: u64) -> Capability {
        Capability {
            object: ObjectId(object),
            kind: ObjectKind::Memory,
            rights: rights(),
        }
    }

    #[test]
    fn stale_handle_does_not_refer_to_reused_slot() {
        let mut table = CapabilityTable::<1>::new();
        let first = table.insert(capability(10)).unwrap();

        assert_eq!(table.revoke(first).unwrap().object, ObjectId(10));

        let second = table.insert(capability(20)).unwrap();
        assert_eq!(first.slot, second.slot);
        assert_ne!(first.generation, second.generation);
        assert_eq!(table.get(first), Err(CapabilityError::InvalidHandle));
        assert_eq!(table.get(second).unwrap().object, ObjectId(20));
    }

    #[test]
    fn derived_capability_can_only_reduce_rights() {
        let mut table = CapabilityTable::<3>::new();
        let source = table.insert(capability(7)).unwrap();

        let read_only = table.derive(source, Rights::READ).unwrap();
        let derived = table.get(read_only).unwrap();

        assert_eq!(derived.object, ObjectId(7));
        assert_eq!(derived.rights, Rights::READ);
        assert_eq!(
            table.derive(source, Rights::MANAGE),
            Err(CapabilityError::RightsEscalation)
        );
    }

    #[test]
    fn transfer_requires_explicit_right() {
        let mut table = CapabilityTable::<2>::new();
        let handle = table
            .insert(Capability {
                object: ObjectId(1),
                kind: ObjectKind::Endpoint,
                rights: Rights::READ,
            })
            .unwrap();

        assert_eq!(
            table.take_for_transfer(handle),
            Err(CapabilityError::MissingRight)
        );
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn transfer_consumes_the_source_handle() {
        let mut table = CapabilityTable::<1>::new();
        let handle = table.insert(capability(33)).unwrap();

        let moved = table.take_for_transfer(handle).unwrap();

        assert_eq!(moved.object, ObjectId(33));
        assert!(table.is_empty());
        assert_eq!(table.get(handle), Err(CapabilityError::InvalidHandle));
    }

    #[test]
    fn table_enforces_capacity() {
        let mut table = CapabilityTable::<1>::new();
        table.insert(capability(1)).unwrap();

        assert_eq!(
            table.insert(capability(2)),
            Err(CapabilityError::CapacityExceeded)
        );
    }
}
