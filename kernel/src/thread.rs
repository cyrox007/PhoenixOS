use alloc::boxed::Box;
use alloc::vec;

use crate::arch::x86_64::context::{
    Context, ReturningThreadEntry, StackError, ThreadEntry, ThreadExit,
};

const MINIMUM_STACK_SIZE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Blocked,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadError {
    StackTooSmall,
    Stack(StackError),
    InvalidState {
        current: ThreadState,
        operation: &'static str,
    },
}

pub struct KernelThread {
    id: ThreadId,
    context: Context,
    stack: Box<[u8]>,
    state: ThreadState,
}

impl KernelThread {
    pub fn new(
        id: ThreadId,
        stack_size: usize,
        entry: ThreadEntry,
        argument: usize,
    ) -> Result<Self, ThreadError> {
        if stack_size < MINIMUM_STACK_SIZE {
            return Err(ThreadError::StackTooSmall);
        }

        let mut stack = vec![0; stack_size].into_boxed_slice();
        let context =
            Context::for_stack(&mut stack, entry, argument).map_err(ThreadError::Stack)?;

        Ok(Self {
            id,
            context,
            stack,
            state: ThreadState::Ready,
        })
    }

    pub fn new_returning(
        id: ThreadId,
        stack_size: usize,
        entry: ReturningThreadEntry,
        argument: usize,
        exit: ThreadExit,
        exit_argument: usize,
    ) -> Result<Self, ThreadError> {
        if stack_size < MINIMUM_STACK_SIZE {
            return Err(ThreadError::StackTooSmall);
        }

        let mut stack = vec![0; stack_size].into_boxed_slice();
        let context = Context::for_returning_stack(
            &mut stack,
            entry,
            argument,
            exit,
            exit_argument,
        )
        .map_err(ThreadError::Stack)?;

        Ok(Self {
            id,
            context,
            stack,
            state: ThreadState::Ready,
        })
    }

    pub const fn id(&self) -> ThreadId {
        self.id
    }

    pub const fn state(&self) -> ThreadState {
        self.state
    }

    pub fn stack_size(&self) -> usize {
        self.stack.len()
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut Context {
        &mut self.context
    }

    pub fn start(&mut self) -> Result<(), ThreadError> {
        self.transition(ThreadState::Ready, ThreadState::Running, "start")
    }

    pub fn block(&mut self) -> Result<(), ThreadError> {
        match self.state {
            ThreadState::Ready | ThreadState::Running => {
                self.state = ThreadState::Blocked;
                Ok(())
            }
            current => Err(ThreadError::InvalidState {
                current,
                operation: "block",
            }),
        }
    }

    pub fn wake(&mut self) -> Result<(), ThreadError> {
        self.transition(ThreadState::Blocked, ThreadState::Ready, "wake")
    }

    pub fn finish(&mut self) -> Result<(), ThreadError> {
        self.transition(ThreadState::Running, ThreadState::Finished, "finish")
    }

    fn transition(
        &mut self,
        expected: ThreadState,
        next: ThreadState,
        operation: &'static str,
    ) -> Result<(), ThreadError> {
        if self.state != expected {
            return Err(ThreadError::InvalidState {
                current: self.state,
                operation,
            });
        }

        self.state = next;
        Ok(())
    }
}
