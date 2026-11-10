//! Учёт изменений таблиц страниц и доказательств от каждого процессора.
//! Ни одно изменение/переключение CR3 нельзя считать проверенным автоматически.
//! Инструмент не изменяет MSR и не запускает дополнительные процессоры.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const MAX_PROCESSORS: usize = 64;
pub const MAX_ROOTS: usize = 64;
static MAPPING_EPOCH: AtomicU64 = AtomicU64::new(1);
static EPOCH_EXHAUSTED: AtomicBool = AtomicBool::new(false);

/// Выполнять ПЕРЕД любым созданием, удалением или сменой корня/отображения.
/// При переполнении эпохи доказательства безвозвратно блокируются.
pub fn invalidate_mappings() {
    if MAPPING_EPOCH
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .is_err()
    {
        EPOCH_EXHAUSTED.store(true, Ordering::Release);
    }
}

pub fn current_mapping_epoch() -> Option<u64> {
    if EPOCH_EXHAUSTED.load(Ordering::Acquire) {
        None
    } else {
        Some(MAPPING_EPOCH.load(Ordering::Acquire))
    }
}

/// Полный снимок настроек кэширования ОДНОГО реально исполняющего CPU.
/// Для сравнения необходимы фактически прочитанные на этом ядре регистры.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessorCacheRegisters {
    pub pat: u64,
    pub mtrr_cap: u64,
    pub mtrr_default: u64,
    pub variable: [(u64, u64); 16],
    pub variable_count: usize,
    pub fixed: [u64; 11],
    pub fixed_count: usize,
    pub amd_syscfg: Option<u64>,
    pub amd_top_mem2: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessorReport {
    pub logical_apic_id: u32,
    pub registers: ProcessorCacheRegisters,
    pub observed_epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressSpaceReport {
    pub cr3: u64,
    pub aliases_clear: bool,
    pub observed_epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceError {
    Capacity,
    DuplicateCpu,
    DuplicateRoot,
    InvalidRoot,
    InconsistentRegisters,
    MissingProcessor,
    MissingRoot,
    UnsafeAlias,
    StaleGeneration,
}

pub struct CoverageLedger {
    processors: [Option<ProcessorReport>; MAX_PROCESSORS],
    roots: [Option<AddressSpaceReport>; MAX_ROOTS],
    processor_count: usize,
    root_count: usize,
    generation: u64,
}

impl CoverageLedger {
    pub fn new(generation: u64) -> Self {
        Self {
            processors: [None; MAX_PROCESSORS],
            roots: [None; MAX_ROOTS],
            processor_count: 0,
            root_count: 0,
            generation,
        }
    }

    pub fn add_processor(&mut self, item: ProcessorReport) -> Result<(), EvidenceError> {
        if self.processor_count == MAX_PROCESSORS {
            return Err(EvidenceError::Capacity);
        }
        if self.processors[..self.processor_count]
            .iter()
            .flatten()
            .any(|previous| previous.logical_apic_id == item.logical_apic_id)
        {
            return Err(EvidenceError::DuplicateCpu);
        }
        if item.observed_epoch != self.generation {
            return Err(EvidenceError::StaleGeneration);
        }
        if let Some(reference) = self.processors[0] {
            if reference.registers != item.registers {
                return Err(EvidenceError::InconsistentRegisters);
            }
        }
        self.processors[self.processor_count] = Some(item);
        self.processor_count += 1;
        Ok(())
    }

    pub fn add_root(&mut self, item: AddressSpaceReport) -> Result<(), EvidenceError> {
        if self.root_count == MAX_ROOTS {
            return Err(EvidenceError::Capacity);
        }
        if item.cr3 == 0 || item.cr3 & 4095 != 0 {
            return Err(EvidenceError::InvalidRoot);
        }
        if item.observed_epoch != self.generation {
            return Err(EvidenceError::StaleGeneration);
        }
        if !item.aliases_clear {
            return Err(EvidenceError::UnsafeAlias);
        }
        if self.roots[..self.root_count]
            .iter()
            .flatten()
            .any(|previous| previous.cr3 == item.cr3)
        {
            return Err(EvidenceError::DuplicateRoot);
        }
        self.roots[self.root_count] = Some(item);
        self.root_count += 1;
        Ok(())
    }

    /// expected_* получает только доверенный реестр работающих процессоров
    /// и созданных корней. Эти числа не могут выводиться из отчёта Windows.
    pub fn verify(
        &self,
        expected_processors: usize,
        expected_roots: &[u64],
        current_epoch: Option<u64>,
    ) -> Result<(), EvidenceError> {
        if current_epoch != Some(self.generation) {
            return Err(EvidenceError::StaleGeneration);
        }
        if expected_processors == 0 || expected_processors > MAX_PROCESSORS
            || self.processor_count != expected_processors
        {
            return Err(EvidenceError::MissingProcessor);
        }
        if expected_roots.is_empty() || expected_roots.len() > MAX_ROOTS
            || self.root_count != expected_roots.len()
        {
            return Err(EvidenceError::MissingRoot);
        }
        if expected_roots.iter().any(|required| {
            !self.roots[..self.root_count]
                .iter()
                .flatten()
                .any(|found| &found.cr3 == required)
        }) {
            return Err(EvidenceError::MissingRoot);
        }
        Ok(())
    }

    pub fn processor_count(&self) -> usize {
        self.processor_count
    }

    pub fn root_count(&self) -> usize {
        self.root_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register() -> ProcessorCacheRegisters {
        ProcessorCacheRegisters {
            pat: 0x0007_0406_0007_0406,
            mtrr_cap: 8,
            mtrr_default: 0xc00,
            variable: [(0, 0); 16],
            variable_count: 8,
            fixed: [0; 11],
            fixed_count: 11,
            amd_syscfg: Some(0),
            amd_top_mem2: Some(0),
        }
    }

    fn processor(id: u32, epoch: u64) -> ProcessorReport {
        ProcessorReport { logical_apic_id: id, registers: register(), observed_epoch: epoch }
    }

    #[test]
    fn complete_consistent_synthetic_coverage_can_be_verified() {
        let mut ledger = CoverageLedger::new(11);
        for index in 0..12 {
            ledger.add_processor(processor(index, 11)).unwrap();
        }
        ledger.add_root(AddressSpaceReport { cr3: 0x1000, aliases_clear: true, observed_epoch: 11 }).unwrap();
        ledger.add_root(AddressSpaceReport { cr3: 0x2000, aliases_clear: true, observed_epoch: 11 }).unwrap();
        assert_eq!(ledger.verify(12, &[0x1000, 0x2000], Some(11)), Ok(()));
        assert_eq!(ledger.verify(12, &[0x1000, 0x2000], Some(12)), Err(EvidenceError::StaleGeneration));
    }

    #[test]
    fn missing_duplicate_or_inconsistent_cpu_is_never_counted_twice() {
        let mut ledger = CoverageLedger::new(3);
        ledger.add_processor(processor(0, 3)).unwrap();
        assert_eq!(ledger.add_processor(processor(0, 3)), Err(EvidenceError::DuplicateCpu));
        let mut other = processor(1, 3);
        other.registers.pat ^= 1;
        assert_eq!(ledger.add_processor(other), Err(EvidenceError::InconsistentRegisters));
        assert_eq!(ledger.verify(12, &[0x1000], Some(3)), Err(EvidenceError::MissingProcessor));
    }

    #[test]
    fn incomplete_or_corrupt_roots_fail() {
        let mut ledger = CoverageLedger::new(3);
        ledger.add_processor(processor(0, 3)).unwrap();
        for cr3 in [0, 0x1001] {
            assert_eq!(ledger.add_root(AddressSpaceReport { cr3, aliases_clear: true, observed_epoch: 3 }), Err(EvidenceError::InvalidRoot));
        }
        assert_eq!(ledger.add_root(AddressSpaceReport { cr3: 0x1000, aliases_clear: false, observed_epoch: 3 }), Err(EvidenceError::UnsafeAlias));
        ledger.add_root(AddressSpaceReport { cr3: 0x1000, aliases_clear: true, observed_epoch: 3 }).unwrap();
        assert_eq!(ledger.add_root(AddressSpaceReport { cr3: 0x1000, aliases_clear: true, observed_epoch: 3 }), Err(EvidenceError::DuplicateRoot));
        assert_eq!(ledger.verify(1, &[0x2000], Some(3)), Err(EvidenceError::MissingRoot));
    }

    #[test]
    fn mapping_changes_invalidate_previous_coverage() {
        let before = current_mapping_epoch().unwrap();
        invalidate_mappings();
        let after = current_mapping_epoch().unwrap();
        assert!(after > before);
        let mut ledger = CoverageLedger::new(before);
        ledger.add_processor(processor(0, before)).unwrap();
        ledger.add_root(AddressSpaceReport { cr3: 0x1000, aliases_clear: true, observed_epoch: before }).unwrap();
        assert_eq!(ledger.verify(1, &[0x1000], current_mapping_epoch()), Err(EvidenceError::StaleGeneration));
    }

    #[test]
    fn stale_observations_are_rejected_at_insertion() {
        let mut ledger = CoverageLedger::new(8);
        assert_eq!(ledger.add_processor(processor(1, 7)), Err(EvidenceError::StaleGeneration));
        assert_eq!(ledger.add_root(AddressSpaceReport { cr3: 0x1000, aliases_clear: true, observed_epoch: 9 }), Err(EvidenceError::StaleGeneration));
    }
}
