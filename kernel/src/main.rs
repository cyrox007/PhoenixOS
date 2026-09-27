#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

mod arch;
mod heap;
mod process_space;
mod qemu;
mod serial;
mod thread;
mod thread_manager;

use alloc::boxed::Box;
use alloc::vec::Vec;
use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{BootInfo, entry_point};
use core::alloc::Layout;
use phoenix_framebuffer::draw_boot_banner;
use phoenix_memory::{MemorySummary, SystemFrameAllocator};
use phoenix_vm::{ActivePageTable, InactivePageTable};
use qemu::ExitCode;
use x86_64::VirtAddr;
use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};

const SYSTEM_MEMORY_RANGE_CAPACITY: usize = 256;
const VM_TEST_ADDRESS: u64 = 0x0000_6000_0000_0000;
const VM_TEST_VALUE: u64 = 0x5048_4f45_4e49_584f;
const PROCESS_VM_TEST_ADDRESS: u64 = 0x0000_0000_4000_0000;
const PROCESS_VM_TEST_VALUE: u64 = 0x5052_4f43_5f56_4d21;
const PROCESS_CR3_TEST_VALUE: u64 = 0x4352_335f_5357_4954;
const USER_MODE_TEST_CODE_ADDRESS: u64 = 0x0000_0000_4000_0000;
const USER_MODE_TEST_STACK_ADDRESS: u64 = 0x0000_0000_4000_1000;
const HEAP_TEST_VALUE: u64 = 0x4845_4150_5f4f_4b21;
const APIC_TIMER_TEST_TICKS: u64 = 3;
const THREAD_CONTEXT_TEST_STACK_SIZE: usize = 64 * 1024;

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

    if !arch::x86_64::user_privilege_stack_ready() {
        panic!("TSS не содержит корректный стек смены привилегий");
    }
    serial::line(
        &mut out,
        "INFO",
        format_args!("user privilege stack self-test: OK"),
    );

    let syscall_msrs =
        arch::x86_64::syscall::init().expect("процессор не поддерживает x86-64 SYSCALL/SYSRET");
    if syscall_msrs.lstar == 0 || syscall_msrs.efer & 1 == 0 {
        panic!("MSR системных вызовов не прошли проверку после записи");
    }
    serial::line(&mut out, "INFO", format_args!("syscall msr self-test: OK"));

    if !arch::x86_64::syscall::user_return_selectors_valid(syscall_msrs) {
        panic!("пользовательские сегменты SYSRET не согласованы с IA32_STAR");
    }
    serial::line(
        &mut out,
        "INFO",
        format_args!("syscall user selectors self-test: OK"),
    );

    if !arch::x86_64::syscall::entry_self_test() {
        panic!("аппаратный вход SYSCALL повредил запрос или результат");
    }
    serial::line(
        &mut out,
        "INFO",
        format_args!("syscall entry self-test: OK"),
    );

    if !arch::x86_64::syscall::entry_stack_self_test() {
        panic!("SYSCALL не переключился на отдельный стек ядра");
    }
    serial::line(
        &mut out,
        "INFO",
        format_args!("syscall entry stack self-test: OK"),
    );

    if !arch::x86_64::syscall::return_context_self_test() {
        panic!("контекст возврата SYSRET не прошёл проверку");
    }
    serial::line(
        &mut out,
        "INFO",
        format_args!("syscall return context self-test: OK"),
    );

    process_capability_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("process capability self-test: OK"),
    );

    ipc_self_test();
    serial::line(&mut out, "INFO", format_args!("ipc endpoint self-test: OK"));

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

    let physical_memory_offset = VirtAddr::new(
        boot_info
            .physical_memory_offset
            .into_option()
            .expect("загрузчик не передал отображение физической памяти"),
    );

    let mut page_table = unsafe { ActivePageTable::from_current(physical_memory_offset) };

    virtual_memory_self_test(&mut page_table, &mut frames);
    serial::line(
        &mut out,
        "INFO",
        format_args!("virtual memory self-test: OK"),
    );

    inactive_page_table_self_test(physical_memory_offset, &mut frames);
    serial::line(
        &mut out,
        "INFO",
        format_args!("process page table root self-test: OK"),
    );
    serial::line(
        &mut out,
        "INFO",
        format_args!("process user mapping self-test: OK"),
    );

    process_region_mapping_self_test(physical_memory_offset, &mut frames);
    serial::line(
        &mut out,
        "INFO",
        format_args!("process region mapping self-test: OK"),
    );

    process_cr3_switch_self_test(physical_memory_offset, &mut frames);
    serial::line(
        &mut out,
        "INFO",
        format_args!("process cr3 switch self-test: OK"),
    );

    user_mode_syscall_self_test(physical_memory_offset, &mut frames);
    serial::line(
        &mut out,
        "INFO",
        format_args!("user mode syscall self-test: OK"),
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

    thread_context_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("thread context self-test: OK"),
    );

    serial::line(
        &mut out,
        "INFO",
        format_args!("kernel thread object self-test: OK"),
    );

    kernel_thread_scheduler_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("kernel thread scheduler self-test: OK"),
    );

    init_apic_timer(&mut out, &mut page_table, &mut frames);

    render_boot_banner(boot_info, &mut out);

    serial::line(&mut out, "INFO", format_args!("bootstrap: OK"));
    qemu::exit(ExitCode::Success);
}

fn process_capability_self_test() {
    use phoenix_capability::{Capability, CapabilityError, ObjectId, ObjectKind, Rights};
    use phoenix_process::{ProcessCapabilitySet, ProcessId};

    let mut source = ProcessCapabilitySet::<4>::new(ProcessId(100));
    let mut target = ProcessCapabilitySet::<4>::new(ProcessId(200));
    let rights = Rights::READ
        .union(Rights::TRANSFER)
        .union(Rights::DUPLICATE);

    let original = source
        .insert(Capability {
            object: ObjectId(0x4341_5041_4249_4c49),
            kind: ObjectKind::Endpoint,
            rights,
        })
        .expect("не удалось выдать исходную возможность процессу");

    let read_only = source
        .derive(original, Rights::READ)
        .expect("не удалось создать возможность с уменьшенными правами");

    if source.require(read_only, Rights::READ).is_err()
        || source.require(read_only, Rights::WRITE) != Err(CapabilityError::MissingRight)
    {
        panic!("производная возможность получила неверные права");
    }

    source
        .revoke(read_only)
        .expect("не удалось отозвать производную возможность");

    if source.get(read_only) != Err(CapabilityError::InvalidHandle) {
        panic!("отозванный дескриптор остался действительным");
    }

    let received = source
        .transfer_to(&mut target, original)
        .expect("не удалось передать возможность другому процессу");

    if source.get(original) != Err(CapabilityError::InvalidHandle) {
        panic!("исходный дескриптор остался действительным после передачи");
    }

    let capability = target
        .require(received, Rights::READ)
        .expect("получатель не получил переданную возможность");

    if target.owner() != ProcessId(200)
        || capability.object != ObjectId(0x4341_5041_4249_4c49)
        || capability.kind != ObjectKind::Endpoint
    {
        panic!("переданная возможность повреждена");
    }
}

fn ipc_self_test() {
    use phoenix_ipc::{Endpoint, EndpointId, IpcError, Message};
    use phoenix_process::ProcessId;

    let mut endpoint = Endpoint::<2>::new(EndpointId(0x4950_435f_5445_5354));
    let first = Message::new(ProcessId(100), &[0x11, 0x22])
        .expect("не удалось создать первое IPC-сообщение");
    let second =
        Message::new(ProcessId(200), &[0x33]).expect("не удалось создать второе IPC-сообщение");

    endpoint
        .send(first)
        .expect("не удалось отправить первое IPC-сообщение");
    endpoint
        .send(second)
        .expect("не удалось отправить второе IPC-сообщение");

    if !endpoint.is_full()
        || endpoint.receive() != Ok(first)
        || endpoint.receive() != Ok(second)
        || endpoint.receive() != Err(IpcError::QueueEmpty)
    {
        panic!("кольцевая очередь IPC нарушила порядок или состояние");
    }
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

struct ThreadContextTestState {
    main: *mut arch::x86_64::context::Context,
    worker: *mut arch::x86_64::context::Context,
    completed: bool,
}

fn thread_context_self_test() {
    let mut main = arch::x86_64::context::Context::empty();
    let mut state = ThreadContextTestState {
        main: &mut main,
        worker: core::ptr::null_mut(),
        completed: false,
    };

    let argument = &mut state as *mut ThreadContextTestState as usize;
    let mut worker = thread::KernelThread::new(
        thread::ThreadId(1),
        THREAD_CONTEXT_TEST_STACK_SIZE,
        thread_context_test_entry,
        argument,
    )
    .expect("не удалось создать тестовый поток");

    if worker.id() != thread::ThreadId(1)
        || worker.state() != thread::ThreadState::Ready
        || worker.stack_size() != THREAD_CONTEXT_TEST_STACK_SIZE
    {
        panic!("объект тестового потока создан с неверными свойствами");
    }

    state.worker = worker.context_mut();
    worker.start().expect("не удалось запустить тестовый поток");

    unsafe {
        arch::x86_64::context::switch(&mut main, worker.context());
    }

    if !state.completed {
        panic!("тестовый поток не вернул управление основному контексту");
    }

    worker
        .finish()
        .expect("не удалось завершить тестовый поток");
    if worker.state() != thread::ThreadState::Finished {
        panic!("тестовый поток не перешёл в завершённое состояние");
    }
}

extern "C" fn thread_context_test_entry(argument: usize) -> ! {
    let state = unsafe { &mut *(argument as *mut ThreadContextTestState) };
    state.completed = true;

    unsafe {
        arch::x86_64::context::switch(&mut *state.worker, &*state.main);
    }

    loop {
        core::hint::spin_loop();
    }
}

fn kernel_thread_scheduler_self_test() {
    let mut first = 0_u64;
    let mut second = 0_u64;
    let mut manager = thread_manager::ThreadManager::<4>::new();

    let first_id = manager
        .spawn(
            scheduled_thread_test_entry,
            &mut first as *mut u64 as usize,
            1,
        )
        .expect("не удалось создать первый планируемый поток");

    let second_id = manager
        .spawn(
            scheduled_thread_test_entry,
            &mut second as *mut u64 as usize,
            1,
        )
        .expect("не удалось создать второй планируемый поток");

    manager
        .set_blocked(second_id, true)
        .expect("не удалось заблокировать второй поток");

    if manager.state(second_id) != Some(thread::ThreadState::Blocked) {
        panic!("заблокированный поток имеет неверное состояние");
    }

    let first_result = manager
        .dispatch_tick()
        .expect("ошибка диспетчеризации первого потока");

    if first_result != thread_manager::DispatchResult::Completed(first_id) || first != 1 {
        panic!("первый планируемый поток завершился некорректно");
    }

    manager
        .set_blocked(second_id, false)
        .expect("не удалось пробудить второй поток");

    let second_result = manager
        .dispatch_tick()
        .expect("ошибка диспетчеризации второго потока");

    if second_result != thread_manager::DispatchResult::Completed(second_id) || second != 1 {
        panic!("второй планируемый поток завершился некорректно");
    }

    if manager.live_count() != 0 {
        panic!("завершённые потоки остались в менеджере");
    }

    if manager.dispatch_tick().expect("ошибка проверки простоя")
        != thread_manager::DispatchResult::Idle
    {
        panic!("пустой менеджер потоков не перешёл в простой");
    }
}

extern "C" fn scheduled_thread_test_entry(argument: usize) {
    let value = unsafe { &mut *(argument as *mut u64) };
    *value += 1;
}

fn init_apic_timer(
    out: &mut serial::Com1,
    page_table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<SYSTEM_MEMORY_RANGE_CAPACITY>,
) {
    let mode = match arch::x86_64::apic::init_periodic_timer(page_table, frames) {
        Ok(mode) => mode,
        Err(error) => panic!("не удалось инициализировать локальный APIC: {error:?}"),
    };

    serial::line(
        out,
        "INFO",
        format_args!("local APIC: mode={}", mode.name()),
    );

    arch::x86_64::apic::install_timer_hook(thread_manager::timer_tick_hook)
        .expect("не удалось установить обработчик планировочного тика");

    let target_tick = arch::x86_64::apic::ticks()
        .checked_add(APIC_TIMER_TEST_TICKS)
        .expect("счётчик системных тиков переполнен");

    arch::x86_64::apic::wait_for_ticks(target_tick);

    let observed_tick = thread_manager::last_timer_tick();
    let observed_context = thread_manager::last_timer_context()
        .expect("планировочный слой не получил прерываемый контекст таймера");
    arch::x86_64::apic::remove_timer_hook();

    if observed_tick < target_tick {
        panic!("планировочный слой не получил ожидаемые аппаратные тики");
    }

    if observed_context.instruction_pointer == 0 || observed_context.stack_pointer == 0 {
        panic!("таймерный IRQ передал неполный аппаратный контекст");
    }

    if observed_context.register_capture_marker != arch::x86_64::apic::TIMER_REGISTER_CAPTURE_MARKER
    {
        panic!("таймерный IRQ не прошёл через вход сохранения регистров");
    }

    if core::mem::size_of::<arch::x86_64::apic::TimerGeneralRegisters>() != 15 * 8 {
        panic!("снимок регистров таймера имеет неожиданный размер");
    }

    if observed_context.stack_frame_address == 0
        || observed_context.stack_frame_address >= observed_context.stack_pointer
    {
        panic!("таймерный IRQ не передал стековый кадр прерывания");
    }

    serial::line(
        out,
        "INFO",
        format_args!(
            "apic timer self-test: OK ticks={}",
            arch::x86_64::apic::ticks()
        ),
    );

    serial::line(
        out,
        "INFO",
        format_args!("thread preemption hook self-test: OK tick={observed_tick}"),
    );

    serial::line(
        out,
        "INFO",
        format_args!(
            "thread interrupt frame self-test: OK rip={:#x} rsp={:#x} rflags={:#x}",
            observed_context.instruction_pointer,
            observed_context.stack_pointer,
            observed_context.cpu_flags
        ),
    );

    serial::line(
        out,
        "INFO",
        format_args!("thread general register frame self-test: OK"),
    );

    serial::line(
        out,
        "INFO",
        format_args!("thread stack interrupt frame self-test: OK"),
    );

    serial::line(
        out,
        "INFO",
        format_args!("thread resumable interrupt frame self-test: OK"),
    );
}

fn inactive_page_table_self_test(
    physical_memory_offset: VirtAddr,
    frames: &mut SystemFrameAllocator<SYSTEM_MEMORY_RANGE_CAPACITY>,
) {
    let free_before = frames.free_frames();

    let mut root = unsafe { InactivePageTable::new(physical_memory_offset, frames) }
        .expect("не удалось выделить корень таблиц страниц процесса");

    if root.is_active() {
        panic!("новый корень адресного пространства неожиданно стал активным");
    }

    if !root.is_empty() {
        panic!("новый корень адресного пространства не обнулён");
    }

    if frames.free_frames() + 1 != free_before {
        panic!("создание корня адресного пространства заняло неверное число страниц");
    }

    unsafe {
        root.inherit_kernel_mappings(physical_memory_offset);
    }

    if !root.user_space_is_empty()
        || !root.shared_kernel_mappings_match_active(physical_memory_offset)
    {
        panic!("корень процесса неверно унаследовал отображения ядра");
    }

    let process_page = Page::<Size4KiB>::containing_address(VirtAddr::new(PROCESS_VM_TEST_ADDRESS));
    let process_frame = root
        .map_owned_user_4k(process_page, PageTableFlags::WRITABLE, frames)
        .expect("не удалось создать пользовательское отображение процесса");

    let translated = root
        .translate_user_addr(VirtAddr::new(PROCESS_VM_TEST_ADDRESS))
        .expect("пользовательское отображение процесса не транслируется");

    if translated != process_frame.start_address() {
        panic!("пользовательское отображение процесса указывает на неверный кадр");
    }

    let physical_ptr =
        (physical_memory_offset + process_frame.start_address().as_u64()).as_mut_ptr::<u64>();
    unsafe {
        physical_ptr.write_volatile(PROCESS_VM_TEST_VALUE);
    }

    if unsafe { physical_ptr.read_volatile() } != PROCESS_VM_TEST_VALUE {
        panic!("физическая страница процесса повредила контрольное значение");
    }

    let released = root
        .destroy_user_space(frames)
        .expect("не удалось уничтожить пользовательскую половину адресного пространства");

    if released < 4 || !root.user_space_is_empty() {
        panic!("пользовательские страницы процесса освобождены некорректно");
    }

    // Разделяемые ядерные записи не принадлежат процессу, поэтому очищаем их
    // перед возвратом самой страницы P4 распределителю.
    unsafe {
        root.clear_shared_kernel_mappings();
    }

    root.release_empty(frames)
        .expect("не удалось освободить пустой корень адресного пространства");

    if frames.free_frames() != free_before {
        panic!("корень адресного пространства не вернул физическую страницу");
    }
}

fn process_region_mapping_self_test(
    physical_memory_offset: VirtAddr,
    frames: &mut SystemFrameAllocator<SYSTEM_MEMORY_RANGE_CAPACITY>,
) {
    use phoenix_process::{AddressSpaceId, MemoryPermissions, RegionKind, VirtualRegion};

    let free_before = frames.free_frames();
    let mut space = unsafe {
        process_space::ProcessAddressSpace::<4>::new(
            AddressSpaceId(42),
            physical_memory_offset,
            frames,
        )
    }
    .expect("не удалось создать адресное пространство процесса");

    let region = VirtualRegion::new(
        PROCESS_VM_TEST_ADDRESS,
        2 * phoenix_process::PAGE_SIZE,
        MemoryPermissions::USER_READ_WRITE,
        RegionKind::Program,
    )
    .expect("не удалось описать тестовый пользовательский регион");

    space
        .map_region(region, frames)
        .expect("не удалось отобразить пользовательский регион процесса");

    if space.id() != AddressSpaceId(42) || space.region_count() != 1 {
        panic!("модель адресного пространства процесса рассинхронизирована");
    }

    for offset in [0, phoenix_process::PAGE_SIZE] {
        if space
            .translate_addr(VirtAddr::new(PROCESS_VM_TEST_ADDRESS + offset))
            .is_none()
        {
            panic!("страница зарегистрированного региона не отображена");
        }
    }

    let image_start = PROCESS_VM_TEST_ADDRESS + phoenix_process::PAGE_SIZE - 3;
    let image = [0x48, 0x31, 0xc0, 0x48, 0xff, 0xc0, 0xc3];
    space
        .write_user_bytes(VirtAddr::new(image_start), &image)
        .expect("не удалось загрузить байты в пользовательский регион");

    for (index, expected) in image.iter().copied().enumerate() {
        let address = VirtAddr::new(image_start + index as u64);
        let physical = space
            .translate_addr(address)
            .expect("байт загруженного образа потерял отображение");
        let actual = unsafe {
            (physical_memory_offset + physical.as_u64())
                .as_ptr::<u8>()
                .read_volatile()
        };

        if actual != expected {
            panic!("байты пользовательского образа записаны неверно");
        }
    }

    serial::emergency(format_args!(
        "[INFO] process user image write self-test: OK\n"
    ));

    let unmapped = space
        .unmap_region(region.start, region.length, frames)
        .expect("не удалось снять пользовательский регион процесса");

    if unmapped != region || space.region_count() != 0 {
        panic!("реестр региона не синхронизирован после снятия отображения");
    }

    space
        .destroy(frames)
        .expect("не удалось уничтожить адресное пространство процесса");

    if frames.free_frames() != free_before {
        panic!("адресное пространство процесса не вернуло всю физическую память");
    }
}

fn process_cr3_switch_self_test(
    physical_memory_offset: VirtAddr,
    frames: &mut SystemFrameAllocator<SYSTEM_MEMORY_RANGE_CAPACITY>,
) {
    use phoenix_process::{AddressSpaceId, MemoryPermissions, RegionKind, VirtualRegion};

    let kernel_code = process_cr3_switch_self_test as *const () as usize as u64;
    let kernel_stack: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, rsp",
            out(reg) kernel_stack,
            options(nomem, nostack, preserves_flags),
        );
    }

    if physical_memory_offset.as_u64() < phoenix_vm::USER_SPACE_END_EXCLUSIVE
        || kernel_code < phoenix_vm::USER_SPACE_END_EXCLUSIVE
        || kernel_stack < phoenix_vm::USER_SPACE_END_EXCLUSIVE
    {
        panic!("ядро, стек или physical-memory map пересекаются с пользовательским P4-окном");
    }

    let free_before = frames.free_frames();
    let mut space = unsafe {
        process_space::ProcessAddressSpace::<2>::new(
            AddressSpaceId(43),
            physical_memory_offset,
            frames,
        )
    }
    .expect("не удалось создать адресное пространство для CR3 self-test");

    let region = VirtualRegion::new(
        PROCESS_VM_TEST_ADDRESS,
        phoenix_process::PAGE_SIZE,
        MemoryPermissions::USER_READ_WRITE,
        RegionKind::Program,
    )
    .expect("не удалось описать регион CR3 self-test");

    space
        .map_region(region, frames)
        .expect("не удалось отобразить регион CR3 self-test");

    let physical = space
        .translate_addr(VirtAddr::new(PROCESS_VM_TEST_ADDRESS))
        .expect("регион CR3 self-test не транслируется до активации");

    let observed = x86_64::instructions::interrupts::without_interrupts(|| {
        let guard = unsafe { space.activate() };
        let ptr = VirtAddr::new(PROCESS_VM_TEST_ADDRESS).as_mut_ptr::<u64>();

        unsafe {
            ptr.write_volatile(PROCESS_CR3_TEST_VALUE);
        }
        let value = unsafe { ptr.read_volatile() };

        drop(guard);
        value
    });

    if observed != PROCESS_CR3_TEST_VALUE {
        panic!("пользовательская страница недоступна после CR3 switch");
    }

    if space.is_active() {
        panic!("guard не восстановил системный CR3");
    }

    let physical_ptr = (physical_memory_offset + physical.as_u64()).as_ptr::<u64>();
    if unsafe { physical_ptr.read_volatile() } != PROCESS_CR3_TEST_VALUE {
        panic!("данные пользовательской страницы потеряны после возврата CR3");
    }

    space
        .unmap_region(region.start, region.length, frames)
        .expect("не удалось снять регион после CR3 self-test");
    space
        .destroy(frames)
        .expect("не удалось уничтожить адресное пространство после CR3 self-test");

    if frames.free_frames() != free_before {
        panic!("CR3 self-test не вернул всю физическую память");
    }
}

fn user_mode_syscall_self_test(
    physical_memory_offset: VirtAddr,
    frames: &mut SystemFrameAllocator<SYSTEM_MEMORY_RANGE_CAPACITY>,
) {
    use phoenix_process::{AddressSpaceId, MemoryPermissions, RegionKind, VirtualRegion};

    let free_before = frames.free_frames();
    let mut space = unsafe {
        process_space::ProcessAddressSpace::<2>::new(
            AddressSpaceId(44),
            physical_memory_offset,
            frames,
        )
    }
    .expect("не удалось создать адресное пространство пользовательской самопроверки");

    let code_region = VirtualRegion::new(
        USER_MODE_TEST_CODE_ADDRESS,
        phoenix_process::PAGE_SIZE,
        MemoryPermissions::USER_READ_EXECUTE,
        RegionKind::Program,
    )
    .expect("не удалось описать пользовательский код");

    let stack_region = VirtualRegion::new(
        USER_MODE_TEST_STACK_ADDRESS,
        phoenix_process::PAGE_SIZE,
        MemoryPermissions::USER_READ_WRITE,
        RegionKind::Stack,
    )
    .expect("не удалось описать пользовательский стек");

    space
        .map_region(code_region, frames)
        .expect("не удалось отобразить пользовательский код");
    space
        .map_region(stack_region, frames)
        .expect("не удалось отобразить пользовательский стек");

    let image = arch::x86_64::syscall::user_mode_test_image();
    if image.is_empty() || image.len() > phoenix_process::PAGE_SIZE as usize {
        panic!("пользовательская самопроверка имеет недопустимый размер");
    }

    space
        .write_user_bytes(VirtAddr::new(USER_MODE_TEST_CODE_ADDRESS), image)
        .expect("не удалось загрузить пользовательскую самопроверку");

    let stack_pointer = USER_MODE_TEST_STACK_ADDRESS + phoenix_process::PAGE_SIZE - 16;

    let passed = x86_64::instructions::interrupts::without_interrupts(|| {
        let guard = unsafe { space.activate() };
        let result = arch::x86_64::syscall::run_user_mode_self_test(
            USER_MODE_TEST_CODE_ADDRESS,
            stack_pointer,
        );
        drop(guard);
        result
    });

    if !passed {
        panic!("пользовательский цикл CPL3/SYSCALL/SYSRET завершился ошибкой");
    }

    space
        .unmap_region(code_region.start, code_region.length, frames)
        .expect("не удалось снять пользовательский код");
    space
        .unmap_region(stack_region.start, stack_region.length, frames)
        .expect("не удалось снять пользовательский стек");
    space
        .destroy(frames)
        .expect("не удалось уничтожить адресное пространство пользовательской самопроверки");

    if frames.free_frames() != free_before {
        panic!("пользовательская самопроверка не вернула всю физическую память");
    }
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
