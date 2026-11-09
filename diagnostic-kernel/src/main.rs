#![no_std]
#![no_main]

// Никаких ссылок на штатные драйверы NVMe/xHCI, DMA или PhoenixFS.
// Только независимый проверочный образ: чтение ЦП/ACPI/PCI и вывод в GOP/COM1.

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{BootInfo, entry_point};
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
    // Без собственного обработчика прерываний внешние IRQ здесь запрещены.
    x86_64::instructions::interrupts::disable();
    let context = hardware::HardwareContext::from_boot_info(info);
    let mut report = screen::Report::new(info.framebuffer.as_mut());

    log!(report, "[INFO] PhoenixOS physical diagnostic");
    log!(report, "[INFO] diagnostic mode: OK readonly=1");
    log!(report, "[INFO] storage controllers: not initialized");
    log!(report, "[INFO] device DMA: disabled");
    hardware::run(&context, &mut report);
    if report.journal_complete() {
        log!(report, "[INFO] diagnostic complete: OK");
    } else {
        log!(report, "[WARN] diagnostic complete: INCOMPLETE journal-overflow");
    }
    let mut page = report.page_count().saturating_sub(1);
    report.display_page(page);

    // Не перезагружаем компьютер автоматически, чтобы отчёт оставался
    // на экране. Не запускаем драйверы, задачи или прерывания.
    // Только чтение состояния совместимого контроллера клавиатуры.
    // Если он отсутствует, последний экран остаётся видимым.
    let mut extended = false;
    loop {
        use x86_64::instructions::port::Port;
        let status = unsafe { Port::<u8>::new(0x64).read() };
        if status & 1 == 0 {
            core::hint::spin_loop();
            continue;
        }
        let code = unsafe { Port::<u8>::new(0x60).read() };
        if code == 0xe0 {
            extended = true;
            continue;
        }
        if extended && code == 0x49 {
            page = page.saturating_sub(1);
            report.display_page(page);
        }
        if extended && code == 0x51 {
            page = (page + 1).min(report.page_count().saturating_sub(1));
            report.display_page(page);
        }
        extended = false;
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    screen::emergency(format_args!("{info}"));
    loop {
        x86_64::instructions::hlt();
    }
}
