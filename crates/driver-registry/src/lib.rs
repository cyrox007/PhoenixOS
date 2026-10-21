#![no_std]

use phoenix_pcie::PcieFunction;

pub const DRIVER_ABI_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DriverVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl DriverVersion {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcieMatchRule {
    pub vendor_id: Option<u16>,
    pub device_id: Option<u16>,
    pub class_code: Option<u8>,
    pub subclass: Option<u8>,
    pub programming_interface: Option<u8>,
}

impl PcieMatchRule {
    pub const fn any() -> Self {
        Self {
            vendor_id: None,
            device_id: None,
            class_code: None,
            subclass: None,
            programming_interface: None,
        }
    }

    pub const fn matches(self, function: PcieFunction) -> bool {
        option_u16_matches(self.vendor_id, function.vendor_id)
            && option_u16_matches(self.device_id, function.device_id)
            && option_u8_matches(self.class_code, function.class_code)
            && option_u8_matches(self.subclass, function.subclass)
            && option_u8_matches(self.programming_interface, function.programming_interface)
    }

    const fn specificity(self) -> u8 {
        option_score(self.vendor_id.is_some())
            + option_score(self.device_id.is_some())
            + option_score(self.class_code.is_some())
            + option_score(self.subclass.is_some())
            + option_score(self.programming_interface.is_some())
    }
}

const fn option_u16_matches(expected: Option<u16>, actual: u16) -> bool {
    match expected {
        Some(value) => value == actual,
        None => true,
    }
}

const fn option_u8_matches(expected: Option<u8>, actual: u8) -> bool {
    match expected {
        Some(value) => value == actual,
        None => true,
    }
}

const fn option_score(value: bool) -> u8 {
    if value { 1 } else { 0 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverDescriptor {
    pub name: &'static str,
    pub abi_version: u32,
    pub version: DriverVersion,
    pub priority: u16,
    pub pcie_match: PcieMatchRule,
}

impl DriverDescriptor {
    pub const fn new(
        name: &'static str,
        version: DriverVersion,
        priority: u16,
        pcie_match: PcieMatchRule,
    ) -> Self {
        Self {
            name,
            abi_version: DRIVER_ABI_VERSION,
            version,
            priority,
            pcie_match,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverStartError {
    UnsupportedDevice,
    ResourceUnavailable,
    InitializationFailed,
}

pub type DriverStart = fn(PcieFunction) -> Result<(), DriverStartError>;

#[derive(Clone, Copy)]
pub struct RegisteredDriver {
    pub descriptor: DriverDescriptor,
    pub start: DriverStart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverLaunch {
    pub name: &'static str,
    pub version: DriverVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverRegistryError {
    CapacityExceeded,
    EmptyName,
    UnsupportedAbi(u32),
    DuplicateDriver,
    NoMatchingDriver,
    StartFailed(DriverStartError),
}

pub struct DriverRegistry<const CAPACITY: usize> {
    drivers: [Option<RegisteredDriver>; CAPACITY],
    count: usize,
}

impl<const CAPACITY: usize> DriverRegistry<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            drivers: [None; CAPACITY],
            count: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn register(&mut self, driver: RegisteredDriver) -> Result<(), DriverRegistryError> {
        validate_descriptor(driver.descriptor)?;

        if self.contains(driver.descriptor) {
            return Err(DriverRegistryError::DuplicateDriver);
        }
        if self.count == CAPACITY {
            return Err(DriverRegistryError::CapacityExceeded);
        }

        self.drivers[self.count] = Some(driver);
        self.count += 1;
        Ok(())
    }

    pub fn select(&self, function: PcieFunction) -> Option<RegisteredDriver> {
        let mut selected: Option<RegisteredDriver> = None;

        for driver in self.drivers[..self.count].iter().flatten().copied() {
            if !driver.descriptor.pcie_match.matches(function) {
                continue;
            }

            if is_better_match(driver, selected) {
                selected = Some(driver);
            }
        }

        selected
    }

    pub fn start_best(
        &self,
        function: PcieFunction,
    ) -> Result<DriverLaunch, DriverRegistryError> {
        let driver = self
            .select(function)
            .ok_or(DriverRegistryError::NoMatchingDriver)?;

        (driver.start)(function).map_err(DriverRegistryError::StartFailed)?;

        Ok(DriverLaunch {
            name: driver.descriptor.name,
            version: driver.descriptor.version,
        })
    }

    fn contains(&self, descriptor: DriverDescriptor) -> bool {
        self.drivers[..self.count].iter().flatten().any(|registered| {
            registered.descriptor.name == descriptor.name
                && registered.descriptor.version == descriptor.version
        })
    }
}

impl<const CAPACITY: usize> Default for DriverRegistry<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_descriptor(descriptor: DriverDescriptor) -> Result<(), DriverRegistryError> {
    if descriptor.name.is_empty() {
        return Err(DriverRegistryError::EmptyName);
    }
    if descriptor.abi_version != DRIVER_ABI_VERSION {
        return Err(DriverRegistryError::UnsupportedAbi(
            descriptor.abi_version,
        ));
    }
    Ok(())
}

fn is_better_match(
    candidate: RegisteredDriver,
    selected: Option<RegisteredDriver>,
) -> bool {
    let Some(selected) = selected else {
        return true;
    };

    match_rank(candidate.descriptor) > match_rank(selected.descriptor)
}

fn match_rank(descriptor: DriverDescriptor) -> (u8, u16, DriverVersion) {
    (
        descriptor.pcie_match.specificity(),
        descriptor.priority,
        descriptor.version,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};

    static START_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn started(_: PcieFunction) -> Result<(), DriverStartError> {
        START_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn failed(_: PcieFunction) -> Result<(), DriverStartError> {
        Err(DriverStartError::ResourceUnavailable)
    }

    fn nvme_function() -> PcieFunction {
        PcieFunction {
            segment_group: 0,
            bus: 0,
            device: 1,
            function: 0,
            vendor_id: 0x8086,
            device_id: 0x5845,
            class_code: 0x01,
            subclass: 0x08,
            programming_interface: 0x02,
            revision_id: 1,
            header_type: 0,
            config_address: 0x8000_8000,
        }
    }

    fn registered(
        name: &'static str,
        version: DriverVersion,
        priority: u16,
        rule: PcieMatchRule,
        start: DriverStart,
    ) -> RegisteredDriver {
        RegisteredDriver {
            descriptor: DriverDescriptor::new(name, version, priority, rule),
            start,
        }
    }

    #[test]
    fn rejects_incompatible_abi_before_registration() {
        let mut registry = DriverRegistry::<2>::new();
        let mut descriptor = DriverDescriptor::new(
            "nvme",
            DriverVersion::new(1, 0, 0),
            10,
            PcieMatchRule::any(),
        );
        descriptor.abi_version = DRIVER_ABI_VERSION + 1;

        assert_eq!(
            registry.register(RegisteredDriver {
                descriptor,
                start: started,
            }),
            Err(DriverRegistryError::UnsupportedAbi(
                DRIVER_ABI_VERSION + 1
            ))
        );
        assert!(registry.is_empty());
    }

    #[test]
    fn selects_more_specific_rule_before_generic_driver() {
        let mut registry = DriverRegistry::<4>::new();
        registry
            .register(registered(
                "storage-generic",
                DriverVersion::new(9, 0, 0),
                500,
                PcieMatchRule {
                    class_code: Some(0x01),
                    ..PcieMatchRule::any()
                },
                started,
            ))
            .unwrap();
        registry
            .register(registered(
                "nvme",
                DriverVersion::new(1, 0, 0),
                1,
                PcieMatchRule {
                    class_code: Some(0x01),
                    subclass: Some(0x08),
                    programming_interface: Some(0x02),
                    ..PcieMatchRule::any()
                },
                started,
            ))
            .unwrap();

        let selected = registry.select(nvme_function()).unwrap();
        assert_eq!(selected.descriptor.name, "nvme");
    }

    #[test]
    fn uses_priority_then_version_for_equally_specific_matches() {
        let rule = PcieMatchRule {
            class_code: Some(0x01),
            subclass: Some(0x08),
            ..PcieMatchRule::any()
        };
        let mut registry = DriverRegistry::<4>::new();

        registry
            .register(registered(
                "nvme-stable",
                DriverVersion::new(1, 4, 0),
                20,
                rule,
                started,
            ))
            .unwrap();
        registry
            .register(registered(
                "nvme-preferred",
                DriverVersion::new(1, 0, 0),
                30,
                rule,
                started,
            ))
            .unwrap();

        assert_eq!(
            registry.select(nvme_function()).unwrap().descriptor.name,
            "nvme-preferred"
        );

        let mut equal_priority = DriverRegistry::<4>::new();
        equal_priority
            .register(registered(
                "nvme-old",
                DriverVersion::new(1, 2, 0),
                30,
                rule,
                started,
            ))
            .unwrap();
        equal_priority
            .register(registered(
                "nvme-new",
                DriverVersion::new(1, 3, 0),
                30,
                rule,
                started,
            ))
            .unwrap();

        assert_eq!(
            equal_priority
                .select(nvme_function())
                .unwrap()
                .descriptor
                .name,
            "nvme-new"
        );
    }

    #[test]
    fn starts_selected_driver_and_reports_identity() {
        START_COUNT.store(0, Ordering::SeqCst);
        let mut registry = DriverRegistry::<2>::new();
        registry
            .register(registered(
                "nvme",
                DriverVersion::new(2, 1, 3),
                10,
                PcieMatchRule {
                    class_code: Some(0x01),
                    subclass: Some(0x08),
                    programming_interface: Some(0x02),
                    ..PcieMatchRule::any()
                },
                started,
            ))
            .unwrap();

        let launch = registry.start_best(nvme_function()).unwrap();

        assert_eq!(launch.name, "nvme");
        assert_eq!(launch.version, DriverVersion::new(2, 1, 3));
        assert_eq!(START_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn propagates_start_failure_without_fallback() {
        let mut registry = DriverRegistry::<2>::new();
        registry
            .register(registered(
                "nvme",
                DriverVersion::new(1, 0, 0),
                10,
                PcieMatchRule {
                    class_code: Some(0x01),
                    subclass: Some(0x08),
                    ..PcieMatchRule::any()
                },
                failed,
            ))
            .unwrap();

        assert_eq!(
            registry.start_best(nvme_function()),
            Err(DriverRegistryError::StartFailed(
                DriverStartError::ResourceUnavailable
            ))
        );
    }

    #[test]
    fn rejects_duplicate_version_and_capacity_overflow() {
        let mut registry = DriverRegistry::<1>::new();
        let first = registered(
            "nvme",
            DriverVersion::new(1, 0, 0),
            10,
            PcieMatchRule::any(),
            started,
        );
        registry.register(first).unwrap();

        assert_eq!(
            registry.register(first),
            Err(DriverRegistryError::DuplicateDriver)
        );

        assert_eq!(
            registry.register(registered(
                "xhci",
                DriverVersion::new(1, 0, 0),
                10,
                PcieMatchRule::any(),
                started,
            )),
            Err(DriverRegistryError::CapacityExceeded)
        );
    }
}
