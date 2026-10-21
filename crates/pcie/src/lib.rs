#![no_std]

pub const CONFIG_SPACE_BYTES: u64 = 4096;
pub const DEVICE_COUNT_PER_BUS: u8 = 32;
pub const FUNCTION_COUNT_PER_DEVICE: u8 = 8;
pub const ABSENT_VENDOR_ID: u16 = 0xffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcieError {
    InvalidBusRange,
    InvalidDevice,
    InvalidFunction,
    AddressOverflow,
    FunctionCountOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamSegment {
    pub base_address: u64,
    pub segment_group: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

impl EcamSegment {
    pub const fn new(
        base_address: u64,
        segment_group: u16,
        start_bus: u8,
        end_bus: u8,
    ) -> Result<Self, PcieError> {
        if base_address == 0 || start_bus > end_bus {
            return Err(PcieError::InvalidBusRange);
        }
        Ok(Self {
            base_address,
            segment_group,
            start_bus,
            end_bus,
        })
    }

    pub fn function_address(self, bus: u8, device: u8, function: u8) -> Result<u64, PcieError> {
        if bus < self.start_bus || bus > self.end_bus {
            return Err(PcieError::InvalidBusRange);
        }
        if device >= DEVICE_COUNT_PER_BUS {
            return Err(PcieError::InvalidDevice);
        }
        if function >= FUNCTION_COUNT_PER_DEVICE {
            return Err(PcieError::InvalidFunction);
        }

        let bus_offset = u64::from(bus) << 20;
        let device_offset = u64::from(device) << 15;
        let function_offset = u64::from(function) << 12;

        self.base_address
            .checked_add(bus_offset)
            .and_then(|address| address.checked_add(device_offset))
            .and_then(|address| address.checked_add(function_offset))
            .ok_or(PcieError::AddressOverflow)
    }
}

pub trait ConfigAccess {
    fn read_u32(&self, physical_address: u64) -> u32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcieFunction {
    pub segment_group: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub programming_interface: u8,
    pub revision_id: u8,
    pub header_type: u8,
    pub config_address: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumerationSummary {
    pub function_count: u32,
    pub bridge_count: u32,
    pub storage_controller_count: u32,
    pub usb_controller_count: u32,
}

pub fn enumerate_segment<A, F>(
    access: &A,
    segment: EcamSegment,
    mut visitor: F,
) -> Result<EnumerationSummary, PcieError>
where
    A: ConfigAccess,
    F: FnMut(PcieFunction),
{
    let mut summary = EnumerationSummary {
        function_count: 0,
        bridge_count: 0,
        storage_controller_count: 0,
        usb_controller_count: 0,
    };

    for bus in segment.start_bus..=segment.end_bus {
        for device in 0..DEVICE_COUNT_PER_BUS {
            for function in 0..FUNCTION_COUNT_PER_DEVICE {
                let address = segment.function_address(bus, device, function)?;
                let Some(info) = read_function(
                    access,
                    segment.segment_group,
                    bus,
                    device,
                    function,
                    address,
                ) else {
                    continue;
                };

                summary.function_count = summary
                    .function_count
                    .checked_add(1)
                    .ok_or(PcieError::FunctionCountOverflow)?;
                if info.class_code == 0x06 && info.subclass == 0x04 {
                    summary.bridge_count = summary.bridge_count.saturating_add(1);
                }
                if info.class_code == 0x01 {
                    summary.storage_controller_count =
                        summary.storage_controller_count.saturating_add(1);
                }
                if info.class_code == 0x0c && info.subclass == 0x03 {
                    summary.usb_controller_count = summary.usb_controller_count.saturating_add(1);
                }

                visitor(info);
            }
        }
    }

    Ok(summary)
}

fn read_function<A: ConfigAccess>(
    access: &A,
    segment_group: u16,
    bus: u8,
    device: u8,
    function: u8,
    address: u64,
) -> Option<PcieFunction> {
    let identity = access.read_u32(address);
    let vendor_id = identity as u16;
    if vendor_id == ABSENT_VENDOR_ID {
        return None;
    }

    let class = access.read_u32(address + 0x08);
    let header = access.read_u32(address + 0x0c);

    Some(PcieFunction {
        segment_group,
        bus,
        device,
        function,
        vendor_id,
        device_id: (identity >> 16) as u16,
        revision_id: class as u8,
        programming_interface: (class >> 8) as u8,
        subclass: (class >> 16) as u8,
        class_code: (class >> 24) as u8,
        header_type: (header >> 16) as u8,
        config_address: address,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeConfig {
        address: u64,
        identity: u32,
        class: u32,
        header: u32,
    }

    impl ConfigAccess for FakeConfig {
        fn read_u32(&self, physical_address: u64) -> u32 {
            if physical_address == self.address {
                return self.identity;
            }
            if physical_address == self.address + 0x08 {
                return self.class;
            }
            if physical_address == self.address + 0x0c {
                return self.header;
            }
            u32::MAX
        }
    }

    #[test]
    fn calculates_ecam_address_from_absolute_bus_number() {
        let segment = EcamSegment::new(0x8000_0000, 0, 0x20, 0x2f).unwrap();
        assert_eq!(
            segment.function_address(0x22, 3, 5),
            Ok(0x8000_0000 + (0x22_u64 << 20) + (3_u64 << 15) + (5_u64 << 12))
        );
    }

    #[test]
    fn rejects_coordinates_outside_segment() {
        let segment = EcamSegment::new(0x8000_0000, 0, 4, 8).unwrap();
        assert_eq!(
            segment.function_address(3, 0, 0),
            Err(PcieError::InvalidBusRange)
        );
        assert_eq!(
            segment.function_address(4, 32, 0),
            Err(PcieError::InvalidDevice)
        );
        assert_eq!(
            segment.function_address(4, 0, 8),
            Err(PcieError::InvalidFunction)
        );
    }

    #[test]
    fn enumerates_present_function_and_decodes_class() {
        let segment = EcamSegment::new(0x8000_0000, 7, 0, 0).unwrap();
        let address = segment.function_address(0, 2, 0).unwrap();
        let access = FakeConfig {
            address,
            identity: 0x1234_8086,
            class: 0x0108_0201,
            header: 0x0080_0000,
        };
        let mut observed = None;

        let summary = enumerate_segment(&access, segment, |function| {
            observed = Some(function);
        })
        .unwrap();

        let function = observed.unwrap();
        assert_eq!(summary.function_count, 1);
        assert_eq!(summary.storage_controller_count, 1);
        assert_eq!(function.vendor_id, 0x8086);
        assert_eq!(function.device_id, 0x1234);
        assert_eq!(function.class_code, 0x01);
        assert_eq!(function.subclass, 0x08);
        assert_eq!(function.programming_interface, 0x02);
        assert_eq!(function.revision_id, 0x01);
        assert_eq!(function.header_type, 0x80);
        assert_eq!(function.segment_group, 7);
        assert_eq!(function.device, 2);
    }
}
