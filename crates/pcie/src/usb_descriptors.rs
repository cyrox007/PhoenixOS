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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidConfiguration {
    pub value: u8,
    pub interface_number: u8,
    pub protocol: u8,
    pub endpoint_address: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

/// Находит одну загрузочную HID-точку прерываний IN в конфигурации.
///
/// Поддерживается ограниченный профиль USB-клавиатуры или мыши QEMU.
/// Длина и границы каждого дескриптора проверяются до чтения полей.
pub fn parse_hid_configuration(data: &[u8]) -> Result<HidConfiguration, DescriptorError> {
    if data.len() < 9 {
        return Err(DescriptorError::Truncated);
    }
    if data[0] != 9 || data[1] != 2 {
        return Err(DescriptorError::WrongType);
    }
    let length = usize::from(u16::from_le_bytes([data[2], data[3]]));
    if length < 9 || data[5] == 0 {
        return Err(DescriptorError::InvalidLength);
    }
    if length > data.len() {
        return Err(DescriptorError::Truncated);
    }
    let mut selected = None;
    let mut offset = 0;
    while offset < length {
        if offset + 2 > length {
            return Err(DescriptorError::Truncated);
        }
        let size = usize::from(data[offset]);
        if size < 2 || offset + size > length {
            return Err(DescriptorError::InvalidLength);
        }
        let descriptor_type = data[offset + 1];
        if descriptor_type == 4 {
            if size < 9 {
                return Err(DescriptorError::InvalidLength);
            }
            selected = None;
            if data[offset + 3] == 0
                && data[offset + 5] == 3
                && data[offset + 6] == 1
                && matches!(data[offset + 7], 1 | 2)
            {
                selected = Some((data[offset + 2], data[offset + 7]));
            }
        }
        if descriptor_type == 5 {
            if size < 7 {
                return Err(DescriptorError::InvalidLength);
            }
            if let Some((interface_number, protocol)) = selected {
                let endpoint_address = data[offset + 2];
                let packet_size = u16::from_le_bytes([data[offset + 4], data[offset + 5]]);
                let interval = data[offset + 6];
                if endpoint_address & 0x80 != 0
                    && endpoint_address & 0x0f != 0
                    && data[offset + 3] & 3 == 3
                    && (1..=64).contains(&packet_size)
                    && interval != 0
                {
                    return Ok(HidConfiguration {
                        value: data[5],
                        interface_number,
                        protocol,
                        endpoint_address,
                        max_packet_size: packet_size,
                        interval,
                    });
                }
            }
        }
        offset += size;
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
    #[test]
    fn parses_boot_keyboard_and_mouse_interrupt_endpoints() {
        let keyboard = [
            9, 2, 34, 0, 1, 1, 0, 0xa0, 50, 9, 4, 0, 0, 1, 3, 1, 1, 0, 9, 0x21, 0x11, 1, 0, 1,
            0x22, 63, 0, 7, 5, 0x81, 3, 8, 0, 10,
        ];
        assert_eq!(
            parse_hid_configuration(&keyboard),
            Ok(HidConfiguration {
                value: 1,
                interface_number: 0,
                protocol: 1,
                endpoint_address: 0x81,
                max_packet_size: 8,
                interval: 10,
            })
        );
        let mut mouse = keyboard;
        mouse[16] = 2;
        mouse[31] = 4;
        assert_eq!(parse_hid_configuration(&mouse).unwrap().protocol, 2);
        assert_eq!(parse_hid_configuration(&mouse).unwrap().max_packet_size, 4);
    }

    #[test]
    fn rejects_invalid_or_outgoing_interrupt_endpoints() {
        let mut config = [
            9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 3, 1, 1, 0, 7, 5, 0x01, 3, 8, 0, 10,
        ];
        assert_eq!(
            parse_hid_configuration(&config),
            Err(DescriptorError::NoInterface)
        );
        config[20] = 0x81;
        config[24] = 0;
        assert_eq!(
            parse_hid_configuration(&config),
            Err(DescriptorError::NoInterface)
        );
        config[24] = 10;
        config[2] = 26;
        assert_eq!(
            parse_hid_configuration(&config),
            Err(DescriptorError::Truncated)
        );
    }
}
