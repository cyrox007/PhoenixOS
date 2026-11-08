//! Проверка современных PCIe-возможностей VirtIO без записи в устройство.
//!
//! Используется существующее чтение ECAM. Любая цепочка с циклом,
//! выходом за пределы конфигурации или неправильной областью BAR отвергается.

use crate::graphics::{VIRTIO_GPU_DEVICE_ID, VIRTIO_VENDOR_ID};
use crate::{ConfigAccess, PcieFunction};

pub const COMMON_CFG: u8 = 1;
pub const NOTIFY_CFG: u8 = 2;
pub const DEVICE_CFG: u8 = 4;
const PCI_VENDOR_CAPABILITY: u8 = 0x09;
const MIN_CAP_PTR: u8 = 0x40;
const CAPABILITY_POINTER: u8 = 0x34;
const MAX_CAPABILITIES: usize = 48;
const BAR_START: u64 = 0x10;
const MMIO_WINDOW_BYTES: u64 = 4 * 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioPciError {
    UnsupportedDevice,
    MissingCapabilities,
    InvalidCapability,
    CapabilityLoop,
    MissingRegion,
    InvalidBar,
    IncompatibleRegions,
    AddressOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioRegion {
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioPciLayout {
    pub mmio_base: u64,
    pub common: VirtioRegion,
    pub notify: VirtioRegion,
    pub device: VirtioRegion,
    pub notify_multiplier: u32,
}

impl VirtioPciLayout {
    pub fn notify_address(self, queue_notify_off: u16) -> Result<u64, VirtioPciError> {
        let relative = u64::from(queue_notify_off)
            .checked_mul(u64::from(self.notify_multiplier))
            .ok_or(VirtioPciError::AddressOverflow)?;
        if relative
            .checked_add(2)
            .is_none_or(|end| end > u64::from(self.notify.length))
        {
            return Err(VirtioPciError::InvalidCapability);
        }
        u64::from(self.notify.offset)
            .checked_add(relative)
            .ok_or(VirtioPciError::AddressOverflow)
    }
}

fn field<A: ConfigAccess>(access: &A, function: PcieFunction, offset: u8) -> u8 {
    let word = access.read_u32(function.config_address + u64::from(offset & !3));
    (word >> (8 * (offset & 3))) as u8
}

fn word<A: ConfigAccess>(access: &A, function: PcieFunction, offset: u8) -> u32 {
    access.read_u32(function.config_address + u64::from(offset))
}

fn valid_region(region: VirtioRegion, minimum_length: u32) -> bool {
    let end = u64::from(region.offset).checked_add(u64::from(region.length));
    region.length >= minimum_length && end.is_some_and(|end| end <= MMIO_WINDOW_BYTES)
}

/// Разбирает ограничения QEMU VirtIO-GPU: общая конфигурация,
/// уведомления и сведения об устройстве внутри одного BAR до 16 КиБ.
pub fn discover<A: ConfigAccess>(
    access: &A,
    function: PcieFunction,
) -> Result<VirtioPciLayout, VirtioPciError> {
    if function.vendor_id != VIRTIO_VENDOR_ID
        || function.device_id != VIRTIO_GPU_DEVICE_ID
        || function.class_code != 0x03
        || function.header_type & 0x7f != 0
    {
        return Err(VirtioPciError::UnsupportedDevice);
    }

    let mut pointer = field(access, function, CAPABILITY_POINTER);
    if pointer == 0 {
        return Err(VirtioPciError::MissingCapabilities);
    }

    let mut common = None;
    let mut notify = None;
    let mut device = None;
    let mut multiplier = 0;
    let mut visited = [false; 64];

    for _ in 0..MAX_CAPABILITIES {
        if pointer == 0 {
            break;
        }
        if pointer < MIN_CAP_PTR || pointer & 3 != 0 {
            return Err(VirtioPciError::InvalidCapability);
        }
        let index = usize::from(pointer / 4);
        if visited[index] {
            return Err(VirtioPciError::CapabilityLoop);
        }
        visited[index] = true;

        let id = field(access, function, pointer);
        let next = field(access, function, pointer + 1);
        if id == PCI_VENDOR_CAPABILITY {
            if pointer > 0xec {
                return Err(VirtioPciError::InvalidCapability);
            }
            let cap_length = field(access, function, pointer + 2);
            let cfg_type = field(access, function, pointer + 3);
            let bar = field(access, function, pointer + 4);
            if cap_length < 16 || bar > 5 {
                return Err(VirtioPciError::InvalidCapability);
            }
            let region = VirtioRegion {
                bar,
                offset: word(access, function, pointer + 8),
                length: word(access, function, pointer + 12),
            };
            match cfg_type {
                COMMON_CFG if common.is_none() => common = Some(region),
                NOTIFY_CFG if notify.is_none() => {
                    if cap_length < 20 || pointer > 0xe8 {
                        return Err(VirtioPciError::InvalidCapability);
                    }
                    notify = Some(region);
                    multiplier = word(access, function, pointer + 16);
                }
                DEVICE_CFG if device.is_none() => device = Some(region),
                _ => {}
            }
        }
        pointer = next;
    }

    if pointer != 0 {
        return Err(VirtioPciError::CapabilityLoop);
    }
    let common = common.ok_or(VirtioPciError::MissingRegion)?;
    let notify = notify.ok_or(VirtioPciError::MissingRegion)?;
    let device = device.ok_or(VirtioPciError::MissingRegion)?;
    if common.bar != notify.bar || common.bar != device.bar {
        return Err(VirtioPciError::IncompatibleRegions);
    }
    if !valid_region(common, 56) || !valid_region(notify, 2) || !valid_region(device, 16) {
        return Err(VirtioPciError::InvalidCapability);
    }

    let bar_addr = function
        .config_address
        .checked_add(BAR_START + u64::from(common.bar) * 4)
        .ok_or(VirtioPciError::AddressOverflow)?;
    let bar_value = access.read_u32(bar_addr);
    if bar_value & 1 != 0 || bar_value & 6 == 2 || bar_value & 6 == 6 {
        return Err(VirtioPciError::InvalidBar);
    }
    let mut mmio_base = u64::from(bar_value & !0xf);
    if bar_value & 6 == 4 {
        if common.bar == 5 {
            return Err(VirtioPciError::InvalidBar);
        }
        let high = access.read_u32(bar_addr + 4);
        mmio_base |= u64::from(high) << 32;
    }
    if mmio_base == 0 || mmio_base & 4095 != 0 {
        return Err(VirtioPciError::InvalidBar);
    }
    mmio_base
        .checked_add(MMIO_WINDOW_BYTES - 1)
        .ok_or(VirtioPciError::AddressOverflow)?;
    Ok(VirtioPciLayout {
        mmio_base,
        common,
        notify,
        device,
        notify_multiplier: multiplier,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeConfig {
        data: [u32; 64],
        base: u64,
    }

    impl ConfigAccess for FakeConfig {
        fn read_u32(&self, address: u64) -> u32 {
            let Some(offset) = address.checked_sub(self.base) else {
                return u32::MAX;
            };
            let Ok(index) = usize::try_from(offset / 4) else {
                return u32::MAX;
            };
            self.data.get(index).copied().unwrap_or(u32::MAX)
        }
    }

    fn gpu() -> PcieFunction {
        PcieFunction {
            segment_group: 0,
            bus: 0,
            device: 3,
            function: 0,
            vendor_id: VIRTIO_VENDOR_ID,
            device_id: VIRTIO_GPU_DEVICE_ID,
            class_code: 0x03,
            subclass: 0,
            programming_interface: 0,
            revision_id: 1,
            header_type: 0,
            config_address: 0x8000_0000,
        }
    }

    fn setup() -> FakeConfig {
        let mut data = [0_u32; 64];
        data[0x34 / 4] = 0x80;
        data[0x80 / 4] = 0x0014_0009 | (0xa0 << 8) | ((COMMON_CFG as u32) << 24);
        data[0x84 / 4] = 4;
        data[0x88 / 4] = 0;
        data[0x8c / 4] = 56;
        data[0xa0 / 4] = 0x0014_0009 | (0xc0 << 8) | ((NOTIFY_CFG as u32) << 24);
        data[0xa4 / 4] = 4;
        data[0xa8 / 4] = 0x3000;
        data[0xac / 4] = 4096;
        data[0xb0 / 4] = 4;
        data[0xc0 / 4] = 0x0010_0009 | ((DEVICE_CFG as u32) << 24);
        data[0xc4 / 4] = 4;
        data[0xc8 / 4] = 0x2000;
        data[0xcc / 4] = 16;
        data[0x20 / 4] = 0x8000_0004;
        data[0x24 / 4] = 0x0000_0038;
        FakeConfig {
            data,
            base: gpu().config_address,
        }
    }

    #[test]
    fn parses_capabilities_and_notification_offset() {
        let config = setup();
        let layout = discover(&config, gpu()).unwrap();
        assert_eq!(layout.mmio_base, 0x38_8000_0000);
        assert_eq!(layout.common.offset, 0);
        assert_eq!(layout.notify_address(2), Ok(0x3008));
        assert_eq!(
            layout.notify_address(2048),
            Err(VirtioPciError::InvalidCapability)
        );
    }

    #[test]
    fn rejects_capability_cycle_and_mismatched_bars() {
        let mut config = setup();
        config.data[0xc0 / 4] |= 0x80 << 8;
        assert_eq!(
            discover(&config, gpu()),
            Err(VirtioPciError::CapabilityLoop)
        );
        config = setup();
        config.data[0xc4 / 4] = 0;
        assert_eq!(
            discover(&config, gpu()),
            Err(VirtioPciError::IncompatibleRegions)
        );
    }

    #[test]
    fn refuses_unknown_device_or_missing_regions() {
        let config = setup();
        let mut unknown = gpu();
        unknown.vendor_id = 0x1002;
        assert_eq!(
            discover(&config, unknown),
            Err(VirtioPciError::UnsupportedDevice)
        );
        let mut broken = setup();
        broken.data[0x34 / 4] = 0;
        assert_eq!(
            discover(&broken, gpu()),
            Err(VirtioPciError::MissingCapabilities)
        );
    }
}
