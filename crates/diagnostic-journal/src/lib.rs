#![no_std]

//! Ограниченный журнал пассивной диагностики без кучи и доступа к накопителям.
//! Переполнение всегда фиксируется и не может считаться полным протоколом.

use core::fmt::{self, Write};

#[derive(Debug)]
pub struct Journal<const N: usize> {
    bytes: [u8; N],
    len: usize,
    truncated: bool,
    lines: usize,
}

impl<const N: usize> Journal<N> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            truncated: false,
            lines: 0,
        }
    }

    pub fn is_complete(&self) -> bool {
        !self.truncated
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn line_count(&self) -> usize {
        self.lines
    }

    pub fn contents(&self) -> &str {
        // Запись производится только целыми корректными UTF-8 фрагментами.
        core::str::from_utf8(&self.bytes[..self.len]).expect("журнал должен содержать UTF-8")
    }

    /// Возвращает строку по индексу без завершающего перевода строки.
    pub fn line(&self, index: usize) -> Option<&str> {
        self.contents().split_terminator('\n').nth(index)
    }

    pub fn push_line(&mut self, arguments: fmt::Arguments<'_>) {
        let _ = self.write_fmt(arguments);
        let _ = self.write_str("\n");
    }
}

impl<const N: usize> Default for Journal<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for Journal<N> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Ok(());
        }
        let Some(end) = self.len.checked_add(value.len()) else {
            self.truncated = true;
            return Ok(());
        };
        if end > N {
            self.truncated = true;
            return Ok(());
        }
        self.bytes[self.len..end].copy_from_slice(value.as_bytes());
        self.len = end;
        self.lines += value.bytes().filter(|byte| *byte == b'\n').count();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_lines_in_order_without_allocator() {
        let mut journal = Journal::<64>::new();
        journal.push_line(format_args!("RAM={}", 128));
        journal.push_line(format_args!("PAT={}", "WB"));
        assert_eq!(journal.line_count(), 2);
        assert_eq!(journal.line(0), Some("RAM=128"));
        assert_eq!(journal.line(1), Some("PAT=WB"));
        assert_eq!(journal.line(2), None);
        assert!(journal.is_complete());
    }

    #[test]
    fn overflowing_journal_never_reports_complete() {
        let mut journal = Journal::<8>::new();
        journal.push_line(format_args!("first"));
        journal.push_line(format_args!("second"));
        assert!(journal.is_truncated());
        assert_eq!(journal.contents(), "first\n");
        journal.push_line(format_args!("third"));
        assert_eq!(journal.contents(), "first\n");
    }

    #[test]
    fn zero_capacity_is_safe() {
        let mut journal = Journal::<0>::new();
        journal.push_line(format_args!("x"));
        assert!(journal.is_truncated());
        assert_eq!(journal.line_count(), 0);
    }

    #[test]
    fn utf8_is_never_cut_mid_character() {
        let mut journal = Journal::<3>::new();
        journal.push_line(format_args!("Ж"));
        assert_eq!(journal.contents(), "Ж\n");
        journal.push_line(format_args!("я"));
        assert!(journal.is_truncated());
        assert_eq!(journal.contents(), "Ж\n");
    }
}

/// Смена страниц по счётчику тактов ЦП без таймера, прерываний и клавиатуры.
/// Интервал измеряется в тактах, а не секундах: частота ЦП зависит от оборудования.
#[derive(Debug, Clone, Copy)]
pub struct PageCycle {
    page: usize,
    pages: usize,
    last_tick: u64,
    tick_interval: u64,
}

impl PageCycle {
    pub fn new(pages: usize, initial_tick: u64, tick_interval: u64) -> Self {
        Self {
            page: 0,
            pages: pages.max(1),
            last_tick: initial_tick,
            tick_interval: tick_interval.max(1),
        }
    }

    pub fn page(&self) -> usize {
        self.page
    }

    /// Возвращает true, если экран необходимо перерисовать.
    pub fn advance(&mut self, now: u64) -> bool {
        if self.pages <= 1 || now.wrapping_sub(self.last_tick) < self.tick_interval {
            return false;
        }
        self.last_tick = now;
        self.page = (self.page + 1) % self.pages;
        true
    }

    /// Ручное листание остаётся доступным, когда контроллер клавиатуры работает.
    pub fn previous(&mut self, now: u64) {
        self.page = self.page.saturating_sub(1);
        self.last_tick = now;
    }

    pub fn next(&mut self, now: u64) {
        self.page = (self.page + 1).min(self.pages - 1);
        self.last_tick = now;
    }
}

#[cfg(test)]
mod page_cycle_tests {
    use super::PageCycle;

    #[test]
    fn rotates_without_keyboard_and_wraps_last_page() {
        let mut cycle = PageCycle::new(3, 100, 10);
        assert!(!cycle.advance(109));
        assert!(cycle.advance(110));
        assert_eq!(cycle.page(), 1);
        assert!(cycle.advance(120));
        assert_eq!(cycle.page(), 2);
        assert!(cycle.advance(130));
        assert_eq!(cycle.page(), 0);
    }

    #[test]
    fn never_rotates_a_single_page() {
        let mut cycle = PageCycle::new(0, 0, 1);
        assert!(!cycle.advance(100));
        assert_eq!(cycle.page(), 0);
    }

    #[test]
    fn manual_navigation_restarts_the_interval() {
        let mut cycle = PageCycle::new(3, 0, 100);
        cycle.next(50);
        assert_eq!(cycle.page(), 1);
        assert!(!cycle.advance(149));
        cycle.previous(150);
        assert_eq!(cycle.page(), 0);
        assert!(!cycle.advance(249));
    }

    #[test]
    fn handles_wrapping_tick_counter() {
        let mut cycle = PageCycle::new(2, u64::MAX - 5, 10);
        assert!(!cycle.advance(3));
        assert!(cycle.advance(4));
        assert_eq!(cycle.page(), 1);
    }
}
