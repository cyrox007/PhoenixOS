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
const PAGE_COUNT: u64 = 8;
const INPUT_PAGE: u64 = 4;
const OUTPUT_PAGE: u64 = 5;
const TRANSFER_PAGE: u64 = 6;
const DESCRIPTOR_PAGE: u64 = 7;
const COMMAND_RING_PAGE: u64 = 1;
const EVENT_RING_PAGE: u64 = 2;
const EVENT_TABLE_PAGE: u64 = 3;
const RING_ITEMS: u32 = 256;
const MAX_POLLS: usize = 5_000_000;
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
    InvalidSlot,
    InvalidSpeed,
    InvalidDescriptor,
    InvalidTransferEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandReport {
    pub completion_code: u8,
    pub event_type: u8,
    pub used_pages: u64,
    pub slot_id: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_class: u8,
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

#[derive(Clone, Copy)]
struct Completion {
    kind: u8,
    code: u8,
    slot: u8,
    pointer: u64,
}

fn await_completion<R: OperationalRegisters>(
    registers: &R,
    runtime: u64,
    base: u64,
    memory: &GuestMemory,
    index: usize,
) -> Result<Completion, CommandError> {
    let offset = index * 16;
    for _ in 0..MAX_POLLS {
        let control = memory.read(EVENT_RING_PAGE, offset + 12);
        if control & 1 == 0 {
            core::hint::spin_loop();
            continue;
        }
        compiler_fence(Ordering::SeqCst);
        let event = Completion {
            kind: ((control >> 10) & 0x3f) as u8,
            code: (memory.read(EVENT_RING_PAGE, offset + 8) >> 24) as u8,
            slot: (control >> 24) as u8,
            pointer: u64::from(memory.read(EVENT_RING_PAGE, offset))
                | (u64::from(memory.read(EVENT_RING_PAGE, offset + 4)) << 32),
        };
        // Указываем следующий свободный элемент, учитывая бит EHB.
        write_address(registers, runtime + 0x18, base + EVENT_RING_PAGE * PAGE_BYTES
            + ((index + 1) * 16) as u64 | (1 << 3));
        return Ok(event);
    }
    Err(CommandError::CommandTimeout)
}

fn command<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    index: usize,
    parameter: u64,
    control: u32,
) -> Result<Completion, CommandError> {
    let address = base + COMMAND_RING_PAGE * PAGE_BYTES + (index * 16) as u64;
    memory.write(COMMAND_RING_PAGE, index * 16, parameter as u32);
    memory.write(COMMAND_RING_PAGE, index * 16 + 4, (parameter >> 32) as u32);
    memory.write(COMMAND_RING_PAGE, index * 16 + 8, 0);
    memory.write(COMMAND_RING_PAGE, index * 16 + 12, control | 1);
    compiler_fence(Ordering::SeqCst);
    registers.write_u32(capabilities.doorbell_offset, 0);
    let event = await_completion(
        registers,
        capabilities.runtime_offset + 0x20,
        base,
        memory,
        index,
    )?;
    if event.kind != COMMAND_COMPLETION_EVENT as u8 {
        return Err(CommandError::WrongEventType);
    }
    if event.pointer != address {
        return Err(CommandError::WrongCommandPointer);
    }
    if event.code != SUCCESS_COMPLETION as u8 {
        return Err(CommandError::WrongCompletionCode);
    }
    Ok(event)
}

fn read_descriptor<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    port: u8,
    speed: u8,
) -> Result<CommandReport, CommandError> {
    let enabled = command(registers, capabilities, base, memory, 1, 0, 9 << 10)?;
    if enabled.slot == 0 || enabled.slot > capabilities.max_slots {
        return Err(CommandError::InvalidSlot);
    }
    let slot = enabled.slot;
    let context = usize::from(capabilities.context_size_bytes);
    let packet_size = match speed {
        1 | 2 => 8,
        3 => 64,
        4 => 512,
        _ => return Err(CommandError::InvalidSpeed),
    };
    let output = base + OUTPUT_PAGE * PAGE_BYTES;
    let input = base + INPUT_PAGE * PAGE_BYTES;
    let transfer = base + TRANSFER_PAGE * PAGE_BYTES;
    let descriptor = base + DESCRIPTOR_PAGE * PAGE_BYTES;

    // DCBAA[slot] — адрес выходного контекста устройства.
    memory.write(0, usize::from(slot) * 8, output as u32);
    memory.write(0, usize::from(slot) * 8 + 4, (output >> 32) as u32);

    // Контекст управления (Add: Slot + EP0), за ним Slot и EP0.
    memory.write(INPUT_PAGE, 4, 3);
    memory.write(INPUT_PAGE, context, (u32::from(speed) << 20) | (1 << 27));
    memory.write(INPUT_PAGE, context + 4, u32::from(port) << 16);
    let ep = context * 2;
    memory.write(
        INPUT_PAGE,
        ep + 4,
        (3 << 1) | (4 << 3) | (packet_size << 16),
    );
    memory.write(INPUT_PAGE, ep + 8, (transfer | 1) as u32);
    memory.write(INPUT_PAGE, ep + 12, (transfer >> 32) as u32);
    memory.write(INPUT_PAGE, ep + 16, 8);

    // Link TRB на конце управляющего кольца.
    let link = (RING_ITEMS as usize - 1) * 16;
    memory.write(TRANSFER_PAGE, link, transfer as u32);
    memory.write(TRANSFER_PAGE, link + 4, (transfer >> 32) as u32);
    memory.write(TRANSFER_PAGE, link + 12, LINK_TRB);

    compiler_fence(Ordering::SeqCst);
    let _addressed = command(
        registers, capabilities, base, memory, 2, input,
        (11 << 10) | (u32::from(slot) << 24),
    )?;

    // GET_DESCRIPTOR(Device, 18): Setup -> Data IN -> Status OUT.
    memory.write(TRANSFER_PAGE, 0, 0x0100_0680);
    memory.write(TRANSFER_PAGE, 4, 18 << 16);
    memory.write(TRANSFER_PAGE, 8, 8);
    memory.write(TRANSFER_PAGE, 12, (2 << 10) | (3 << 16) | (1 << 6) | 1);
    memory.write(TRANSFER_PAGE, 16, descriptor as u32);
    memory.write(TRANSFER_PAGE, 20, (descriptor >> 32) as u32);
    memory.write(TRANSFER_PAGE, 24, 18);
    memory.write(TRANSFER_PAGE, 28, (3 << 10) | (1 << 16) | 1);
    memory.write(TRANSFER_PAGE, 32, 0);
    memory.write(TRANSFER_PAGE, 36, 0);
    memory.write(TRANSFER_PAGE, 40, 0);
    memory.write(TRANSFER_PAGE, 44, (4 << 10) | (1 << 5) | 1);
    compiler_fence(Ordering::SeqCst);
    // Doorbell[slot], DB Target 1 = управляющая конечная точка.
    registers.write_u32(capabilities.doorbell_offset + u64::from(slot) * 4, 1);
    let event = await_completion(
        registers,
        capabilities.runtime_offset + 0x20,
        base,
        memory,
        3,
    )?;
    if event.kind != 32 || event.code != 1 || event.slot != slot {
        return Err(CommandError::InvalidTransferEvent);
    }
    if event.pointer != transfer + 32 {
        return Err(CommandError::InvalidTransferEvent);
    }

    let first = memory.read(DESCRIPTOR_PAGE, 0);
    let second = memory.read(DESCRIPTOR_PAGE, 4);
    if first & 0xffff != 0x0112 {
        return Err(CommandError::InvalidDescriptor);
    }
    let identifiers = memory.read(DESCRIPTOR_PAGE, 8);
    Ok(CommandReport {
        completion_code: event.code,
        event_type: event.kind,
        used_pages: PAGE_COUNT,
        slot_id: slot,
        vendor_id: identifiers as u16,
        product_id: (identifiers >> 16) as u16,
        device_class: (second >> 8) as u8,
    })
}

fn issue_noop<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    port: u8,
    speed: u8,
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

    // Проверяем старый безопасный путь No Op до выдачи слота.
    let noop = command(registers, capabilities, base, memory, 0, 0, 23 << 10)?;
    if noop.code != SUCCESS_COMPLETION as u8 {
        return Err(CommandError::WrongCompletionCode);
    }
    read_descriptor(registers, capabilities, base, memory, port, speed)

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
    port: u8,
    speed: u8,
) -> Result<CommandReport, CommandError> {
    let free_before = frames.free_frames();
    let first = frames
        .allocate_contiguous_below_4k(PAGE_COUNT, 1, 0xffff_ffff)
        .ok_or(CommandError::NoFrames)?;
    let base = first.start_address().as_u64();
    let Some(cpu_base) = physical_memory_offset.as_u64().checked_add(base) else {
        frames
            .release_contiguous_4k(first, PAGE_COUNT)
            .map_err(|_| CommandError::FrameReleaseFailed)?;
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
        match DeviceMapping::establish(&mut backend, PhysicalAddress::new(base), layout) {
            Ok(mapping) => mapping,
            Err(_) => {
                frames
                    .release_contiguous_4k(first, PAGE_COUNT)
                    .map_err(|_| CommandError::FrameReleaseFailed)?;
                return Err(CommandError::DeviceMappingFailed);
            }
        };
    assert_eq!(device_mapping.device_address().value(), base);

    let result = issue_noop(registers, capabilities, base, &memory, port, speed);

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
    assert_eq!(
        frames.free_frames(),
        free_before,
        "DMA-обмен xHCI не вернул все физические страницы"
    );
    result
}
