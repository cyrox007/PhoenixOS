#![no_std]
#![no_main]

// Никаких ссылок на штатные драйверы NVMe/xHCI, DMA или PhoenixFS.
// Только независимый проверочный образ: чтение ЦП/ACPI/PCI и вывод в GOP/COM1.

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;

macro_rules! log {
    ($report:expr, $($arg:tt)*) => {
        $report.line(format_args!($($arg)*))
    };
}

mod hardware;
mod screen;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.kernel_stack_size = 128 * 1024;
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(info: &'static mut BootInfo) -> ! {
    let context = hardware::HardwareContext::from_boot_info(info);
    let mut report = screen::Report::new(info.framebuffer.as_mut());

    log!(report, "[INFO] PhoenixOS physical diagnostic");
    log!(report, "[INFO] diagnostic mode: OK readonly=1");
    log!(report, "[INFO] storage controllers: not initialized");
    log!(report, "[INFO] device DMA: disabled");
    hardware::run(&context, &mut report);
    log!(report, "[INFO] diagnostic complete: OK");

    // Не перезагружаем компьютер автоматически, чтобы отчёт оставался
    // на экране. Не запускаем драйверы, задачи или прерывания.
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    screen::emergency(format_args!("{info}"));
    loop {
        core::hint::spin_loop();
    }
}
