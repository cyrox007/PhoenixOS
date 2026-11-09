//! Условия допуска физической DMA-передачи.
//!
//! Диагностическая загрузка намеренно не выдаёт разрешение.
//! Проверки должны выполняться заново для каждого устройства и буфера.

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
