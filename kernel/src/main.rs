#![no_std]
#![no_main]

mod qemu;
mod serial;

use bootloader_api::{BootInfo, entry_point};
use phoenix_framebuffer::draw_boot_banner;
use qemu::ExitCode;

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let mut out = serial::console();

    serial::line(&mut out, "INFO", format_args!("PhoenixOS bootstrap kernel"));
    serial::line(&mut out, "INFO", format_args!("architecture: x86_64"));
    serial::line(&mut out, "INFO", format_args!("firmware target: UEFI"));

    match boot_info.framebuffer.as_mut() {
        Some(framebuffer) => match draw_boot_banner(framebuffer) {
            Ok(info) => {
                serial::line(
                    &mut out,
                    "INFO",
                    format_args!(
                        "framebuffer: {}x{} {} B/px",
                        info.width, info.height, info.bytes_per_pixel
                    ),
                );
            }
            Err(error) => {
                serial::line(
                    &mut out,
                    "WARN",
                    format_args!("framebuffer banner unavailable: {error:?}"),
                );
            }
        },
        None => {
            serial::line(&mut out, "WARN", format_args!("framebuffer: unavailable"));
        }
    }

    serial::line(&mut out, "INFO", format_args!("bootstrap: OK"));

    qemu::exit(ExitCode::Success);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial::emergency(format_args!("\n[PANIC] {info}\n"));
    qemu::exit(ExitCode::Failure);
}
