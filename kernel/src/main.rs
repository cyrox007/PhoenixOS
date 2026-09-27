#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

mod arch;
mod heap;
mod qemu;
mod serial;

use alloc::boxed::Box;
use alloc::vec::Vec;
use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{BootInfo, entry_point};
use core::alloc::Layout;
use phoenix_framebuffer::draw_boot_banner;
use phoenix_memory::{MemorySummary, SystemFrameAllocator};
use phoenix_vm::ActivePageTable;
use qemu::ExitCode;
use x86_64::VirtAddr;
use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};

const SYSTEM_MEMORY_RANGE_CAPACITY: usize = 256;
const VM_TEST_ADDRESS: u64 = 0x0000_6000_0000_0000;
const VM_TEST_VALUE: u64 = 0x5048_4f45_4e49_584f;
const HEAP_TEST_VALUE: u64 = 0x4845_4150_5f4f_4b21;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let mut out = serial::console();

    log_boot_start(&mut out);

    arch::x86_64::init();
    serial::line(&mut out, "INFO", format_args!("interrupt tables: OK"));

    arch::x86_64::exceptions::smoke_test_breakpoint();
    serial::line(&mut out, "INFO", format_args!("breakpoint self-test: OK"));

    let memory = MemorySummary::from_regions(&boot_info.memory_regions);
    log_memory_summary(&mut out, memory);

    let mut frames = SystemFrameAllocator::<SYSTEM_MEMORY_RANGE_CAPACITY>::from_regions(
        &boot_info.memory_regions,
    )
    .expect("не удалось создать системный распределитель физической памяти");

    serial::line(
        &mut out,
        "INFO",
        format_args!("physical memory: free_frames={}", frames.free_frames()),
    );

    let physical_memory_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("загрузчик не передал отображение физической памяти");

    let mut page_table =
        unsafe { ActivePageTable::from_current(VirtAddr::new(physical_memory_offset)) };

    virtual_memory_self_test(&mut page_table, &mut frames);
    serial::line(
        &mut out,
        "INFO",
        format_args!("virtual memory self-test: OK"),
    );

    let heap_stats =
        heap::init(&mut page_table, &mut frames).expect("не удалось инициализировать кучу ядра");

    serial::line(
        &mut out,
        "INFO",
        format_args!(
            "kernel heap: size={} KiB free={} KiB",
            heap_stats.size / 1024,
            heap_stats.free / 1024
        ),
    );

    heap_self_test();
    serial::line(&mut out, "INFO", format_args!("kernel heap self-test: OK"));

    render_boot_banner(boot_info, &mut out);

    serial::line(&mut out, "INFO", format_args!("bootstrap: OK"));
    qemu::exit(ExitCode::Success);
}

fn log_boot_start(out: &mut serial::Com1) {
    serial::line(out, "INFO", format_args!("PhoenixOS bootstrap kernel"));
    serial::line(out, "INFO", format_args!("architecture: x86_64"));
    serial::line(out, "INFO", format_args!("firmware target: UEFI"));
}

fn log_memory_summary(out: &mut serial::Com1, memory: MemorySummary) {
    serial::line(
        out,
        "INFO",
        format_args!(
            "memory: regions={} usable_regions={} total={} MiB usable={} MiB",
            memory.region_count,
            memory.usable_region_count,
            memory.total_bytes / (1024 * 1024),
            memory.usable_bytes / (1024 * 1024)
        ),
    );
}

fn virtual_memory_self_test(
    page_table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<SYSTEM_MEMORY_RANGE_CAPACITY>,
) {
    let address = VirtAddr::new(VM_TEST_ADDRESS);
    let page = Page::<Size4KiB>::containing_address(address);

    if page_table.translate_addr(address).is_some() {
        panic!("виртуальный адрес самопроверки уже занят");
    }

    let frame = frames
        .allocate_4k()
        .expect("нет физической страницы для проверки виртуальной памяти");

    unsafe {
        page_table
            .map_4k(page, frame, PageTableFlags::WRITABLE, frames)
            .expect("не удалось отобразить тестовую виртуальную страницу");
    }

    let ptr = address.as_mut_ptr::<u64>();

    unsafe {
        ptr.write_volatile(VM_TEST_VALUE);
    }

    let value = unsafe { ptr.read_volatile() };
    if value != VM_TEST_VALUE {
        panic!("контрольное значение виртуальной памяти повреждено");
    }

    let translated = page_table
        .translate_addr(address)
        .expect("отображённый адрес не преобразуется в физический");

    if translated != frame.start_address() {
        panic!("виртуальный адрес преобразован в неверную физическую страницу");
    }

    let unmapped = page_table
        .unmap_4k(page)
        .expect("не удалось снять тестовое отображение");

    if unmapped != frame {
        panic!("при снятии отображения возвращена неверная физическая страница");
    }

    frames
        .release_4k(frame)
        .expect("не удалось вернуть тестовую физическую страницу");
}

fn heap_self_test() {
    let before = heap::stats();

    let boxed = Box::new(HEAP_TEST_VALUE);
    if *boxed != HEAP_TEST_VALUE {
        panic!("куча вернула повреждённое значение Box");
    }

    let mut values = Vec::with_capacity(128);
    for value in 0_u64..128 {
        values.push(value);
    }

    let expected_sum = (0_u64..128).sum::<u64>();
    if values.iter().copied().sum::<u64>() != expected_sum {
        panic!("куча повредила содержимое Vec");
    }

    let during = heap::stats();
    if during.used <= before.used {
        panic!("самопроверка кучи не зафиксировала выделение памяти");
    }

    drop(values);
    drop(boxed);

    let after = heap::stats();
    if after.used != before.used {
        panic!("куча не вернула память после освобождения тестовых объектов");
    }
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

#[alloc_error_handler]
fn allocation_error(layout: Layout) -> ! {
    serial::emergency(format_args!(
        "\n[OOM] не удалось выделить {} байт с выравниванием {}\n",
        layout.size(),
        layout.align()
    ));
    qemu::exit(ExitCode::Failure);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial::emergency(format_args!("\n[PANIC] {info}\n"));
    qemu::exit(ExitCode::Failure);
}
