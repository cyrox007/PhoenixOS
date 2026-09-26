#![no_std]

use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use x86_64::{
    PhysAddr,
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

pub const FRAME_SIZE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySummary {
    pub region_count: usize,
    pub usable_region_count: usize,
    pub total_bytes: u64,
    pub usable_bytes: u64,
}

impl MemorySummary {
    pub fn from_regions(regions: &[MemoryRegion]) -> Self {
        let mut summary = Self {
            region_count: regions.len(),
            usable_region_count: 0,
            total_bytes: 0,
            usable_bytes: 0,
        };

        for region in regions {
            let bytes = region.end.saturating_sub(region.start);
            summary.total_bytes = summary.total_bytes.saturating_add(bytes);

            if region.kind == MemoryRegionKind::Usable {
                summary.usable_region_count += 1;
                summary.usable_bytes = summary.usable_bytes.saturating_add(bytes);
            }
        }

        summary
    }
}

/// Bootstrap-only allocator over firmware-provided usable memory regions.
///
/// This allocator is intentionally simple and heap-free. It advances monotonically
/// and never reclaims frames. A later milestone will replace it with the system
/// physical-memory allocator while preserving the same safety rules.
pub struct BootFrameAllocator<'a> {
    regions: &'a [MemoryRegion],
    region_index: usize,
    cursor: u64,
}

impl<'a> BootFrameAllocator<'a> {
    /// # Safety
    ///
    /// The caller must ensure that this allocator has exclusive ownership of frame
    /// allocation from the supplied usable regions. Creating two live allocators
    /// for the same regions and allocating from both can return duplicate frames.
    pub unsafe fn new(regions: &'a [MemoryRegion]) -> Self {
        Self {
            regions,
            region_index: 0,
            cursor: 0,
        }
    }

    pub fn allocate_4k(&mut self) -> Option<PhysFrame<Size4KiB>> {
        loop {
            let region = self.regions.get(self.region_index)?;

            if region.kind != MemoryRegionKind::Usable {
                self.region_index += 1;
                self.cursor = 0;
                continue;
            }

            let candidate = align_up(self.cursor.max(region.start), FRAME_SIZE)?;

            if candidate.checked_add(FRAME_SIZE)? <= region.end {
                self.cursor = candidate + FRAME_SIZE;
                return Some(PhysFrame::containing_address(PhysAddr::new(candidate)));
            }

            self.region_index += 1;
            self.cursor = 0;
        }
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootFrameAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.allocate_4k()
    }
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_regions() {
        let regions = [
            MemoryRegion {
                start: 0,
                end: 0x1000,
                kind: MemoryRegionKind::Bootloader,
            },
            MemoryRegion {
                start: 0x1000,
                end: 0x9000,
                kind: MemoryRegionKind::Usable,
            },
        ];

        let summary = MemorySummary::from_regions(&regions);

        assert_eq!(summary.region_count, 2);
        assert_eq!(summary.usable_region_count, 1);
        assert_eq!(summary.total_bytes, 0x9000);
        assert_eq!(summary.usable_bytes, 0x8000);
    }

    #[test]
    fn allocator_skips_reserved_and_aligns_frames() {
        let regions = [
            MemoryRegion {
                start: 0,
                end: 0x1800,
                kind: MemoryRegionKind::Bootloader,
            },
            MemoryRegion {
                start: 0x1801,
                end: 0x5000,
                kind: MemoryRegionKind::Usable,
            },
        ];

        let mut allocator = unsafe { BootFrameAllocator::new(&regions) };

        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x2000
        );
        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x3000
        );
        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x4000
        );
        assert!(allocator.allocate_4k().is_none());
    }

    #[test]
    fn allocator_moves_between_usable_regions() {
        let regions = [
            MemoryRegion {
                start: 0x1000,
                end: 0x2000,
                kind: MemoryRegionKind::Usable,
            },
            MemoryRegion {
                start: 0x2000,
                end: 0x3000,
                kind: MemoryRegionKind::Bootloader,
            },
            MemoryRegion {
                start: 0x8000,
                end: 0x9000,
                kind: MemoryRegionKind::Usable,
            },
        ];

        let mut allocator = unsafe { BootFrameAllocator::new(&regions) };

        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x1000
        );
        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x8000
        );
        assert!(allocator.allocate_4k().is_none());
    }
}
