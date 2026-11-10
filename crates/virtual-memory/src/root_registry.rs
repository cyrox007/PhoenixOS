//! Ограниченный реестр корней CR3, создаваемых самим ядром.
//! Реестр не претендует на полноту внешних таблиц загрузчика и других ядер.
//! При любой ошибке глобальный экземпляр необратимо запрещает доверять снимкам.

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::dma_evidence::{MAX_ROOTS, current_mapping_epoch};

// Ограничение ожидания исключает бесконечное зависание при неисправном владельце.
const MAX_LOCK_ATTEMPTS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootKind {
    Boot,
    Process,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RootEntry {
    address: u64,
    kind: RootKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootError {
    InvalidAddress,
    MissingBootRoot,
    DifferentBootRoot,
    DuplicateRoot,
    MissingRoot,
    NotProcessRoot,
    Capacity,
    ConcurrentAccess,
    Compromised,
}

/// Снимок учитывает только корни, созданные через подконтрольные пути ядра.
/// Полное аппаратное покрытие других CR3/CPU этим снимком НЕ подтверждается.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootSnapshot {
    roots: [u64; MAX_ROOTS],
    count: usize,
    boot: u64,
    mapping_epoch: Option<u64>,
}

impl RootSnapshot {
    pub fn roots(&self) -> &[u64] {
        &self.roots[..self.count]
    }

    pub fn boot_root(&self) -> u64 {
        self.boot
    }

    pub fn mapping_epoch(&self) -> Option<u64> {
        self.mapping_epoch
    }

    /// Без межъядерной остановки и проверки внешних CR3 полный допуск запрещён.
    pub fn physical_coverage_verified(&self) -> bool {
        false
    }
}

/// Самостоятельная структура для проверок. Глобальная обёртка ниже
/// обеспечивает исключительный доступ и консервативный отказ.
pub struct RootRegistry {
    entries: [Option<RootEntry>; MAX_ROOTS],
    count: usize,
    boot: Option<u64>,
}

impl RootRegistry {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_ROOTS],
            count: 0,
            boot: None,
        }
    }

    fn valid_root(address: u64) -> bool {
        address != 0 && address & 4095 == 0
    }

    fn find(&self, address: u64) -> Option<usize> {
        self.entries[..self.count]
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.address == address))
    }

    fn push(&mut self, address: u64, kind: RootKind) -> Result<(), RootError> {
        if !Self::valid_root(address) {
            return Err(RootError::InvalidAddress);
        }
        if self.find(address).is_some() {
            return Err(RootError::DuplicateRoot);
        }
        if self.count == MAX_ROOTS {
            return Err(RootError::Capacity);
        }
        self.entries[self.count] = Some(RootEntry { address, kind });
        self.count += 1;
        Ok(())
    }

    pub fn register_boot(&mut self, address: u64) -> Result<(), RootError> {
        if !Self::valid_root(address) {
            return Err(RootError::InvalidAddress);
        }
        if let Some(boot) = self.boot {
            return if boot == address {
                Ok(())
            } else {
                Err(RootError::DifferentBootRoot)
            };
        }
        self.push(address, RootKind::Boot)?;
        self.boot = Some(address);
        Ok(())
    }

    /// Первый наблюдаемый корень считается системным; последующие должны
    /// уже существовать в реестре, в том числе корни процессов.
    pub fn observe_current(&mut self, address: u64) -> Result<(), RootError> {
        if self.boot.is_none() {
            return self.register_boot(address);
        }
        if self.contains(address) {
            return Ok(());
        }
        Err(RootError::MissingRoot)
    }

    pub fn register_process(&mut self, address: u64) -> Result<(), RootError> {
        if self.boot.is_none() {
            return Err(RootError::MissingBootRoot);
        }
        self.push(address, RootKind::Process)
    }

    pub fn unregister_process(&mut self, address: u64) -> Result<(), RootError> {
        let Some(index) = self.find(address) else {
            return Err(RootError::MissingRoot);
        };
        if self.entries[index].is_none_or(|item| item.kind != RootKind::Process) {
            return Err(RootError::NotProcessRoot);
        }
        self.count -= 1;
        self.entries[index] = self.entries[self.count];
        self.entries[self.count] = None;
        Ok(())
    }

    pub fn contains(&self, address: u64) -> bool {
        self.find(address).is_some()
    }

    pub fn snapshot(&self, epoch: Option<u64>) -> Result<RootSnapshot, RootError> {
        let boot = self.boot.ok_or(RootError::MissingBootRoot)?;
        let mut roots = [0; MAX_ROOTS];
        for (index, entry) in self.entries[..self.count].iter().enumerate() {
            roots[index] = entry.ok_or(RootError::MissingRoot)?.address;
        }
        Ok(RootSnapshot {
            roots,
            count: self.count,
            boot,
            mapping_epoch: epoch,
        })
    }
}

impl Default for RootRegistry {
    fn default() -> Self {
        Self::new()
    }
}

struct SharedRegistry {
    locked: AtomicBool,
    compromised: AtomicBool,
    state: UnsafeCell<RootRegistry>,
}

// Доступ к UnsafeCell выдаётся только после исключительного захвата locked.
// На физическом ядре прерывания отключаются на время короткой операции реестра.
unsafe impl Sync for SharedRegistry {}

impl SharedRegistry {
    fn access<T>(
        &self,
        operation: impl FnOnce(&mut RootRegistry) -> Result<T, RootError>,
    ) -> Result<T, RootError> {
        #[cfg(target_os = "none")]
        {
            x86_64::instructions::interrupts::without_interrupts(|| self.access_locked(operation))
        }
        #[cfg(not(target_os = "none"))]
        {
            self.access_locked(operation)
        }
    }

    fn access_locked<T>(
        &self,
        operation: impl FnOnce(&mut RootRegistry) -> Result<T, RootError>,
    ) -> Result<T, RootError> {
        if self.compromised.load(Ordering::Acquire) {
            return Err(RootError::Compromised);
        }

        for _ in 0..MAX_LOCK_ATTEMPTS {
            if self
                .locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                // Все изменения под исключительной блокировкой.
                // Ошибка структуры необратимо запрещает доверие; обычное
                // соперничество процессоров само по себе структуру не портит.
                let result = operation(unsafe { &mut *self.state.get() });
                if result.is_err() {
                    self.compromised.store(true, Ordering::Release);
                }
                self.locked.store(false, Ordering::Release);
                return result;
            }
            spin_loop();
        }

        // Вызов отклонён без изменений реестра. Его вызывающий код обязан
        // сохранить владение памятью и не выдавать допуск физическому DMA.
        Err(RootError::ConcurrentAccess)
    }
}

static REGISTRY: SharedRegistry = SharedRegistry {
    locked: AtomicBool::new(false),
    compromised: AtomicBool::new(false),
    state: UnsafeCell::new(RootRegistry::new()),
};

/// Вызывается только для текущего доверенного корня ядра.
pub fn register_boot_root(address: u64) -> Result<(), RootError> {
    REGISTRY.access(|state| state.register_boot(address))
}

/// Все процессы ядра должны проходить через этот путь создания CR3.
pub fn observe_current_root(address: u64) -> Result<(), RootError> {
    REGISTRY.access(|state| state.observe_current(address))
}

pub fn register_process_root(address: u64) -> Result<(), RootError> {
    REGISTRY.access(|state| state.register_process(address))
}

pub fn unregister_process_root(address: u64) -> Result<(), RootError> {
    REGISTRY.access(|state| state.unregister_process(address))
}

pub fn contains_root(address: u64) -> Result<bool, RootError> {
    REGISTRY.access(|state| Ok(state.contains(address)))
}

pub fn snapshot() -> Result<RootSnapshot, RootError> {
    REGISTRY.access(|state| state.snapshot(current_mapping_epoch()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contention_does_not_irreversibly_poison_registry() {
        let registry = SharedRegistry {
            locked: AtomicBool::new(true),
            compromised: AtomicBool::new(false),
            state: UnsafeCell::new(RootRegistry::new()),
        };
        assert_eq!(
            registry.access(|state| state.register_boot(0x1000)),
            Err(RootError::ConcurrentAccess)
        );
        assert!(!registry.compromised.load(Ordering::Acquire));

        registry.locked.store(false, Ordering::Release);
        registry
            .access(|state| state.register_boot(0x1000))
            .unwrap();
        assert!(registry.access(|state| Ok(state.contains(0x1000))).unwrap());
    }

    #[test]
    fn structural_error_still_blocks_further_registry_access() {
        let registry = SharedRegistry {
            locked: AtomicBool::new(false),
            compromised: AtomicBool::new(false),
            state: UnsafeCell::new(RootRegistry::new()),
        };
        registry.access(|state| state.register_boot(0x1000)).unwrap();
        assert_eq!(
            registry.access(|state| state.register_process(0x1000)),
            Err(RootError::DuplicateRoot)
        );
        assert_eq!(
            registry.access(|state| Ok(state.contains(0x1000))),
            Err(RootError::Compromised)
        );
    }

    #[test]
    fn lifecycle_of_boot_and_process_roots_is_exact() {
        let mut registry = RootRegistry::new();
        assert_eq!(
            registry.register_process(0x2000),
            Err(RootError::MissingBootRoot)
        );
        registry.register_boot(0x1000).unwrap();
        registry.register_boot(0x1000).unwrap();
        registry.register_process(0x2000).unwrap();
        registry.register_process(0x3000).unwrap();
        assert_eq!(
            registry.register_process(0x3000),
            Err(RootError::DuplicateRoot)
        );
        assert_eq!(
            registry.snapshot(Some(10)).unwrap().roots(),
            &[0x1000, 0x2000, 0x3000]
        );
        registry.unregister_process(0x2000).unwrap();
        let snapshot = registry.snapshot(Some(11)).unwrap();
        assert_eq!(snapshot.boot_root(), 0x1000);
        assert_eq!(snapshot.roots(), &[0x1000, 0x3000]);
        assert!(!snapshot.physical_coverage_verified());
    }

    #[test]
    fn current_process_root_is_not_misidentified_as_boot_root() {
        let mut registry = RootRegistry::new();
        registry.observe_current(0x1000).unwrap();
        registry.register_process(0x2000).unwrap();
        registry.observe_current(0x2000).unwrap();
        assert_eq!(
            registry.observe_current(0x3000),
            Err(RootError::MissingRoot)
        );
        assert_eq!(registry.snapshot(Some(2)).unwrap().boot_root(), 0x1000);
    }

    #[test]
    fn boot_cannot_be_replaced_or_freed_as_process() {
        let mut registry = RootRegistry::new();
        registry.register_boot(0x1000).unwrap();
        assert_eq!(
            registry.register_boot(0x2000),
            Err(RootError::DifferentBootRoot)
        );
        assert_eq!(
            registry.unregister_process(0x1000),
            Err(RootError::NotProcessRoot)
        );
        assert_eq!(
            registry.register_process(0x1000),
            Err(RootError::DuplicateRoot)
        );
        assert_eq!(
            registry.unregister_process(0x2000),
            Err(RootError::MissingRoot)
        );
    }

    #[test]
    fn all_invalid_addresses_are_rejected() {
        let mut registry = RootRegistry::new();
        for address in [0, 1, 0x1001, u64::MAX] {
            assert_eq!(
                registry.register_boot(address),
                Err(RootError::InvalidAddress)
            );
        }
        registry.register_boot(0x1000).unwrap();
        assert_eq!(
            registry.register_process(0x2001),
            Err(RootError::InvalidAddress)
        );
    }

    #[test]
    fn overflow_blocks_new_roots_without_eviction() {
        let mut registry = RootRegistry::new();
        registry.register_boot(0x1000).unwrap();
        for index in 1..MAX_ROOTS {
            registry
                .register_process(((index + 1) as u64) * 4096)
                .unwrap();
        }
        assert_eq!(
            registry.register_process(((MAX_ROOTS + 1) as u64) * 4096),
            Err(RootError::Capacity)
        );
        assert_eq!(registry.snapshot(Some(3)).unwrap().roots().len(), MAX_ROOTS);
    }
}
