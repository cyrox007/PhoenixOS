use bootloader_api::info::MemoryRegion;
use phoenix_acpi::McfgInfo;
use phoenix_pcie::{ConfigAccess, EcamSegment, EnumerationSummary, PcieError};
use x86_64::VirtAddr;

const MINIMUM_BOOTLOADER_PHYSICAL_MAPPING: u64 = 0x1_0000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformPcieError {
    InvalidSegment(PcieError),
    EcamOutsidePhysicalMapping,
    AddressOverflow,
}

impl From<PcieError> for PlatformPcieError {
    fn from(error: PcieError) -> Self {
        Self::InvalidSegment(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformPcieSummary {
    pub function_count: u32,
    pub bridge_count: u32,
    pub storage_controller_count: u32,
    pub usb_controller_count: u32,
}

impl PlatformPcieSummary {
    const fn empty() -> Self {
        Self {
            function_count: 0,
            bridge_count: 0,
            storage_controller_count: 0,
            usb_controller_count: 0,
        }
    }

    fn include(&mut self, segment: EnumerationSummary) {
        self.function_count = self.function_count.saturating_add(segment.function_count);
        self.bridge_count = self.bridge_count.saturating_add(segment.bridge_count);
        self.storage_controller_count = self
            .storage_controller_count
            .saturating_add(segment.storage_controller_count);
        self.usb_controller_count = self
            .usb_controller_count
            .saturating_add(segment.usb_controller_count);
    }
}

struct DirectEcam {
    physical_memory_offset: VirtAddr,
}

impl ConfigAccess for DirectEcam {
    fn read_u32(&self, physical_address: u64) -> u32 {
        let virtual_address = self.physical_memory_offset.as_u64() + physical_address;
        let pointer = VirtAddr::new(virtual_address).as_ptr::<u32>();
        unsafe { pointer.read_volatile() }
    }
}

pub fn enumerate(
    mcfg: &McfgInfo,
    regions: &[MemoryRegion],
    physical_memory_offset: VirtAddr,
) -> Result<PlatformPcieSummary, PlatformPcieError> {
    let mapped_physical_end = regions
        .iter()
        .fold(MINIMUM_BOOTLOADER_PHYSICAL_MAPPING, |current, region| {
            current.max(region.end)
        });
    let access = DirectEcam {
        physical_memory_offset,
    };
    let mut summary = PlatformPcieSummary::empty();

    for allocation in mcfg.iter() {
        let segment = EcamSegment::new(
            allocation.base_address,
            allocation.segment_group,
            allocation.start_bus,
            allocation.end_bus,
        )?;
        validate_segment_mapping(segment, mapped_physical_end)?;
        let segment_summary = phoenix_pcie::enumerate_segment(&access, segment, |_| {})?;
        summary.include(segment_summary);
    }

    Ok(summary)
}

fn validate_segment_mapping(
    segment: EcamSegment,
    mapped_physical_end: u64,
) -> Result<(), PlatformPcieError> {
    let buses_end = u64::from(segment.end_bus)
        .checked_add(1)
        .and_then(|bus| bus.checked_shl(20))
        .ok_or(PlatformPcieError::AddressOverflow)?;
    let end = segment
        .base_address
        .checked_add(buses_end)
        .ok_or(PlatformPcieError::AddressOverflow)?;

    if end > mapped_physical_end {
        return Err(PlatformPcieError::EcamOutsidePhysicalMapping);
    }

    Ok(())
}
