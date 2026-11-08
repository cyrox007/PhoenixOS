//! Ограниченный диапазон существующего блочного устройства.
//!
//! Границы проверяются до передачи команд нижележащему драйверу.
//! Адаптер не выделяет память, не создаёт собственный блочный слой
//! и позволяет монтировать том без доступа к соседним секторам.

use crate::{BlockDevice, BlockError, BlockGeometry, validate_transfer};

pub struct BlockRegion<'a, D: BlockDevice + ?Sized> {
    inner: &'a mut D,
    first_block: u64,
    block_count: u64,
}

impl<'a, D: BlockDevice + ?Sized> BlockRegion<'a, D> {
    pub fn new(inner: &'a mut D, first_block: u64, block_count: u64) -> Result<Self, BlockError> {
        let geometry = inner.geometry();
        if geometry.block_size == 0 || geometry.block_count == 0 || block_count == 0 {
            return Err(BlockError::InvalidGeometry);
        }
        let end = first_block
            .checked_add(block_count)
            .ok_or(BlockError::ArithmeticOverflow)?;
        if end > geometry.block_count {
            return Err(BlockError::OutOfRange);
        }
        Ok(Self {
            inner,
            first_block,
            block_count,
        })
    }

    pub fn into_inner(self) -> &'a mut D {
        self.inner
    }

    fn translate(&self, first_block: u64, bytes: usize) -> Result<u64, BlockError> {
        validate_transfer(self.geometry(), first_block, bytes)?;
        self.first_block
            .checked_add(first_block)
            .ok_or(BlockError::ArithmeticOverflow)
    }
}

impl<D: BlockDevice + ?Sized> BlockDevice for BlockRegion<'_, D> {
    fn geometry(&self) -> BlockGeometry {
        BlockGeometry {
            block_size: self.inner.geometry().block_size,
            block_count: self.block_count,
        }
    }

    fn read_blocks(&mut self, first_block: u64, destination: &mut [u8]) -> Result<(), BlockError> {
        let absolute = self.translate(first_block, destination.len())?;
        self.inner.read_blocks(absolute, destination)
    }

    fn write_blocks(&mut self, first_block: u64, source: &[u8]) -> Result<(), BlockError> {
        let absolute = self.translate(first_block, source.len())?;
        self.inner.write_blocks(absolute, source)
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlockDevice;

    #[test]
    fn region_cannot_read_or_write_neighboring_sectors() {
        let mut disk = MemoryBlockDevice::<512, 64>::new();
        let mut guard_before = [0x19_u8; 512];
        let mut guard_after = [0x7c_u8; 512];
        disk.write_blocks(7, &guard_before).unwrap();
        disk.write_blocks(24, &guard_after).unwrap();

        {
            let mut region = BlockRegion::new(&mut disk, 8, 16).unwrap();
            assert_eq!(
                region.geometry(),
                BlockGeometry {
                    block_size: 512,
                    block_count: 16,
                }
            );
            assert_eq!(region.write_blocks(0, &[0x55; 512]), Ok(()));
            assert_eq!(region.write_blocks(15, &[0x66; 512]), Ok(()));
            assert_eq!(
                region.write_blocks(16, &[0x88; 512]),
                Err(BlockError::OutOfRange)
            );
            assert_eq!(
                region.read_blocks(15, &mut [0; 1024]),
                Err(BlockError::OutOfRange)
            );
            assert_eq!(
                region.write_blocks(0, &[0; 513]),
                Err(BlockError::InvalidBufferLength)
            );
            assert_eq!(
                region.read_blocks(u64::MAX, &mut [0; 512]),
                Err(BlockError::ArithmeticOverflow)
            );
            region.flush().unwrap();
            assert!(!region.into_inner().is_dirty());
        }

        let mut observed = [0_u8; 512];
        disk.read_blocks(8, &mut observed).unwrap();
        assert_eq!(observed, [0x55; 512]);
        disk.read_blocks(23, &mut observed).unwrap();
        assert_eq!(observed, [0x66; 512]);
        disk.read_blocks(7, &mut guard_before).unwrap();
        disk.read_blocks(24, &mut guard_after).unwrap();
        assert_eq!(guard_before, [0x19; 512]);
        assert_eq!(guard_after, [0x7c; 512]);
    }

    #[test]
    fn region_rejects_invalid_and_overflowing_bounds() {
        let mut disk = MemoryBlockDevice::<512, 64>::new();
        assert!(matches!(
            BlockRegion::new(&mut disk, 0, 0),
            Err(BlockError::InvalidGeometry)
        ));
        assert!(matches!(
            BlockRegion::new(&mut disk, 63, 2),
            Err(BlockError::OutOfRange)
        ));
        assert!(matches!(
            BlockRegion::new(&mut disk, u64::MAX, 2),
            Err(BlockError::ArithmeticOverflow)
        ));
        assert!(matches!(BlockRegion::new(&mut disk, 8, 16), Ok(_)));
    }
}
