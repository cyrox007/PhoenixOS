use core::arch::{global_asm, x86_64::__cpuid};

const IA32_EFER: u32 = 0xc000_0080;
const IA32_STAR: u32 = 0xc000_0081;
const IA32_LSTAR: u32 = 0xc000_0082;
const IA32_FMASK: u32 = 0xc000_0084;

const EFER_SCE: u64 = 1 << 0;
const RFLAGS_TRAP: u64 = 1 << 8;
const RFLAGS_INTERRUPT: u64 = 1 << 9;
const RFLAGS_DIRECTION: u64 = 1 << 10;
const SYSCALL_CPUID_BIT: u32 = 1 << 11;

global_asm!(
    r#"
    .global phoenix_syscall_unreachable_entry
    .type phoenix_syscall_unreachable_entry,@function
phoenix_syscall_unreachable_entry:
    ud2
    .size phoenix_syscall_unreachable_entry, .-phoenix_syscall_unreachable_entry
"#
);

unsafe extern "C" {
    fn phoenix_syscall_unreachable_entry();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    Unsupported,
    VerificationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyscallMsrState {
    pub star: u64,
    pub lstar: u64,
    pub fmask: u64,
    pub efer: u64,
}

pub fn supported() -> bool {
    let maximum = unsafe { __cpuid(0x8000_0000) }.eax;
    if maximum < 0x8000_0001 {
        return false;
    }

    unsafe { __cpuid(0x8000_0001) }.edx & SYSCALL_CPUID_BIT != 0
}

pub fn init() -> Result<SyscallMsrState, InitError> {
    if !supported() {
        return Err(InitError::Unsupported);
    }

    let kernel_code_selector = u64::from(super::gdt::kernel_code_selector_raw());
    let star = kernel_code_selector << 32;
    let lstar = phoenix_syscall_unreachable_entry as *const () as u64;
    let fmask = RFLAGS_TRAP | RFLAGS_INTERRUPT | RFLAGS_DIRECTION;
    let efer = unsafe { read_msr(IA32_EFER) } | EFER_SCE;

    unsafe {
        write_msr(IA32_STAR, star);
        write_msr(IA32_LSTAR, lstar);
        write_msr(IA32_FMASK, fmask);
        write_msr(IA32_EFER, efer);
    }

    let state = SyscallMsrState {
        star: unsafe { read_msr(IA32_STAR) },
        lstar: unsafe { read_msr(IA32_LSTAR) },
        fmask: unsafe { read_msr(IA32_FMASK) },
        efer: unsafe { read_msr(IA32_EFER) },
    };

    if state.star != star
        || state.lstar != lstar
        || state.fmask != fmask
        || state.efer & EFER_SCE == 0
    {
        return Err(InitError::VerificationFailed);
    }

    Ok(state)
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
