use core::str;

use phoenix_vm::{UserBuffer, UserBufferError};
use x86_64::VirtAddr;

use crate::process_space::{ProcessAddressSpace, ProcessAddressSpaceError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileUserMemoryError {
    Buffer(UserBufferError),
    AddressSpace(ProcessAddressSpaceError),
    BufferTooSmall,
    InvalidPathEncoding,
    UnmappedUserBuffer,
}

impl From<UserBufferError> for FileUserMemoryError {
    fn from(error: UserBufferError) -> Self {
        Self::Buffer(error)
    }
}

impl From<ProcessAddressSpaceError> for FileUserMemoryError {
    fn from(error: ProcessAddressSpaceError) -> Self {
        Self::AddressSpace(error)
    }
}

/// Copies an explicitly sized path without dereferencing a user pointer in the kernel.
pub fn copy_path<'a, const REGIONS: usize>(
    space: &mut ProcessAddressSpace<REGIONS>,
    address: u64,
    length: u64,
    destination: &'a mut [u8],
) -> Result<&'a str, FileUserMemoryError> {
    let bytes = copy_from_user(space, address, length, destination)?;
    str::from_utf8(bytes).map_err(|_| FileUserMemoryError::InvalidPathEncoding)
}

pub fn copy_from_user<'a, const REGIONS: usize>(
    space: &mut ProcessAddressSpace<REGIONS>,
    address: u64,
    length: u64,
    destination: &'a mut [u8],
) -> Result<&'a [u8], FileUserMemoryError> {
    let buffer = UserBuffer::for_array(address, length, 1, 1)?;
    let length = usize::try_from(length).map_err(|_| FileUserMemoryError::BufferTooSmall)?;
    if length > destination.len() {
        return Err(FileUserMemoryError::BufferTooSmall);
    }
    preflight(space, buffer.start(), length)?;
    space.read_user_bytes(VirtAddr::new(buffer.start()), &mut destination[..length])?;
    Ok(&destination[..length])
}

/// Preflights the complete destination before exposing any write to user space.
pub fn copy_to_user<const REGIONS: usize>(
    space: &mut ProcessAddressSpace<REGIONS>,
    address: u64,
    capacity: u64,
    source: &[u8],
) -> Result<(), FileUserMemoryError> {
    if source.len() as u64 > capacity {
        return Err(FileUserMemoryError::BufferTooSmall);
    }
    let buffer = UserBuffer::for_array(address, capacity, 1, 1)?;
    preflight(space, buffer.start(), source.len())?;
    space.write_user_bytes(VirtAddr::new(buffer.start()), source)?;
    Ok(())
}

fn preflight<const REGIONS: usize>(
    space: &mut ProcessAddressSpace<REGIONS>,
    start: u64,
    length: usize,
) -> Result<(), FileUserMemoryError> {
    if length == 0 {
        return Ok(());
    }

    let end = start
        .checked_add(length as u64 - 1)
        .ok_or(FileUserMemoryError::BufferTooSmall)?;
    let mut address = start;
    loop {
        if space.translate_addr(VirtAddr::new(address)).is_none() {
            return Err(FileUserMemoryError::UnmappedUserBuffer);
        }
        if address == end {
            break;
        }
        let next_page = (address | (phoenix_process::PAGE_SIZE - 1))
            .checked_add(1)
            .ok_or(FileUserMemoryError::BufferTooSmall)?;
        address = next_page.min(end);
    }
    Ok(())
}
