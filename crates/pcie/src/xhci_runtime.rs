//! Жизненный цикл службы USB xHCI между потоками ядра.
//!
//! Никакой поток не освобождает DMA самостоятельно: запрос остановки
//! подтверждает поток-владелец, после чего распорядитель может остановить
//! контроллер и вернуть страницы. Изменения состояния атомарны.

use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RuntimePhase {
    Ready = 0,
    Running = 1,
    StopRequested = 2,
    Stopped = 3,
    Faulted = 4,
}

impl RuntimePhase {
    const fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::Ready,
            1 => Self::Running,
            2 => Self::StopRequested,
            3 => Self::Stopped,
            _ => Self::Faulted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeError {
    InvalidTransition,
}

/// Состояние потока-владельца без выделения памяти.
/// Остановка аппаратных DMA и MMIO выполняется отдельно, после Stopped.
pub struct RuntimeLifecycle {
    phase: AtomicU8,
}

impl RuntimeLifecycle {
    pub const fn new() -> Self {
        Self {
            phase: AtomicU8::new(RuntimePhase::Ready as u8),
        }
    }

    pub fn phase(&self) -> RuntimePhase {
        RuntimePhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    pub fn start(&self) -> Result<(), RuntimeError> {
        self.transition(RuntimePhase::Ready, RuntimePhase::Running)
    }

    pub fn request_stop(&self) -> Result<(), RuntimeError> {
        self.transition(RuntimePhase::Running, RuntimePhase::StopRequested)
    }

    pub fn acknowledge_stop(&self) -> Result<(), RuntimeError> {
        self.transition(RuntimePhase::StopRequested, RuntimePhase::Stopped)
    }

    /// Невосстановимый отказ завершает обработку без освобождения DMA.
    /// Оборудование должен отдельно и безопасно остановить распорядитель.
    pub fn fail(&self) {
        self.phase
            .store(RuntimePhase::Faulted as u8, Ordering::Release);
    }

    fn transition(&self, from: RuntimePhase, to: RuntimePhase) -> Result<(), RuntimeError> {
        self.phase
            .compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| RuntimeError::InvalidTransition)
    }
}

impl Default for RuntimeLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_requires_explicit_stop_and_acknowledgement() {
        let lifecycle = RuntimeLifecycle::new();
        assert_eq!(lifecycle.phase(), RuntimePhase::Ready);
        assert_eq!(
            lifecycle.acknowledge_stop(),
            Err(RuntimeError::InvalidTransition)
        );
        lifecycle.start().unwrap();
        assert_eq!(lifecycle.start(), Err(RuntimeError::InvalidTransition));
        lifecycle.request_stop().unwrap();
        assert_eq!(lifecycle.phase(), RuntimePhase::StopRequested);
        assert_eq!(
            lifecycle.request_stop(),
            Err(RuntimeError::InvalidTransition)
        );
        lifecycle.acknowledge_stop().unwrap();
        assert_eq!(lifecycle.phase(), RuntimePhase::Stopped);
        assert_eq!(lifecycle.start(), Err(RuntimeError::InvalidTransition));
    }

    #[test]
    fn fault_prevents_restarting_or_releasing_early() {
        let lifecycle = RuntimeLifecycle::new();
        lifecycle.start().unwrap();
        lifecycle.fail();
        assert_eq!(lifecycle.phase(), RuntimePhase::Faulted);
        assert_eq!(
            lifecycle.request_stop(),
            Err(RuntimeError::InvalidTransition)
        );
        assert_eq!(
            lifecycle.acknowledge_stop(),
            Err(RuntimeError::InvalidTransition)
        );
    }

    #[test]
    fn cannot_stop_unstarted_controller() {
        let lifecycle = RuntimeLifecycle::new();
        assert_eq!(
            lifecycle.request_stop(),
            Err(RuntimeError::InvalidTransition)
        );
        assert_eq!(lifecycle.phase(), RuntimePhase::Ready);
    }
}
