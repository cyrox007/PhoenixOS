use phoenix_elf_loader::{ElfError, ElfImage, ElfType, LoadSegment};
use phoenix_process::{MemoryPermissions, PAGE_SIZE, RegionKind, VirtualRegion};
use x86_64::VirtAddr;
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, Size4KiB};

use crate::process_space::{ProcessAddressSpace, ProcessAddressSpaceError};

#[derive(Debug)]
pub enum ElfLoadError {
    Elf(ElfError),
    UnsupportedImageType,
    UnalignedSegment,
    UnreadableSegment,
    WritableExecutableSegment,
    AddressOverflow,
    AddressSpace(ProcessAddressSpaceError),
    EntryPointNotExecutable,
}

impl From<ElfError> for ElfLoadError {
    fn from(error: ElfError) -> Self {
        Self::Elf(error)
    }
}

impl From<ProcessAddressSpaceError> for ElfLoadError {
    fn from(error: ProcessAddressSpaceError) -> Self {
        Self::AddressSpace(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedElf {
    pub entry_point: u64,
    pub segment_count: usize,
}

pub fn load_elf<A, const CAPACITY: usize>(
    image: &[u8],
    space: &mut ProcessAddressSpace<CAPACITY>,
    allocator: &mut A,
) -> Result<LoadedElf, ElfLoadError>
where
    A: FrameAllocator<Size4KiB> + FrameDeallocator<Size4KiB>,
{
    let elf = ElfImage::parse(image)?;
    if elf.elf_type() != ElfType::Executable {
        return Err(ElfLoadError::UnsupportedImageType);
    }

    let mut mapped = [(0_u64, 0_u64); CAPACITY];
    let mut mapped_count = 0_usize;
    let mut segment_count = 0_usize;
    let mut entry_point_executable = false;

    for segment in elf.load_segments() {
        let segment = match segment {
            Ok(segment) => segment,
            Err(error) => {
                rollback(space, allocator, &mapped, mapped_count);
                return Err(ElfLoadError::Elf(error));
            }
        };

        if segment.memory_size == 0 {
            continue;
        }

        let region = match region_for_segment(segment) {
            Ok(region) => region,
            Err(error) => {
                rollback(space, allocator, &mapped, mapped_count);
                return Err(error);
            }
        };

        if let Err(error) = space.map_region(region, allocator) {
            rollback(space, allocator, &mapped, mapped_count);
            return Err(ElfLoadError::AddressSpace(error));
        }

        mapped[mapped_count] = (region.start, region.length);
        mapped_count += 1;

        if let Err(error) =
            space.write_user_bytes(VirtAddr::new(segment.virtual_address), segment.file_bytes())
        {
            rollback(space, allocator, &mapped, mapped_count);
            return Err(ElfLoadError::AddressSpace(error));
        }

        if segment.flags.executable() && segment_contains(segment, elf.entry_point()) {
            entry_point_executable = true;
        }
        segment_count += 1;
    }

    if !entry_point_executable {
        rollback(space, allocator, &mapped, mapped_count);
        return Err(ElfLoadError::EntryPointNotExecutable);
    }

    Ok(LoadedElf {
        entry_point: elf.entry_point(),
        segment_count,
    })
}

fn region_for_segment(segment: LoadSegment<'_>) -> Result<VirtualRegion, ElfLoadError> {
    if segment.virtual_address % PAGE_SIZE != 0 {
        return Err(ElfLoadError::UnalignedSegment);
    }
    if !segment.flags.readable() {
        return Err(ElfLoadError::UnreadableSegment);
    }
    if segment.flags.writable() && segment.flags.executable() {
        return Err(ElfLoadError::WritableExecutableSegment);
    }

    let length = page_aligned_length(segment.memory_size)?;
    let permissions = segment_permissions(segment);
    VirtualRegion::new(
        segment.virtual_address,
        length,
        permissions,
        RegionKind::Program,
    )
    .map_err(ProcessAddressSpaceError::Region)
    .map_err(ElfLoadError::AddressSpace)
}

fn segment_permissions(segment: LoadSegment<'_>) -> MemoryPermissions {
    if segment.flags.executable() {
        return MemoryPermissions::USER_READ_EXECUTE;
    }
    if segment.flags.writable() {
        return MemoryPermissions::USER_READ_WRITE;
    }
    MemoryPermissions::USER_READ_ONLY
}

fn page_aligned_length(length: u64) -> Result<u64, ElfLoadError> {
    let adjusted = length
        .checked_add(PAGE_SIZE - 1)
        .ok_or(ElfLoadError::AddressOverflow)?;
    Ok(adjusted & !(PAGE_SIZE - 1))
}

fn segment_contains(segment: LoadSegment<'_>, address: u64) -> bool {
    let Some(end) = segment.virtual_address.checked_add(segment.memory_size) else {
        return false;
    };
    address >= segment.virtual_address && address < end
}

fn rollback<A, const CAPACITY: usize>(
    space: &mut ProcessAddressSpace<CAPACITY>,
    allocator: &mut A,
    mapped: &[(u64, u64); CAPACITY],
    mapped_count: usize,
) where
    A: FrameDeallocator<Size4KiB>,
{
    let mut remaining = mapped_count;
    while remaining > 0 {
        remaining -= 1;
        let (start, length) = mapped[remaining];
        let _ = space.unmap_region(start, length, allocator);
    }
}
