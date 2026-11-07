//! Отдельный контракт отображения физической области в адресное пространство устройства.
//!
//! Здесь нет предположения, что физический адрес процессора совпадает
//! с адресом устройства. Настройка аппаратных таблиц остаётся за реализацией.

use crate::{DeviceAddress, DmaError, DmaLayout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalAddress(u64);

impl PhysicalAddress {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaMappingError {
    InvalidRange(DmaError),
    Backend(DmaError),
    AlreadyUnmapped,
}

pub trait DmaMappingBackend {
    /// Реализация должна обеспечить видимость области для выбранного устройства,
    /// права доступа и вернуть корректный адрес всего диапазона. При ошибке
    /// она не должна оставлять неучтённого отображения.
    fn map(
        &mut self,
        physical: PhysicalAddress,
        layout: DmaLayout,
    ) -> Result<DeviceAddress, DmaError>;

    /// При ошибке отображение считается действующим до устранения причины.
    fn unmap(&mut self, device: DeviceAddress, layout: DmaLayout) -> Result<(), DmaError>;
}

/// Действующее отображение, которым владеет вызывающая сторона.
/// Не является автоматической остановкой устройства или блокировкой страниц.
pub struct DeviceMapping {
    physical: PhysicalAddress,
    device: DeviceAddress,
    layout: DmaLayout,
    mapped: bool,
}

impl DeviceMapping {
    pub fn establish<B: DmaMappingBackend>(
        backend: &mut B,
        physical: PhysicalAddress,
        layout: DmaLayout,
    ) -> Result<Self, DmaMappingError> {
        validate_range(physical.value(), layout).map_err(DmaMappingError::InvalidRange)?;

        let device = backend
            .map(physical, layout)
            .map_err(DmaMappingError::Backend)?;

        Ok(Self {
            physical,
            device,
            layout,
            mapped: true,
        })
    }

    pub const fn physical_address(&self) -> PhysicalAddress {
        self.physical
    }

    pub const fn device_address(&self) -> DeviceAddress {
        self.device
    }

    pub const fn layout(&self) -> DmaLayout {
        self.layout
    }

    pub const fn is_mapped(&self) -> bool {
        self.mapped
    }

    /// Допустимо только после подтверждённого завершения DMA устройством.
    /// При отказе владение сохраняется для повторной попытки.
    pub fn close<B: DmaMappingBackend>(&mut self, backend: &mut B) -> Result<(), DmaMappingError> {
        if !self.mapped {
            return Err(DmaMappingError::AlreadyUnmapped);
        }
        backend
            .unmap(self.device, self.layout)
            .map_err(DmaMappingError::Backend)?;
        self.mapped = false;
        Ok(())
    }
}

fn validate_range(address: u64, layout: DmaLayout) -> Result<(), DmaError> {
    let alignment = u64::try_from(layout.alignment()).map_err(|_| DmaError::Unaddressable)?;
    let length = u64::try_from(layout.length()).map_err(|_| DmaError::Unaddressable)?;
    if address % alignment != 0 {
        return Err(DmaError::MisalignedAddress);
    }
    address
        .checked_add(length - 1)
        .ok_or(DmaError::AddressOverflow)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeMapper {
        map_count: usize,
        unmap_count: usize,
        fail_unmap_once: bool,
    }

    impl DmaMappingBackend for FakeMapper {
        fn map(
            &mut self,
            physical: PhysicalAddress,
            _layout: DmaLayout,
        ) -> Result<DeviceAddress, DmaError> {
            self.map_count += 1;
            Ok(DeviceAddress::new(physical.value() + 0x1000))
        }

        fn unmap(&mut self, _device: DeviceAddress, _layout: DmaLayout) -> Result<(), DmaError> {
            if self.fail_unmap_once {
                self.fail_unmap_once = false;
                return Err(DmaError::InvalidRange);
            }
            self.unmap_count += 1;
            Ok(())
        }
    }

    #[test]
    fn mapping_keeps_physical_and_device_addresses_distinct() {
        let mut backend = FakeMapper {
            map_count: 0,
            unmap_count: 0,
            fail_unmap_once: false,
        };
        let layout = DmaLayout::new(4096, 4096).unwrap();
        let mut mapping =
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x2000), layout).unwrap();
        assert_eq!(mapping.physical_address().value(), 0x2000);
        assert_eq!(mapping.device_address().value(), 0x3000);
        assert!(mapping.is_mapped());
        mapping.close(&mut backend).unwrap();
        assert!(!mapping.is_mapped());
        assert_eq!(
            mapping.close(&mut backend),
            Err(DmaMappingError::AlreadyUnmapped)
        );
        assert_eq!((backend.map_count, backend.unmap_count), (1, 1));
    }

    #[test]
    fn invalid_source_never_calls_mapping_backend() {
        let mut backend = FakeMapper {
            map_count: 0,
            unmap_count: 0,
            fail_unmap_once: false,
        };
        let layout = DmaLayout::new(4096, 4096).unwrap();
        let result = DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x2001), layout);
        assert!(matches!(
            result,
            Err(DmaMappingError::InvalidRange(DmaError::MisalignedAddress))
        ));
        assert_eq!(backend.map_count, 0);
        let byte_layout = DmaLayout::new(4096, 1).unwrap();
        let result = DeviceMapping::establish(
            &mut backend,
            PhysicalAddress::new(u64::MAX - 1),
            byte_layout,
        );
        assert!(matches!(
            result,
            Err(DmaMappingError::InvalidRange(DmaError::AddressOverflow))
        ));
        assert_eq!(backend.map_count, 0);
    }

    #[test]
    fn failed_unmap_keeps_mapping_active_for_retry() {
        let mut backend = FakeMapper {
            map_count: 0,
            unmap_count: 0,
            fail_unmap_once: true,
        };
        let layout = DmaLayout::new(4096, 4096).unwrap();
        let mut mapping =
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x4000), layout).unwrap();
        assert_eq!(
            mapping.close(&mut backend),
            Err(DmaMappingError::Backend(DmaError::InvalidRange))
        );
        assert!(mapping.is_mapped());
        mapping.close(&mut backend).unwrap();
        assert_eq!(backend.unmap_count, 1);
    }
}

/// Явно разрешённый диапазон физических адресов, доступных устройству.
/// Для QEMU без IOMMU адрес шины совпадает с гостевым физическим адресом
/// только при соответствующей конфигурации виртуальной машины.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectDmaAperture {
    start: u64,
    end_exclusive: u64,
}

impl DirectDmaAperture {
    pub fn new(start: u64, end_exclusive: u64) -> Result<Self, DmaError> {
        if start >= end_exclusive {
            return Err(DmaError::InvalidRange);
        }
        Ok(Self {
            start,
            end_exclusive,
        })
    }

    fn contains(self, physical: u64, length: u64) -> bool {
        let Some(end) = physical.checked_add(length) else {
            return false;
        };
        physical >= self.start && end <= self.end_exclusive
    }
}

#[derive(Clone, Copy)]
struct DirectSlot {
    start: u64,
    length: u64,
    active: bool,
}

impl DirectSlot {
    const EMPTY: Self = Self {
        start: 0,
        length: 0,
        active: false,
    };
}

/// Проверяемый вариант адресации без IOMMU для фиксированного профиля QEMU.
///
/// Не закрепляет физические страницы, не включает PCI Bus Master и не
/// подтверждает завершение операции. Эти действия обязательны для драйвера.
/// При нехватке слотов и при пересечении уже активных областей возвращает
/// ошибку до выдачи устройству адреса.
pub struct QemuDirectDma<const N: usize> {
    aperture: DirectDmaAperture,
    slots: [DirectSlot; N],
}

impl<const N: usize> QemuDirectDma<N> {
    /// # Безопасность
    ///
    /// Вызывающий код обязан подтвердить отсутствие IOMMU-перевода для
    /// выбранного устройства, доступность всего диапазона адресов устройству,
    /// корректную настройку PCI и закрепление выделяемых страниц до unmap.
    /// Этот контракт предназначен только для явно проверенного профиля QEMU.
    pub unsafe fn new(aperture: DirectDmaAperture) -> Self {
        Self {
            aperture,
            slots: [DirectSlot::EMPTY; N],
        }
    }

    pub fn active_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.active).count()
    }
}

impl<const N: usize> DmaMappingBackend for QemuDirectDma<N> {
    fn map(
        &mut self,
        physical: PhysicalAddress,
        layout: DmaLayout,
    ) -> Result<DeviceAddress, DmaError> {
        validate_range(physical.value(), layout)?;
        let length = u64::try_from(layout.length()).map_err(|_| DmaError::Unaddressable)?;
        let start = physical.value();
        if !self.aperture.contains(start, length) {
            return Err(DmaError::Unaddressable);
        }
        let end = start.checked_add(length).ok_or(DmaError::AddressOverflow)?;
        if self
            .slots
            .iter()
            .any(|slot| slot.active && start < slot.start + slot.length && slot.start < end)
        {
            return Err(DmaError::InvalidRange);
        }
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| !slot.active)
            .ok_or(DmaError::Unaddressable)?;
        *slot = DirectSlot {
            start,
            length,
            active: true,
        };
        Ok(DeviceAddress::new(start))
    }

    fn unmap(&mut self, device: DeviceAddress, layout: DmaLayout) -> Result<(), DmaError> {
        let length = u64::try_from(layout.length()).map_err(|_| DmaError::Unaddressable)?;
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.active && slot.start == device.value() && slot.length == length)
            .ok_or(DmaError::InvalidRange)?;
        slot.active = false;
        Ok(())
    }
}

#[cfg(test)]
mod direct_tests {
    use super::*;

    fn backend<const N: usize>() -> QemuDirectDma<N> {
        let aperture = DirectDmaAperture::new(0x1000, 0x9000).unwrap();
        unsafe { QemuDirectDma::new(aperture) }
    }

    #[test]
    fn maps_physical_to_device_only_inside_aperture() {
        let mut backend = backend::<2>();
        let layout = DmaLayout::new(0x1000, 0x1000).unwrap();
        let mut first =
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x2000), layout).unwrap();
        assert_eq!(first.device_address().value(), 0x2000);
        assert_eq!(backend.active_count(), 1);
        assert!(matches!(
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x8001), layout),
            Err(DmaMappingError::InvalidRange(DmaError::MisalignedAddress))
        ));
        assert!(matches!(
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x9000), layout),
            Err(DmaMappingError::Backend(DmaError::Unaddressable))
        ));
        first.close(&mut backend).unwrap();
        assert_eq!(backend.active_count(), 0);
    }

    #[test]
    fn refuses_aliases_and_exhaustion_without_losing_existing_mapping() {
        let mut backend = backend::<1>();
        let layout = DmaLayout::new(0x1000, 0x1000).unwrap();
        let mut first =
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x2000), layout).unwrap();
        assert!(matches!(
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x2000), layout),
            Err(DmaMappingError::Backend(DmaError::InvalidRange))
        ));
        assert!(matches!(
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x4000), layout),
            Err(DmaMappingError::Backend(DmaError::Unaddressable))
        ));
        assert_eq!(backend.active_count(), 1);
        first.close(&mut backend).unwrap();
        let mut second =
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x4000), layout).unwrap();
        second.close(&mut backend).unwrap();
    }

    #[test]
    fn validates_bounds_and_unmap_identity() {
        assert_eq!(
            DirectDmaAperture::new(0x2000, 0x2000),
            Err(DmaError::InvalidRange)
        );
        let mut backend = backend::<1>();
        let layout = DmaLayout::new(0x1000, 0x1000).unwrap();
        assert_eq!(
            backend.unmap(DeviceAddress::new(0x2000), layout),
            Err(DmaError::InvalidRange)
        );
        let mut mapping =
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(0x8000), layout).unwrap();
        assert_eq!(
            backend.unmap(DeviceAddress::new(0x7000), layout),
            Err(DmaError::InvalidRange)
        );
        mapping.close(&mut backend).unwrap();
        assert_eq!(
            backend.unmap(DeviceAddress::new(0x8000), layout),
            Err(DmaError::InvalidRange)
        );
    }
}
