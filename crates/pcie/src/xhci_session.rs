//! Детерминированная схема ресурсов двух USB-устройств в одном сеансе xHCI.
//!
//! Индексы команд и событий отличаются: управляющие передачи создают события,
//! но не занимают элементы командного кольца. Проверки защищают от пересечения
//! адресов DMA и пропуска событий при перечислении нескольких устройств.

pub const DMA_PAGE_COUNT: u64 = 12;
pub const FIRST_DEVICE: SessionDevice = SessionDevice::new(4, 1, 1);
pub const SECOND_DEVICE: SessionDevice = SessionDevice::new(8, 3, 6);
pub const RECHECK_EVENT_INDEX: usize = 11;
pub const RECHECK_TRANSFER_TRB: usize = 9;
pub const RING_ITEMS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionDevice {
    pub input: u64,
    pub output: u64,
    pub transfer: u64,
    pub descriptor: u64,
    pub command_index: usize,
    pub event_index: usize,
}

impl SessionDevice {
    const fn new(start_page: u64, command_index: usize, event_index: usize) -> Self {
        Self {
            input: start_page,
            output: start_page + 1,
            transfer: start_page + 2,
            descriptor: start_page + 3,
            command_index,
            event_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_pages_are_owned_by_one_device_only() {
        assert_eq!(FIRST_DEVICE.input, 4);
        assert_eq!(FIRST_DEVICE.descriptor + 1, SECOND_DEVICE.input);
        assert_eq!(SECOND_DEVICE.descriptor + 1, DMA_PAGE_COUNT);
        assert!(FIRST_DEVICE.transfer < SECOND_DEVICE.input);
    }

    #[test]
    fn command_ring_does_not_skip_commands() {
        assert_eq!(FIRST_DEVICE.command_index, 1);
        assert_eq!(FIRST_DEVICE.command_index + 2, SECOND_DEVICE.command_index);
        assert!(SECOND_DEVICE.command_index + 1 < RING_ITEMS - 1);
    }

    #[test]
    fn event_sequence_accounts_for_all_control_transfers() {
        // No Op (0), два события команд и три передачи каждого устройства.
        assert_eq!(FIRST_DEVICE.event_index, 1);
        assert_eq!(FIRST_DEVICE.event_index + 5, SECOND_DEVICE.event_index);
        assert_eq!(SECOND_DEVICE.event_index + 5, RECHECK_EVENT_INDEX);
        assert!(RECHECK_EVENT_INDEX < RING_ITEMS);
        assert_eq!(RECHECK_TRANSFER_TRB, 9);
        assert!(RECHECK_TRANSFER_TRB + 3 < RING_ITEMS - 1);
    }
}

/// Позиция чтения кольца событий. Управление битом цикла необходимо
/// после оборота кольца, иначе старые события читаются повторно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventCursor {
    pub index: usize,
    pub cycle: u32,
}

impl EventCursor {
    pub const fn new() -> Self {
        Self { index: 0, cycle: 1 }
    }

    pub const fn after_enumeration() -> Self {
        Self {
            index: RECHECK_EVENT_INDEX + 1,
            cycle: 1,
        }
    }

    pub fn available(&self, control: u32) -> bool {
        control & 1 == self.cycle
    }

    pub fn advance(&mut self) {
        self.index += 1;
        if self.index == RING_ITEMS {
            self.index = 0;
            self.cycle ^= 1;
        }
    }
}

#[cfg(test)]
mod resident_tests {
    use super::*;

    #[test]
    fn starts_after_last_consumed_enumeration_event() {
        let mut cursor = EventCursor::after_enumeration();
        assert_eq!(cursor.index, 12);
        assert!(cursor.available(1 | (34 << 10)));
        assert!(!cursor.available(0));
        cursor.advance();
        assert_eq!(cursor.index, 13);
    }

    #[test]
    fn toggles_cycle_only_at_ring_boundary() {
        let mut cursor = EventCursor {
            index: RING_ITEMS - 1,
            cycle: 1,
        };
        cursor.advance();
        assert_eq!(cursor.index, 0);
        assert_eq!(cursor.cycle, 0);
        assert!(cursor.available(34 << 10));
        assert!(!cursor.available((34 << 10) | 1));
    }
}

 
/// Положение следующего элемента командного кольца.
/// Последний (255-й) элемент занят переходом на начало кольца.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandCursor {
    pub index: usize,
    pub cycle: u32,
}

impl CommandCursor {
    pub const fn new() -> Self {
        Self { index: 0, cycle: 1 }
    }

    /// Возвращает true при переходе через конечный элемент Link TRB.
    pub fn advance(&mut self) -> bool {
        self.index += 1;
        if self.index < RING_ITEMS - 1 {
            return false;
        }
        self.index = 0;
        self.cycle ^= 1;
        true
    }
}

#[cfg(test)]
mod command_cursor_tests {
    use super::*;

    #[test]
    fn indexes_skip_the_link_and_toggle_the_cycle() {
        let mut cursor = CommandCursor::new();
        for _ in 0..RING_ITEMS - 2 {
            assert!(!cursor.advance());
        }
        assert_eq!(cursor.index, RING_ITEMS - 2);
        assert_eq!(cursor.cycle, 1);
        assert!(cursor.advance());
        assert_eq!(cursor.index, 0);
        assert_eq!(cursor.cycle, 0);
        assert!(!cursor.advance());
        assert_eq!(cursor.index, 1);
    }

    #[test]
    fn event_cursor_starts_at_zero_with_producer_cycle() {
        assert_eq!(EventCursor::new(), EventCursor { index: 0, cycle: 1 });
    }
}
