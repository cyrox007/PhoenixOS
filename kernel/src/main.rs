#![no_std]
#![no_main]

mod qemu;
mod serial;

use bootloader_api::{BootInfo, entry_point};
use qemu::ExitCode;

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let mut out = serial::console();

    serial::line(&mut out, "INFO", format_args!("PhoenixOS bootstrap kernel"));
    serial::line(&mut out, "INFO", format_args!("architecture: x86_64"));
    serial::line(&mut out, "INFO", format_args!("firmware target: UEFI"));
    serial::line(
        &mut out,
        "INFO",
        format_args!("memory regions: {}", boot_info.memory_regions.len()),
    );
    serial::line(&mut out, "INFO", format_args!("bootstrap: OK"));

    qemu::exit(ExitCode::Success);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial::emergency(format_args!("\n[PANIC] {info}\n"));
    qemu::exit(ExitCode::Failure);
}
