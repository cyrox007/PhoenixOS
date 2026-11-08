#![no_std]

//! Общая ограниченная очередь событий ввода без выделения памяти.
//! Вытеснение самой старой записи фиксируется в счётчике потерь; поэтому
//! потребитель может сбросить ошибочно сохранившиеся состояния клавиш.

use phoenix_syscall_abi::InputEventRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputQueueError {
    ZeroCapacity,
}

pub struct InputQueue<const CAPACITY: usize> {
    events: [InputEventRecord; CAPACITY],
    head: usize,
    len: usize,
    next_sequence: u64,
    lost: u64,
}

impl<const CAPACITY: usize> InputQueue<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            events: [InputEventRecord::EMPTY; CAPACITY],
            head: 0,
            len: 0,
            next_sequence: 1,
            lost: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn lost(&self) -> u64 {
        self.lost
    }

    /// Не блокирует при заполнении: вытесняет старейшее событие.
    /// Индекс и счётчик потерь всегда обновляются в одной операции.
    pub fn push(&mut self, mut event: InputEventRecord) -> Result<(), InputQueueError> {
        if CAPACITY == 0 {
            return Err(InputQueueError::ZeroCapacity);
        }
        if self.len == CAPACITY {
            self.head = (self.head + 1) % CAPACITY;
            self.len -= 1;
            self.lost = self.lost.saturating_add(1);
        }
        event.sequence = self.next_sequence;
        event.lost = self.lost;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let tail = (self.head + self.len) % CAPACITY;
        self.events[tail] = event;
        self.len += 1;
        Ok(())
    }

    /// Не удаляет запись: пользовательский адрес сначала должен пройти проверку.
    pub fn front(&self) -> Option<InputEventRecord> {
        if self.len == 0 {
            return None;
        }
        let mut event = self.events[self.head];
        event.lost = self.lost;
        Some(event)
    }

    /// Подтверждает получение ранее скопированной первой записи.
    /// Не использовать отдельно от front() под одним замком.
    pub fn commit(&mut self) -> Option<InputEventRecord> {
        let event = self.front()?;
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;
        Some(event)
    }

    pub fn pop(&mut self) -> Option<InputEventRecord> {
        self.commit()
    }
}

impl<const CAPACITY: usize> Default for InputQueue<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_keeps_usb_input_order() {
        let mut queue = InputQueue::<3>::new();
        queue.push(InputEventRecord::key(1, 4, true, 0)).unwrap();
        queue.push(InputEventRecord::key(1, 4, false, 0)).unwrap();
        queue
            .push(InputEventRecord::mouse(2, 1, 12, -6, 0))
            .unwrap();
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.pop().unwrap().sequence, 1);
        assert_eq!(queue.pop().unwrap().value, 0);
        assert_eq!(queue.pop().unwrap().y, -6);
        assert!(queue.pop().is_none());
        assert_eq!(queue.lost(), 0);
    }

    #[test]
    fn overflow_replaces_oldest_and_signals_all_losses() {
        let mut queue = InputQueue::<2>::new();
        for usage in 4..7 {
            queue
                .push(InputEventRecord::key(1, usage, true, 0))
                .unwrap();
        }
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.lost(), 1);
        let first = queue.pop().unwrap();
        assert_eq!((first.code, first.sequence, first.lost), (5, 2, 1));
        queue.push(InputEventRecord::key(1, 7, true, 0)).unwrap();
        queue.push(InputEventRecord::key(1, 8, true, 0)).unwrap();
        assert_eq!(queue.lost(), 2);
        assert_eq!(
            (queue.pop().unwrap().code, queue.pop().unwrap().code),
            (7, 8)
        );
    }

    #[test]
    fn peek_without_commit_preserves_event() {
        let mut queue = InputQueue::<1>::new();
        queue.push(InputEventRecord::device(3, false)).unwrap();
        assert_eq!(queue.front(), queue.front());
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.commit().unwrap().code, 2);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn zero_capacity_rejects_without_modulo_by_zero() {
        let mut queue = InputQueue::<0>::new();
        assert_eq!(
            queue.push(InputEventRecord::key(1, 4, true, 0)),
            Err(InputQueueError::ZeroCapacity)
        );
        assert!(queue.front().is_none());
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn wraps_ring_and_keeps_monotonic_sequences() {
        let mut queue = InputQueue::<2>::new();
        for usage in 1..=20 {
            queue
                .push(InputEventRecord::key(1, usage, true, 0))
                .unwrap();
            let event = queue.pop().unwrap();
            assert_eq!(event.code, usage);
            assert_eq!(event.sequence, u64::from(usage));
        }
        assert_eq!(queue.lost(), 0);
    }
}
