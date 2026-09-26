use core::fmt::{self, Write};
use uart_16550::backend::PioBackend;
use uart_16550::{Config, Uart16550Tty};

pub type Com1 = Uart16550Tty<PioBackend>;

pub fn console() -> Com1 {
    unsafe { Uart16550Tty::new_port(0x3F8, Config::default()) }.expect("COM1 UART must initialize")
}

pub fn try_console() -> Option<Com1> {
    unsafe { Uart16550Tty::new_port(0x3F8, Config::default()) }.ok()
}

pub fn emergency(args: fmt::Arguments<'_>) {
    if let Some(mut out) = try_console() {
        let _ = out.write_fmt(args);
    }
}

pub fn line(out: &mut Com1, level: &str, args: fmt::Arguments<'_>) {
    let _ = write!(out, "[{level}] ");
    let _ = out.write_fmt(args);
    let _ = writeln!(out);
}
