//! Безопасная остановка и сброс xHCI перед настройкой DMA-структур.
//!
//! Запуск контроллера выполняется только после размещения DCBAA, командного
//! и событийного колец. Этот модуль намеренно не устанавливает Run/Stop=1.

pub const USBCMD: u64 = 0x00;
pub const USBSTS: u64 = 0x04;
const RUN_STOP: u32 = 1 << 0;
const HOST_CONTROLLER_RESET: u32 = 1 << 1;
const HC_HALTED: u32 = 1 << 0;
const CONTROLLER_NOT_READY: u32 = 1 << 11;
const HOST_CONTROLLER_ERROR: u32 = 1 << 12;

/// Верхняя граница числа обращений. Для полноценного драйвера время
/// ожидания будет отсчитываться по монотонным часам ядра.
pub const MAX_POLLS: usize = 5_000_000;

pub trait OperationalRegisters {
    fn read_u32(&self, offset: u64) -> u32;
    fn write_u32(&self, offset: u64, value: u32);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetError {
    InvalidOperationalOffset,
    ControllerFault,
    HaltTimeout,
    ResetTimeout,
    ReadyTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetResult {
    pub halted_before: bool,
    pub status_after: u32,
}

/// Останавливает контроллер, сбрасывает аппаратную логику и ждёт CNR=0.
/// Не настраивает PCI и не включает DMA; регистры должны быть отображены.
///
/// После успешного завершения устройство остаётся остановленным.
/// Все существовавшие настройки командного и событийного колец недействительны.
pub fn stop_and_reset<R: OperationalRegisters>(
    registers: &R,
    operational_offset: u64,
    max_polls: usize,
) -> Result<ResetResult, ResetError> {
    if operational_offset < 0x20 || operational_offset % 4 != 0 {
        return Err(ResetError::InvalidOperationalOffset);
    }

    let command = operational_offset;
    let status = operational_offset + USBSTS;
    let initial = registers.read_u32(status);
    if initial & HOST_CONTROLLER_ERROR != 0 {
        return Err(ResetError::ControllerFault);
    }

    let halted_before = initial & HC_HALTED != 0;
    let command_value = registers.read_u32(command);
    if command_value & RUN_STOP != 0 {
        registers.write_u32(command, command_value & !RUN_STOP);
    }

    wait_for(registers, status, HC_HALTED, HC_HALTED, max_polls)
        .map_err(|_| ResetError::HaltTimeout)?;
    let command_value = registers.read_u32(command);
    registers.write_u32(command, (command_value & !RUN_STOP) | HOST_CONTROLLER_RESET);

    wait_for(registers, command, HOST_CONTROLLER_RESET, 0, max_polls)
    .map_err(|_| ResetError::ResetTimeout)?;
    wait_for(registers, status, CONTROLLER_NOT_READY, 0, max_polls)
    .map_err(|_| ResetError::ReadyTimeout)?;

    let status_after = registers.read_u32(status);
    if status_after & HOST_CONTROLLER_ERROR != 0 {
        return Err(ResetError::ControllerFault);
    }
    if status_after & HC_HALTED == 0 {
        return Err(ResetError::HaltTimeout);
    }

    Ok(ResetResult {
        halted_before,
        status_after,
    })
}

fn wait_for<R: OperationalRegisters>(
    registers: &R,
    offset: u64,
    mask: u32,
    expected: u32,
    polls: usize,
) -> Result<(), ()> {
    for _ in 0..polls {
        let value = registers.read_u32(offset);
        if value & mask == expected {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    struct FakeController {
        command: Cell<u32>,
        status: Cell<u32>,
        clear_reset: bool,
        ready_after_reset: bool,
        halt_on_stop: bool,
        writes: Cell<usize>,
    }

    impl FakeController {
        fn new() -> Self {
            Self {
                command: Cell::new(RUN_STOP),
                status: Cell::new(0),
                clear_reset: true,
                ready_after_reset: true,
                halt_on_stop: true,
                writes: Cell::new(0),
            }
        }
    }

    impl OperationalRegisters for FakeController {
        fn read_u32(&self, offset: u64) -> u32 {
            match offset {
                0x40 => self.command.get(),
                0x44 => self.status.get(),
                _ => panic!("неожиданный регистр"),
            }
        }

        fn write_u32(&self, offset: u64, value: u32) {
            assert_eq!(offset, 0x40);
            self.writes.set(self.writes.get() + 1);
            if value & HOST_CONTROLLER_RESET != 0 {
                if self.clear_reset {
                    self.command.set(0);
                } else {
                    self.command.set(value);
                }
                self.status.set(if self.ready_after_reset {
                    HC_HALTED
                } else {
                    HC_HALTED | CONTROLLER_NOT_READY
                });
                return;
            }
            self.command.set(value);
            if value & RUN_STOP == 0 && self.halt_on_stop {
                self.status.set(HC_HALTED);
            }
        }
    }

    #[test]
    fn stops_running_controller_resets_and_leaves_it_halted() {
        let registers = FakeController::new();
        let result = stop_and_reset(&registers, 0x40, 8).unwrap();
        assert!(!result.halted_before);
        assert_eq!(result.status_after & HC_HALTED, HC_HALTED);
        assert_eq!(registers.command.get(), 0);
        assert_eq!(registers.writes.get(), 2);
    }

    #[test]
    fn already_halted_controller_is_reset_without_run_command() {
        let registers = FakeController::new();
        registers.command.set(0);
        registers.status.set(HC_HALTED);
        let result = stop_and_reset(&registers, 0x40, 8).unwrap();
        assert!(result.halted_before);
        assert_eq!(registers.writes.get(), 1);
    }

    #[test]
    fn rejects_invalid_offset_and_stop_timeout() {
        let mut registers = FakeController::new();
        assert_eq!(
            stop_and_reset(&registers, 0x11, 8),
            Err(ResetError::InvalidOperationalOffset)
        );
        registers.halt_on_stop = false;
        assert_eq!(
            stop_and_reset(&registers, 0x40, 2),
            Err(ResetError::HaltTimeout)
        );
    }

    #[test]
    fn detects_reset_timeout_and_controller_not_ready() {
        let mut registers = FakeController::new();
        registers.status.set(HC_HALTED);
        registers.command.set(0);
        registers.clear_reset = false;
        assert_eq!(
            stop_and_reset(&registers, 0x40, 2),
            Err(ResetError::ResetTimeout)
        );
        let mut registers = FakeController::new();
        registers.status.set(HC_HALTED);
        registers.command.set(0);
        registers.ready_after_reset = false;
        assert_eq!(
            stop_and_reset(&registers, 0x40, 2),
            Err(ResetError::ReadyTimeout)
        );
    }

    #[test]
    fn rejects_controller_fault_without_modification() {
        let registers = FakeController::new();
        registers.status.set(HOST_CONTROLLER_ERROR);
        assert_eq!(
            stop_and_reset(&registers, 0x40, 8),
            Err(ResetError::ControllerFault)
        );
        assert_eq!(registers.writes.get(), 0);
    }
}
