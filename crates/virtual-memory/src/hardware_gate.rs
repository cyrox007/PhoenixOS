//! Условия допуска физической DMA-передачи.
//!
//! Диагностическая загрузка намеренно не выдаёт разрешение.
//! Проверки должны выполняться заново для каждого устройства и буфера.

use crate::dma_evidence::{CoverageLedger, EvidenceError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaBlockReason {
    MemoryMap,
    DirectMapping,
    ConflictingAliases,
    OtherAddressSpaces,
    OtherProcessors,
    IommuNotConfigured,
    ControllerNotIsolated,
    BufferNotOwned,
    RollbackNotProven,
    OperatorApprovalMissing,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DmaSafetyEvidence {
    pub memory_map_valid: bool,
    pub direct_mapping_valid: bool,
    pub current_cr3_aliases_clear: bool,
    pub all_address_spaces_checked: bool,
    pub all_processors_checked: bool,
    pub iommu_translation_active: bool,
    pub device_in_isolated_domain: bool,
    pub buffer_is_owned: bool,
    pub rollback_verified: bool,
    pub operator_approved: bool,
}

/// Сверить оба требования с фактически заполненным журналом, а не с
/// предполагаемым числом ядер или одной текущей CR3.
/// При ошибке оба признака сбрасываются, включая ранее положительные.
pub fn verify_coverage(
    evidence: &mut DmaSafetyEvidence,
    ledger: &CoverageLedger,
    expected_processors: usize,
    expected_roots: &[u64],
) -> Result<(), EvidenceError> {
    evidence.all_processors_checked = false;
    evidence.all_address_spaces_checked = false;
    ledger.verify(
        expected_processors,
        expected_roots,
        crate::dma_evidence::current_mapping_epoch(),
    )?;
    evidence.all_processors_checked = true;
    evidence.all_address_spaces_checked = true;
    Ok(())
}

/// Аппаратная граница обязана пересчитать доказательства при каждом допуске.
/// Обычный blocking_reason применяется к модельным проверкам и не заменяет
/// этот метод на физическом IOMMU-контроллере.
pub fn blocking_reason_with_current_coverage(
    e: &DmaSafetyEvidence,
    ledger: &CoverageLedger,
    expected_processors: usize,
    expected_roots: &[u64],
) -> Option<DmaBlockReason> {
    if let Err(reason) = ledger.verify(
        expected_processors,
        expected_roots,
        crate::dma_evidence::current_mapping_epoch(),
    ) {
        return Some(match reason {
            EvidenceError::MissingProcessor
            | EvidenceError::DuplicateCpu
            | EvidenceError::InconsistentRegisters => DmaBlockReason::OtherProcessors,
            _ => DmaBlockReason::OtherAddressSpaces,
        });
    }
    blocking_reason(e)
}

pub fn blocking_reason(e: &DmaSafetyEvidence) -> Option<DmaBlockReason> {
    if !e.memory_map_valid {
        return Some(DmaBlockReason::MemoryMap);
    }
    if !e.direct_mapping_valid {
        return Some(DmaBlockReason::DirectMapping);
    }
    if !e.current_cr3_aliases_clear {
        return Some(DmaBlockReason::ConflictingAliases);
    }
    if !e.all_address_spaces_checked {
        return Some(DmaBlockReason::OtherAddressSpaces);
    }
    if !e.all_processors_checked {
        return Some(DmaBlockReason::OtherProcessors);
    }
    if !e.iommu_translation_active {
        return Some(DmaBlockReason::IommuNotConfigured);
    }
    if !e.device_in_isolated_domain {
        return Some(DmaBlockReason::ControllerNotIsolated);
    }
    if !e.buffer_is_owned {
        return Some(DmaBlockReason::BufferNotOwned);
    }
    if !e.rollback_verified {
        return Some(DmaBlockReason::RollbackNotProven);
    }
    if !e.operator_approved {
        return Some(DmaBlockReason::OperatorApprovalMissing);
    }
    None
}

/// Даже положительный протокол без отдельного доверенного управляющего
/// кода не переводит диагностический образ в режим DMA.
pub fn diagnostic_dma_authorized(_: &DmaSafetyEvidence) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_ledger_cannot_leave_positive_coverage_flags() {
        let mut evidence = DmaSafetyEvidence {
            all_processors_checked: true,
            all_address_spaces_checked: true,
            ..DmaSafetyEvidence::default()
        };
        let ledger = CoverageLedger::new(0);
        assert_eq!(
            verify_coverage(&mut evidence, &ledger, 12, &[0x1000]),
            Err(EvidenceError::StaleGeneration)
        );
        assert!(!evidence.all_processors_checked);
        assert!(!evidence.all_address_spaces_checked);
    }

    #[test]
    fn live_coverage_gate_rejects_stale_or_empty_cpu_and_cr3_reports() {
        let evidence = DmaSafetyEvidence {
            memory_map_valid: true,
            direct_mapping_valid: true,
            current_cr3_aliases_clear: true,
            all_address_spaces_checked: true,
            all_processors_checked: true,
            iommu_translation_active: true,
            device_in_isolated_domain: true,
            buffer_is_owned: true,
            rollback_verified: true,
            operator_approved: true,
        };
        let stale = CoverageLedger::new(0);
        assert_eq!(
            blocking_reason_with_current_coverage(&evidence, &stale, 12, &[0x1000]),
            Some(DmaBlockReason::OtherAddressSpaces)
        );
        let current = CoverageLedger::new(
            crate::dma_evidence::current_mapping_epoch().unwrap()
        );
        assert_eq!(
            blocking_reason_with_current_coverage(&evidence, &current, 12, &[0x1000]),
            Some(DmaBlockReason::OtherProcessors)
        );
    }

    #[test]
    fn empty_evidence_blocks_first_prerequisite() {
        assert_eq!(
            blocking_reason(&DmaSafetyEvidence::default()),
            Some(DmaBlockReason::MemoryMap)
        );
    }

    #[test]
    fn every_missing_evidence_blocks_hardware_dma() {
        let verified = DmaSafetyEvidence {
            memory_map_valid: true,
            direct_mapping_valid: true,
            current_cr3_aliases_clear: true,
            all_address_spaces_checked: true,
            all_processors_checked: true,
            iommu_translation_active: true,
            device_in_isolated_domain: true,
            buffer_is_owned: true,
            rollback_verified: true,
            operator_approved: true,
        };
        assert_eq!(blocking_reason(&verified), None);
        for field in 0..10 {
            let mut e = verified;
            match field {
                0 => e.memory_map_valid = false,
                1 => e.direct_mapping_valid = false,
                2 => e.current_cr3_aliases_clear = false,
                3 => e.all_address_spaces_checked = false,
                4 => e.all_processors_checked = false,
                5 => e.iommu_translation_active = false,
                6 => e.device_in_isolated_domain = false,
                7 => e.buffer_is_owned = false,
                8 => e.rollback_verified = false,
                _ => e.operator_approved = false,
            }
            assert!(blocking_reason(&e).is_some());
            assert!(!diagnostic_dma_authorized(&e));
        }
        assert!(!diagnostic_dma_authorized(&verified));
    }
}
