#![no_std]

pub mod leases;

//! Проверяемые адреса и параметры прямого доступа к памяти.
//! Выделение и отображение страниц будут подключены отдельным серверным
//! механизмом. Сам этот модуль не выделяет память и не включает DMA.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    ZeroLength,
    InvalidAlignment,
    MisalignedAddress,
    AddressOverflow,
    Unaddressable,
    InvalidRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaLayout {
    length: usize,
    alignment: usize,
}

impl DmaLayout {
    pub const fn new(length: usize, alignment: usize) -> Result<Self, DmaError> {
        if length == 0 {
            return Err(DmaError::ZeroLength);
        }
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(DmaError::InvalidAlignment);
        }
        Ok(Self { length, alignment })
    }

    pub const fn length(self) -> usize {
        self.length
    }

    pub const fn alignment(self) -> usize {
        self.alignment
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuVirtualAddress(usize);

impl CpuVirtualAddress {
    pub const fn new(address: usize) -> Self {
        Self(address)
    }

    pub const fn value(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceAddress(u64);

impl DeviceAddress {
    pub const fn new(address: u64) -> Self {
        Self(address)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaRegion {
    cpu: CpuVirtualAddress,
    device: DeviceAddress,
    layout: DmaLayout,
}

impl DmaRegion {
    pub fn new(
        cpu: CpuVirtualAddress,
        device: DeviceAddress,
        layout: DmaLayout,
    ) -> Result<Self, DmaError> {
        let length = layout.length();
        let alignment = layout.alignment();
        if cpu.value() % alignment != 0 || device.value() % alignment as u64 != 0 {
            return Err(DmaError::MisalignedAddress);
        }
        if cpu.value().checked_add(length - 1).is_none()
            || device.value().checked_add((length - 1) as u64).is_none()
        {
            return Err(DmaError::AddressOverflow);
        }
        Ok(Self {
            cpu,
            device,
            layout,
        })
    }

    pub const fn cpu_address(self) -> CpuVirtualAddress {
        self.cpu
    }

    pub const fn device_address(self) -> DeviceAddress {
        self.device
    }

    pub const fn layout(self) -> DmaLayout {
        self.layout
    }

    pub fn device_offset(self, offset: usize, size: usize) -> Result<DeviceAddress, DmaError> {
        if size == 0 || offset > self.layout.length() || size > self.layout.length() - offset {
            return Err(DmaError::InvalidRange);
        }
        let offset = u64::try_from(offset).map_err(|_| DmaError::Unaddressable)?;
        let address = self
            .device
            .value()
            .checked_add(offset)
            .ok_or(DmaError::AddressOverflow)?;
        Ok(DeviceAddress::new(address))
    }
}

/// Аппаратный слой может переопределить отображение адреса устройства через IOMMU.
/// Адрес процессора нельзя автоматически считать доступным для устройства.
pub trait DmaAddressTranslator {
    fn translate(
        &self,
        cpu: CpuVirtualAddress,
        layout: DmaLayout,
    ) -> Result<DeviceAddress, DmaError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_layouts() {
        assert_eq!(DmaLayout::new(0, 16), Err(DmaError::ZeroLength));
        assert_eq!(DmaLayout::new(128, 0), Err(DmaError::InvalidAlignment));
        assert_eq!(DmaLayout::new(128, 3), Err(DmaError::InvalidAlignment));
    }

    #[test]
    fn checks_both_address_spaces() {
        let layout = DmaLayout::new(4096, 4096).unwrap();
        assert_eq!(
            DmaRegion::new(
                CpuVirtualAddress::new(3),
                DeviceAddress::new(0x2000),
                layout
            ),
            Err(DmaError::MisalignedAddress)
        );
        assert_eq!(
            DmaRegion::new(
                CpuVirtualAddress::new(0x1000),
                DeviceAddress::new(3),
                layout
            ),
            Err(DmaError::MisalignedAddress)
        );
        assert!(
            DmaRegion::new(
                CpuVirtualAddress::new(0x1000),
                DeviceAddress::new(0x2000),
                layout
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_overflow_and_out_of_bounds() {
        let layout = DmaLayout::new(4096, 1).unwrap();
        assert_eq!(
            DmaRegion::new(
                CpuVirtualAddress::new(usize::MAX),
                DeviceAddress::new(0),
                layout
            ),
            Err(DmaError::AddressOverflow)
        );
        assert_eq!(
            DmaRegion::new(
                CpuVirtualAddress::new(0),
                DeviceAddress::new(u64::MAX),
                layout
            ),
            Err(DmaError::AddressOverflow)
        );
        let region = DmaRegion::new(
            CpuVirtualAddress::new(0),
            DeviceAddress::new(0x2000),
            layout,
        )
        .unwrap();
        assert_eq!(region.device_offset(4095, 1).unwrap().value(), 0x2fff);
        assert_eq!(region.device_offset(4095, 2), Err(DmaError::InvalidRange));
        assert_eq!(region.device_offset(0, 0), Err(DmaError::InvalidRange));
    }
}
