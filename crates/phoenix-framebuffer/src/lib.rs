#![no_std]

use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    InvalidFramebufferLayout,
    UnsupportedPixelFormat,
    UnsupportedGlyph(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerInfo {
    pub width: usize,
    pub height: usize,
    pub bytes_per_pixel: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
}

impl Color {
    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

const BACKGROUND: Color = Color::rgb(0, 0, 0);
const PANEL: Color = Color::rgb(24, 31, 46);
const ACCENT: Color = Color::rgb(229, 94, 54);
const PRIMARY_TEXT: Color = Color::rgb(245, 247, 250);
const SECONDARY_TEXT: Color = Color::rgb(168, 179, 196);

pub fn draw_boot_banner(framebuffer: &mut FrameBuffer) -> Result<BannerInfo, RenderError> {
    let info = framebuffer.info();
    validate_info(info, framebuffer.buffer().len())?;

    let buffer = framebuffer.buffer_mut();
    let mut canvas = Canvas { buffer, info };

    canvas.clear(BACKGROUND)?;

    let panel_height = core::cmp::min(info.height, 112);
    canvas.fill_rect(0, 0, info.width, panel_height, PANEL)?;

    let accent_width = core::cmp::min(info.width, 8);
    canvas.fill_rect(0, 0, accent_width, panel_height, ACCENT)?;

    if info.width >= 160 && info.height >= 64 {
        canvas.draw_text(24, 18, "PhoenixOS", PRIMARY_TEXT, PANEL, 2)?;
        canvas.draw_text(
            24,
            62,
            "x86_64 / UEFI bootstrap",
            SECONDARY_TEXT,
            PANEL,
            1,
        )?;
    }

    Ok(BannerInfo {
        width: info.width,
        height: info.height,
        bytes_per_pixel: info.bytes_per_pixel,
    })
}

fn validate_info(info: FrameBufferInfo, buffer_len: usize) -> Result<(), RenderError> {
    if info.width == 0
        || info.height == 0
        || info.stride < info.width
        || info.bytes_per_pixel == 0
        || info.byte_len > buffer_len
    {
        return Err(RenderError::InvalidFramebufferLayout);
    }

    match info.pixel_format {
        PixelFormat::Rgb | PixelFormat::Bgr if info.bytes_per_pixel >= 3 => Ok(()),
        PixelFormat::U8 if info.bytes_per_pixel >= 1 => Ok(()),
        PixelFormat::Unknown { .. } => Err(RenderError::UnsupportedPixelFormat),
        _ => Err(RenderError::InvalidFramebufferLayout),
    }
}

struct Canvas<'a> {
    buffer: &'a mut [u8],
    info: FrameBufferInfo,
}

impl Canvas<'_> {
    fn clear(&mut self, color: Color) -> Result<(), RenderError> {
        if color == Color::rgb(0, 0, 0) {
            self.buffer.fill(0);
            return Ok(());
        }

        self.fill_rect(0, 0, self.info.width, self.info.height, color)
    }

    fn fill_rect(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: Color,
    ) -> Result<(), RenderError> {
        let max_x = core::cmp::min(x.saturating_add(width), self.info.width);
        let max_y = core::cmp::min(y.saturating_add(height), self.info.height);

        for py in y..max_y {
            for px in x..max_x {
                self.write_pixel(px, py, color)?;
            }
        }

        Ok(())
    }

    fn draw_text(
        &mut self,
        mut x: usize,
        y: usize,
        text: &str,
        foreground: Color,
        background: Color,
        scale: usize,
    ) -> Result<(), RenderError> {
        let scale = core::cmp::max(scale, 1);

        for ch in text.chars() {
            let raster = get_raster(ch, FontWeight::Regular, RasterHeight::Size16)
                .ok_or(RenderError::UnsupportedGlyph(ch))?;

            for (row_index, row) in raster.raster().iter().enumerate() {
                for (column_index, intensity) in row.iter().enumerate() {
                    if *intensity == 0 {
                        continue;
                    }

                    let color = blend(background, foreground, *intensity);
                    let px = x.saturating_add(column_index.saturating_mul(scale));
                    let py = y.saturating_add(row_index.saturating_mul(scale));
                    self.fill_rect(px, py, scale, scale, color)?;
                }
            }

            x = x.saturating_add((raster.width() + 1).saturating_mul(scale));
            if x >= self.info.width {
                break;
            }
        }

        Ok(())
    }

    fn write_pixel(&mut self, x: usize, y: usize, color: Color) -> Result<(), RenderError> {
        if x >= self.info.width || y >= self.info.height {
            return Ok(());
        }

        let pixel_index = y
            .checked_mul(self.info.stride)
            .and_then(|value| value.checked_add(x))
            .ok_or(RenderError::InvalidFramebufferLayout)?;

        let byte_offset = pixel_index
            .checked_mul(self.info.bytes_per_pixel)
            .ok_or(RenderError::InvalidFramebufferLayout)?;

        let end = byte_offset
            .checked_add(self.info.bytes_per_pixel)
            .ok_or(RenderError::InvalidFramebufferLayout)?;

        if end > self.buffer.len() {
            return Err(RenderError::InvalidFramebufferLayout);
        }

        match self.info.pixel_format {
            PixelFormat::Rgb => {
                self.write_component(byte_offset, color.r);
                self.write_component(byte_offset + 1, color.g);
                self.write_component(byte_offset + 2, color.b);
                self.zero_extra_components(byte_offset, 3);
            }
            PixelFormat::Bgr => {
                self.write_component(byte_offset, color.b);
                self.write_component(byte_offset + 1, color.g);
                self.write_component(byte_offset + 2, color.r);
                self.zero_extra_components(byte_offset, 3);
            }
            PixelFormat::U8 => {
                let luminance =
                    ((u16::from(color.r) * 54 + u16::from(color.g) * 183 + u16::from(color.b) * 19)
                        / 256) as u8;
                self.write_component(byte_offset, luminance);
                self.zero_extra_components(byte_offset, 1);
            }
            PixelFormat::Unknown { .. } => return Err(RenderError::UnsupportedPixelFormat),
            _ => return Err(RenderError::UnsupportedPixelFormat),
        }

        Ok(())
    }

    fn zero_extra_components(&mut self, byte_offset: usize, used: usize) {
        for offset in used..self.info.bytes_per_pixel {
            self.write_component(byte_offset + offset, 0);
        }
    }

    fn write_component(&mut self, offset: usize, value: u8) {
        unsafe {
            core::ptr::write_volatile(self.buffer.as_mut_ptr().add(offset), value);
        }
    }
}

fn blend(background: Color, foreground: Color, alpha: u8) -> Color {
    let alpha = u16::from(alpha);
    let inverse = 255 - alpha;

    let channel = |background: u8, foreground: u8| -> u8 {
        ((u16::from(background) * inverse + u16::from(foreground) * alpha) / 255) as u8
    };

    Color::rgb(
        channel(background.r, foreground.r),
        channel(background.g, foreground.g),
        channel(background.b, foreground.b),
    )
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;

    fn info(pixel_format: PixelFormat, bytes_per_pixel: usize) -> FrameBufferInfo {
        FrameBufferInfo {
            byte_len: 8 * 4 * bytes_per_pixel,
            width: 4,
            height: 4,
            pixel_format,
            bytes_per_pixel,
            stride: 8,
        }
    }

    #[test]
    fn rgb_pixel_respects_stride() {
        let info = info(PixelFormat::Rgb, 4);
        let mut buffer = [0u8; 128];
        let mut canvas = Canvas {
            buffer: &mut buffer,
            info,
        };

        canvas.write_pixel(2, 1, Color::rgb(10, 20, 30)).unwrap();

        let offset = (1 * info.stride + 2) * info.bytes_per_pixel;
        assert_eq!(&buffer[offset..offset + 4], &[10, 20, 30, 0]);
    }

    #[test]
    fn bgr_pixel_reorders_channels() {
        let info = info(PixelFormat::Bgr, 3);
        let mut buffer = [0u8; 96];
        let mut canvas = Canvas {
            buffer: &mut buffer,
            info,
        };

        canvas.write_pixel(0, 0, Color::rgb(10, 20, 30)).unwrap();

        assert_eq!(&buffer[..3], &[30, 20, 10]);
    }

    #[test]
    fn blending_preserves_endpoints() {
        let background = Color::rgb(10, 20, 30);
        let foreground = Color::rgb(210, 220, 230);

        assert_eq!(blend(background, foreground, 0), background);
        assert_eq!(blend(background, foreground, 255), foreground);
    }

    #[test]
    fn rejects_unknown_pixel_format() {
        let info = info(
            PixelFormat::Unknown {
                red_position: 0,
                green_position: 8,
                blue_position: 16,
            },
            4,
        );

        assert_eq!(
            validate_info(info, info.byte_len),
            Err(RenderError::UnsupportedPixelFormat)
        );
    }
}
