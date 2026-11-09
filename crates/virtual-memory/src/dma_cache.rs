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
const AMD_SYSCFG_TOM2_ENABLE: u64 = 1 << 21;
const AMD_SYSCFG_TOM2_FORCE_WB: u64 = 1 << 22;
const FOUR_GIB: u64 = 1 << 32;
const AMD_TOP_MEM2_ALIGNMENT: u64 = 1 << 23;
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
    amd_forced_wb_end: Option<u64>,
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
        // На реальных AMD тип по умолчанию часто UC: диапазоны RAM
        // становятся WB через переменные MTRR. Отвергать весь снимок
        // по одной только настройке UC неправильно. Но MTRR обязаны
        // быть включены, а тип/зарезервированные биты — корректны.
        if mtrr_default & MTRR_ENABLE == 0
            || mtrr_default & !(MTRR_ENABLE | MTRR_FIXED_ENABLE | 0xff) != 0
            || !valid_memory_type(mtrr_default as u8)
        {
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
            amd_forced_wb_end: None,
        })
    }

    /// Фиксированные MTRR аппаратуры для первых 1024 КиБ.
    /// Отсутствие этих данных не разрешает WB-псевдоним в этом диапазоне.
    pub fn with_fixed(mut self, fixed: [u64; FIXED_MTRR_REGISTERS]) -> Self {
        self.fixed = Some(fixed);
        self
    }

    /// Дополнительное правило AMD для 4 ГиБ..TOP_MEM2. Оно действует
    /// только при одновременной установке SYSCFG.TOM2 и SYSCFG.FWB.
    /// Не изменяет регистры, не делает неподтверждённые алиасы допустимыми.
    pub fn with_amd_top_mem2(mut self, syscfg: u64, top_mem2: u64) -> Result<Self, DmaCacheError> {
        let active = syscfg & (AMD_SYSCFG_TOM2_ENABLE | AMD_SYSCFG_TOM2_FORCE_WB)
            == AMD_SYSCFG_TOM2_ENABLE | AMD_SYSCFG_TOM2_FORCE_WB;
        if !active {
            return Ok(self);
        }

        let physical_limit = 1u64 << self.physical_bits;
        if top_mem2 <= FOUR_GIB
            || top_mem2 > physical_limit
            || top_mem2 % AMD_TOP_MEM2_ALIGNMENT != 0
        {
            return Err(DmaCacheError::InvalidMtrr);
        }

        self.amd_forced_wb_end = Some(top_mem2);
        Ok(self)
    }

    pub const fn amd_forced_wb_end(&self) -> Option<u64> {
        self.amd_forced_wb_end
    }

    pub const fn variable_count(&self) -> usize {
        self.variable_count
    }

    pub const fn physical_bits(&self) -> u8 {
        self.physical_bits
    }

    pub const fn default_memory_type(&self) -> u8 {
        self.mtrr_default as u8
    }

    pub fn pat_memory_type(&self, index: u8) -> Option<u8> {
        pat_type(self.pat, index)
    }

    /// Итоговый тип MTRR для физической страницы. Фиксированные MTRR
    /// имеют приоритет на первом мегабайте. При отсутствии совпадений
    /// применяем тип по умолчанию, а не автоматически WB.
    /// Не учитывает PAT и права виртуальных отображений; для создания
    /// WB-алиаса необходима отдельная проверка verify_writeback_alias.
    pub fn effective_mtrr_type(&self, physical_page: u64) -> Result<u8, DmaCacheError> {
        let physical_limit = 1u64 << self.physical_bits;
        if physical_page % PAGE_SIZE != 0 || physical_page >= physical_limit {
            return Err(DmaCacheError::InvalidRange);
        }

        if physical_page < 0x10_0000 && self.mtrr_default & MTRR_FIXED_ENABLE != 0 {
            let fixed = self.fixed.ok_or(DmaCacheError::UnsupportedFixedRange)?;
            let (register, slot) = fixed_entry(physical_page);
            let kind = ((fixed[register] >> (slot * 8)) & 0xff) as u8;
            // Расширенные кодировки фиксированных AMD MTRR здесь
            // намеренно не разрешены: нужен отдельный анализ SYSCFG.
            if !valid_memory_type(kind) {
                return Err(DmaCacheError::UnknownMemoryType);
            }
            return Ok(kind);
        }

        let physical_mask = (physical_limit - 1) & !(PAGE_SIZE - 1);
        let mut matched_types = 0u8;
        for &(base, mask) in &self.variable[..self.variable_count] {
            let Some((start, end, kind)) = decode_variable_region(base, mask, physical_mask)?
            else {
                continue;
            };
            if physical_page < start || physical_page >= end {
                continue;
            }
            matched_types |= 1u8 << kind;
        }

        let mut fallback = self.default_memory_type();
        if let Some(top_mem2) = self.amd_forced_wb_end {
            if physical_page >= FOUR_GIB && physical_page < top_mem2 {
                fallback = PAT_WRITE_BACK;
            }
        }
        effective_variable_type(matched_types, fallback)
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
        if pat_type(self.pat, existing_pat_index) != Some(PAT_WRITE_BACK) {
            return Err(DmaCacheError::ConflictingAlias);
        }

        // Проверять только одну точку в диапазоне недостаточно:
        // граница MTRR может проходить между соседними страницами.
        // Разрешаем WB-алиас только если КАЖДАЯ страница — MTRR WB.
        let mut page = physical_start;
        while page < end {
            if self.effective_mtrr_type(page)? != PAT_WRITE_BACK {
                return Err(DmaCacheError::UnknownMemoryType);
            }
            page += PAGE_SIZE;
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

fn valid_memory_type(kind: u8) -> bool {
    matches!(kind, 0 | 1 | 4 | 5 | 6)
}

/// Приоритет UC действует для всех совпавших диапазонов, независимо
/// от порядка регистров. WT+WB даёт WT. Иные сочетания неопределённы.
fn effective_variable_type(types: u8, default: u8) -> Result<u8, DmaCacheError> {
    if types == 0 {
        return Ok(default);
    }
    if types & (1 << PAT_UNCACHEABLE) != 0 {
        return Ok(PAT_UNCACHEABLE);
    }
    if types.count_ones() == 1 {
        return Ok(types.trailing_zeros() as u8);
    }
    if types == ((1u8 << 4) | (1u8 << PAT_WRITE_BACK)) {
        return Ok(4);
    }
    Err(DmaCacheError::UnknownMemoryType)
}

pub fn memory_type_name(kind: u8) -> &'static str {
    match kind {
        0 => "UC",
        1 => "WC",
        4 => "WT",
        5 => "WP",
        6 => "WB",
        _ => "unknown",
    }
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
    if raw_base & !(physical_mask | 0xff) != 0
        || raw_mask & !(physical_mask | MTRR_RANGE_VALID) != 0
    {
        return Err(DmaCacheError::InvalidMtrr);
    }
    let mask = raw_mask & physical_mask;
    let size = (!mask & physical_mask) + PAGE_SIZE;
    if !size.is_power_of_two() || mask != (physical_mask & !(size - 1)) {
        return Err(DmaCacheError::InvalidMtrr);
    }
    let start = raw_base & physical_mask;
    let end = start.checked_add(size).ok_or(DmaCacheError::InvalidMtrr)?;
    let kind = (raw_base & 0xff) as u8;
    if start & (size - 1) != 0 || end > physical_mask + PAGE_SIZE || !valid_memory_type(kind) {
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
        let uncacheable = snapshot(0x800, &[]).unwrap();
        assert_eq!(uncacheable.effective_mtrr_type(0x20_0000), Ok(0));
        assert_eq!(
            uncacheable.verify_writeback_alias(0x20_0000, 1, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );
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
    fn amd_default_uncacheable_accepts_explicit_writeback_ram_only() {
        let state = snapshot(0xc00, &[region(0x20_0000, 0x20_0000, 6)]).unwrap();
        assert_eq!(state.default_memory_type(), 0);
        assert_eq!(state.effective_mtrr_type(0x20_0000), Ok(6));
        assert_eq!(state.effective_mtrr_type(0x10_0000), Ok(0));
        assert_eq!(state.effective_mtrr_type(0x40_0000), Ok(0));
        state.verify_writeback_alias(0x20_0000, 2, 0).unwrap();
        assert_eq!(
            state.verify_writeback_alias(0x1f_f000, 2, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );
        assert_eq!(
            state.verify_writeback_alias(0x3f_f000, 2, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );
        assert_eq!(
            state.verify_writeback_alias(0x50_0000, 1, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );
    }

    #[test]
    fn mtrr_overlap_priority_is_resolved_without_leaking_unsafe_types() {
        let outer = region(0, 0x80_0000, 6);
        let inner_wb = region(0x20_0000, 0x20_0000, 6);
        let inner_uc = region(0x20_0000, 0x20_0000, 0);
        let inner_wt = region(0x20_0000, 0x20_0000, 4);
        let inner_wc = region(0x20_0000, 0x20_0000, 1);
        let wb_wb = snapshot(0xc00, &[outer, inner_wb]).unwrap();
        assert_eq!(wb_wb.effective_mtrr_type(0x20_0000), Ok(6));
        wb_wb.verify_writeback_alias(0x20_0000, 1, 0).unwrap();

        let wb_uc = snapshot(0xc00, &[outer, inner_uc]).unwrap();
        assert_eq!(wb_uc.effective_mtrr_type(0x20_0000), Ok(0));
        assert_eq!(
            wb_uc.verify_writeback_alias(0x20_0000, 1, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );
        let wb_wt = snapshot(0xc00, &[outer, inner_wt]).unwrap();
        assert_eq!(wb_wt.effective_mtrr_type(0x20_0000), Ok(4));
        let wb_wc = snapshot(0xc00, &[outer, inner_wc]).unwrap();
        assert_eq!(
            wb_wc.effective_mtrr_type(0x20_0000),
            Err(DmaCacheError::UnknownMemoryType)
        );

        let uc_wc = snapshot(0xc00, &[inner_uc, inner_wc]).unwrap();
        assert_eq!(uc_wc.effective_mtrr_type(0x20_0000), Ok(0));
        let wb_wc_uc = snapshot(0xc00, &[outer, inner_wc, inner_uc]).unwrap();
        assert_eq!(wb_wc_uc.effective_mtrr_type(0x20_0000), Ok(0));
    }

    #[test]
    fn fixed_mtrr_overrides_variable_ranges_with_uc_default() {
        let all_wb = [0x0606_0606_0606_0606; FIXED_MTRR_REGISTERS];
        let state = snapshot(0xc00, &[region(0, 0x20_0000, 0)])
            .unwrap()
            .with_fixed(all_wb);
        assert_eq!(state.effective_mtrr_type(0x8_0000), Ok(6));
        state.verify_writeback_alias(0x8_0000, 1, 0).unwrap();
        assert_eq!(state.effective_mtrr_type(0x10_0000), Ok(0));
        assert_eq!(
            state.verify_writeback_alias(0x10_0000, 1, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );

        let missing = snapshot(0xc00, &[]).unwrap();
        assert_eq!(
            missing.effective_mtrr_type(0x8_0000),
            Err(DmaCacheError::UnsupportedFixedRange)
        );
    }

    #[test]
    fn invalid_mtrr_registers_do_not_obtain_permissions() {
        assert!(matches!(
            snapshot(0xc02, &[]),
            Err(DmaCacheError::InvalidMtrr)
        ));
        assert!(matches!(
            snapshot(0xc00 | (1 << 12), &[]),
            Err(DmaCacheError::InvalidMtrr)
        ));
        let unaligned = snapshot(0xc00, &[(0x30_0000 | 6, region(0, 0x20_0000, 6).1)]).unwrap();
        assert_eq!(
            unaligned.effective_mtrr_type(0x20_0000),
            Err(DmaCacheError::InvalidMtrr)
        );
        let reserved = snapshot(0xc00, &[(6 | (1u64 << 58), MTRR_RANGE_VALID)]).unwrap();
        assert_eq!(
            reserved.effective_mtrr_type(0x20_0000),
            Err(DmaCacheError::InvalidMtrr)
        );
        assert_eq!(
            snapshot(0xc00, &[]).unwrap().effective_mtrr_type(0x20_0001),
            Err(DmaCacheError::InvalidRange)
        );
    }

    #[test]
    fn amd_tom2_force_wb_only_inside_enabled_high_memory_window() {
        let enabled = (1 << 21) | (1 << 22);
        let top = 0x2_0000_0000;
        let state = snapshot(0xc00, &[])
            .unwrap()
            .with_amd_top_mem2(enabled, top)
            .unwrap();

        assert_eq!(state.default_memory_type(), 0);
        assert_eq!(state.amd_forced_wb_end(), Some(top));
        assert_eq!(state.effective_mtrr_type(FOUR_GIB - PAGE_SIZE), Ok(0));
        assert_eq!(state.effective_mtrr_type(FOUR_GIB), Ok(6));
        assert_eq!(state.effective_mtrr_type(top - PAGE_SIZE), Ok(6));
        assert_eq!(state.effective_mtrr_type(top), Ok(0));
        state.verify_writeback_alias(FOUR_GIB, 1, 0).unwrap();
        assert_eq!(
            state.verify_writeback_alias(top - PAGE_SIZE, 2, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );
        assert_eq!(
            state.verify_writeback_alias(FOUR_GIB, 1, 3),
            Err(DmaCacheError::ConflictingAlias)
        );
    }

    #[test]
    fn amd_tom2_disabled_or_invalid_must_not_create_writeback_permissions() {
        let top = 0x2_0000_0000;
        for syscfg in [0, 1 << 21, 1 << 22] {
            let state = snapshot(0xc00, &[])
                .unwrap()
                .with_amd_top_mem2(syscfg, top)
                .unwrap();
            assert_eq!(state.amd_forced_wb_end(), None);
            assert_eq!(state.effective_mtrr_type(FOUR_GIB), Ok(0));
        }
        for bad_top in [FOUR_GIB, FOUR_GIB + PAGE_SIZE, 1u64 << 48] {
            assert!(matches!(
                snapshot(0xc00, &[])
                    .unwrap()
                    .with_amd_top_mem2((1 << 21) | (1 << 22), bad_top),
                Err(DmaCacheError::InvalidMtrr)
            ));
        }
    }

    #[test]
    fn explicit_variable_mtrrs_override_amd_forced_default() {
        let base = snapshot(0xc00, &[region(FOUR_GIB, FOUR_GIB, 0)])
            .unwrap()
            .with_amd_top_mem2((1 << 21) | (1 << 22), 0x2_0000_0000)
            .unwrap();
        assert_eq!(base.effective_mtrr_type(FOUR_GIB), Ok(0));
        assert_eq!(
            base.verify_writeback_alias(FOUR_GIB, 1, 0),
            Err(DmaCacheError::UnknownMemoryType)
        );

        let write_through = snapshot(0xc00, &[region(FOUR_GIB, FOUR_GIB, 4)])
            .unwrap()
            .with_amd_top_mem2((1 << 21) | (1 << 22), 0x2_0000_0000)
            .unwrap();
        assert_eq!(write_through.effective_mtrr_type(FOUR_GIB), Ok(4));
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
