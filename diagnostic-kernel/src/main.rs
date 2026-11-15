#![no_std]
#![no_main]

// Никаких ссылок на штатные драйверы NVMe/xHCI, DMA или PhoenixFS.
// Только независимый проверочный образ: чтение ЦП/ACPI/PCI и вывод в GOP/COM1.

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use phoenix_diagnostic_journal::PageCycle;
use phoenix_memory::SystemFrameAllocator;
use x86_64::VirtAddr;

macro_rules! log {
    ($report:expr, $($arg:tt)*) => {
        $report.line(format_args!($($arg)*))
    };
}

mod hardware;
mod screen;
#[path = "../../kernel/src/arch/x86_64/smp_probe.rs"]
mod smp_probe;

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
    let ap_complete = measure_secondary_processors(info, &context, &mut report);
    log!(
        report,
        "[INFO] diagnostic acceptance: point16=pending-other-address-spaces point17=blocked-device-isolation dma_authorized=false"
    );
    if ap_complete && report.journal_complete() {
        log!(report, "[INFO] diagnostic complete: OK");
    } else {
        log!(
            report,
            "[WARN] diagnostic complete: INCOMPLETE cpu-or-journal"
        );
    }
    // Просмотр запускается с первой страницы и автоматически обходит все.
    // 15 млрд тактов НЕ означают фиксированное число секунд на разных ЦП.
    // Это резервный способ снять полный отчёт на видео при отсутствии i8042.
    const PAGE_INTERVAL_TICKS: u64 = 15_000_000_000;
    let mut pages = PageCycle::new(
        report.page_count(),
        unsafe { core::arch::x86_64::_rdtsc() },
        PAGE_INTERVAL_TICKS,
    );
    report.display_page(pages.page());

    // Не перезагружаем компьютер автоматически, чтобы отчёт оставался
    // на экране. Не запускаем драйверы, задачи или прерывания.
    // Только чтение состояния совместимого контроллера клавиатуры.
    // Если клавиатура отсутствует, экран меняется по счётчику тактов ЦП.
    let mut extended = false;
    loop {
        use x86_64::instructions::port::Port;
        let now = unsafe { core::arch::x86_64::_rdtsc() };
        if pages.advance(now) {
            report.display_page(pages.page());
        }
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
            pages.previous(now);
            report.display_page(pages.page());
        }
        if extended && code == 0x51 {
            pages.next(now);
            report.display_page(pages.page());
        }
        extended = false;
    }
}

#[panic_handler]
/// Собирает снимки регистров всех ядер в этом же образе.
/// Физическая DMA-передача и управление накопителями не включены.
fn measure_secondary_processors(
    info: &BootInfo,
    context: &hardware::HardwareContext,
    report: &mut screen::Report<'_>,
) -> bool {
    let Some(topology) = hardware::processor_topology(context) else {
        log!(report, "[WARN] diagnostic kernel AP: INCOMPLETE invalid-madt");
        return false;
    };
    let Some(offset) = info.physical_memory_offset.into_option() else {
        log!(report, "[WARN] diagnostic kernel AP: INCOMPLETE missing-physical-map");
        return false;
    };
    let Ok(mut frames) = SystemFrameAllocator::<256>::from_regions(&info.memory_regions) else {
        log!(report, "[WARN] diagnostic kernel AP: INCOMPLETE bad-memory-map");
        return false;
    };
    match smp_probe::run_diagnostic(&topology, &mut frames, VirtAddr::new(offset)) {
        Ok(result) => {
            log!(
                report,
                "[INFO] diagnostic kernel AP: OK expected={} measured={} distinct={} cache={} cr3={} amd_msr={} dma_authorized=false",
                result.expected,
                result.measured,
                result.distinct,
                result.cache_agrees,
                result.cr3_agrees,
                result.amd_registers_measured
            );
            true
        }
        Err(error) => {
            log!(report, "[WARN] diagnostic kernel AP: INCOMPLETE reason={error:?}");
            false
        }
    }
}

fn panic(info: &PanicInfo<'_>) -> ! {
    screen::emergency(format_args!("{info}"));
    loop {
        x86_64::instructions::hlt();
    }
}
