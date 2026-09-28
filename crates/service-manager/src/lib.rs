#![no_std]

use phoenix_process::ProcessId;
use phoenix_process_manager::{ProcessManager, ProcessManagerError, ProcessScheduleDecision};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceManifest {
    pub id: ServiceId,
    pub image_id: u64,
    pub quantum_ticks: u32,
    pub restart_policy: RestartPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Registered,
    Running,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceRecord {
    pub manifest: ServiceManifest,
    pub state: ServiceState,
    pub process: Option<ProcessId>,
    pub last_exit_status: Option<u64>,
    pub restart_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceCompletion {
    pub exited_process: ProcessId,
    pub restarted_as: Option<ProcessId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManagerError {
    DuplicateService,
    RegistryFull,
    ServiceNotFound,
    ServiceAlreadyRunning,
    ServiceNotRunning,
    InvalidManifest,
    Process(ProcessManagerError),
}

pub struct ServiceManager<const SERVICES: usize, const PROCESSES: usize> {
    services: [Option<ServiceRecord>; SERVICES],
    service_count: usize,
    processes: ProcessManager<PROCESSES>,
}

impl<const SERVICES: usize, const PROCESSES: usize> ServiceManager<SERVICES, PROCESSES> {
    pub const fn new() -> Self {
        Self {
            services: [None; SERVICES],
            service_count: 0,
            processes: ProcessManager::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.service_count
    }

    pub fn record(&self, id: ServiceId) -> Option<ServiceRecord> {
        self.find_index(id).and_then(|index| self.services[index])
    }

    pub fn register(&mut self, manifest: ServiceManifest) -> Result<(), ServiceManagerError> {
        if manifest.id.0 == 0 || manifest.image_id == 0 || manifest.quantum_ticks == 0 {
            return Err(ServiceManagerError::InvalidManifest);
        }
        if self.find_index(manifest.id).is_some() {
            return Err(ServiceManagerError::DuplicateService);
        }

        let Some(index) = self.services.iter().position(Option::is_none) else {
            return Err(ServiceManagerError::RegistryFull);
        };
        self.services[index] = Some(ServiceRecord {
            manifest,
            state: ServiceState::Registered,
            process: None,
            last_exit_status: None,
            restart_count: 0,
        });
        self.service_count += 1;
        Ok(())
    }

    pub fn start(&mut self, id: ServiceId) -> Result<ProcessId, ServiceManagerError> {
        let index = self
            .find_index(id)
            .ok_or(ServiceManagerError::ServiceNotFound)?;
        let record = self.services[index].ok_or(ServiceManagerError::ServiceNotFound)?;
        if record.state == ServiceState::Running {
            return Err(ServiceManagerError::ServiceAlreadyRunning);
        }

        let process = self
            .processes
            .create_runnable(record.manifest.quantum_ticks)
            .map_err(ServiceManagerError::Process)?;
        self.services[index] = Some(ServiceRecord {
            state: ServiceState::Running,
            process: Some(process.id),
            ..record
        });
        Ok(process.id)
    }

    pub fn complete(
        &mut self,
        id: ServiceId,
        status: u64,
    ) -> Result<ServiceCompletion, ServiceManagerError> {
        let index = self
            .find_index(id)
            .ok_or(ServiceManagerError::ServiceNotFound)?;
        let record = self.services[index].ok_or(ServiceManagerError::ServiceNotFound)?;
        if record.state != ServiceState::Running {
            return Err(ServiceManagerError::ServiceNotRunning);
        }
        let process = record
            .process
            .ok_or(ServiceManagerError::ServiceNotRunning)?;

        self.processes
            .exit(process, status)
            .map_err(ServiceManagerError::Process)?;
        self.processes
            .reap(process)
            .map_err(ServiceManagerError::Process)?;

        let restart = match record.manifest.restart_policy {
            RestartPolicy::Never => false,
            RestartPolicy::OnFailure => status != 0,
            RestartPolicy::Always => true,
        };
        self.services[index] = Some(ServiceRecord {
            state: ServiceState::Exited,
            process: None,
            last_exit_status: Some(status),
            ..record
        });

        let restarted_as = if restart {
            let next = self.start(id)?;
            let running = self.services[index].ok_or(ServiceManagerError::ServiceNotFound)?;
            self.services[index] = Some(ServiceRecord {
                restart_count: record.restart_count.saturating_add(1),
                ..running
            });
            Some(next)
        } else {
            None
        };

        Ok(ServiceCompletion {
            exited_process: process,
            restarted_as,
        })
    }

    pub fn on_tick(&mut self) -> ProcessScheduleDecision {
        self.processes.on_tick()
    }

    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    fn find_index(&self, id: ServiceId) -> Option<usize> {
        self.services
            .iter()
            .position(|entry| entry.is_some_and(|record| record.manifest.id == id))
    }
}

impl<const SERVICES: usize, const PROCESSES: usize> Default
    for ServiceManager<SERVICES, PROCESSES>
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVICE: ServiceManifest = ServiceManifest {
        id: ServiceId(1),
        image_id: 0x494e_4954,
        quantum_ticks: 2,
        restart_policy: RestartPolicy::OnFailure,
    };

    #[test]
    fn registers_and_starts_service_as_runnable_process() {
        let mut manager = ServiceManager::<2, 2>::new();
        manager.register(SERVICE).unwrap();
        let process = manager.start(SERVICE.id).unwrap();

        assert_eq!(manager.len(), 1);
        assert_eq!(manager.process_count(), 1);
        assert_eq!(manager.record(SERVICE.id).unwrap().process, Some(process));
        assert!(matches!(
            manager.on_tick(),
            ProcessScheduleDecision::Switch { to, .. } if to == process
        ));
    }

    #[test]
    fn restarts_failed_service_and_keeps_last_status() {
        let mut manager = ServiceManager::<1, 1>::new();
        manager.register(SERVICE).unwrap();
        let first = manager.start(SERVICE.id).unwrap();
        let completion = manager.complete(SERVICE.id, 7).unwrap();
        let second = completion.restarted_as.unwrap();

        assert_eq!(completion.exited_process, first);
        assert_ne!(second, first);
        let record = manager.record(SERVICE.id).unwrap();
        assert_eq!(record.state, ServiceState::Running);
        assert_eq!(record.process, Some(second));
        assert_eq!(record.last_exit_status, Some(7));
        assert_eq!(record.restart_count, 1);
    }

    #[test]
    fn successful_on_failure_service_stays_exited() {
        let mut manager = ServiceManager::<1, 1>::new();
        manager.register(SERVICE).unwrap();
        manager.start(SERVICE.id).unwrap();
        let completion = manager.complete(SERVICE.id, 0).unwrap();

        assert_eq!(completion.restarted_as, None);
        assert_eq!(manager.process_count(), 0);
        assert_eq!(
            manager.record(SERVICE.id).unwrap().state,
            ServiceState::Exited
        );
    }

    #[test]
    fn rejects_duplicate_and_invalid_manifests() {
        let mut manager = ServiceManager::<1, 1>::new();
        manager.register(SERVICE).unwrap();
        assert_eq!(
            manager.register(SERVICE),
            Err(ServiceManagerError::DuplicateService)
        );
        assert_eq!(
            ServiceManager::<1, 1>::new().register(ServiceManifest {
                quantum_ticks: 0,
                ..SERVICE
            }),
            Err(ServiceManagerError::InvalidManifest)
        );
    }
}
