//! Двухмерные команды VirtIO-GPU версии 1 (без аппаратного ускорения).
//! Формат пакетов соответствует стандарту VirtIO и заголовку virtio_gpu.h.

pub const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const CMD_SET_SCANOUT: u32 = 0x0103;
pub const CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
pub const RESP_OK_NODATA: u32 = 0x1100;
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const FORMAT_X8R8G8B8: u32 = 4;
pub const CONTROL_HEADER_LEN: usize = 24;
pub const RESOURCE_ID: u32 = 1;

/// Представление байтов для эмулируемого формата X8R8G8B8:
/// первый байт не несёт цвета, затем следуют R, G и B.
pub const fn pixel_x8r8g8b8(red: u8, green: u8, blue: u8) -> u32 {
    (blue as u32) << 24 | (green as u32) << 16 | (red as u32) << 8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuProtocolError {
    InvalidDimensions,
    InvalidBacking,
    InvalidResponse,
    InvalidDisplayInfo,
    DeviceRejected(u32),
}

#[derive(Clone, Copy)]
pub struct GpuRequest {
    data: [u8; 64],
    length: usize,
}

impl GpuRequest {
    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.length]
    }

    fn new(command: u32, length: usize) -> Self {
        let mut request = Self {
            data: [0; 64],
            length,
        };
        put_u32(&mut request.data, 0, command);
        request
    }

    pub fn display_info() -> Self {
        Self::new(CMD_GET_DISPLAY_INFO, CONTROL_HEADER_LEN)
    }

    pub fn create_2d(width: u32, height: u32) -> Result<Self, GpuProtocolError> {
        if width == 0 || height == 0 || width.checked_mul(height).is_none() {
            return Err(GpuProtocolError::InvalidDimensions);
        }
        let mut request = Self::new(CMD_RESOURCE_CREATE_2D, 40);
        put_u32(&mut request.data, 24, RESOURCE_ID);
        put_u32(&mut request.data, 28, FORMAT_X8R8G8B8);
        put_u32(&mut request.data, 32, width);
        put_u32(&mut request.data, 36, height);
        Ok(request)
    }

    pub fn attach_backing(physical: u64, bytes: u32) -> Result<Self, GpuProtocolError> {
        if physical & 4095 != 0 || bytes == 0 || bytes & 4095 != 0 {
            return Err(GpuProtocolError::InvalidBacking);
        }
        physical
            .checked_add(u64::from(bytes) - 1)
            .ok_or(GpuProtocolError::InvalidBacking)?;
        let mut request = Self::new(CMD_RESOURCE_ATTACH_BACKING, 48);
        put_u32(&mut request.data, 24, RESOURCE_ID);
        put_u32(&mut request.data, 28, 1);
        put_u64(&mut request.data, 32, physical);
        put_u32(&mut request.data, 40, bytes);
        Ok(request)
    }

    pub fn scanout(width: u32, height: u32, enabled: bool) -> Self {
        let mut request = Self::new(CMD_SET_SCANOUT, 48);
        put_u32(&mut request.data, 32, width);
        put_u32(&mut request.data, 36, height);
        put_u32(&mut request.data, 44, if enabled { RESOURCE_ID } else { 0 });
        request
    }

    pub fn transfer(width: u32, height: u32) -> Self {
        let mut request = Self::new(CMD_TRANSFER_TO_HOST_2D, 56);
        put_u32(&mut request.data, 32, width);
        put_u32(&mut request.data, 36, height);
        put_u32(&mut request.data, 48, RESOURCE_ID);
        request
    }

    pub fn flush(width: u32, height: u32) -> Self {
        let mut request = Self::new(CMD_RESOURCE_FLUSH, 48);
        put_u32(&mut request.data, 32, width);
        put_u32(&mut request.data, 36, height);
        put_u32(&mut request.data, 40, RESOURCE_ID);
        request
    }

    pub fn detach_backing() -> Self {
        let mut request = Self::new(CMD_RESOURCE_DETACH_BACKING, 32);
        put_u32(&mut request.data, 24, RESOURCE_ID);
        request
    }

    pub fn unref() -> Self {
        let mut request = Self::new(CMD_RESOURCE_UNREF, 32);
        put_u32(&mut request.data, 24, RESOURCE_ID);
        request
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
}

pub fn check_response(
    response: &[u8],
    used_length: usize,
    expected: u32,
) -> Result<(), GpuProtocolError> {
    if used_length < CONTROL_HEADER_LEN || response.len() < CONTROL_HEADER_LEN {
        return Err(GpuProtocolError::InvalidResponse);
    }
    let command = get_u32(response, 0);
    if command >= 0x1200 {
        return Err(GpuProtocolError::DeviceRejected(command));
    }
    if command != expected {
        return Err(GpuProtocolError::InvalidResponse);
    }
    Ok(())
}

pub fn first_display_mode(response: &[u8]) -> Result<DisplayMode, GpuProtocolError> {
    if response.len() < 48 || get_u32(response, 0) != RESP_OK_DISPLAY_INFO {
        return Err(GpuProtocolError::InvalidDisplayInfo);
    }
    let width = get_u32(response, 32);
    let height = get_u32(response, 36);
    let enabled = get_u32(response, 40);
    if width == 0 || height == 0 || enabled != 1 {
        return Err(GpuProtocolError::InvalidDisplayInfo);
    }
    Ok(DisplayMode { width, height })
}

fn put_u32(buffer: &mut [u8], start: usize, value: u32) {
    buffer[start..start + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buffer: &mut [u8], start: usize, value: u64) {
    buffer[start..start + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(buffer: &[u8], start: usize) -> u32 {
    u32::from_le_bytes(buffer[start..start + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_has_expected_byte_order_for_qemu() {
        assert_eq!(pixel_x8r8g8b8(24, 31, 46).to_le_bytes(), [0, 24, 31, 46]);
        assert_eq!(
            pixel_x8r8g8b8(255, 255, 255).to_le_bytes(),
            [0, 255, 255, 255]
        );
        assert_eq!(pixel_x8r8g8b8(229, 94, 54).to_le_bytes(), [0, 229, 94, 54]);
    }

    #[test]
    fn command_layout_matches_virtio_gpu_protocol() {
        let create = GpuRequest::create_2d(128, 128).unwrap();
        assert_eq!(create.bytes().len(), 40);
        assert_eq!(get_u32(create.bytes(), 0), CMD_RESOURCE_CREATE_2D);
        assert_eq!(get_u32(create.bytes(), 24), RESOURCE_ID);
        assert_eq!(get_u32(create.bytes(), 28), FORMAT_X8R8G8B8);
        assert_eq!(get_u32(create.bytes(), 32), 128);
        let backing = GpuRequest::attach_backing(0x1000, 65536).unwrap();
        assert_eq!(backing.bytes().len(), 48);
        assert_eq!(get_u32(backing.bytes(), 24), RESOURCE_ID);
        assert_eq!(get_u32(backing.bytes(), 28), 1);
        assert_eq!(get_u32(backing.bytes(), 40), 65536);
        assert_eq!(
            get_u32(GpuRequest::scanout(128, 128, true).bytes(), 44),
            RESOURCE_ID
        );
        assert_eq!(get_u32(GpuRequest::scanout(128, 128, false).bytes(), 44), 0);
        assert_eq!(
            get_u32(GpuRequest::transfer(128, 128).bytes(), 48),
            RESOURCE_ID
        );
        assert_eq!(
            get_u32(GpuRequest::flush(128, 128).bytes(), 40),
            RESOURCE_ID
        );
    }

    #[test]
    fn rejects_invalid_backing_and_resource_geometry() {
        assert!(GpuRequest::create_2d(0, 128).is_err());
        assert!(GpuRequest::create_2d(u32::MAX, 2).is_err());
        assert!(GpuRequest::attach_backing(3, 4096).is_err());
        assert!(GpuRequest::attach_backing(4096, 0).is_err());
        assert!(GpuRequest::attach_backing(u64::MAX & !4095, 8192).is_err());
    }

    #[test]
    fn response_validation_is_strict() {
        let mut response = [0_u8; 48];
        put_u32(&mut response, 0, RESP_OK_NODATA);
        assert_eq!(check_response(&response, 24, RESP_OK_NODATA), Ok(()));
        assert_eq!(
            check_response(&response, 20, RESP_OK_NODATA),
            Err(GpuProtocolError::InvalidResponse)
        );
        put_u32(&mut response, 0, 0x1205);
        assert_eq!(
            check_response(&response, 24, RESP_OK_NODATA),
            Err(GpuProtocolError::DeviceRejected(0x1205))
        );
        put_u32(&mut response, 0, RESP_OK_DISPLAY_INFO);
        put_u32(&mut response, 32, 1024);
        put_u32(&mut response, 36, 768);
        put_u32(&mut response, 40, 1);
        assert_eq!(
            first_display_mode(&response),
            Ok(DisplayMode {
                width: 1024,
                height: 768,
            })
        );
        put_u32(&mut response, 40, 0);
        assert_eq!(
            first_display_mode(&response),
            Err(GpuProtocolError::InvalidDisplayInfo)
        );
    }
}
