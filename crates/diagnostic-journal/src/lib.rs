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
