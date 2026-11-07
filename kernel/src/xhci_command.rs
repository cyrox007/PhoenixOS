//! Первый реальный DMA-обмен с контроллером xHCI в QEMU.
//!
//! Для испытания используется только явно разрешённый профиль без IOMMU.
//! Четыре физические страницы: DCBAA, командное кольцо, событийное кольцо,
//! таблица сегментов событий. Кольца остаются закреплёнными до сброса xHCI.

use core::sync::atomic::{Ordering, compiler_fence};
use phoenix_dma::DmaLayout;
use phoenix_dma::mapping::{DeviceMapping, DirectDmaAperture, PhysicalAddress, QemuDirectDma};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::xhci_capabilities::XhciCapabilities;
use phoenix_pcie::xhci_control::{self, OperationalRegisters};
use x86_64::VirtAddr;

const PAGE_BYTES: u64 = 4096;
const PAGE_COUNT: u64 = 4;
const COMMAND_RING_PAGE: u64 = 1;
const EVENT_RING_PAGE: u64 = 2;
const EVENT_TABLE_PAGE: u64 = 3;
const RING_ITEMS: u32 = 256;
const MAX_POLLS: usize = 5_000_000;
const NOOP_COMMAND: u32 = (23 << 10) | 1;
const LINK_TRB: u32 = (6 << 10) | 2 | 1;
const COMMAND_COMPLETION_EVENT: u32 = 33;
const SUCCESS_COMPLETION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    NoFrames,
    InvalidPhysicalAddress,
    DeviceMappingFailed,
    UnsupportedPageSize,
    ScratchpadUnsupported,
    StartTimeout,
    CommandTimeout,
    WrongEventType,
    WrongCommandPointer,
    WrongCompletionCode,
    UnsafeShutdown,
    DeviceUnmapFailed,
    FrameReleaseFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandReport {
    pub completion_code: u8,
    pub event_type: u8,
    pub used_pages: u64,
}

struct GuestMemory {
    virtual_start: VirtAddr,
}

impl GuestMemory {
    fn address(&self, page: u64, offset: usize) -> *mut u32 {
        (self.virtual_start + page * PAGE_BYTES + offset as u64).as_mut_ptr::<u32>()
    }

    fn write(&self, page: u64, offset: usize, value: u32) {
        unsafe { self.address(page, offset).write_volatile(value) };
    }

    fn read(&self, page: u64, offset: usize) -> u32 {
        unsafe { self.address(page, offset).read_volatile() }
    }
}

fn write_address<R: OperationalRegisters>(registers: &R, offset: u64, address: u64) {
    registers.write_u32(offset, address as u32);
    registers.write_u32(offset + 4, (address >> 32) as u32);
}

fn wait_halted<R: OperationalRegisters>(registers: &R, operational: u64, expected: bool) -> bool {
    for _ in 0..MAX_POLLS {
        let halted = registers.read_u32(operational + 4) & 1 != 0;
        if halted == expected {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn issue_noop<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
) -> Result<CommandReport, CommandError> {
    let operational = capabilities.operational_offset;
    let runtime = capabilities.runtime_offset + 0x20;
    let command_address = base + COMMAND_RING_PAGE * PAGE_BYTES;
    let event_address = base + EVENT_RING_PAGE * PAGE_BYTES;
    let table_address = base + EVENT_TABLE_PAGE * PAGE_BYTES;

    // xHCI HCSParams2 содержит число обязательных scratchpad-буферов.
    // В проверочном QEMU-профиле их нет; не запускаем без поддержки.
    let hcs2 = registers.read_u32(0x08);
    let scratchpads = ((hcs2 >> 27) & 31) << 5 | ((hcs2 >> 21) & 31);
    if scratchpads != 0 {
        return Err(CommandError::ScratchpadUnsupported);
    }
    if registers.read_u32(operational + 8) & 1 == 0 {
        return Err(CommandError::UnsupportedPageSize);
    }

    // Командный сегмент завершается Link TRB, замыкающим кольцо.
    memory.write(COMMAND_RING_PAGE, 12, NOOP_COMMAND);
    let link_offset = (RING_ITEMS as usize - 1) * 16;
    memory.write(COMMAND_RING_PAGE, link_offset, command_address as u32);
    memory.write(
        COMMAND_RING_PAGE,
        link_offset + 4,
        (command_address >> 32) as u32,
    );
    memory.write(COMMAND_RING_PAGE, link_offset + 12, LINK_TRB);

    // Одна запись ERST описывает 256 записей событий по 16 байт.
    memory.write(EVENT_TABLE_PAGE, 0, event_address as u32);
    memory.write(EVENT_TABLE_PAGE, 4, (event_address >> 32) as u32);
    memory.write(EVENT_TABLE_PAGE, 8, RING_ITEMS);

    registers.write_u32(operational + 0x38, u32::from(capabilities.max_slots));
    write_address(registers, operational + 0x30, base);
    // CRCR, бит 0 — начальное состояние циклового бита.
    write_address(registers, operational + 0x18, command_address | 1);
    registers.write_u32(runtime + 0x08, 1);
    write_address(registers, runtime + 0x18, event_address);
    write_address(registers, runtime + 0x10, table_address);
    // Прерывания не нужны: события читаем из оперативной памяти.
    registers.write_u32(runtime, 0);

    compiler_fence(Ordering::SeqCst);
    // Run/Stop=1 и Interrupt Enable=0.
    registers.write_u32(operational, 1);
    if !wait_halted(registers, operational, false) {
        return Err(CommandError::StartTimeout);
    }

    // Уведомляем контроллер о первой команде в кольце.
    registers.write_u32(capabilities.doorbell_offset, 0);
    for _ in 0..MAX_POLLS {
        let control = memory.read(EVENT_RING_PAGE, 12);
        if control & 1 == 0 {
            core::hint::spin_loop();
            continue;
        }
        compiler_fence(Ordering::SeqCst);
        let kind = ((control >> 10) & 0x3f) as u8;
        let code = (memory.read(EVENT_RING_PAGE, 8) >> 24) as u8;
        let pointer = u64::from(memory.read(EVENT_RING_PAGE, 0))
            | (u64::from(memory.read(EVENT_RING_PAGE, 4)) << 32);
        if kind != COMMAND_COMPLETION_EVENT as u8 {
            return Err(CommandError::WrongEventType);
        }
        if pointer != command_address {
            return Err(CommandError::WrongCommandPointer);
        }
        if code != SUCCESS_COMPLETION as u8 {
            return Err(CommandError::WrongCompletionCode);
        }
        // Объявляем контроллеру, что запись события обработана.
        write_address(registers, runtime + 0x18, event_address + 16 | (1 << 3));
        return Ok(CommandReport {
            completion_code: code,
            event_type: kind,
            used_pages: PAGE_COUNT,
        });
    }
    Err(CommandError::CommandTimeout)
}

/// Контроллер должен быть предварительно сброшен и остановлен.
///
/// # Безопасность
///
/// Вызывающий код гарантирует, что QEMU работает без IOMMU, выдаёт устройству
/// гостевые физические адреса и обеспечивает согласованность памяти x86.
/// При невозможности остановить/сбросить контроллер страницы намеренно
/// остаются закреплёнными: освобождать их небезопасно.
pub unsafe fn first_command<const N: usize, R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    frames: &mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
) -> Result<CommandReport, CommandError> {
    let first = frames
        .allocate_contiguous_below_4k(PAGE_COUNT, 1, 0xffff_ffff)
        .ok_or(CommandError::NoFrames)?;
    let base = first.start_address().as_u64();
    let Some(cpu_base) = physical_memory_offset.as_u64().checked_add(base) else {
        return Err(CommandError::InvalidPhysicalAddress);
    };
    let memory = GuestMemory {
        virtual_start: VirtAddr::new(cpu_base),
    };
    unsafe { core::ptr::write_bytes(cpu_base as *mut u8, 0, (PAGE_COUNT * PAGE_BYTES) as usize) };

    let aperture =
        DirectDmaAperture::new(0, 0x1_0000_0000).expect("некорректное адресное окно QEMU");
    let mut backend = unsafe { QemuDirectDma::<1>::new(aperture) };
    let layout = DmaLayout::new((PAGE_COUNT * PAGE_BYTES) as usize, PAGE_BYTES as usize)
        .expect("некорректный размер буфера xHCI");
    let mut device_mapping =
        DeviceMapping::establish(&mut backend, PhysicalAddress::new(base), layout)
            .map_err(|_| CommandError::DeviceMappingFailed)?;
    assert_eq!(device_mapping.device_address().value(), base);

    let result = issue_noop(registers, capabilities, base, &memory);

    // Независимо от результата прекращаем доступ контроллера к памяти.
    // Сброс удаляет ранее запрограммированные адреса колец.
    if xhci_control::stop_and_reset(
        registers,
        capabilities.operational_offset,
        xhci_control::MAX_POLLS,
    )
    .is_err()
    {
        return Err(CommandError::UnsafeShutdown);
    }
    device_mapping
        .close(&mut backend)
        .map_err(|_| CommandError::DeviceUnmapFailed)?;
    frames
        .release_contiguous_4k(first, PAGE_COUNT)
        .map_err(|_| CommandError::FrameReleaseFailed)?;
    result
}
