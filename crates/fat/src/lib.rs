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

pub const DIRECTORY_ENTRY_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatDirectory {
    Root,
    Cluster(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatDirectoryEntry {
    pub short_name: [u8; 11],
    pub attributes: u8,
    pub first_cluster: u32,
    pub file_size: u32,
}

impl FatDirectoryEntry {
    pub const fn is_directory(self) -> bool {
        self.attributes & 0x10 != 0
    }

    pub const fn is_volume_label(self) -> bool {
        self.attributes & 0x08 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatDirectoryError {
    Read(FatReadError),
    Chain(FatChainError),
    MissingRootCluster,
}

impl From<FatReadError> for FatDirectoryError {
    fn from(error: FatReadError) -> Self {
        Self::Read(error)
    }
}

impl From<FatChainError> for FatDirectoryError {
    fn from(error: FatChainError) -> Self {
        Self::Chain(error)
    }
}

impl From<BlockError> for FatDirectoryError {
    fn from(error: BlockError) -> Self {
        Self::Read(FatReadError::Device(error))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatFileError {
    Read(FatReadError),
    Chain(FatChainError),
    NotRegularFile,
    DestinationTooSmall,
    FileTooLarge,
    MissingFirstCluster,
    ChainTooShort,
}

impl From<FatReadError> for FatFileError {
    fn from(error: FatReadError) -> Self {
        Self::Read(error)
    }
}

impl From<FatChainError> for FatFileError {
    fn from(error: FatChainError) -> Self {
        Self::Chain(error)
    }
}

impl From<BlockError> for FatFileError {
    fn from(error: BlockError) -> Self {
        Self::Read(FatReadError::Device(error))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedDirectorySlot {
    End,
    Skip,
    Entry(FatDirectoryEntry),
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

    pub fn read_file(
        &mut self,
        entry: FatDirectoryEntry,
        chain: &mut [u32],
        sector_buffer: &mut [u8],
        destination: &mut [u8],
    ) -> Result<usize, FatFileError> {
        self.validate_sector_buffer(sector_buffer)?;

        if entry.is_directory() || entry.is_volume_label() {
            return Err(FatFileError::NotRegularFile);
        }

        let file_size =
            usize::try_from(entry.file_size).map_err(|_| FatFileError::FileTooLarge)?;
        if destination.len() < file_size {
            return Err(FatFileError::DestinationTooSmall);
        }
        if file_size == 0 {
            return Ok(0);
        }
        if entry.first_cluster < 2 {
            return Err(FatFileError::MissingFirstCluster);
        }

        let chain_length = self.read_chain(entry.first_cluster, chain, sector_buffer)?;
        let sectors_per_cluster = u64::from(self.boot.sectors_per_cluster);
        let mut written = 0_usize;

        for cluster in &chain[..chain_length] {
            let first_sector = self.cluster_first_sector(*cluster)?;
            for offset in 0..sectors_per_cluster {
                if written == file_size {
                    return Ok(written);
                }

                let sector = first_sector
                    .checked_add(offset)
                    .ok_or(FatReadError::ArithmeticOverflow)?;
                self.device.read_blocks(sector, sector_buffer)?;

                let remaining = file_size - written;
                let copied = remaining.min(sector_buffer.len());
                destination[written..written + copied].copy_from_slice(&sector_buffer[..copied]);
                written += copied;
            }
        }

        if written != file_size {
            return Err(FatFileError::ChainTooShort);
        }
        Ok(written)
    }

    pub fn read_directory<F>(
        &mut self,
        directory: FatDirectory,
        chain: &mut [u32],
        sector_buffer: &mut [u8],
        mut visitor: F,
    ) -> Result<(), FatDirectoryError>
    where
        F: FnMut(FatDirectoryEntry) -> bool,
    {
        self.validate_sector_buffer(sector_buffer)?;

        if matches!(self.boot.kind, FatKind::Fat12) {
            return Err(FatReadError::UnsupportedFat12.into());
        }

        match directory {
            FatDirectory::Root if matches!(self.boot.kind, FatKind::Fat16) => {
                self.read_fat16_root_directory(sector_buffer, &mut visitor)
            }
            FatDirectory::Root => {
                let start_cluster = self
                    .boot
                    .root_cluster
                    .ok_or(FatDirectoryError::MissingRootCluster)?;
                self.read_cluster_directory(start_cluster, chain, sector_buffer, &mut visitor)
            }
            FatDirectory::Cluster(start_cluster) => {
                self.read_cluster_directory(start_cluster, chain, sector_buffer, &mut visitor)
            }
        }
    }

    fn read_fat16_root_directory<F>(
        &mut self,
        sector_buffer: &mut [u8],
        visitor: &mut F,
    ) -> Result<(), FatDirectoryError>
    where
        F: FnMut(FatDirectoryEntry) -> bool,
    {
        let fat_area = u64::from(self.boot.fat_count)
            .checked_mul(u64::from(self.boot.fat_size_sectors))
            .ok_or(FatReadError::ArithmeticOverflow)?;
        let first_sector = u64::from(self.boot.reserved_sectors)
            .checked_add(fat_area)
            .ok_or(FatReadError::ArithmeticOverflow)?;
        let root_bytes = u64::from(self.boot.root_entry_count)
            .checked_mul(DIRECTORY_ENTRY_SIZE as u64)
            .ok_or(FatReadError::ArithmeticOverflow)?;
        let bytes_per_sector = u64::from(self.boot.bytes_per_sector);
        let sector_count = root_bytes
            .checked_add(bytes_per_sector - 1)
            .ok_or(FatReadError::ArithmeticOverflow)?
            / bytes_per_sector;

        for offset in 0..sector_count {
            let sector = first_sector
                .checked_add(offset)
                .ok_or(FatReadError::ArithmeticOverflow)?;
            self.device.read_blocks(sector, sector_buffer)?;
            if visit_directory_sector(self.boot.kind, sector_buffer, visitor) {
                return Ok(());
            }
        }

        Ok(())
    }

    fn read_cluster_directory<F>(
        &mut self,
        start_cluster: u32,
        chain: &mut [u32],
        sector_buffer: &mut [u8],
        visitor: &mut F,
    ) -> Result<(), FatDirectoryError>
    where
        F: FnMut(FatDirectoryEntry) -> bool,
    {
        let chain_length = self.read_chain(start_cluster, chain, sector_buffer)?;
        let sectors_per_cluster = u64::from(self.boot.sectors_per_cluster);

        for cluster in &chain[..chain_length] {
            let first_sector = self.cluster_first_sector(*cluster)?;
            for offset in 0..sectors_per_cluster {
                let sector = first_sector
                    .checked_add(offset)
                    .ok_or(FatReadError::ArithmeticOverflow)?;
                self.device.read_blocks(sector, sector_buffer)?;
                if visit_directory_sector(self.boot.kind, sector_buffer, visitor) {
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    fn cluster_first_sector(&self, cluster: u32) -> Result<u64, FatReadError> {
        self.validate_cluster(cluster)?;
        let cluster_index = u64::from(cluster - 2);
        let sector_offset = cluster_index
            .checked_mul(u64::from(self.boot.sectors_per_cluster))
            .ok_or(FatReadError::ArithmeticOverflow)?;
        u64::from(self.boot.first_data_sector)
            .checked_add(sector_offset)
            .ok_or(FatReadError::ArithmeticOverflow)
    }

    fn validate_sector_buffer(&self, sector_buffer: &[u8]) -> Result<(), FatReadError> {
        if sector_buffer.len() != usize::from(self.boot.bytes_per_sector) {
            return Err(FatReadError::BufferSize);
        }
        Ok(())
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

fn visit_directory_sector<F>(kind: FatKind, sector: &[u8], visitor: &mut F) -> bool
where
    F: FnMut(FatDirectoryEntry) -> bool,
{
    for raw_entry in sector.chunks_exact(DIRECTORY_ENTRY_SIZE) {
        match parse_directory_slot(kind, raw_entry) {
            ParsedDirectorySlot::End => return true,
            ParsedDirectorySlot::Skip => continue,
            ParsedDirectorySlot::Entry(entry) if !visitor(entry) => return true,
            ParsedDirectorySlot::Entry(_) => {}
        }
    }
    false
}

fn parse_directory_slot(kind: FatKind, raw: &[u8]) -> ParsedDirectorySlot {
    if raw[0] == 0x00 {
        return ParsedDirectorySlot::End;
    }
    if raw[0] == 0xe5 || raw[11] == 0x0f {
        return ParsedDirectorySlot::Skip;
    }

    let mut short_name = [0_u8; 11];
    short_name.copy_from_slice(&raw[..11]);

    let high_cluster = if matches!(kind, FatKind::Fat32) {
        u32::from(read_u16(raw, 20))
    } else {
        0
    };
    let low_cluster = u32::from(read_u16(raw, 26));

    ParsedDirectorySlot::Entry(FatDirectoryEntry {
        short_name,
        attributes: raw[11],
        first_cluster: (high_cluster << 16) | low_cluster,
        file_size: read_u32(raw, 28),
    })
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
    fn reads_file_content_across_cluster_chain() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 1,
            root_entry_count: 0,
            total_sectors: 5,
            fat_size_sectors: 1,
            first_data_sector: 2,
            data_cluster_count: 3,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 5>::new();

        let mut fat_sector = [0_u8; 512];
        fat_sector[8..12].copy_from_slice(&3_u32.to_le_bytes());
        fat_sector[12..16].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        device.write_blocks(2, &[b'A'; 512]).unwrap();
        device.write_blocks(3, &[b'B'; 512]).unwrap();

        let entry = FatDirectoryEntry {
            short_name: *b"DATA    BIN",
            attributes: 0x20,
            first_cluster: 2,
            file_size: 700,
        };
        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 2];
        let mut destination = [0_u8; 700];

        let length = reader
            .read_file(entry, &mut chain, &mut sector_buffer, &mut destination)
            .unwrap();

        assert_eq!(length, 700);
        assert!(destination[..512].iter().all(|byte| *byte == b'A'));
        assert!(destination[512..].iter().all(|byte| *byte == b'B'));
    }

    #[test]
    fn file_reader_rejects_small_destination_and_accepts_empty_file() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut reader = FatTableReader::new(&mut device, fat32_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 1];

        let non_empty = FatDirectoryEntry {
            short_name: *b"DATA    BIN",
            attributes: 0x20,
            first_cluster: 2,
            file_size: 2,
        };
        assert_eq!(
            reader.read_file(
                non_empty,
                &mut chain,
                &mut sector_buffer,
                &mut [0_u8; 1],
            ),
            Err(FatFileError::DestinationTooSmall)
        );

        let empty = FatDirectoryEntry {
            short_name: *b"EMPTY   TXT",
            attributes: 0x20,
            first_cluster: 0,
            file_size: 0,
        };
        assert_eq!(
            reader.read_file(empty, &mut [], &mut sector_buffer, &mut []),
            Ok(0)
        );
    }

    #[test]
    fn parses_short_directory_entry_metadata() {
        let mut raw = [0_u8; DIRECTORY_ENTRY_SIZE];
        raw[..11].copy_from_slice(b"README  TXT");
        raw[11] = 0x20;
        raw[20..22].copy_from_slice(&1_u16.to_le_bytes());
        raw[26..28].copy_from_slice(&2_u16.to_le_bytes());
        raw[28..32].copy_from_slice(&1234_u32.to_le_bytes());

        assert_eq!(
            parse_directory_slot(FatKind::Fat32, &raw),
            ParsedDirectorySlot::Entry(FatDirectoryEntry {
                short_name: *b"README  TXT",
                attributes: 0x20,
                first_cluster: 0x0001_0002,
                file_size: 1234,
            })
        );
    }

    #[test]
    fn reads_fat16_fixed_root_directory() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 80>::new();
        let mut directory_sector = [0_u8; 512];
        directory_sector[..11].copy_from_slice(b"README  TXT");
        directory_sector[11] = 0x20;
        directory_sector[26..28].copy_from_slice(&2_u16.to_le_bytes());
        directory_sector[28..32].copy_from_slice(&77_u32.to_le_bytes());
        device.write_blocks(41, &directory_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut visited = None;
        reader
            .read_directory(FatDirectory::Root, &mut [], &mut sector_buffer, |entry| {
                visited = Some(entry);
                false
            })
            .unwrap();

        assert_eq!(
            visited,
            Some(FatDirectoryEntry {
                short_name: *b"README  TXT",
                attributes: 0x20,
                first_cluster: 2,
                file_size: 77,
            })
        );
    }

    #[test]
    fn reads_fat32_root_directory_through_cluster_chain() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 1,
            root_entry_count: 0,
            total_sectors: 4,
            fat_size_sectors: 1,
            first_data_sector: 2,
            data_cluster_count: 2,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();

        let mut fat_sector = [0_u8; 512];
        fat_sector[8..12].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut directory_sector = [0_u8; 512];
        directory_sector[..11].copy_from_slice(b"SUBDIR     ");
        directory_sector[11] = 0x10;
        directory_sector[26..28].copy_from_slice(&3_u16.to_le_bytes());
        device.write_blocks(2, &directory_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 2];
        let mut visited = None;
        reader
            .read_directory(
                FatDirectory::Root,
                &mut chain,
                &mut sector_buffer,
                |entry| {
                    visited = Some(entry);
                    false
                },
            )
            .unwrap();

        assert_eq!(
            visited,
            Some(FatDirectoryEntry {
                short_name: *b"SUBDIR     ",
                attributes: 0x10,
                first_cluster: 3,
                file_size: 0,
            })
        );
        assert!(visited.unwrap().is_directory());
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
