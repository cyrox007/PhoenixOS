//! Безопасный разбор загрузочных USB HID-отчётов без выделения памяти.
//!
//! События остаются в области драйвера: доставка в пользовательские
//! приложения через системную очередь ввода — отдельная задача.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidError {
    ShortKeyboard,
    ShortMouse,
    InvalidKeyboardRollover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardReport {
    pub modifiers: u8,
    pub keys: [u8; 6],
}

impl KeyboardReport {
    pub fn parse(bytes: &[u8]) -> Result<Self, HidError> {
        if bytes.len() < 8 {
            return Err(HidError::ShortKeyboard);
        }
        let mut keys = [0; 6];
        keys.copy_from_slice(&bytes[2..8]);
        if keys.iter().any(|&key| (1..=3).contains(&key)) {
            return Err(HidError::InvalidKeyboardRollover);
        }
        Ok(Self {
            modifiers: bytes[0],
            keys,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyChange {
    pub usage: u8,
    pub pressed: bool,
    pub modifiers: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardState {
    previous: KeyboardReport,
}

impl KeyboardState {
    pub const fn new() -> Self {
        Self {
            previous: KeyboardReport {
                modifiers: 0,
                keys: [0; 6],
            },
        }
    }

    /// Возвращает первое изменение кода клавиши. Состояние остальных
    /// клавиш сохраняется, их отдельная доставка пока не реализована.
    pub fn update(&mut self, next: KeyboardReport) -> Option<KeyChange> {
        let previous = self.previous;
        self.previous = next;
        for &usage in previous.keys.iter().filter(|&&usage| usage != 0) {
            if !next.keys.contains(&usage) {
                return Some(KeyChange {
                    usage,
                    pressed: false,
                    modifiers: next.modifiers,
                });
            }
        }
        for &usage in next.keys.iter().filter(|&&usage| usage != 0) {
            if !previous.keys.contains(&usage) {
                return Some(KeyChange {
                    usage,
                    pressed: true,
                    modifiers: next.modifiers,
                });
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseReport {
    pub buttons: u8,
    pub delta_x: i8,
    pub delta_y: i8,
    pub wheel: i8,
}

impl MouseReport {
    pub fn parse(bytes: &[u8]) -> Result<Self, HidError> {
        if bytes.len() < 3 {
            return Err(HidError::ShortMouse);
        }
        Ok(Self {
            buttons: bytes[0],
            delta_x: bytes[1] as i8,
            delta_y: bytes[2] as i8,
            wheel: bytes.get(3).copied().unwrap_or(0) as i8,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyChange),
    Mouse(MouseReport),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_press_and_release_are_separate_events() {
        let mut state = KeyboardState::new();
        let report = KeyboardReport::parse(&[0, 0, 4, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(state.update(report), Some(KeyChange { usage: 4, pressed: true, modifiers: 0 }));
        assert_eq!(state.update(report), None);
        let release = KeyboardReport::parse(&[0; 8]).unwrap();
        assert_eq!(state.update(release), Some(KeyChange { usage: 4, pressed: false, modifiers: 0 }));
    }

    #[test]
    fn mouse_reports_preserve_signed_deltas() {
        assert_eq!(
            MouseReport::parse(&[1, 12, 250, 0]),
            Ok(MouseReport { buttons: 1, delta_x: 12, delta_y: -6, wheel: 0 })
        );
    }

    #[test]
    fn invalid_reports_do_not_change_keyboard_state() {
        let mut state = KeyboardState::new();
        assert_eq!(KeyboardReport::parse(&[0; 7]), Err(HidError::ShortKeyboard));
        assert_eq!(KeyboardReport::parse(&[0, 0, 1, 0, 0, 0, 0, 0]), Err(HidError::InvalidKeyboardRollover));
        assert_eq!(MouseReport::parse(&[0, 0]), Err(HidError::ShortMouse));
        assert_eq!(state.update(KeyboardReport::parse(&[0; 8]).unwrap()), None);
    }
}
