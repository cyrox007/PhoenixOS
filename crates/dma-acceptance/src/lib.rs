#![no_std]

//! Отдельный, не имеющий доступа к оборудованию, профиль подготовки DMA.
//! Он не содержит ни обращения к портам, ни MMIO, ни драйвера, ни разрешения
//! запуска на физическом ASUS. Проверки предназначены для QEMU и аудита.

use phoenix_vm::hardware_gate::{DmaSafetyEvidence, blocking_reason};

const PAGE_SIZE: u64 = 4096;
const MAX_BUFFER: u64 = 1024 * 1024;
const MIN_TIMEOUT_TICKS: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub length: u64,
}

impl Range {
    pub const fn end(self) -> Option<u64> {
        self.start.checked_add(self.length)
    }

    pub fn contains(self, other: Range) -> bool {
        let (Some(end), Some(other_end)) = (self.end(), other.end()) else {
            return false;
        };
        self.length > 0 && other.length > 0 && self.start <= other.start && other_end <= end
    }

    pub fn overlaps(self, other: Range) -> bool {
        let (Some(end), Some(other_end)) = (self.end(), other.end()) else {
            return true; // При неверной адресации нельзя утверждать отсутствие пересечения.
        };
        self.start < other_end && other.start < end
    }

    pub fn aligned(self) -> bool {
        self.length > 0
            && self.start % PAGE_SIZE == 0
            && self.length % PAGE_SIZE == 0
            && self.end().is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciIdentity {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor: u16,
    pub product: u16,
}

impl PciIdentity {
    pub fn valid(self) -> bool {
        self.device < 32 && self.function < 8 && self.vendor != 0xffff && self.vendor != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    PhysicalExecutionForbidden,
    MissingEvidence,
    NoDevice,
    WrongDevice,
    InvalidBuffer,
    BufferOutsideDomain,
    ProtectedStorageOverlap,
    InvalidTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Единственный исполнительный вариант. Никаких реальных регистров.
    QemuModel,
    /// Запрещён до появления независимой проверенной реализации IOMMU.
    PhysicalAsus,
}

pub struct AcceptancePlan {
    pub profile: Profile,
    pub expected_device: Option<PciIdentity>,
    pub observed_device: Option<PciIdentity>,
    pub owned_buffer: Range,
    pub iommu_aperture: Range,
    pub protected_storage: Range,
    pub timeout_ticks: u64,
    pub evidence: DmaSafetyEvidence,
}

/// Возвращает разрешение *лишь на модельный цикл* без аппаратного обращения.
/// Даже полные фиктивные доказательства не могут разрешить физический DMA.
pub fn validate(plan: &AcceptancePlan) -> Result<ModelPermit, Rejection> {
    if plan.profile != Profile::QemuModel {
        return Err(Rejection::PhysicalExecutionForbidden);
    }
    if blocking_reason(&plan.evidence).is_some() {
        return Err(Rejection::MissingEvidence);
    }
    let expected = plan.expected_device.ok_or(Rejection::NoDevice)?;
    let observed = plan.observed_device.ok_or(Rejection::NoDevice)?;
    if !expected.valid() || !observed.valid() || expected != observed {
        return Err(Rejection::WrongDevice);
    }
    if !plan.owned_buffer.aligned() || plan.owned_buffer.length > MAX_BUFFER {
        return Err(Rejection::InvalidBuffer);
    }
    if !plan.iommu_aperture.aligned() || !plan.iommu_aperture.contains(plan.owned_buffer) {
        return Err(Rejection::BufferOutsideDomain);
    }
    if plan.owned_buffer.overlaps(plan.protected_storage) {
        return Err(Rejection::ProtectedStorageOverlap);
    }
    if plan.timeout_ticks < MIN_TIMEOUT_TICKS {
        return Err(Rejection::InvalidTimeout);
    }
    Ok(ModelPermit {
        timeout_ticks: plan.timeout_ticks,
    })
}

/// Разрешение не экспортирует адреса или аппаратные команды.
pub struct ModelPermit {
    timeout_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Prepared,
    Running,
    Stopped,
    Unmapped,
    Released,
    Faulted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleError {
    InvalidTransition,
    Timeout,
    DeviceStillActive,
}

/// Детерминированный протокол; сам ничего не останавливает и не освобождает.
/// Подтверждение остановки обязательно приходит от внешней проверяющей стороны.
pub struct ModelCycle {
    phase: Phase,
    began_at: u64,
    timeout_ticks: u64,
}

impl ModelCycle {
    pub fn new(permit: ModelPermit) -> Self {
        Self {
            phase: Phase::Prepared,
            began_at: 0,
            timeout_ticks: permit.timeout_ticks,
        }
    }

    pub const fn phase(&self) -> Phase {
        self.phase
    }

    pub fn begin(&mut self, tick: u64) -> Result<(), CycleError> {
        if self.phase != Phase::Prepared {
            return Err(CycleError::InvalidTransition);
        }
        self.began_at = tick;
        self.phase = Phase::Running;
        Ok(())
    }

    pub fn poll(&mut self, tick: u64) -> Result<(), CycleError> {
        if self.phase != Phase::Running {
            return Err(CycleError::InvalidTransition);
        }
        if tick.wrapping_sub(self.began_at) >= self.timeout_ticks {
            self.phase = Phase::Faulted;
            return Err(CycleError::Timeout);
        }
        Ok(())
    }

    /// Только после внешнего подтверждения фактической остановки.
    pub fn confirm_stopped(&mut self, hardware_stopped: bool) -> Result<(), CycleError> {
        if self.phase != Phase::Running && self.phase != Phase::Faulted {
            return Err(CycleError::InvalidTransition);
        }
        if !hardware_stopped {
            return Err(CycleError::DeviceStillActive);
        }
        self.phase = Phase::Stopped;
        Ok(())
    }

    pub fn confirm_unmapped(&mut self, iommu_unmapped: bool) -> Result<(), CycleError> {
        if self.phase != Phase::Stopped {
            return Err(CycleError::InvalidTransition);
        }
        if !iommu_unmapped {
            return Err(CycleError::InvalidTransition);
        }
        self.phase = Phase::Unmapped;
        Ok(())
    }

    pub fn confirm_released(&mut self, pages_released: bool) -> Result<(), CycleError> {
        if self.phase != Phase::Unmapped || !pages_released {
            return Err(CycleError::InvalidTransition);
        }
        self.phase = Phase::Released;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> DmaSafetyEvidence {
        DmaSafetyEvidence {
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
        }
    }

    fn device() -> PciIdentity {
        PciIdentity {
            segment: 0,
            bus: 1,
            device: 2,
            function: 0,
            vendor: 0x1234,
            product: 0x5678,
        }
    }

    fn plan() -> AcceptancePlan {
        AcceptancePlan {
            profile: Profile::QemuModel,
            expected_device: Some(device()),
            observed_device: Some(device()),
            owned_buffer: Range {
                start: 0x4000,
                length: PAGE_SIZE,
            },
            iommu_aperture: Range {
                start: 0x4000,
                length: 0x4000,
            },
            protected_storage: Range {
                start: 0x8000,
                length: PAGE_SIZE,
            },
            timeout_ticks: 10,
            evidence: evidence(),
        }
    }

    #[test]
    fn physical_hardware_is_always_denied_even_with_all_flags_true() {
        let mut plan = plan();
        plan.profile = Profile::PhysicalAsus;
        assert!(matches!(
            validate(&plan),
            Err(Rejection::PhysicalExecutionForbidden)
        ));
    }

    #[test]
    fn missing_each_hardware_condition_fails_closed() {
        let mut plan = plan();
        for bit in 0..10 {
            plan.evidence = evidence();
            match bit {
                0 => plan.evidence.memory_map_valid = false,
                1 => plan.evidence.direct_mapping_valid = false,
                2 => plan.evidence.current_cr3_aliases_clear = false,
                3 => plan.evidence.all_address_spaces_checked = false,
                4 => plan.evidence.all_processors_checked = false,
                5 => plan.evidence.iommu_translation_active = false,
                6 => plan.evidence.device_in_isolated_domain = false,
                7 => plan.evidence.buffer_is_owned = false,
                8 => plan.evidence.rollback_verified = false,
                _ => plan.evidence.operator_approved = false,
            }
            assert!(matches!(validate(&plan), Err(Rejection::MissingEvidence)));
        }
    }

    #[test]
    fn rejects_missing_or_different_controller() {
        let mut plan = plan();
        plan.expected_device = None;
        assert!(matches!(validate(&plan), Err(Rejection::NoDevice)));
        plan.expected_device = Some(device());
        plan.observed_device = Some(PciIdentity { bus: 3, ..device() });
        assert!(matches!(validate(&plan), Err(Rejection::WrongDevice)));
    }

    #[test]
    fn blocks_unaligned_overflowing_or_oversize_ranges() {
        let mut plan = plan();
        plan.owned_buffer.start += 1;
        assert!(matches!(validate(&plan), Err(Rejection::InvalidBuffer)));
        plan = plan();
        plan.owned_buffer = Range {
            start: u64::MAX - 4095,
            length: PAGE_SIZE,
        };
        assert!(matches!(validate(&plan), Err(Rejection::InvalidBuffer)));
        plan = plan();
        plan.owned_buffer.length = MAX_BUFFER + PAGE_SIZE;
        assert!(matches!(validate(&plan), Err(Rejection::InvalidBuffer)));
    }

    #[test]
    fn blocks_cross_domain_and_protected_storage() {
        let mut plan = plan();
        plan.owned_buffer.start = 0x7000;
        plan.owned_buffer.length = 0x2000;
        assert!(matches!(
            validate(&plan),
            Err(Rejection::BufferOutsideDomain)
        ));
        plan = plan();
        plan.protected_storage.start = 0x4000;
        assert!(matches!(
            validate(&plan),
            Err(Rejection::ProtectedStorageOverlap)
        ));
    }

    #[test]
    fn refuses_zero_timeout() {
        let mut plan = plan();
        plan.timeout_ticks = 0;
        assert!(matches!(validate(&plan), Err(Rejection::InvalidTimeout)));
    }

    #[test]
    fn completes_model_lifecycle_in_strict_order() {
        let permit = validate(&plan()).unwrap();
        let mut cycle = ModelCycle::new(permit);
        assert_eq!(
            cycle.confirm_released(true),
            Err(CycleError::InvalidTransition)
        );
        cycle.begin(100).unwrap();
        assert!(cycle.poll(109).is_ok());
        assert_eq!(
            cycle.confirm_stopped(false),
            Err(CycleError::DeviceStillActive)
        );
        assert_eq!(
            cycle.confirm_unmapped(true),
            Err(CycleError::InvalidTransition)
        );
        cycle.confirm_stopped(true).unwrap();
        cycle.confirm_unmapped(true).unwrap();
        cycle.confirm_released(true).unwrap();
        assert_eq!(cycle.phase(), Phase::Released);
    }

    #[test]
    fn timeout_does_not_fake_device_stop_or_free_pages() {
        let permit = validate(&plan()).unwrap();
        let mut cycle = ModelCycle::new(permit);
        cycle.begin(u64::MAX - 3).unwrap();
        assert_eq!(cycle.poll(6), Err(CycleError::Timeout));
        assert_eq!(cycle.phase(), Phase::Faulted);
        assert_eq!(cycle.confirm_unmapped(true), Err(CycleError::InvalidTransition));
        assert_eq!(cycle.confirm_stopped(false), Err(CycleError::DeviceStillActive));
        assert_eq!(cycle.phase(), Phase::Faulted);
        cycle.confirm_stopped(true).unwrap();
        cycle.confirm_unmapped(true).unwrap();
        cycle.confirm_released(true).unwrap();
    }

    #[test]
    fn cannot_reuse_finished_cycle() {
        let permit = validate(&plan()).unwrap();
        let mut cycle = ModelCycle::new(permit);
        cycle.begin(0).unwrap();
        cycle.confirm_stopped(true).unwrap();
        cycle.confirm_unmapped(true).unwrap();
        cycle.confirm_released(true).unwrap();
        assert_eq!(cycle.begin(100), Err(CycleError::InvalidTransition));
    }
}
