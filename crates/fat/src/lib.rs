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
    FatTableTooSmall,
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
pub const FAT_LONG_NAME_MAX_UNITS: usize = 255;
pub const FAT_LONG_NAME_BUFFER_UNITS: usize = 260;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatDirectoryLocation {
    sector: u64,
    offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatLocatedDirectoryEntry {
    pub entry: FatDirectoryEntry,
    pub location: FatDirectoryLocation,
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
    LongNameBufferTooSmall,
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
pub enum FatWriteError {
    Read(FatReadError),
    Chain(FatChainError),
    NotRegularFile,
    SourceTooLarge,
    WriteBeyondFile,
    MissingFirstCluster,
    ChainTooShort,
    PreparedChainChanged(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatMutationError {
    Read(FatReadError),
    Chain(FatChainError),
    UnsupportedValue,
    NoFreeCluster,
    PlanBufferTooSmall,
    EmptyChainPlan,
    DuplicateCluster(u32),
    ClusterNotFree(u32),
    InvalidDirectoryLocation,
    DirectoryEntryChanged,
    NotRegularFile,
    MissingFirstCluster,
}

impl From<FatReadError> for FatMutationError {
    fn from(error: FatReadError) -> Self {
        Self::Read(error)
    }
}

impl From<BlockError> for FatMutationError {
    fn from(error: BlockError) -> Self {
        Self::Read(FatReadError::Device(error))
    }
}

impl From<FatChainError> for FatMutationError {
    fn from(error: FatChainError) -> Self {
        Self::Chain(error)
    }
}

impl From<FatReadError> for FatWriteError {
    fn from(error: FatReadError) -> Self {
        Self::Read(error)
    }
}

impl From<FatChainError> for FatWriteError {
    fn from(error: FatChainError) -> Self {
        Self::Chain(error)
    }
}

impl From<BlockError> for FatWriteError {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectorySearchResult {
    Continue,
    End,
    Found(FatLocatedDirectoryEntry),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LongNameState {
    active: bool,
    expected_sequence: u8,
    checksum: u8,
    total_units: usize,
}

impl LongNameState {
    const fn new() -> Self {
        Self {
            active: false,
            expected_sequence: 0,
            checksum: 0,
            total_units: 0,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
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
        let sector_inside_fat = entry_offset / bytes_per_sector;
        if sector_inside_fat >= u64::from(self.boot.fat_size_sectors) {
            return Err(FatReadError::FatTableTooSmall);
        }
        let fat_sector = u64::from(self.boot.reserved_sectors)
            .checked_add(sector_inside_fat)
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

        let file_size = usize::try_from(entry.file_size).map_err(|_| FatFileError::FileTooLarge)?;
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

    pub fn update_file_directory_entry(
        &mut self,
        located: FatLocatedDirectoryEntry,
        first_cluster: u32,
        file_size: u32,
        sector_buffer: &mut [u8],
    ) -> Result<FatDirectoryEntry, FatMutationError> {
        self.validate_sector_buffer(sector_buffer)?;
        if matches!(self.boot.kind, FatKind::Fat12) {
            return Err(FatReadError::UnsupportedFat12.into());
        }
        if located.entry.is_directory() || located.entry.is_volume_label() {
            return Err(FatMutationError::NotRegularFile);
        }
        if first_cluster == 0 && file_size != 0 {
            return Err(FatMutationError::MissingFirstCluster);
        }
        if first_cluster != 0 {
            self.validate_cluster(first_cluster)?;
        }

        let offset = located.location.offset;
        let Some(end) = offset.checked_add(DIRECTORY_ENTRY_SIZE) else {
            return Err(FatMutationError::InvalidDirectoryLocation);
        };
        if offset % DIRECTORY_ENTRY_SIZE != 0 || end > sector_buffer.len() {
            return Err(FatMutationError::InvalidDirectoryLocation);
        }

        self.device
            .read_blocks(located.location.sector, sector_buffer)?;
        let raw_entry = &sector_buffer[offset..end];
        let ParsedDirectorySlot::Entry(current) = parse_directory_slot(self.boot.kind, raw_entry)
        else {
            return Err(FatMutationError::DirectoryEntryChanged);
        };
        if current != located.entry {
            return Err(FatMutationError::DirectoryEntryChanged);
        }

        if matches!(self.boot.kind, FatKind::Fat32) {
            let high_cluster = ((first_cluster >> 16) & 0xffff) as u16;
            sector_buffer[offset + 20..offset + 22].copy_from_slice(&high_cluster.to_le_bytes());
        }
        let low_cluster = (first_cluster & 0xffff) as u16;
        sector_buffer[offset + 26..offset + 28].copy_from_slice(&low_cluster.to_le_bytes());
        sector_buffer[offset + 28..offset + 32].copy_from_slice(&file_size.to_le_bytes());

        self.device
            .write_blocks(located.location.sector, sector_buffer)?;
        self.device.flush()?;

        Ok(FatDirectoryEntry {
            short_name: located.entry.short_name,
            attributes: located.entry.attributes,
            first_cluster,
            file_size,
        })
    }

    pub fn free_detached_chain(
        &mut self,
        start_cluster: u32,
        chain: &mut [u32],
        sector_buffer: &mut [u8],
    ) -> Result<usize, FatMutationError> {
        let chain_length = self.read_chain(start_cluster, chain, sector_buffer)?;

        for cluster in &chain[..chain_length] {
            self.write_fat_entry(*cluster, FatEntry::Free, sector_buffer)?;
        }

        Ok(chain_length)
    }

    pub fn commit_planned_chain(
        &mut self,
        clusters: &[u32],
        sector_buffer: &mut [u8],
    ) -> Result<(), FatMutationError> {
        self.validate_sector_buffer(sector_buffer)?;
        if clusters.is_empty() {
            return Err(FatMutationError::EmptyChainPlan);
        }

        for (index, cluster) in clusters.iter().copied().enumerate() {
            self.validate_cluster(cluster)?;
            if clusters[..index].contains(&cluster) {
                return Err(FatMutationError::DuplicateCluster(cluster));
            }
            if !matches!(self.read_entry(cluster, sector_buffer)?, FatEntry::Free) {
                return Err(FatMutationError::ClusterNotFree(cluster));
            }
        }

        for index in (0..clusters.len()).rev() {
            let value = match clusters.get(index + 1) {
                Some(next) => FatEntry::Data(*next),
                None => FatEntry::EndOfChain,
            };
            self.write_fat_entry(clusters[index], value, sector_buffer)?;
        }

        Ok(())
    }

    pub fn plan_free_clusters(
        &mut self,
        count: usize,
        start_cluster: u32,
        output: &mut [u32],
        sector_buffer: &mut [u8],
    ) -> Result<usize, FatMutationError> {
        self.validate_sector_buffer(sector_buffer)?;
        if count > output.len() {
            return Err(FatMutationError::PlanBufferTooSmall);
        }
        if count == 0 {
            return Ok(0);
        }
        if matches!(self.boot.kind, FatKind::Fat12) {
            return Err(FatReadError::UnsupportedFat12.into());
        }

        let last = self
            .boot
            .data_cluster_count
            .checked_add(1)
            .ok_or(FatReadError::ArithmeticOverflow)?;
        let start = start_cluster.max(2);
        if start > last {
            return Err(FatReadError::ClusterOutOfRange.into());
        }

        let mut found = 0_usize;
        for cluster in start..=last {
            if !matches!(self.read_entry(cluster, sector_buffer)?, FatEntry::Free) {
                continue;
            }

            output[found] = cluster;
            found += 1;
            if found == count {
                return Ok(found);
            }
        }

        for cluster in 2..start {
            if !matches!(self.read_entry(cluster, sector_buffer)?, FatEntry::Free) {
                continue;
            }

            output[found] = cluster;
            found += 1;
            if found == count {
                return Ok(found);
            }
        }

        Err(FatMutationError::NoFreeCluster)
    }

    pub fn find_free_cluster(
        &mut self,
        start_cluster: u32,
        sector_buffer: &mut [u8],
    ) -> Result<u32, FatMutationError> {
        self.validate_sector_buffer(sector_buffer)?;
        if matches!(self.boot.kind, FatKind::Fat12) {
            return Err(FatReadError::UnsupportedFat12.into());
        }

        let first = start_cluster.max(2);
        let last = self
            .boot
            .data_cluster_count
            .checked_add(1)
            .ok_or(FatReadError::ArithmeticOverflow)?;

        for cluster in first..=last {
            if matches!(self.read_entry(cluster, sector_buffer)?, FatEntry::Free) {
                return Ok(cluster);
            }
        }

        Err(FatMutationError::NoFreeCluster)
    }

    pub fn write_fat_entry(
        &mut self,
        cluster: u32,
        value: FatEntry,
        sector_buffer: &mut [u8],
    ) -> Result<(), FatMutationError> {
        self.validate_cluster(cluster)?;
        self.validate_sector_buffer(sector_buffer)?;
        if let FatEntry::Data(next_cluster) = value {
            self.validate_cluster(next_cluster)?;
        }
        let raw_value = encode_fat_entry(self.boot.kind, value)?;
        let entry_size = self.entry_size()?;
        let bytes_per_sector = u64::from(self.boot.bytes_per_sector);
        let entry_offset = u64::from(cluster)
            .checked_mul(entry_size)
            .ok_or(FatReadError::ArithmeticOverflow)?;
        let sector_inside_fat = entry_offset / bytes_per_sector;
        if sector_inside_fat >= u64::from(self.boot.fat_size_sectors) {
            return Err(FatReadError::FatTableTooSmall.into());
        }
        let byte_offset = usize::try_from(entry_offset % bytes_per_sector)
            .map_err(|_| FatReadError::ArithmeticOverflow)?;

        for fat_index in 0..u64::from(self.boot.fat_count) {
            let fat_base = u64::from(self.boot.reserved_sectors)
                .checked_add(
                    fat_index
                        .checked_mul(u64::from(self.boot.fat_size_sectors))
                        .ok_or(FatReadError::ArithmeticOverflow)?,
                )
                .ok_or(FatReadError::ArithmeticOverflow)?;
            let sector = fat_base
                .checked_add(sector_inside_fat)
                .ok_or(FatReadError::ArithmeticOverflow)?;

            self.device.read_blocks(sector, sector_buffer)?;
            match self.boot.kind {
                FatKind::Fat12 => return Err(FatReadError::UnsupportedFat12.into()),
                FatKind::Fat16 => {
                    let raw =
                        u16::try_from(raw_value).map_err(|_| FatMutationError::UnsupportedValue)?;
                    sector_buffer[byte_offset..byte_offset + 2].copy_from_slice(&raw.to_le_bytes());
                }
                FatKind::Fat32 => {
                    let existing = read_u32(sector_buffer, byte_offset);
                    let raw = (existing & 0xf000_0000) | (raw_value & 0x0fff_ffff);
                    sector_buffer[byte_offset..byte_offset + 4].copy_from_slice(&raw.to_le_bytes());
                }
            }
            self.device.write_blocks(sector, sector_buffer)?;
        }

        self.device.flush()?;
        Ok(())
    }

    pub fn write_prepared_chain(
        &mut self,
        clusters: &[u32],
        sector_buffer: &mut [u8],
        source: &[u8],
    ) -> Result<usize, FatWriteError> {
        self.validate_sector_buffer(sector_buffer)?;
        u32::try_from(source.len()).map_err(|_| FatWriteError::SourceTooLarge)?;

        if source.is_empty() {
            return Ok(0);
        }
        if clusters.is_empty() {
            return Err(FatWriteError::ChainTooShort);
        }

        let bytes_per_sector = usize::from(self.boot.bytes_per_sector);
        let sectors_per_cluster = usize::from(self.boot.sectors_per_cluster);
        let bytes_per_cluster = bytes_per_sector
            .checked_mul(sectors_per_cluster)
            .ok_or(FatReadError::ArithmeticOverflow)?;
        let capacity = clusters
            .len()
            .checked_mul(bytes_per_cluster)
            .ok_or(FatReadError::ArithmeticOverflow)?;
        if source.len() > capacity {
            return Err(FatWriteError::ChainTooShort);
        }

        for (index, cluster) in clusters.iter().copied().enumerate() {
            self.validate_cluster(cluster)?;

            let expected = match clusters.get(index + 1) {
                Some(next_cluster) => {
                    self.validate_cluster(*next_cluster)?;
                    FatEntry::Data(*next_cluster)
                }
                None => FatEntry::EndOfChain,
            };
            if self.read_entry(cluster, sector_buffer)? != expected {
                return Err(FatWriteError::PreparedChainChanged(cluster));
            }
        }

        let mut written = 0_usize;
        for cluster in clusters {
            let first_sector = self.cluster_first_sector(*cluster)?;

            for offset in 0..sectors_per_cluster {
                sector_buffer.fill(0);

                let remaining = source.len() - written;
                let copied = remaining.min(bytes_per_sector);
                if copied != 0 {
                    sector_buffer[..copied]
                        .copy_from_slice(&source[written..written + copied]);
                    written += copied;
                }

                let sector = first_sector
                    .checked_add(offset as u64)
                    .ok_or(FatReadError::ArithmeticOverflow)?;
                self.device.write_blocks(sector, sector_buffer)?;
            }
        }

        self.device.flush()?;
        Ok(written)
    }

    pub fn write_file_in_place(
        &mut self,
        entry: FatDirectoryEntry,
        offset: u32,
        chain: &mut [u32],
        sector_buffer: &mut [u8],
        source: &[u8],
    ) -> Result<usize, FatWriteError> {
        self.validate_sector_buffer(sector_buffer)?;

        if entry.is_directory() || entry.is_volume_label() {
            return Err(FatWriteError::NotRegularFile);
        }

        let source_length =
            u32::try_from(source.len()).map_err(|_| FatWriteError::SourceTooLarge)?;
        let end = offset
            .checked_add(source_length)
            .ok_or(FatReadError::ArithmeticOverflow)?;
        if end > entry.file_size {
            return Err(FatWriteError::WriteBeyondFile);
        }
        if source.is_empty() {
            return Ok(0);
        }
        if entry.first_cluster < 2 {
            return Err(FatWriteError::MissingFirstCluster);
        }

        let chain_length = self.read_chain(entry.first_cluster, chain, sector_buffer)?;
        let bytes_per_cluster = usize::from(self.boot.bytes_per_sector)
            .checked_mul(usize::from(self.boot.sectors_per_cluster))
            .ok_or(FatReadError::ArithmeticOverflow)?;
        let file_size =
            usize::try_from(entry.file_size).map_err(|_| FatReadError::ArithmeticOverflow)?;
        let required_clusters = file_size.div_ceil(bytes_per_cluster);
        if chain_length < required_clusters {
            return Err(FatWriteError::ChainTooShort);
        }

        let start_offset = usize::try_from(offset).map_err(|_| FatReadError::ArithmeticOverflow)?;
        let bytes_per_sector = usize::from(self.boot.bytes_per_sector);
        let mut written = 0_usize;

        while written < source.len() {
            let absolute_offset = start_offset
                .checked_add(written)
                .ok_or(FatReadError::ArithmeticOverflow)?;
            let cluster_index = absolute_offset / bytes_per_cluster;
            if cluster_index >= chain_length {
                return Err(FatWriteError::ChainTooShort);
            }

            let within_cluster = absolute_offset % bytes_per_cluster;
            let sector_index = within_cluster / bytes_per_sector;
            let within_sector = within_cluster % bytes_per_sector;
            let first_sector = self.cluster_first_sector(chain[cluster_index])?;
            let sector = first_sector
                .checked_add(sector_index as u64)
                .ok_or(FatReadError::ArithmeticOverflow)?;

            self.device.read_blocks(sector, sector_buffer)?;
            let remaining_in_sector = bytes_per_sector - within_sector;
            let copied = remaining_in_sector.min(source.len() - written);
            sector_buffer[within_sector..within_sector + copied]
                .copy_from_slice(&source[written..written + copied]);
            self.device.write_blocks(sector, sector_buffer)?;
            written += copied;
        }

        self.device.flush()?;
        Ok(written)
    }

    pub fn find_directory_entry(
        &mut self,
        directory: FatDirectory,
        short_name: [u8; 11],
        chain: &mut [u32],
        sector_buffer: &mut [u8],
    ) -> Result<Option<FatLocatedDirectoryEntry>, FatDirectoryError> {
        self.validate_sector_buffer(sector_buffer)?;
        if matches!(self.boot.kind, FatKind::Fat12) {
            return Err(FatReadError::UnsupportedFat12.into());
        }

        match directory {
            FatDirectory::Root if matches!(self.boot.kind, FatKind::Fat16) => {
                self.find_fat16_root_directory_entry(short_name, sector_buffer)
            }
            FatDirectory::Root => {
                let start_cluster = self
                    .boot
                    .root_cluster
                    .ok_or(FatDirectoryError::MissingRootCluster)?;
                self.find_cluster_directory_entry(start_cluster, short_name, chain, sector_buffer)
            }
            FatDirectory::Cluster(start_cluster) => {
                self.find_cluster_directory_entry(start_cluster, short_name, chain, sector_buffer)
            }
        }
    }

    pub fn read_directory_with_names<F>(
        &mut self,
        directory: FatDirectory,
        chain: &mut [u32],
        sector_buffer: &mut [u8],
        long_name_buffer: &mut [u16],
        mut visitor: F,
    ) -> Result<(), FatDirectoryError>
    where
        F: FnMut(FatDirectoryEntry, Option<&[u16]>) -> bool,
    {
        self.validate_sector_buffer(sector_buffer)?;
        if long_name_buffer.len() < FAT_LONG_NAME_BUFFER_UNITS {
            return Err(FatDirectoryError::LongNameBufferTooSmall);
        }
        if matches!(self.boot.kind, FatKind::Fat12) {
            return Err(FatReadError::UnsupportedFat12.into());
        }

        let mut state = LongNameState::new();
        match directory {
            FatDirectory::Root if matches!(self.boot.kind, FatKind::Fat16) => self
                .read_fat16_root_directory_with_names(
                    sector_buffer,
                    long_name_buffer,
                    &mut state,
                    &mut visitor,
                ),
            FatDirectory::Root => {
                let start_cluster = self
                    .boot
                    .root_cluster
                    .ok_or(FatDirectoryError::MissingRootCluster)?;
                self.read_cluster_directory_with_names(
                    start_cluster,
                    chain,
                    sector_buffer,
                    long_name_buffer,
                    &mut state,
                    &mut visitor,
                )
            }
            FatDirectory::Cluster(start_cluster) => self.read_cluster_directory_with_names(
                start_cluster,
                chain,
                sector_buffer,
                long_name_buffer,
                &mut state,
                &mut visitor,
            ),
        }
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

    fn find_fat16_root_directory_entry(
        &mut self,
        short_name: [u8; 11],
        sector_buffer: &mut [u8],
    ) -> Result<Option<FatLocatedDirectoryEntry>, FatDirectoryError> {
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

            match find_directory_entry_in_sector(self.boot.kind, sector, sector_buffer, short_name)
            {
                DirectorySearchResult::Found(entry) => return Ok(Some(entry)),
                DirectorySearchResult::End => return Ok(None),
                DirectorySearchResult::Continue => {}
            }
        }

        Ok(None)
    }

    fn find_cluster_directory_entry(
        &mut self,
        start_cluster: u32,
        short_name: [u8; 11],
        chain: &mut [u32],
        sector_buffer: &mut [u8],
    ) -> Result<Option<FatLocatedDirectoryEntry>, FatDirectoryError> {
        let chain_length = self.read_chain(start_cluster, chain, sector_buffer)?;
        let sectors_per_cluster = u64::from(self.boot.sectors_per_cluster);

        for cluster in &chain[..chain_length] {
            let first_sector = self.cluster_first_sector(*cluster)?;
            for offset in 0..sectors_per_cluster {
                let sector = first_sector
                    .checked_add(offset)
                    .ok_or(FatReadError::ArithmeticOverflow)?;
                self.device.read_blocks(sector, sector_buffer)?;

                match find_directory_entry_in_sector(
                    self.boot.kind,
                    sector,
                    sector_buffer,
                    short_name,
                ) {
                    DirectorySearchResult::Found(entry) => return Ok(Some(entry)),
                    DirectorySearchResult::End => return Ok(None),
                    DirectorySearchResult::Continue => {}
                }
            }
        }

        Ok(None)
    }

    fn read_fat16_root_directory_with_names<F>(
        &mut self,
        sector_buffer: &mut [u8],
        long_name_buffer: &mut [u16],
        state: &mut LongNameState,
        visitor: &mut F,
    ) -> Result<(), FatDirectoryError>
    where
        F: FnMut(FatDirectoryEntry, Option<&[u16]>) -> bool,
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
            if visit_directory_sector_with_names(
                self.boot.kind,
                sector_buffer,
                long_name_buffer,
                state,
                visitor,
            ) {
                return Ok(());
            }
        }

        Ok(())
    }

    fn read_cluster_directory_with_names<F>(
        &mut self,
        start_cluster: u32,
        chain: &mut [u32],
        sector_buffer: &mut [u8],
        long_name_buffer: &mut [u16],
        state: &mut LongNameState,
        visitor: &mut F,
    ) -> Result<(), FatDirectoryError>
    where
        F: FnMut(FatDirectoryEntry, Option<&[u16]>) -> bool,
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
                if visit_directory_sector_with_names(
                    self.boot.kind,
                    sector_buffer,
                    long_name_buffer,
                    state,
                    visitor,
                ) {
                    return Ok(());
                }
            }
        }

        Ok(())
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

fn encode_fat_entry(kind: FatKind, value: FatEntry) -> Result<u32, FatMutationError> {
    match (kind, value) {
        (FatKind::Fat12, _) => Err(FatReadError::UnsupportedFat12.into()),
        (FatKind::Fat16, FatEntry::Free) => Ok(0),
        (FatKind::Fat16, FatEntry::Data(next)) if next <= 0xffef => Ok(next),
        (FatKind::Fat16, FatEntry::Bad) => Ok(0xfff7),
        (FatKind::Fat16, FatEntry::EndOfChain) => Ok(0xffff),
        (FatKind::Fat32, FatEntry::Free) => Ok(0),
        (FatKind::Fat32, FatEntry::Data(next)) if next <= 0x0fff_ffef => Ok(next),
        (FatKind::Fat32, FatEntry::Bad) => Ok(0x0fff_fff7),
        (FatKind::Fat32, FatEntry::EndOfChain) => Ok(0x0fff_ffff),
        (_, FatEntry::Reserved(_)) | (_, FatEntry::Data(_)) => {
            Err(FatMutationError::UnsupportedValue)
        }
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

fn visit_directory_sector_with_names<F>(
    kind: FatKind,
    sector: &[u8],
    long_name_buffer: &mut [u16],
    state: &mut LongNameState,
    visitor: &mut F,
) -> bool
where
    F: FnMut(FatDirectoryEntry, Option<&[u16]>) -> bool,
{
    for raw_entry in sector.chunks_exact(DIRECTORY_ENTRY_SIZE) {
        if raw_entry[0] == 0x00 {
            state.reset();
            return true;
        }
        if raw_entry[0] == 0xe5 {
            state.reset();
            continue;
        }
        if raw_entry[11] == 0x0f {
            consume_long_name_slot(raw_entry, long_name_buffer, state);
            continue;
        }

        let ParsedDirectorySlot::Entry(entry) = parse_directory_slot(kind, raw_entry) else {
            state.reset();
            continue;
        };
        let long_name = finish_long_name(entry.short_name, long_name_buffer, state);
        if !visitor(entry, long_name) {
            state.reset();
            return true;
        }
        state.reset();
    }
    false
}

fn consume_long_name_slot(raw: &[u8], buffer: &mut [u16], state: &mut LongNameState) {
    let sequence = raw[0] & 0x3f;
    let is_last = raw[0] & 0x40 != 0;
    let checksum = raw[13];

    if sequence == 0
        || sequence > 20
        || raw[12] != 0
        || read_u16(raw, 26) != 0
        || usize::from(sequence) * 13 > buffer.len()
    {
        state.reset();
        return;
    }

    if is_last {
        state.active = true;
        state.expected_sequence = sequence;
        state.checksum = checksum;
        state.total_units = usize::from(sequence) * 13;
    }

    if !state.active || state.expected_sequence != sequence || state.checksum != checksum {
        state.reset();
        return;
    }

    let start = usize::from(sequence - 1) * 13;
    for (index, offset) in [1_usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
        .iter()
        .copied()
        .enumerate()
    {
        buffer[start + index] = read_u16(raw, offset);
    }

    state.expected_sequence -= 1;
}

fn finish_long_name<'a>(
    short_name: [u8; 11],
    buffer: &'a [u16],
    state: &LongNameState,
) -> Option<&'a [u16]> {
    if !state.active
        || state.expected_sequence != 0
        || state.checksum != short_name_checksum(&short_name)
    {
        return None;
    }

    let mut length = 0;
    while length < state.total_units {
        let value = buffer[length];
        if value == 0x0000 || value == 0xffff {
            break;
        }
        length += 1;
    }

    if length > FAT_LONG_NAME_MAX_UNITS {
        return None;
    }
    Some(&buffer[..length])
}

fn short_name_checksum(short_name: &[u8; 11]) -> u8 {
    let mut checksum = 0_u8;
    for byte in short_name {
        checksum = checksum.rotate_right(1).wrapping_add(*byte);
    }
    checksum
}

fn find_directory_entry_in_sector(
    kind: FatKind,
    sector_number: u64,
    sector: &[u8],
    short_name: [u8; 11],
) -> DirectorySearchResult {
    for (index, raw_entry) in sector.chunks_exact(DIRECTORY_ENTRY_SIZE).enumerate() {
        match parse_directory_slot(kind, raw_entry) {
            ParsedDirectorySlot::End => return DirectorySearchResult::End,
            ParsedDirectorySlot::Skip => continue,
            ParsedDirectorySlot::Entry(entry) if entry.short_name == short_name => {
                return DirectorySearchResult::Found(FatLocatedDirectoryEntry {
                    entry,
                    location: FatDirectoryLocation {
                        sector: sector_number,
                        offset: index * DIRECTORY_ENTRY_SIZE,
                    },
                });
            }
            ParsedDirectorySlot::Entry(_) => {}
        }
    }

    DirectorySearchResult::Continue
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

    fn write_long_name_slot(slot: &mut [u8], sequence: u8, checksum: u8, name: &[u16]) {
        slot.fill(0xff);
        slot[0] = sequence;
        slot[11] = 0x0f;
        slot[12] = 0;
        slot[13] = checksum;
        slot[26..28].copy_from_slice(&0_u16.to_le_bytes());

        let ordinal = usize::from(sequence & 0x3f);
        let start = (ordinal - 1) * 13;
        let offsets = [1_usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        for (index, offset) in offsets.iter().copied().enumerate() {
            let name_index = start + index;
            let value = if name_index < name.len() {
                name[name_index]
            } else if name_index == name.len() {
                0
            } else {
                0xffff
            };
            slot[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
    }

    #[test]
    fn finds_fat16_root_directory_entry_with_location() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 80>::new();
        let mut sector = [0_u8; 512];
        sector[0] = 0xe5;

        let offset = DIRECTORY_ENTRY_SIZE;
        sector[offset..offset + 11].copy_from_slice(b"DATA    BIN");
        sector[offset + 11] = 0x20;
        sector[offset + 26..offset + 28].copy_from_slice(&2_u16.to_le_bytes());
        sector[offset + 28..offset + 32].copy_from_slice(&55_u32.to_le_bytes());
        device.write_blocks(41, &sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];

        let located = reader
            .find_directory_entry(
                FatDirectory::Root,
                *b"DATA    BIN",
                &mut [],
                &mut sector_buffer,
            )
            .unwrap()
            .unwrap();

        assert_eq!(located.entry.first_cluster, 2);
        assert_eq!(located.entry.file_size, 55);
        assert_eq!(located.location.sector, 41);
        assert_eq!(located.location.offset, DIRECTORY_ENTRY_SIZE);
    }

    #[test]
    fn finds_fat32_root_directory_entry_with_location() {
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
        fat_sector[8..12].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut directory_sector = [0_u8; 512];
        directory_sector[..11].copy_from_slice(b"ROOT    TXT");
        directory_sector[11] = 0x20;
        directory_sector[20..22].copy_from_slice(&0_u16.to_le_bytes());
        directory_sector[26..28].copy_from_slice(&2_u16.to_le_bytes());
        directory_sector[28..32].copy_from_slice(&7_u32.to_le_bytes());
        device.write_blocks(2, &directory_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 1];

        let located = reader
            .find_directory_entry(
                FatDirectory::Root,
                *b"ROOT    TXT",
                &mut chain,
                &mut sector_buffer,
            )
            .unwrap()
            .unwrap();

        assert_eq!(located.entry.first_cluster, 2);
        assert_eq!(located.entry.file_size, 7);
        assert_eq!(located.location.sector, 2);
        assert_eq!(located.location.offset, 0);
    }

    #[test]
    fn reads_long_name_across_root_directory_sector_boundary() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 80>::new();
        let short_name = *b"PHOENI~1TXT";
        let checksum = short_name_checksum(&short_name);
        let name: [u16; 21] = [
            0x0050, 0x0068, 0x006f, 0x0065, 0x006e, 0x0069, 0x0078, 0x0020, 0x004c, 0x006f, 0x006e,
            0x0067, 0x0020, 0x0046, 0x0069, 0x006c, 0x0065, 0x002e, 0x0074, 0x0078, 0x0074,
        ];

        let mut first_sector = [0xe5_u8; 512];
        write_long_name_slot(&mut first_sector[448..480], 0x42, checksum, &name);
        write_long_name_slot(&mut first_sector[480..512], 0x01, checksum, &name);
        device.write_blocks(41, &first_sector).unwrap();

        let mut second_sector = [0_u8; 512];
        second_sector[..11].copy_from_slice(&short_name);
        second_sector[11] = 0x20;
        second_sector[26..28].copy_from_slice(&2_u16.to_le_bytes());
        second_sector[28..32].copy_from_slice(&55_u32.to_le_bytes());
        device.write_blocks(42, &second_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut long_name_buffer = [0_u16; FAT_LONG_NAME_BUFFER_UNITS];
        let mut actual_name = [0_u16; 21];
        let mut actual_length = 0_usize;

        reader
            .read_directory_with_names(
                FatDirectory::Root,
                &mut [],
                &mut sector_buffer,
                &mut long_name_buffer,
                |entry, long_name| {
                    if entry.short_name == short_name {
                        let long_name = long_name.unwrap();
                        actual_length = long_name.len();
                        actual_name[..long_name.len()].copy_from_slice(long_name);
                        return false;
                    }
                    true
                },
            )
            .unwrap();

        assert_eq!(actual_length, name.len());
        assert_eq!(actual_name, name);
    }

    #[test]
    fn invalid_long_name_checksum_falls_back_to_short_name() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 80>::new();
        let short_name = *b"README~1TXT";
        let name = [
            0x0052_u16, 0x0065, 0x0061, 0x0064, 0x006d, 0x0065, 0x002e, 0x0074, 0x0078, 0x0074,
        ];
        let mut sector = [0_u8; 512];
        write_long_name_slot(&mut sector[..32], 0x41, 0x55, &name);
        sector[32..43].copy_from_slice(&short_name);
        sector[43] = 0x20;
        device.write_blocks(41, &sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut long_name_buffer = [0_u16; FAT_LONG_NAME_BUFFER_UNITS];
        let mut saw_short_name = false;

        reader
            .read_directory_with_names(
                FatDirectory::Root,
                &mut [],
                &mut sector_buffer,
                &mut long_name_buffer,
                |entry, long_name| {
                    if entry.short_name == short_name {
                        saw_short_name = true;
                        assert!(long_name.is_none());
                        return false;
                    }
                    true
                },
            )
            .unwrap();

        assert!(saw_short_name);
    }

    #[test]
    fn long_name_reader_requires_complete_work_buffer() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 80>::new();
        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut short_buffer = [0_u16; FAT_LONG_NAME_MAX_UNITS];

        assert_eq!(
            reader.read_directory_with_names(
                FatDirectory::Root,
                &mut [],
                &mut sector_buffer,
                &mut short_buffer,
                |_, _| true,
            ),
            Err(FatDirectoryError::LongNameBufferTooSmall)
        );
    }

    #[test]
    fn updates_only_file_cluster_and_size_in_directory_entry() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 80>::new();
        let mut sector = [0_u8; 512];
        sector[..11].copy_from_slice(b"DATA    BIN");
        sector[11] = 0x20;
        sector[12] = 0x7a;
        sector[26..28].copy_from_slice(&2_u16.to_le_bytes());
        sector[28..32].copy_from_slice(&55_u32.to_le_bytes());
        device.write_blocks(41, &sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let located = reader
            .find_directory_entry(
                FatDirectory::Root,
                *b"DATA    BIN",
                &mut [],
                &mut sector_buffer,
            )
            .unwrap()
            .unwrap();

        let updated = reader
            .update_file_directory_entry(located, 3, 99, &mut sector_buffer)
            .unwrap();

        assert_eq!(updated.first_cluster, 3);
        assert_eq!(updated.file_size, 99);

        reader.device.read_blocks(41, &mut sector_buffer).unwrap();
        assert_eq!(&sector_buffer[..11], b"DATA    BIN");
        assert_eq!(sector_buffer[11], 0x20);
        assert_eq!(sector_buffer[12], 0x7a);
        assert_eq!(read_u16(&sector_buffer, 26), 3);
        assert_eq!(read_u32(&sector_buffer, 28), 99);
    }

    #[test]
    fn directory_entry_update_rejects_stale_location() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 80>::new();
        let mut sector = [0_u8; 512];
        sector[..11].copy_from_slice(b"DATA    BIN");
        sector[11] = 0x20;
        sector[26..28].copy_from_slice(&2_u16.to_le_bytes());
        sector[28..32].copy_from_slice(&55_u32.to_le_bytes());
        device.write_blocks(41, &sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let located = reader
            .find_directory_entry(
                FatDirectory::Root,
                *b"DATA    BIN",
                &mut [],
                &mut sector_buffer,
            )
            .unwrap()
            .unwrap();

        sector_buffer[28..32].copy_from_slice(&56_u32.to_le_bytes());
        reader.device.write_blocks(41, &sector_buffer).unwrap();

        assert_eq!(
            reader.update_file_directory_entry(located, 3, 99, &mut sector_buffer),
            Err(FatMutationError::DirectoryEntryChanged)
        );

        reader.device.read_blocks(41, &mut sector_buffer).unwrap();
        assert_eq!(read_u16(&sector_buffer, 26), 2);
        assert_eq!(read_u32(&sector_buffer, 28), 56);
    }

    #[test]
    fn directory_entry_update_requires_cluster_for_nonempty_file() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 80>::new();
        let mut sector = [0_u8; 512];
        sector[..11].copy_from_slice(b"DATA    BIN");
        sector[11] = 0x20;
        sector[26..28].copy_from_slice(&2_u16.to_le_bytes());
        sector[28..32].copy_from_slice(&55_u32.to_le_bytes());
        device.write_blocks(41, &sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, fat16_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let located = reader
            .find_directory_entry(
                FatDirectory::Root,
                *b"DATA    BIN",
                &mut [],
                &mut sector_buffer,
            )
            .unwrap()
            .unwrap();

        assert_eq!(
            reader.update_file_directory_entry(located, 0, 1, &mut sector_buffer),
            Err(FatMutationError::MissingFirstCluster)
        );

        reader.device.read_blocks(41, &mut sector_buffer).unwrap();
        assert_eq!(read_u16(&sector_buffer, 26), 2);
        assert_eq!(read_u32(&sector_buffer, 28), 55);
    }

    #[test]
    fn frees_detached_chain_after_full_validation() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 1,
            root_entry_count: 0,
            total_sectors: 6,
            fat_size_sectors: 1,
            first_data_sector: 2,
            data_cluster_count: 4,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 6>::new();
        let mut fat_sector = [0_u8; 512];
        fat_sector[8..12].copy_from_slice(&3_u32.to_le_bytes());
        fat_sector[12..16].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 2];

        assert_eq!(
            reader.free_detached_chain(2, &mut chain, &mut sector_buffer),
            Ok(2)
        );
        assert_eq!(reader.read_entry(2, &mut sector_buffer), Ok(FatEntry::Free));
        assert_eq!(reader.read_entry(3, &mut sector_buffer), Ok(FatEntry::Free));
    }

    #[test]
    fn detached_chain_is_not_modified_when_validation_fails() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 1,
            root_entry_count: 0,
            total_sectors: 6,
            fat_size_sectors: 1,
            first_data_sector: 2,
            data_cluster_count: 4,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 6>::new();
        let mut fat_sector = [0_u8; 512];
        fat_sector[8..12].copy_from_slice(&3_u32.to_le_bytes());
        fat_sector[12..16].copy_from_slice(&2_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 3];

        assert_eq!(
            reader.free_detached_chain(2, &mut chain, &mut sector_buffer),
            Err(FatMutationError::Chain(FatChainError::LoopDetected(2)))
        );
        assert_eq!(
            reader.read_entry(2, &mut sector_buffer),
            Ok(FatEntry::Data(3))
        );
        assert_eq!(
            reader.read_entry(3, &mut sector_buffer),
            Ok(FatEntry::Data(2))
        );
    }

    #[test]
    fn commits_planned_chain_from_tail_to_head() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 1,
            root_entry_count: 0,
            total_sectors: 8,
            fat_size_sectors: 1,
            first_data_sector: 2,
            data_cluster_count: 6,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 8>::new();
        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];

        reader
            .commit_planned_chain(&[4, 5, 6], &mut sector_buffer)
            .unwrap();

        assert_eq!(
            reader.read_entry(4, &mut sector_buffer),
            Ok(FatEntry::Data(5))
        );
        assert_eq!(
            reader.read_entry(5, &mut sector_buffer),
            Ok(FatEntry::Data(6))
        );
        assert_eq!(
            reader.read_entry(6, &mut sector_buffer),
            Ok(FatEntry::EndOfChain)
        );
    }

    #[test]
    fn chain_commit_validates_entire_plan_before_writing() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 1,
            root_entry_count: 0,
            total_sectors: 8,
            fat_size_sectors: 1,
            first_data_sector: 2,
            data_cluster_count: 6,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 8>::new();
        let mut fat_sector = [0_u8; 512];
        fat_sector[20..24].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];

        assert_eq!(
            reader.commit_planned_chain(&[4, 5, 6], &mut sector_buffer),
            Err(FatMutationError::ClusterNotFree(5))
        );
        assert_eq!(reader.read_entry(4, &mut sector_buffer), Ok(FatEntry::Free));
        assert_eq!(reader.read_entry(6, &mut sector_buffer), Ok(FatEntry::Free));

        assert_eq!(
            reader.commit_planned_chain(&[4, 4], &mut sector_buffer),
            Err(FatMutationError::DuplicateCluster(4))
        );
        assert_eq!(reader.read_entry(4, &mut sector_buffer), Ok(FatEntry::Free));
    }

    #[test]
    fn chain_commit_rejects_empty_plan() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut reader = FatTableReader::new(&mut device, fat32_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];

        assert_eq!(
            reader.commit_planned_chain(&[], &mut sector_buffer),
            Err(FatMutationError::EmptyChainPlan)
        );
    }

    #[test]
    fn plans_free_clusters_without_modifying_table() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 1,
            root_entry_count: 0,
            total_sectors: 7,
            fat_size_sectors: 1,
            first_data_sector: 2,
            data_cluster_count: 5,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 7>::new();
        let mut fat_sector = [0_u8; 512];
        fat_sector[8..12].copy_from_slice(&3_u32.to_le_bytes());
        fat_sector[12..16].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        fat_sector[20..24].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut plan = [0_u32; 2];

        assert_eq!(
            reader.plan_free_clusters(2, 5, &mut plan, &mut sector_buffer),
            Ok(2)
        );
        assert_eq!(plan, [6, 4]);
        assert_eq!(reader.read_entry(6, &mut sector_buffer), Ok(FatEntry::Free));
        assert_eq!(reader.read_entry(4, &mut sector_buffer), Ok(FatEntry::Free));
    }

    #[test]
    fn free_cluster_plan_rejects_small_output_buffer() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut reader = FatTableReader::new(&mut device, fat32_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut plan = [0_u32; 1];

        assert_eq!(
            reader.plan_free_clusters(2, 2, &mut plan, &mut sector_buffer),
            Err(FatMutationError::PlanBufferTooSmall)
        );
    }

    #[test]
    fn rejects_entry_outside_declared_fat_size() {
        let mut boot = fat32_boot();
        boot.fat_size_sectors = 1;
        boot.data_cluster_count = 1_000;

        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];

        assert_eq!(
            reader.read_entry(200, &mut sector_buffer),
            Err(FatReadError::FatTableTooSmall)
        );
        assert_eq!(
            reader.write_fat_entry(200, FatEntry::Free, &mut sector_buffer),
            Err(FatMutationError::Read(FatReadError::FatTableTooSmall))
        );
    }

    #[test]
    fn finds_first_free_cluster_at_or_after_requested_start() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 1,
            root_entry_count: 0,
            total_sectors: 6,
            fat_size_sectors: 1,
            first_data_sector: 2,
            data_cluster_count: 4,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 6>::new();
        let mut fat_sector = [0_u8; 512];
        fat_sector[8..12].copy_from_slice(&3_u32.to_le_bytes());
        fat_sector[12..16].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];

        assert_eq!(reader.find_free_cluster(2, &mut sector_buffer), Ok(4));
        assert_eq!(reader.find_free_cluster(5, &mut sector_buffer), Ok(5));
    }

    #[test]
    fn fat_entry_write_rejects_invalid_targets_and_reserved_values() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut reader = FatTableReader::new(&mut device, fat32_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];

        assert_eq!(
            reader.write_fat_entry(2, FatEntry::Data(1), &mut sector_buffer),
            Err(FatMutationError::Read(FatReadError::ClusterOutOfRange))
        );
        assert_eq!(
            reader.write_fat_entry(2, FatEntry::Reserved(7), &mut sector_buffer),
            Err(FatMutationError::UnsupportedValue)
        );
    }

    #[test]
    fn writes_fat32_entry_to_all_tables_and_preserves_reserved_bits() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 2,
            root_entry_count: 0,
            total_sectors: 7,
            fat_size_sectors: 1,
            first_data_sector: 3,
            data_cluster_count: 4,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 7>::new();

        let mut first_fat = [0_u8; 512];
        first_fat[8..12].copy_from_slice(&0xf000_0000_u32.to_le_bytes());
        device.write_blocks(1, &first_fat).unwrap();

        let mut second_fat = [0_u8; 512];
        second_fat[8..12].copy_from_slice(&0xa000_0000_u32.to_le_bytes());
        device.write_blocks(2, &second_fat).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        reader
            .write_fat_entry(2, FatEntry::Data(3), &mut sector_buffer)
            .unwrap();

        reader.device.read_blocks(1, &mut first_fat).unwrap();
        reader.device.read_blocks(2, &mut second_fat).unwrap();
        assert_eq!(read_u32(&first_fat, 8), 0xf000_0003);
        assert_eq!(read_u32(&second_fat, 8), 0xa000_0003);
    }

    #[test]
    fn writes_prepared_chain_and_zeros_unused_tail() {
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
        device.write_blocks(2, &[0xaa_u8; 512]).unwrap();
        device.write_blocks(3, &[0xbb_u8; 512]).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let source = [b'X'; 520];

        assert_eq!(
            reader.write_prepared_chain(&[2, 3], &mut sector_buffer, &source),
            Ok(source.len())
        );

        reader.device.read_blocks(2, &mut sector_buffer).unwrap();
        assert!(sector_buffer.iter().all(|byte| *byte == b'X'));

        reader.device.read_blocks(3, &mut sector_buffer).unwrap();
        assert!(sector_buffer[..8].iter().all(|byte| *byte == b'X'));
        assert!(sector_buffer[8..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn prepared_chain_write_rejects_changed_chain_before_data() {
        let boot = FatBootSector {
            kind: FatKind::Fat32,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            reserved_sectors: 1,
            fat_count: 1,
            root_entry_count: 0,
            total_sectors: 6,
            fat_size_sectors: 1,
            first_data_sector: 2,
            data_cluster_count: 4,
            root_cluster: Some(2),
        };
        let mut device = phoenix_block::MemoryBlockDevice::<512, 6>::new();

        let mut fat_sector = [0_u8; 512];
        fat_sector[8..12].copy_from_slice(&4_u32.to_le_bytes());
        fat_sector[12..16].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        fat_sector[16..20].copy_from_slice(&0x0fff_fff8_u32.to_le_bytes());
        device.write_blocks(1, &fat_sector).unwrap();
        device.write_blocks(2, &[b'A'; 512]).unwrap();
        device.write_blocks(3, &[b'B'; 512]).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];

        assert_eq!(
            reader.write_prepared_chain(&[2, 3], &mut sector_buffer, b"new data"),
            Err(FatWriteError::PreparedChainChanged(2))
        );

        reader.device.read_blocks(2, &mut sector_buffer).unwrap();
        assert!(sector_buffer.iter().all(|byte| *byte == b'A'));

        reader.device.read_blocks(3, &mut sector_buffer).unwrap();
        assert!(sector_buffer.iter().all(|byte| *byte == b'B'));
    }

    #[test]
    fn prepared_chain_write_rejects_insufficient_capacity() {
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
        device.write_blocks(2, &[b'A'; 512]).unwrap();

        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let source = [b'X'; 513];

        assert_eq!(
            reader.write_prepared_chain(&[2], &mut sector_buffer, &source),
            Err(FatWriteError::ChainTooShort)
        );

        reader.device.read_blocks(2, &mut sector_buffer).unwrap();
        assert!(sector_buffer.iter().all(|byte| *byte == b'A'));
    }

    #[test]
    fn writes_existing_file_range_across_cluster_boundary() {
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
        let source = [b'X'; 24];

        assert_eq!(
            reader.write_file_in_place(entry, 500, &mut chain, &mut sector_buffer, &source),
            Ok(source.len())
        );

        let mut output = [0_u8; 700];
        reader
            .read_file(entry, &mut chain, &mut sector_buffer, &mut output)
            .unwrap();
        assert!(output[..500].iter().all(|byte| *byte == b'A'));
        assert!(output[500..524].iter().all(|byte| *byte == b'X'));
        assert!(output[524..].iter().all(|byte| *byte == b'B'));
    }

    #[test]
    fn in_place_write_rejects_growth_before_touching_data() {
        let mut device = phoenix_block::MemoryBlockDevice::<512, 4>::new();
        let mut reader = FatTableReader::new(&mut device, fat32_boot()).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 1];
        let entry = FatDirectoryEntry {
            short_name: *b"DATA    BIN",
            attributes: 0x20,
            first_cluster: 2,
            file_size: 10,
        };

        assert_eq!(
            reader.write_file_in_place(entry, 9, &mut chain, &mut sector_buffer, b"XX"),
            Err(FatWriteError::WriteBeyondFile)
        );
    }

    #[test]
    fn in_place_write_rejects_chain_shorter_than_file() {
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

        let entry = FatDirectoryEntry {
            short_name: *b"DATA    BIN",
            attributes: 0x20,
            first_cluster: 2,
            file_size: 700,
        };
        let mut reader = FatTableReader::new(&mut device, boot).unwrap();
        let mut sector_buffer = [0_u8; 512];
        let mut chain = [0_u32; 2];

        assert_eq!(
            reader.write_file_in_place(entry, 0, &mut chain, &mut sector_buffer, b"X"),
            Err(FatWriteError::ChainTooShort)
        );
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
            reader.read_file(non_empty, &mut chain, &mut sector_buffer, &mut [0_u8; 1],),
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
