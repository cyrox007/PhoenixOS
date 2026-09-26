#![no_std]
#![no_main]

use bootloader_api::{entry_point, BootInfo};
use core::fmt::Write;
use uart_16550::backend::PioBackend;
use uart_16550::{Config, Uart16550Tty};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum QemuExitCode {
    Success = 0x10,
    Failure = 0x11,
}

fn serial() -> Uart16550Tty<PioBackend> {
    unsafe { Uart16550Tty::new_port(0x3F8, Config::default()) }
        .expect("COM1 UART must initialize")
}

fn exit_qemu(code: QemuExitCode) -> ! {
    use x86_64::instructions::{nop, port::Port};

    unsafe {
        let mut port = Port::new(0xF4);
        port.write(code as u32);
    }

    loop {
        nop();
    }
}

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let mut out = serial();

    writeln!(out, "PhoenixOS bootstrap kernel").ok();
    writeln!(out, "architecture: x86_64").ok();
    writeln!(out, "firmware target: UEFI").ok();
    writeln!(out, "boot_info: {boot_info:?}").ok();
    writeln!(out, "bootstrap: OK").ok();

    exit_qemu(QemuExitCode::Success);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let _ = writeln!(serial(), "PANIC: {info}");
    exit_qemu(QemuExitCode::Failure);
}
