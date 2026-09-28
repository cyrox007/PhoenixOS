#![no_std]

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
        assert_eq!(FatBootSector::parse(&sector), Err(FatError::InvalidGeometry));
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
