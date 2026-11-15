#![no_std]
#![no_main]

// Никаких ссылок на штатные драйверы NVMe/xHCI, DMA или PhoenixFS.
// Только независимый проверочный образ: чтение ЦП/ACPI/PCI и вывод в GOP/COM1.

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use phoenix_diagnostic_journal::PageCycle;

macro_rules! log {
    ($report:expr, $($arg:tt)*) => {
        $report.line(format_args!($($arg)*))
    };
}

mod hardware;
mod screen;
#[cfg(feature = "asus-acceptance")]
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
    #[cfg(feature = "asus-acceptance")]
    run_processor_acceptance(&info.memory_regions, &context, &mut report);
    #[cfg(not(feature = "asus-acceptance"))]
    log!(
        report,
        "[INFO] diagnostic acceptance 16/17: NOT_RUN passive=true"
    );
    // Настоящая передача DMA намеренно не запускается, пока отсутствует
    // выбранное и подтверждённое физическое устройство ASUS. Наличие
    // рабочей реализации AMD IOMMU в QEMU не является доказательством.
    log!(
        report,
        "[INFO] diagnostic acceptance 17/17: BLOCKED reason=no-verified-physical-controller-and-iommu-domain dma_authorized=false"
    );
    if report.journal_complete() {
        log!(report, "[INFO] diagnostic complete: OK");
    } else {
        log!(
            report,
            "[WARN] diagnostic complete: INCOMPLETE journal-overflow"
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

#[cfg(feature = "asus-acceptance")]
/// Собирает измерения всех процессоров в ТОЙ ЖЕ загрузке,
/// где выполнены проверки ACPI, RAM, PAT/MTRR и PCI.
/// На неподтверждённой платформе любое активное действие запрещено.
fn run_processor_acceptance(
    regions: &[bootloader_api::info::MemoryRegion],
    context: &hardware::HardwareContext,
    report: &mut screen::Report<'_>,
) {
    if !hardware::safe_for_smp(context) {
        log!(
            report,
            "[WARN] diagnostic AP snapshot: BLOCKED reason=invalid-memory-map"
        );
        return;
    }
    let Some(topology) = hardware::processor_topology(context) else {
        log!(
            report,
            "[WARN] diagnostic AP snapshot: BLOCKED reason=missing-madt"
        );
        return;
    };
    let Some(offset) = hardware::physical_offset(context) else {
        log!(
            report,
            "[WARN] diagnostic AP snapshot: BLOCKED reason=missing-direct-map"
        );
        return;
    };
    let Ok(mut frames) =
        phoenix_memory::SystemFrameAllocator::<256>::from_regions(regions)
    else {
        log!(
            report,
            "[WARN] diagnostic AP snapshot: BLOCKED reason=allocator"
        );
        return;
    };
    match smp_probe::run(&topology, &mut frames, offset) {
        Ok(result) => {
            smp_probe::report_snapshots(report, &topology);
            log!(
                report,
                "[INFO] diagnostic AP snapshot: OK measured={} expected={} distinct={} cache={} cr3={} dma_authorized=false",
                result.measured,
                result.expected,
                result.distinct,
                result.cache_agrees,
                result.cr3_agrees
            );
            log!(
                report,
                "[INFO] diagnostic acceptance 16/17: PARTIAL ap_measured=true active_cr3_checked=true inactive_cr3_unverified=true"
            );
        }
        Err(reason) => {
            smp_probe::report_snapshots(report, &topology);
            log!(
                report,
                "[WARN] diagnostic AP snapshot: BLOCKED reason={reason:?} dma_authorized=false"
            );
            log!(
                report,
                "[INFO] diagnostic acceptance 16/17: BLOCKED ap_measured=false"
            );
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    screen::emergency(format_args!("{info}"));
    loop {
        x86_64::instructions::hlt();
    }
}
