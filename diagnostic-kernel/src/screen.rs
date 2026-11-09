//! Вывод диагностического отчёта без файловой системы.
//! Разрешены только запись на экран GOP и ранний последовательный порт.

use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use core::fmt::{self, Write};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster};
use phoenix_diagnostic_journal::Journal;
use uart_16550::backend::PioBackend;
use uart_16550::{Config, Uart16550Tty};

type Com1 = Uart16550Tty<PioBackend>;

pub struct Report<'a> {
    serial: Option<Com1>,
    screen: Option<Screen<'a>>,
    journal: Journal<32768>,
}

impl<'a> Report<'a> {
    pub fn new(framebuffer: Option<&'a mut FrameBuffer>) -> Self {
        let screen = framebuffer.and_then(|frame| {
            let _ = phoenix_framebuffer::draw_boot_banner(frame);
            Screen::new(frame)
        });
        let serial = unsafe { Uart16550Tty::new_port(0x3f8, Config::default()) }.ok();
        Self {
            serial,
            screen,
            journal: Journal::new(),
        }
    }

    pub fn line(&mut self, args: fmt::Arguments<'_>) {
        self.journal.push_line(args);
        if let Some(serial) = self.serial.as_mut() {
            let _ = serial.write_fmt(args);
            let _ = serial.write_str("\n");
        }
        if let Some(screen) = self.screen.as_mut() {
            let _ = screen.write_fmt(args);
            let _ = screen.write_str("\n");
        }
    }
    pub fn journal_complete(&self) -> bool {
        self.journal.is_complete()
    }

    pub fn page_count(&self) -> usize {
        let Some(screen) = self.screen.as_ref() else {
            return 0;
        };
        let rows = screen.rows_per_page();
        self.journal.line_count().div_ceil(rows).max(1)
    }

    /// Просмотр отдельных страниц на экране без обращения к накопителям.
    pub fn display_page(&mut self, page: usize) {
        let Some(screen) = self.screen.as_mut() else {
            return;
        };
        let rows = screen.rows_per_page();
        let pages = self.journal.line_count().div_ceil(rows).max(1);
        let page = page.min(pages - 1);
        screen.clear_text();
        for index in page * rows..(page * rows + rows).min(self.journal.line_count()) {
            let Some(line) = self.journal.line(index) else {
                continue;
            };
            screen.write_clipped_line(line);
        }
        screen.y = screen.info.height.saturating_sub(28);
        screen.x = 20;
        let _ = write!(
            screen,
            "PhoenixOS journal: page {}/{} | complete={} | PgUp/PgDn",
            page + 1,
            pages,
            self.journal.is_complete()
        );
    }
}

struct Screen<'a> {
    buffer: &'a mut [u8],
    info: FrameBufferInfo,
    x: usize,
    y: usize,
}

impl<'a> Screen<'a> {
    fn new(fb: &'a mut FrameBuffer) -> Option<Self> {
        let info = fb.info();
        if info.width < 120
            || info.height < 200
            || info.stride < info.width
            || info.bytes_per_pixel < 3
            || info.byte_len > fb.buffer().len()
            || !matches!(info.pixel_format, PixelFormat::Rgb | PixelFormat::Bgr)
        {
            return None;
        }
        Some(Self {
            buffer: fb.buffer_mut(),
            info,
            x: 20,
            y: 140,
        })
    }

    fn rows_per_page(&self) -> usize {
        (self.info.height.saturating_sub(200) / 20).max(1)
    }

    fn clear_text(&mut self) {
        let start = 140usize
            .saturating_mul(self.info.stride)
            .saturating_mul(self.info.bytes_per_pixel);
        let end = self.info.byte_len.min(self.buffer.len());
        if start < end {
            self.buffer[start..end].fill(0);
        }
        self.x = 20;
        self.y = 140;
    }

    fn write_clipped_line(&mut self, line: &str) {
        let columns = self.info.width.saturating_sub(40) / 10;
        for ch in line.chars().take(columns) {
            self.character(ch);
        }
        self.next_line();
    }

    fn pixel(&mut self, x: usize, y: usize) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let Some(offset) = y
            .checked_mul(self.info.stride)
            .and_then(|v| v.checked_add(x))
            .and_then(|v| v.checked_mul(self.info.bytes_per_pixel))
        else {
            return;
        };
        let Some(pixel) = self.buffer.get_mut(offset..offset + 3) else {
            return;
        };
        // Нейтральный светлый текст для обеих схем пикселей.
        pixel.copy_from_slice(&[235, 242, 249]);
    }

    fn character(&mut self, ch: char) {
        if self.y + 20 >= self.info.height {
            return;
        }
        let raster = get_raster(ch, FontWeight::Regular, RasterHeight::Size16)
            .or_else(|| get_raster('?', FontWeight::Regular, RasterHeight::Size16));
        let Some(raster) = raster else {
            return;
        };
        if self.x + raster.width() + 1 >= self.info.width {
            self.next_line();
        }
        if self.y + 20 >= self.info.height {
            return;
        }
        for (ry, row) in raster.raster().iter().enumerate() {
            for (rx, intensity) in row.iter().copied().enumerate() {
                if intensity >= 128 {
                    self.pixel(self.x + rx, self.y + ry);
                }
            }
        }
        self.x += raster.width() + 1;
    }

    fn next_line(&mut self) {
        self.x = 20;
        self.y += 20;
    }
}

impl Write for Screen<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            if c == '\n' {
                self.next_line();
                continue;
            }
            self.character(c);
        }
        Ok(())
    }
}

pub fn emergency(args: fmt::Arguments<'_>) {
    use core::fmt::Write;
    if let Ok(mut console) =
        unsafe { Uart16550Tty::<PioBackend>::new_port(0x3f8, Config::default()) }
    {
        let _ = console.write_str("[PANIC] ");
        let _ = console.write_fmt(args);
        let _ = console.write_str("\n");
    }
}
