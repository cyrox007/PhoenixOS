//! Проверка регистров возможностей xHCI перед запуском контроллера.
//!
//! Только чтение: этот модуль не запускает устройство и не разрешает DMA.
//! Владелец MMIO обязан заранее создать корректное отображение регистров.

pub const CAPLENGTH_HCIVERSION: u64 = 0x00;
pub const HCSPARAMS1: u64 = 0x04;
pub const HCCPARAMS1: u64 = 0x10;
pub const DBOFF: u64 = 0x14;
pub const RTSOFF: u64 = 0x18;
pub const MINIMUM_CAPABILITY_BYTES: u64 = 0x1c;
pub const MINIMUM_OPERATIONAL_BYTES: u64 = 0x18;
pub const MINIMUM_RUNTIME_BYTES: u64 = 0x20;

pub trait RegisterAccess {
    fn read_u32(&self, offset: u64) -> u32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityError {
    MmioTooSmall,
    InvalidCapabilityLength,
    UnsupportedVersion,
    MissingSlots,
    MissingPorts,
    InvalidOffsets,
    AddressOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XhciCapabilities {
    pub version: u16,
    pub max_slots: u8,
    pub max_ports: u8,
    pub context_size_bytes: u8,
    pub operational_offset: u64,
    pub doorbell_offset: u64,
    pub runtime_offset: u64,
}

impl XhciCapabilities {
    /// Смещения проверяются относительно полного диапазона BAR, не только
    /// относительно первых регистров возможностей.
    pub fn probe<A: RegisterAccess>(registers: &A, bar_size: u64) -> Result<Self, CapabilityError> {
        if bar_size < MINIMUM_CAPABILITY_BYTES {
            return Err(CapabilityError::MmioTooSmall);
        }
        let header = registers.read_u32(CAPLENGTH_HCIVERSION);
        let operational_offset = u64::from(header & 0xff);
        if operational_offset < MINIMUM_CAPABILITY_BYTES || operational_offset % 4 != 0 {
            return Err(CapabilityError::InvalidCapabilityLength);
        }
        let version = (header >> 16) as u16;
        if !(0x0096..=0x0120).contains(&version) {
            return Err(CapabilityError::UnsupportedVersion);
        }
        let hcs = registers.read_u32(HCSPARAMS1);
        let max_slots = (hcs & 0xff) as u8;
        let max_ports = (hcs >> 24) as u8;
        if max_slots == 0 {
            return Err(CapabilityError::MissingSlots);
        }
        if max_ports == 0 {
            return Err(CapabilityError::MissingPorts);
        }
        let hcc = registers.read_u32(HCCPARAMS1);
        let doorbell_offset = u64::from(registers.read_u32(DBOFF) & !3);
        let runtime_offset = u64::from(registers.read_u32(RTSOFF) & !31);
        let operational_end = operational_offset
            .checked_add(MINIMUM_OPERATIONAL_BYTES)
            .ok_or(CapabilityError::AddressOverflow)?;
        let doorbell_end = doorbell_offset
            .checked_add(u64::from(max_slots) * 4 + 4)
            .ok_or(CapabilityError::AddressOverflow)?;
        let runtime_end = runtime_offset
            .checked_add(MINIMUM_RUNTIME_BYTES)
            .ok_or(CapabilityError::AddressOverflow)?;
        if operational_end > bar_size
            || doorbell_offset == 0
            || doorbell_end > bar_size
            || runtime_offset == 0
            || runtime_end > bar_size
        {
            return Err(CapabilityError::InvalidOffsets);
        }
        Ok(Self {
            version,
            max_slots,
            max_ports,
            context_size_bytes: if hcc & (1 << 2) != 0 { 64 } else { 32 },
            operational_offset,
            doorbell_offset,
            runtime_offset,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRegisters {
        header: u32,
        hcs: u32,
        hcc: u32,
        doorbell: u32,
        runtime: u32,
    }

    impl RegisterAccess for FakeRegisters {
        fn read_u32(&self, offset: u64) -> u32 {
            match offset {
                CAPLENGTH_HCIVERSION => self.header,
                HCSPARAMS1 => self.hcs,
                HCCPARAMS1 => self.hcc,
                DBOFF => self.doorbell,
                RTSOFF => self.runtime,
                _ => u32::MAX,
            }
        }
    }

    fn valid() -> FakeRegisters {
        FakeRegisters {
            header: 0x0110_0040,
            hcs: 4 | (8 << 24),
            hcc: 1 << 2,
            doorbell: 0x1000,
            runtime: 0x2000,
        }
    }

    #[test]
    fn parses_valid_controller_without_writes() {
        let result = XhciCapabilities::probe(&valid(), 0x3000).unwrap();
        assert_eq!(result.version, 0x0110);
        assert_eq!(result.max_slots, 4);
        assert_eq!(result.max_ports, 8);
        assert_eq!(result.context_size_bytes, 64);
        assert_eq!(result.operational_offset, 0x40);
        assert_eq!(result.doorbell_offset, 0x1000);
        assert_eq!(result.runtime_offset, 0x2000);
    }

    #[test]
    fn rejects_invalid_layout_and_missing_resources() {
        assert_eq!(
            XhciCapabilities::probe(&valid(), 0x10),
            Err(CapabilityError::MmioTooSmall)
        );
        let mut data = valid();
        data.header = 0x0110_0010;
        assert_eq!(
            XhciCapabilities::probe(&data, 0x3000),
            Err(CapabilityError::InvalidCapabilityLength)
        );
        data = valid();
        data.header = 0x0200_0040;
        assert_eq!(
            XhciCapabilities::probe(&data, 0x3000),
            Err(CapabilityError::UnsupportedVersion)
        );
        data = valid();
        data.hcs = 8 << 24;
        assert_eq!(
            XhciCapabilities::probe(&data, 0x3000),
            Err(CapabilityError::MissingSlots)
        );
        data = valid();
        data.hcs = 4;
        assert_eq!(
            XhciCapabilities::probe(&data, 0x3000),
            Err(CapabilityError::MissingPorts)
        );
        data = valid();
        data.runtime = 0x3000;
        assert_eq!(
            XhciCapabilities::probe(&data, 0x3000),
            Err(CapabilityError::InvalidOffsets)
        );
        data = valid();
        data.doorbell = 0x3000;
        assert_eq!(
            XhciCapabilities::probe(&data, 0x3000),
            Err(CapabilityError::InvalidOffsets)
        );
    }

    #[test]
    fn accepts_32_byte_context_and_masks_reserved_offset_bits() {
        let mut data = valid();
        data.hcc = 0;
        data.doorbell = 0x1003;
        data.runtime = 0x201f;
        let result = XhciCapabilities::probe(&data, 0x3000).unwrap();
        assert_eq!(result.context_size_bytes, 32);
        assert_eq!(result.doorbell_offset, 0x1000);
        assert_eq!(result.runtime_offset, 0x2000);
    }
}
