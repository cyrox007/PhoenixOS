from pathlib import Path

path = Path("Cargo.toml")
text = path.read_text()
old = '    "crates/user-runtime",\n    "crates/vfs",\n'
new = '    "crates/user-runtime",\n    "crates/vfs",\n    "userspace/smoke",\n'
if old not in text:
    raise SystemExit("Не найден список workspace")
text = text.replace(old, new, 1)
path.write_text(text)

path = Path("kernel/Cargo.toml")
text = path.read_text().rstrip() + """
\n[build-dependencies]
userspace-smoke = { package = "phoenix-userspace-smoke", path = "../userspace/smoke", artifact = "bin", target = "x86_64-unknown-none" }
"""
path.write_text(text + "\n")

path = Path("kernel/src/main.rs")
text = path.read_text()
marker = 'const SYSTEM_MEMORY_RANGE_CAPACITY: usize = 256;\n'
if marker not in text:
    raise SystemExit("Не найдено место подключения пользовательского ELF")
text = text.replace(
    marker,
    'static USERSPACE_SMOKE_ELF: &[u8] = include_bytes!(env!("PHOENIX_USERSPACE_SMOKE"));\n\n' + marker,
    1,
)

start = text.index("fn elf_user_execution_self_test(")
end = text.index("\nfn virtual_memory_self_test(", start)
replacement = r'''fn elf_user_execution_self_test(
    physical_memory_offset: VirtAddr,
    frames: &mut SystemFrameAllocator<SYSTEM_MEMORY_RANGE_CAPACITY>,
) {
    use phoenix_process::{
        AddressSpaceId, MemoryPermissions, ProcessCapabilitySet, RegionKind, VirtualRegion,
    };
    use phoenix_service_manager::{
        RestartPolicy, ServiceId, ServiceManager, ServiceManifest, ServiceState,
    };

    const STACK_ADDRESS: u64 = 0x0000_0000_4500_0000;
    const SERVICE_ID: ServiceId = ServiceId(2);
    const SERVICE_IMAGE_ID: u64 = 0x5048_4f45_4e49_5853;

    if USERSPACE_SMOKE_ELF.is_empty() {
        panic!("собранный пользовательский ELF пуст");
    }

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

    let free_before = frames.free_frames();
    let mut space = unsafe {
        process_space::ProcessAddressSpace::<{ process_space::PROCESS_REGION_CAPACITY }>::new(
            AddressSpaceId(47),
            physical_memory_offset,
            frames,
        )
    }
    .expect("не удалось создать адресное пространство для запуска ELF");

    let loaded = elf_user_loader::load_elf(USERSPACE_SMOKE_ELF, &mut space, frames)
        .expect("не удалось загрузить собранную пользовательскую программу");
    if loaded.segment_count == 0 {
        panic!("пользовательский ELF не содержит загружаемых сегментов");
    }

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
        panic!("собранная пользовательская программа не завершила SYSCALL-цикл");
    };
    if exit_status != arch::x86_64::syscall::USER_SELF_TEST_SUCCESS {
        panic!("собранная пользовательская программа вернула неожиданный статус");
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
'''
text = text[:start] + replacement + text[end:]
path.write_text(text)

path = Path("ROADMAP.md")
text = path.read_text()
text = text.replace("- среда выполнения пользовательских программ;", "- [x] среда выполнения пользовательских программ;", 1)
text = text.replace("  - снимки и восстановление;", "  - [x] снимки и восстановление;", 1)
text = text.replace("  - контрольные суммы пользовательских данных;", "  - [x] контрольные суммы пользовательских данных;", 1)
path.write_text(text)

path = Path("userspace/README.md")
text = """# Пользовательское пространство

Пользовательское пространство PhoenixOS уже имеет минимальный исполняемый контур: отдельное адресное пространство, загрузку ELF64, передачу аргументов и окружения через стабильный ABI, системные вызовы и библиотеку phoenix-user-runtime.

Каталог smoke содержит минимальную реально компилируемую пользовательскую программу. Она собирается отдельно для x86_64-unknown-none, встраивается в проверочный образ ядра, загружается штатным ELF-загрузчиком, запускается в пользовательском режиме и завершает процесс через системный вызов. Это эталон минимального исполняемого файла до появления оболочки и обычных системных программ.
"""
path.write_text(text)

path = Path("STATUS.md")
text = path.read_text().rstrip() + r'''

## Пользовательская среда — первый реально скомпилированный процесс

Минимальная среда выполнения пользовательских программ теперь проверяется не только вручную сформированным машинным кодом. Добавлена отдельная no_std-программа userspace/smoke, которая собирается для x86_64-unknown-none как самостоятельный ELF64 и использует библиотеку phoenix-user-runtime.

Ядро получает собранный ELF как зависимость сборки, загружает его штатным ELF-загрузчиком в отдельное адресное пространство, создаёт пользовательский стек, передаёт ProcessStartInfo с аргументами и окружением, переводит процесс в пользовательский режим и принимает завершение через системный вызов. Программа проверяет переданные строки и возвращает контрольный статус через phoenix-user-runtime::start_with_info.

Тем самым сквозной путь теперь выглядит как настоящий путь приложения: исходный код пользовательской программы → отдельная сборка ELF64 → загрузка сегментов с правами памяти → запуск в Ring 3 → системный вызов → завершение жизненного цикла службы → освобождение адресного пространства. Ручной машинный код остаётся низкоуровневой проверкой механизма системных вызовов, а не заменой пользовательской программы.
'''
path.write_text(text + "\n")
