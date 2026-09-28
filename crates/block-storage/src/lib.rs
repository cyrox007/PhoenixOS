#![no_std]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockGeometry {
    pub block_size: usize,
    pub block_count: u64,
}

impl BlockGeometry {
    pub const fn byte_capacity(self) -> Option<u64> {
        self.block_count.checked_mul(self.block_size as u64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    InvalidGeometry,
    InvalidBufferLength,
    OutOfRange,
    ArithmeticOverflow,
    ReadOnly,
    BackendFailure,
}

/// Минимальная граница блочного устройства.
///
/// Файловая система работает только с открытыми блоками через этот интерфейс.
/// Драйвер, кэш и будущий слой полного шифрования могут оборачивать друг друга,
/// не меняя файловую систему.
pub trait BlockDevice {
    fn geometry(&self) -> BlockGeometry;

    fn read_blocks(
        &mut self,
        first_block: u64,
        destination: &mut [u8],
    ) -> Result<(), BlockError>;

    fn write_blocks(&mut self, first_block: u64, source: &[u8]) -> Result<(), BlockError>;

    fn flush(&mut self) -> Result<(), BlockError>;
}

pub fn validate_transfer(
    geometry: BlockGeometry,
    first_block: u64,
    byte_length: usize,
) -> Result<u64, BlockError> {
    if geometry.block_size == 0 || geometry.block_count == 0 {
        return Err(BlockError::InvalidGeometry);
    }
    if byte_length == 0 || !byte_length.is_multiple_of(geometry.block_size) {
        return Err(BlockError::InvalidBufferLength);
    }

    let blocks = byte_length / geometry.block_size;
    let blocks = u64::try_from(blocks).map_err(|_| BlockError::ArithmeticOverflow)?;
    let end = first_block
        .checked_add(blocks)
        .ok_or(BlockError::ArithmeticOverflow)?;
    if end > geometry.block_count {
        return Err(BlockError::OutOfRange);
    }
    Ok(blocks)
}

/// Раннее устройство памяти для модульных и файловых проверок.
///
/// Размер блока и число блоков задаются типом, поэтому реализация не требует
/// распределителя памяти и пригодна для раннего `no_std`-этапа.
pub struct MemoryBlockDevice<const BLOCK_SIZE: usize, const BLOCK_COUNT: usize> {
    bytes: [[u8; BLOCK_SIZE]; BLOCK_COUNT],
    dirty: bool,
}

impl<const BLOCK_SIZE: usize, const BLOCK_COUNT: usize>
    MemoryBlockDevice<BLOCK_SIZE, BLOCK_COUNT>
{
    pub const fn new() -> Self {
        Self {
            bytes: [[0; BLOCK_SIZE]; BLOCK_COUNT],
            dirty: false,
        }
    }

    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn checked_range(
        &self,
        first_block: u64,
        byte_length: usize,
    ) -> Result<(usize, usize), BlockError> {
        let geometry = self.geometry();
        let blocks = validate_transfer(geometry, first_block, byte_length)?;
        let first = usize::try_from(first_block).map_err(|_| BlockError::ArithmeticOverflow)?;
        let blocks = usize::try_from(blocks).map_err(|_| BlockError::ArithmeticOverflow)?;
        let end = first
            .checked_add(blocks)
            .ok_or(BlockError::ArithmeticOverflow)?;
        Ok((first, end))
    }
}

impl<const BLOCK_SIZE: usize, const BLOCK_COUNT: usize> Default
    for MemoryBlockDevice<BLOCK_SIZE, BLOCK_COUNT>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<const BLOCK_SIZE: usize, const BLOCK_COUNT: usize> BlockDevice
    for MemoryBlockDevice<BLOCK_SIZE, BLOCK_COUNT>
{
    fn geometry(&self) -> BlockGeometry {
        BlockGeometry {
            block_size: BLOCK_SIZE,
            block_count: BLOCK_COUNT as u64,
        }
    }

    fn read_blocks(
        &mut self,
        first_block: u64,
        destination: &mut [u8],
    ) -> Result<(), BlockError> {
        let (first, end) = self.checked_range(first_block, destination.len())?;
        for (chunk, block) in destination
            .chunks_exact_mut(BLOCK_SIZE)
            .zip(&self.bytes[first..end])
        {
            chunk.copy_from_slice(block);
        }
        Ok(())
    }

    fn write_blocks(&mut self, first_block: u64, source: &[u8]) -> Result<(), BlockError> {
        let (first, end) = self.checked_range(first_block, source.len())?;
        for (block, chunk) in self.bytes[first..end]
            .iter_mut()
            .zip(source.chunks_exact(BLOCK_SIZE))
        {
            block.copy_from_slice(chunk);
        }
        self.dirty = true;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_device_round_trips_multiple_blocks() {
        let mut device = MemoryBlockDevice::<8, 4>::new();
        let source = *b"phoenix-block-io";
        device.write_blocks(1, &source).unwrap();
        assert!(device.is_dirty());

        let mut output = [0_u8; 16];
        device.read_blocks(1, &mut output).unwrap();
        assert_eq!(output, source);

        device.flush().unwrap();
        assert!(!device.is_dirty());
    }

    #[test]
    fn transfer_rejects_partial_and_out_of_range_access() {
        let mut device = MemoryBlockDevice::<8, 2>::new();
        assert_eq!(
            device.write_blocks(0, b"partial"),
            Err(BlockError::InvalidBufferLength)
        );
        assert_eq!(
            device.write_blocks(1, &[0_u8; 16]),
            Err(BlockError::OutOfRange)
        );
    }

    #[test]
    fn geometry_capacity_is_checked() {
        assert_eq!(
            BlockGeometry {
                block_size: 512,
                block_count: 8,
            }
            .byte_capacity(),
            Some(4096)
        );
        assert_eq!(
            BlockGeometry {
                block_size: usize::MAX,
                block_count: u64::MAX,
            }
            .byte_capacity(),
            None
        );
    }
}
