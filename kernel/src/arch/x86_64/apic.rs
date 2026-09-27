use core::arch::x86_64::__cpuid;
use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use phoenix_memory::SystemFrameAllocator;
use phoenix_vm::ActivePageTable;
use x86_64::instructions::interrupts;
use x86_64::instructions::port::Port;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

pub const TIMER_VECTOR: u8 = 0xe0;
pub const SPURIOUS_VECTOR: u8 = 0xff;

const IA32_APIC_BASE: u32 = 0x1b;
const IA32_X2APIC_EOI: u32 = 0x80b;
const IA32_X2APIC_SPURIOUS_VECTOR: u32 = 0x80f;
const IA32_X2APIC_LVT_TIMER: u32 = 0x832;
const IA32_X2APIC_INITIAL_COUNT: u32 = 0x838;
const IA32_X2APIC_DIVIDE_CONFIGURATION: u32 = 0x83e;

const XAPIC_EOI: u64 = 0x0b0;
const XAPIC_SPURIOUS_VECTOR: u64 = 0x0f0;
const XAPIC_LVT_TIMER: u64 = 0x320;
const XAPIC_INITIAL_COUNT: u64 = 0x380;
const XAPIC_DIVIDE_CONFIGURATION: u64 = 0x3e0;
const XAPIC_VIRTUAL_BASE: u64 = 0x0000_6000_1000_0000;
const APIC_PHYSICAL_BASE_MASK: u64 = 0x000f_ffff_ffff_f000;

const APIC_GLOBAL_ENABLE: u64 = 1 << 11;
const X2APIC_ENABLE: u64 = 1 << 10;
const APIC_SOFTWARE_ENABLE: u64 = 1 << 8;
const TIMER_PERIODIC_MODE: u64 = 1 << 17;
const TIMER_DIVIDE_BY_ONE: u64 = 0b1011;
const TIMER_INITIAL_COUNT: u64 = 10_000_000;

const MODE_UNINITIALIZED: u8 = 0;
const MODE_XAPIC: u8 = 1;
const MODE_X2APIC: u8 = 2;

static APIC_MODE: AtomicU8 = AtomicU8::new(MODE_UNINITIALIZED);
static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static TIMER_HOOK: AtomicUsize = AtomicUsize::new(0);

pub type TimerHook = fn(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerHookError {
    AlreadyInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApicMode {
    XApic,
    X2Apic,
}

impl ApicMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::XApic => "xAPIC",
            Self::X2Apic => "x2APIC",
        }
    }
}

#[derive(Debug)]
pub enum InitError {
    LocalApicUnavailable,
    MmioVirtualAddressOccupied,
    Map(MapToError<Size4KiB>),
}

pub fn init_periodic_timer<const MAX_RANGES: usize>(
    page_table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<MAX_RANGES>,
) -> Result<ApicMode, InitError> {
    let features = __cpuid(1);
    if features.edx & (1 << 9) == 0 {
        return Err(InitError::LocalApicUnavailable);
    }

    interrupts::without_interrupts(|| unsafe {
        mask_legacy_pic();
    });

    if features.ecx & (1 << 21) != 0 {
        interrupts::without_interrupts(|| unsafe {
            init_x2apic();
        });
        APIC_MODE.store(MODE_X2APIC, Ordering::Release);
        return Ok(ApicMode::X2Apic);
    }

    init_xapic(page_table, frames)?;
    APIC_MODE.store(MODE_XAPIC, Ordering::Release);
    Ok(ApicMode::XApic)
}

pub fn ticks() -> u64 {
    TIMER_TICKS.load(Ordering::Relaxed)
}

pub fn wait_for_ticks(minimum: u64) {
    while ticks() < minimum {
        interrupts::enable_and_hlt();
    }
}

pub fn install_timer_hook(hook: TimerHook) -> Result<(), TimerHookError> {
    let raw = hook as usize;
    let result = TIMER_HOOK.compare_exchange(0, raw, Ordering::AcqRel, Ordering::Acquire);

    match result {
        Ok(_) => Ok(()),
        Err(_) => Err(TimerHookError::AlreadyInstalled),
    }
}

pub fn remove_timer_hook() {
    TIMER_HOOK.store(0, Ordering::Release);
}

pub(super) fn handle_timer_interrupt() {
    let tick = TIMER_TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    end_of_interrupt();
    notify_timer_hook(tick);
}

fn end_of_interrupt() {
    match APIC_MODE.load(Ordering::Acquire) {
        MODE_XAPIC => unsafe {
            write_xapic(XAPIC_EOI, 0);
        },
        MODE_X2APIC => unsafe {
            write_msr(IA32_X2APIC_EOI, 0);
        },
        _ => {}
    }
}

fn notify_timer_hook(tick: u64) {
    let raw = TIMER_HOOK.load(Ordering::Acquire);
    if raw == 0 {
        return;
    }

    let hook: TimerHook = unsafe { core::mem::transmute(raw) };
    hook(tick);
}

fn init_xapic<const MAX_RANGES: usize>(
    page_table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<MAX_RANGES>,
) -> Result<(), InitError> {
    let apic_base = unsafe { ensure_apic_global_enable() };
    let physical_base = apic_base & APIC_PHYSICAL_BASE_MASK;

    let virtual_address = VirtAddr::new(XAPIC_VIRTUAL_BASE);
    if page_table.translate_addr(virtual_address).is_some() {
        return Err(InitError::MmioVirtualAddressOccupied);
    }

    let page = Page::<Size4KiB>::containing_address(virtual_address);
    let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(physical_base));

    unsafe {
        page_table
            .map_4k(
                page,
                frame,
                PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE | PageTableFlags::NO_CACHE,
                frames,
            )
            .map_err(InitError::Map)?;
    }

    interrupts::without_interrupts(|| unsafe {
        write_xapic(
            XAPIC_SPURIOUS_VECTOR,
            APIC_SOFTWARE_ENABLE as u32 | u32::from(SPURIOUS_VECTOR),
        );
        write_xapic(XAPIC_DIVIDE_CONFIGURATION, TIMER_DIVIDE_BY_ONE as u32);
        write_xapic(
            XAPIC_LVT_TIMER,
            TIMER_PERIODIC_MODE as u32 | u32::from(TIMER_VECTOR),
        );
        write_xapic(XAPIC_INITIAL_COUNT, TIMER_INITIAL_COUNT as u32);
    });

    Ok(())
}

unsafe fn init_x2apic() {
    let current = unsafe { ensure_apic_global_enable() };

    if current & X2APIC_ENABLE == 0 {
        unsafe {
            write_msr(IA32_APIC_BASE, current | X2APIC_ENABLE);
        }
    }

    unsafe {
        write_msr(
            IA32_X2APIC_SPURIOUS_VECTOR,
            APIC_SOFTWARE_ENABLE | u64::from(SPURIOUS_VECTOR),
        );
        write_msr(IA32_X2APIC_DIVIDE_CONFIGURATION, TIMER_DIVIDE_BY_ONE);
        write_msr(
            IA32_X2APIC_LVT_TIMER,
            TIMER_PERIODIC_MODE | u64::from(TIMER_VECTOR),
        );
        write_msr(IA32_X2APIC_INITIAL_COUNT, TIMER_INITIAL_COUNT);
    }
}

unsafe fn ensure_apic_global_enable() -> u64 {
    let current = unsafe { read_msr(IA32_APIC_BASE) };
    if current & APIC_GLOBAL_ENABLE != 0 {
        return current;
    }

    let enabled = current | APIC_GLOBAL_ENABLE;
    unsafe {
        write_msr(IA32_APIC_BASE, enabled);
    }
    enabled
}

unsafe fn mask_legacy_pic() {
    let mut primary_mask = Port::<u8>::new(0x21);
    let mut secondary_mask = Port::<u8>::new(0xa1);

    unsafe {
        primary_mask.write(0xff);
        secondary_mask.write(0xff);
    }
}

unsafe fn write_xapic(offset: u64, value: u32) {
    let address = XAPIC_VIRTUAL_BASE + offset;
    let register = address as *mut u32;

    unsafe {
        register.write_volatile(value);
    }
}

unsafe fn read_msr(register: u32) -> u64 {
    let low: u32;
    let high: u32;

    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") register,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }

    (u64::from(high) << 32) | u64::from(low)
}

unsafe fn write_msr(register: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;

    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") register,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
}
