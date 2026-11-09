//! Проверяемая политика кэширования временных окон DMA.
//!
//! Не программирует PAT/MTRR и не заменяет аппаратную приёмку. Любая
//! неопределённость запрещает выдачу нового алиаса с типом WriteBack.

use crate::PAGE_SIZE;

const PAT_WRITE_BACK: u8 = 6;
const PAT_UNCACHEABLE: u8 = 0;
const MTRR_ENABLE: u64 = 1 << 11;
const MTRR_FIXED_ENABLE: u64 = 1 << 10;
const MTRR_RANGE_VALID: u64 = 1 << 11;
pub const MAX_VARIABLE_MTRRS: usize = 16;
pub const FIXED_MTRR_REGISTERS: usize = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaCacheError {
    UnsupportedConfiguration,
    InvalidRange,
    InvalidPat,
    InvalidMtrr,
    UnknownMemoryType,
    ConflictingAlias,
    UnsupportedFixedRange,
    InsufficientCapacity,
}

/// Снимок аппаратных регистров без изменения настроек процессора.
/// Источник MSR/CPUID должен быть привилегированным и достоверным.
#[derive(Debug, Clone, Copy)]
pub struct DmaCacheSnapshot {
    pat: u64,
    mtrr_default: u64,
    physical_bits: u8,
    variable: [(u64, u64); MAX_VARIABLE_MTRRS],
    variable_count: usize,
    fixed: Option<[u64; FIXED_MTRR_REGISTERS]>,
}

impl DmaCacheSnapshot {
    pub fn new(
        pat: u64,
        mtrr_default: u64,
        physical_bits: u8,
        variable: &[(u64, u64)],
    ) -> Result<Self, DmaCacheError> {
        if !(36..=52).contains(&physical_bits) {
            return Err(DmaCacheError::UnsupportedConfiguration);
        }
        if variable.len() > MAX_VARIABLE_MTRRS {
            return Err(DmaCacheError::InsufficientCapacity);
        }
        if pat_type(pat, 0) != Some(PAT_WRITE_BACK) || pat_type(pat, 3) != Some(PAT_UNCACHEABLE) {
            return Err(DmaCacheError::InvalidPat);
        }
        // Для нового WB-алиаса требуем включённый MTRR и известный
        // исходный тип WB. Более сложные комбинации пока отклоняются.
        if mtrr_default & MTRR_ENABLE == 0 || mtrr_default as u8 != PAT_WRITE_BACK {
            return Err(DmaCacheError::InvalidMtrr);
        }
        let mut ranges = [(0, 0); MAX_VARIABLE_MTRRS];
        ranges[..variable.len()].copy_from_slice(variable);
        Ok(Self {
            pat,
            mtrr_default,
            physical_bits,
            variable: ranges,
            variable_count: variable.len(),
            fixed: None,
        })
    }

    /// Фиксированные MTRR аппаратуры для первых 1024 КиБ.
    /// Отсутствие этих данных не разрешает WB-псевдоним в этом диапазоне.
    pub fn with_fixed(mut self, fixed: [u64; FIXED_MTRR_REGISTERS]) -> Self {
        self.fixed = Some(fixed);
        self
    }

    pub const fn variable_count(&self) -> usize {
        self.variable_count
    }

    pub const fn physical_bits(&self) -> u8 {
        self.physical_bits
    }

    /// Разрешение только двух WB-алиасов обычной RAM. UC/WC/MMIO сюда
    /// не относятся; их владельцы должны использовать отдельную политику.
    pub fn verify_writeback_alias(
        &self,
        physical_start: u64,
        page_count: u64,
        existing_pat_index: u8,
    ) -> Result<(), DmaCacheError> {
        let size = page_count
            .checked_mul(PAGE_SIZE)
            .ok_or(DmaCacheError::InvalidRange)?;
        let end = physical_start
            .checked_add(size)
            .ok_or(DmaCacheError::InvalidRange)?;
        let physical_limit = 1u64 << self.physical_bits;
        if page_count == 0 || physical_start % PAGE_SIZE != 0 || end > physical_limit {
            return Err(DmaCacheError::InvalidRange);
        }
        if self.mtrr_default & MTRR_FIXED_ENABLE != 0 {
            self.verify_fixed_writeback(physical_start, end)?;
        }
        if pat_type(self.pat, existing_pat_index) != Some(PAT_WRITE_BACK) {
            return Err(DmaCacheError::ConflictingAlias);
        }
        let physical_mask = (physical_limit - 1) & !(PAGE_SIZE - 1);
        for &(base, mask) in &self.variable[..self.variable_count] {
            let Some((region_start, region_end, kind)) =
                decode_variable_region(base, mask, physical_mask)?
            else {
                continue;
            };
            if physical_start < region_end && region_start < end && kind != PAT_WRITE_BACK {
                return Err(DmaCacheError::UnknownMemoryType);
            }
        }
        Ok(())
    }

    /// Проверяет каждую пересекаемую область фиксированного MTRR. Без
    /// доступных регистров запрос не разрешается даже при MTRR WB по умолчанию.
    fn verify_fixed_writeback(&self, start: u64, end: u64) -> Result<(), DmaCacheError> {
        if start >= 0x10_0000 {
            return Ok(());
        }
        let fixed = self.fixed.ok_or(DmaCacheError::UnsupportedFixedRange)?;
        let mut address = start;
        let upper = end.min(0x10_0000);
        while address < upper {
            let (register, slot) = fixed_entry(address);
            let kind = ((fixed[register] >> (slot * 8)) & 0xff) as u8;
            if kind != PAT_WRITE_BACK {
                return Err(DmaCacheError::UnknownMemoryType);
            }
            address += PAGE_SIZE;
        }
        Ok(())
    }
}

fn fixed_entry(address: u64) -> (usize, u64) {
    if address < 0x8_0000 {
        return (0, address / 0x1_0000);
    }
    if address < 0xa_0000 {
        return (1, (address - 0x8_0000) / 0x4000);
    }
    if address < 0xc_0000 {
        return (2, (address - 0xa_0000) / 0x4000);
    }
    let page = (address - 0xc_0000) / PAGE_SIZE;
    (3 + (page / 8) as usize, page % 8)
}

pub fn pat_type(pat: u64, index: u8) -> Option<u8> {
    if index > 7 {
        return None;
    }
    let kind = ((pat >> (u32::from(index) * 8)) & 0xff) as u8;
    match kind {
        0 | 1 | 4 | 5 | 6 | 7 => Some(kind),
        _ => None,
    }
}

fn decode_variable_region(
    raw_base: u64,
    raw_mask: u64,
    physical_mask: u64,
) -> Result<Option<(u64, u64, u8)>, DmaCacheError> {
    if raw_mask & MTRR_RANGE_VALID == 0 {
        return Ok(None);
    }
    let mask = raw_mask & physical_mask;
    let size = ((!mask & physical_mask) + PAGE_SIZE);
    if !size.is_power_of_two() || mask != (physical_mask & !(size - 1)) {
        return Err(DmaCacheError::InvalidMtrr);
    }
    let start = (raw_base & physical_mask) & mask;
    let end = start.checked_add(size).ok_or(DmaCacheError::InvalidMtrr)?;
    let kind = (raw_base & 0xff) as u8;
    if !matches!(kind, 0 | 1 | 4 | 5 | 6) {
        return Err(DmaCacheError::InvalidMtrr);
    }
    Ok(Some((start, end, kind)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_PAT: u64 = 0x0007_0406_0007_0406;

    fn snapshot(
        mtrr_default: u64,
        regions: &[(u64, u64)],
    ) -> Result<DmaCacheSnapshot, DmaCacheError> {
        DmaCacheSnapshot::new(DEFAULT_PAT, mtrr_default, 48, regions)
    }

    fn region(base: u64, size: u64, kind: u8) -> (u64, u64) {
        let physical_mask = ((1u64 << 48) - 1) & !(PAGE_SIZE - 1);
        (
            base | u64::from(kind),
            (physical_mask & !(size - 1)) | MTRR_RANGE_VALID,
        )
    }

    #[test]
    fn normal_writeback_alias_is_allowed() {
        let state = snapshot(0xc06, &[region(0, 1 << 32, 6)]).unwrap();
        state.verify_writeback_alias(0x20_0000, 2, 0).unwrap();
        assert_eq!(state.physical_bits(), 48);
        assert_eq!(state.variable_count(), 1);
    }

    #[test]
    fn uncached_direct_alias_is_rejected() {
        let state = snapshot(0xc06, &[]).unwrap();
        assert_eq!(
            state.verify_writeback_alias(0x20_0000, 1, 3),
            Err(DmaCacheError::ConflictingAlias)
        );
        assert_eq!(
            state.verify_writeback_alias(0x20_0000, 1, 8),
            Err(DmaCacheError::ConflictingAlias)
        );
    }

    #[test]
    fn mixed_mtrr_region_is_rejected() {
        let state = snapshot(0xc06, &[region(0x20_0000, 0x20_0000, 0)]).unwrap();
        assert_eq!(
            state.verify_writeback_alias(0x3f_f000, 2, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );
        state.verify_writeback_alias(0x40_0000, 1, 0).unwrap();
    }

    #[test]
    fn malformed_and_disabled_mtrr_fail_closed() {
        assert!(matches!(
            snapshot(0x406, &[]),
            Err(DmaCacheError::InvalidMtrr)
        ));
        assert!(matches!(
            snapshot(0x800, &[]),
            Err(DmaCacheError::InvalidMtrr)
        ));
        let state = snapshot(0x806, &[(0x6, MTRR_RANGE_VALID | (1u64 << 40))]).unwrap();
        assert_eq!(
            state.verify_writeback_alias(0x20_0000, 1, 0),
            Err(DmaCacheError::InvalidMtrr)
        );
    }

    #[test]
    fn fixed_mtrr_range_and_invalid_physical_addresses_are_rejected() {
        let state = snapshot(0xc06, &[]).unwrap();
        assert_eq!(
            state.verify_writeback_alias(0x80_000, 1, 0),
            Err(DmaCacheError::UnsupportedFixedRange)
        );
        assert_eq!(
            state.verify_writeback_alias(0x20_0000, 0, 0),
            Err(DmaCacheError::InvalidRange)
        );
        assert_eq!(
            state.verify_writeback_alias(0x20_0001, 1, 0),
            Err(DmaCacheError::InvalidRange)
        );
        assert_eq!(
            state.verify_writeback_alias(1u64 << 48, 1, 0),
            Err(DmaCacheError::InvalidRange)
        );
    }

    #[test]
    fn fixed_mtrr_is_verified_instead_of_assuming_writeback() {
        let all_wb = [0x0606_0606_0606_0606; FIXED_MTRR_REGISTERS];
        let state = snapshot(0xc06, &[]).unwrap().with_fixed(all_wb);
        state.verify_writeback_alias(0x2000, 2, 0).unwrap();
        state.verify_writeback_alias(0x9_f000, 2, 0).unwrap();

        let mut mixed = all_wb;
        mixed[2] = 0;
        let state = snapshot(0xc06, &[]).unwrap().with_fixed(mixed);
        assert_eq!(
            state.verify_writeback_alias(0xa_0000, 1, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );
        state.verify_writeback_alias(0x8_0000, 1, 0).unwrap();
    }

    #[test]
    fn unknown_pat_and_capacity_are_rejected() {
        assert!(matches!(
            DmaCacheSnapshot::new(0, 0x806, 48, &[]),
            Err(DmaCacheError::InvalidPat)
        ));
        assert!(matches!(
            DmaCacheSnapshot::new(DEFAULT_PAT, 0x806, 48, &[(0, 0); 17]),
            Err(DmaCacheError::InsufficientCapacity)
        ));
        assert_eq!(pat_type(DEFAULT_PAT, 3), Some(PAT_UNCACHEABLE));
        assert_eq!(pat_type(DEFAULT_PAT, 8), None);
    }
}
