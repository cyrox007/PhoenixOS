//! Общий безопасный интерфейс передачи двухмерных кадров компоновщика.
//!
//! Никаких PCIe, DMA, виртуальных адресов и зависимости от видеодрайвера.
//! Формат пикселя явно задаётся в виде; драйвер решает, как обновить экран.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceError {
    EmptyDimensions,
    InvalidStride,
    BufferTooSmall,
    Overflow,
    UnsupportedFormat,
    OutsideScreen,
    DriverUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceFormat {
    /// Упаковка little-endian: [X, R, G, B] — VirtIO-GPU X8R8G8B8.
    X8R8G8B8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceSize {
    pub width: u32,
    pub height: u32,
}

impl SurfaceSize {
    pub fn pixels(self) -> Result<usize, SurfaceError> {
        if self.width == 0 || self.height == 0 {
            return Err(SurfaceError::EmptyDimensions);
        }
        let count = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .ok_or(SurfaceError::Overflow)?;
        usize::try_from(count).map_err(|_| SurfaceError::Overflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfacePosition {
    pub x: u32,
    pub y: u32,
}

/// Заимствованный кадр без передачи владения исходной памятью драйверу.
pub struct SurfaceView<'a> {
    size: SurfaceSize,
    stride: usize,
    format: SurfaceFormat,
    pixels: &'a [u32],
}

impl<'a> SurfaceView<'a> {
    pub fn new(
        pixels: &'a [u32],
        size: SurfaceSize,
        stride: usize,
        format: SurfaceFormat,
    ) -> Result<Self, SurfaceError> {
        size.pixels()?;
        let width = usize::try_from(size.width).map_err(|_| SurfaceError::Overflow)?;
        let height = usize::try_from(size.height).map_err(|_| SurfaceError::Overflow)?;
        if stride < width {
            return Err(SurfaceError::InvalidStride);
        }
        let required = (height - 1)
            .checked_mul(stride)
            .and_then(|n| n.checked_add(width))
            .ok_or(SurfaceError::Overflow)?;
        if required > pixels.len() {
            return Err(SurfaceError::BufferTooSmall);
        }
        Ok(Self {
            size,
            stride,
            format,
            pixels,
        })
    }

    pub const fn size(&self) -> SurfaceSize {
        self.size
    }

    pub const fn format(&self) -> SurfaceFormat {
        self.format
    }

    pub fn pixel(&self, x: usize, y: usize) -> u32 {
        // Границы предварительно проверены в new и в драйвере.
        self.pixels[y * self.stride + x]
    }

    pub fn fits(&self, position: SurfacePosition, screen: SurfaceSize) -> Result<(), SurfaceError> {
        let right = position
            .x
            .checked_add(self.size.width)
            .ok_or(SurfaceError::Overflow)?;
        let bottom = position
            .y
            .checked_add(self.size.height)
            .ok_or(SurfaceError::Overflow)?;
        if right > screen.width || bottom > screen.height {
            return Err(SurfaceError::OutsideScreen);
        }
        Ok(())
    }
}

/// Компоновщик работает только с этим интерфейсом, не с DMA/BAR.
pub trait SurfaceTarget {
    type Error;

    fn screen_size(&self) -> SurfaceSize;

    /// Драйвер копирует переданные пиксели до возврата метода.
    /// Нельзя удерживать ссылку на кадр после завершения вызова.
    fn present(
        &mut self,
        view: &SurfaceView<'_>,
        position: SurfacePosition,
    ) -> Result<(), Self::Error>;
}

/// Простейший независимый от устройства источник кадра.
pub const fn x8r8g8b8(red: u8, green: u8, blue: u8) -> u32 {
    (blue as u32) << 24 | (green as u32) << 16 | (red as u32) << 8
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockDisplay {
        width: u32,
        height: u32,
        presented: usize,
        last: u32,
    }

    impl SurfaceTarget for MockDisplay {
        type Error = SurfaceError;

        fn screen_size(&self) -> SurfaceSize {
            SurfaceSize {
                width: self.width,
                height: self.height,
            }
        }

        fn present(
            &mut self,
            view: &SurfaceView<'_>,
            position: SurfacePosition,
        ) -> Result<(), Self::Error> {
            view.fits(position, self.screen_size())?;
            self.presented += 1;
            self.last = view.pixel(0, 0);
            Ok(())
        }
    }

    #[test]
    fn validates_stride_and_buffer_before_access() {
        let pixels = [0_u32; 16];
        let size = SurfaceSize {
            width: 4,
            height: 4,
        };
        assert!(SurfaceView::new(&pixels, size, 4, SurfaceFormat::X8R8G8B8).is_ok());
        assert!(matches!(
            SurfaceView::new(&pixels, size, 3, SurfaceFormat::X8R8G8B8),
            Err(SurfaceError::InvalidStride)
        ));
        assert!(matches!(
            SurfaceView::new(&pixels, size, 5, SurfaceFormat::X8R8G8B8),
            Err(SurfaceError::BufferTooSmall)
        ));
        assert!(matches!(
            SurfaceView::new(
                &pixels,
                SurfaceSize {
                    width: 0,
                    height: 4
                },
                4,
                SurfaceFormat::X8R8G8B8
            ),
            Err(SurfaceError::EmptyDimensions)
        ));
    }

    #[test]
    fn checks_destination_and_overflow() {
        let pixels = [0_u32; 16];
        let view = SurfaceView::new(
            &pixels,
            SurfaceSize {
                width: 4,
                height: 4,
            },
            4,
            SurfaceFormat::X8R8G8B8,
        )
        .unwrap();
        let screen = SurfaceSize {
            width: 128,
            height: 128,
        };
        assert_eq!(
            view.fits(SurfacePosition { x: 124, y: 124 }, screen),
            Ok(())
        );
        assert_eq!(
            view.fits(SurfacePosition { x: 125, y: 124 }, screen),
            Err(SurfaceError::OutsideScreen)
        );
        assert_eq!(
            view.fits(SurfacePosition { x: u32::MAX, y: 0 }, screen),
            Err(SurfaceError::Overflow)
        );
    }

    #[test]
    fn generic_compositor_can_submit_to_mock_driver() {
        let pixels = [x8r8g8b8(58, 108, 242); 16];
        let view = SurfaceView::new(
            &pixels,
            SurfaceSize {
                width: 4,
                height: 4,
            },
            4,
            SurfaceFormat::X8R8G8B8,
        )
        .unwrap();
        let mut display = MockDisplay {
            width: 128,
            height: 128,
            presented: 0,
            last: 0,
        };
        display
            .present(&view, SurfacePosition { x: 80, y: 8 })
            .unwrap();
        assert_eq!(display.presented, 1);
        assert_eq!(display.last.to_le_bytes(), [0, 58, 108, 242]);
        assert_eq!(display.screen_size().width, 128);
    }
}
