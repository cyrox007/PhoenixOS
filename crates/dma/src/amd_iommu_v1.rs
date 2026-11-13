//! Реальное программирование таблиц и очереди AMD IOMMU v1.
//!
//! Модуль не получает права обращаться к устройствам самостоятельно.
//! Вызывающая сторона обязана проверить диапазон регистров, тип
//! кэширования (UC), владение физическими страницами, идентификатор
//! устройства и его полную остановку.
//!
//! Переход к физической аппаратуре ASUS ЗАПРЕЩЁН до отдельной приёмки.
//! Поддерживается только ограниченное испытание на QEMU AMD-Vi.

use core::sync::atomic::{Ordering, fence};

use crate::iommu_domain::{DomainFault, IommuEngine};

const PAGE: u64 = 4096;
const TWO_MIB: u64 = 2 * 1024 * 1024;
const MAX_PHYSICAL: u64 = 0x000f_ffff_ffff_f000;
const VALID: u64 = 1;
const TRANSLATION_VALID: u64 = 2;
const PERM_READ: u64 = 1 << 61;
const PERM_WRITE: u64 = 1 << 62;
const READ_WRITE: u64 = PERM_READ | PERM_WRITE;
const COMMAND_CAPACITY: u64 = PAGE / 16;
const COMMAND_SIZE_EXPONENT: u64 = 8;
const DEVICE_ENTRY_BYTES: u64 = 32;

pub const MMIO_DEVICE_TABLE: u64 = 0x0000;
pub const MMIO_COMMAND_BASE: u64 = 0x0008;
pub const MMIO_CONTROL: u64 = 0x0018;
pub const MMIO_COMMAND_HEAD: u64 = 0x2000;
pub const MMIO_COMMAND_TAIL: u64 = 0x2008;
pub const MMIO_STATUS: u64 = 0x2020;
const CONTROL_ENABLE: u64 = 1;
const CONTROL_COMMAND_BUFFER_ENABLE: u64 = 1 << 12;
const STATUS_COMMAND_RUNNING: u64 = 1 << 4;

/// Предоставляется только проверенным устройством и отображением MMIO.
/// Все чтения и записи в физическую память должны быть упорядочены.
pub trait AmdIommuIo {
    fn mmio_read(&self, offset: u64) -> u64;
    fn mmio_write(&mut self, offset: u64, value: u64);
    fn memory_read(&self, address: u64) -> u64;
    fn memory_write(&mut self, address: u64, value: u64);
}

/// Источник подтверждения остановки устройства. Текущий статус PCI Bus Master
/// сам по себе не доказывает завершение уже поставленных команд.
pub trait StoppedDevice {
    fn is_fully_stopped(&mut self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareError {
    InvalidConfiguration,
    UnverifiedDevice,
    InvalidBuffer,
    Busy,
    CommandTimeout,
    EngineInactive,
    UnsafeExistingConfiguration,
    MappingAlreadyPresent,
    MappingNotFound,
    Faulted,
}

/// Все страницы предоставляются вызывающей стороной с исключительным
/// владением. Нельзя совпадать с RAM Windows, таблицами ядра и IVMD.
#[derive(Clone, Copy, Debug)]
pub struct AmdIommuPages {
    pub device_table: u64,
    pub command_buffer: u64,
    pub root: u64,
    pub level3: u64,
    pub level2: u64,
    pub level1: u64,
    pub completion: u64,
}

impl AmdIommuPages {
    fn all(self) -> [u64; 7] {
        [
            self.device_table,
            self.command_buffer,
            self.root,
            self.level3,
            self.level2,
            self.level1,
            self.completion,
        ]
    }

    fn validate(self, device_id: u16) -> Result<(), HardwareError> {
        let all = self.all();
        if device_id >= 128 {
            return Err(HardwareError::InvalidConfiguration);
        }
        for (index, address) in all.iter().enumerate() {
            if *address < PAGE || *address > MAX_PHYSICAL || *address % PAGE != 0 {
                return Err(HardwareError::InvalidConfiguration);
            }
            if all[..index].contains(address) {
                return Err(HardwareError::InvalidConfiguration);
            }
        }
        Ok(())
    }
}

/// Поддерживает только одну ограниченную область IOVA размером 2 МиБ,
/// одно заранее выбранное устройство (PCI BDF в шине 0) и явные
/// 4-КиБ отображения. Ничего не освобождает автоматически при ошибке.
pub struct AmdV1Engine<B: AmdIommuIo, D: StoppedDevice> {
    io: B,
    device: D,
    pages: AmdIommuPages,
    device_id: u16,
    domain_id: u16,
    aperture: u64,
    command_tail: u64,
    sequence: u64,
    active: bool,
    poisoned: bool,
    mapped: [bool; 512],
}

/// Допуск только явным вызовом после подтверждённой остановки EDU QEMU.
/// `aperture` обязан быть выровнен по 2 МиБ, домен — ненулевой.
impl<B: AmdIommuIo, D: StoppedDevice> AmdV1Engine<B, D> {
    pub fn new(
        io: B,
        device: D,
        pages: AmdIommuPages,
        device_id: u16,
        domain_id: u16,
        aperture: u64,
    ) -> Result<Self, HardwareError> {
        pages.validate(device_id)?;
        if domain_id == 0 || aperture < TWO_MIB || aperture % TWO_MIB != 0 {
            return Err(HardwareError::InvalidConfiguration);
        }
        if aperture.checked_add(TWO_MIB).is_none() {
            return Err(HardwareError::InvalidConfiguration);
        }
        Ok(Self {
            io,
            device,
            pages,
            device_id,
            domain_id,
            aperture,
            command_tail: 0,
            sequence: 0,
            active: false,
            poisoned: false,
            mapped: [false; 512],
        })
    }

    fn zero_page(&mut self, address: u64) {
        for index in 0..512 {
            self.io.memory_write(address + index * 8, 0);
        }
    }

    fn entry_address(&self) -> u64 {
        self.pages.device_table + u64::from(self.device_id) * DEVICE_ENTRY_BYTES
    }

    fn build_tables(&mut self) {
        for address in self.pages.all() {
            self.zero_page(address);
        }
        let index4 = (self.aperture >> 39) & 0x1ff;
        let index3 = (self.aperture >> 30) & 0x1ff;
        let index2 = (self.aperture >> 21) & 0x1ff;
        // Директории содержат единственный явно разрешённый путь.
        let level4_entry = self.pages.level3 | ((3 << 9) | VALID | READ_WRITE);
        let level3_entry = self.pages.level2 | ((2 << 9) | VALID | READ_WRITE);
        let level2_entry = self.pages.level1 | ((1 << 9) | VALID | READ_WRITE);
        self.io.memory_write(self.pages.root + index4 * 8, level4_entry);
        self.io.memory_write(self.pages.level3 + index3 * 8, level3_entry);
        self.io.memory_write(self.pages.level2 + index2 * 8, level2_entry);
        let dte = self.pages.root | (4 << 9) | VALID | TRANSLATION_VALID | READ_WRITE;
        self.io.memory_write(self.entry_address(), dte);
        self.io
            .memory_write(self.entry_address() + 8, u64::from(self.domain_id));
        fence(Ordering::SeqCst);
    }

    /// Включение изолированного домена. Не перенастраивает работающий
    /// IOMMU: чужая/действующая конфигурация строго отклоняется.
    pub fn initialize(&mut self) -> Result<(), HardwareError> {
        if self.active || self.poisoned {
            return Err(HardwareError::Faulted);
        }
        if !self.device.is_fully_stopped() {
            return Err(HardwareError::Busy);
        }
        if self.io.mmio_read(MMIO_CONTROL) & (CONTROL_ENABLE | CONTROL_COMMAND_BUFFER_ENABLE) != 0 {
            return Err(HardwareError::UnsafeExistingConfiguration);
        }

        self.build_tables();
        self.io.mmio_write(MMIO_DEVICE_TABLE, self.pages.device_table);
        self.io.mmio_write(MMIO_COMMAND_BASE, self.pages.command_buffer | COMMAND_SIZE_EXPONENT);
        self.io.mmio_write(MMIO_COMMAND_HEAD, 0);
        self.io.mmio_write(MMIO_COMMAND_TAIL, 0);
        self.io.mmio_write(
            MMIO_CONTROL,
            CONTROL_ENABLE | CONTROL_COMMAND_BUFFER_ENABLE,
        );
        fence(Ordering::SeqCst);

        if !self.hardware_active() {
            self.poisoned = true;
            return Err(HardwareError::EngineInactive);
        }
        self.active = true;
        self.issue_commands(&[(u64::from(self.device_id) | (2 << 60), 0)])?;
        Ok(())
    }

    fn hardware_active(&self) -> bool {
        let control = self.io.mmio_read(MMIO_CONTROL);
        let status = self.io.mmio_read(MMIO_STATUS);
        control & (CONTROL_ENABLE | CONTROL_COMMAND_BUFFER_ENABLE)
            == CONTROL_ENABLE | CONTROL_COMMAND_BUFFER_ENABLE
            && status & STATUS_COMMAND_RUNNING != 0
            && self.io.mmio_read(MMIO_DEVICE_TABLE) & MAX_PHYSICAL == self.pages.device_table
    }

    fn submit(&mut self, first: u64, second: u64) {
        let slot = self.pages.command_buffer + self.command_tail;
        self.io.memory_write(slot, first);
        self.io.memory_write(slot + 8, second);
        self.command_tail = (self.command_tail + 16) % PAGE;
    }

    /// Последняя команда требует аппаратной записи в отдельный флаг
    /// завершения: равенства указателей головы/хвоста недостаточно.
    fn issue_commands(&mut self, cmds: &[(u64, u64)]) -> Result<(), HardwareError> {
        if !self.active || self.poisoned || cmds.len() + 1 >= COMMAND_CAPACITY as usize {
            return Err(HardwareError::Faulted);
        }
        if !self.hardware_active() {
            self.poisoned = true;
            return Err(HardwareError::EngineInactive);
        }
        self.sequence = self.sequence.wrapping_add(1);
        if self.sequence == 0 {
            self.poisoned = true;
            return Err(HardwareError::Faulted);
        }
        // Сигнал не может быть принят от старой команды.
        self.io.memory_write(self.pages.completion, 0);
        for (first, second) in cmds.iter().copied() {
            self.submit(first, second);
        }
        self.submit((1 << 60) | self.pages.completion | 1, self.sequence);
        fence(Ordering::SeqCst);
        self.io.mmio_write(MMIO_COMMAND_TAIL, self.command_tail);
        for _ in 0..1_000_000 {
            fence(Ordering::Acquire);
            if self.io.memory_read(self.pages.completion) == self.sequence {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        self.poisoned = true;
        Err(HardwareError::CommandTimeout)
    }

    fn slot(&self, address: u64, length: u64) -> Result<usize, HardwareError> {
        if length != PAGE || address < self.aperture || address % PAGE != 0 {
            return Err(HardwareError::InvalidBuffer);
        }
        let index = (address - self.aperture) / PAGE;
        if index >= 512 {
            return Err(HardwareError::InvalidBuffer);
        }
        Ok(index as usize)
    }

    fn ensure_ready(&mut self) -> Result<(), HardwareError> {
        if !self.active || self.poisoned {
            return Err(HardwareError::Faulted);
        }
        if !self.hardware_active() {
            self.poisoned = true;
            return Err(HardwareError::EngineInactive);
        }
        if !self.device.is_fully_stopped() {
            return Err(HardwareError::Busy);
        }
        Ok(())
    }

    pub fn add_page(&mut self, address: u64, physical: u64) -> Result<(), HardwareError> {
        self.ensure_ready()?;
        let index = self.slot(address, PAGE)?;
        if physical < PAGE || physical & (PAGE - 1) != 0 || physical > MAX_PHYSICAL {
            return Err(HardwareError::InvalidBuffer);
        }
        if self.mapped[index] {
            return Err(HardwareError::MappingAlreadyPresent);
        }
        let leaf = self.pages.level1 + u64::try_from(index).unwrap_or(0) * 8;
        self.io.memory_write(leaf, physical | VALID | READ_WRITE);
        fence(Ordering::SeqCst);
        // Сброс кэша преобразований указанного домена.
        self.issue_commands(&[((3 << 60) | (u64::from(self.domain_id) << 32), 3)])?;
        self.mapped[index] = true;
        Ok(())
    }

    pub fn remove_page(&mut self, address: u64) -> Result<(), HardwareError> {
        self.ensure_ready()?;
        let index = self.slot(address, PAGE)?;
        if !self.mapped[index] {
            return Err(HardwareError::MappingNotFound);
        }
        self.io
            .memory_write(self.pages.level1 + u64::try_from(index).unwrap_or(0) * 8, 0);
        fence(Ordering::SeqCst);
        self.issue_commands(&[((3 << 60) | (u64::from(self.domain_id) << 32), 3)])?;
        self.mapped[index] = false;
        Ok(())
    }

    pub fn mapping_count(&self) -> usize {
        self.mapped.iter().filter(|entry| **entry).count()
    }

    pub fn faulted(&self) -> bool {
        self.poisoned
    }

    pub fn into_parts(self) -> (B, D) {
        (self.io, self.device)
    }
}

impl<B: AmdIommuIo, D: StoppedDevice> IommuEngine for AmdV1Engine<B, D> {
    fn translation_active(&self) -> bool {
        self.active && !self.poisoned && self.hardware_active()
    }

    fn device_in_private_domain(&self) -> bool {
        self.translation_active()
            && self.io.memory_read(self.entry_address())
                == (self.pages.root | (4 << 9) | VALID | TRANSLATION_VALID | READ_WRITE)
            && self.io.memory_read(self.entry_address() + 8) == u64::from(self.domain_id)
    }

    fn device_stopped(&mut self) -> Result<bool, DomainFault> {
        Ok(self.device.is_fully_stopped())
    }

    fn install_mapping(&mut self, address: u64, physical: u64, length: u64) -> Result<(), DomainFault> {
        if length != PAGE {
            return Err(DomainFault::InstallFailed);
        }
        self.add_page(address, physical)
            .map_err(|_| DomainFault::InstallFailed)
    }

    fn remove_mapping(&mut self, address: u64, length: u64) -> Result<(), DomainFault> {
        if length != PAGE {
            return Err(DomainFault::RemovalFailed);
        }
        self.remove_page(address).map_err(|_| DomainFault::RemovalFailed)
    }

    fn invalidate_and_wait(&mut self) -> Result<(), DomainFault> {
        self.issue_commands(&[((3 << 60) | (u64::from(self.domain_id) << 32), 3)])
            .map_err(|_| DomainFault::CacheInvalidationFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Fake {
        regs: BTreeMap<u64, u64>,
        pages: BTreeMap<u64, u64>,
        complete: bool,
    }

    impl Fake {
        fn new(complete: bool) -> Self {
            Self {
                regs: BTreeMap::new(),
                pages: BTreeMap::new(),
                complete,
            }
        }
    }

    impl AmdIommuIo for Fake {
        fn mmio_read(&self, offset: u64) -> u64 {
            self.regs.get(&offset).copied().unwrap_or(0)
        }

        fn mmio_write(&mut self, offset: u64, value: u64) {
            self.regs.insert(offset, value);
            if offset == MMIO_CONTROL {
                self.regs.insert(MMIO_STATUS, STATUS_COMMAND_RUNNING);
            }
            if offset == MMIO_COMMAND_TAIL && self.complete {
                let written = self.pages.get(&0x2000 + ((value + PAGE - 8) % PAGE)).copied();
                if let Some(sequence) = written {
                    self.pages.insert(0x7000, sequence);
                }
            }
        }

        fn memory_read(&self, address: u64) -> u64 {
            self.pages.get(&address).copied().unwrap_or(0)
        }

        fn memory_write(&mut self, address: u64, value: u64) {
            self.pages.insert(address, value);
        }
    }

    struct Device(bool);

    impl StoppedDevice for Device {
        fn is_fully_stopped(&mut self) -> bool {
            self.0
        }
    }

    fn pages() -> AmdIommuPages {
        AmdIommuPages {
            device_table: 0x1000,
            command_buffer: 0x2000,
            root: 0x3000,
            level3: 0x4000,
            level2: 0x5000,
            level1: 0x6000,
            completion: 0x7000,
        }
    }

    fn engine(complete: bool) -> AmdV1Engine<Fake, Device> {
        AmdV1Engine::new(Fake::new(complete), Device(true), pages(), 0x20, 7, 0x4000_0000).unwrap()
    }

    #[test]
    fn full_lifecycle_requires_real_completion() {
        let mut engine = engine(true);
        engine.initialize().unwrap();
        assert!(engine.translation_active());
        assert!(engine.device_in_private_domain());
        engine.add_page(0x4000_0000, 0x9000).unwrap();
        assert_eq!(engine.mapping_count(), 1);
        assert_eq!(
            engine.add_page(0x4000_0000, 0xa000),
            Err(HardwareError::MappingAlreadyPresent)
        );
        engine.remove_page(0x4000_0000).unwrap();
        assert_eq!(engine.mapping_count(), 0);
    }

    #[test]
    fn missing_completion_fails_closed() {
        let mut engine = engine(false);
        assert_eq!(engine.initialize(), Err(HardwareError::CommandTimeout));
        assert!(engine.faulted());
        assert!(!engine.translation_active());
    }

    #[test]
    fn missing_stop_rejects_write_to_controller() {
        let mut engine = AmdV1Engine::new(Fake::new(true), Device(false), pages(), 0x20, 7, 0x4000_0000).unwrap();
        assert_eq!(engine.initialize(), Err(HardwareError::Busy));
        assert!(!engine.translation_active());
    }

    #[test]
    fn invalid_pages_and_id_rejected() {
        let mut bad = pages();
        bad.completion = bad.root;
        assert!(matches!(
            AmdV1Engine::new(Fake::new(true), Device(true), bad, 2, 7, 0x4000_0000),
            Err(HardwareError::InvalidConfiguration)
        ));
        assert!(matches!(
            AmdV1Engine::new(Fake::new(true), Device(true), pages(), 128, 7, 0x4000_0000),
            Err(HardwareError::InvalidConfiguration)
        ));
    }

    #[test]
    fn outside_one_aperture_or_unaligned_fails() {
        let mut engine = engine(true);
        engine.initialize().unwrap();
        assert_eq!(
            engine.add_page(0x4020_0000, 0x9000),
            Err(HardwareError::InvalidBuffer)
        );
        assert_eq!(
            engine.add_page(0x4000_0000, 0x9001),
            Err(HardwareError::InvalidBuffer)
        );
    }

    #[test]
    fn existing_iommu_configuration_is_not_replaced() {
        let mut fake = Fake::new(true);
        fake.regs.insert(MMIO_CONTROL, CONTROL_ENABLE);
        let mut engine = AmdV1Engine::new(fake, Device(true), pages(), 0x20, 7, 0x4000_0000).unwrap();
        assert_eq!(engine.initialize(), Err(HardwareError::UnsafeExistingConfiguration));
    }
}
