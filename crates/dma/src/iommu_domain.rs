//! Проверяемый жизненный цикл изолированного домена IOMMU.
//! Сам модуль не пишет в MMIO/PCI. Реальную изоляцию обязан обеспечить
//! отдельно испытанный аппаратный исполнитель `IommuEngine`.
//! Прямое DMA (identity mapping) никогда не подразумевается автоматически.

use crate::mapping::{DmaMappingBackend, PhysicalAddress};
use crate::{DeviceAddress, DmaError, DmaLayout};

const PAGE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainFault {
    EngineUnavailable,
    DeviceNotIsolated,
    DeviceBusy,
    InstallFailed,
    CacheInvalidationFailed,
    StopFailed,
    RemovalFailed,
    MappingInconsistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotPhase {
    Free,
    Installing,
    Active,
    Removing,
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    phase: SlotPhase,
    physical: u64,
    address: u64,
    length: u64,
}

impl Entry {
    const EMPTY: Self = Self {
        phase: SlotPhase::Free,
        physical: 0,
        address: 0,
        length: 0,
    };

    fn overlaps(self, start: u64, end: u64) -> bool {
        self.phase != SlotPhase::Free
            && self.address < end
            && start < self.address.saturating_add(self.length)
    }
}

/// Драйвер реального контроллера должен иметь аппаратно подтверждённые
/// методы проверки, программирования таблиц и завершения инвалидации.
/// Простого наличия ACPI IVRS и бита IOMMUEn недостаточно.
///
/// Не реализуйте эти методы как безусловный успех на физическом устройстве.
pub trait IommuEngine {
    fn translation_active(&self) -> bool;
    fn device_in_private_domain(&self) -> bool;
    fn device_stopped(&mut self) -> Result<bool, DomainFault>;
    fn install_mapping(&mut self, address: u64, physical: u64, length: u64) -> Result<(), DomainFault>;
    fn remove_mapping(&mut self, address: u64, length: u64) -> Result<(), DomainFault>;
    /// Возвращает Ok только после фактического завершения сброса IOTLB.
    fn invalidate_and_wait(&mut self) -> Result<(), DomainFault>;
}

/// Ограниченный домен: устройство видит лишь явно назначенные IOVA.
/// Каждому слоту соответствует один непересекающийся физический диапазон.
/// Все неисправности переводят слот в карантин до явного восстановления.
/// Неявное освобождение страниц при Drop отсутствует.
pub struct IsolatedIommuDomain<E, const N: usize> {
    engine: E,
    aperture_start: u64,
    aperture_end: u64,
    slots: [Entry; N],
    fault: Option<DomainFault>,
}

impl<E: IommuEngine, const N: usize> IsolatedIommuDomain<E, N> {
    pub fn new(engine: E, start: u64, end: u64) -> Result<Self, DmaError> {
        if N == 0 || start == 0 || start % PAGE != 0 || end % PAGE != 0 || start >= end {
            return Err(DmaError::InvalidRange);
        }
        Ok(Self {
            engine,
            aperture_start: start,
            aperture_end: end,
            slots: [Entry::EMPTY; N],
            fault: None,
        })
    }

    pub fn engine(&self) -> &E {
        &self.engine
    }

    pub fn fault(&self) -> Option<DomainFault> {
        self.fault
    }

    pub fn active_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.phase == SlotPhase::Active).count()
    }

    pub fn quarantined_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.phase == SlotPhase::Quarantined).count()
    }

    fn ready(&mut self) -> Result<(), DomainFault> {
        if let Some(reason) = self.fault {
            return Err(reason);
        }
        if !self.engine.translation_active() {
            self.fault = Some(DomainFault::EngineUnavailable);
            return Err(DomainFault::EngineUnavailable);
        }
        if !self.engine.device_in_private_domain() {
            self.fault = Some(DomainFault::DeviceNotIsolated);
            return Err(DomainFault::DeviceNotIsolated);
        }
        Ok(())
    }

    fn stopped(&mut self) -> Result<(), DomainFault> {
        match self.engine.device_stopped() {
            Ok(true) => Ok(()),
            Ok(false) => Err(DomainFault::DeviceBusy),
            Err(reason) => Err(reason),
        }
    }

    fn map_internal(&mut self, physical: u64, length: u64) -> Result<DeviceAddress, DmaError> {
        if length == 0 || physical % PAGE != 0 || length % PAGE != 0 {
            return Err(DmaError::InvalidRange);
        }
        if physical.checked_add(length).is_none() {
            return Err(DmaError::AddressOverflow);
        }
        self.ready().map_err(|_| DmaError::Unaddressable)?;
        self.stopped().map_err(|_| DmaError::Unaddressable)?;

        // Распределение IOVA ограничено заданным окном, никогда не даёт
        // произвольный физический адрес или доступ к соседней странице.
        let mut candidate = self.aperture_start;
        let mut address = None;
        while let Some(end) = candidate.checked_add(length) {
            if end > self.aperture_end {
                break;
            }
            if !self.slots.iter().any(|slot| slot.overlaps(candidate, end)) {
                address = Some(candidate);
                break;
            }
            candidate = match candidate.checked_add(PAGE) {
                Some(value) => value,
                None => break,
            };
        }
        let address = address.ok_or(DmaError::Unaddressable)?;
        let index = self.slots.iter().position(|slot| slot.phase == SlotPhase::Free)
            .ok_or(DmaError::Unaddressable)?;
        self.slots[index] = Entry {
            phase: SlotPhase::Installing,
            physical,
            address,
            length,
        };

        if let Err(reason) = self.engine.install_mapping(address, physical, length) {
            self.slots[index].phase = SlotPhase::Quarantined;
            self.fault = Some(reason);
            return Err(DmaError::Unaddressable);
        }
        if let Err(reason) = self.engine.invalidate_and_wait() {
            self.slots[index].phase = SlotPhase::Quarantined;
            self.fault = Some(reason);
            return Err(DmaError::Unaddressable);
        }
        self.slots[index].phase = SlotPhase::Active;
        Ok(DeviceAddress::new(address))
    }

    fn unmap_internal(&mut self, address: u64, length: u64) -> Result<(), DmaError> {
        let index = self.slots.iter().position(|slot| {
            slot.phase == SlotPhase::Active && slot.address == address && slot.length == length
        }).ok_or(DmaError::InvalidRange)?;
        self.stopped().map_err(|_| DmaError::Unaddressable)?;
        self.slots[index].phase = SlotPhase::Removing;
        if let Err(reason) = self.engine.remove_mapping(address, length) {
            self.slots[index].phase = SlotPhase::Quarantined;
            self.fault = Some(reason);
            return Err(DmaError::Unaddressable);
        }
        if let Err(reason) = self.engine.invalidate_and_wait() {
            self.slots[index].phase = SlotPhase::Quarantined;
            self.fault = Some(reason);
            return Err(DmaError::Unaddressable);
        }
        self.slots[index] = Entry::EMPTY;
        Ok(())
    }

    /// Восстановление после неполной операции. При любом сбое слот
    /// остаётся занятым. Пользователь обязан сохранить владение страницами.
    /// Ни восстановление, ни отсутствие ошибок не разрешают физический DMA.
    pub fn recover_quarantined(&mut self) -> Result<(), DomainFault> {
        self.stopped()?;
        // Даже при fault нельзя освобождать память, пока трансляция
        // или частично созданные записи могут оставаться активными.
        for index in 0..N {
            if self.slots[index].phase != SlotPhase::Quarantined {
                continue;
            }
            let entry = self.slots[index];
            self.engine.remove_mapping(entry.address, entry.length)?;
            self.engine.invalidate_and_wait()?;
            self.slots[index] = Entry::EMPTY;
        }
        if self.quarantined_count() == 0
            && self.engine.translation_active()
            && self.engine.device_in_private_domain()
        {
            self.fault = None;
            return Ok(());
        }
        Err(DomainFault::MappingInconsistent)
    }
}

impl<E: IommuEngine, const N: usize> DmaMappingBackend for IsolatedIommuDomain<E, N> {
    fn map(
        &mut self,
        physical: PhysicalAddress,
        layout: DmaLayout,
    ) -> Result<DeviceAddress, DmaError> {
        let length = u64::try_from(layout.length()).map_err(|_| DmaError::Unaddressable)?;
        if layout.alignment() < PAGE as usize {
            return Err(DmaError::InvalidAlignment);
        }
        self.map_internal(physical.value(), length)
    }

    fn unmap(&mut self, device: DeviceAddress, layout: DmaLayout) -> Result<(), DmaError> {
        let length = u64::try_from(layout.length()).map_err(|_| DmaError::Unaddressable)?;
        self.unmap_internal(device.value(), length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::DeviceMapping;

    #[derive(Default)]
    struct FakeEngine {
        active: bool,
        isolated: bool,
        stopped: bool,
        installed: usize,
        removed: usize,
        invalidated: usize,
        fail_install: bool,
        fail_remove: bool,
        fail_invalidate: bool,
    }

    impl IommuEngine for FakeEngine {
        fn translation_active(&self) -> bool { self.active }
        fn device_in_private_domain(&self) -> bool { self.isolated }
        fn device_stopped(&mut self) -> Result<bool, DomainFault> { Ok(self.stopped) }
        fn install_mapping(&mut self, _: u64, _: u64, _: u64) -> Result<(), DomainFault> {
            if self.fail_install { return Err(DomainFault::InstallFailed); }
            self.installed += 1;
            Ok(())
        }
        fn remove_mapping(&mut self, _: u64, _: u64) -> Result<(), DomainFault> {
            if self.fail_remove { return Err(DomainFault::RemovalFailed); }
            self.removed += 1;
            Ok(())
        }
        fn invalidate_and_wait(&mut self) -> Result<(), DomainFault> {
            if self.fail_invalidate { return Err(DomainFault::CacheInvalidationFailed); }
            self.invalidated += 1;
            Ok(())
        }
    }

    fn engine() -> FakeEngine {
        FakeEngine { active: true, isolated: true, stopped: true, ..FakeEngine::default() }
    }

    fn domain() -> IsolatedIommuDomain<FakeEngine, 2> {
        IsolatedIommuDomain::new(engine(), 0x8000_0000, 0x8000_4000).unwrap()
    }

    fn layout() -> DmaLayout { DmaLayout::new(4096, 4096).unwrap() }

    #[test]
    fn real_device_address_is_distinct_from_physical() {
        let mut d = domain();
        let mut map = DeviceMapping::establish(&mut d, PhysicalAddress::new(0x4000), layout())
            .unwrap();
        assert_eq!(map.device_address().value(), 0x8000_0000);
        assert_eq!(d.active_count(), 1);
        assert_eq!(d.engine().installed, 1);
        assert_eq!(d.engine().invalidated, 1);
        map.close(&mut d).unwrap();
        assert_eq!(d.engine().removed, 1);
        assert_eq!(d.engine().invalidated, 2);
        assert_eq!(d.active_count(), 0);
    }

    #[test]
    fn never_maps_without_isolation_and_translation() {
        for (active, isolated) in [(false, true), (true, false), (false, false)] {
            let mut e = engine();
            e.active = active;
            e.isolated = isolated;
            let mut d = IsolatedIommuDomain::<_, 2>::new(e, 0x8000_0000, 0x8000_4000).unwrap();
            assert!(DeviceMapping::establish(&mut d, PhysicalAddress::new(0x4000), layout()).is_err());
            assert_eq!(d.engine().installed, 0);
            assert!(d.fault().is_some());
        }
    }

    #[test]
    fn busy_device_blocks_mapping_and_unmapping() {
        let mut d = domain();
        d.engine.stopped = false;
        assert!(DeviceMapping::establish(&mut d, PhysicalAddress::new(0x4000), layout()).is_err());
        assert_eq!(d.engine().installed, 0);
        d.engine.stopped = true;
        let mut map = DeviceMapping::establish(&mut d, PhysicalAddress::new(0x4000), layout()).unwrap();
        d.engine.stopped = false;
        assert!(map.close(&mut d).is_err());
        assert!(map.is_mapped());
        assert_eq!(d.engine().removed, 0);
        d.engine.stopped = true;
        map.close(&mut d).unwrap();
    }

    #[test]
    fn separate_windows_do_not_overlap_and_reuse_only_after_invalidation() {
        let mut d = domain();
        let mut a = DeviceMapping::establish(&mut d, PhysicalAddress::new(0x4000), layout()).unwrap();
        let mut b = DeviceMapping::establish(&mut d, PhysicalAddress::new(0x5000), layout()).unwrap();
        assert_ne!(a.device_address(), b.device_address());
        assert_eq!(d.active_count(), 2);
        a.close(&mut d).unwrap();
        b.close(&mut d).unwrap();
        let mut c = DeviceMapping::establish(&mut d, PhysicalAddress::new(0x6000), layout()).unwrap();
        assert_eq!(c.device_address().value(), 0x8000_0000);
        c.close(&mut d).unwrap();
    }

    #[test]
    fn failed_install_quarantines_iova_until_explicit_recovery() {
        let mut d = domain();
        d.engine.fail_install = true;
        assert!(DeviceMapping::establish(&mut d, PhysicalAddress::new(0x4000), layout()).is_err());
        assert_eq!(d.quarantined_count(), 1);
        assert!(DeviceMapping::establish(&mut d, PhysicalAddress::new(0x5000), layout()).is_err());
        d.engine.fail_install = false;
        d.recover_quarantined().unwrap();
        assert_eq!(d.quarantined_count(), 0);
        assert_eq!(d.fault(), None);
        let mut map = DeviceMapping::establish(&mut d, PhysicalAddress::new(0x4000), layout()).unwrap();
        map.close(&mut d).unwrap();
    }

    #[test]
    fn failed_invalidation_never_reuses_accessible_buffer() {
        let mut d = domain();
        d.engine.fail_invalidate = true;
        assert!(DeviceMapping::establish(&mut d, PhysicalAddress::new(0x4000), layout()).is_err());
        assert_eq!(d.quarantined_count(), 1);
        assert!(d.fault().is_some());
        assert_eq!(d.recover_quarantined(), Err(DomainFault::CacheInvalidationFailed));
        assert_eq!(d.quarantined_count(), 1);
        d.engine.fail_invalidate = false;
        d.recover_quarantined().unwrap();
        assert_eq!(d.quarantined_count(), 0);
    }

    #[test]
    fn failed_unmap_preserves_ownership_and_cannot_reuse_iova() {
        let mut d = domain();
        let mut map = DeviceMapping::establish(&mut d, PhysicalAddress::new(0x4000), layout()).unwrap();
        d.engine.fail_remove = true;
        assert!(map.close(&mut d).is_err());
        assert!(map.is_mapped());
        assert_eq!(d.quarantined_count(), 1);
        d.engine.fail_remove = false;
        d.recover_quarantined().unwrap();
        assert_eq!(d.quarantined_count(), 0);
        assert!(map.is_mapped());
    }

    #[test]
    fn rejects_bad_ranges_before_hardware_call() {
        let mut d = domain();
        assert!(DeviceMapping::establish(
            &mut d, PhysicalAddress::new(0x4001), layout()
        ).is_err());
        assert_eq!(d.engine().installed, 0);
        assert!(IsolatedIommuDomain::<_, 0>::new(engine(), 0x1000, 0x2000).is_err());
        assert!(IsolatedIommuDomain::<_, 2>::new(engine(), 0x1001, 0x2000).is_err());
    }
}
