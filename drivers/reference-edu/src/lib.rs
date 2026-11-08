#![no_std]

//! Эталонный PCIe-драйвер QEMU EDU: поиск устройства, контракт DMA и IRQ.
//!
//! Ядро предоставляет выделение физических страниц, отображение MMIO,
//! вход аппаратного прерывания и подтверждение EOI. Библиотека не
//! угадывает эти ресурсы, не создаёт отдельный распределитель памяти
//! и не обращается к устройствам напрямую.

use core::sync::atomic::{AtomicU32, Ordering};
use phoenix_driver_registry::{
    ContextRegisteredDriver, DriverDescriptor, DriverLaunch, DriverRegistry, DriverRegistryError,
    DriverStartError, DriverVersion, PcieMatchRule,
};
use phoenix_pcie::PcieFunction;

pub const EDU_VENDOR_ID: u16 = 0x1234;
pub const EDU_DEVICE_ID: u16 = 0x11e8;
pub const DRIVER_VERSION: DriverVersion = DriverVersion::new(0, 3, 0);
pub const MMIO_IDENTIFICATION: usize = 0x00;
pub const MMIO_LIVENESS: usize = 0x04;
pub const MMIO_IRQ_STATUS: usize = 0x24;
pub const MMIO_IRQ_RAISE: usize = 0x60;
pub const MMIO_IRQ_ACK: usize = 0x64;
pub const MMIO_DMA_SRC: usize = 0x80;
pub const MMIO_DMA_DST: usize = 0x88;
pub const MMIO_DMA_COUNT: usize = 0x90;
pub const MMIO_DMA_COMMAND: usize = 0x98;
pub const EDU_INTERNAL_BUFFER: u64 = 0x40000;
pub const EDU_DMA_LIMIT: u64 = 1 << 28;
pub const EDU_TRANSFER_BYTES: usize = 256;
pub const EDU_IRQ_PROBE: u32 = 0x42;
pub const EDU_IRQ_DMA: u32 = 0x100;
pub const EDU_DMA_START: u32 = 1;
pub const EDU_DMA_FROM_DEVICE: u32 = 2;
pub const EDU_DMA_INTERRUPT: u32 = 4;

/// Сопоставление только по точному сочетанию идентификаторов:
/// учебный драйвер не должен захватить реальный контроллер пользователя.
pub const fn descriptor() -> DriverDescriptor {
    DriverDescriptor::new(
        "qemu-edu-reference",
        DRIVER_VERSION,
        100,
        PcieMatchRule {
            vendor_id: Some(EDU_VENDOR_ID),
            device_id: Some(EDU_DEVICE_ID),
            class_code: None,
            subclass: None,
            programming_interface: None,
        },
    )
}

#[derive(Default)]
pub struct ProbeContext {
    pub discovered: Option<PcieFunction>,
}

pub fn probe(function: PcieFunction, context: &mut ProbeContext) -> Result<(), DriverStartError> {
    if function.vendor_id != EDU_VENDOR_ID || function.device_id != EDU_DEVICE_ID {
        return Err(DriverStartError::UnsupportedDevice);
    }
    context.discovered = Some(function);
    Ok(())
}

/// Используется тот же реестр и проверка версии интерфейса, что у
/// реальных драйверов NVMe, USB и VirtIO-GPU.
pub fn start_reference(
    function: PcieFunction,
    context: &mut ProbeContext,
) -> Result<DriverLaunch, DriverRegistryError> {
    let mut registry = DriverRegistry::<1, ContextRegisteredDriver<ProbeContext>>::new();
    registry.register(ContextRegisteredDriver {
        descriptor: descriptor(),
        start: probe,
    })?;
    registry.start_best_with_context(function, context)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExampleError {
    InvalidDmaRange,
    InvalidDeviceSignature,
    DmaTimeout,
    InterruptTimeout,
    UnexpectedResponse,
}

/// У EDU только 28 адресных бит DMA, пока устройство не перенастроено.
/// Диапазон ограничивается ещё до выдачи адреса устройству.
pub fn check_dma_range(address: u64, length: usize) -> Result<(), ExampleError> {
    if length == 0 {
        return Err(ExampleError::InvalidDmaRange);
    }
    let length = u64::try_from(length).map_err(|_| ExampleError::InvalidDmaRange)?;
    let end = address
        .checked_add(length)
        .ok_or(ExampleError::InvalidDmaRange)?;
    if end > EDU_DMA_LIMIT {
        return Err(ExampleError::InvalidDmaRange);
    }
    Ok(())
}

/// Без блокировок и выделения памяти в обработчике прерывания.
/// ISR читает и подтверждает причину на устройстве, затем публикует
/// событие. Сложную работу выполняет обычный поток после take().
pub struct InterruptMailbox {
    pending: AtomicU32,
    observed: AtomicU32,
}

impl InterruptMailbox {
    pub const fn new() -> Self {
        Self {
            pending: AtomicU32::new(0),
            observed: AtomicU32::new(0),
        }
    }

    /// Может вызываться только после фактического чтения причины IRQ.
    /// Адаптер ядра обязан также подтвердить её в MMIO и послать EOI.
    pub fn record(&self, status: u32) {
        if status != 0 {
            self.pending.fetch_or(status, Ordering::Release);
            self.observed.fetch_or(status, Ordering::Relaxed);
        }
    }

    pub fn take(&self) -> u32 {
        self.pending.swap(0, Ordering::AcqRel)
    }

    /// Сбрасывать только при отключённом источнике IRQ. Иначе новое
    /// аппаратное событие может быть потеряно при повторной инициализации.
    pub fn reset(&self) {
        self.pending.store(0, Ordering::Release);
        self.observed.store(0, Ordering::Release);
    }

    pub fn observed(&self) -> u32 {
        self.observed.load(Ordering::Acquire)
    }
}

impl Default for InterruptMailbox {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoenix_dma::{
        CpuVirtualAddress, DeviceAddress, DmaError, DmaLayout, DmaRegion,
        buffer::{DmaBuffer, DmaBufferError, DmaBufferPlatform},
        mapping::{DmaMappingBackend, PhysicalAddress},
    };
    use phoenix_driver_registry::{DRIVER_ABI_VERSION, DriverRegistryError};

    fn device(vendor: u16, id: u16) -> PcieFunction {
        PcieFunction {
            segment_group: 0,
            bus: 0,
            device: 7,
            function: 0,
            vendor_id: vendor,
            device_id: id,
            class_code: 0xff,
            subclass: 0,
            programming_interface: 0,
            revision_id: 1,
            header_type: 0,
            config_address: 0x8003_8000,
        }
    }

    #[test]
    fn registers_and_selects_only_edu_with_supported_interface() {
        let mut context = ProbeContext::default();
        let found = device(EDU_VENDOR_ID, EDU_DEVICE_ID);
        let launched = start_reference(found, &mut context).unwrap();
        assert_eq!(launched.name, "qemu-edu-reference");
        assert_eq!(launched.version, DRIVER_VERSION);
        assert_eq!(context.discovered, Some(found));
        let mut other_context = ProbeContext::default();
        assert_eq!(
            start_reference(device(0x10de, 0x1f99), &mut other_context),
            Err(DriverRegistryError::NoMatchingDriver)
        );
        assert!(other_context.discovered.is_none());
    }

    #[test]
    fn rejects_newer_abi_and_duplicate_registration() {
        let mut registry = DriverRegistry::<1, ContextRegisteredDriver<ProbeContext>>::new();
        let mut unsupported = descriptor();
        unsupported.abi_version = DRIVER_ABI_VERSION + 1;
        assert_eq!(
            registry.register(ContextRegisteredDriver {
                descriptor: unsupported,
                start: probe,
            }),
            Err(DriverRegistryError::UnsupportedAbi(DRIVER_ABI_VERSION + 1))
        );
        assert!(registry.is_empty());
        registry
            .register(ContextRegisteredDriver {
                descriptor: descriptor(),
                start: probe,
            })
            .unwrap();
        assert_eq!(
            registry.register(ContextRegisteredDriver {
                descriptor: descriptor(),
                start: probe,
            }),
            Err(DriverRegistryError::DuplicateDriver)
        );
    }

    #[test]
    fn dma_mask_rejects_boundary_and_overflow() {
        assert_eq!(check_dma_range(0x0fff_f000, 4096), Ok(()));
        assert_eq!(
            check_dma_range(0x0fff_f001, 4096),
            Err(ExampleError::InvalidDmaRange)
        );
        assert_eq!(check_dma_range(0, 0), Err(ExampleError::InvalidDmaRange));
        assert_eq!(
            check_dma_range(u64::MAX, 4),
            Err(ExampleError::InvalidDmaRange)
        );
    }

    #[test]
    fn irq_mailbox_coalesces_and_clears_events() {
        let mailbox = InterruptMailbox::new();
        mailbox.record(EDU_IRQ_PROBE);
        mailbox.record(EDU_IRQ_DMA);
        mailbox.record(0);
        assert_eq!(mailbox.observed(), EDU_IRQ_PROBE | EDU_IRQ_DMA);
        assert_eq!(mailbox.take(), EDU_IRQ_PROBE | EDU_IRQ_DMA);
        assert_eq!(mailbox.take(), 0);
        mailbox.reset();
        assert_eq!(mailbox.observed(), 0);
        mailbox.record(EDU_IRQ_DMA);
        assert_eq!(mailbox.take(), EDU_IRQ_DMA);
        assert_eq!(mailbox.observed(), EDU_IRQ_DMA);
    }

    #[derive(Default)]
    struct FakeMemory {
        frees: usize,
        device_unmaps: usize,
        cpu_unmaps: usize,
    }

    impl DmaMappingBackend for FakeMemory {
        fn map(
            &mut self,
            physical: PhysicalAddress,
            _layout: DmaLayout,
        ) -> Result<DeviceAddress, DmaError> {
            Ok(DeviceAddress::new(physical.value()))
        }

        fn unmap(&mut self, _: DeviceAddress, _: DmaLayout) -> Result<(), DmaError> {
            self.device_unmaps += 1;
            Ok(())
        }
    }

    impl DmaBufferPlatform for FakeMemory {
        fn reserve(&mut self, _: DmaLayout) -> Result<PhysicalAddress, DmaError> {
            Ok(PhysicalAddress::new(0x6000))
        }

        fn map_cpu(
            &mut self,
            _: PhysicalAddress,
            _: DmaLayout,
        ) -> Result<CpuVirtualAddress, DmaError> {
            Ok(CpuVirtualAddress::new(0x9000))
        }

        fn unmap_cpu(&mut self, _: CpuVirtualAddress, _: DmaLayout) -> Result<(), DmaError> {
            self.cpu_unmaps += 1;
            Ok(())
        }

        fn free(&mut self, _: PhysicalAddress, _: DmaLayout) -> Result<(), DmaError> {
            self.frees += 1;
            Ok(())
        }
    }

    #[test]
    fn dma_example_requires_completed_device_access_before_release() {
        let mut platform = FakeMemory::default();
        let layout = DmaLayout::new(4096, 4096).unwrap();
        let mut buffer = DmaBuffer::reserve(&mut platform, layout).unwrap();
        buffer.map_cpu(&mut platform).unwrap();
        buffer.map_device(&mut platform).unwrap();
        assert_eq!(buffer.region().unwrap().device_address().value(), 0x6000);
        buffer.begin_device_access().unwrap();
        assert_eq!(
            buffer.close(&mut platform),
            Err(DmaBufferError::DeviceActive)
        );
        assert_eq!(platform.frees, 0);
        buffer.finish_device_access().unwrap();
        buffer.close(&mut platform).unwrap();
        assert_eq!(
            (platform.device_unmaps, platform.cpu_unmaps, platform.frees),
            (1, 1, 1)
        );
        assert_eq!(buffer.close(&mut platform), Err(DmaBufferError::NotReady));
    }

    #[test]
    fn dma_buffer_is_not_ready_before_device_mapping() {
        let mut platform = FakeMemory::default();
        let mut buffer =
            DmaBuffer::reserve(&mut platform, DmaLayout::new(4096, 4096).unwrap()).unwrap();
        assert_eq!(buffer.begin_device_access(), Err(DmaBufferError::NotReady));
        buffer.close(&mut platform).unwrap();
        assert_eq!(platform.frees, 1);
    }
}
