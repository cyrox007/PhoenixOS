//! Обнаружение подключений и управляемый сброс корневых портов xHCI.
//!
//! Регистр PORTSC содержит смешанные поля: запись единицы в биты изменений
//! сбрасывает признаки, а запись единицы в PED выключает порт. Поэтому
//! никогда нельзя записывать в него ранее прочитанное значение целиком.

use crate::xhci_control::OperationalRegisters;

const PORT_REGISTERS_OFFSET: u64 = 0x400;
const PORT_REGISTER_STRIDE: u64 = 0x10;
const PORT_CONNECTED: u32 = 1 << 0;
const PORT_ENABLED: u32 = 1 << 1;
const PORT_OVERCURRENT: u32 = 1 << 3;
const PORT_RESET: u32 = 1 << 4;
const PORT_POWER: u32 = 1 << 9;
const PORT_SPEED_SHIFT: u32 = 10;
const PORT_SPEED_MASK: u32 = 0xf;
const PORT_CHANGE_MASK: u32 = 0x00fe_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortError {
    InvalidPortCount,
    RegisterRange,
    Overcurrent(u8),
    ResetTimeout(u8),
    DeviceDisconnected(u8),
    PortNotEnabled(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortSummary {
    pub total: u8,
    pub connected: u8,
    pub enabled: u8,
    pub reset: u8,
    pub first_connected_port: u8,
    pub first_speed: u8,
}

fn register_offset(operational: u64, port: u8, bar_bytes: u64) -> Result<u64, PortError> {
    let index = u64::from(port.checked_sub(1).ok_or(PortError::InvalidPortCount)?);
    let offset = operational
        .checked_add(PORT_REGISTERS_OFFSET)
        .and_then(|value| value.checked_add(index * PORT_REGISTER_STRIDE))
        .ok_or(PortError::RegisterRange)?;
    if offset.checked_add(4).is_none_or(|end| end > bar_bytes) {
        return Err(PortError::RegisterRange);
    }
    Ok(offset)
}

fn speed(status: u32) -> u8 {
    ((status >> PORT_SPEED_SHIFT) & PORT_SPEED_MASK) as u8
}

/// Сканирует порты и сбрасывает подключённые, ещё не активные устройства.
///
/// Вся запись в PORTSC формируется из безопасных полей: питание (PP) и
/// команда сброса (PR). Биты W1C и PED всегда записываются нулями.
/// Сброс выполняется только для портов с CCS=1 и PED=0.
pub fn scan_ports<R: OperationalRegisters>(
    registers: &R,
    operational: u64,
    ports: u8,
    bar_bytes: u64,
    max_polls: usize,
) -> Result<PortSummary, PortError> {
    if ports == 0 {
        return Err(PortError::InvalidPortCount);
    }

    let mut summary = PortSummary {
        total: ports,
        connected: 0,
        enabled: 0,
        reset: 0,
        first_connected_port: 0,
        first_speed: 0,
    };

    for port in 1..=ports {
        let offset = register_offset(operational, port, bar_bytes)?;
        let initial = registers.read_u32(offset);
        if initial & PORT_CONNECTED == 0 {
            continue;
        }
        if initial & PORT_OVERCURRENT != 0 {
            return Err(PortError::Overcurrent(port));
        }

        summary.connected += 1;
        if summary.first_connected_port == 0 {
            summary.first_connected_port = port;
        }

        let final_status = if initial & PORT_ENABLED != 0 {
            initial
        } else {
            // Не возвращаем W1C-биты (в том числе CSC/PRC) и бит PED.
            let command = (initial & PORT_POWER) | PORT_RESET;
            registers.write_u32(offset, command);
            let mut completed = None;
            for _ in 0..max_polls {
                let current = registers.read_u32(offset);
                if current & PORT_CONNECTED == 0 {
                    return Err(PortError::DeviceDisconnected(port));
                }
                if current & PORT_OVERCURRENT != 0 {
                    return Err(PortError::Overcurrent(port));
                }
                if current & PORT_RESET == 0 {
                    completed = Some(current);
                    break;
                }
                core::hint::spin_loop();
            }
            summary.reset += 1;
            completed.ok_or(PortError::ResetTimeout(port))?
        };

        if final_status & PORT_ENABLED == 0 {
            return Err(PortError::PortNotEnabled(port));
        }
        if speed(final_status) == 0 {
            return Err(PortError::PortNotEnabled(port));
        }
        summary.enabled += 1;
        if port == summary.first_connected_port {
            summary.first_speed = speed(final_status);
        }

        // Очищаем только прочитанные признаки изменений; сохраняем питание.
        let changes = final_status & PORT_CHANGE_MASK;
        if changes != 0 {
            registers.write_u32(offset, (final_status & PORT_POWER) | changes);
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    struct FakePorts {
        port_one: Cell<u32>,
        port_two: Cell<u32>,
        writes: Cell<u32>,
        finish_reset: bool,
    }

    impl FakePorts {
        fn new(first: u32, second: u32) -> Self {
            Self {
                port_one: Cell::new(first),
                port_two: Cell::new(second),
                writes: Cell::new(0),
                finish_reset: true,
            }
        }
    }

    impl OperationalRegisters for FakePorts {
        fn read_u32(&self, offset: u64) -> u32 {
            match offset {
                0x440 => self.port_one.get(),
                0x450 => self.port_two.get(),
                _ => panic!("недопустимое смещение порта"),
            }
        }

        fn write_u32(&self, offset: u64, value: u32) {
            assert_eq!(offset, 0x440);
            assert_eq!(value & PORT_ENABLED, 0);
            self.writes.set(self.writes.get() + 1);
            if value & PORT_RESET != 0 && self.finish_reset {
                self.port_one
                    .set(PORT_CONNECTED | PORT_ENABLED | PORT_POWER | (3 << 10));
            }
        }
    }

    #[test]
    fn finds_two_ports_and_resets_only_disabled_one() {
        let ports = FakePorts::new(
            PORT_CONNECTED | PORT_POWER | (3 << 10) | (1 << 17),
            PORT_CONNECTED | PORT_ENABLED | PORT_POWER | (1 << 10),
        );
        let summary = scan_ports(&ports, 0x40, 2, 0x4000, 5).unwrap();
        assert_eq!(summary.connected, 2);
        assert_eq!(summary.enabled, 2);
        assert_eq!(summary.reset, 1);
        assert_eq!(summary.first_connected_port, 1);
        assert_eq!(summary.first_speed, 3);
        assert_eq!(ports.writes.get(), 1);
    }

    #[test]
    fn empty_ports_do_not_cause_writes() {
        let ports = FakePorts::new(PORT_POWER, PORT_POWER);
        let summary = scan_ports(&ports, 0x40, 2, 0x4000, 3).unwrap();
        assert_eq!(summary.connected, 0);
        assert_eq!(ports.writes.get(), 0);
    }

    #[test]
    fn invalid_register_range_and_reset_timeout() {
        let ports = FakePorts::new(PORT_CONNECTED | PORT_POWER, 0);
        assert_eq!(
            scan_ports(&ports, 0x40, 2, 0x448, 3),
            Err(PortError::RegisterRange)
        );
        let mut ports = FakePorts::new(PORT_CONNECTED | PORT_POWER, 0);
        ports.finish_reset = false;
        assert_eq!(
            scan_ports(&ports, 0x40, 1, 0x4000, 3),
            Err(PortError::PortNotEnabled(1))
        );
        ports.port_one.set(PORT_CONNECTED | PORT_POWER | PORT_RESET);
        assert_eq!(
            scan_ports(&ports, 0x40, 1, 0x4000, 2),
            Err(PortError::ResetTimeout(1))
        );
    }

    #[test]
    fn rejects_overcurrent_and_invalid_port_count() {
        let ports = FakePorts::new(PORT_CONNECTED | PORT_OVERCURRENT, 0);
        assert_eq!(
            scan_ports(&ports, 0x40, 1, 0x4000, 2),
            Err(PortError::Overcurrent(1))
        );
        assert_eq!(
            scan_ports(&ports, 0x40, 0, 0x4000, 2),
            Err(PortError::InvalidPortCount)
        );
    }
}
