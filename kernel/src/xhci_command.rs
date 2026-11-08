//! Перечисление двух USB-устройств в одном сеансе xHCI через DMA в QEMU.
//!
//! Двенадцать страниц закреплены на весь сеанс: четыре общие страницы
//! и по четыре для каждого USB-устройства. У каждого собственный слот,
//! контексты и кольцо EP0. Сброс выполняется только после всех обменов.
//! Это диагностический сеанс, не служба постоянного приёма HID-отчётов.

use core::sync::atomic::{Ordering, compiler_fence};
use phoenix_dma::DmaLayout;
use phoenix_dma::mapping::{DeviceMapping, DirectDmaAperture, PhysicalAddress, QemuDirectDma};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::usb_descriptors::{self, DescriptorError, HidConfiguration};
use phoenix_pcie::xhci_capabilities::XhciCapabilities;
use phoenix_pcie::xhci_control::{self, OperationalRegisters};
use phoenix_pcie::xhci_session::{self, CommandCursor, EventCursor, SessionDevice};
use x86_64::VirtAddr;
use x86_64::structures::paging::{PhysFrame, Size4KiB};

const PAGE_BYTES: u64 = 4096;
const PAGE_COUNT: u64 = xhci_session::DMA_PAGE_COUNT;
const COMMAND_RING_PAGE: u64 = 1;
const EVENT_RING_PAGE: u64 = 2;
const EVENT_TABLE_PAGE: u64 = 3;
const RING_ITEMS: u32 = 256;
const MAX_POLLS: usize = 5_000_000;
const LINK_TRB: u32 = (6 << 10) | 2 | 1;
const COMMAND_COMPLETION_EVENT: u32 = 33;
const SUCCESS_COMPLETION: u32 = 1;
const HID_RING_START: usize = 1024;
const HID_RING_ITEMS: usize = (PAGE_BYTES as usize - HID_RING_START) / 16;
const HID_REPORT_OFFSET: usize = 896;
const HID_ENDPOINT_TYPE: u32 = 7;

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
    InvalidConfiguration,
    Configuration(DescriptorError),
    DuplicateSlot,
    RetainedConfigurationChanged,
    ControllerStopped,
    UnexpectedRuntimeEvent(u8),
    InvalidPortEvent,
    AlreadyStopped,
    UnexpectedPort(u8),
    NoFreeDevicePages,
    ServiceFaulted,
    AlreadyConfigured,
    InvalidEndpoint,
    InvalidHidEvent,
    EndpointRingFull,
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
    pub interface_class: u8,
    pub interface_protocol: u8,
    pub endpoint_count: u8,
    pub configuration_length: u16,
    pub hid: HidConfiguration,
}

type DevicePages = SessionDevice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetainedSlotsReport {
    pub first_slot: u8,
    pub second_slot: u8,
    pub rechecked_slot: u8,
    pub pages: u64,
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

    fn read_byte(&self, page: u64, offset: usize) -> u8 {
        let pointer = (self.virtual_start + page * PAGE_BYTES + offset as u64).as_ptr::<u8>();
        unsafe { pointer.read_volatile() }
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
    remaining: u32,
}

/// Действующая передача по USB-точке прерываний.
/// Кольцо размещается в свободной части страницы дескриптора.
#[derive(Clone, Copy)]
struct HidChannel {
    pages: DevicePages,
    slot: u8,
    descriptor: HidConfiguration,
    index: usize,
    cycle: u32,
    outstanding: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidRawReport {
    pub slot: u8,
    pub protocol: u8,
    pub length: u8,
    pub bytes: [u8; 8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEvent {
    Port(RuntimePortEvent),
    Hid(HidRawReport),
}

/// Состояние двух колец и отложенных извещений портов.
struct IoState {
    commands: CommandCursor,
    events: EventCursor,
    pending_ports: [Option<RuntimePortEvent>; 256],
    hid: [Option<HidChannel>; 2],
    pending_hid: [Option<Completion>; 2],
    control_next: [usize; 2],
}

impl IoState {
    fn new() -> Self {
        Self {
            commands: CommandCursor::new(),
            events: EventCursor::new(),
            pending_ports: [None; 256],
            hid: [None; 2],
            pending_hid: [None; 2],
            control_next: [12, 9],
        }
    }

    fn take_port(&mut self) -> Option<RuntimePortEvent> {
        for pending in &mut self.pending_ports {
            if let Some(event) = pending.take() {
                return Some(event);
            }
        }
        None
    }
}

fn hid_transfer_index(io: &IoState, event: Completion) -> Option<usize> {
    if event.kind != 32 {
        return None;
    }
    io.hid.iter().position(|channel| {
        channel.is_some_and(|hid| {
            hid.slot == event.slot && hid.outstanding == event.pointer
        })
    })
}

fn poll_event<R: OperationalRegisters>(
    registers: &R,
    runtime: u64,
    base: u64,
    memory: &GuestMemory,
    io: &mut IoState,
) -> Option<Completion> {
    let offset = io.events.index * 16;
    let control = memory.read(EVENT_RING_PAGE, offset + 12);
    if !io.events.available(control) {
        return None;
    }
    compiler_fence(Ordering::SeqCst);
    let event = Completion {
        kind: ((control >> 10) & 0x3f) as u8,
        code: (memory.read(EVENT_RING_PAGE, offset + 8) >> 24) as u8,
        slot: (control >> 24) as u8,
        pointer: u64::from(memory.read(EVENT_RING_PAGE, offset))
            | (u64::from(memory.read(EVENT_RING_PAGE, offset + 4)) << 32),
        remaining: memory.read(EVENT_RING_PAGE, offset + 8) & 0x00ff_ffff,
    };
    io.events.advance();
    // ERDP всегда адресует следующий элемент, в том числе после оборота.
    let next = base + EVENT_RING_PAGE * PAGE_BYTES + (io.events.index * 16) as u64;
    write_address(registers, runtime + 0x18, next | (1 << 3));
    Some(event)
}

fn await_completion<R: OperationalRegisters>(
    registers: &R,
    runtime: u64,
    base: u64,
    memory: &GuestMemory,
    io: &mut IoState,
) -> Result<Completion, CommandError> {
    for _ in 0..MAX_POLLS {
        let Some(event) = poll_event(registers, runtime, base, memory, io) else {
            core::hint::spin_loop();
            continue;
        };
        if event.kind == 34 {
            let port = (event.pointer >> 24) as u8;
            if port == 0 {
                return Err(CommandError::InvalidPortEvent);
            }
            io.pending_ports[usize::from(port)] = Some(RuntimePortEvent {
                port,
                completion_code: event.code,
            });
            continue;
        }
        if let Some(index) = hid_transfer_index(io, event) {
            io.pending_hid[index] = Some(event);
            continue;
        }
        return Ok(event);
    }
    Err(CommandError::CommandTimeout)
}

fn command<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    io: &mut IoState,
    parameter: u64,
    control: u32,
) -> Result<Completion, CommandError> {
    let index = io.commands.index;
    let cycle = io.commands.cycle;
    let address = base + COMMAND_RING_PAGE * PAGE_BYTES + (index * 16) as u64;
    memory.write(COMMAND_RING_PAGE, index * 16, parameter as u32);
    memory.write(COMMAND_RING_PAGE, index * 16 + 4, (parameter >> 32) as u32);
    memory.write(COMMAND_RING_PAGE, index * 16 + 8, 0);
    memory.write(COMMAND_RING_PAGE, index * 16 + 12, control | cycle);
    compiler_fence(Ordering::SeqCst);
    registers.write_u32(capabilities.doorbell_offset, 0);
    let event = await_completion(
        registers,
        capabilities.runtime_offset + 0x20,
        base,
        memory,
        io,
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
    if io.commands.advance() {
        // Прежний Link TRB уже пройден контроллером. Подготавливаем
        // цикловый бит для следующего оборота командного кольца.
        let offset = (RING_ITEMS as usize - 1) * 16 + 12;
        memory.write(COMMAND_RING_PAGE, offset, (6 << 10) | 2 | io.commands.cycle);
        compiler_fence(Ordering::SeqCst);
    }
    Ok(event)
}

/// Одна управляющая передача GET_DESCRIPTOR(Configuration).
/// Длина данных ограничена заранее выделенной страницей.
fn configuration_transfer<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    pages: DevicePages,
    slot: u8,
    trb: usize,
    length: usize,
    buffer_offset: usize,
    io: &mut IoState,
) -> Result<(), CommandError> {
    if length == 0 || length > 512 || buffer_offset + length > PAGE_BYTES as usize {
        return Err(CommandError::InvalidConfiguration);
    }
    let page = pages.transfer;
    let offset = trb * 16;
    let destination = base + pages.descriptor * PAGE_BYTES + buffer_offset as u64;
    memory.write(page, offset, 0x0200_0680);
    memory.write(page, offset + 4, (length as u32) << 16);
    memory.write(page, offset + 8, 8);
    memory.write(page, offset + 12, (2 << 10) | (3 << 16) | (1 << 6) | 1);
    memory.write(page, offset + 16, destination as u32);
    memory.write(page, offset + 20, (destination >> 32) as u32);
    memory.write(page, offset + 24, length as u32);
    memory.write(page, offset + 28, (3 << 10) | (1 << 16) | 1);
    memory.write(page, offset + 32, 0);
    memory.write(page, offset + 36, 0);
    memory.write(page, offset + 40, 0);
    memory.write(page, offset + 44, (4 << 10) | (1 << 5) | 1);
    compiler_fence(Ordering::SeqCst);
    registers.write_u32(capabilities.doorbell_offset + u64::from(slot) * 4, 1);
    let event = await_completion(
        registers,
        capabilities.runtime_offset + 0x20,
        base,
        memory,
        io,
    )?;
    if event.kind != 32 || event.code != 1 || event.slot != slot {
        return Err(CommandError::InvalidTransferEvent);
    }
    if event.pointer != base + page * PAGE_BYTES + offset as u64 + 32 {
        return Err(CommandError::InvalidTransferEvent);
    }
    Ok(())
}

fn read_descriptor<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    pages: DevicePages,
    port: u8,
    speed: u8,
    io: &mut IoState,
) -> Result<CommandReport, CommandError> {
    let enabled = command(registers, capabilities, base, memory, io, 0, 9 << 10)?;
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
    let output = base + pages.output * PAGE_BYTES;
    let input = base + pages.input * PAGE_BYTES;
    let transfer = base + pages.transfer * PAGE_BYTES;
    let descriptor = base + pages.descriptor * PAGE_BYTES;

    // DCBAA[slot] — адрес выходного контекста устройства.
    memory.write(0, usize::from(slot) * 8, output as u32);
    memory.write(0, usize::from(slot) * 8 + 4, (output >> 32) as u32);

    // Контекст управления (Add: Slot + EP0), за ним Slot и EP0.
    memory.write(pages.input, 4, 3);
    memory.write(pages.input, context, (u32::from(speed) << 20) | (1 << 27));
    memory.write(pages.input, context + 4, u32::from(port) << 16);
    let ep = context * 2;
    memory.write(
        pages.input,
        ep + 4,
        (3 << 1) | (4 << 3) | (packet_size << 16),
    );
    memory.write(pages.input, ep + 8, (transfer | 1) as u32);
    memory.write(pages.input, ep + 12, (transfer >> 32) as u32);
    memory.write(pages.input, ep + 16, 8);

    // Link TRB на конце управляющего кольца.
    let link = (RING_ITEMS as usize - 1) * 16;
    memory.write(pages.transfer, link, transfer as u32);
    memory.write(pages.transfer, link + 4, (transfer >> 32) as u32);
    memory.write(pages.transfer, link + 12, LINK_TRB);

    compiler_fence(Ordering::SeqCst);
    let _addressed = command(
        registers,
        capabilities,
        base,
        memory,
        io,
        input,
        (11 << 10) | (u32::from(slot) << 24),
    )?;

    // GET_DESCRIPTOR(Device, 18): Setup -> Data IN -> Status OUT.
    memory.write(pages.transfer, 0, 0x0100_0680);
    memory.write(pages.transfer, 4, 18 << 16);
    memory.write(pages.transfer, 8, 8);
    memory.write(pages.transfer, 12, (2 << 10) | (3 << 16) | (1 << 6) | 1);
    memory.write(pages.transfer, 16, descriptor as u32);
    memory.write(pages.transfer, 20, (descriptor >> 32) as u32);
    memory.write(pages.transfer, 24, 18);
    memory.write(pages.transfer, 28, (3 << 10) | (1 << 16) | 1);
    memory.write(pages.transfer, 32, 0);
    memory.write(pages.transfer, 36, 0);
    memory.write(pages.transfer, 40, 0);
    memory.write(pages.transfer, 44, (4 << 10) | (1 << 5) | 1);
    compiler_fence(Ordering::SeqCst);
    // Doorbell[slot], DB Target 1 = управляющая конечная точка.
    registers.write_u32(capabilities.doorbell_offset + u64::from(slot) * 4, 1);
    let event = await_completion(
        registers,
        capabilities.runtime_offset + 0x20,
        base,
        memory,
        io,
    )?;
    if event.kind != 32 || event.code != 1 || event.slot != slot {
        return Err(CommandError::InvalidTransferEvent);
    }
    if event.pointer != transfer + 32 {
        return Err(CommandError::InvalidTransferEvent);
    }

    let first = memory.read(pages.descriptor, 0);
    let second = memory.read(pages.descriptor, 4);
    if first & 0xffff != 0x0112 {
        return Err(CommandError::InvalidDescriptor);
    }
    let identifiers = memory.read(pages.descriptor, 8);

    // Сначала короткий заголовок конфигурации, затем весь заявленный объём.
    configuration_transfer(
        registers,
        capabilities,
        base,
        memory,
        pages,
        slot,
        3,
        9,
        64,
        io,
    )?;
    let header = memory.read(pages.descriptor, 64);
    if header & 0xffff != 0x0209 {
        return Err(CommandError::InvalidConfiguration);
    }
    let configuration_size = (header >> 16) as usize;
    if !(9..=512).contains(&configuration_size) {
        return Err(CommandError::InvalidConfiguration);
    }
    configuration_transfer(
        registers,
        capabilities,
        base,
        memory,
        pages,
        slot,
        6,
        configuration_size,
        128,
        io,
    )?;
    let mut configuration = [0_u8; 512];
    for (index, byte) in configuration
        .iter_mut()
        .take(configuration_size)
        .enumerate()
    {
        *byte = memory.read_byte(pages.descriptor, 128 + index);
    }
    let interface = usb_descriptors::parse_configuration(&configuration[..configuration_size])
        .map_err(CommandError::Configuration)?;
    let hid = usb_descriptors::parse_hid_configuration(&configuration[..configuration_size])
        .map_err(CommandError::Configuration)?;
    if hid.protocol != interface.protocol {
        return Err(CommandError::InvalidConfiguration);
    }
    Ok(CommandReport {
        completion_code: event.code,
        event_type: event.kind,
        used_pages: PAGE_COUNT,
        slot_id: slot,
        vendor_id: identifiers as u16,
        product_id: (identifiers >> 16) as u16,
        device_class: second as u8,
        interface_class: interface.class,
        interface_protocol: interface.protocol,
        endpoint_count: interface.endpoint_count,
        configuration_length: interface.total_length,
        hid,
    })
}

fn issue_enumeration<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    first_port: u8,
    first_speed: u8,
    second_port: u8,
    second_speed: u8,
) -> Result<(CommandReport, CommandReport, RetainedSlotsReport, IoState), CommandError> {
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
    let mut io = IoState::new();
    let noop = command(registers, capabilities, base, memory, &mut io, 0, 23 << 10)?;
    if noop.code != SUCCESS_COMPLETION as u8 {
        return Err(CommandError::WrongCompletionCode);
    }
    let first_pages = xhci_session::FIRST_DEVICE;
    let second_pages = xhci_session::SECOND_DEVICE;
    let first = read_descriptor(
        registers,
        capabilities,
        base,
        memory,
        first_pages,
        first_port,
        first_speed,
        &mut io,
    )?;
    // Командный и событийный курсоры продолжают движение независимо.
    let second = read_descriptor(
        registers,
        capabilities,
        base,
        memory,
        second_pages,
        second_port,
        second_speed,
        &mut io,
    )?;
    if first.slot_id == second.slot_id {
        return Err(CommandError::DuplicateSlot);
    }
    if !wait_halted(registers, operational, false) {
        return Err(CommandError::ControllerStopped);
    }
    // Повторная передача к ПЕРВОМУ устройству после полного перечисления второго.
    // Без новой адресации и без сброса, с живыми DCBAA и собственным кольцом EP0.
    configuration_transfer(
        registers,
        capabilities,
        base,
        memory,
        first_pages,
        first.slot_id,
        xhci_session::RECHECK_TRANSFER_TRB,
        usize::from(first.configuration_length),
        768,
        &mut io,
    )?;
    for index in 0..usize::from(first.configuration_length) {
        let previous = memory.read_byte(first_pages.descriptor, 128 + index);
        let repeated = memory.read_byte(first_pages.descriptor, 768 + index);
        if previous != repeated {
            return Err(CommandError::RetainedConfigurationChanged);
        }
    }
    if !wait_halted(registers, operational, false) {
        return Err(CommandError::ControllerStopped);
    }
    Ok((
        first,
        second,
        RetainedSlotsReport {
            first_slot: first.slot_id,
            second_slot: second.slot_id,
            rechecked_slot: first.slot_id,
            pages: PAGE_COUNT,
        },
        io,
    ))
}

/// Контроллер должен быть предварительно сброшен и остановлен.
///
/// # Безопасность
///
/// Вызывающий код гарантирует, что QEMU работает без IOMMU, выдаёт устройству
/// гостевые физические адреса и обеспечивает согласованность памяти x86.
/// При невозможности остановить/сбросить контроллер страницы намеренно
/// остаются закреплёнными: освобождать их небезопасно.
pub unsafe fn start_pair<const N: usize, R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    frames: &mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
    first_port: u8,
    first_speed: u8,
    second_port: u8,
    second_speed: u8,
) -> Result<(CommandReport, CommandReport, RetainedSlotsReport, ActiveDma), CommandError> {
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

    let result = issue_enumeration(
        registers,
        capabilities,
        base,
        &memory,
        first_port,
        first_speed,
        second_port,
        second_speed,
    );

    // При ошибке инициализации обязательно останавливаем аппаратный DMA
    // перед возвратом страниц. При ошибке остановки память остаётся занятой.
    let (first_report, second_report, retained, io) = match result {
        Ok(reports) => reports,
        Err(error) => {
            xhci_control::stop_and_reset(
                registers,
                capabilities.operational_offset,
                xhci_control::MAX_POLLS,
            )
            .map_err(|_| CommandError::UnsafeShutdown)?;
            device_mapping
                .close(&mut backend)
                .map_err(|_| CommandError::DeviceUnmapFailed)?;
            frames
                .release_contiguous_4k(first, PAGE_COUNT)
                .map_err(|_| CommandError::FrameReleaseFailed)?;
            return Err(error);
        }
    };

    // В отличие от прежнего диагностического вызова здесь нет остановки:
    // страницами, отображением и курсором событий владеет ActiveDma.
    Ok((
        first_report,
        second_report,
        retained,
        ActiveDma {
            first,
            memory,
            base,
            backend,
            device_mapping,
            io,
            stopped: false,
            released: false,
            faulted: false,
        },
    ))
}

/// Кодирует завершающий управляющий запрос SET_CONFIGURATION через EP0.
/// Для короткого управляющего запроса нужны Setup и Status TRB.
fn set_configuration<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    io: &mut IoState,
    pages: DevicePages,
    slot: u8,
    index: usize,
    configuration: u8,
) -> Result<(), CommandError> {
    if configuration == 0 || index + 2 >= RING_ITEMS as usize - 1 {
        return Err(CommandError::EndpointRingFull);
    }
    let offset = index * 16;
    let setup = u32::from(configuration) << 16 | (9 << 8);
    memory.write(pages.transfer, offset, setup);
    memory.write(pages.transfer, offset + 4, 0);
    memory.write(pages.transfer, offset + 8, 8);
    memory.write(pages.transfer, offset + 12, (2 << 10) | (1 << 6) | 1);
    memory.write(pages.transfer, offset + 16, 0);
    memory.write(pages.transfer, offset + 20, 0);
    memory.write(pages.transfer, offset + 24, 0);
    memory.write(
        pages.transfer,
        offset + 28,
        (4 << 10) | (1 << 16) | (1 << 5) | 1,
    );
    compiler_fence(Ordering::SeqCst);
    registers.write_u32(capabilities.doorbell_offset + u64::from(slot) * 4, 1);
    let event = await_completion(
        registers,
        capabilities.runtime_offset + 0x20,
        base,
        memory,
        io,
    )?;
    let expected = base + pages.transfer * PAGE_BYTES + offset as u64 + 16;
    if event.kind != 32 || event.slot != slot || event.pointer != expected || event.code != 1 {
        return Err(CommandError::InvalidTransferEvent);
    }
    Ok(())
}

fn interrupt_interval(microframes: u8, speed: u8) -> u32 {
    if speed > 2 {
        return u32::from(microframes.saturating_sub(1)).min(15);
    }
    let periods = u32::from(microframes).saturating_mul(8).max(1);
    31 - periods.leading_zeros()
}

fn configure_interrupt_endpoint<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    io: &mut IoState,
    pages: DevicePages,
    slot: u8,
    hid: HidConfiguration,
) -> Result<(), CommandError> {
    let endpoint_number = u32::from(hid.endpoint_address & 0x0f);
    let dci = endpoint_number * 2 + 1;
    if dci > 31 {
        return Err(CommandError::InvalidEndpoint);
    }
    let context = usize::from(capabilities.context_size_bytes);
    let input_slot = context;
    let output_slot = pages.output;
    for word in 0..context / 4 {
        let value = memory.read(output_slot, word * 4);
        memory.write(pages.input, input_slot + word * 4, value);
    }
    let slot_flags = memory.read(pages.input, input_slot);
    memory.write(pages.input, input_slot, (slot_flags & !(0x1f << 27)) | (dci << 27));
    memory.write(pages.input, 0, 0);
    memory.write(pages.input, 4, 1 | (1 << dci));

    let ring = base + pages.descriptor * PAGE_BYTES + HID_RING_START as u64;
    let link_offset = (HID_RING_ITEMS - 1) * 16 + HID_RING_START;
    memory.write(pages.descriptor, link_offset, ring as u32);
    memory.write(pages.descriptor, link_offset + 4, (ring >> 32) as u32);
    memory.write(pages.descriptor, link_offset + 8, 0);
    memory.write(pages.descriptor, link_offset + 12, LINK_TRB);

    let speed = ((slot_flags >> 20) & 0x0f) as u8;
    let interval = interrupt_interval(hid.interval, speed);
    let endpoint_offset = context * (dci as usize + 1);
    memory.write(pages.input, endpoint_offset, interval << 16);
    memory.write(
        pages.input,
        endpoint_offset + 4,
        (3 << 1) | (HID_ENDPOINT_TYPE << 3) | (u32::from(hid.max_packet_size) << 16),
    );
    memory.write(pages.input, endpoint_offset + 8, (ring | 1) as u32);
    memory.write(pages.input, endpoint_offset + 12, (ring >> 32) as u32);
    memory.write(
        pages.input,
        endpoint_offset + 16,
        u32::from(hid.max_packet_size) | (u32::from(hid.max_packet_size) << 16),
    );
    compiler_fence(Ordering::SeqCst);
    let event = command(
        registers,
        capabilities,
        base,
        memory,
        io,
        base + pages.input * PAGE_BYTES,
        (12 << 10) | (u32::from(slot) << 24),
    )?;
    if event.slot != slot {
        return Err(CommandError::InvalidSlot);
    }
    Ok(())
}

fn arm_hid<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    channel: &mut HidChannel,
) {
    let address = base + channel.pages.descriptor * PAGE_BYTES + HID_REPORT_OFFSET as u64;
    let trb = HID_RING_START + channel.index * 16;
    // Только один активный TRB на устройство: отчёт читается до повторной
    // выдачи того же буфера xHCI.
    for offset in (0..8).step_by(4) {
        memory.write(channel.pages.descriptor, HID_REPORT_OFFSET + offset, 0);
    }
    memory.write(channel.pages.descriptor, trb, address as u32);
    memory.write(channel.pages.descriptor, trb + 4, (address >> 32) as u32);
    memory.write(channel.pages.descriptor, trb + 8, u32::from(channel.descriptor.max_packet_size));
    memory.write(channel.pages.descriptor, trb + 12, (1 << 10) | (1 << 5) | channel.cycle);
    channel.outstanding = base + channel.pages.descriptor * PAGE_BYTES + trb as u64;
    channel.index += 1;
    if channel.index == HID_RING_ITEMS - 1 {
        let link = HID_RING_START + (HID_RING_ITEMS - 1) * 16 + 12;
        memory.write(channel.pages.descriptor, link, (6 << 10) | 2 | channel.cycle);
        channel.index = 0;
        channel.cycle ^= 1;
    }
    compiler_fence(Ordering::SeqCst);
    let dci = u32::from(channel.descriptor.endpoint_address & 0x0f) * 2 + 1;
    registers.write_u32(capabilities.doorbell_offset + u64::from(channel.slot) * 4, dci);
}

fn completed_hid<R: OperationalRegisters>(
    registers: &R,
    capabilities: XhciCapabilities,
    base: u64,
    memory: &GuestMemory,
    io: &mut IoState,
    event: Completion,
) -> Result<HidRawReport, CommandError> {
    let index = hid_transfer_index(io, event).ok_or(CommandError::InvalidHidEvent)?;
    let mut channel = io.hid[index].ok_or(CommandError::InvalidHidEvent)?;
    if event.code != 1 && event.code != 13 {
        return Err(CommandError::WrongCompletionCode);
    }
    let transfer_size = u32::from(channel.descriptor.max_packet_size);
    if event.remaining > transfer_size {
        return Err(CommandError::InvalidHidEvent);
    }
    let received = (transfer_size - event.remaining) as usize;
    if received > 8 {
        return Err(CommandError::InvalidHidEvent);
    }
    let mut bytes = [0_u8; 8];
    for (offset, byte) in bytes.iter_mut().take(received).enumerate() {
        *byte = memory.read_byte(channel.pages.descriptor, HID_REPORT_OFFSET + offset);
    }
    let report = HidRawReport {
        slot: channel.slot,
        protocol: channel.descriptor.protocol,
        length: received as u8,
        bytes,
    };
    arm_hid(registers, capabilities, base, memory, &mut channel);
    io.hid[index] = Some(channel);
    Ok(report)
}

/// Владелец закреплённых DMA-страниц живого контроллера xHCI.
///
/// Нельзя освободить страницы посредством Drop: прежде требуется
/// аппаратно подтвердить остановку и снятие адресного отображения.
pub struct ActiveDma {
    first: PhysFrame<Size4KiB>,
    memory: GuestMemory,
    base: u64,
    backend: QemuDirectDma<1>,
    device_mapping: DeviceMapping,
    io: IoState,
    stopped: bool,
    released: bool,
    faulted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePortEvent {
    pub port: u8,
    pub completion_code: u8,
}

/// Считывает одно новое событие, если оно появилось.
/// Сейчас принимаются только извещения об изменении состояния порта.
/// Управление подключением и отключением слотов — отдельная задача.
impl ActiveDma {
    pub fn poll_port_event<R: OperationalRegisters>(
        &mut self,
        registers: &R,
        capabilities: XhciCapabilities,
    ) -> Result<Option<RuntimePortEvent>, CommandError> {
        if self.stopped {
            return Err(CommandError::AlreadyStopped);
        }
        if !wait_halted(registers, capabilities.operational_offset, false) {
            return Err(CommandError::ControllerStopped);
        }
        if let Some(event) = self.io.take_port() {
            return Ok(Some(event));
        }
        let runtime = capabilities.runtime_offset + 0x20;
        let Some(event) = poll_event(registers, runtime, self.base, &self.memory, &mut self.io)
        else {
            return Ok(None);
        };
        if event.kind != 34 {
            return Err(CommandError::UnexpectedRuntimeEvent(event.kind));
        }
        let port = (event.pointer >> 24) as u8;
        if port == 0 || port > capabilities.max_ports {
            return Err(CommandError::InvalidPortEvent);
        }
        Ok(Some(RuntimePortEvent {
            port,
            completion_code: event.code,
        }))
    }

    /// Освобождает адресованный слот в действующем контроллере.
    /// Контексты и буфер остаются закреплёнными до общего shutdown.
    pub fn disable_slot<R: OperationalRegisters>(
        &mut self,
        registers: &R,
        capabilities: XhciCapabilities,
        slot: u8,
    ) -> Result<(), CommandError> {
        if self.stopped || self.released {
            return Err(CommandError::AlreadyStopped);
        }
        if self.faulted {
            return Err(CommandError::ServiceFaulted);
        }
        if slot == 0 || slot > capabilities.max_slots {
            return Err(CommandError::InvalidSlot);
        }
        let result = command(
            registers,
            capabilities,
            self.base,
            &self.memory,
            &mut self.io,
            0,
            (10 << 10) | (u32::from(slot) << 24),
        );
        let event = match result {
            Ok(event) if event.slot == slot => event,
            Ok(_) => {
                self.faulted = true;
                return Err(CommandError::InvalidSlot);
            }
            Err(error) => {
                self.faulted = true;
                return Err(error);
            }
        };
        debug_assert_eq!(event.slot, slot);
        for index in 0..self.io.hid.len() {
            if self.io.hid[index].is_some_and(|channel| channel.slot == slot) {
                self.io.hid[index] = None;
                self.io.pending_hid[index] = None;
            }
        }
        // Аппаратное подтверждение Disable Slot означает отсутствие
        // дальнейшего доступа xHCI к выходному контексту этого слота.
        self.memory.write(0, usize::from(slot) * 8, 0);
        self.memory.write(0, usize::from(slot) * 8 + 4, 0);
        Ok(())
    }

    /// Повторно адресует новое устройство через освобождённый комплект
    /// контекстов. Выделение физических страниц не повторяется.
    pub fn attach_device<R: OperationalRegisters>(
        &mut self,
        registers: &R,
        capabilities: XhciCapabilities,
        device_index: usize,
        port: u8,
        speed: u8,
    ) -> Result<CommandReport, CommandError> {
        if self.stopped || self.released {
            return Err(CommandError::AlreadyStopped);
        }
        if self.faulted {
            return Err(CommandError::ServiceFaulted);
        }
        let pages = match device_index {
            0 => xhci_session::FIRST_DEVICE,
            1 => xhci_session::SECOND_DEVICE,
            _ => return Err(CommandError::NoFreeDevicePages),
        };
        // После подтверждённой команды Disable Slot отсутствуют обращения
        // контроллера к прежним контекстам; можно переиспользовать 4 страницы.
        for page in [pages.input, pages.output, pages.transfer, pages.descriptor] {
            unsafe {
                core::ptr::write_bytes(self.memory.address(page, 0), 0, (PAGE_BYTES / 4) as usize);
            }
        }
        let result = read_descriptor(
            registers,
            capabilities,
            self.base,
            &self.memory,
            pages,
            port,
            speed,
            &mut self.io,
        );
        if result.is_ok() {
            self.io.control_next[device_index] = 9;
        }
        if result.is_err() {
            // Ошибка после Enable Slot может оставить в устройстве
            // незавершённые аппаратные обращения. До общего shutdown
            // запрещено отдавать этот комплект страниц другому USB.
            self.faulted = true;
        }
        result
    }

    pub fn is_mapped(&self) -> bool {
        self.device_mapping.is_mapped() && !self.released
    }

    /// После подтверждения остановки возвращает DMA в системный распределитель.
    /// Повторное освобождение запрещено, ошибка снятия оставляет владение
    /// для следующей попытки без повторного доступа контроллера к памяти.
    pub fn shutdown<const N: usize, R: OperationalRegisters>(
        &mut self,
        registers: &R,
        capabilities: XhciCapabilities,
        frames: &mut SystemFrameAllocator<N>,
    ) -> Result<(), CommandError> {
        if self.released {
            return Err(CommandError::AlreadyStopped);
        }
        if !self.stopped {
            xhci_control::stop_and_reset(
                registers,
                capabilities.operational_offset,
                xhci_control::MAX_POLLS,
            )
            .map_err(|_| CommandError::UnsafeShutdown)?;
            self.stopped = true;
        }
        if self.device_mapping.is_mapped() {
            self.device_mapping
                .close(&mut self.backend)
                .map_err(|_| CommandError::DeviceUnmapFailed)?;
        }
        let free_before = frames.free_frames();
        frames
            .release_contiguous_4k(self.first, PAGE_COUNT)
            .map_err(|_| CommandError::FrameReleaseFailed)?;
        self.released = true;
        assert_eq!(
            frames.free_frames(),
            free_before + PAGE_COUNT,
            "DMA-обмен xHCI не вернул все физические страницы"
        );
        Ok(())
    }
}
