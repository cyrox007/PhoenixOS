//! Строгая сверка диапазонов PCI BDF из блоков AMD IVHD.
//! Не создаёт домен, не читает MMIO и не выдаёт разрешение DMA.

use crate::ivrs::parse_ivrs;
use crate::{AcpiError, SDT_HEADER_SIZE};

const HEADER: usize = SDT_HEADER_SIZE + 12;
pub const MAX_IVHD_SCOPES: usize = 256;
const MAX_BLOCKS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IvhdDeviceRange {
    pub segment: u16,
    pub first: u16,
    pub last: u16,
    pub iommu_base: u64,
    /// ID источника для проходящего через мост DMA (проверяется отдельно).
    pub alias: Option<u16>,
}

impl IvhdDeviceRange {
    const EMPTY: Self = Self {
        segment: 0,
        first: 0,
        last: 0,
        iommu_base: 0,
        alias: None,
    };

    fn matches(self, segment: u16, bdf: u16) -> bool {
        self.segment == segment && self.first <= bdf && bdf <= self.last
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeError {
    NotMapped,
    Ambiguous,
    IncompleteTopology,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IommuScope {
    pub base: u64,
    pub alias: Option<u16>,
    /// Функция лишь сверяет ACPI-описание; фактический домен не подтверждён.
    pub domain_verified: bool,
}

#[derive(Debug, Clone)]
pub struct IvhdScopes {
    ranges: [IvhdDeviceRange; MAX_IVHD_SCOPES],
    count: usize,
    pub unknown_entries: u32,
    pub unsupported_blocks: u16,
    pub hardware_blocks: u16,
}

impl IvhdScopes {
    pub fn ranges_count(&self) -> usize {
        self.count
    }

    pub fn query(&self, segment: u16, bdf: u16) -> Result<IommuScope, ScopeError> {
        if self.unknown_entries > 0 || self.unsupported_blocks > 0 {
            return Err(ScopeError::IncompleteTopology);
        }

        let mut result: Option<IvhdDeviceRange> = None;
        for &range in &self.ranges[..self.count] {
            if !range.matches(segment, bdf) {
                continue;
            }
            if let Some(previous) = result {
                if previous.iommu_base != range.iommu_base || previous.alias != range.alias {
                    return Err(ScopeError::Ambiguous);
                }
            }
            result = Some(range);
        }
        let matching = result.ok_or(ScopeError::NotMapped)?;
        Ok(IommuScope {
            base: matching.iommu_base,
            alias: matching.alias,
            domain_verified: false,
        })
    }

    fn insert(&mut self, range: IvhdDeviceRange) -> Result<(), AcpiError> {
        if self.count == MAX_IVHD_SCOPES {
            return Err(AcpiError::TooManyEntries);
        }
        self.ranges[self.count] = range;
        self.count += 1;
        Ok(())
    }
}

fn word(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn qword(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[derive(Clone, Copy)]
struct Pending {
    start: u16,
    alias: Option<u16>,
}

fn parse_ivhd_entries(
    bytes: &[u8],
    header_length: usize,
    segment: u16,
    base: u64,
    out: &mut IvhdScopes,
) -> Result<(), AcpiError> {
    let mut pos = header_length;
    let mut pending: Option<Pending> = None;
    while pos < bytes.len() {
        let kind = bytes[pos];
        let size = if kind == 0xf0 {
            if bytes.len() - pos < 22 {
                return Err(AcpiError::InvalidLength);
            }
            22 + usize::from(bytes[pos + 21])
        } else {
            4usize << (kind >> 6)
        };
        if size > bytes.len() - pos {
            return Err(AcpiError::InvalidLength);
        }
        let entry = &bytes[pos..pos + size];
        let bdf = word(entry, 1);
        match kind {
            0x01 => {
                if pending.is_some() {
                    return Err(AcpiError::InvalidLength);
                }
                out.insert(IvhdDeviceRange {
                    segment,
                    first: 0,
                    last: u16::MAX,
                    iommu_base: base,
                    alias: None,
                })?;
            }
            0x02 | 0x46 => {
                if pending.is_some() {
                    return Err(AcpiError::InvalidLength);
                }
                out.insert(IvhdDeviceRange {
                    segment,
                    first: bdf,
                    last: bdf,
                    iommu_base: base,
                    alias: None,
                })?;
            }
            0x42 => {
                if pending.is_some() {
                    return Err(AcpiError::InvalidLength);
                }
                out.insert(IvhdDeviceRange {
                    segment,
                    first: bdf,
                    last: bdf,
                    iommu_base: base,
                    alias: Some(word(entry, 5)),
                })?;
            }
            0x03 | 0x43 | 0x47 => {
                if pending.is_some() {
                    return Err(AcpiError::InvalidLength);
                }
                pending = Some(Pending {
                    start: bdf,
                    alias: if kind == 0x43 {
                        Some(word(entry, 5))
                    } else {
                        None
                    },
                });
            }
            0x04 => {
                let first = pending.take().ok_or(AcpiError::InvalidLength)?;
                if bdf < first.start {
                    return Err(AcpiError::InvalidLength);
                }
                out.insert(IvhdDeviceRange {
                    segment,
                    first: first.start,
                    last: bdf,
                    iommu_base: base,
                    alias: first.alias,
                })?;
            }
            _ => out.unknown_entries += 1,
        }
        pos += size;
    }
    if pending.is_some() {
        return Err(AcpiError::InvalidLength);
    }
    Ok(())
}

/// Сохраняет только известные записи и запрещает подтверждать неполную
/// топологию. Присутствие IVHD не доказывает исправность IOMMU и его домена.
pub fn parse_ivhd_scopes(bytes: &[u8]) -> Result<IvhdScopes, AcpiError> {
    let summary = parse_ivrs(bytes)?;
    let mut out = IvhdScopes {
        ranges: [IvhdDeviceRange::EMPTY; MAX_IVHD_SCOPES],
        count: 0,
        unknown_entries: 0,
        unsupported_blocks: summary.unsupported_blocks,
        hardware_blocks: summary.hardware_blocks,
    };
    let mut position = HEADER;
    let size = crate::SdtHeader::validate_table(bytes, *b"IVRS")?.length as usize;
    let mut blocks = 0;
    while position < size {
        if blocks == MAX_BLOCKS || size - position < 4 {
            return Err(AcpiError::InvalidLength);
        }
        let kind = bytes[position];
        let length = word(bytes, position + 2) as usize;
        if length < 4 || length > size - position {
            return Err(AcpiError::InvalidLength);
        }
        if matches!(kind, 0x10 | 0x11 | 0x40) {
            let header_length = if kind == 0x10 { 24 } else { 40 };
            if length < header_length {
                return Err(AcpiError::InvalidLength);
            }
            let block = &bytes[position..position + length];
            parse_ivhd_entries(
                block,
                header_length,
                word(block, 16),
                qword(block, 8),
                &mut out,
            )?;
        }
        position += length;
        blocks += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::checksum;

    fn fixture(items: &[(u64, u16, &[u8])]) -> std::vec::Vec<u8> {
        let mut buffer = std::vec![0u8; HEADER];
        buffer[..4].copy_from_slice(b"IVRS");
        for (base, segment, entries) in items {
            let mut header = [0u8; 24];
            header[0] = 0x10;
            let length = (24 + entries.len()) as u16;
            header[2..4].copy_from_slice(&length.to_le_bytes());
            header[8..16].copy_from_slice(&base.to_le_bytes());
            header[16..18].copy_from_slice(&segment.to_le_bytes());
            buffer.extend_from_slice(&header);
            buffer.extend_from_slice(entries);
        }
        let length = buffer.len() as u32;
        buffer[4..8].copy_from_slice(&length.to_le_bytes());
        buffer[9] = 0u8.wrapping_sub(checksum(&buffer));
        buffer
    }

    fn select(bdf: u16) -> [u8; 4] {
        [0x02, bdf as u8, (bdf >> 8) as u8, 0]
    }

    #[test]
    fn selected_device_is_recorded_but_domain_never_verified() {
        let ivrs = fixture(&[(0xfed8_0000, 0, &select(0x0100))]);
        let map = parse_ivhd_scopes(&ivrs).unwrap();
        assert_eq!(map.ranges_count(), 1);
        let record = map.query(0, 0x0100).unwrap();
        assert_eq!(record.base, 0xfed8_0000);
        assert_eq!(record.alias, None);
        assert!(!record.domain_verified);
        assert_eq!(map.query(0, 0x0101), Err(ScopeError::NotMapped));
    }

    #[test]
    fn inclusive_range_and_segment_are_checked() {
        let entries = [0x03, 0, 1, 0, 0x04, 7, 1, 0];
        let ivrs = fixture(&[(0xfed8_0000, 2, &entries)]);
        let map = parse_ivhd_scopes(&ivrs).unwrap();
        assert!(map.query(2, 0x0100).is_ok());
        assert!(map.query(2, 0x0107).is_ok());
        assert_eq!(map.query(2, 0x0108), Err(ScopeError::NotMapped));
        assert_eq!(map.query(0, 0x0100), Err(ScopeError::NotMapped));
    }

    #[test]
    fn incompatible_alias_and_duplicate_controllers_fail() {
        let a = select(0x0110);
        let b = select(0x0110);
        let ivrs = fixture(&[(0xfed8_0000, 0, &a), (0xfed9_0000, 0, &b)]);
        assert_eq!(
            parse_ivhd_scopes(&ivrs).unwrap().query(0, 0x0110),
            Err(ScopeError::Ambiguous)
        );
        let alias = [0x42, 0x10, 0x01, 0, 0, 0x20, 0x01, 0];
        let ivrs = fixture(&[(0xfed8_0000, 0, &alias)]);
        assert_eq!(
            parse_ivhd_scopes(&ivrs)
                .unwrap()
                .query(0, 0x0110)
                .unwrap()
                .alias,
            Some(0x0120)
        );
    }

    #[test]
    fn all_devices_are_reported_but_never_authorized() {
        let ivrs = fixture(&[(0xfed8_0000, 0, &[1, 0, 0, 0])]);
        let record = parse_ivhd_scopes(&ivrs).unwrap().query(0, 0xffff).unwrap();
        assert!(!record.domain_verified);
    }

    #[test]
    fn unknown_and_unterminated_entries_fail_closed() {
        let unknown = fixture(&[(0xfed8_0000, 0, &[0x05, 0, 0, 0])]);
        assert_eq!(
            parse_ivhd_scopes(&unknown).unwrap().query(0, 0),
            Err(ScopeError::IncompleteTopology)
        );
        let unterminated = fixture(&[(0xfed8_0000, 0, &[0x03, 0, 0, 0])]);
        assert_eq!(
            parse_ivhd_scopes(&unterminated).unwrap_err(),
            AcpiError::InvalidLength
        );
        let truncated_alias = fixture(&[(0xfed8_0000, 0, &[0x42, 0, 0, 0])]);
        assert_eq!(
            parse_ivhd_scopes(&truncated_alias).unwrap_err(),
            AcpiError::InvalidLength
        );
    }

    #[test]
    fn reversed_range_and_stray_end_are_invalid() {
        let bad = [0x03, 9, 0, 0, 0x04, 1, 0, 0];
        assert_eq!(
            parse_ivhd_scopes(&fixture(&[(0xfed8_0000, 0, &bad)])).unwrap_err(),
            AcpiError::InvalidLength
        );
        let end = [0x04, 5, 0, 0];
        assert_eq!(
            parse_ivhd_scopes(&fixture(&[(0xfed8_0000, 0, &end)])).unwrap_err(),
            AcpiError::InvalidLength
        );
    }
}
