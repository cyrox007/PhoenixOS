use core::arch::x86_64::__cpuid;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use x86_64::instructions::interrupts;
use x86_64::instructions::port::Port;

pub const TIMER_VECTOR: u8 = 0xe0;
pub const SPURIOUS_VECTOR: u8 = 0xff;
pub const TIMER_REGISTER_CAPTURE_MARKER: u64 = 0x5449_4d52;

const IA32_APIC_BASE: u32 = 0x1b;
const IA32_X2APIC_EOI: u32 = 0x80b;
const IA32_X2APIC_SPURIOUS_VECTOR: u32 = 0x80f;
const IA32_X2APIC_LVT_TIMER: u32 = 0x832;
const IA32_X2APIC_INITIAL_COUNT: u32 = 0x838;
const IA32_X2APIC_DIVIDE_CONFIGURATION: u32 = 0x83e;

const APIC_GLOBAL_ENABLE: u64 = 1 << 11;
const X2APIC_ENABLE: u64 = 1 << 10;
const APIC_SOFTWARE_ENABLE: u64 = 1 << 8;
const TIMER_PERIODIC_MODE: u64 = 1 << 17;
const TIMER_DIVIDE_BY_ONE: u64 = 0b1011;
const TIMER_INITIAL_COUNT: u64 = 10_000_000;

static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static TIMER_HOOK: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerGeneralRegisters {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

impl TimerGeneralRegisters {
    pub const EMPTY: Self = Self {
        rax: 0,
        rbx: 0,
        rcx: 0,
        rdx: 0,
        rsi: 0,
        rdi: 0,
        rbp: 0,
        r8: 0,
        r9: 0,
        r10: 0,
        r11: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerInterruptContext {
    pub instruction_pointer: u64,
    pub stack_pointer: u64,
    pub cpu_flags: u64,
    pub general_registers: TimerGeneralRegisters,
    pub register_capture_marker: u64,
    pub stack_frame_address: u64,
}

pub type TimerHook = fn(u64, TimerInterruptContext) -> Option<u64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerHookError {
    AlreadyInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    X2ApicUnavailable,
}

pub fn init_periodic_timer() -> Result<(), InitError> {
    let features = __cpuid(1);
    if features.edx & (1 << 9) == 0 || features.ecx & (1 << 21) == 0 {
        return Err(InitError::X2ApicUnavailable);
    }

    interrupts::without_interrupts(|| unsafe {
        disable_legacy_pic();
        init_x2apic();
    });

    Ok(())
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

pub(super) fn handle_timer_interrupt(context: TimerInterruptContext) -> u64 {
    let tick = TIMER_TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    end_of_interrupt();
    notify_timer_hook(tick, context).unwrap_or(context.stack_frame_address)
}

fn end_of_interrupt() {
    unsafe {
        write_msr(IA32_X2APIC_EOI, 0);
    }
}

fn notify_timer_hook(tick: u64, context: TimerInterruptContext) -> Option<u64> {
    let raw = TIMER_HOOK.load(Ordering::Acquire);
    if raw == 0 {
        return None;
    }

    let hook: TimerHook = unsafe { core::mem::transmute(raw) };
    hook(tick, context)
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

unsafe fn disable_legacy_pic() {
    let mut primary_mask = Port::<u8>::new(0x21);
    let mut secondary_mask = Port::<u8>::new(0xa1);

    unsafe {
        primary_mask.write(0xff);
        secondary_mask.write(0xff);
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
