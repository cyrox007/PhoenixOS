#![no_std]

use phoenix_block::{BlockDevice, BlockError};

pub const MIN_BOOT_SECTOR_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatKind {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatError {
    SectorTooShort,
    MissingBootSignature,
    InvalidBytesPerSector,
    InvalidSectorsPerCluster,
    InvalidReservedSectors,
    InvalidFatCount,
    MissingTotalSectors,
    MissingFatSize,
    InvalidGeometry,
    InvalidFat32RootCluster,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatBootSector {
    pub kind: FatKind,
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub fat_count: u8,
    pub root_entry_count: u16,
    pub total_sectors: u32,
    pub fat_size_sectors: u32,
    pub first_data_sector: u32,
    pub data_cluster_count: u32,
    pub root_cluster: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatEntry {
    Free,
    Data(u32),
    Reserved(u32),
    Bad,
    EndOfChain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatReadError {
    UnsupportedFat12,
    BufferSize,
    ClusterOutOfRange,
    ArithmeticOverflow,
    Device(BlockError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatChainError {
    Read(FatReadError),
    ChainBufferFull,
    LoopDetected(u32),
    UnexpectedFree(u32),
    BadCluster(u32),
    ReservedEntry(u32),
}

impl From<FatReadError> for FatChainError {
    fn from(error: FatReadError) -> Self {
        Self::Read(error)
    }
}

impl From<BlockError> for FatReadError {
    fn from(error: BlockError) -> Self {
        Self::Device(error)
    }
}

pub struct FatTableReader<'a, D: BlockDevice> {
    device: &'a mut D,
    boot: FatBootSector,
}

impl<'a, D: BlockDevice> FatTableReader<'a, D> {
    pub fn new(device: &'a mut D, boot: FatBootSector) -> Result<Self, FatReadError> {
        if device.geometry().block_size != usize::from(boot.bytes_per_sector) {
            return Err(FatReadError::BufferSize);
        }
        Ok(Self { device, boot })
    }

    pub fn read_entry(
        &mut self,
        cluster: u32,
        sector_buffer: &mut [u8],
    ) -> Result<FatEntry, FatReadError> {
        self.validate_cluster(cluster)?;
        let entry_size = self.entry_size()?;
        let bytes_per_sector = u64::from(self.boot.bytes_per_sector);
        if sector_buffer.len() != bytes_per_sector as usize {
            return Err(FatReadError::BufferSize);
        }

        let entry_offset = u64::from(cluster)
            .checked_mul(entry_size)
            .ok_or(FatReadError::ArithmeticOverflow)?;
        let fat_sector = u64::from(self.boot.reserved_sectors)
            .checked_add(entry_offset / bytes_per_sector)
            .ok_or(FatReadError::ArithmeticOverflow)?;
        let offset = usize::try_from(entry_offset % bytes_per_sector)
            .map_err(|_| FatReadError::ArithmeticOverflow)?;

        self.device.read_blocks(fat_sector, sector_buffer)?;
        let raw = match self.boot.kind {
            FatKind::Fat12 => return Err(FatReadError::UnsupportedFat12),
            FatKind::Fat16 => u32::from(read_u16(sector_buffer, offset)),
            FatKind::Fat32 => read_u32(sector_buffer, offset) & 0x0fff_ffff,
        };

        Ok(classify_entry(self.boot.kind, raw))
    }

    pub fn read_chain(
        &mut self,
        start_cluster: u32,
        chain: &mut [u32],
        sector_buffer: &mut [u8],
    ) -> Result<usize, FatChainError> {
        let mut current = start_cluster;
        let mut count = 0;

        loop {
            if chain[..count].contains(&current) {
                return Err(FatChainError::LoopDetected(current));
            }
            if count == chain.len() {
                return Err(FatChainError::ChainBufferFull);
            }

            chain[count] = current;
            count += 1;

            match self.read_entry(current, sector_buffer)? {
                FatEntry::Data(next) => current = next,
                FatEntry::EndOfChain => return Ok(count),
                FatEntry::Free => return Err(FatChainError::UnexpectedFree(current)),
                FatEntry::Bad => return Err(FatChainError::BadCluster(current)),
                FatEntry::Reserved(value) => return Err(FatChainError::ReservedEntry(value)),
            }
        }
    }

    fn validate_cluster(&self, cluster: u32) -> Result<(), FatReadError> {
        if cluster < 2 {
            return Err(FatReadError::ClusterOutOfRange);
        }
        let Some(last_cluster) = self.boot.data_cluster_count.checked_add(1) else {
            return Err(FatReadError::ArithmeticOverflow);
        };
        if cluster > last_cluster {
            return Err(FatReadError::ClusterOutOfRange);
        }
        Ok(())
    }

    const fn entry_size(&self) -> Result<u64, FatReadError> {
        match self.boot.kind {
            FatKind::Fat12 => Err(FatReadError::UnsupportedFat12),
            FatKind::Fat16 => Ok(2),
            FatKind::Fat32 => Ok(4),
        }
    }
}

impl FatBootSector {
    pub fn parse(sector: &[u8]) -> Result<Self, FatError> {
        if sector.len() < MIN_BOOT_SECTOR_SIZE {
            return Err(FatError::SectorTooShort);
        }
        if sector[510] != 0x55 || sector[511] != 0xaa {
            return Err(FatError::MissingBootSignature);
        }

        let bytes_per_sector = read_u16(sector, 11);
        if !valid_bytes_per_sector(bytes_per_sector) {
            return Err(FatError::InvalidBytesPerSector);
        }

        let sectors_per_cluster = sector[13];
        if !valid_sectors_per_cluster(sectors_per_cluster) {
            return Err(FatError::InvalidSectorsPerCluster);
        }

        let reserved_sectors = read_u16(sector, 14);
        if reserved_sectors == 0 {
            return Err(FatError::InvalidReservedSectors);
        }

        let fat_count = sector[16];
        if fat_count == 0 {
            return Err(FatError::InvalidFatCount);
        }

        let root_entry_count = read_u16(sector, 17);
        let total_sectors_16 = u32::from(read_u16(sector, 19));
        let total_sectors_32 = read_u32(sector, 32);
        let total_sectors = if total_sectors_16 != 0 {
            total_sectors_16
        } else {
            total_sectors_32
        };
        if total_sectors == 0 {
            return Err(FatError::MissingTotalSectors);
        }

        let fat_size_16 = u32::from(read_u16(sector, 22));
        let fat_size_32 = read_u32(sector, 36);
        let fat_size_sectors = if fat_size_16 != 0 {
            fat_size_16
        } else {
            fat_size_32
        };
        if fat_size_sectors == 0 {
            return Err(FatError::MissingFatSize);
        }

        let root_dir_bytes = u64::from(root_entry_count) * 32;
        let bytes_per_sector_u64 = u64::from(bytes_per_sector);
        let root_dir_sectors = root_dir_bytes
            .checked_add(bytes_per_sector_u64 - 1)
            .ok_or(FatError::InvalidGeometry)?
            / bytes_per_sector_u64;

        let fat_area = u64::from(fat_count)
            .checked_mul(u64::from(fat_size_sectors))
            .ok_or(FatError::InvalidGeometry)?;
        let first_data_sector = u64::from(reserved_sectors)
            .checked_add(fat_area)
            .and_then(|value| value.checked_add(root_dir_sectors))
            .ok_or(FatError::InvalidGeometry)?;
        if first_data_sector >= u64::from(total_sectors) {
            return Err(FatError::InvalidGeometry);
        }

        let data_sectors = u64::from(total_sectors) - first_data_sector;
        let data_cluster_count = data_sectors / u64::from(sectors_per_cluster);
        let kind = classify_fat(data_cluster_count);

        validate_variant_fields(kind, root_entry_count, fat_size_16, sector)?;

        let first_data_sector =
            u32::try_from(first_data_sector).map_err(|_| FatError::InvalidGeometry)?;
        let data_cluster_count =
            u32::try_from(data_cluster_count).map_err(|_| FatError::InvalidGeometry)?;
        let root_cluster = if matches!(kind, FatKind::Fat32) {
            Some(read_u32(sector, 44))
        } else {
            None
        };

        Ok(Self {
            kind,
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            fat_count,
            root_entry_count,
            total_sectors,
            fat_size_sectors,
            first_data_sector,
            data_cluster_count,
            root_cluster,
        })
    }
}

const fn valid_bytes_per_sector(value: u16) -> bool {
    matches!(value, 512 | 1024 | 2048 | 4096)
}

const fn valid_sectors_per_cluster(value: u8) -> bool {
    value != 0 && value <= 128 && value.is_power_of_two()
}

const fn classify_fat(cluster_count: u64) -> FatKind {
    if cluster_count < 4_085 {
        return FatKind::Fat12;
    }
    if cluster_count < 65_525 {
        return FatKind::Fat16;
    }
    FatKind::Fat32
}

const fn classify_entry(kind: FatKind, raw: u32) -> FatEntry {
    match kind {
        FatKind::Fat12 => FatEntry::Reserved(raw),
        FatKind::Fat16 => classify_fat16_entry(raw),
        FatKind::Fat32 => classify_fat32_entry(raw),
    }
}

const fn classify_fat16_entry(raw: u32) -> FatEntry {
    match raw {
        0 => FatEntry::Free,
        0xfff7 => FatEntry::Bad,
        0xfff8..=0xffff => FatEntry::EndOfChain,
        0xfff0..=0xfff6 | 1 => FatEntry::Reserved(raw),
        value => FatEntry::Data(value),
    }
}

const fn classify_fat32_entry(raw: u32) -> FatEntry {
    match raw {
        0 => FatEntry::Free,
        0x0fff_fff7 => FatEntry::Bad,
        0x0fff_fff8..=0x0fff_ffff => FatEntry::EndOfChain,
        0x0fff_fff0..=0x0fff_fff6 | 1 => FatEntry::Reserved(raw),
        value => FatEntry::Data(value),
    }
}

fn validate_variant_fields(
    kind: FatKind,
    root_entry_count: u16,
    fat_size_16: u32,
    sector: &[u8],
) -> Result<(), FatError> {
    if matches!(kind, FatKind::Fat32) {
        if root_entry_count != 0 || fat_size_16 != 0 {
            return Err(FatError::InvalidGeometry);
        }
        if read_u32(sector, 44) < 2 {
            return Err(FatError::InvalidFat32RootCluster);
        }
        return Ok(());
    }

    if root_entry_count == 0 || fat_size_16 == 0 {
        return Err(FatError::InvalidGeometry);
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_sector() -> [u8; MIN_BOOT_SECTOR_SIZE] {
        let mut sector = [0_u8; MIN_BOOT_SECTOR_SIZE];
        sector[510] = 0x55;
        sector[511] = 0xaa;
        sector[11..13].copy_from_slice(&512_u16.to_le_bytes());
        sector[13] = 1;
        sector[14..16].copy_from_slice(&1_u16.to_le_bytes());
        sector[16] = 2;
        sector
    }

    fn fat16_boot() -> FatBootSector {
        FatBootSector {
            kind: FatKind::Fat16,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 2,
            root_entry_count: 512,
            total_sectors: 10_000,
            fat_size_sectors: 20,
            first_data_sector: 73,
            data_cluster_count: 9_927,
            root_cluster: None,
        }
    }

    fn fat32_boot() -> FatBootSector {
        FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 8,
            reserved_sectors: 1,
            fat_count: 2,
            root_entry_count: 0,
            total_sectors: 1_000_000,
            fat_size_sectors: 1_000,
            first_data_sector: 2_001,
            data_cluster_count: 124_749,
            root_cluster: Some(2),
        }
    }

    #[test]
    fn reads_cluster_chain_without_allocation() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut fat_sector = [0_u8; 512];
        fat_sector[4..6].copy_from_slice(&3_u16.to_le_bytes());
        fat_sector[6..8].copy_from_slice(&4_u16.to_le_bytes());
        fat_sector[8..10].copy_from_slice(&0xfff8_u16.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 4];
        let length = reader
            .read_chain(2, &mut chain, &mut sector_buffer)
            .unwrap();

        assert_eq!(length, 3);
        assert_eq!(&chain[..length], &[2, 3, 4]);
    }

    #[test]
    fn cluster_chain_detects_loop_and_capacity_limit() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut fat_sector = [0_u8; 512];
        fat_sector[4..6].copy_from_slice(&3_u16.to_le_bytes());
        fat_sector[6..8].copy_from_slice(&2_u16.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 4];
        assert_eq!(
            reader.read_chain(2, &mut chain, &mut sector_buffer),
            Err(FatChainError::LoopDetected(2))
        );

        let mut short_chain = [0_u32; 1];
        assert_eq!(
            reader.read_chain(2, &mut short_chain, &mut sector_buffer),
            Err(FatChainError::ChainBufferFull)
        );
    }

    #[test]
    fn reads_fat16_end_of_chain_from_block_device() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut fat_sector = [0_u8; 512];
        fat_sector[4..6].copy_from_slice(&0xfff8_u16.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut buffer = [0_u8; 512];
        assert_eq!(reader.read_entry(2, &mut buffer), Ok(FatEntry::EndOfChain));
    }

    #[test]
    fn reads_fat32_data_cluster_and_masks_high_bits() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut fat_sector = [0_u8; 512];
        fat_sector[8..12].copy_from_slice(&0xf000_0005_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat32_boot()).unwrap();
        let mut buffer = [0_u8; 512];
        assert_eq!(reader.read_entry(2, &mut buffer), Ok(FatEntry::Data(5)));
    }

    #[test]
    fn table_reader_rejects_invalid_cluster_and_buffer_size() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut buffer = [0_u8; 512];

        assert_eq!(
            reader.read_entry(1, &mut buffer),
            Err(FatReadError::ClusterOutOfRange)
        );

        let mut short = [0_u8; 128];
        assert_eq!(
            reader.read_entry(2, &mut short),
            Err(FatReadError::BufferSize)
        );
    }

    #[test]
    fn parses_fat16_geometry() {
        let mut sector = base_sector();
        sector[17..19].copy_from_slice(&512_u16.to_le_bytes());
        sector[19..21].copy_from_slice(&10_000_u16.to_le_bytes());
        sector[22..24].copy_from_slice(&20_u16.to_le_bytes());

        let parsed = FatBootSector::parse(&sector).unwrap();
        assert_eq!(parsed.kind, FatKind::Fat16);
        assert_eq!(parsed.first_data_sector, 73);
        assert_eq!(parsed.data_cluster_count, 9_927);
        assert_eq!(parsed.root_cluster, None);
    }

    #[test]
    fn parses_fat32_geometry_and_root_cluster() {
        let mut sector = base_sector();
        sector[13] = 8;
        sector[14..16].copy_from_slice(&32_u16.to_le_bytes());
        sector[17..19].copy_from_slice(&0_u16.to_le_bytes());
        sector[19..21].copy_from_slice(&0_u16.to_le_bytes());
        sector[22..24].copy_from_slice(&0_u16.to_le_bytes());
        sector[32..36].copy_from_slice(&1_000_000_u32.to_le_bytes());
        sector[36..40].copy_from_slice(&1_000_u32.to_le_bytes());
        sector[44..48].copy_from_slice(&2_u32.to_le_bytes());

        let parsed = FatBootSector::parse(&sector).unwrap();
        assert_eq!(parsed.kind, FatKind::Fat32);
        assert_eq!(parsed.first_data_sector, 2_032);
        assert_eq!(parsed.data_cluster_count, 124_746);
        assert_eq!(parsed.root_cluster, Some(2));
    }

    #[test]
    fn rejects_missing_signature_and_invalid_geometry() {
        let mut sector = base_sector();
        sector[510] = 0;
        assert_eq!(
            FatBootSector::parse(&sector),
            Err(FatError::MissingBootSignature)
        );

        let mut sector = base_sector();
        sector[17..19].copy_from_slice(&512_u16.to_le_bytes());
        sector[19..21].copy_from_slice(&20_u16.to_le_bytes());
        sector[22..24].copy_from_slice(&20_u16.to_le_bytes());
        assert_eq!(
            FatBootSector::parse(&sector),
            Err(FatError::InvalidGeometry)
        );
    }

    #[test]
    fn rejects_invalid_fat32_root_cluster() {
        let mut sector = base_sector();
        sector[13] = 8;
        sector[14..16].copy_from_slice(&32_u16.to_le_bytes());
        sector[32..36].copy_from_slice(&1_000_000_u32.to_le_bytes());
        sector[36..40].copy_from_slice(&1_000_u32.to_le_bytes());
        sector[44..48].copy_from_slice(&1_u32.to_le_bytes());

        assert_eq!(
            FatBootSector::parse(&sector),
            Err(FatError::InvalidFat32RootCluster)
        );
    }
}
