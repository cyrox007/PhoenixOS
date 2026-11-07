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
    pub fn close<B: DmaMappingBackend>(
        &mut self,
        backend: &mut B,
    ) -> Result<(), DmaMappingError> {
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
        assert_eq!(mapping.close(&mut backend), Err(DmaMappingError::AlreadyUnmapped));
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
