//! Сквозная проверка реестра драйверов и безопасного владения DMA без аппаратуры.

use phoenix_dma::{
    CpuVirtualAddress, DeviceAddress, DmaError, DmaLayout, DmaRegion,
    leases::{DmaBacking, DmaLeaseError, DmaLeaseTable},
};
use phoenix_driver_registry::{
    DriverDescriptor, DriverRegistry, DriverStartError, DriverVersion, PcieMatchRule,
    RegisteredDriver,
};
use phoenix_pcie::PcieFunction;

fn usb_controller() -> PcieFunction {
    PcieFunction {
        segment_group: 0,
        bus: 0,
        device: 20,
        function: 0,
        vendor_id: 0x8086,
        device_id: 0x1234,
        class_code: 0x0c,
        subclass: 0x03,
        programming_interface: 0x30,
        revision_id: 1,
        header_type: 0,
        config_address: 0x800a_0000,
    }
}

fn start_mock_xhci(function: PcieFunction) -> Result<(), DriverStartError> {
    if function.class_code != 0x0c || function.programming_interface != 0x30 {
        return Err(DriverStartError::UnsupportedDevice);
    }
    Ok(())
}

struct TestMemory {
    allocations: usize,
    releases: usize,
}

impl DmaBacking for TestMemory {
    fn allocate(&mut self, layout: DmaLayout) -> Result<DmaRegion, DmaError> {
        let region = DmaRegion::new(
            CpuVirtualAddress::new(0x1000),
            DeviceAddress::new(0x2000),
            layout,
        )?;
        self.allocations += 1;
        Ok(region)
    }

    fn release(&mut self, _region: DmaRegion) {
        self.releases += 1;
    }
}

#[test]
fn selected_usb_driver_respects_dma_buffer_lifetime() {
    let mut registry = DriverRegistry::<2>::new();
    registry
        .register(RegisteredDriver {
            descriptor: DriverDescriptor::new(
                "xhci-mock",
                DriverVersion::new(0, 1, 0),
                10,
                PcieMatchRule {
                    class_code: Some(0x0c),
                    subclass: Some(0x03),
                    programming_interface: Some(0x30),
                    ..PcieMatchRule::any()
                },
            ),
            start: start_mock_xhci,
        })
        .unwrap();

    let started = registry.start_best(usb_controller()).unwrap();
    assert_eq!(started.name, "xhci-mock");

    let mut memory = TestMemory {
        allocations: 0,
        releases: 0,
    };
    let mut leases = DmaLeaseTable::<1>::new();
    let layout = DmaLayout::new(4096, 4096).unwrap();
    let handle = leases.acquire(&mut memory, layout).unwrap();
    assert_eq!(leases.region(handle).unwrap().device_address().value(), 0x2000);

    leases.begin_device_access(handle).unwrap();
    assert_eq!(
        leases.release(&mut memory, handle),
        Err(DmaLeaseError::DeviceActive)
    );
    assert_eq!(memory.releases, 0);

    // В реальном драйвере завершение должно быть подтверждено контроллером.
    leases.finish_device_access(handle).unwrap();
    leases.release(&mut memory, handle).unwrap();
    assert_eq!((memory.allocations, memory.releases), (1, 1));
    assert_eq!(leases.region(handle), Err(DmaLeaseError::StaleHandle));
}

#[test]
fn unsupported_device_does_not_start_dma_flow() {
    let registry = DriverRegistry::<0>::new();
    assert!(registry.start_best(usb_controller()).is_err());

    let mut memory = TestMemory {
        allocations: 0,
        releases: 0,
    };
    let mut leases = DmaLeaseTable::<0>::new();
    let layout = DmaLayout::new(4096, 4096).unwrap();
    assert_eq!(
        leases.acquire(&mut memory, layout),
        Err(DmaLeaseError::Exhausted)
    );
    assert_eq!(memory.allocations, 0);
}
