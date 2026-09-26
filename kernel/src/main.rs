#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod arch;
mod qemu;
mod serial;

use bootloader_api::{BootInfo, entry_point};
use phoenix_framebuffer::draw_boot_banner;
use phoenix_memory::{BootFrameAllocator, MemorySummary};
use qemu::ExitCode;

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let mut out = serial::console();

    serial::line(&mut out, "INFO", format_args!("PhoenixOS bootstrap kernel"));
    serial::line(&mut out, "INFO", format_args!("architecture: x86_64"));
    serial::line(&mut out, "INFO", format_args!("firmware target: UEFI"));

    arch::x86_64::init();
    serial::line(&mut out, "INFO", format_args!("interrupt tables: OK"));

    arch::x86_64::exceptions::smoke_test_breakpoint();
    serial::line(&mut out, "INFO", format_args!("breakpoint self-test: OK"));

    let memory = MemorySummary::from_regions(&boot_info.memory_regions);
    serial::line(
        &mut out,
        "INFO",
        format_args!(
            "memory: regions={} usable_regions={} total={} MiB usable={} MiB",
            memory.region_count,
            memory.usable_region_count,
            memory.total_bytes / (1024 * 1024),
            memory.usable_bytes / (1024 * 1024)
        ),
    );

    let mut frames = unsafe { BootFrameAllocator::new(&boot_info.memory_regions) };
    let first = frames.allocate_4k().expect("at least one usable frame");
    let second = frames.allocate_4k().expect("at least two usable frames");

    serial::line(
        &mut out,
        "INFO",
        format_args!(
            "frame allocator: first={:#x} second={:#x}",
            first.start_address().as_u64(),
            second.start_address().as_u64()
        ),
    );

    render_boot_banner(boot_info, &mut out);

    serial::line(&mut out, "INFO", format_args!("bootstrap: OK"));
    qemu::exit(ExitCode::Success);
}

fn render_boot_banner(boot_info: &'static mut BootInfo, out: &mut serial::Com1) {
    let Some(framebuffer) = boot_info.framebuffer.as_mut() else {
        serial::line(out, "WARN", format_args!("framebuffer: unavailable"));
        return;
    };

    let Ok(info) = draw_boot_banner(framebuffer) else {
        serial::line(out, "WARN", format_args!("framebuffer banner unavailable"));
        return;
    };

    serial::line(
        out,
        "INFO",
        format_args!(
            "framebuffer: {}x{} {} B/px",
            info.width, info.height, info.bytes_per_pixel
        ),
    );
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial::emergency(format_args!("\n[PANIC] {info}\n"));
    qemu::exit(ExitCode::Failure);
}
