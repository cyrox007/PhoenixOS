use core::arch::{global_asm, x86_64::__cpuid};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use phoenix_syscall_abi::{
    IpcReceiveArguments, IpcSendArguments, NO_TRANSFERRED_CAPABILITY, PackedCapabilityHandle,
    SYSCALL_IPC_RECEIVE, SYSCALL_IPC_SEND, SyscallRequest, SyscallReturn, SyscallStatus,
};
use phoenix_capability::{CapabilityError, CapabilityHandle, ObjectKind, Rights};
use phoenix_ipc::EndpointId;
use phoenix_process::ProcessCapabilitySet;

use crate::ipc_user_memory::{self, IpcUserMemoryError};
use crate::process_space::{PROCESS_REGION_CAPACITY, ProcessAddressSpace};

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
const STATUS_OPERATION_NOT_READY: u32 = 3;
const IPC_INLINE_WORD_CAPACITY: u64 = 6;
const ENTRY_STACK_SIZE: u64 = 64 * 1024;
pub const PROCESS_CAPABILITY_CAPACITY: usize = 64;

static USER_SELF_TEST_RESULT: AtomicU64 = AtomicU64::new(0);
static CURRENT_PROCESS_ADDRESS_SPACE: AtomicUsize = AtomicUsize::new(0);
static CURRENT_PROCESS_CAPABILITIES: AtomicUsize = AtomicUsize::new(0);

pub struct CurrentProcessContextGuard {
    address_space: usize,
    capabilities: usize,
}

impl Drop for CurrentProcessContextGuard {
    fn drop(&mut self) {
        let _ = CURRENT_PROCESS_ADDRESS_SPACE.compare_exchange(
            self.address_space,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = CURRENT_PROCESS_CAPABILITIES.compare_exchange(
            self.capabilities,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentProcessContextError {
    NotInstalled,
    UserMemory(IpcUserMemoryError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointCapabilityError {
    ContextNotInstalled,
    Capability(CapabilityError),
    WrongObjectKind,
}

/// Registers the address space used by the current system-call execution context.
///
/// # Safety
///
/// `space` and `capabilities` must remain alive and exclusively owned until the
/// returned guard is dropped. The early implementation is single-CPU and
/// callers must keep interrupts disabled while user code can enter the
/// system-call dispatcher.
pub unsafe fn install_current_process_context(
    space: &mut ProcessAddressSpace<PROCESS_REGION_CAPACITY>,
    capabilities: &mut ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>,
) -> Option<CurrentProcessContextGuard> {
    let address_space = space as *mut ProcessAddressSpace<PROCESS_REGION_CAPACITY> as usize;
    let capabilities =
        capabilities as *mut ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY> as usize;
    if CURRENT_PROCESS_CAPABILITIES
        .compare_exchange(0, capabilities, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }

    CURRENT_PROCESS_ADDRESS_SPACE
        .compare_exchange(0, address_space, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| CurrentProcessContextGuard {
            address_space,
            capabilities,
        })
        .or_else(|| {
            CURRENT_PROCESS_CAPABILITIES.store(0, Ordering::Release);
            None
        })
}

fn with_current_process_address_space<T>(
    operation: impl FnOnce(&mut ProcessAddressSpace<PROCESS_REGION_CAPACITY>) -> T,
) -> Option<T> {
    let address_space = CURRENT_PROCESS_ADDRESS_SPACE.load(Ordering::Acquire);
    if address_space == 0 {
        return None;
    }

    let space =
        unsafe { &mut *(address_space as *mut ProcessAddressSpace<PROCESS_REGION_CAPACITY>) };
    Some(operation(space))
}

pub fn copy_ipc_receive_words_to_current_process(
    arguments: IpcReceiveArguments,
    words: &[u64],
) -> Result<(), CurrentProcessContextError> {
    with_current_process_address_space(|space| {
        ipc_user_memory::copy_receive_words(space, arguments, words)
    })
    .ok_or(CurrentProcessContextError::NotInstalled)?
    .map_err(CurrentProcessContextError::UserMemory)
}

pub fn resolve_current_endpoint(
    packed: PackedCapabilityHandle,
    required_rights: Rights,
) -> Result<EndpointId, EndpointCapabilityError> {
    let capabilities = CURRENT_PROCESS_CAPABILITIES.load(Ordering::Acquire);
    if capabilities == 0 {
        return Err(EndpointCapabilityError::ContextNotInstalled);
    }

    let capabilities = unsafe {
        &*(capabilities as *const ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>)
    };
    let capability = capabilities
        .require(
            CapabilityHandle {
                slot: packed.slot(),
                generation: packed.generation(),
            },
            required_rights,
        )
        .map_err(EndpointCapabilityError::Capability)?;

    if capability.kind != ObjectKind::Endpoint {
        return Err(EndpointCapabilityError::WrongObjectKind);
    }

    Ok(EndpointId(capability.object.0))
}

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
    let Ok(context) =
        UserReturnContext::new(instruction_pointer, stack_pointer, RFLAGS_RESERVED_ONE)
    else {
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

    match request.number {
        SYSCALL_IPC_SEND => return dispatch_ipc_send(request),
        SYSCALL_IPC_RECEIVE => return dispatch_ipc_receive(request),
        SELF_TEST_NUMBER => {}
        _ => return syscall_failure(STATUS_UNKNOWN_CALL),
    }

    if request.arguments != SELF_TEST_ARGUMENTS {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    SyscallReturn::success(SELF_TEST_RESULT)
}

fn dispatch_ipc_send(request: SyscallRequest) -> SyscallReturn {
    let arguments = ipc_send_arguments(request);
    let words_address = arguments.words_address;
    let word_count = arguments.word_count;
    let flags = arguments.flags;
    let reserved = request.arguments[5];

    if flags != 0 || reserved != 0 || word_count > IPC_INLINE_WORD_CAPACITY {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    if phoenix_vm::UserBuffer::for_array(words_address, word_count, 8, 8).is_err() {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    if matches!(
        with_current_process_address_space(|space| {
            ipc_user_memory::copy_send_words(space, arguments)
        }),
        Some(Err(_))
    ) {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    match resolve_current_endpoint(arguments.endpoint, Rights::WRITE) {
        Ok(_) | Err(EndpointCapabilityError::ContextNotInstalled) => {}
        Err(_) => return syscall_failure(STATUS_BAD_ARGUMENTS),
    }

    syscall_failure(STATUS_OPERATION_NOT_READY)
}

fn dispatch_ipc_receive(request: SyscallRequest) -> SyscallReturn {
    let arguments = ipc_receive_arguments(request);
    let words_address = arguments.words_address;
    let word_capacity = arguments.word_capacity;
    let metadata_address = arguments.metadata_address;
    let flags = arguments.flags;
    let reserved = request.arguments[5];

    if flags != 0 || reserved != 0 {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    if phoenix_vm::UserBuffer::for_array(words_address, word_capacity, 8, 8).is_err()
        || phoenix_vm::UserBuffer::for_array(metadata_address, 1, 8, 8).is_err()
    {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    if matches!(
        with_current_process_address_space(|space| {
            ipc_user_memory::copy_receive_words(space, arguments, &[])
        }),
        Some(Err(_))
    ) {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    match resolve_current_endpoint(arguments.endpoint, Rights::READ) {
        Ok(_) | Err(EndpointCapabilityError::ContextNotInstalled) => {}
        Err(_) => return syscall_failure(STATUS_BAD_ARGUMENTS),
    }

    syscall_failure(STATUS_OPERATION_NOT_READY)
}

fn ipc_send_arguments(request: SyscallRequest) -> IpcSendArguments {
    IpcSendArguments {
        endpoint: PackedCapabilityHandle::from_raw(request.arguments[0]),
        words_address: request.arguments[1],
        word_count: request.arguments[2],
        transferred_capability: (request.arguments[3] != NO_TRANSFERRED_CAPABILITY)
            .then(|| PackedCapabilityHandle::from_raw(request.arguments[3])),
        flags: request.arguments[4],
    }
}

fn ipc_receive_arguments(request: SyscallRequest) -> IpcReceiveArguments {
    IpcReceiveArguments {
        endpoint: PackedCapabilityHandle::from_raw(request.arguments[0]),
        words_address: request.arguments[1],
        word_capacity: request.arguments[2],
        metadata_address: request.arguments[3],
        flags: request.arguments[4],
    }
}

pub fn ipc_user_memory_context_self_test(
    send: IpcSendArguments,
    unmapped_send: IpcSendArguments,
    denied_send: IpcSendArguments,
    receive: IpcReceiveArguments,
    receive_words: &[u64],
    expected_endpoint: EndpointId,
) -> bool {
    let send_result = dispatch(SyscallRequest::ipc_send(send));
    let unmapped_result = dispatch(SyscallRequest::ipc_send(unmapped_send));
    let denied_result = dispatch(SyscallRequest::ipc_send(denied_send));
    let receive_dispatch_result = dispatch(SyscallRequest::ipc_receive(receive));
    let receive_result = copy_ipc_receive_words_to_current_process(receive, receive_words);
    let resolved = resolve_current_endpoint(send.endpoint, Rights::WRITE);

    send_result.status.raw() == u64::from(STATUS_OPERATION_NOT_READY)
        && unmapped_result.status.raw() == u64::from(STATUS_BAD_ARGUMENTS)
        && denied_result.status.raw() == u64::from(STATUS_BAD_ARGUMENTS)
        && receive_dispatch_result.status.raw() == u64::from(STATUS_OPERATION_NOT_READY)
        && receive_result.is_ok()
        && resolved == Ok(expected_endpoint)
}

pub fn ipc_dispatch_self_test() -> bool {
    let valid_send = SyscallRequest::ipc_send(IpcSendArguments {
        endpoint: PackedCapabilityHandle::new(1, 1),
        words_address: 0x4000,
        word_count: 2,
        transferred_capability: None,
        flags: 0,
    });
    let send_result = dispatch(valid_send);

    let valid_receive = SyscallRequest::ipc_receive(IpcReceiveArguments {
        endpoint: PackedCapabilityHandle::new(1, 1),
        words_address: 0x5000,
        word_capacity: IPC_INLINE_WORD_CAPACITY,
        metadata_address: 0x6000,
        flags: 0,
    });
    let receive_result = dispatch(valid_receive);

    let invalid_send = SyscallRequest::ipc_send(IpcSendArguments {
        endpoint: PackedCapabilityHandle::new(1, 1),
        words_address: phoenix_vm::USER_SPACE_END_EXCLUSIVE - 8,
        word_count: 2,
        transferred_capability: None,
        flags: 0,
    });
    let invalid_send_result = dispatch(invalid_send);

    send_result.status.raw() == u64::from(STATUS_OPERATION_NOT_READY)
        && receive_result.status.raw() == u64::from(STATUS_OPERATION_NOT_READY)
        && invalid_send_result.status.raw() == u64::from(STATUS_BAD_ARGUMENTS)
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
