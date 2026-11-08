//! Ограниченное ожидание готовности и завершений NVMe.
//!
//! Чтение регистров и памяти очередей передаётся вызывающим кодом.
//! Это позволяет проверять остановку, потерю готовности и фатальные
//! ошибки без выдачи опасных команд настоящему накопителю.
//!
//! Лимит относится к числу опросов, а не к физическому времени:
//! точный срок в миллисекундах требует откалиброванных часов платформы.

use crate::nvme_admin::{CONTROLLER_FATAL, CONTROLLER_READY};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    ControllerFault,
    ControllerNotReady,
    TimedOut,
}

/// Для остановки достаточно аппаратного RDY=0, даже если CFS установлен.
/// При включении CFS всегда означает неисправность, даже при RDY=1.
pub fn wait_ready(
    mut controller_status: impl FnMut() -> u32,
    expected_ready: bool,
    max_polls: usize,
) -> Result<(), WaitError> {
    for _ in 0..max_polls {
        let status = controller_status();
        let ready = status & CONTROLLER_READY != 0;
        if expected_ready && status & CONTROLLER_FATAL != 0 {
            return Err(WaitError::ControllerFault);
        }
        if ready == expected_ready {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(WaitError::TimedOut)
}

/// Не принимает фазовый признак как успешное завершение, если устройство
/// уже неисправно или перестало быть готовым. Остальные слова завершения
/// проверяет вызывающий код после барьера чтения памяти.
pub fn wait_completion_phase(
    mut controller_status: impl FnMut() -> u32,
    mut completion_status: impl FnMut() -> u32,
    expected_phase: bool,
    max_polls: usize,
) -> Result<u32, WaitError> {
    for _ in 0..max_polls {
        let status = controller_status();
        if status & CONTROLLER_FATAL != 0 {
            return Err(WaitError::ControllerFault);
        }
        if status & CONTROLLER_READY == 0 {
            return Err(WaitError::ControllerNotReady);
        }
        let completion = completion_status();
        if ((completion >> 16) & 1 != 0) == expected_phase {
            return Ok(completion);
        }
        core::hint::spin_loop();
    }
    Err(WaitError::TimedOut)
}

/// Детерминированная проверка модели отказов в рабочем ядре QEMU.
/// Ни одна из проверок не обращается к физическому контроллеру или DMA.
pub fn failure_model_checks() -> bool {
    let mut polls = 0;
    let completed = wait_completion_phase(
        || CONTROLLER_READY,
        || {
            polls += 1;
            if polls == 3 { 1 << 16 } else { 0 }
        },
        true,
        4,
    ) == Ok(1 << 16);
    let timeout = wait_completion_phase(|| CONTROLLER_READY, || 0, true, 3)
        == Err(WaitError::TimedOut);
    let fatal = wait_completion_phase(
        || CONTROLLER_READY | CONTROLLER_FATAL,
        || 1 << 16,
        true,
        3,
    ) == Err(WaitError::ControllerFault);
    let not_ready = wait_completion_phase(|| 0, || 1 << 16, true, 3)
        == Err(WaitError::ControllerNotReady);
    let stop = wait_ready(|| CONTROLLER_FATAL, false, 3) == Ok(());
    let stuck = wait_ready(|| CONTROLLER_READY, false, 3)
        == Err(WaitError::TimedOut);
    completed && polls == 3 && timeout && fatal && not_ready && stop && stuck
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_model_covers_all_transitions() {
        assert!(failure_model_checks());
    }

    #[test]
    fn completion_arriving_on_last_poll_is_accepted() {
        let mut checks = 0;
        assert_eq!(
            wait_completion_phase(
                || CONTROLLER_READY,
                || {
                    checks += 1;
                    if checks == 4 { 1 << 16 } else { 0 }
                },
                true,
                4,
            ),
            Ok(1 << 16),
        );
        assert_eq!(checks, 4);
    }

    #[test]
    fn mismatched_phase_is_bounded_and_never_accepted() {
        let mut checks = 0;
        assert_eq!(
            wait_completion_phase(
                || CONTROLLER_READY,
                || {
                    checks += 1;
                    1 << 16
                },
                false,
                5,
            ),
            Err(WaitError::TimedOut),
        );
        assert_eq!(checks, 5);
        assert_eq!(
            wait_completion_phase(|| CONTROLLER_READY, || 0, true, 0),
            Err(WaitError::TimedOut),
        );
    }

    #[test]
    fn fatal_or_stopped_controller_never_reads_completion() {
        let mut dma_reads = 0;
        assert_eq!(
            wait_completion_phase(
                || CONTROLLER_FATAL | CONTROLLER_READY,
                || {
                    dma_reads += 1;
                    1 << 16
                },
                true,
                10,
            ),
            Err(WaitError::ControllerFault),
        );
        assert_eq!(
            wait_completion_phase(
                || 0,
                || {
                    dma_reads += 1;
                    1 << 16
                },
                true,
                10,
            ),
            Err(WaitError::ControllerNotReady),
        );
        assert_eq!(dma_reads, 0);
    }

    #[test]
    fn ready_wait_accepts_confirmed_stop_and_rejects_fault_on_start() {
        assert_eq!(wait_ready(|| CONTROLLER_FATAL, false, 1), Ok(()));
        assert_eq!(
            wait_ready(|| CONTROLLER_FATAL | CONTROLLER_READY, true, 1),
            Err(WaitError::ControllerFault),
        );
        assert_eq!(
            wait_ready(|| CONTROLLER_READY, false, 2),
            Err(WaitError::TimedOut),
        );
        assert_eq!(
            wait_ready(|| 0, true, 0),
            Err(WaitError::TimedOut),
        );
    }
}
