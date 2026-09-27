use alloc::boxed::Box;
use alloc::vec::Vec;

use phoenix_scheduler::{RoundRobinScheduler, ScheduleDecision, SchedulerError, TaskId};

use crate::arch::x86_64::context::{self, Context, ReturningThreadEntry};
use crate::thread::{KernelThread, ThreadError, ThreadId, ThreadState};

const DEFAULT_STACK_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub enum ThreadManagerError {
    Scheduler(SchedulerError),
    Thread(ThreadError),
    MissingThread,
    InvalidState,
    IdExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchResult {
    Idle,
    Completed(ThreadId),
}

struct ExitState {
    thread: *mut KernelThread,
    dispatcher_context: *mut Context,
}

struct ThreadSlot {
    thread: Box<KernelThread>,
    _exit_state: Box<ExitState>,
}

pub struct ThreadManager<const CAPACITY: usize> {
    scheduler: RoundRobinScheduler<CAPACITY>,
    dispatcher_context: Box<Context>,
    threads: Vec<ThreadSlot>,
    next_id: u64,
}

impl<const CAPACITY: usize> ThreadManager<CAPACITY> {
    pub fn new() -> Self {
        Self {
            scheduler: RoundRobinScheduler::new(),
            dispatcher_context: Box::new(Context::empty()),
            threads: Vec::with_capacity(CAPACITY),
            next_id: 1,
        }
    }

    pub fn live_count(&self) -> usize {
        self.threads.len()
    }

    pub fn state(&self, id: ThreadId) -> Option<ThreadState> {
        self.find_thread(id)
            .map(|index| self.threads[index].thread.state())
    }

    pub fn spawn(
        &mut self,
        entry: ReturningThreadEntry,
        argument: usize,
        quantum_ticks: u32,
    ) -> Result<ThreadId, ThreadManagerError> {
        self.spawn_with_stack(entry, argument, quantum_ticks, DEFAULT_STACK_SIZE)
    }

    pub fn spawn_with_stack(
        &mut self,
        entry: ReturningThreadEntry,
        argument: usize,
        quantum_ticks: u32,
        stack_size: usize,
    ) -> Result<ThreadId, ThreadManagerError> {
        let id = ThreadId(self.next_id);
        let task_id = TaskId(id.0);
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ThreadManagerError::IdExhausted)?;

        self.scheduler
            .add(task_id, quantum_ticks)
            .map_err(ThreadManagerError::Scheduler)?;

        let result = self.build_slot(id, entry, argument, stack_size);
        let slot = match result {
            Ok(slot) => slot,
            Err(error) => {
                let _ = self.scheduler.remove(task_id);
                return Err(error);
            }
        };

        self.threads.push(slot);
        self.next_id = next_id;

        Ok(id)
    }

    pub fn set_blocked(&mut self, id: ThreadId, blocked: bool) -> Result<(), ThreadManagerError> {
        let index = self
            .find_thread(id)
            .ok_or(ThreadManagerError::MissingThread)?;
        let state = self.threads[index].thread.state();

        if blocked {
            return self.block_thread(index, id, state);
        }

        self.wake_thread(index, id, state)
    }

    pub fn dispatch_tick(&mut self) -> Result<DispatchResult, ThreadManagerError> {
        let decision = self.scheduler.on_tick();
        let task_id = match decision {
            ScheduleDecision::Idle => return Ok(DispatchResult::Idle),
            ScheduleDecision::Continue(id) => id,
            ScheduleDecision::Switch { to, .. } => to,
        };

        let id = ThreadId(task_id.0);
        let index = self
            .find_thread(id)
            .ok_or(ThreadManagerError::MissingThread)?;

        self.threads[index]
            .thread
            .start()
            .map_err(ThreadManagerError::Thread)?;

        let dispatcher_context: *mut Context = &mut *self.dispatcher_context;
        let thread_context: *const Context = self.threads[index].thread.context();

        unsafe {
            context::switch(&mut *dispatcher_context, &*thread_context);
        }

        if self.threads[index].thread.state() != ThreadState::Finished {
            return Err(ThreadManagerError::InvalidState);
        }

        self.scheduler
            .remove(task_id)
            .map_err(ThreadManagerError::Scheduler)?;
        self.threads.swap_remove(index);

        Ok(DispatchResult::Completed(id))
    }

    fn build_slot(
        &mut self,
        id: ThreadId,
        entry: ReturningThreadEntry,
        argument: usize,
        stack_size: usize,
    ) -> Result<ThreadSlot, ThreadManagerError> {
        let mut exit_state = Box::new(ExitState {
            thread: core::ptr::null_mut(),
            dispatcher_context: &mut *self.dispatcher_context,
        });

        let exit_argument = &mut *exit_state as *mut ExitState as usize;
        let mut thread = Box::new(
            KernelThread::new_returning(
                id,
                stack_size,
                entry,
                argument,
                thread_exit,
                exit_argument,
            )
            .map_err(ThreadManagerError::Thread)?,
        );

        exit_state.thread = &mut *thread;

        Ok(ThreadSlot {
            thread,
            _exit_state: exit_state,
        })
    }

    fn block_thread(
        &mut self,
        index: usize,
        id: ThreadId,
        state: ThreadState,
    ) -> Result<(), ThreadManagerError> {
        if !matches!(state, ThreadState::Ready | ThreadState::Running) {
            return Err(ThreadManagerError::InvalidState);
        }

        self.scheduler
            .set_runnable(TaskId(id.0), false)
            .map_err(ThreadManagerError::Scheduler)?;

        self.threads[index]
            .thread
            .block()
            .map_err(ThreadManagerError::Thread)
    }

    fn wake_thread(
        &mut self,
        index: usize,
        id: ThreadId,
        state: ThreadState,
    ) -> Result<(), ThreadManagerError> {
        if state != ThreadState::Blocked {
            return Err(ThreadManagerError::InvalidState);
        }

        self.threads[index]
            .thread
            .wake()
            .map_err(ThreadManagerError::Thread)?;

        let result = self.scheduler.set_runnable(TaskId(id.0), true);
        if result.is_err() {
            let _ = self.threads[index].thread.block();
        }

        result.map_err(ThreadManagerError::Scheduler)
    }

    fn find_thread(&self, id: ThreadId) -> Option<usize> {
        self.threads.iter().position(|slot| slot.thread.id() == id)
    }
}

impl<const CAPACITY: usize> Default for ThreadManager<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

extern "C" fn thread_exit(argument: usize) -> ! {
    let state = unsafe { &mut *(argument as *mut ExitState) };
    let thread = unsafe { &mut *state.thread };

    thread
        .finish()
        .expect("поток завершился в недопустимом состоянии");

    unsafe {
        context::switch(thread.context_mut(), &*state.dispatcher_context);
    }

    loop {
        core::hint::spin_loop();
    }
}
