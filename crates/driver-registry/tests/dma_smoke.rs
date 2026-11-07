//! Совместная проверка подбора драйвера PCIe и жизненного цикла DMA.
//! Испытание использует фиктивное устройство и не запускает аппаратный DMA.

use phoenix_dma::{
    CpuVirtualAddress, DeviceAddress, DmaError, DmaLayout, DmaRegion,
    leases::{DmaBacking, DmaLeaseError, DmaLeaseTable},
};
use phoenix_driver_registry::{
    DriverDescriptor, DriverRegistry, DriverStartError, DriverVersion, PcieMatchRule,
    RegisteredDriver,
};
use phoenix_pcie::PcieFunction;

fn start_device(_: PcieFunction) -> Result<(), DriverStartError> {
    Ok(())
}

fn usb_controller() -> PcieFunction {
    PcieFunction {
        segment_group: 0,
        bus: 0,
        device: 2,
        function: 0,
        vendor_id: 0x1234,
        device_id: 0x5678,
        class_code: 0x0c,
        subclass: 0x03,
        programming_interface: 0x30,
        revision_id: 1,
        header_type: 0,
        config_address: 0x8000_1000,
    }
}

struct TestBacking {
    releases: usize,
}

impl DmaBacking for TestBacking {
    fn allocate(&mut self, layout: DmaLayout) -> Result<DmaRegion, DmaError> {
        DmaRegion::new(
            CpuVirtualAddress::new(0x4000),
            DeviceAddress::new(0x8000),
            layout,
        )
    }

    fn release(&mut self, _: DmaRegion) {
        self.releases += 1;
    }
}

#[test]
fn selected_xhci_driver_obeys_dma_lifecycle() {
    let mut registry = DriverRegistry::<1>::new();
    registry
        .register(RegisteredDriver {
            descriptor: DriverDescriptor::new(
                "test-xhci",
                DriverVersion::new(0, 1, 0),
                1,
                PcieMatchRule {
                    class_code: Some(0x0c),
                    subclass: Some(0x03),
                    programming_interface: Some(0x30),
                    ..PcieMatchRule::any()
                },
            ),
            start: start_device,
        })
        .unwrap();

    let launch = registry.start_best(usb_controller()).unwrap();
    assert_eq!(launch.name, "test-xhci");

    let mut backing = TestBacking { releases: 0 };
    let mut leases = DmaLeaseTable::<1>::new();
    let layout = DmaLayout::new(4096, 4096).unwrap();
    let handle = leases.acquire(&mut backing, layout).unwrap();
    let region = leases.region(handle).unwrap();
    assert_eq!(region.cpu_address().value(), 0x4000);
    assert_eq!(region.device_address().value(), 0x8000);
    leases.begin_device_access(handle).unwrap();
    assert_eq!(
        leases.release(&mut backing, handle),
        Err(DmaLeaseError::DeviceActive)
    );
    assert_eq!(backing.releases, 0);

    // Фиктивный контроллер считается остановленным только после этой отметки.
    leases.finish_device_access(handle).unwrap();
    leases.release(&mut backing, handle).unwrap();
    assert_eq!(backing.releases, 1);
    assert_eq!(leases.region(handle), Err(DmaLeaseError::StaleHandle));
}
