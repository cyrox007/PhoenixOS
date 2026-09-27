use core::arch::{global_asm, x86_64::__cpuid};

use phoenix_syscall_abi::{SyscallRequest, SyscallReturn, SyscallStatus};

const IA32_EFER: u32 = 0xc000_0080;
const IA32_STAR: u32 = 0xc000_0081;
const IA32_LSTAR: u32 = 0xc000_0082;
const IA32_FMASK: u32 = 0xc000_0084;

const EFER_SCE: u64 = 1 << 0;
const RFLAGS_TRAP: u64 = 1 << 8;
const RFLAGS_INTERRUPT: u64 = 1 << 9;
const RFLAGS_DIRECTION: u64 = 1 << 10;
const SYSCALL_CPUID_BIT: u32 = 1 << 11;

const SELF_TEST_NUMBER: u64 = u64::MAX;
const SELF_TEST_ARGUMENTS: [u64; 6] = [
    0x11,
    0x2233,
    0x4455_6677,
    0x8899_aabb_ccdd_eeff,
    0x1234_5678_9abc_def0,
    0xfedc_ba98_7654_3210,
];
const SELF_TEST_RESULT: u64 = 0x5359_5343_414c_4c21;
const STATUS_UNKNOWN_CALL: u32 = 1;
const STATUS_BAD_ARGUMENTS: u32 = 2;
const ENTRY_STACK_SIZE: u64 = 64 * 1024;

global_asm!(
    r#"
    .pushsection .bss
    .balign 16
    .global phoenix_syscall_entry_stack
phoenix_syscall_entry_stack:
    .skip 65536
    .global phoenix_syscall_entry_stack_end
phoenix_syscall_entry_stack_end:
    .global phoenix_syscall_saved_rsp
phoenix_syscall_saved_rsp:
    .quad 0
    .global phoenix_syscall_observed_rsp
phoenix_syscall_observed_rsp:
    .quad 0
    .popsection

    .global phoenix_syscall_entry
    .type phoenix_syscall_entry,@function
phoenix_syscall_entry:
    mov [rip + phoenix_syscall_saved_rsp], rsp
    lea rsp, [rip + phoenix_syscall_entry_stack_end]
    mov [rip + phoenix_syscall_observed_rsp], rsp

    push rcx
    push r11
    sub rsp, 80

    mov [rsp + 0], rax
    mov [rsp + 8], rdi
    mov [rsp + 16], rsi
    mov [rsp + 24], rdx
    mov [rsp + 32], r10
    mov [rsp + 40], r8
    mov [rsp + 48], r9

    lea rdi, [rsp]
    lea rsi, [rsp + 64]
    call phoenix_syscall_dispatch

    mov rax, [rsp + 64]
    mov rdx, [rsp + 72]

    add rsp, 80
    pop r11
    pop rcx
    push r11
    popfq
    mov rsp, [rip + phoenix_syscall_saved_rsp]
    jmp rcx
    .size phoenix_syscall_entry, .-phoenix_syscall_entry

    .global phoenix_syscall_kernel_test_invoke
    .type phoenix_syscall_kernel_test_invoke,@function
phoenix_syscall_kernel_test_invoke:
    push rbx
    push r12
    push rbp

    mov rbx, rdi
    mov r12, rsi

    mov rax, [rbx + 0]
    mov rdi, [rbx + 8]
    mov rsi, [rbx + 16]
    mov rdx, [rbx + 24]
    mov r10, [rbx + 32]
    mov r8, [rbx + 40]
    mov r9, [rbx + 48]

    syscall

    mov [r12 + 0], rax
    mov [r12 + 8], rdx

    pop rbp
    pop r12
    pop rbx
    ret
    .size phoenix_syscall_kernel_test_invoke, .-phoenix_syscall_kernel_test_invoke
"#
);

unsafe extern "C" {
    static phoenix_syscall_entry_stack: u8;
    static phoenix_syscall_entry_stack_end: u8;
    static phoenix_syscall_saved_rsp: u64;
    static phoenix_syscall_observed_rsp: u64;

    fn phoenix_syscall_entry();
    fn phoenix_syscall_kernel_test_invoke(
        request: *const SyscallRequest,
        result: *mut SyscallReturn,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    Unsupported,
    InvalidSelectors,
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
    let user_selector_base = user_return_selector_base().ok_or(InitError::InvalidSelectors)?;
    let star = (u64::from(user_selector_base) << 48) | (kernel_code_selector << 32);
    let lstar = phoenix_syscall_entry as *const () as u64;
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

pub fn user_return_selectors_valid(state: SyscallMsrState) -> bool {
    let user_code = super::gdt::user_code_selector_raw();
    let user_data = super::gdt::user_data_selector_raw();
    let selector_base = (state.star >> 48) as u16;

    if user_code & 0b11 != 0b11 || user_data & 0b11 != 0b11 {
        return false;
    }

    let expected_data = selector_base.wrapping_add(8) | 0b11;
    let expected_code = selector_base.wrapping_add(16) | 0b11;

    user_data == expected_data && user_code == expected_code
}

fn user_return_selector_base() -> Option<u16> {
    let user_code = super::gdt::user_code_selector_raw() & !0b11;
    let user_data = super::gdt::user_data_selector_raw() & !0b11;
    let base = user_code.checked_sub(16)?;

    if base.checked_add(8)? != user_data {
        return None;
    }

    Some(base)
}

pub fn entry_self_test() -> bool {
    let request = SyscallRequest::new(SELF_TEST_NUMBER, SELF_TEST_ARGUMENTS);
    let mut result = SyscallReturn::success(0);

    unsafe {
        phoenix_syscall_kernel_test_invoke(&request, &mut result);
    }

    result.is_success() && result.value == SELF_TEST_RESULT
}

pub fn entry_stack_self_test() -> bool {
    let saved_rsp = unsafe { phoenix_syscall_saved_rsp };
    let observed_rsp = unsafe { phoenix_syscall_observed_rsp };
    let stack_start = core::ptr::addr_of!(phoenix_syscall_entry_stack) as u64;
    let stack_end = core::ptr::addr_of!(phoenix_syscall_entry_stack_end) as u64;

    if stack_end.saturating_sub(stack_start) != ENTRY_STACK_SIZE {
        return false;
    }

    if saved_rsp == 0 || observed_rsp == 0 || saved_rsp == observed_rsp {
        return false;
    }

    observed_rsp >= stack_start && observed_rsp <= stack_end && observed_rsp & 0xf == 0
}

#[unsafe(no_mangle)]
extern "C" fn phoenix_syscall_dispatch(request: *const SyscallRequest, result: *mut SyscallReturn) {
    if request.is_null() || result.is_null() {
        return;
    }

    let request = unsafe { *request };
    let response = dispatch(request);

    unsafe {
        result.write(response);
    }
}

fn dispatch(request: SyscallRequest) -> SyscallReturn {
    if request.number != SELF_TEST_NUMBER {
        return syscall_failure(STATUS_UNKNOWN_CALL);
    }

    if request.arguments != SELF_TEST_ARGUMENTS {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    SyscallReturn::success(SELF_TEST_RESULT)
}

fn syscall_failure(code: u32) -> SyscallReturn {
    let Some(status) = SyscallStatus::from_error_code(code) else {
        return SyscallReturn::success(0);
    };

    SyscallReturn::failure(status)
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
