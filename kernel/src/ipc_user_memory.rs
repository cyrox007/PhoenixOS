use phoenix_syscall_abi::{IpcReceiveArguments, IpcReceiveMetadata, IpcSendArguments};
use phoenix_vm::{UserBuffer, UserBufferError};
use x86_64::VirtAddr;

use crate::process_space::{ProcessAddressSpace, ProcessAddressSpaceError};

pub const IPC_INLINE_WORD_CAPACITY: usize = 6;
const WORD_SIZE: u64 = size_of::<u64>() as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcUserMemoryError {
    Buffer(UserBufferError),
    AddressSpace(ProcessAddressSpaceError),
    TooManyWords,
    ReceiveCapacityTooSmall,
    UnmappedUserWord,
}

impl From<UserBufferError> for IpcUserMemoryError {
    fn from(error: UserBufferError) -> Self {
        Self::Buffer(error)
    }
}

impl From<ProcessAddressSpaceError> for IpcUserMemoryError {
    fn from(error: ProcessAddressSpaceError) -> Self {
        Self::AddressSpace(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopiedIpcWords {
    words: [u64; IPC_INLINE_WORD_CAPACITY],
    len: usize,
}

impl CopiedIpcWords {
    pub const fn words(&self) -> &[u64; IPC_INLINE_WORD_CAPACITY] {
        &self.words
    }

    pub fn as_slice(&self) -> &[u64] {
        &self.words[..self.len]
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

pub fn copy_send_words<const CAPACITY: usize>(
    space: &mut ProcessAddressSpace<CAPACITY>,
    arguments: IpcSendArguments,
) -> Result<CopiedIpcWords, IpcUserMemoryError> {
    if arguments.word_count > IPC_INLINE_WORD_CAPACITY as u64 {
        return Err(IpcUserMemoryError::TooManyWords);
    }

    let buffer = UserBuffer::for_array(
        arguments.words_address,
        arguments.word_count,
        WORD_SIZE,
        WORD_SIZE,
    )?;
    let len = arguments.word_count as usize;
    preflight_words(space, buffer.start(), len)?;

    let mut words = [0_u64; IPC_INLINE_WORD_CAPACITY];
    for (index, word) in words[..len].iter_mut().enumerate() {
        let address = word_address(buffer.start(), index)?;
        let mut bytes = [0_u8; size_of::<u64>()];
        space.read_user_bytes(VirtAddr::new(address), &mut bytes)?;
        *word = u64::from_le_bytes(bytes);
    }

    Ok(CopiedIpcWords { words, len })
}

pub fn copy_receive_words<const CAPACITY: usize>(
    space: &mut ProcessAddressSpace<CAPACITY>,
    arguments: IpcReceiveArguments,
    words: &[u64],
) -> Result<(), IpcUserMemoryError> {
    if words.len() > IPC_INLINE_WORD_CAPACITY {
        return Err(IpcUserMemoryError::TooManyWords);
    }
    if words.len() as u64 > arguments.word_capacity {
        return Err(IpcUserMemoryError::ReceiveCapacityTooSmall);
    }

    let buffer = UserBuffer::for_array(
        arguments.words_address,
        arguments.word_capacity,
        WORD_SIZE,
        WORD_SIZE,
    )?;
    preflight_words(space, buffer.start(), words.len())?;

    for (index, word) in words.iter().enumerate() {
        let address = word_address(buffer.start(), index)?;
        space.write_user_bytes(VirtAddr::new(address), &word.to_le_bytes())?;
    }

    Ok(())
}

pub fn copy_receive_message<const CAPACITY: usize>(
    space: &mut ProcessAddressSpace<CAPACITY>,
    arguments: IpcReceiveArguments,
    words: &[u64],
    metadata: IpcReceiveMetadata,
) -> Result<(), IpcUserMemoryError> {
    if words.len() > IPC_INLINE_WORD_CAPACITY {
        return Err(IpcUserMemoryError::TooManyWords);
    }
    if words.len() as u64 > arguments.word_capacity {
        return Err(IpcUserMemoryError::ReceiveCapacityTooSmall);
    }

    let word_buffer = UserBuffer::for_array(
        arguments.words_address,
        arguments.word_capacity,
        WORD_SIZE,
        WORD_SIZE,
    )?;
    let metadata_buffer =
        UserBuffer::for_array(arguments.metadata_address, 3, WORD_SIZE, WORD_SIZE)?;
    preflight_words(space, word_buffer.start(), words.len())?;
    preflight_words(space, metadata_buffer.start(), 3)?;

    for (index, word) in words.iter().enumerate() {
        let address = word_address(word_buffer.start(), index)?;
        space.write_user_bytes(VirtAddr::new(address), &word.to_le_bytes())?;
    }
    for (index, word) in metadata.words().iter().enumerate() {
        let address = word_address(metadata_buffer.start(), index)?;
        space.write_user_bytes(VirtAddr::new(address), &word.to_le_bytes())?;
    }

    Ok(())
}

fn preflight_words<const CAPACITY: usize>(
    space: &mut ProcessAddressSpace<CAPACITY>,
    start: u64,
    count: usize,
) -> Result<(), IpcUserMemoryError> {
    for index in 0..count {
        let address = word_address(start, index)?;
        if space.translate_addr(VirtAddr::new(address)).is_none() {
            return Err(IpcUserMemoryError::UnmappedUserWord);
        }
    }
    Ok(())
}

fn word_address(start: u64, index: usize) -> Result<u64, IpcUserMemoryError> {
    let offset = (index as u64)
        .checked_mul(WORD_SIZE)
        .ok_or(IpcUserMemoryError::TooManyWords)?;
    start
        .checked_add(offset)
        .ok_or(IpcUserMemoryError::TooManyWords)
}
