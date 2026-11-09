//! Только разбор известных регистров AMD IOMMU.
//! Чтение MMIO разрешено вызывающей стороне исключительно после проверки
//! физического диапазона, прямого отображения и явно некэшируемого типа.
//! В этой библиотеке нет ни аппаратного чтения, ни аппаратной записи.

pub const DEVICE_TABLE_OFFSET: u64 = 0x0000;
pub const CONTROL_OFFSET: u64 = 0x0018;
pub const REQUIRED_BYTES: u64 = CONTROL_OFFSET + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationError {
    InvalidBase,
    OutsideKnownMapping,
    OverlapsRam,
    TranslationMismatch,
    CacheNotUncached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOnlyIommuState {
    pub enabled_bit: bool,
    pub device_table_address: u64,
    pub table_address_nonzero: bool,
    /// Не подтверждает ни работу домена, ни права отдельного устройства.
    pub domain_verified: bool,
}

impl ReadOnlyIommuState {
    pub const fn decode(control: u64, device_table: u64) -> Self {
        let address = device_table & 0x000f_ffff_ffff_f000;
        Self {
            enabled_bit: control & 1 != 0,
            device_table_address: address,
            table_address_nonzero: address != 0,
            domain_verified: false,
        }
    }
}

/// Проверить право на *чтение* двух известных аппаратных регистров.
/// Функция сама не читает память. Типы PAT/MTRR: 0 означает UC.
/// Неизвестная или конфликтующая политика должна отклонять чтение.
pub fn validate_passive_read(
    base: u64,
    known_physical_end: u64,
    translated_base: Option<u64>,
    mtrr_type: Option<u8>,
    pat_type: Option<u8>,
    usable_ram: &[(u64, u64)],
) -> Result<(), ObservationError> {
    if base == 0 || base & 0xfff != 0 {
        return Err(ObservationError::InvalidBase);
    }
    let end = base.checked_add(REQUIRED_BYTES).ok_or(ObservationError::InvalidBase)?;
    if end > known_physical_end {
        return Err(ObservationError::OutsideKnownMapping);
    }
    if usable_ram.iter().any(|&(first, last)| base < last && first < end) {
        return Err(ObservationError::OverlapsRam);
    }
    if translated_base != Some(base) {
        return Err(ObservationError::TranslationMismatch);
    }
    if mtrr_type != Some(0) || pat_type != Some(0) {
        return Err(ObservationError::CacheNotUncached);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Result<(), ObservationError> {
        validate_passive_read(
            0xfed8_0000, 0x1_0000_0000, Some(0xfed8_0000),
            Some(0), Some(0), &[(0x10_0000, 0x8000_0000)],
        )
    }

    #[test]
    fn accepts_explicitly_uncached_non_ram_mapping_only() {
        assert_eq!(valid(), Ok(()));
    }

    #[test]
    fn rejects_bad_base_and_missing_mapping() {
        assert_eq!(
            validate_passive_read(0, u64::MAX, Some(0), Some(0), Some(0), &[]),
            Err(ObservationError::InvalidBase),
        );
        assert_eq!(
            validate_passive_read(0xfed8_0001, u64::MAX, None, Some(0), Some(0), &[]),
            Err(ObservationError::InvalidBase),
        );
        assert_eq!(
            validate_passive_read(0xfed8_0000, 0xfed8_0018, Some(0xfed8_0000), Some(0), Some(0), &[]),
            Err(ObservationError::OutsideKnownMapping),
        );
    }

    #[test]
    fn rejects_ram_and_translation_aliases() {
        assert_eq!(
            validate_passive_read(
                0xfed8_0000, u64::MAX, Some(0xfed8_0000), Some(0), Some(0),
                &[(0xfed8_0000, 0xfed8_1000)],
            ),
            Err(ObservationError::OverlapsRam),
        );
        assert_eq!(
            validate_passive_read(
                0xfed8_0000, u64::MAX, Some(0xdead_0000), Some(0), Some(0), &[],
            ),
            Err(ObservationError::TranslationMismatch),
        );
    }

    #[test]
    fn unknown_or_writeback_policy_blocks_even_reading() {
        for mtrr in [None, Some(6), Some(1)] {
            assert_eq!(
                validate_passive_read(
                    0xfed8_0000, u64::MAX, Some(0xfed8_0000), mtrr, Some(0), &[],
                ),
                Err(ObservationError::CacheNotUncached),
            );
        }
        for pat in [None, Some(6), Some(4)] {
            assert_eq!(
                validate_passive_read(
                    0xfed8_0000, u64::MAX, Some(0xfed8_0000), Some(0), pat, &[],
                ),
                Err(ObservationError::CacheNotUncached),
            );
        }
    }

    #[test]
    fn enabled_bit_never_proves_isolated_domain() {
        let state = ReadOnlyIommuState::decode(1, 0x1234_5000);
        assert!(state.enabled_bit);
        assert!(state.table_address_nonzero);
        assert_eq!(state.device_table_address, 0x1234_5000);
        assert!(!state.domain_verified);
        let off = ReadOnlyIommuState::decode(0, 0);
        assert!(!off.enabled_bit);
        assert!(!off.table_address_nonzero);
        assert!(!off.domain_verified);
    }

    #[test]
    fn address_overflow_rejected() {
        assert_eq!(
            validate_passive_read(u64::MAX - 4095, u64::MAX, None, Some(0), Some(0), &[]),
            Err(ObservationError::OutsideKnownMapping),
        );
    }
}
