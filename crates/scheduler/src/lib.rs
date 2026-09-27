#![no_std]

/// Идентификатор планируемой задачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(pub u64);

/// Ошибки изменения состава планировщика.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerError {
    CapacityExceeded,
    DuplicateTask,
    TaskNotFound,
    InvalidQuantum,
}

/// Решение, принятое на очередном системном тике.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleDecision {
    Idle,
    Continue(TaskId),
    Switch {
        from: Option<TaskId>,
        to: TaskId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskSlot {
    id: TaskId,
    quantum_ticks: u32,
    runnable: bool,
}

/// Планировщик кругового обслуживания с фиксированной вместимостью.
///
/// Структура не выполняет переключение контекста сама. Она только принимает
/// решение, какую задачу следует продолжить или выбрать на очередном тике.
pub struct RoundRobinScheduler<const CAPACITY: usize> {
    slots: [Option<TaskSlot>; CAPACITY],
    current: Option<usize>,
    remaining_ticks: u32,
    len: usize,
}

impl<const CAPACITY: usize> RoundRobinScheduler<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            slots: [None; CAPACITY],
            current: None,
            remaining_ticks: 0,
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn current(&self) -> Option<TaskId> {
        let index = self.current?;
        self.slots[index].map(|slot| slot.id)
    }

    pub fn add(&mut self, id: TaskId, quantum_ticks: u32) -> Result<(), SchedulerError> {
        if quantum_ticks == 0 {
            return Err(SchedulerError::InvalidQuantum);
        }

        if self.find_index(id).is_some() {
            return Err(SchedulerError::DuplicateTask);
        }

        let Some(index) = self.first_free_index() else {
            return Err(SchedulerError::CapacityExceeded);
        };

        self.slots[index] = Some(TaskSlot {
            id,
            quantum_ticks,
            runnable: true,
        });
        self.len += 1;

        Ok(())
    }

    pub fn remove(&mut self, id: TaskId) -> Result<(), SchedulerError> {
        let Some(index) = self.find_index(id) else {
            return Err(SchedulerError::TaskNotFound);
        };

        self.slots[index] = None;
        self.len -= 1;

        if self.current == Some(index) {
            self.current = None;
            self.remaining_ticks = 0;
        }

        Ok(())
    }

    pub fn set_runnable(&mut self, id: TaskId, runnable: bool) -> Result<(), SchedulerError> {
        let Some(index) = self.find_index(id) else {
            return Err(SchedulerError::TaskNotFound);
        };

        let Some(mut slot) = self.slots[index] else {
            return Err(SchedulerError::TaskNotFound);
        };

        slot.runnable = runnable;
        self.slots[index] = Some(slot);

        if !runnable && self.current == Some(index) {
            self.remaining_ticks = 0;
        }

        Ok(())
    }

    pub fn on_tick(&mut self) -> ScheduleDecision {
        let Some(current_index) = self.current else {
            return self.select_from(None);
        };

        let Some(current_slot) = self.slots[current_index] else {
            self.current = None;
            self.remaining_ticks = 0;
            return self.select_from(None);
        };

        if !current_slot.runnable {
            return self.select_from(Some(current_index));
        }

        if self.remaining_ticks > 1 {
            self.remaining_ticks -= 1;
            return ScheduleDecision::Continue(current_slot.id);
        }

        self.select_from(Some(current_index))
    }

    fn select_from(&mut self, previous: Option<usize>) -> ScheduleDecision {
        let next = self.find_next_runnable(previous);
        let Some(next_index) = next else {
            self.current = None;
            self.remaining_ticks = 0;
            return ScheduleDecision::Idle;
        };

        let next_slot = self.slots[next_index].expect("индекс выбран только для занятого слота");
        let previous_id = previous.and_then(|index| self.slots[index].map(|slot| slot.id));

        self.current = Some(next_index);
        self.remaining_ticks = next_slot.quantum_ticks;

        if previous == Some(next_index) {
            return ScheduleDecision::Continue(next_slot.id);
        }

        ScheduleDecision::Switch {
            from: previous_id,
            to: next_slot.id,
        }
    }

    fn find_next_runnable(&self, after: Option<usize>) -> Option<usize> {
        if CAPACITY == 0 {
            return None;
        }

        let start = after.map_or(0, |index| (index + 1) % CAPACITY);

        for offset in 0..CAPACITY {
            let index = (start + offset) % CAPACITY;
            let Some(slot) = self.slots[index] else {
                continue;
            };

            if slot.runnable {
                return Some(index);
            }
        }

        None
    }

    fn find_index(&self, id: TaskId) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.is_some_and(|entry| entry.id == id))
    }

    fn first_free_index(&self) -> Option<usize> {
        self.slots.iter().position(Option::is_none)
    }
}

impl<const CAPACITY: usize> Default for RoundRobinScheduler<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{RoundRobinScheduler, ScheduleDecision, SchedulerError, TaskId};

    #[test]
    fn switches_tasks_after_quantum() {
        let mut scheduler = RoundRobinScheduler::<4>::new();
        scheduler.add(TaskId(1), 2).unwrap();
        scheduler.add(TaskId(2), 2).unwrap();

        assert_eq!(
            scheduler.on_tick(),
            ScheduleDecision::Switch {
                from: None,
                to: TaskId(1),
            }
        );
        assert_eq!(scheduler.on_tick(), ScheduleDecision::Continue(TaskId(1)));
        assert_eq!(
            scheduler.on_tick(),
            ScheduleDecision::Switch {
                from: Some(TaskId(1)),
                to: TaskId(2),
            }
        );
        assert_eq!(scheduler.on_tick(), ScheduleDecision::Continue(TaskId(2)));
    }

    #[test]
    fn skips_blocked_tasks() {
        let mut scheduler = RoundRobinScheduler::<4>::new();
        scheduler.add(TaskId(1), 1).unwrap();
        scheduler.add(TaskId(2), 1).unwrap();
        scheduler.add(TaskId(3), 1).unwrap();
        scheduler.set_runnable(TaskId(2), false).unwrap();

        assert_eq!(
            scheduler.on_tick(),
            ScheduleDecision::Switch {
                from: None,
                to: TaskId(1),
            }
        );
        assert_eq!(
            scheduler.on_tick(),
            ScheduleDecision::Switch {
                from: Some(TaskId(1)),
                to: TaskId(3),
            }
        );
    }

    #[test]
    fn removing_current_task_selects_another_task() {
        let mut scheduler = RoundRobinScheduler::<2>::new();
        scheduler.add(TaskId(10), 1).unwrap();
        scheduler.add(TaskId(20), 1).unwrap();

        scheduler.on_tick();
        scheduler.remove(TaskId(10)).unwrap();

        assert_eq!(
            scheduler.on_tick(),
            ScheduleDecision::Switch {
                from: None,
                to: TaskId(20),
            }
        );
    }

    #[test]
    fn reports_capacity_and_input_errors() {
        let mut scheduler = RoundRobinScheduler::<1>::new();

        assert_eq!(
            scheduler.add(TaskId(1), 0),
            Err(SchedulerError::InvalidQuantum)
        );

        scheduler.add(TaskId(1), 1).unwrap();

        assert_eq!(
            scheduler.add(TaskId(1), 1),
            Err(SchedulerError::DuplicateTask)
        );
        assert_eq!(
            scheduler.add(TaskId(2), 1),
            Err(SchedulerError::CapacityExceeded)
        );
        assert_eq!(
            scheduler.remove(TaskId(9)),
            Err(SchedulerError::TaskNotFound)
        );
    }

    #[test]
    fn idles_when_all_tasks_are_blocked() {
        let mut scheduler = RoundRobinScheduler::<2>::new();
        scheduler.add(TaskId(1), 1).unwrap();
        scheduler.set_runnable(TaskId(1), false).unwrap();

        assert_eq!(scheduler.on_tick(), ScheduleDecision::Idle);
        assert_eq!(scheduler.current(), None);
    }

    #[test]
    fn resumes_task_after_wakeup() {
        let mut scheduler = RoundRobinScheduler::<2>::new();
        scheduler.add(TaskId(1), 1).unwrap();
        scheduler.set_runnable(TaskId(1), false).unwrap();

        assert_eq!(scheduler.on_tick(), ScheduleDecision::Idle);

        scheduler.set_runnable(TaskId(1), true).unwrap();

        assert_eq!(
            scheduler.on_tick(),
            ScheduleDecision::Switch {
                from: None,
                to: TaskId(1),
            }
        );
    }

    #[test]
    fn renews_quantum_for_single_runnable_task() {
        let mut scheduler = RoundRobinScheduler::<1>::new();
        scheduler.add(TaskId(7), 2).unwrap();

        assert_eq!(
            scheduler.on_tick(),
            ScheduleDecision::Switch {
                from: None,
                to: TaskId(7),
            }
        );
        assert_eq!(scheduler.on_tick(), ScheduleDecision::Continue(TaskId(7)));
        assert_eq!(scheduler.on_tick(), ScheduleDecision::Continue(TaskId(7)));
    }
}
