//! Проверка смены способа DMA-адресации без изменения кода драйвера.
//!
//! Оба сценария используют одну и ту же функцию `run_driver` и один
//! `DmaBuffer`. «IOMMU» ниже — имитация таблицы преобразования, а не
//! аппаратная реализация или доказательство безопасности физического ПК.

use phoenix_dma::{
    CpuVirtualAddress, DeviceAddress, DmaError, DmaLayout,
    buffer::{DmaBuffer, DmaBufferError, DmaBufferPlatform, DmaMemoryBackend, SplitDmaPlatform},
    mapping::{DmaMappingBackend, PhysicalAddress},
};

const PAGE: usize = 4096;
const PHYSICAL: u64 = 0x8000;
const CPU: usize = 0x2000_8000;
const REMAPPED: u64 = 0x8000_0000;

#[derive(Default)]
struct Memory {
    reserved: bool,
    mapped: bool,
    returned: usize,
    invalid_cpu: bool,
}

impl DmaMemoryBackend for Memory {
    fn reserve(&mut self, _: DmaLayout) -> Result<PhysicalAddress, DmaError> {
        if self.reserved {
            return Err(DmaError::InvalidRange);
        }
        self.reserved = true;
        Ok(PhysicalAddress::new(PHYSICAL))
    }

    fn map_cpu(
        &mut self,
        physical: PhysicalAddress,
        _: DmaLayout,
    ) -> Result<CpuVirtualAddress, DmaError> {
        if !self.reserved || self.mapped || physical.value() != PHYSICAL {
            return Err(DmaError::InvalidRange);
        }
        self.mapped = true;
        let address = if self.invalid_cpu { CPU + 1 } else { CPU };
        Ok(CpuVirtualAddress::new(address))
    }

    fn unmap_cpu(&mut self, cpu: CpuVirtualAddress, _: DmaLayout) -> Result<(), DmaError> {
        if !self.mapped || cpu.value() != if self.invalid_cpu { CPU + 1 } else { CPU } {
            return Err(DmaError::InvalidRange);
        }
        self.mapped = false;
        Ok(())
    }

    fn free(&mut self, physical: PhysicalAddress, _: DmaLayout) -> Result<(), DmaError> {
        if !self.reserved || self.mapped || physical.value() != PHYSICAL {
            return Err(DmaError::InvalidRange);
        }
        self.reserved = false;
        self.returned += 1;
        Ok(())
    }
}

/// Таблица с одним закреплённым диапазоном и выбором адреса устройства.
/// Не управляет настоящим IOMMU и не обеспечивает очистку аппаратного кэша.
struct MockDeviceMapper {
    iova: u64,
    active: bool,
    fail_unmap_once: bool,
    map_calls: usize,
    unmap_calls: usize,
}

impl MockDeviceMapper {
    fn new(iova: u64) -> Self {
        Self {
            iova,
            active: false,
            fail_unmap_once: false,
            map_calls: 0,
            unmap_calls: 0,
        }
    }
}

impl DmaMappingBackend for MockDeviceMapper {
    fn map(
        &mut self,
        physical: PhysicalAddress,
        layout: DmaLayout,
    ) -> Result<DeviceAddress, DmaError> {
        if self.active || physical.value() != PHYSICAL || layout.length() != PAGE {
            return Err(DmaError::InvalidRange);
        }
        self.active = true;
        self.map_calls += 1;
        Ok(DeviceAddress::new(self.iova))
    }

    fn unmap(&mut self, device: DeviceAddress, layout: DmaLayout) -> Result<(), DmaError> {
        if !self.active || device.value() != self.iova || layout.length() != PAGE {
            return Err(DmaError::InvalidRange);
        }
        if self.fail_unmap_once {
            self.fail_unmap_once = false;
            return Err(DmaError::InvalidRange);
        }
        self.active = false;
        self.unmap_calls += 1;
        Ok(())
    }
}

/// Тот же драйвер работает и при тождественном отображении, и при
/// независимом адресе устройства. Физические адреса ему неизвестны.
fn run_driver<P: DmaBufferPlatform>(
    platform: &mut P,
    expected_device_address: u64,
) -> Result<(), DmaBufferError> {
    let layout = DmaLayout::new(PAGE, PAGE).unwrap();
    let mut buffer = DmaBuffer::reserve(platform, layout)?;
    buffer.map_cpu(platform)?;
    buffer.map_device(platform)?;
    let region = buffer.region()?;
    assert_eq!(region.cpu_address().value(), CPU);
    assert_eq!(region.device_address().value(), expected_device_address);
    assert_eq!(
        region.device_offset(128, 16).unwrap().value(),
        expected_device_address + 128
    );
    buffer.begin_device_access()?;
    assert_eq!(buffer.close(platform), Err(DmaBufferError::DeviceActive));
    buffer.finish_device_access()?;
    buffer.close(platform)
}

#[test]
fn driver_code_is_identical_with_direct_and_remapped_addresses() {
    for iova in [PHYSICAL, REMAPPED] {
        let mut memory = Memory::default();
        let mut mapper = MockDeviceMapper::new(iova);
        {
            let mut platform = SplitDmaPlatform::new(&mut memory, &mut mapper);
            run_driver(&mut platform, iova).unwrap();
        }
        assert!(!memory.reserved);
        assert!(!memory.mapped);
        assert_eq!(memory.returned, 1);
        assert!(!mapper.active);
        assert_eq!((mapper.map_calls, mapper.unmap_calls), (1, 1));
    }
}

#[test]
fn failed_translation_unmap_preserves_all_ownership_until_retry() {
    let mut memory = Memory::default();
    let mut mapper = MockDeviceMapper::new(REMAPPED);
    mapper.fail_unmap_once = true;
    let mut platform = SplitDmaPlatform::new(&mut memory, &mut mapper);
    let layout = DmaLayout::new(PAGE, PAGE).unwrap();
    let mut buffer = DmaBuffer::reserve(&mut platform, layout).unwrap();
    buffer.map_cpu(&mut platform).unwrap();
    buffer.map_device(&mut platform).unwrap();
    assert_eq!(buffer.region().unwrap().device_address().value(), REMAPPED);
    assert_eq!(
        buffer.close(&mut platform),
        Err(DmaBufferError::Mapping(
            phoenix_dma::mapping::DmaMappingError::Backend(DmaError::InvalidRange)
        ))
    );
    assert!(buffer.is_reserved());
    // Доступ к источникам ресурса выполняется после завершения заимствования адаптером.
    buffer.close(&mut platform).unwrap();
    drop(platform);
    assert!(!mapper.active);
    assert!(!memory.reserved);
    assert_eq!(memory.returned, 1);
}

#[test]
fn translated_address_must_still_satisfy_device_alignment() {
    let mut memory = Memory::default();
    let mut mapper = MockDeviceMapper::new(REMAPPED + 1);
    let mut platform = SplitDmaPlatform::new(&mut memory, &mut mapper);
    let layout = DmaLayout::new(PAGE, PAGE).unwrap();
    let mut buffer = DmaBuffer::reserve(&mut platform, layout).unwrap();
    buffer.map_cpu(&mut platform).unwrap();
    assert_eq!(
        buffer.map_device(&mut platform),
        Err(DmaBufferError::InvalidAddress)
    );
    assert_eq!(buffer.region(), Err(DmaBufferError::InvalidAddress));
    assert_eq!(
        buffer.begin_device_access(),
        Err(DmaBufferError::InvalidAddress)
    );
    assert!(buffer.is_reserved());
    buffer.close(&mut platform).unwrap();
    drop(platform);
    assert_eq!(memory.returned, 1);
    assert!(!mapper.active);
    assert_eq!(mapper.unmap_calls, 1);
}

#[test]
fn invalid_cpu_address_rejected_before_device_mapping_and_still_released() {
    let mut memory = Memory {
        invalid_cpu: true,
        ..Memory::default()
    };
    let mut mapper = MockDeviceMapper::new(REMAPPED);
    let mut platform = SplitDmaPlatform::new(&mut memory, &mut mapper);
    let layout = DmaLayout::new(PAGE, PAGE).unwrap();
    let mut buffer = DmaBuffer::reserve(&mut platform, layout).unwrap();

    assert_eq!(
        buffer.map_cpu(&mut platform),
        Err(DmaBufferError::InvalidAddress)
    );
    assert_eq!(buffer.region(), Err(DmaBufferError::NotReady));
    buffer.close(&mut platform).unwrap();
    drop(platform);

    assert_eq!(memory.returned, 1);
    assert!(!memory.reserved);
    assert!(!mapper.active);
    assert_eq!(mapper.map_calls, 0);
}
