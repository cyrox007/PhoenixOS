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
    /// Диапазоны IVMD, которые нельзя использовать для будущего DMA-буфера.
    pub protected: [(u64, u64); MAX_IVRS_BLOCKS],
    pub protected_count: usize,
    /// Несколько разных адресов IOMMU запрещают чтение первого наугад.
    pub ambiguous_iommu_base: bool,
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
        protected: [(0, 0); MAX_IVRS_BLOCKS],
        protected_count: 0,
        ambiguous_iommu_base: false,
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
                if summary
                    .first_iommu_base
                    .is_some_and(|previous| previous != base)
                {
                    summary.ambiguous_iommu_base = true;
                }
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
                // Поля IVMD: start_address=16, memory_length=24.
                // Консервативно защищаем любые IVMD, независимо от флагов.
                let start = u64_at(block, 16);
                let length = u64_at(block, 24);
                let end = start.checked_add(length).ok_or(AcpiError::InvalidLength)?;
                if length == 0 || start & 4095 != 0 || length & 4095 != 0 {
                    return Err(AcpiError::InvalidLength);
                }
                summary.protected[summary.protected_count] = (start, end);
                summary.protected_count += 1;
                summary.memory_reservations += 1;
            }
            _ => summary.unsupported_blocks += 1,
        }
        blocks += 1;
        cursor += size;
    }
    if summary.ambiguous_iommu_base {
        summary.first_iommu_base = None;
    }
    Ok(summary)
}

impl IvrsSummary {
    /// Проверка пересечения с IVMD НЕ является разрешением DMA.
    pub fn conflicts_with_protected_memory(&self, start: u64, length: u64) -> bool {
        let Some(end) = start.checked_add(length) else {
            return true;
        };
        if length == 0 {
            return true;
        }
        self.protected[..self.protected_count]
            .iter()
            .any(|&(first, last)| start < last && first < end)
    }

    /// Сопоставление PCI-функции с отдельным IOMMU-доменом не реализовано.
    pub const fn device_scope_verified(&self) -> bool {
        false
    }
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

    #[test]
    fn ivmd_ranges_are_recorded_and_cannot_be_used_as_dma_buffers() {
        let mut t = ivrs().to_vec();
        let mut m = [0u8; 32];
        m[0] = 0x20;
        m[2..4].copy_from_slice(&32u16.to_le_bytes());
        m[16..24].copy_from_slice(&0x3000u64.to_le_bytes());
        m[24..32].copy_from_slice(&0x2000u64.to_le_bytes());
        t.extend_from_slice(&m);
        let length = t.len() as u32;
        t[4..8].copy_from_slice(&length.to_le_bytes());
        seal(&mut t);
        let parsed = parse_ivrs(&t).unwrap();
        assert_eq!(parsed.protected_count, 1);
        assert_eq!(parsed.protected[0], (0x3000, 0x5000));
        assert!(parsed.conflicts_with_protected_memory(0x4000, 4096));
        assert!(!parsed.conflicts_with_protected_memory(0x5000, 4096));
        assert!(parsed.conflicts_with_protected_memory(u64::MAX, 4096));
        assert!(!parsed.device_scope_verified());
    }

    #[test]
    fn invalid_ivmd_sizes_fail_closed() {
        for size in [0u64, 1u64, 4097u64] {
            let mut t = ivrs().to_vec();
            let mut m = [0u8; 32];
            m[0] = 0x22;
            m[2..4].copy_from_slice(&32u16.to_le_bytes());
            m[16..24].copy_from_slice(&0x3000u64.to_le_bytes());
            m[24..32].copy_from_slice(&size.to_le_bytes());
            t.extend_from_slice(&m);
            let length = t.len() as u32;
            t[4..8].copy_from_slice(&length.to_le_bytes());
            seal(&mut t);
            assert_eq!(parse_ivrs(&t), Err(AcpiError::InvalidLength));
        }
    }

    #[test]
    fn multiple_distinct_iommu_bases_do_not_select_first() {
        let mut t = ivrs().to_vec();
        let mut second = [0u8; 24];
        second[0] = 0x10;
        second[2..4].copy_from_slice(&24u16.to_le_bytes());
        second[8..16].copy_from_slice(&0xfed9_0000u64.to_le_bytes());
        t.extend_from_slice(&second);
        let length = t.len() as u32;
        t[4..8].copy_from_slice(&length.to_le_bytes());
        seal(&mut t);
        let parsed = parse_ivrs(&t).unwrap();
        assert_eq!(parsed.hardware_blocks, 2);
        assert!(parsed.ambiguous_iommu_base);
        assert_eq!(parsed.first_iommu_base, None);
    }
}
