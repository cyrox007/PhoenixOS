#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

mod arch;
mod elf_user_loader;
mod file_user_memory;
mod heap;
mod ipc_user_memory;
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
const PREEMPTION_TEST_STACK_SIZE: usize = 16 * 1024;
static PREEMPTION_TEST_ENTERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static HARDWARE_PREEMPTION_FIRST_SEEN: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static HARDWARE_PREEMPTION_SECOND_SEEN: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static HARDWARE_PREEMPTION_THIRD_SEEN: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

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

    if !arch::x86_64::exceptions::prepared_kernel_timer_frame_self_test() {
        panic!("заранее подготовленный кадр таймера не совпадает с ассемблерным входом");
    }
    serial::line(
        &mut out,
        "INFO",
        format_args!("thread prepared interrupt frame self-test: OK"),
    );

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

    if !arch::x86_64::syscall::process_exit_dispatch_self_test() {
        panic!("системный вызов завершения процесса не прошёл проверку диспетчера");
    }

    if !arch::x86_64::syscall::ipc_dispatch_self_test() {
        panic!("IPC-вызовы не прошли проверку диспетчера");
    }
    if !arch::x86_64::syscall::file_dispatch_self_test() {
        panic!("файловые системные вызовы не прошли проверку диспетчера");
    }
    serial::line(
        &mut out,
        "INFO",
        format_args!("file syscall dispatch self-test: OK"),
    );
    serial::line(
        &mut out,
        "INFO",
        format_args!("ipc syscall dispatch self-test: OK"),
    );

    process_capability_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("process capability self-test: OK"),
    );

    ipc_self_test();
    serial::line(&mut out, "INFO", format_args!("ipc endpoint self-test: OK"));
    serial::line(
        &mut out,
        "INFO",
        format_args!("ipc endpoint registry self-test: OK"),
    );

    system_service_lifecycle_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("system service lifecycle self-test: OK"),
    );

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

    elf_process_loader_self_test(physical_memory_offset, &mut frames);
    serial::line(
        &mut out,
        "INFO",
        format_args!("elf process load self-test: OK"),
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
    serial::line(
        &mut out,
        "INFO",
        format_args!("user process exit self-test: OK"),
    );
    serial::line(
        &mut out,
        "INFO",
        format_args!("user process start arguments self-test: OK"),
    );

    elf_user_execution_self_test(physical_memory_offset, &mut frames);
    serial::line(
        &mut out,
        "INFO",
        format_args!("elf user execution self-test: OK"),
    );
    serial::line(
        &mut out,
        "INFO",
        format_args!("system service elf execution self-test: OK"),
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

    kernel_thread_interrupt_frame_self_test();

    serial::line(
        &mut out,
        "INFO",
        format_args!("kernel thread object self-test: OK"),
    );
    serial::line(
        &mut out,
        "INFO",
        format_args!("kernel thread interrupt frame self-test: OK"),
    );

    kernel_thread_scheduler_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("kernel thread scheduler self-test: OK"),
    );

    kernel_thread_frame_routing_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("kernel thread frame routing self-test: OK"),
    );

    preemptive_thread_start_frame_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("kernel thread initial irq frame self-test: OK"),
    );

    kernel_thread_preemptive_manager_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("kernel thread preemptive manager self-test: OK"),
    );

    kernel_thread_preemptive_spawn_self_test();
    serial::line(
        &mut out,
        "INFO",
        format_args!("kernel thread preemptive spawn self-test: OK"),
    );

    init_apic_timer(&mut out);

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
    use phoenix_ipc::{
        Endpoint, EndpointId, EndpointRegistry, EndpointRegistryError, IpcError, Message,
    };
    use phoenix_process::ProcessId;

    let endpoint_id = EndpointId(0x4950_435f_5445_5354);
    let mut registry = EndpointRegistry::<2, 2>::new();
    registry
        .insert(Endpoint::new(endpoint_id))
        .expect("не удалось зарегистрировать IPC endpoint");
    if registry.insert(Endpoint::new(endpoint_id)) != Err(EndpointRegistryError::DuplicateId)
        || !matches!(
            registry.get(EndpointId(endpoint_id.0 + 1)),
            Err(EndpointRegistryError::NotFound)
        )
    {
        panic!("реестр IPC endpoint не проверяет идентификаторы");
    }

    let endpoint = registry
        .get_mut(endpoint_id)
        .expect("зарегистрированный IPC endpoint не найден");
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

fn kernel_thread_interrupt_frame_self_test() {
    let mut thread = thread::KernelThread::new(
        thread::ThreadId(0x1f),
        THREAD_CONTEXT_TEST_STACK_SIZE,
        thread_context_test_entry,
        0,
    )
    .expect("не удалось создать поток для проверки IRQ-кадра");

    if thread.interrupt_frame().is_some() {
        panic!("новый поток неожиданно содержит IRQ-кадр");
    }
    if thread.save_interrupt_frame(0).is_ok() || thread.save_interrupt_frame(3).is_ok() {
        panic!("поток принял некорректный адрес IRQ-кадра");
    }

    const FRAME: u64 = 0x8000;
    thread
        .save_interrupt_frame(FRAME)
        .expect("не удалось сохранить IRQ-кадр потока");
    if thread.interrupt_frame() != Some(FRAME) || thread.take_interrupt_frame() != Some(FRAME) {
        panic!("IRQ-кадр потока не прошёл полный жизненный цикл");
    }
    if thread.interrupt_frame().is_some() {
        panic!("изъятый IRQ-кадр остался привязан к потоку");
    }
}

fn kernel_thread_preemptive_spawn_self_test() {
    let mut manager = thread_manager::ThreadManager::<2>::new();
    let id = manager
        .spawn_preemptive(preemptive_spawn_test_entry, 0x42, 1)
        .expect("не удалось создать preemptive-поток через ThreadManager");

    let frame = manager
        .interrupt_frame(id)
        .expect("ThreadManager не сохранил стартовый IRQ-кадр");
    if manager.state(id) != Some(thread::ThreadState::Ready) {
        panic!("новый preemptive-поток имеет неверное состояние");
    }

    if manager
        .schedule_interrupt_frame(0)
        .expect("ошибка первого выбора preemptive-потока")
        != Some(frame)
    {
        panic!("round-robin не выбрал стартовый кадр preemptive-потока");
    }

    if manager.interrupt_frame(id).is_some() {
        panic!("выбранный стартовый кадр не был изъят у потока");
    }
}

extern "C" fn preemptive_spawn_test_entry(_argument: usize) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn preemptive_thread_start_frame_self_test() {
    const ARGUMENT: usize = 0x1234;
    const EXIT_ARGUMENT: usize = 0x5678;

    let thread = thread::KernelThread::new_preemptive_returning(
        thread::ThreadId(0x20),
        THREAD_CONTEXT_TEST_STACK_SIZE,
        scheduled_thread_test_entry,
        ARGUMENT,
        preemptive_start_test_exit,
        EXIT_ARGUMENT,
    )
    .expect("не удалось создать preemptive kernel thread");

    let frame = thread
        .interrupt_frame()
        .expect("preemptive kernel thread не получил стартовый IRQ-кадр");
    if !thread.interrupt_frame_is_on_stack() {
        panic!("стартовый IRQ-кадр находится вне стека потока");
    }

    let registers = arch::x86_64::exceptions::prepared_kernel_timer_frame_registers(frame);
    let (instruction_pointer, _, _) =
        arch::x86_64::exceptions::prepared_kernel_timer_frame_words(frame);

    if instruction_pointer != arch::x86_64::context::returning_thread_trampoline_address()
        || registers.r12 != scheduled_thread_test_entry as usize as u64
        || registers.r13 != ARGUMENT as u64
        || registers.r14 != preemptive_start_test_exit as usize as u64
        || registers.r15 != EXIT_ARGUMENT as u64
    {
        panic!("стартовый IRQ-кадр не содержит trampoline-контекст потока");
    }
}

extern "C" fn preemptive_start_test_exit(_argument: usize) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn kernel_thread_preemptive_manager_self_test() {
    const FIRST_ARGUMENT: usize = 0x1111;
    const SECOND_ARGUMENT: usize = 0x2222;
    const FIRST_SAVED: u64 = 0x9000;
    const SECOND_SAVED: u64 = 0xa000;

    let mut manager = thread_manager::ThreadManager::<4>::new();
    let first = manager
        .spawn_preemptive(preemptive_spawn_test_entry, FIRST_ARGUMENT, 1)
        .expect("не удалось создать первый preemptive-поток менеджера");
    let second = manager
        .spawn_preemptive(preemptive_spawn_test_entry, SECOND_ARGUMENT, 1)
        .expect("не удалось создать второй preemptive-поток менеджера");

    let first_frame = manager
        .interrupt_frame(first)
        .expect("первый preemptive-поток не получил стартовый IRQ-кадр");
    let second_frame = manager
        .interrupt_frame(second)
        .expect("второй preemptive-поток не получил стартовый IRQ-кадр");

    if first_frame == second_frame {
        panic!("preemptive-потоки получили один и тот же IRQ-кадр");
    }

    if manager
        .schedule_interrupt_frame(0)
        .expect("не удалось выбрать первый preemptive IRQ-кадр")
        != Some(first_frame)
        || manager.state(first) != Some(thread::ThreadState::Running)
    {
        panic!("первый preemptive-поток не стартовал через менеджер");
    }

    if manager
        .schedule_interrupt_frame(FIRST_SAVED)
        .expect("не удалось выбрать второй preemptive IRQ-кадр")
        != Some(second_frame)
        || manager.interrupt_frame(first) != Some(FIRST_SAVED)
        || manager.state(second) != Some(thread::ThreadState::Running)
    {
        panic!("менеджер не переключил preemptive IRQ-кадр на второй поток");
    }

    if manager
        .schedule_interrupt_frame(SECOND_SAVED)
        .expect("не удалось вернуть первый preemptive IRQ-кадр")
        != Some(FIRST_SAVED)
        || manager.interrupt_frame(second) != Some(SECOND_SAVED)
    {
        panic!("менеджер не сохранил round-robin состояние preemptive-потоков");
    }
}

fn kernel_thread_frame_routing_self_test() {
    let mut first_value = 0_u64;
    let mut second_value = 0_u64;
    let mut manager = thread_manager::ThreadManager::<4>::new();

    let first = manager
        .spawn(
            scheduled_thread_test_entry,
            &mut first_value as *mut u64 as usize,
            1,
        )
        .expect("не удалось создать первый поток проверки IRQ-маршрутизации");
    let second = manager
        .spawn(
            scheduled_thread_test_entry,
            &mut second_value as *mut u64 as usize,
            1,
        )
        .expect("не удалось создать второй поток проверки IRQ-маршрутизации");

    const FIRST_INITIAL: u64 = 0x1000;
    const SECOND_INITIAL: u64 = 0x2000;
    const FIRST_SAVED: u64 = 0x1100;
    const SECOND_SAVED: u64 = 0x2200;

    manager
        .save_interrupt_frame(first, FIRST_INITIAL)
        .expect("не удалось установить первый IRQ-кадр");
    manager
        .save_interrupt_frame(second, SECOND_INITIAL)
        .expect("не удалось установить второй IRQ-кадр");

    if manager
        .schedule_interrupt_frame(0)
        .expect("ошибка первого выбора IRQ-кадра")
        != Some(FIRST_INITIAL)
    {
        panic!("планировщик не выбрал первый подготовленный IRQ-кадр");
    }

    if manager
        .schedule_interrupt_frame(FIRST_SAVED)
        .expect("ошибка переключения на второй IRQ-кадр")
        != Some(SECOND_INITIAL)
        || manager.interrupt_frame(first) != Some(FIRST_SAVED)
    {
        panic!("IRQ-кадр первого потока не сохранён перед переключением");
    }

    if manager
        .schedule_interrupt_frame(SECOND_SAVED)
        .expect("ошибка возврата к первому IRQ-кадру")
        != Some(FIRST_SAVED)
        || manager.interrupt_frame(second) != Some(SECOND_SAVED)
    {
        panic!("round-robin не выполнил ротацию IRQ-кадров 1-2-1");
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

extern "C" fn preemption_test_entry() -> ! {
    PREEMPTION_TEST_ENTERED.store(true, core::sync::atomic::Ordering::Release);
    x86_64::instructions::interrupts::enable();
    loop {
        core::hint::spin_loop();
    }
}

extern "C" fn hardware_preemption_returning_entry(argument: usize) {
    let seen = unsafe { &*(argument as *const core::sync::atomic::AtomicBool) };
    seen.store(true, core::sync::atomic::Ordering::Release);
}

extern "C" fn hardware_preemption_thread_entry(argument: usize) -> ! {
    let seen = unsafe { &*(argument as *const core::sync::atomic::AtomicBool) };
    seen.store(true, core::sync::atomic::Ordering::Release);
    x86_64::instructions::interrupts::enable();

    loop {
        core::hint::spin_loop();
    }
}

fn hardware_scheduler_preemption_self_test() {
    HARDWARE_PREEMPTION_FIRST_SEEN.store(false, core::sync::atomic::Ordering::Relaxed);
    HARDWARE_PREEMPTION_SECOND_SEEN.store(false, core::sync::atomic::Ordering::Relaxed);
    HARDWARE_PREEMPTION_THIRD_SEEN.store(false, core::sync::atomic::Ordering::Relaxed);

    let mut manager =
        thread_manager::ThreadManager::<{ thread_manager::KERNEL_THREAD_CAPACITY }>::new();

    let returning = manager
        .spawn_preemptive_returning(
            hardware_preemption_returning_entry,
            &HARDWARE_PREEMPTION_FIRST_SEEN as *const _ as usize,
            1,
        )
        .expect("не удалось создать возвращающийся аппаратно вытесняемый поток");
    manager
        .spawn_preemptive(
            hardware_preemption_thread_entry,
            &HARDWARE_PREEMPTION_SECOND_SEEN as *const _ as usize,
            1,
        )
        .expect("не удалось создать второй аппаратно вытесняемый поток");
    manager
        .spawn_preemptive(
            hardware_preemption_thread_entry,
            &HARDWARE_PREEMPTION_THIRD_SEEN as *const _ as usize,
            1,
        )
        .expect("не удалось создать третий аппаратно вытесняемый поток");

    let installed = unsafe { thread_manager::install_preemptive_manager(&mut manager) };
    if !installed {
        panic!("постоянный контроллер вытеснения уже занят");
    }
    if !thread_manager::arm_hardware_preemption_test() {
        thread_manager::remove_preemptive_manager();
        panic!("не удалось включить аппаратную проверку контроллера вытеснения");
    }

    while !thread_manager::hardware_preemption_test_completed() {
        core::hint::spin_loop();
    }

    thread_manager::disarm_hardware_preemption_test();
    thread_manager::remove_preemptive_manager();

    if thread_manager::hardware_preemption_test_failed()
        || thread_manager::preemptive_scheduler_failed()
        || thread_manager::hardware_preemption_test_switches() < 4
        || !HARDWARE_PREEMPTION_FIRST_SEEN.load(core::sync::atomic::Ordering::Acquire)
        || !HARDWARE_PREEMPTION_SECOND_SEEN.load(core::sync::atomic::Ordering::Acquire)
        || !HARDWARE_PREEMPTION_THIRD_SEEN.load(core::sync::atomic::Ordering::Acquire)
        || manager.state(returning).is_some()
        || manager.live_count() != 2
    {
        panic!("аппаратный round-robin потоков завершился некорректно");
    }
}

fn init_apic_timer(out: &mut serial::Com1) {
    if let Err(error) = arch::x86_64::apic::init_periodic_timer() {
        panic!("x2APIC обязателен для поддерживаемого аппаратного профиля: {error:?}");
    }

    serial::line(out, "INFO", format_args!("local APIC: mode=x2APIC"));

    let mut preemption_stack = alloc::vec![0u8; PREEMPTION_TEST_STACK_SIZE].into_boxed_slice();
    let preemption_frame = arch::x86_64::exceptions::prepare_kernel_timer_frame(
        &mut preemption_stack,
        preemption_test_entry as *const () as u64,
    )
    .expect("не удалось подготовить кадр вытесняемого потока");
    let (prepared_rip, prepared_cs, prepared_rflags) =
        arch::x86_64::exceptions::prepared_kernel_timer_frame_words(preemption_frame);
    serial::line(
        out,
        "INFO",
        format_args!(
            "prepared timer frame: address={preemption_frame:#x} rip={prepared_rip:#x} cs={prepared_cs:#x} rflags={prepared_rflags:#x}"
        ),
    );
    thread_manager::arm_preemption_frame_test(preemption_frame);

    arch::x86_64::apic::install_timer_hook(thread_manager::timer_tick_hook)
        .expect("не удалось установить обработчик планировочного тика");

    let target_tick = arch::x86_64::apic::ticks()
        .checked_add(APIC_TIMER_TEST_TICKS)
        .expect("счётчик системных тиков переполнен");

    arch::x86_64::apic::wait_for_ticks(target_tick);

    let observed_tick = thread_manager::last_timer_tick();
    let observed_context = thread_manager::last_timer_context()
        .expect("планировочный слой не получил прерываемый контекст таймера");
    thread_manager::disarm_preemption_frame_test();

    if !PREEMPTION_TEST_ENTERED.load(core::sync::atomic::Ordering::Acquire)
        || !thread_manager::preemption_frame_test_completed()
    {
        panic!("таймер не выполнил iretq-переход на отдельный стек и обратно");
    }

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

    hardware_scheduler_preemption_self_test();
    arch::x86_64::apic::remove_timer_hook();

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

    serial::line(
        out,
        "INFO",
        format_args!("thread iretq stack switch self-test: OK"),
    );

    serial::line(
        out,
        "INFO",
        format_args!(
            "kernel thread hardware round robin self-test: OK switches={}",
            thread_manager::hardware_preemption_test_switches()
        ),
    );

    serial::line(
        out,
        "INFO",
        format_args!("kernel thread preemptive return self-test: OK"),
    );

    serial::line(
        out,
        "INFO",
        format_args!("kernel thread preemption controller self-test: OK"),
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

    let mut copied_image = [0_u8; 7];
    space
        .read_user_bytes(VirtAddr::new(image_start), &mut copied_image)
        .expect("не удалось скопировать байты из пользовательского региона");
    if copied_image != image {
        panic!("копирование из пользовательского региона повредило данные");
    }
    serial::emergency(format_args!("[INFO] process user copy-in self-test: OK\n"));

    let ipc_words_address = PROCESS_VM_TEST_ADDRESS + phoenix_process::PAGE_SIZE - 8;
    let send_words = [0x1111_2222_3333_4444_u64, 0xaaaa_bbbb_cccc_dddd_u64];
    for (index, word) in send_words.iter().enumerate() {
        space
            .write_user_bytes(
                VirtAddr::new(ipc_words_address + (index as u64) * 8),
                &word.to_le_bytes(),
            )
            .expect("не удалось подготовить IPC-слова в пользовательской памяти");
    }

    let copied = ipc_user_memory::copy_send_words(
        &mut space,
        phoenix_syscall_abi::IpcSendArguments {
            endpoint: phoenix_syscall_abi::PackedCapabilityHandle::new(1, 1),
            words_address: ipc_words_address,
            word_count: send_words.len() as u64,
            transferred_capability: None,
            flags: 0,
        },
    )
    .expect("checked IPC copy-in отклонил отображённый буфер");

    let file_path_address = PROCESS_VM_TEST_ADDRESS + phoenix_process::PAGE_SIZE - 5;
    let file_path = b"/tmp/phoenix.log";
    space
        .write_user_bytes(VirtAddr::new(file_path_address), file_path)
        .expect("не удалось подготовить путь файлового системного вызова");
    let mut copied_path = [0_u8; 64];
    let copied_path = file_user_memory::copy_path(
        &mut space,
        file_path_address,
        file_path.len() as u64,
        &mut copied_path,
    )
    .expect("checked file copy-in отклонил отображённый путь");
    if copied_path.as_bytes() != file_path {
        panic!("checked file copy-in повредил путь");
    }

    let file_output_address = PROCESS_VM_TEST_ADDRESS + 0x280;
    let file_output = b"phoenix-file-buffer";
    file_user_memory::copy_to_user(
        &mut space,
        file_output_address,
        file_output.len() as u64,
        file_output,
    )
    .expect("checked file copy-out отклонил отображённый буфер");
    let mut copied_file_output = [0_u8; 19];
    space
        .read_user_bytes(VirtAddr::new(file_output_address), &mut copied_file_output)
        .expect("не удалось проверить файловый copy-out");
    if copied_file_output != *file_output {
        panic!("checked file copy-out повредил данные");
    }
    serial::emergency(format_args!(
        "[INFO] file syscall user memory self-test: OK\n"
    ));

    if copied.as_slice() != send_words {
        panic!("checked IPC copy-in повредил слова сообщения");
    }

    let receive_words = [0x0123_4567_89ab_cdef_u64, 0xfedc_ba98_7654_3210_u64];
    ipc_user_memory::copy_receive_words(
        &mut space,
        phoenix_syscall_abi::IpcReceiveArguments {
            endpoint: phoenix_syscall_abi::PackedCapabilityHandle::new(1, 1),
            words_address: ipc_words_address,
            word_capacity: 2,
            metadata_address: PROCESS_VM_TEST_ADDRESS + 0x100,
            flags: 0,
        },
        &receive_words,
    )
    .expect("checked IPC copy-out отклонил отображённый буфер");

    let mut copied_back = [0_u8; 16];
    space
        .read_user_bytes(VirtAddr::new(ipc_words_address), &mut copied_back)
        .expect("не удалось проверить IPC copy-out");
    let first = u64::from_le_bytes(copied_back[..8].try_into().unwrap());
    let second = u64::from_le_bytes(copied_back[8..].try_into().unwrap());
    if [first, second] != receive_words {
        panic!("checked IPC copy-out повредил слова сообщения");
    }
    serial::emergency(format_args!("[INFO] ipc user word copy self-test: OK\n"));

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

fn elf_process_loader_self_test(
    physical_memory_offset: VirtAddr,
    frames: &mut SystemFrameAllocator<SYSTEM_MEMORY_RANGE_CAPACITY>,
) {
    use phoenix_process::AddressSpaceId;

    const IMAGE_SIZE: usize = 4096;
    const LOAD_ADDRESS: u64 = 0x0000_0000_4200_0000;
    const ENTRY_OFFSET: u64 = 0x80;
    const FILE_SIZE: u64 = 0x100;
    const MEMORY_SIZE: u64 = 0x1000;

    let free_before = frames.free_frames();
    let mut space = unsafe {
        process_space::ProcessAddressSpace::<4>::new(
            AddressSpaceId(46),
            physical_memory_offset,
            frames,
        )
    }
    .expect("не удалось создать адресное пространство ELF self-test");

    let mut image = [0_u8; IMAGE_SIZE];
    image[0..4].copy_from_slice(b"\x7fELF");
    image[4] = 2;
    image[5] = 1;
    image[6] = 1;
    image[16..18].copy_from_slice(&2_u16.to_le_bytes());
    image[18..20].copy_from_slice(&62_u16.to_le_bytes());
    image[20..24].copy_from_slice(&1_u32.to_le_bytes());
    image[24..32].copy_from_slice(&(LOAD_ADDRESS + ENTRY_OFFSET).to_le_bytes());
    image[32..40].copy_from_slice(&64_u64.to_le_bytes());
    image[52..54].copy_from_slice(&64_u16.to_le_bytes());
    image[54..56].copy_from_slice(&56_u16.to_le_bytes());
    image[56..58].copy_from_slice(&1_u16.to_le_bytes());

    let ph = 64_usize;
    image[ph..ph + 4].copy_from_slice(&1_u32.to_le_bytes());
    image[ph + 4..ph + 8].copy_from_slice(&5_u32.to_le_bytes());
    image[ph + 8..ph + 16].copy_from_slice(&0_u64.to_le_bytes());
    image[ph + 16..ph + 24].copy_from_slice(&LOAD_ADDRESS.to_le_bytes());
    image[ph + 32..ph + 40].copy_from_slice(&FILE_SIZE.to_le_bytes());
    image[ph + 40..ph + 48].copy_from_slice(&MEMORY_SIZE.to_le_bytes());
    image[ph + 48..ph + 56].copy_from_slice(&0x1000_u64.to_le_bytes());
    image[ENTRY_OFFSET as usize] = 0xc3;

    let loaded = elf_user_loader::load_elf(&image, &mut space, frames)
        .expect("не удалось загрузить тестовый ELF64 в адресное пространство");
    if loaded.entry_point != LOAD_ADDRESS + ENTRY_OFFSET
        || loaded.segment_count != 1
        || space.region_count() != 1
    {
        panic!("ELF-загрузчик вернул неверный план пользовательского образа");
    }

    let mut magic = [0_u8; 4];
    space
        .read_user_bytes(VirtAddr::new(LOAD_ADDRESS), &mut magic)
        .expect("не удалось прочитать загруженный ELF-заголовок");
    if magic != *b"\x7fELF" {
        panic!("ELF-загрузчик повредил файловую часть PT_LOAD");
    }

    let mut entry_byte = [0_u8; 1];
    space
        .read_user_bytes(VirtAddr::new(loaded.entry_point), &mut entry_byte)
        .expect("не удалось прочитать точку входа ELF");
    if entry_byte != [0xc3] {
        panic!("точка входа ELF не совпадает с загруженными данными");
    }

    let mut bss = [0xff_u8; 32];
    space
        .read_user_bytes(VirtAddr::new(LOAD_ADDRESS + FILE_SIZE), &mut bss)
        .expect("не удалось прочитать нулевую часть PT_LOAD");
    if bss.iter().any(|byte| *byte != 0) {
        panic!("ELF-загрузчик не обнулил диапазон memsz - filesz");
    }

    space
        .destroy(frames)
        .expect("не удалось уничтожить адресное пространство ELF self-test");
    if frames.free_frames() != free_before {
        panic!("ELF self-test не вернул физические страницы распределителю");
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
    use phoenix_capability::{Capability, ObjectId, ObjectKind, Rights};
    use phoenix_process::{
        MemoryPermissions, ProcessCapabilitySet, ProcessId, ProcessState, RegionKind, VirtualRegion,
    };
    use phoenix_process_manager::{ProcessManager, ProcessScheduleDecision};

    let free_before = frames.free_frames();
    let mut process_manager = ProcessManager::<1>::new();
    let process = process_manager
        .create_runnable(1)
        .expect("не удалось создать готовый пользовательский процесс");
    if process_manager.on_tick()
        != (ProcessScheduleDecision::Switch {
            from: None,
            to: process.id,
        })
    {
        panic!("планировщик не выбрал пользовательский процесс");
    }
    let mut space = unsafe {
        process_space::ProcessAddressSpace::<{ process_space::PROCESS_REGION_CAPACITY }>::new(
            process.address_space,
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
    let start_info_address = write_process_start_info(&mut space, USER_MODE_TEST_STACK_ADDRESS);

    let ipc_words_address = USER_MODE_TEST_STACK_ADDRESS;
    let send_words = [0x1122_3344_5566_7788_u64, 0x8877_6655_4433_2211_u64];
    for (index, word) in send_words.iter().enumerate() {
        space
            .write_user_bytes(
                VirtAddr::new(ipc_words_address + index as u64 * 8),
                &word.to_le_bytes(),
            )
            .expect("не удалось подготовить IPC send-буфер системного вызова");
    }

    let endpoint_id = phoenix_ipc::EndpointId(0x5359_5343_414c_4c49);
    let transfer_endpoint_id = phoenix_ipc::EndpointId(0x5452_414e_5346_4552);
    let mut endpoint_registry = arch::x86_64::syscall::KernelEndpointRegistry::new();
    endpoint_registry
        .insert(phoenix_ipc::Endpoint::new(endpoint_id))
        .expect("не удалось зарегистрировать endpoint системного вызова");
    let mut capabilities = ProcessCapabilitySet::<
        { arch::x86_64::syscall::PROCESS_CAPABILITY_CAPACITY },
    >::new(process.id);
    let mut receiver_capabilities = ProcessCapabilitySet::<
        { arch::x86_64::syscall::PROCESS_CAPABILITY_CAPACITY },
    >::new(ProcessId(45));
    endpoint_registry
        .insert(phoenix_ipc::Endpoint::new_owned(
            transfer_endpoint_id,
            receiver_capabilities.owner(),
        ))
        .expect("не удалось зарегистрировать endpoint получателя capability");
    let mut process_capability_registry =
        arch::x86_64::syscall::KernelProcessCapabilityRegistry::new();
    let endpoint_handle = capabilities
        .insert(Capability {
            object: ObjectId(endpoint_id.0),
            kind: ObjectKind::Endpoint,
            rights: Rights::READ.union(Rights::WRITE),
        })
        .expect("не удалось выдать endpoint-возможность пользовательскому процессу");
    let read_only_handle = capabilities
        .insert(Capability {
            object: ObjectId(endpoint_id.0),
            kind: ObjectKind::Endpoint,
            rights: Rights::READ,
        })
        .expect("не удалось выдать read-only endpoint-возможность");
    let transfer_endpoint_handle = capabilities
        .insert(Capability {
            object: ObjectId(transfer_endpoint_id.0),
            kind: ObjectKind::Endpoint,
            rights: Rights::WRITE,
        })
        .expect("не удалось выдать возможность отправки в endpoint получателя");
    let transferred_handle = capabilities
        .insert(Capability {
            object: ObjectId(0x4d45_4d4f_5259),
            kind: ObjectKind::Memory,
            rights: Rights::READ.union(Rights::TRANSFER),
        })
        .expect("не удалось создать передаваемую возможность");
    process_capability_registry
        .register(&mut capabilities)
        .expect("не удалось зарегистрировать таблицу возможностей отправителя");
    process_capability_registry
        .register(&mut receiver_capabilities)
        .expect("не удалось зарегистрировать таблицу возможностей получателя");
    let packed_endpoint = phoenix_syscall_abi::PackedCapabilityHandle::new(
        endpoint_handle.slot,
        endpoint_handle.generation,
    );
    let packed_read_only = phoenix_syscall_abi::PackedCapabilityHandle::new(
        read_only_handle.slot,
        read_only_handle.generation,
    );
    let packed_transfer_endpoint = phoenix_syscall_abi::PackedCapabilityHandle::new(
        transfer_endpoint_handle.slot,
        transfer_endpoint_handle.generation,
    );
    let packed_transferred = phoenix_syscall_abi::PackedCapabilityHandle::new(
        transferred_handle.slot,
        transferred_handle.generation,
    );

    let file_path = b"/syscall";
    let file_path_address = USER_MODE_TEST_STACK_ADDRESS + 0x180;
    let file_input = b"phoenix-file-io";
    let file_input_address = USER_MODE_TEST_STACK_ADDRESS + 0x400;
    let file_output_address = USER_MODE_TEST_STACK_ADDRESS + 0x440;
    space
        .write_user_bytes(VirtAddr::new(file_path_address), file_path)
        .expect("не удалось подготовить путь файловой syscall-проверки");
    space
        .write_user_bytes(VirtAddr::new(file_input_address), file_input)
        .expect("не удалось подготовить входной буфер файловой syscall-проверки");
    space
        .write_user_bytes(VirtAddr::new(file_output_address), &[0; 15])
        .expect("не удалось подготовить выходной буфер файловой syscall-проверки");
    let mut filesystem = arch::x86_64::syscall::KernelFileSystem::new();
    filesystem
        .create_file("/syscall")
        .expect("не удалось создать файл syscall-проверки");
    let root_node = filesystem
        .resolve("/")
        .expect("не удалось получить корень файловой syscall-проверки");
    let mut filesystems = arch::x86_64::syscall::KernelFileSystemRegistry::new();
    filesystems
        .register(arch::x86_64::syscall::ROOT_FILESYSTEM_ID, &mut filesystem)
        .expect("не удалось зарегистрировать корневую файловую систему");
    let mut mounts = arch::x86_64::syscall::KernelMountTable::new(phoenix_vfs::VfsNode::new(
        arch::x86_64::syscall::ROOT_FILESYSTEM_ID,
        root_node,
    ));
    let mut file_descriptors = arch::x86_64::syscall::KernelDescriptorTable::new();
    let file_context_guard = unsafe {
        arch::x86_64::syscall::install_current_file_context(
            &mut filesystems,
            &mut mounts,
            &mut file_descriptors,
        )
    }
    .expect("файловый контекст текущего процесса уже установлен");

    let context_guard = unsafe {
        arch::x86_64::syscall::install_current_process_context(
            &mut space,
            &mut capabilities,
            &mut endpoint_registry,
            &mut process_capability_registry,
        )
    }
    .expect("контекст текущего процесса уже установлен");
    let context_ok = arch::x86_64::syscall::ipc_endpoint_operations_self_test(
        phoenix_syscall_abi::IpcSendArguments {
            endpoint: packed_endpoint,
            words_address: ipc_words_address,
            word_count: send_words.len() as u64,
            transferred_capability: None,
            flags: 0,
        },
        phoenix_syscall_abi::IpcSendArguments {
            endpoint: packed_endpoint,
            words_address: USER_MODE_TEST_STACK_ADDRESS + phoenix_process::PAGE_SIZE,
            word_count: 1,
            transferred_capability: None,
            flags: 0,
        },
        phoenix_syscall_abi::IpcSendArguments {
            endpoint: packed_read_only,
            words_address: ipc_words_address,
            word_count: send_words.len() as u64,
            transferred_capability: None,
            flags: 0,
        },
        phoenix_syscall_abi::IpcReceiveArguments {
            endpoint: packed_endpoint,
            words_address: ipc_words_address,
            word_capacity: send_words.len() as u64,
            metadata_address: USER_MODE_TEST_STACK_ADDRESS + 0x100,
            flags: 0,
        },
        endpoint_id,
    );
    let capability_transfer_ok = arch::x86_64::syscall::ipc_capability_transfer_self_test(
        phoenix_syscall_abi::IpcSendArguments {
            endpoint: packed_transfer_endpoint,
            words_address: ipc_words_address,
            word_count: send_words.len() as u64,
            transferred_capability: Some(packed_transferred),
            flags: 0,
        },
    );
    let file_context_ok = arch::x86_64::syscall::file_context_operations_self_test(
        file_path_address,
        file_path.len() as u64,
        file_input_address,
        file_output_address,
        file_input.len() as u64,
    );
    drop(context_guard);
    drop(file_context_guard);

    let mut copied_back = [0_u8; 16];
    space
        .read_user_bytes(VirtAddr::new(ipc_words_address), &mut copied_back)
        .expect("не удалось проверить IPC copy-out из контекста системного вызова");
    let copied_receive_words = [
        u64::from_le_bytes(copied_back[..8].try_into().unwrap()),
        u64::from_le_bytes(copied_back[8..].try_into().unwrap()),
    ];
    let mut copied_metadata = [0_u8; 24];
    space
        .read_user_bytes(
            VirtAddr::new(USER_MODE_TEST_STACK_ADDRESS + 0x100),
            &mut copied_metadata,
        )
        .expect("не удалось прочитать IPC-метаданные из пользовательской памяти");
    let metadata_sender = u64::from_le_bytes(copied_metadata[..8].try_into().unwrap());
    let metadata_word_count = u64::from_le_bytes(copied_metadata[8..16].try_into().unwrap());
    let metadata_capability = u64::from_le_bytes(copied_metadata[16..].try_into().unwrap());
    if !context_ok
        || copied_receive_words != send_words
        || metadata_sender != capabilities.owner().0
        || metadata_word_count != send_words.len() as u64
        || metadata_capability != phoenix_syscall_abi::NO_TRANSFERRED_CAPABILITY
    {
        panic!("IPC syscall не сохранил payload или метаданные сообщения");
    }
    let transfer_message = endpoint_registry
        .get(transfer_endpoint_id)
        .expect("endpoint переноса capability исчез из реестра")
        .peek()
        .expect("ipc_send не поставил сообщение с capability в очередь");
    let received_handle = transfer_message
        .transferred_capability()
        .expect("сообщение потеряло переданную capability");
    let received_capability = receiver_capabilities
        .get(received_handle)
        .expect("получатель не получил переданную capability");
    if !capability_transfer_ok
        || capabilities.get(transferred_handle).is_ok()
        || transfer_message.sender != capabilities.owner()
        || transfer_message.words() != send_words
        || received_capability.object != ObjectId(0x4d45_4d4f_5259)
        || received_capability.kind != ObjectKind::Memory
        || !received_capability.rights.contains(Rights::TRANSFER)
    {
        panic!("атомарная передача capability через ipc_send нарушена");
    }
    let mut file_output = [0_u8; 15];
    space
        .read_user_bytes(VirtAddr::new(file_output_address), &mut file_output)
        .expect("не удалось прочитать результат файловой syscall-проверки");
    if !file_context_ok || !file_descriptors.is_empty() || file_output != *file_input {
        panic!("файловый syscall-контекст нарушил жизненный цикл дескриптора");
    }
    serial::emergency(format_args!(
        "[INFO] ipc syscall memory context self-test: OK\n"
    ));
    serial::emergency(format_args!(
        "[INFO] ipc capability endpoint self-test: OK\n"
    ));
    serial::emergency(format_args!(
        "[INFO] ipc endpoint syscall operations self-test: OK\n"
    ));
    serial::emergency(format_args!(
        "[INFO] ipc capability transfer self-test: OK\n"
    ));
    serial::emergency(format_args!(
        "[INFO] file syscall context operations self-test: OK\n"
    ));

    let stack_pointer = USER_MODE_TEST_STACK_ADDRESS + phoenix_process::PAGE_SIZE - 16;

    let exit_status = x86_64::instructions::interrupts::without_interrupts(|| {
        let file_context_guard = unsafe {
            arch::x86_64::syscall::install_current_file_context(
                &mut filesystems,
                &mut mounts,
                &mut file_descriptors,
            )
        }
        .expect("файловый контекст пользовательского процесса уже установлен");
        let context_guard = unsafe {
            arch::x86_64::syscall::install_current_process_context(
                &mut space,
                &mut capabilities,
                &mut endpoint_registry,
                &mut process_capability_registry,
            )
        }
        .expect("контекст пользовательского системного вызова уже установлен");
        let guard = unsafe { space.activate() };
        let result = arch::x86_64::syscall::run_user_process_with_start_info(
            USER_MODE_TEST_CODE_ADDRESS,
            stack_pointer,
            start_info_address,
        );
        drop(guard);
        drop(context_guard);
        drop(file_context_guard);
        result
    });

    let Ok(exit_status) = exit_status else {
        panic!("пользовательский цикл CPL3/SYSCALL/SYSRET завершился ошибкой");
    };
    if exit_status != arch::x86_64::syscall::USER_SELF_TEST_SUCCESS {
        let stage = arch::x86_64::syscall::user_self_test_failure_stage(exit_status)
            .unwrap_or("неизвестный этап");
        panic!(
            "пользовательский процесс вернул неожиданный статус {exit_status:#x}, этап: {stage}"
        );
    }
    process_manager
        .exit(process.id, exit_status)
        .expect("не удалось завершить и снять пользовательский процесс с планирования");
    let exited = process_manager
        .record(process.id)
        .expect("завершённый пользовательский процесс исчез из таблицы");
    if exited.state != ProcessState::Exited
        || exited.exit_status != Some(exit_status)
        || process_manager.current().is_some()
        || process_manager.scheduled_len() != 0
        || process_manager.on_tick() != ProcessScheduleDecision::Idle
    {
        panic!("завершённый процесс остался доступен планировщику");
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

    let reaped = process_manager
        .reap(process.id)
        .expect("не удалось очистить завершённый пользовательский процесс");
    if reaped.exit_status != Some(exit_status) || process_manager.len() != 0 {
        panic!("очистка процесса потеряла статус завершения");
    }

    if frames.free_frames() != free_before {
        panic!("пользовательская самопроверка не вернула всю физическую память");
    }
}

fn system_service_lifecycle_self_test() {
    use phoenix_process_manager::ProcessScheduleDecision;
    use phoenix_service_manager::{
        RestartPolicy, ServiceId, ServiceManager, ServiceManifest, ServiceState,
    };

    let manifest = ServiceManifest {
        id: ServiceId(1),
        image_id: 0x5048_4f45_4e49_5849,
        quantum_ticks: 2,
        restart_policy: RestartPolicy::OnFailure,
    };
    let mut manager = ServiceManager::<1, 1>::new();
    manager
        .register(manifest)
        .expect("не удалось зарегистрировать начальную системную службу");
    let first_process = manager
        .start(manifest.id)
        .expect("не удалось запустить начальную системную службу");
    if !matches!(
        manager.on_tick(),
        ProcessScheduleDecision::Switch { to, .. } if to == first_process
    ) {
        panic!("процесс системной службы не попал в планировщик");
    }

    let failed = manager
        .complete(manifest.id, 1)
        .expect("не удалось обработать аварийное завершение системной службы");
    let restarted_process = failed
        .restarted_as
        .expect("аварийная системная служба не была перезапущена");
    if failed.exited_process != first_process || restarted_process == first_process {
        panic!("перезапуск системной службы не заменил завершённый процесс");
    }

    let completed = manager
        .complete(manifest.id, 0)
        .expect("не удалось обработать штатное завершение системной службы");
    let record = manager
        .record(manifest.id)
        .expect("запись системной службы исчезла из реестра");
    if completed.restarted_as.is_some()
        || record.state != ServiceState::Exited
        || record.process.is_some()
        || record.last_exit_status != Some(0)
        || record.restart_count != 1
        || manager.process_count() != 0
    {
        panic!("политика жизненного цикла системной службы нарушена");
    }
}

fn write_process_start_info(
    space: &mut process_space::ProcessAddressSpace<{ process_space::PROCESS_REGION_CAPACITY }>,
    stack_address: u64,
) -> u64 {
    use phoenix_syscall_abi::{ProcessStartInfo, ProcessStartString};

    const ARGUMENT_ZERO: &[u8] = b"phoenix-test";
    const ARGUMENT_ONE: &[u8] = b"--self-test";
    const ENVIRONMENT_ZERO: &[u8] = b"MODE=qemu";

    let info_address = stack_address + 0x200;
    let argument_vector = stack_address + 0x300;
    let environment_vector = stack_address + 0x340;
    let argument_zero_address = stack_address + 0x380;
    let argument_one_address = stack_address + 0x390;
    let environment_zero_address = stack_address + 0x3a0;

    let info = ProcessStartInfo::new(2, argument_vector, 1, environment_vector)
        .expect("не удалось описать аргументы запуска процесса");
    let arguments = [
        ProcessStartString::new(argument_zero_address, ARGUMENT_ZERO.len() as u64),
        ProcessStartString::new(argument_one_address, ARGUMENT_ONE.len() as u64),
    ];
    let environment = [ProcessStartString::new(
        environment_zero_address,
        ENVIRONMENT_ZERO.len() as u64,
    )];

    for (index, word) in info.words().iter().enumerate() {
        space
            .write_user_bytes(
                VirtAddr::new(info_address + index as u64 * 8),
                &word.to_le_bytes(),
            )
            .expect("не удалось записать блок запуска процесса");
    }
    for (index, entry) in arguments.iter().enumerate() {
        let address = argument_vector + index as u64 * 16;
        space
            .write_user_bytes(VirtAddr::new(address), &entry.address.to_le_bytes())
            .expect("не удалось записать адрес аргумента процесса");
        space
            .write_user_bytes(VirtAddr::new(address + 8), &entry.length.to_le_bytes())
            .expect("не удалось записать длину аргумента процесса");
    }
    for (index, entry) in environment.iter().enumerate() {
        let address = environment_vector + index as u64 * 16;
        space
            .write_user_bytes(VirtAddr::new(address), &entry.address.to_le_bytes())
            .expect("не удалось записать адрес окружения процесса");
        space
            .write_user_bytes(VirtAddr::new(address + 8), &entry.length.to_le_bytes())
            .expect("не удалось записать длину окружения процесса");
    }
    space
        .write_user_bytes(VirtAddr::new(argument_zero_address), ARGUMENT_ZERO)
        .expect("не удалось записать первый аргумент процесса");
    space
        .write_user_bytes(VirtAddr::new(argument_one_address), ARGUMENT_ONE)
        .expect("не удалось записать второй аргумент процесса");
    space
        .write_user_bytes(VirtAddr::new(environment_zero_address), ENVIRONMENT_ZERO)
        .expect("не удалось записать окружение процесса");

    info_address
}

fn elf_user_execution_self_test(
    physical_memory_offset: VirtAddr,
    frames: &mut SystemFrameAllocator<SYSTEM_MEMORY_RANGE_CAPACITY>,
) {
    use phoenix_process::{
        AddressSpaceId, MemoryPermissions, ProcessCapabilitySet, RegionKind, VirtualRegion,
    };
    use phoenix_service_manager::{
        RestartPolicy, ServiceId, ServiceManager, ServiceManifest, ServiceState,
    };

    const LOAD_ADDRESS: u64 = 0x0000_0000_4300_0000;
    const STACK_ADDRESS: u64 = LOAD_ADDRESS + phoenix_process::PAGE_SIZE;
    const CODE_OFFSET: usize = 0x100;
    const IMAGE_SIZE: usize = phoenix_process::PAGE_SIZE as usize;
    const SERVICE_ID: ServiceId = ServiceId(2);
    const SERVICE_IMAGE_ID: u64 = 0x5048_4f45_4e49_5853;

    let manifest = ServiceManifest {
        id: SERVICE_ID,
        image_id: SERVICE_IMAGE_ID,
        quantum_ticks: 1,
        restart_policy: RestartPolicy::Never,
    };
    let mut service_manager = ServiceManager::<1, 1>::new();
    service_manager
        .register(manifest)
        .expect("не удалось зарегистрировать ELF-службу");
    let launch = service_manager
        .launch(SERVICE_ID)
        .expect("не удалось создать процесс ELF-службы");
    if launch.image_id != SERVICE_IMAGE_ID {
        panic!("менеджер служб выбрал неверный ELF-образ");
    }

    let user_code = arch::x86_64::syscall::user_mode_test_image();
    if user_code.is_empty() || CODE_OFFSET + user_code.len() > IMAGE_SIZE {
        panic!("пользовательский код не помещается в тестовый ELF64");
    }

    let free_before = frames.free_frames();
    let mut space = unsafe {
        process_space::ProcessAddressSpace::<{ process_space::PROCESS_REGION_CAPACITY }>::new(
            AddressSpaceId(47),
            physical_memory_offset,
            frames,
        )
    }
    .expect("не удалось создать адресное пространство для запуска ELF");

    let mut image = [0_u8; IMAGE_SIZE];
    image[0..4].copy_from_slice(b"\x7fELF");
    image[4] = 2;
    image[5] = 1;
    image[6] = 1;
    image[16..18].copy_from_slice(&2_u16.to_le_bytes());
    image[18..20].copy_from_slice(&62_u16.to_le_bytes());
    image[20..24].copy_from_slice(&1_u32.to_le_bytes());
    image[24..32].copy_from_slice(&(LOAD_ADDRESS + CODE_OFFSET as u64).to_le_bytes());
    image[32..40].copy_from_slice(&64_u64.to_le_bytes());
    image[52..54].copy_from_slice(&64_u16.to_le_bytes());
    image[54..56].copy_from_slice(&56_u16.to_le_bytes());
    image[56..58].copy_from_slice(&1_u16.to_le_bytes());

    let file_size = (CODE_OFFSET + user_code.len()) as u64;
    let ph = 64_usize;
    image[ph..ph + 4].copy_from_slice(&1_u32.to_le_bytes());
    image[ph + 4..ph + 8].copy_from_slice(&5_u32.to_le_bytes());
    image[ph + 8..ph + 16].copy_from_slice(&0_u64.to_le_bytes());
    image[ph + 16..ph + 24].copy_from_slice(&LOAD_ADDRESS.to_le_bytes());
    image[ph + 32..ph + 40].copy_from_slice(&file_size.to_le_bytes());
    image[ph + 40..ph + 48].copy_from_slice(&(IMAGE_SIZE as u64).to_le_bytes());
    image[ph + 48..ph + 56].copy_from_slice(&0x1000_u64.to_le_bytes());
    image[CODE_OFFSET..CODE_OFFSET + user_code.len()].copy_from_slice(user_code);

    let loaded = elf_user_loader::load_elf(&image[..file_size as usize], &mut space, frames)
        .expect("не удалось загрузить исполняемый ELF64");

    let stack_region = VirtualRegion::new(
        STACK_ADDRESS,
        phoenix_process::PAGE_SIZE,
        MemoryPermissions::USER_READ_WRITE,
        RegionKind::Stack,
    )
    .expect("не удалось описать стек ELF-процесса");
    space
        .map_region(stack_region, frames)
        .expect("не удалось отобразить стек ELF-процесса");
    let start_info_address = write_process_start_info(&mut space, STACK_ADDRESS);

    let mut capabilities = ProcessCapabilitySet::<
        { arch::x86_64::syscall::PROCESS_CAPABILITY_CAPACITY },
    >::new(launch.process);
    let mut endpoint_registry = arch::x86_64::syscall::KernelEndpointRegistry::new();
    let mut process_capability_registry =
        arch::x86_64::syscall::KernelProcessCapabilityRegistry::new();

    let stack_pointer = STACK_ADDRESS + phoenix_process::PAGE_SIZE - 16;
    let passed = x86_64::instructions::interrupts::without_interrupts(|| {
        let context_guard = unsafe {
            arch::x86_64::syscall::install_current_process_context(
                &mut space,
                &mut capabilities,
                &mut endpoint_registry,
                &mut process_capability_registry,
            )
        }
        .expect("контекст ELF-процесса уже установлен");
        let address_space_guard = unsafe { space.activate() };
        let result = arch::x86_64::syscall::run_user_process_with_start_info(
            loaded.entry_point,
            stack_pointer,
            start_info_address,
        );
        drop(address_space_guard);
        drop(context_guard);
        result
    });

    let Ok(exit_status) = passed else {
        panic!("ELF-служба не завершила пользовательский SYSCALL-цикл");
    };
    if exit_status != arch::x86_64::syscall::USER_SELF_TEST_SUCCESS {
        panic!("ELF-служба вернула неожиданный статус");
    }

    let completion = service_manager
        .complete(SERVICE_ID, exit_status)
        .expect("не удалось завершить ELF-службу");
    let record = service_manager
        .record(SERVICE_ID)
        .expect("ELF-служба исчезла из реестра");
    if completion.exited_process != launch.process
        || completion.restarted_as.is_some()
        || record.state != ServiceState::Exited
        || record.last_exit_status != Some(exit_status)
        || service_manager.process_count() != 0
    {
        panic!("жизненный цикл ELF-службы завершён несогласованно");
    }

    space
        .destroy(frames)
        .expect("не удалось уничтожить адресное пространство ELF-процесса");
    if frames.free_frames() != free_before {
        panic!("запуск ELF не вернул физические страницы распределителю");
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
