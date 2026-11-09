//! Разбор ACPI IVRS без доступа к MMIO IOMMU и без его включения.

use crate::{AcpiError, SDT_HEADER_SIZE, SdtHeader};

pub const MAX_IVRS_BLOCKS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IvrsSummary {
    pub hardware_blocks: u16,
    pub memory_reservations: u16,
    pub unsupported_blocks: u16,
    pub first_iommu_base: Option<u64>,
    pub first_pci_segment: Option<u16>,
    pub first_device_id: Option<u16>,
    pub dma_remap_reported: bool,
}

fn u16_at(data: &[u8], start: usize) -> u16 {
    u16::from_le_bytes([data[start], data[start + 1]])
}

fn u32_at(data: &[u8], start: usize) -> u32 {
    u32::from_le_bytes([
        data[start],
        data[start + 1],
        data[start + 2],
        data[start + 3],
    ])
}

fn u64_at(data: &[u8], start: usize) -> u64 {
    u64::from_le_bytes([
        data[start],
        data[start + 1],
        data[start + 2],
        data[start + 3],
        data[start + 4],
        data[start + 5],
        data[start + 6],
        data[start + 7],
    ])
}

pub fn parse_ivrs(bytes: &[u8]) -> Result<IvrsSummary, AcpiError> {
    let header = SdtHeader::validate_table(bytes, *b"IVRS")?;
    let limit = header.length as usize;
    let first = SDT_HEADER_SIZE + 12;
    if limit < first {
        return Err(AcpiError::InvalidLength);
    }
    let ivinfo = u32_at(bytes, SDT_HEADER_SIZE);
    let mut summary = IvrsSummary {
        hardware_blocks: 0,
        memory_reservations: 0,
        unsupported_blocks: 0,
        first_iommu_base: None,
        first_pci_segment: None,
        first_device_id: None,
        dma_remap_reported: ivinfo & (1 << 1) != 0,
    };
    let mut cursor = first;
    let mut blocks = 0;
    while cursor < limit {
        if limit - cursor < 4 || blocks == MAX_IVRS_BLOCKS {
            return Err(AcpiError::InvalidLength);
        }
        let kind = bytes[cursor];
        let size = u16_at(bytes, cursor + 2) as usize;
        if size < 4 || size > limit - cursor {
            return Err(AcpiError::InvalidLength);
        }
        let block = &bytes[cursor..cursor + size];
        match kind {
            0x10 | 0x11 | 0x40 => {
                let minimum = if kind == 0x10 { 24 } else { 40 };
                if size < minimum {
                    return Err(AcpiError::InvalidLength);
                }
                let base = u64_at(block, 8);
                if base == 0 || base & 0xfff != 0 {
                    return Err(AcpiError::InvalidLength);
                }
                summary.hardware_blocks += 1;
                if summary.first_iommu_base.is_none() {
                    summary.first_iommu_base = Some(base);
                    summary.first_device_id = Some(u16_at(block, 4));
                    summary.first_pci_segment = Some(u16_at(block, 16));
                }
            }
            0x20 | 0x21 | 0x22 => {
                if size < 32 {
                    return Err(AcpiError::InvalidLength);
                }
                summary.memory_reservations += 1;
            }
            _ => summary.unsupported_blocks += 1,
        }
        blocks += 1;
        cursor += size;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checksum;

    fn seal(table: &mut [u8]) {
        table[9] = 0;
        table[9] = 0_u8.wrapping_sub(checksum(table));
    }
    fn ivrs() -> [u8; SDT_HEADER_SIZE + 12 + 24] {
        let mut t = [0u8; SDT_HEADER_SIZE + 12 + 24];
        t[..4].copy_from_slice(b"IVRS");
        let size = t.len() as u32;
        t[4..8].copy_from_slice(&size.to_le_bytes());
        t[36..40].copy_from_slice(&2u32.to_le_bytes());
        let b = 48;
        t[b] = 0x10;
        t[b + 2..b + 4].copy_from_slice(&24u16.to_le_bytes());
        t[b + 4..b + 6].copy_from_slice(&0x120u16.to_le_bytes());
        t[b + 8..b + 16].copy_from_slice(&0xfed8_0000u64.to_le_bytes());
        seal(&mut t);
        t
    }
    #[test]
    fn reads_valid_ivhd_without_claiming_enabled_iommu() {
        let t = ivrs();
        let x = parse_ivrs(&t).unwrap();
        assert_eq!(x.hardware_blocks, 1);
        assert_eq!(x.first_iommu_base, Some(0xfed8_0000));
        assert_eq!(x.first_device_id, Some(0x120));
        assert!(x.dma_remap_reported);
    }
    #[test]
    fn rejects_bad_lengths_and_checksum() {
        let mut t = ivrs();
        t[50..52].copy_from_slice(&0u16.to_le_bytes());
        seal(&mut t);
        assert_eq!(parse_ivrs(&t), Err(AcpiError::InvalidLength));
        let mut t = ivrs();
        t[55] ^= 1;
        assert_eq!(parse_ivrs(&t), Err(AcpiError::InvalidChecksum));
    }
    #[test]
    fn unknown_blocks_do_not_grant_hardware_capabilities() {
        let mut t = ivrs();
        t[48] = 0xee;
        seal(&mut t);
        let x = parse_ivrs(&t).unwrap();
        assert_eq!(x.hardware_blocks, 0);
        assert_eq!(x.unsupported_blocks, 1);
    }
}
