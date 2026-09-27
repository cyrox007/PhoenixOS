use core::arch::x86_64::__cpuid;
use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::instructions::interrupts;

pub const TIMER_VECTOR: u8 = 0xe0;

const IA32_APIC_BASE: u32 = 0x1b;
const IA32_X2APIC_EOI: u32 = 0x80b;
const IA32_X2APIC_LVT_TIMER: u32 = 0x832;
const IA32_X2APIC_INITIAL_COUNT: u32 = 0x838;
const IA32_X2APIC_DIVIDE_CONFIGURATION: u32 = 0x83e;

const APIC_GLOBAL_ENABLE: u64 = 1 << 11;
const X2APIC_ENABLE: u64 = 1 << 10;
const TIMER_PERIODIC_MODE: u64 = 1 << 17;
const TIMER_DIVIDE_BY_ONE: u64 = 0b1011;
const TIMER_INITIAL_COUNT: u64 = 10_000_000;

static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    LocalApicUnavailable,
    X2ApicUnavailable,
}

pub fn init_periodic_timer() -> Result<(), InitError> {
    verify_capabilities()?;

    interrupts::without_interrupts(|| unsafe {
        enable_x2apic();
        write_msr(IA32_X2APIC_DIVIDE_CONFIGURATION, TIMER_DIVIDE_BY_ONE);
        write_msr(
            IA32_X2APIC_LVT_TIMER,
            u64::from(TIMER_VECTOR) | TIMER_PERIODIC_MODE,
        );
        write_msr(IA32_X2APIC_INITIAL_COUNT, TIMER_INITIAL_COUNT);
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

pub(super) fn handle_timer_interrupt() {
    TIMER_TICKS.fetch_add(1, Ordering::Relaxed);

    unsafe {
        write_msr(IA32_X2APIC_EOI, 0);
    }
}

fn verify_capabilities() -> Result<(), InitError> {
    let features = unsafe { __cpuid(1) };

    if features.edx & (1 << 9) == 0 {
        return Err(InitError::LocalApicUnavailable);
    }

    if features.ecx & (1 << 21) == 0 {
        return Err(InitError::X2ApicUnavailable);
    }

    Ok(())
}

unsafe fn enable_x2apic() {
    let current = unsafe { read_msr(IA32_APIC_BASE) };
    let enabled = current | APIC_GLOBAL_ENABLE | X2APIC_ENABLE;
    unsafe { write_msr(IA32_APIC_BASE, enabled) };
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
