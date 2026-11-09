//! Неизменяющий память аудит прямого отображения доступных страниц RAM.
//!
//! Проверяется только отображение RAM, созданное загрузчиком. Этот модуль
//! НЕ обнаруживает прочие виртуальные псевдонимы и НЕ разрешает устройство DMA.

use crate::PAGE_SIZE;
use crate::dma_cache::DmaCacheSnapshot;

/// Предельный объём одной диагностики: 16 ГиБ по 4-КиБ страницам.
/// При превышении сообщаем о неполном обследовании, а не зависаем.
pub const MAX_AUDIT_PAGES: u64 = 4_194_304;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedMapping {
    Missing,
    Mapped { physical: u64, pat_index: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamAuditError {
    InvalidRegion,
    LimitExceeded,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RamAuditReport {
    pub scanned_pages: u64,
    pub wb_candidates: u64,
    pub blocked_pages: u64,
    pub invalid_mappings: u64,
    pub first_invalid: Option<u64>,
    pub first_blocked: Option<u64>,
}

impl RamAuditReport {
    /// Проверяет каждый полный 4-КиБ кадр одного разрешённого диапазона.
    /// Все границы карты RAM должны быть выровнены. Если лимит превышен,
    /// обработчик наблюдений не вызывается и счётчики остаются прежними.
    pub fn inspect_region<F>(
        &mut self,
        snapshot: &DmaCacheSnapshot,
        start: u64,
        end: u64,
        mut observe: F,
    ) -> Result<(), RamAuditError>
    where
        F: FnMut(u64) -> ObservedMapping,
    {
        let physical_limit = 1u64 << snapshot.physical_bits();
        if start >= end || start % PAGE_SIZE != 0 || end % PAGE_SIZE != 0 || end > physical_limit {
            return Err(RamAuditError::InvalidRegion);
        }

        let pages = (end - start) / PAGE_SIZE;
        if pages > MAX_AUDIT_PAGES.saturating_sub(self.scanned_pages) {
            return Err(RamAuditError::LimitExceeded);
        }

        let mut physical = start;
        while physical < end {
            self.scanned_pages += 1;
            match observe(physical) {
                ObservedMapping::Mapped {
                    physical: translated,
                    pat_index,
                } if translated == physical && pat_index < 8 => {
                    if snapshot
                        .verify_writeback_alias(physical, 1, pat_index)
                        .is_ok()
                    {
                        self.wb_candidates += 1;
                    } else {
                        self.blocked_pages += 1;
                        if self.first_blocked.is_none() {
                            self.first_blocked = Some(physical);
                        }
                    }
                }
                _ => {
                    self.invalid_mappings += 1;
                    if self.first_invalid.is_none() {
                        self.first_invalid = Some(physical);
                    }
                }
            }
            physical += PAGE_SIZE;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dma_cache::DmaCacheSnapshot;

    const DEFAULT_PAT: u64 = 0x0007_0406_0007_0406;

    fn snapshot(default_type: u64) -> DmaCacheSnapshot {
        DmaCacheSnapshot::new(DEFAULT_PAT, default_type, 48, &[]).unwrap()
    }

    #[test]
    fn valid_direct_ram_range_is_checked_page_by_page() {
        let state = snapshot(0x806);
        let mut report = RamAuditReport::default();
        let mut visited = 0;
        report
            .inspect_region(&state, 0x20_0000, 0x20_4000, |physical| {
                visited += 1;
                ObservedMapping::Mapped {
                    physical,
                    pat_index: 0,
                }
            })
            .unwrap();

        assert_eq!(visited, 4);
        assert_eq!(report.scanned_pages, 4);
        assert_eq!(report.wb_candidates, 4);
        assert_eq!(report.blocked_pages, 0);
        assert_eq!(report.invalid_mappings, 0);
    }

    #[test]
    fn uc_or_conflicting_pat_never_becomes_writeback_candidate() {
        let state = snapshot(0x800);
        let mut report = RamAuditReport::default();
        report
            .inspect_region(&state, 0x40_0000, 0x40_2000, |physical| {
                ObservedMapping::Mapped {
                    physical,
                    pat_index: 0,
                }
            })
            .unwrap();
        assert_eq!(report.blocked_pages, 2);
        assert_eq!(report.wb_candidates, 0);
        assert_eq!(report.first_blocked, Some(0x40_0000));

        let state = snapshot(0x806);
        report
            .inspect_region(&state, 0x50_0000, 0x50_1000, |physical| {
                ObservedMapping::Mapped {
                    physical,
                    pat_index: 3,
                }
            })
            .unwrap();
        assert_eq!(report.blocked_pages, 3);
        assert_eq!(report.first_blocked, Some(0x40_0000));
    }

    #[test]
    fn wrong_physical_alias_and_missing_mapping_are_reported() {
        let state = snapshot(0x806);
        let mut report = RamAuditReport::default();
        report
            .inspect_region(&state, 0x60_0000, 0x60_3000, |physical| {
                if physical == 0x60_0000 {
                    return ObservedMapping::Mapped {
                        physical: physical + PAGE_SIZE,
                        pat_index: 0,
                    };
                }
                if physical == 0x60_1000 {
                    return ObservedMapping::Missing;
                }
                ObservedMapping::Mapped {
                    physical,
                    pat_index: 8,
                }
            })
            .unwrap();
        assert_eq!(report.invalid_mappings, 3);
        assert_eq!(report.first_invalid, Some(0x60_0000));
        assert_eq!(report.wb_candidates, 0);
    }

    #[test]
    fn malformed_ranges_and_limit_fail_without_visiting_pages() {
        let state = snapshot(0x806);
        let mut report = RamAuditReport::default();
        for (start, end) in [
            (0, 0),
            (0x1001, 0x3000),
            (0x1000, 0x3001),
            (1u64 << 48, (1u64 << 48) + PAGE_SIZE),
            (0x3000, 0x2000),
        ] {
            assert_eq!(
                report.inspect_region(&state, start, end, |_| panic!("invalid region scanned")),
                Err(RamAuditError::InvalidRegion)
            );
        }
        assert_eq!(
            report.inspect_region(&state, 0, (MAX_AUDIT_PAGES + 1) * PAGE_SIZE, |_| panic!(
                "over-limit region scanned"
            )),
            Err(RamAuditError::LimitExceeded)
        );
        assert_eq!(report.scanned_pages, 0);
    }

    #[test]
    fn exhausted_capacity_does_not_misreport_complete_coverage() {
        let state = snapshot(0x806);
        let mut report = RamAuditReport {
            scanned_pages: MAX_AUDIT_PAGES,
            ..RamAuditReport::default()
        };
        assert_eq!(
            report.inspect_region(&state, 0x2000, 0x3000, |_| panic!("capacity exhausted")),
            Err(RamAuditError::LimitExceeded)
        );
        assert_eq!(report.scanned_pages, MAX_AUDIT_PAGES);
    }
}
