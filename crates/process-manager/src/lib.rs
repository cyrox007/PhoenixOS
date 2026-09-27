#![no_std]

use phoenix_process::{ProcessError, ProcessId, ProcessRecord, ProcessState, ProcessTable};
use phoenix_scheduler::{RoundRobinScheduler, ScheduleDecision, SchedulerError, TaskId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessManagerError {
    Process(ProcessError),
    Scheduler(SchedulerError),
    InconsistentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessScheduleDecision {
    Idle,
    Continue(ProcessId),
    Switch {
        from: Option<ProcessId>,
        to: ProcessId,
    },
}

pub struct ProcessManager<const CAPACITY: usize> {
    processes: ProcessTable<CAPACITY>,
    scheduler: RoundRobinScheduler<CAPACITY>,
}

impl<const CAPACITY: usize> ProcessManager<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            processes: ProcessTable::new(),
            scheduler: RoundRobinScheduler::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.processes.len()
    }

    pub fn scheduled_len(&self) -> usize {
        self.scheduler.len()
    }

    pub fn record(&self, id: ProcessId) -> Option<ProcessRecord> {
        self.processes.record(id)
    }

    pub fn current(&self) -> Option<ProcessId> {
        self.scheduler.current().map(process_id)
    }

    pub fn create_runnable(
        &mut self,
        quantum_ticks: u32,
    ) -> Result<ProcessRecord, ProcessManagerError> {
        let process = self
            .processes
            .create()
            .map_err(ProcessManagerError::Process)?;

        if let Err(error) = self.scheduler.add(task_id(process.id), quantum_ticks) {
            self.processes
                .remove_created(process.id)
                .map_err(|_| ProcessManagerError::InconsistentState)?;
            return Err(ProcessManagerError::Scheduler(error));
        }

        if self
            .processes
            .transition(process.id, ProcessState::Runnable)
            .is_err()
        {
            let _ = self.scheduler.remove(task_id(process.id));
            let _ = self.processes.remove_created(process.id);
            return Err(ProcessManagerError::InconsistentState);
        }

        self.processes
            .record(process.id)
            .ok_or(ProcessManagerError::InconsistentState)
    }

    pub fn set_blocked(&mut self, id: ProcessId, blocked: bool) -> Result<(), ProcessManagerError> {
        let record = self
            .processes
            .record(id)
            .ok_or(ProcessManagerError::Process(ProcessError::ProcessNotFound))?;
        let target = if blocked {
            ProcessState::Blocked
        } else {
            ProcessState::Runnable
        };
        let expected = if blocked {
            ProcessState::Runnable
        } else {
            ProcessState::Blocked
        };

        if record.state != expected {
            return Err(ProcessManagerError::Process(
                ProcessError::InvalidTransition {
                    from: record.state,
                    to: target,
                },
            ));
        }

        if !self.scheduler.contains(task_id(id)) {
            return Err(ProcessManagerError::InconsistentState);
        }

        self.processes
            .transition(id, target)
            .map_err(ProcessManagerError::Process)?;
        if self.scheduler.set_runnable(task_id(id), !blocked).is_err() {
            let _ = self.processes.transition(id, record.state);
            return Err(ProcessManagerError::InconsistentState);
        }

        Ok(())
    }

    pub fn exit(&mut self, id: ProcessId, status: u64) -> Result<(), ProcessManagerError> {
        let record = self
            .processes
            .record(id)
            .ok_or(ProcessManagerError::Process(ProcessError::ProcessNotFound))?;
        if !matches!(record.state, ProcessState::Runnable | ProcessState::Blocked) {
            return Err(ProcessManagerError::Process(
                ProcessError::InvalidTransition {
                    from: record.state,
                    to: ProcessState::Exited,
                },
            ));
        }

        if !self.scheduler.contains(task_id(id)) {
            return Err(ProcessManagerError::InconsistentState);
        }

        self.processes
            .exit(id, status)
            .map_err(ProcessManagerError::Process)?;
        self.scheduler
            .remove(task_id(id))
            .map_err(|_| ProcessManagerError::InconsistentState)
    }

    pub fn reap(&mut self, id: ProcessId) -> Result<ProcessRecord, ProcessManagerError> {
        if self.scheduler.contains(task_id(id)) {
            return Err(ProcessManagerError::InconsistentState);
        }

        self.processes
            .remove_exited(id)
            .map_err(ProcessManagerError::Process)
    }

    pub fn on_tick(&mut self) -> ProcessScheduleDecision {
        match self.scheduler.on_tick() {
            ScheduleDecision::Idle => ProcessScheduleDecision::Idle,
            ScheduleDecision::Continue(id) => ProcessScheduleDecision::Continue(process_id(id)),
            ScheduleDecision::Switch { from, to } => ProcessScheduleDecision::Switch {
                from: from.map(process_id),
                to: process_id(to),
            },
        }
    }
}

impl<const CAPACITY: usize> Default for ProcessManager<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

const fn task_id(id: ProcessId) -> TaskId {
    TaskId(id.0)
}

const fn process_id(id: TaskId) -> ProcessId {
    ProcessId(id.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_runnable_process_and_schedules_it() {
        let mut manager = ProcessManager::<2>::new();
        let process = manager.create_runnable(2).unwrap();

        assert_eq!(process.state, ProcessState::Runnable);
        assert_eq!(process.exit_status, None);
        assert_eq!(manager.len(), 1);
        assert_eq!(manager.scheduled_len(), 1);
        assert_eq!(
            manager.on_tick(),
            ProcessScheduleDecision::Switch {
                from: None,
                to: process.id,
            }
        );
    }

    #[test]
    fn blocking_and_waking_keep_scheduler_in_sync() {
        let mut manager = ProcessManager::<1>::new();
        let process = manager.create_runnable(1).unwrap();

        manager.set_blocked(process.id, true).unwrap();
        assert_eq!(
            manager.record(process.id).unwrap().state,
            ProcessState::Blocked
        );
        assert_eq!(manager.on_tick(), ProcessScheduleDecision::Idle);

        manager.set_blocked(process.id, false).unwrap();
        assert_eq!(
            manager.on_tick(),
            ProcessScheduleDecision::Switch {
                from: None,
                to: process.id,
            }
        );
    }

    #[test]
    fn exiting_process_is_unscheduled_until_reaped() {
        let mut manager = ProcessManager::<2>::new();
        let first = manager.create_runnable(1).unwrap();
        let second = manager.create_runnable(1).unwrap();
        assert!(matches!(
            manager.on_tick(),
            ProcessScheduleDecision::Switch { to, .. } if to == first.id
        ));

        manager.exit(first.id, u64::MAX).unwrap();

        assert_eq!(manager.current(), None);
        assert_eq!(manager.scheduled_len(), 1);
        assert_eq!(
            manager.record(first.id).unwrap().exit_status,
            Some(u64::MAX)
        );
        assert_eq!(
            manager.on_tick(),
            ProcessScheduleDecision::Switch {
                from: None,
                to: second.id,
            }
        );

        let reaped = manager.reap(first.id).unwrap();
        assert_eq!(reaped.exit_status, Some(u64::MAX));
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn failed_creation_rolls_process_record_back() {
        let mut manager = ProcessManager::<1>::new();

        assert_eq!(
            manager.create_runnable(0),
            Err(ProcessManagerError::Scheduler(
                SchedulerError::InvalidQuantum
            ))
        );
        assert_eq!(manager.len(), 0);
        assert_eq!(manager.scheduled_len(), 0);
    }
}
