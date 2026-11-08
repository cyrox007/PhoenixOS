//! Управление жизненным циклом DMA-буфера без аппаратных предположений.
//!
//! Физические страницы принадлежат объекту до полного снятия обоих отображений.

use crate::{
    CpuVirtualAddress, DeviceAddress, DmaError, DmaLayout, DmaRegion,
    mapping::{DeviceMapping, DmaMappingBackend, DmaMappingError, PhysicalAddress},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaBufferError {
    Backend(DmaError),
    Mapping(DmaMappingError),
    NotReady,
    DeviceActive,
    InvalidAddress,
}

/// Подключение к единственному системному распределителю и таблицам страниц.
///
/// Реализация обязана сохранять резерв при ошибке освобождения и не
/// возвращать ошибку выделения после частичного захвата страниц.
pub trait DmaBufferPlatform: DmaMappingBackend {
    fn reserve(&mut self, layout: DmaLayout) -> Result<PhysicalAddress, DmaError>;
    fn map_cpu(
        &mut self,
        physical: PhysicalAddress,
        layout: DmaLayout,
    ) -> Result<CpuVirtualAddress, DmaError>;
    fn unmap_cpu(&mut self, cpu: CpuVirtualAddress, layout: DmaLayout) -> Result<(), DmaError>;
    fn free(&mut self, physical: PhysicalAddress, layout: DmaLayout) -> Result<(), DmaError>;
}

/// Отделяет владение физическими страницами и процессорским отображением
/// от выбранного механизма выдачи адресов устройству.
///
/// Можно заменить прямой вариант на будущую реализацию IOMMU без изменений
/// `DmaBuffer`, `DmaRegion` и драйверов, использующих их. Аппаратные
/// таблицы, очистка кэша и запрет доступа должны обеспечиваться реализацией
/// `DmaMappingBackend`. При ошибке unmap её состояние остаётся активным.
pub struct SplitDmaPlatform<'a, M: DmaBufferPlatform, B: DmaMappingBackend> {
    memory: &'a mut M,
    device_mapper: &'a mut B,
}

impl<'a, M: DmaBufferPlatform, B: DmaMappingBackend> SplitDmaPlatform<'a, M, B> {
    pub fn new(memory: &'a mut M, device_mapper: &'a mut B) -> Self {
        Self {
            memory,
            device_mapper,
        }
    }
}

impl<M: DmaBufferPlatform, B: DmaMappingBackend> DmaMappingBackend for SplitDmaPlatform<'_, M, B> {
    fn map(
        &mut self,
        physical: PhysicalAddress,
        layout: DmaLayout,
    ) -> Result<DeviceAddress, DmaError> {
        self.device_mapper.map(physical, layout)
    }

    fn unmap(&mut self, device: DeviceAddress, layout: DmaLayout) -> Result<(), DmaError> {
        self.device_mapper.unmap(device, layout)
    }
}

impl<M: DmaBufferPlatform, B: DmaMappingBackend> DmaBufferPlatform for SplitDmaPlatform<'_, M, B> {
    fn reserve(&mut self, layout: DmaLayout) -> Result<PhysicalAddress, DmaError> {
        self.memory.reserve(layout)
    }

    fn map_cpu(
        &mut self,
        physical: PhysicalAddress,
        layout: DmaLayout,
    ) -> Result<CpuVirtualAddress, DmaError> {
        self.memory.map_cpu(physical, layout)
    }

    fn unmap_cpu(&mut self, cpu: CpuVirtualAddress, layout: DmaLayout) -> Result<(), DmaError> {
        self.memory.unmap_cpu(cpu, layout)
    }

    fn free(&mut self, physical: PhysicalAddress, layout: DmaLayout) -> Result<(), DmaError> {
        self.memory.free(physical, layout)
    }
}

/// Последовательно закрываемая область, не использующая кучу.
/// Прерывания и многопоточный доступ синхронизируются вызывающей стороной.
pub struct DmaBuffer {
    physical: PhysicalAddress,
    layout: DmaLayout,
    cpu: Option<CpuVirtualAddress>,
    device: Option<DeviceMapping>,
    reserved: bool,
    active: bool,
}

impl DmaBuffer {
    pub fn reserve<P: DmaBufferPlatform>(
        platform: &mut P,
        layout: DmaLayout,
    ) -> Result<Self, DmaBufferError> {
        let physical = platform.reserve(layout).map_err(DmaBufferError::Backend)?;
        Ok(Self {
            physical,
            layout,
            cpu: None,
            device: None,
            reserved: true,
            active: false,
        })
    }

    pub fn map_cpu<P: DmaBufferPlatform>(
        &mut self,
        platform: &mut P,
    ) -> Result<(), DmaBufferError> {
        if !self.reserved || self.cpu.is_some() || self.device.is_some() {
            return Err(DmaBufferError::NotReady);
        }
        let cpu = platform
            .map_cpu(self.physical, self.layout)
            .map_err(DmaBufferError::Backend)?;
        // Договор платформы требует валидный адрес и отсутствие частично
        // созданных отображений при Err.
        self.cpu = Some(cpu);
        Ok(())
    }

    pub fn map_device<P: DmaBufferPlatform>(
        &mut self,
        platform: &mut P,
    ) -> Result<(), DmaBufferError> {
        if !self.reserved || self.cpu.is_none() || self.device.is_some() {
            return Err(DmaBufferError::NotReady);
        }
        let mapping = DeviceMapping::establish(platform, self.physical, self.layout)
            .map_err(DmaBufferError::Mapping)?;
        self.device = Some(mapping);
        Ok(())
    }

    pub fn region(&self) -> Result<DmaRegion, DmaBufferError> {
        let cpu = self.cpu.ok_or(DmaBufferError::NotReady)?;
        let device = self.device.as_ref().ok_or(DmaBufferError::NotReady)?;
        DmaRegion::new(cpu, device.device_address(), self.layout)
            .map_err(|_| DmaBufferError::InvalidAddress)
    }

    pub fn begin_device_access(&mut self) -> Result<(), DmaBufferError> {
        self.region()?;
        if self.active {
            return Err(DmaBufferError::DeviceActive);
        }
        self.active = true;
        Ok(())
    }

    /// Вызывать только после аппаратного подтверждения окончания обмена.
    pub fn finish_device_access(&mut self) -> Result<(), DmaBufferError> {
        if !self.active {
            return Err(DmaBufferError::NotReady);
        }
        self.active = false;
        Ok(())
    }

    /// Сначала устройство, затем процессор, последними — физические страницы.
    /// При любой ошибке владение ещё не освобождёнными ресурсами сохраняется.
    pub fn close<P: DmaBufferPlatform>(&mut self, platform: &mut P) -> Result<(), DmaBufferError> {
        if !self.reserved {
            return Err(DmaBufferError::NotReady);
        }
        if self.active {
            return Err(DmaBufferError::DeviceActive);
        }

        if let Some(device) = self.device.as_mut() {
            device.close(platform).map_err(DmaBufferError::Mapping)?;
            self.device = None;
        }

        if let Some(cpu) = self.cpu {
            platform
                .unmap_cpu(cpu, self.layout)
                .map_err(DmaBufferError::Backend)?;
            self.cpu = None;
        }

        platform
            .free(self.physical, self.layout)
            .map_err(DmaBufferError::Backend)?;
        self.reserved = false;
        Ok(())
    }

    pub const fn is_reserved(&self) -> bool {
        self.reserved
    }

    pub const fn is_active(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakePlatform {
        free_count: usize,
        cpu_unmap_count: usize,
        device_unmap_count: usize,
        fail_device_once: bool,
        fail_cpu_once: bool,
        fail_free_once: bool,
    }

    impl DmaMappingBackend for FakePlatform {
        fn map(
            &mut self,
            _physical: PhysicalAddress,
            _layout: DmaLayout,
        ) -> Result<DeviceAddress, DmaError> {
            Ok(DeviceAddress::new(0x3000))
        }

        fn unmap(&mut self, _device: DeviceAddress, _layout: DmaLayout) -> Result<(), DmaError> {
            if self.fail_device_once {
                self.fail_device_once = false;
                return Err(DmaError::InvalidRange);
            }
            self.device_unmap_count += 1;
            Ok(())
        }
    }

    impl DmaBufferPlatform for FakePlatform {
        fn reserve(&mut self, _layout: DmaLayout) -> Result<PhysicalAddress, DmaError> {
            Ok(PhysicalAddress::new(0x1000))
        }

        fn map_cpu(
            &mut self,
            _physical: PhysicalAddress,
            _layout: DmaLayout,
        ) -> Result<CpuVirtualAddress, DmaError> {
            Ok(CpuVirtualAddress::new(0x2000))
        }

        fn unmap_cpu(
            &mut self,
            _cpu: CpuVirtualAddress,
            _layout: DmaLayout,
        ) -> Result<(), DmaError> {
            if self.fail_cpu_once {
                self.fail_cpu_once = false;
                return Err(DmaError::InvalidRange);
            }
            self.cpu_unmap_count += 1;
            Ok(())
        }

        fn free(&mut self, _physical: PhysicalAddress, _layout: DmaLayout) -> Result<(), DmaError> {
            if self.fail_free_once {
                self.fail_free_once = false;
                return Err(DmaError::InvalidRange);
            }
            self.free_count += 1;
            Ok(())
        }
    }

    fn ready(platform: &mut FakePlatform) -> DmaBuffer {
        let layout = DmaLayout::new(4096, 4096).unwrap();
        let mut buffer = DmaBuffer::reserve(platform, layout).unwrap();
        buffer.map_cpu(platform).unwrap();
        buffer.map_device(platform).unwrap();
        buffer
    }

    #[test]
    fn active_device_prevents_release() {
        let mut platform = FakePlatform::default();
        let mut buffer = ready(&mut platform);
        assert_eq!(buffer.region().unwrap().device_address().value(), 0x3000);
        buffer.begin_device_access().unwrap();
        assert_eq!(
            buffer.close(&mut platform),
            Err(DmaBufferError::DeviceActive)
        );
        assert_eq!(platform.free_count, 0);
        buffer.finish_device_access().unwrap();
        buffer.close(&mut platform).unwrap();
        assert!(!buffer.is_reserved());
        assert_eq!(platform.device_unmap_count, 1);
        assert_eq!(platform.cpu_unmap_count, 1);
        assert_eq!(platform.free_count, 1);
        assert_eq!(buffer.close(&mut platform), Err(DmaBufferError::NotReady));
    }

    #[test]
    fn failures_keep_physical_pages_until_all_mappings_are_gone() {
        let mut platform = FakePlatform {
            fail_device_once: true,
            fail_cpu_once: true,
            fail_free_once: true,
            ..FakePlatform::default()
        };
        let mut buffer = ready(&mut platform);
        assert!(buffer.close(&mut platform).is_err());
        assert_eq!((platform.device_unmap_count, platform.free_count), (0, 0));
        assert!(buffer.close(&mut platform).is_err());
        assert_eq!(
            (platform.device_unmap_count, platform.cpu_unmap_count),
            (1, 0)
        );
        assert_eq!(platform.free_count, 0);
        assert!(buffer.close(&mut platform).is_err());
        assert_eq!((platform.cpu_unmap_count, platform.free_count), (1, 0));
        buffer.close(&mut platform).unwrap();
        assert_eq!(platform.free_count, 1);
    }

    #[test]
    fn incomplete_buffer_is_not_accessible_to_device() {
        let mut platform = FakePlatform::default();
        let layout = DmaLayout::new(4096, 4096).unwrap();
        let mut buffer = DmaBuffer::reserve(&mut platform, layout).unwrap();
        assert!(buffer.region().is_err());
        assert!(buffer.begin_device_access().is_err());
        assert!(buffer.map_device(&mut platform).is_err());
        buffer.close(&mut platform).unwrap();
        assert_eq!(platform.free_count, 1);
    }
}
