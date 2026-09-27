use phoenix_capability::{Capability, CapabilityError, CapabilityHandle, CapabilityTable, Rights};
use phoenix_process::ProcessId;

pub struct ProcessResources<const CAPACITY: usize> {
    owner: ProcessId,
    capabilities: CapabilityTable<CAPACITY>,
}

impl<const CAPACITY: usize> ProcessResources<CAPACITY> {
    pub const fn new(owner: ProcessId) -> Self {
        Self {
            owner,
            capabilities: CapabilityTable::new(),
        }
    }

    pub const fn owner(&self) -> ProcessId {
        self.owner
    }

    pub fn grant(
        &mut self,
        capability: Capability,
    ) -> Result<CapabilityHandle, CapabilityError> {
        self.capabilities.insert(capability)
    }

    pub fn get(&self, handle: CapabilityHandle) -> Result<Capability, CapabilityError> {
        self.capabilities.get(handle)
    }

    pub fn require(
        &self,
        handle: CapabilityHandle,
        rights: Rights,
    ) -> Result<Capability, CapabilityError> {
        self.capabilities.require(handle, rights)
    }

    pub fn derive(
        &mut self,
        handle: CapabilityHandle,
        rights: Rights,
    ) -> Result<CapabilityHandle, CapabilityError> {
        self.capabilities.derive(handle, rights)
    }

    pub fn revoke(
        &mut self,
        handle: CapabilityHandle,
    ) -> Result<Capability, CapabilityError> {
        self.capabilities.revoke(handle)
    }

    pub fn take_for_transfer(
        &mut self,
        handle: CapabilityHandle,
    ) -> Result<Capability, CapabilityError> {
        self.capabilities.take_for_transfer(handle)
    }
}
