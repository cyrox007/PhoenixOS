#![no_std]

use phoenix_block::{BlockDevice, BlockError};

pub const FILESYSTEM_BLOCK_SIZE: usize = 4096;
pub const SUPERBLOCK_COPY_COUNT: u64 = 2;
pub const FORMAT_VERSION: u32 = 1;

const SUPERBLOCK_MAGIC: [u8; 8] = *b"PHXFS\0\x01\0";
const SUPERBLOCK_HEADER_SIZE: u32 = 72;
const CHECKSUM_OFFSET: usize = 64;
const CHECKSUM_END: usize = 68;
const SUPPORTED_INCOMPATIBLE_FEATURES: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoenixFsError {
    BufferSize,
    InvalidMagic,
    UnsupportedVersion(u32),
    InvalidHeaderSize,
    InvalidFilesystemBlockSize,
    UnsupportedDeviceBlockSize,
    InvalidDeviceGeometry,
    InvalidGeneration,
    InvalidVolumeId,
    InvalidVolumeGeometry,
    UnsupportedFeatures(u64),
    InvalidReservedField,
    ChecksumMismatch,
    ArithmeticOverflow,
    ConflictingGeneration(u64),
    NoValidSuperblock,
    Device(BlockError),
}

impl From<BlockError> for PhoenixFsError {
    fn from(error: BlockError) -> Self {
        Self::Device(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    pub generation: u64,
    pub total_blocks: u64,
    pub volume_id: [u8; 16],
    pub incompatible_features: u64,
}

impl Superblock {
    pub fn new(
        generation: u64,
        total_blocks: u64,
        volume_id: [u8; 16],
    ) -> Result<Self, PhoenixFsError> {
        let superblock = Self {
            generation,
            total_blocks,
            volume_id,
            incompatible_features: 0,
        };
        superblock.validate()?;
        Ok(superblock)
    }

    pub fn encode(&self, destination: &mut [u8]) -> Result<(), PhoenixFsError> {
        self.validate()?;
        if destination.len() != FILESYSTEM_BLOCK_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }

        destination.fill(0);
        destination[..8].copy_from_slice(&SUPERBLOCK_MAGIC);
        write_u32(destination, 8, FORMAT_VERSION);
        write_u32(destination, 12, SUPERBLOCK_HEADER_SIZE);
        write_u32(destination, 16, FILESYSTEM_BLOCK_SIZE as u32);
        write_u64(destination, 24, self.generation);
        write_u64(destination, 32, self.total_blocks);
        destination[40..56].copy_from_slice(&self.volume_id);
        write_u64(destination, 56, self.incompatible_features);

        let checksum = superblock_crc32c(destination);
        write_u32(destination, CHECKSUM_OFFSET, checksum);
        Ok(())
    }

    pub fn decode(source: &[u8]) -> Result<Self, PhoenixFsError> {
        if source.len() != FILESYSTEM_BLOCK_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }
        if source[..8] != SUPERBLOCK_MAGIC {
            return Err(PhoenixFsError::InvalidMagic);
        }

        let version = read_u32(source, 8);
        if version != FORMAT_VERSION {
            return Err(PhoenixFsError::UnsupportedVersion(version));
        }
        if read_u32(source, 12) != SUPERBLOCK_HEADER_SIZE {
            return Err(PhoenixFsError::InvalidHeaderSize);
        }
        if read_u32(source, 16) != FILESYSTEM_BLOCK_SIZE as u32 {
            return Err(PhoenixFsError::InvalidFilesystemBlockSize);
        }
        if read_u32(source, 20) != 0 || read_u32(source, 68) != 0 {
            return Err(PhoenixFsError::InvalidReservedField);
        }

        let expected = read_u32(source, CHECKSUM_OFFSET);
        let actual = superblock_crc32c(source);
        if expected != actual {
            return Err(PhoenixFsError::ChecksumMismatch);
        }

        let mut volume_id = [0_u8; 16];
        volume_id.copy_from_slice(&source[40..56]);
        let superblock = Self {
            generation: read_u64(source, 24),
            total_blocks: read_u64(source, 32),
            volume_id,
            incompatible_features: read_u64(source, 56),
        };
        superblock.validate()?;
        Ok(superblock)
    }

    fn validate(&self) -> Result<(), PhoenixFsError> {
        if self.generation == 0 {
            return Err(PhoenixFsError::InvalidGeneration);
        }
        if self.total_blocks <= SUPERBLOCK_COPY_COUNT {
            return Err(PhoenixFsError::InvalidVolumeGeometry);
        }
        if self.volume_id.iter().all(|byte| *byte == 0) {
            return Err(PhoenixFsError::InvalidVolumeId);
        }

        let unsupported = self.incompatible_features & !SUPPORTED_INCOMPATIBLE_FEATURES;
        if unsupported != 0 {
            return Err(PhoenixFsError::UnsupportedFeatures(unsupported));
        }
        Ok(())
    }
}

pub fn filesystem_block_count<D: BlockDevice>(device: &D) -> Result<u64, PhoenixFsError> {
    let geometry = device.geometry();
    if geometry.block_size == 0 || geometry.block_count == 0 {
        return Err(PhoenixFsError::InvalidDeviceGeometry);
    }
    if geometry.block_size > FILESYSTEM_BLOCK_SIZE
        || !FILESYSTEM_BLOCK_SIZE.is_multiple_of(geometry.block_size)
    {
        return Err(PhoenixFsError::UnsupportedDeviceBlockSize);
    }

    let blocks_per_filesystem_block = FILESYSTEM_BLOCK_SIZE / geometry.block_size;
    let blocks_per_filesystem_block = u64::try_from(blocks_per_filesystem_block)
        .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    if !geometry
        .block_count
        .is_multiple_of(blocks_per_filesystem_block)
    {
        return Err(PhoenixFsError::InvalidDeviceGeometry);
    }

    let total = geometry.block_count / blocks_per_filesystem_block;
    if total <= SUPERBLOCK_COPY_COUNT {
        return Err(PhoenixFsError::InvalidVolumeGeometry);
    }
    Ok(total)
}

pub fn read_active_superblock<D: BlockDevice>(
    device: &mut D,
    first_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    second_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Superblock, PhoenixFsError> {
    let total_blocks = filesystem_block_count(device)?;
    let first = read_superblock_copy(device, 0, total_blocks, first_buffer);
    let second = read_superblock_copy(device, 1, total_blocks, second_buffer);

    match (first, second) {
        (Ok(left), Ok(right)) if left.generation > right.generation => Ok(left),
        (Ok(left), Ok(right)) if right.generation > left.generation => Ok(right),
        (Ok(left), Ok(right)) if left == right => Ok(left),
        (Ok(left), Ok(_)) => Err(PhoenixFsError::ConflictingGeneration(left.generation)),
        (Ok(valid), Err(_)) | (Err(_), Ok(valid)) => Ok(valid),
        (Err(_), Err(_)) => Err(PhoenixFsError::NoValidSuperblock),
    }
}

fn read_superblock_copy<D: BlockDevice>(
    device: &mut D,
    filesystem_block: u64,
    total_blocks: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Superblock, PhoenixFsError> {
    read_filesystem_block(device, filesystem_block, buffer)?;
    let superblock = Superblock::decode(buffer)?;
    if superblock.total_blocks != total_blocks {
        return Err(PhoenixFsError::InvalidVolumeGeometry);
    }
    Ok(superblock)
}

fn read_filesystem_block<D: BlockDevice>(
    device: &mut D,
    filesystem_block: u64,
    destination: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    let geometry = device.geometry();
    if geometry.block_size == 0 || !FILESYSTEM_BLOCK_SIZE.is_multiple_of(geometry.block_size) {
        return Err(PhoenixFsError::UnsupportedDeviceBlockSize);
    }

    let device_blocks = FILESYSTEM_BLOCK_SIZE / geometry.block_size;
    let device_blocks =
        u64::try_from(device_blocks).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let first_device_block = filesystem_block
        .checked_mul(device_blocks)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    device.read_blocks(first_device_block, destination)?;
    Ok(())
}

fn superblock_crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let value = if (CHECKSUM_OFFSET..CHECKSUM_END).contains(&index) {
            0
        } else {
            byte
        };
        crc ^= u32::from(value);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoenix_block::MemoryBlockDevice;

    const VOLUME_ID: [u8; 16] = *b"phoenix-volume-1";

    #[test]
    fn superblock_round_trip_preserves_identity_and_generation() {
        let superblock = Superblock::new(7, 128, VOLUME_ID).unwrap();
        let mut encoded = [0_u8; FILESYSTEM_BLOCK_SIZE];

        superblock.encode(&mut encoded).unwrap();

        assert_eq!(Superblock::decode(&encoded), Ok(superblock));
    }

    #[test]
    fn superblock_detects_corrupted_contents() {
        let superblock = Superblock::new(1, 128, VOLUME_ID).unwrap();
        let mut encoded = [0_u8; FILESYSTEM_BLOCK_SIZE];
        superblock.encode(&mut encoded).unwrap();

        encoded[100] ^= 0x80;

        assert_eq!(
            Superblock::decode(&encoded),
            Err(PhoenixFsError::ChecksumMismatch)
        );
    }

    #[test]
    fn selects_newest_valid_superblock_copy() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let older = Superblock::new(4, 8, VOLUME_ID).unwrap();
        let newer = Superblock::new(5, 8, VOLUME_ID).unwrap();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        older.encode(&mut first).unwrap();
        newer.encode(&mut second).unwrap();

        device.write_blocks(0, &first).unwrap();
        device.write_blocks(8, &second).unwrap();

        let mut first_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            read_active_superblock(&mut device, &mut first_read, &mut second_read),
            Ok(newer)
        );
    }

    #[test]
    fn falls_back_to_older_copy_when_newer_copy_is_corrupted() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let older = Superblock::new(9, 8, VOLUME_ID).unwrap();
        let newer = Superblock::new(10, 8, VOLUME_ID).unwrap();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        older.encode(&mut first).unwrap();
        newer.encode(&mut second).unwrap();
        second[80] ^= 1;

        device.write_blocks(0, &first).unwrap();
        device.write_blocks(8, &second).unwrap();

        let mut first_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            read_active_superblock(&mut device, &mut first_read, &mut second_read),
            Ok(older)
        );
    }

    #[test]
    fn rejects_equal_generation_with_different_metadata() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let first_superblock = Superblock::new(3, 8, VOLUME_ID).unwrap();
        let mut other_id = VOLUME_ID;
        other_id[0] ^= 1;
        let second_superblock = Superblock::new(3, 8, other_id).unwrap();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        first_superblock.encode(&mut first).unwrap();
        second_superblock.encode(&mut second).unwrap();

        device.write_blocks(0, &first).unwrap();
        device.write_blocks(8, &second).unwrap();

        let mut first_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            read_active_superblock(&mut device, &mut first_read, &mut second_read),
            Err(PhoenixFsError::ConflictingGeneration(3))
        );
    }

    #[test]
    fn rejects_device_block_size_that_cannot_form_four_kib_block() {
        let device = MemoryBlockDevice::<1000, 8>::new();

        assert_eq!(
            filesystem_block_count(&device),
            Err(PhoenixFsError::UnsupportedDeviceBlockSize)
        );
    }
}
