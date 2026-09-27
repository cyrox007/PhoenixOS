use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use phoenix_scheduler::{RoundRobinScheduler, ScheduleDecision, SchedulerError, TaskId};

use crate::arch::x86_64::context::{self, Context, ReturningThreadEntry};
use crate::thread::{KernelThread, ThreadError, ThreadId, ThreadState};

const DEFAULT_STACK_SIZE: usize = 64 * 1024;

static LAST_TIMER_TICK: AtomicU64 = AtomicU64::new(0);
static LAST_TIMER_RIP: AtomicU64 = AtomicU64::new(0);
static LAST_TIMER_RSP: AtomicU64 = AtomicU64::new(0);
static LAST_TIMER_RFLAGS: AtomicU64 = AtomicU64::new(0);
static LAST_TIMER_REGISTER_MARKER: AtomicU64 = AtomicU64::new(0);
static LAST_TIMER_STACK_FRAME: AtomicU64 = AtomicU64::new(0);
static PREEMPTION_TEST_TARGET_FRAME: AtomicU64 = AtomicU64::new(0);
static PREEMPTION_TEST_ORIGINAL_FRAME: AtomicU64 = AtomicU64::new(0);
static PREEMPTION_TEST_PHASE: AtomicU64 = AtomicU64::new(0);
static LAST_TIMER_REGISTERS: [AtomicU64; 15] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

pub fn timer_tick_hook(
    tick: u64,
    context: crate::arch::x86_64::apic::TimerInterruptContext,
) -> Option<u64> {
    LAST_TIMER_RIP.store(context.instruction_pointer, Ordering::Relaxed);
    LAST_TIMER_RSP.store(context.stack_pointer, Ordering::Relaxed);
    LAST_TIMER_RFLAGS.store(context.cpu_flags, Ordering::Relaxed);
    LAST_TIMER_REGISTER_MARKER.store(context.register_capture_marker, Ordering::Relaxed);
    LAST_TIMER_STACK_FRAME.store(context.stack_frame_address, Ordering::Relaxed);
    store_timer_registers(context.general_registers);
    LAST_TIMER_TICK.store(tick, Ordering::Release);

    let target = PREEMPTION_TEST_TARGET_FRAME.load(Ordering::Acquire);
    if target != 0 {
        match PREEMPTION_TEST_PHASE.load(Ordering::Acquire) {
            0 => {
                PREEMPTION_TEST_ORIGINAL_FRAME
                    .store(context.stack_frame_address, Ordering::Release);
                PREEMPTION_TEST_PHASE.store(1, Ordering::Release);
                return Some(target);
            }
            1 => {
                let original = PREEMPTION_TEST_ORIGINAL_FRAME.load(Ordering::Acquire);
                PREEMPTION_TEST_PHASE.store(2, Ordering::Release);
                return Some(original);
            }
            _ => {}
        }
    }

    None
}

pub fn arm_preemption_frame_test(target_frame: u64) {
    PREEMPTION_TEST_ORIGINAL_FRAME.store(0, Ordering::Relaxed);
    PREEMPTION_TEST_PHASE.store(0, Ordering::Relaxed);
    PREEMPTION_TEST_TARGET_FRAME.store(target_frame, Ordering::Release);
}

pub fn preemption_frame_test_completed() -> bool {
    PREEMPTION_TEST_PHASE.load(Ordering::Acquire) == 2
}

pub fn preemption_frame_test_phase() -> u64 {
    PREEMPTION_TEST_PHASE.load(Ordering::Acquire)
}

pub fn disarm_preemption_frame_test() {
    PREEMPTION_TEST_TARGET_FRAME.store(0, Ordering::Release);
}

pub fn last_timer_tick() -> u64 {
    LAST_TIMER_TICK.load(Ordering::Acquire)
}

pub fn last_timer_context() -> Option<crate::arch::x86_64::apic::TimerInterruptContext> {
    if LAST_TIMER_TICK.load(Ordering::Acquire) == 0 {
        return None;
    }

    Some(crate::arch::x86_64::apic::TimerInterruptContext {
        instruction_pointer: LAST_TIMER_RIP.load(Ordering::Relaxed),
        stack_pointer: LAST_TIMER_RSP.load(Ordering::Relaxed),
        cpu_flags: LAST_TIMER_RFLAGS.load(Ordering::Relaxed),
        general_registers: load_timer_registers(),
        register_capture_marker: LAST_TIMER_REGISTER_MARKER.load(Ordering::Relaxed),
        stack_frame_address: LAST_TIMER_STACK_FRAME.load(Ordering::Relaxed),
    })
}

fn store_timer_registers(registers: crate::arch::x86_64::apic::TimerGeneralRegisters) {
    LAST_TIMER_REGISTERS[0].store(registers.rax, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[1].store(registers.rbx, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[2].store(registers.rcx, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[3].store(registers.rdx, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[4].store(registers.rsi, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[5].store(registers.rdi, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[6].store(registers.rbp, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[7].store(registers.r8, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[8].store(registers.r9, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[9].store(registers.r10, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[10].store(registers.r11, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[11].store(registers.r12, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[12].store(registers.r13, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[13].store(registers.r14, Ordering::Relaxed);
    LAST_TIMER_REGISTERS[14].store(registers.r15, Ordering::Relaxed);
}

fn load_timer_registers() -> crate::arch::x86_64::apic::TimerGeneralRegisters {
    crate::arch::x86_64::apic::TimerGeneralRegisters {
        rax: LAST_TIMER_REGISTERS[0].load(Ordering::Relaxed),
        rbx: LAST_TIMER_REGISTERS[1].load(Ordering::Relaxed),
        rcx: LAST_TIMER_REGISTERS[2].load(Ordering::Relaxed),
        rdx: LAST_TIMER_REGISTERS[3].load(Ordering::Relaxed),
        rsi: LAST_TIMER_REGISTERS[4].load(Ordering::Relaxed),
        rdi: LAST_TIMER_REGISTERS[5].load(Ordering::Relaxed),
        rbp: LAST_TIMER_REGISTERS[6].load(Ordering::Relaxed),
        r8: LAST_TIMER_REGISTERS[7].load(Ordering::Relaxed),
        r9: LAST_TIMER_REGISTERS[8].load(Ordering::Relaxed),
        r10: LAST_TIMER_REGISTERS[9].load(Ordering::Relaxed),
        r11: LAST_TIMER_REGISTERS[10].load(Ordering::Relaxed),
        r12: LAST_TIMER_REGISTERS[11].load(Ordering::Relaxed),
        r13: LAST_TIMER_REGISTERS[12].load(Ordering::Relaxed),
        r14: LAST_TIMER_REGISTERS[13].load(Ordering::Relaxed),
        r15: LAST_TIMER_REGISTERS[14].load(Ordering::Relaxed),
    }
}

#[derive(Debug)]
pub enum ThreadManagerError {
    Scheduler(SchedulerError),
    Thread(ThreadError),
    MissingThread,
    MissingInterruptFrame,
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

    pub fn interrupt_frame(&self, id: ThreadId) -> Option<u64> {
        let index = self.find_thread(id)?;
        self.threads[index].thread.interrupt_frame()
    }

    pub fn save_interrupt_frame(
        &mut self,
        id: ThreadId,
        frame: u64,
    ) -> Result<(), ThreadManagerError> {
        let index = self
            .find_thread(id)
            .ok_or(ThreadManagerError::MissingThread)?;
        self.threads[index]
            .thread
            .save_interrupt_frame(frame)
            .map_err(ThreadManagerError::Thread)
    }

    pub fn schedule_interrupt_frame(
        &mut self,
        current_frame: u64,
    ) -> Result<Option<u64>, ThreadManagerError> {
        if let Some(current) = self.scheduler.current() {
            let id = ThreadId(current.0);
            self.save_interrupt_frame(id, current_frame)?;
        }

        match self.scheduler.on_tick() {
            ScheduleDecision::Idle | ScheduleDecision::Continue(_) => Ok(None),
            ScheduleDecision::Switch { to, .. } => {
                let id = ThreadId(to.0);
                let index = self
                    .find_thread(id)
                    .ok_or(ThreadManagerError::MissingThread)?;
                self.threads[index]
                    .thread
                    .take_interrupt_frame()
                    .map(Some)
                    .ok_or(ThreadManagerError::MissingInterruptFrame)
            }
        }
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
