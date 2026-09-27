use core::arch::{global_asm, x86_64::__cpuid};
use core::sync::atomic::{AtomicU64, Ordering};

use phoenix_syscall_abi::{SyscallRequest, SyscallReturn, SyscallStatus};

const IA32_EFER: u32 = 0xc000_0080;
const IA32_STAR: u32 = 0xc000_0081;
const IA32_LSTAR: u32 = 0xc000_0082;
const IA32_FMASK: u32 = 0xc000_0084;

const EFER_SCE: u64 = 1 << 0;
const RFLAGS_TRAP: u64 = 1 << 8;
const RFLAGS_INTERRUPT: u64 = 1 << 9;
const RFLAGS_DIRECTION: u64 = 1 << 10;
const RFLAGS_RESERVED_ONE: u64 = 1 << 1;
const RFLAGS_IOPL: u64 = 0b11 << 12;
const RFLAGS_NESTED_TASK: u64 = 1 << 14;
const RFLAGS_RESUME: u64 = 1 << 16;
const RFLAGS_VIRTUAL_8086: u64 = 1 << 17;
const RFLAGS_FORBIDDEN_USER_RETURN: u64 =
    RFLAGS_IOPL | RFLAGS_NESTED_TASK | RFLAGS_RESUME | RFLAGS_VIRTUAL_8086;
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
const USER_SELF_TEST_EXIT_NUMBER: u64 = u64::MAX - 1;
const USER_SELF_TEST_SUCCESS: u64 = 0x5553_4552_5f4f_4b21;
const USER_SELF_TEST_FAILURE: u64 = 0x5553_4552_5f42_4144;
const STATUS_UNKNOWN_CALL: u32 = 1;
const STATUS_BAD_ARGUMENTS: u32 = 2;
const ENTRY_STACK_SIZE: u64 = 64 * 1024;

static USER_SELF_TEST_RESULT: AtomicU64 = AtomicU64::new(0);

#[unsafe(export_name = "phoenix_user_test_active")]
static mut USER_TEST_ACTIVE: u64 = 0;

#[unsafe(export_name = "phoenix_user_test_kernel_rsp")]
static mut USER_TEST_KERNEL_RSP: u64 = 0;

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

    cmp qword ptr [rsp + 0], -2
    jne 1f
    cmp qword ptr [rip + phoenix_user_test_active], 1
    je phoenix_user_test_return_from_syscall
1:
    mov rax, [rsp + 64]
    mov rdx, [rsp + 72]

    add rsp, 80
    pop r11
    pop rcx

    cmp qword ptr [rip + phoenix_user_test_active], 1
    je 2f

    push r11
    popfq
    mov rsp, [rip + phoenix_syscall_saved_rsp]
    jmp rcx

2:
    mov rsp, [rip + phoenix_syscall_saved_rsp]
    sysretq
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

    .global phoenix_user_test_enter
    .type phoenix_user_test_enter,@function
phoenix_user_test_enter:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15

    mov [rip + phoenix_user_test_kernel_rsp], rsp
    mov qword ptr [rip + phoenix_user_test_active], 1

    push rcx
    push rsi
    push r8
    push rdx
    push rdi
    iretq
    .size phoenix_user_test_enter, .-phoenix_user_test_enter

phoenix_user_test_return_from_syscall:
    mov qword ptr [rip + phoenix_user_test_active], 0
    mov rsp, [rip + phoenix_user_test_kernel_rsp]

    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    ret

    .global phoenix_user_test_image_start
    .global phoenix_user_test_image_end
phoenix_user_test_image_start:
    mov rax, -1
    mov rdi, 0x11
    mov rsi, 0x2233
    mov rdx, 0x44556677
    mov r10, 0x8899aabbccddeeff
    mov r8, 0x123456789abcdef0
    mov r9, 0xfedcba9876543210
    syscall

    test rdx, rdx
    jne 3f
    mov rbx, 0x53595343414c4c21
    cmp rax, rbx
    jne 3f

    mov ax, cs
    and eax, 3
    cmp eax, 3
    jne 3f

    mov ax, ss
    and eax, 3
    cmp eax, 3
    jne 3f

    mov rdi, 0x555345525f4f4b21
    jmp 4f

3:
    mov rdi, 0x555345525f424144

4:
    mov rax, -2
    syscall
    ud2
phoenix_user_test_image_end:
"#
);

unsafe extern "C" {
    static phoenix_syscall_entry_stack: u8;
    static phoenix_syscall_entry_stack_end: u8;
    static phoenix_syscall_saved_rsp: u64;
    static phoenix_syscall_observed_rsp: u64;
    static phoenix_user_test_image_start: u8;
    static phoenix_user_test_image_end: u8;

    fn phoenix_syscall_entry();
    fn phoenix_syscall_kernel_test_invoke(
        request: *const SyscallRequest,
        result: *mut SyscallReturn,
    );
    fn phoenix_user_test_enter(
        instruction_pointer: u64,
        stack_pointer: u64,
        code_selector: u64,
        data_selector: u64,
        cpu_flags: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserReturnError {
    NullInstructionPointer,
    NullStackPointer,
    InstructionPointerOutsideUserSpace,
    StackPointerOutsideUserSpace,
    ForbiddenFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserReturnContext {
    pub instruction_pointer: u64,
    pub stack_pointer: u64,
    pub cpu_flags: u64,
}

impl UserReturnContext {
    pub fn new(
        instruction_pointer: u64,
        stack_pointer: u64,
        cpu_flags: u64,
    ) -> Result<Self, UserReturnError> {
        if instruction_pointer == 0 {
            return Err(UserReturnError::NullInstructionPointer);
        }

        if stack_pointer == 0 {
            return Err(UserReturnError::NullStackPointer);
        }

        if instruction_pointer >= phoenix_vm::USER_SPACE_END_EXCLUSIVE {
            return Err(UserReturnError::InstructionPointerOutsideUserSpace);
        }

        if stack_pointer >= phoenix_vm::USER_SPACE_END_EXCLUSIVE {
            return Err(UserReturnError::StackPointerOutsideUserSpace);
        }

        if cpu_flags & RFLAGS_FORBIDDEN_USER_RETURN != 0 {
            return Err(UserReturnError::ForbiddenFlags);
        }

        Ok(Self {
            instruction_pointer,
            stack_pointer,
            cpu_flags: cpu_flags | RFLAGS_RESERVED_ONE,
        })
    }
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

pub fn user_mode_test_image() -> &'static [u8] {
    let start = core::ptr::addr_of!(phoenix_user_test_image_start) as usize;
    let end = core::ptr::addr_of!(phoenix_user_test_image_end) as usize;
    let length = end.saturating_sub(start);

    unsafe { core::slice::from_raw_parts(start as *const u8, length) }
}

pub fn run_user_mode_self_test(instruction_pointer: u64, stack_pointer: u64) -> bool {
    let Ok(context) = UserReturnContext::new(
        instruction_pointer,
        stack_pointer,
        RFLAGS_RESERVED_ONE,
    ) else {
        return false;
    };

    USER_SELF_TEST_RESULT.store(0, Ordering::Release);

    unsafe {
        phoenix_user_test_enter(
            context.instruction_pointer,
            context.stack_pointer,
            u64::from(super::gdt::user_code_selector_raw()),
            u64::from(super::gdt::user_data_selector_raw()),
            context.cpu_flags,
        );
    }

    USER_SELF_TEST_RESULT.load(Ordering::Acquire) == USER_SELF_TEST_SUCCESS
}

pub fn return_context_self_test() -> bool {
    let flags = RFLAGS_RESERVED_ONE | RFLAGS_INTERRUPT;
    let Ok(context) = UserReturnContext::new(0x4000_0000, 0x4000_2000, flags) else {
        return false;
    };

    if context.cpu_flags & RFLAGS_RESERVED_ONE == 0 {
        return false;
    }

    if UserReturnContext::new(0, 0x4000_2000, flags).is_ok() {
        return false;
    }

    if UserReturnContext::new(0x4000_0000, 0, flags).is_ok() {
        return false;
    }

    if UserReturnContext::new(phoenix_vm::USER_SPACE_END_EXCLUSIVE, 0x4000_2000, flags).is_ok() {
        return false;
    }

    if UserReturnContext::new(0x4000_0000, phoenix_vm::USER_SPACE_END_EXCLUSIVE, flags).is_ok() {
        return false;
    }

    UserReturnContext::new(0x4000_0000, 0x4000_2000, flags | RFLAGS_IOPL).is_err()
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
    if request.number == USER_SELF_TEST_EXIT_NUMBER {
        let result = request.arguments[0];
        if result != USER_SELF_TEST_SUCCESS && result != USER_SELF_TEST_FAILURE {
            return syscall_failure(STATUS_BAD_ARGUMENTS);
        }

        USER_SELF_TEST_RESULT.store(result, Ordering::Release);
        return SyscallReturn::success(0);
    }

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
