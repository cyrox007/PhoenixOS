//! Проверяемый разбор стандартного USB-дескриптора конфигурации без кучи.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorError {
    Truncated,
    WrongType,
    InvalidLength,
    NoInterface,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbInterface {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoint_count: u8,
    pub total_length: u16,
}

/// Возвращает первый интерфейс с альтернативной настройкой 0.
/// Никогда не читает данные за пределами полученного буфера.
pub fn parse_configuration(data: &[u8]) -> Result<UsbInterface, DescriptorError> {
    if data.len() < 9 {
        return Err(DescriptorError::Truncated);
    }
    if data[0] != 9 || data[1] != 2 {
        return Err(DescriptorError::WrongType);
    }
    let total = u16::from_le_bytes([data[2], data[3]]);
    if usize::from(total) > data.len() {
        return Err(DescriptorError::Truncated);
    }
    if total < 9 {
        return Err(DescriptorError::InvalidLength);
    }
    let mut offset = 0;
    while offset < usize::from(total) {
        if offset + 2 > usize::from(total) {
            return Err(DescriptorError::Truncated);
        }
        let length = usize::from(data[offset]);
        if length < 2 || offset + length > usize::from(total) {
            return Err(DescriptorError::InvalidLength);
        }
        if data[offset + 1] == 4 {
            if length < 9 {
                return Err(DescriptorError::InvalidLength);
            }
            if data[offset + 3] == 0 {
                return Ok(UsbInterface {
                    class: data[offset + 5],
                    subclass: data[offset + 6],
                    protocol: data[offset + 7],
                    endpoint_count: data[offset + 4],
                    total_length: total,
                });
            }
        }
        offset += length;
    }
    Err(DescriptorError::NoInterface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_hid_keyboard() {
        let config = [
            9, 2, 25, 0, 1, 1, 0, 0xa0, 50, 9, 4, 0, 0, 1, 3, 1, 1, 0, 7, 5, 0x81, 3, 8, 0, 10,
        ];
        assert_eq!(
            parse_configuration(&config),
            Ok(UsbInterface {
                class: 3,
                subclass: 1,
                protocol: 1,
                endpoint_count: 1,
                total_length: 25,
            })
        );
    }

    #[test]
    fn rejects_truncated_and_zero_length_descriptors() {
        assert_eq!(
            parse_configuration(&[9, 2]),
            Err(DescriptorError::Truncated)
        );
        let config = [9, 2, 11, 0, 1, 1, 0, 0, 0, 0, 4];
        assert_eq!(
            parse_configuration(&config),
            Err(DescriptorError::InvalidLength)
        );
    }

    #[test]
    fn recognizes_mouse_interface_after_other_descriptors() {
        let config = [
            9, 2, 28, 0, 1, 1, 0, 0x80, 50, 4, 0xff, 1, 2, 9, 4, 0, 0, 1, 3, 1, 2, 0, 6, 0x21,
            0x11, 1, 0, 1,
        ];
        assert_eq!(parse_configuration(&config).unwrap().protocol, 2);
    }
}
