#![no_std]

//! Общая граница сетевого адаптера для ядра PhoenixOS.
//!
//! Кадры Ethernet передаются без контрольной суммы FCS. Буферы и их жизненный
//! цикл принадлежат вызывающему коду; реализация не выделяет память.
//! Протоколы Ethernet, ARP и IP находятся выше этого слоя.

/// Длина заголовка Ethernet II без меток VLAN и контрольной суммы.
pub const ETHERNET_HEADER_LEN: usize = 14;

/// Однозначные ошибки передачи и приёма. Ошибка не считается успешным обменом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkError {
    InvalidMacAddress,
    InvalidFrameLimit,
    FrameTooShort,
    FrameTooLong,
    LinkDown,
    WouldBlock,
    BufferTooSmall { required: usize },
    InvalidDeviceFrame,
    DeviceFailure,
}

/// Аппаратный адрес отдельного интерфейса. Нулевой и групповой адреса
/// недопустимы как собственный адрес сетевого адаптера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAddress([u8; 6]);

impl MacAddress {
    pub fn new(bytes: [u8; 6]) -> Result<Self, NetworkError> {
        if bytes.iter().all(|byte| *byte == 0) || bytes[0] & 1 != 0 {
            return Err(NetworkError::InvalidMacAddress);
        }

        Ok(Self(bytes))
    }

    pub const fn octets(self) -> [u8; 6] {
        self.0
    }
}

/// Состояние носителя. Unknown допустим, если адаптер не сообщает состояние
/// соединения (например, некоторые конфигурации VirtIO-net).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Up,
    Down,
    Unknown,
}

/// Характеристики адаптера. Максимальная длина включает заголовок Ethernet,
/// но не FCS; при поддержке VLAN драйвер учитывает её в указанном пределе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkDeviceInfo {
    mac_address: MacAddress,
    max_frame_size: usize,
}

impl NetworkDeviceInfo {
    pub fn new(mac_address: MacAddress, max_frame_size: usize) -> Result<Self, NetworkError> {
        if max_frame_size < ETHERNET_HEADER_LEN {
            return Err(NetworkError::InvalidFrameLimit);
        }

        Ok(Self {
            mac_address,
            max_frame_size,
        })
    }

    pub const fn mac_address(self) -> MacAddress {
        self.mac_address
    }

    pub const fn max_frame_size(self) -> usize {
        self.max_frame_size
    }

    pub fn validate_frame_length(self, length: usize) -> Result<(), NetworkError> {
        if length < ETHERNET_HEADER_LEN {
            return Err(NetworkError::FrameTooShort);
        }
        if length > self.max_frame_size {
            return Err(NetworkError::FrameTooLong);
        }
        Ok(())
    }
}

/// Не блокирующая граница между сетевым драйвером и стеком протоколов.
///
/// Драйвер владеет аппаратными очередями, а вызывающая сторона — переданными
/// буферами. Для DMA драйвер самостоятельно обеспечивает владение, отображение,
/// согласованность памяти и возврат страниц через общий интерфейс DMA.
///
/// При `Ok(None)` входящего кадра пока нет. При `BufferTooSmall` драйвер обязан
/// оставить входящий кадр в очереди и указать фактическую нужную длину.
/// При `WouldBlock` передача не была принята устройством и может повторяться.
/// Другие ошибки не должны объявлять кадр отправленным или принятым.
/// При `Ok(Some(n))` записано ровно n байт в начало предоставленного буфера;
/// частичный кадр как успех возвращать запрещено.
pub trait NetworkDevice {
    fn info(&self) -> NetworkDeviceInfo;

    fn link_state(&self) -> LinkState;

    fn poll_receive(&mut self, destination: &mut [u8]) -> Result<Option<usize>, NetworkError>;

    fn transmit(&mut self, frame: &[u8]) -> Result<(), NetworkError>;
}

/// Общие проверки размера, состояния соединения и результата драйвера.
/// Тип не требует кучи и не содержит аппаратно-зависимых решений.
pub struct NetworkInterface<Device: NetworkDevice> {
    device: Device,
    info: NetworkDeviceInfo,
}

impl<Device: NetworkDevice> NetworkInterface<Device> {
    pub fn new(device: Device) -> Self {
        let info = device.info();
        Self { device, info }
    }

    pub const fn info(&self) -> NetworkDeviceInfo {
        self.info
    }

    pub fn link_state(&self) -> LinkState {
        self.device.link_state()
    }

    pub fn transmit(&mut self, frame: &[u8]) -> Result<(), NetworkError> {
        self.info.validate_frame_length(frame.len())?;
        if self.link_state() == LinkState::Down {
            return Err(NetworkError::LinkDown);
        }

        self.device.transmit(frame)
    }

    pub fn poll_receive(&mut self, destination: &mut [u8]) -> Result<Option<usize>, NetworkError> {
        if self.link_state() == LinkState::Down {
            return Err(NetworkError::LinkDown);
        }

        let Some(length) = self.device.poll_receive(destination)? else {
            return Ok(None);
        };

        if length > destination.len() || self.info.validate_frame_length(length).is_err() {
            return Err(NetworkError::InvalidDeviceFrame);
        }

        Ok(Some(length))
    }

    pub fn into_device(self) -> Device {
        self.device
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: [u8; 64] = [0x5a; 64];

    struct TestDevice {
        info: NetworkDeviceInfo,
        link: LinkState,
        pending: Option<&'static [u8]>,
        busy: bool,
        reported_length: Option<usize>,
        last_transmitted: [u8; 64],
        transmitted_length: usize,
    }

    impl TestDevice {
        fn new() -> Self {
            let mac = MacAddress::new([0x02, 0, 0, 0, 0, 1]).unwrap();
            Self {
                info: NetworkDeviceInfo::new(mac, 64).unwrap(),
                link: LinkState::Up,
                pending: None,
                busy: false,
                reported_length: None,
                last_transmitted: [0; 64],
                transmitted_length: 0,
            }
        }
    }

    impl NetworkDevice for TestDevice {
        fn info(&self) -> NetworkDeviceInfo {
            self.info
        }

        fn link_state(&self) -> LinkState {
            self.link
        }

        fn poll_receive(&mut self, destination: &mut [u8]) -> Result<Option<usize>, NetworkError> {
            let Some(frame) = self.pending else {
                return Ok(None);
            };
            if destination.len() < frame.len() {
                return Err(NetworkError::BufferTooSmall {
                    required: frame.len(),
                });
            }

            destination[..frame.len()].copy_from_slice(frame);
            self.pending = None;
            Ok(Some(self.reported_length.unwrap_or(frame.len())))
        }

        fn transmit(&mut self, frame: &[u8]) -> Result<(), NetworkError> {
            if self.busy {
                return Err(NetworkError::WouldBlock);
            }

            self.last_transmitted[..frame.len()].copy_from_slice(frame);
            self.transmitted_length = frame.len();
            Ok(())
        }
    }

    #[test]
    fn accepts_only_unicast_nonzero_local_address() {
        assert_eq!(
            MacAddress::new([0; 6]),
            Err(NetworkError::InvalidMacAddress)
        );
        assert_eq!(
            MacAddress::new([0xff; 6]),
            Err(NetworkError::InvalidMacAddress)
        );
        assert_eq!(
            MacAddress::new([0x01, 0, 0, 0, 0, 1]),
            Err(NetworkError::InvalidMacAddress)
        );
        assert_eq!(
            MacAddress::new([0x02, 0, 0, 0, 0, 1]).unwrap().octets(),
            [0x02, 0, 0, 0, 0, 1]
        );
    }

    #[test]
    fn rejects_unusable_frame_limit() {
        let mac = MacAddress::new([0x02, 0, 0, 0, 0, 1]).unwrap();
        assert_eq!(
            NetworkDeviceInfo::new(mac, 13),
            Err(NetworkError::InvalidFrameLimit)
        );
        assert!(NetworkDeviceInfo::new(mac, 14).is_ok());
    }

    #[test]
    fn validates_transmission_before_accessing_device() {
        let mut interface = NetworkInterface::new(TestDevice::new());
        assert_eq!(
            interface.transmit(&FRAME[..13]),
            Err(NetworkError::FrameTooShort)
        );
        let oversized = [0; 65];
        assert_eq!(
            interface.transmit(&oversized),
            Err(NetworkError::FrameTooLong)
        );
        assert_eq!(interface.transmit(&FRAME), Ok(()));
        let device = interface.into_device();
        assert_eq!(device.transmitted_length, FRAME.len());
        assert_eq!(device.last_transmitted, FRAME);
    }

    #[test]
    fn reports_busy_without_implicit_retry_or_partial_transmission() {
        let mut device = TestDevice::new();
        device.busy = true;
        let mut interface = NetworkInterface::new(device);
        assert_eq!(interface.transmit(&FRAME), Err(NetworkError::WouldBlock));
        let mut device = interface.into_device();
        assert_eq!(device.transmitted_length, 0);
        device.busy = false;
        let mut interface = NetworkInterface::new(device);
        assert_eq!(interface.transmit(&FRAME), Ok(()));
    }

    #[test]
    fn receive_is_nonblocking_and_preserves_frame_on_short_buffer() {
        let mut device = TestDevice::new();
        device.pending = Some(&FRAME);
        let mut interface = NetworkInterface::new(device);
        let mut short = [0xa5; 16];
        assert_eq!(
            interface.poll_receive(&mut short),
            Err(NetworkError::BufferTooSmall { required: 64 })
        );
        assert_eq!(short, [0xa5; 16]);

        let mut buffer = [0; 64];
        assert_eq!(interface.poll_receive(&mut buffer), Ok(Some(64)));
        assert_eq!(buffer, FRAME);
        assert_eq!(interface.poll_receive(&mut buffer), Ok(None));
    }

    #[test]
    fn link_down_blocks_io_without_consuming_pending_frame() {
        let mut device = TestDevice::new();
        device.link = LinkState::Down;
        device.pending = Some(&FRAME);
        let mut interface = NetworkInterface::new(device);
        let mut buffer = [0; 64];
        assert_eq!(interface.transmit(&FRAME), Err(NetworkError::LinkDown));
        assert_eq!(
            interface.poll_receive(&mut buffer),
            Err(NetworkError::LinkDown)
        );
        assert_eq!(interface.into_device().pending, Some(&FRAME[..]));
    }

    #[test]
    fn unknown_link_state_is_supported_when_hardware_has_no_status() {
        let mut device = TestDevice::new();
        device.link = LinkState::Unknown;
        let mut interface = NetworkInterface::new(device);
        assert_eq!(interface.transmit(&FRAME), Ok(()));
    }

    #[test]
    fn detects_broken_driver_returning_out_of_bounds_length() {
        let mut device = TestDevice::new();
        device.pending = Some(&FRAME);
        device.reported_length = Some(65);
        let mut interface = NetworkInterface::new(device);
        let mut buffer = [0; 64];
        assert_eq!(
            interface.poll_receive(&mut buffer),
            Err(NetworkError::InvalidDeviceFrame)
        );
    }

    #[test]
    fn detects_broken_driver_returning_incomplete_frame() {
        let mut device = TestDevice::new();
        device.pending = Some(&FRAME);
        device.reported_length = Some(12);
        let mut interface = NetworkInterface::new(device);
        let mut buffer = [0; 64];
        assert_eq!(
            interface.poll_receive(&mut buffer),
            Err(NetworkError::InvalidDeviceFrame)
        );
    }
}
