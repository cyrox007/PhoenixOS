use core::arch::{global_asm, x86_64::__cpuid};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use phoenix_capability::{CapabilityError, CapabilityHandle, ObjectKind, Rights};
use phoenix_ipc::{
    EndpointId, EndpointRegistry, EndpointRegistryError, IpcError, IpcSendError, Message,
    send_with_capability,
};
use phoenix_process::{ProcessCapabilitySet, ProcessId};
use phoenix_syscall_abi::{
    FILE_OPEN_CREATE, FILE_OPEN_READ, FILE_OPEN_TRUNCATE, FILE_OPEN_WRITE, FILE_SEEK_CURRENT,
    FILE_SEEK_END, FILE_SEEK_START, IpcReceiveArguments, IpcReceiveMetadata, IpcSendArguments,
    NO_TRANSFERRED_CAPABILITY, PackedCapabilityHandle, PackedFileDescriptor, SYSCALL_FILE_CLOSE,
    SYSCALL_FILE_OPEN, SYSCALL_FILE_READ, SYSCALL_FILE_SEEK, SYSCALL_FILE_WRITE,
    SYSCALL_IPC_RECEIVE, SYSCALL_IPC_SEND, SYSCALL_PROCESS_EXIT, SyscallRequest, SyscallReturn,
    SyscallStatus,
};
use phoenix_vfs::{
    AccessMode, DescriptorError, DescriptorTable, FileDescriptor, FileSystem, MemoryFileSystem,
    SeekOrigin, VfsError, resolve_path,
};

use crate::file_user_memory;
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
pub const USER_SELF_TEST_SUCCESS: u64 = 0x5553_4552_5f4f_4b21;
const USER_SELF_TEST_FAILURE: u64 = 0x5553_4552_5f42_4144;
const STATUS_UNKNOWN_CALL: u32 = 1;
const STATUS_BAD_ARGUMENTS: u32 = 2;
const STATUS_OPERATION_NOT_READY: u32 = 3;
const FILE_PATH_CAPACITY: usize = 256;
const IPC_INLINE_WORD_CAPACITY: u64 = 6;
const ENTRY_STACK_SIZE: u64 = 64 * 1024;
pub const PROCESS_CAPABILITY_CAPACITY: usize = 64;
pub const ENDPOINT_REGISTRY_CAPACITY: usize = 8;
pub const ENDPOINT_QUEUE_CAPACITY: usize = 4;
pub const PROCESS_CAPABILITY_REGISTRY_CAPACITY: usize = 8;
pub const FILESYSTEM_NODE_CAPACITY: usize = 8;
pub const FILE_CAPACITY: usize = 512;
pub const FILE_DESCRIPTOR_CAPACITY: usize = 16;

pub type KernelFileSystem = MemoryFileSystem<FILESYSTEM_NODE_CAPACITY, FILE_CAPACITY>;
pub type KernelDescriptorTable = DescriptorTable<FILE_DESCRIPTOR_CAPACITY>;

pub type KernelEndpointRegistry =
    EndpointRegistry<ENDPOINT_REGISTRY_CAPACITY, ENDPOINT_QUEUE_CAPACITY>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessCapabilityRegistryError {
    CapacityExceeded,
    DuplicateProcess,
    NotFound,
}

#[derive(Clone, Copy)]
struct RegisteredProcessCapabilities {
    owner: ProcessId,
    capabilities: usize,
}

pub struct KernelProcessCapabilityRegistry {
    entries: [Option<RegisteredProcessCapabilities>; PROCESS_CAPABILITY_REGISTRY_CAPACITY],
    len: usize,
}

impl KernelProcessCapabilityRegistry {
    pub const fn new() -> Self {
        Self {
            entries: [None; PROCESS_CAPABILITY_REGISTRY_CAPACITY],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn register(
        &mut self,
        capabilities: &mut ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>,
    ) -> Result<(), ProcessCapabilityRegistryError> {
        let owner = capabilities.owner();
        if self
            .entries
            .iter()
            .flatten()
            .any(|entry| entry.owner == owner)
        {
            return Err(ProcessCapabilityRegistryError::DuplicateProcess);
        }

        let Some(slot) = self.entries.iter_mut().find(|entry| entry.is_none()) else {
            return Err(ProcessCapabilityRegistryError::CapacityExceeded);
        };

        *slot = Some(RegisteredProcessCapabilities {
            owner,
            capabilities: capabilities as *mut _ as usize,
        });
        self.len += 1;
        Ok(())
    }

    fn get_mut(
        &mut self,
        owner: ProcessId,
    ) -> Result<
        &mut ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>,
        ProcessCapabilityRegistryError,
    > {
        let entry = self
            .entries
            .iter()
            .flatten()
            .find(|entry| entry.owner == owner)
            .ok_or(ProcessCapabilityRegistryError::NotFound)?;
        Ok(unsafe {
            &mut *(entry.capabilities as *mut ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>)
        })
    }
}

impl Default for KernelProcessCapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static USER_PROCESS_EXITED: AtomicBool = AtomicBool::new(false);
static USER_PROCESS_EXIT_STATUS: AtomicU64 = AtomicU64::new(0);
static CURRENT_PROCESS_ADDRESS_SPACE: AtomicUsize = AtomicUsize::new(0);
static CURRENT_PROCESS_CAPABILITIES: AtomicUsize = AtomicUsize::new(0);
static CURRENT_ENDPOINT_REGISTRY: AtomicUsize = AtomicUsize::new(0);
static CURRENT_PROCESS_CAPABILITY_REGISTRY: AtomicUsize = AtomicUsize::new(0);
static CURRENT_FILESYSTEM: AtomicUsize = AtomicUsize::new(0);
static CURRENT_FILE_DESCRIPTORS: AtomicUsize = AtomicUsize::new(0);

pub struct CurrentFileContextGuard {
    filesystem: usize,
    descriptors: usize,
}

impl Drop for CurrentFileContextGuard {
    fn drop(&mut self) {
        let _ = CURRENT_FILE_DESCRIPTORS.compare_exchange(
            self.descriptors,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = CURRENT_FILESYSTEM.compare_exchange(
            self.filesystem,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

pub struct CurrentProcessContextGuard {
    address_space: usize,
    capabilities: usize,
    endpoint_registry: usize,
    process_capability_registry: usize,
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
        let _ = CURRENT_ENDPOINT_REGISTRY.compare_exchange(
            self.endpoint_registry,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = CURRENT_PROCESS_CAPABILITY_REGISTRY.compare_exchange(
            self.process_capability_registry,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointOperationError {
    ContextNotInstalled,
    Registry(EndpointRegistryError),
    ProcessRegistry(ProcessCapabilityRegistryError),
    Transport(IpcError),
    Transfer(IpcSendError),
    UserMemory(IpcUserMemoryError),
    MissingEndpointOwner,
    SameProcessTransfer,
    WrongEndpointReceiver,
}

impl From<EndpointRegistryError> for EndpointOperationError {
    fn from(error: EndpointRegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<IpcError> for EndpointOperationError {
    fn from(error: IpcError) -> Self {
        Self::Transport(error)
    }
}

impl From<IpcSendError> for EndpointOperationError {
    fn from(error: IpcSendError) -> Self {
        Self::Transfer(error)
    }
}

impl From<ProcessCapabilityRegistryError> for EndpointOperationError {
    fn from(error: ProcessCapabilityRegistryError) -> Self {
        Self::ProcessRegistry(error)
    }
}

impl From<IpcUserMemoryError> for EndpointOperationError {
    fn from(error: IpcUserMemoryError) -> Self {
        Self::UserMemory(error)
    }
}

/// Registers the address space used by the current system-call execution context.
///
/// # Safety
///
/// `space` and `capabilities` must remain alive and exclusively owned until the
/// returned guard is dropped. `endpoint_registry` and `process_capability_registry`
/// must remain alive and must not be accessed outside the dispatcher while the guard
/// is active. The early
/// implementation is single-CPU and callers must keep interrupts disabled while
/// user code can enter the system-call dispatcher.
pub unsafe fn install_current_process_context(
    space: &mut ProcessAddressSpace<PROCESS_REGION_CAPACITY>,
    capabilities: &mut ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>,
    endpoint_registry: &mut KernelEndpointRegistry,
    process_capability_registry: &mut KernelProcessCapabilityRegistry,
) -> Option<CurrentProcessContextGuard> {
    let address_space = space as *mut ProcessAddressSpace<PROCESS_REGION_CAPACITY> as usize;
    let capabilities =
        capabilities as *mut ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY> as usize;
    let endpoint_registry = endpoint_registry as *mut KernelEndpointRegistry as usize;
    let process_capability_registry =
        process_capability_registry as *mut KernelProcessCapabilityRegistry as usize;
    if CURRENT_PROCESS_CAPABILITY_REGISTRY
        .compare_exchange(
            0,
            process_capability_registry,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    if CURRENT_ENDPOINT_REGISTRY
        .compare_exchange(0, endpoint_registry, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        CURRENT_PROCESS_CAPABILITY_REGISTRY.store(0, Ordering::Release);
        return None;
    }
    if CURRENT_PROCESS_CAPABILITIES
        .compare_exchange(0, capabilities, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        CURRENT_ENDPOINT_REGISTRY.store(0, Ordering::Release);
        CURRENT_PROCESS_CAPABILITY_REGISTRY.store(0, Ordering::Release);
        return None;
    }

    CURRENT_PROCESS_ADDRESS_SPACE
        .compare_exchange(0, address_space, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| CurrentProcessContextGuard {
            address_space,
            capabilities,
            endpoint_registry,
            process_capability_registry,
        })
        .or_else(|| {
            CURRENT_PROCESS_CAPABILITIES.store(0, Ordering::Release);
            CURRENT_ENDPOINT_REGISTRY.store(0, Ordering::Release);
            CURRENT_PROCESS_CAPABILITY_REGISTRY.store(0, Ordering::Release);
            None
        })
}

/// Installs the filesystem and descriptor table used by the current process.
///
/// # Safety
///
/// Both objects must remain alive and exclusively owned until the guard is dropped.
/// The early implementation is single-CPU; callers must keep interrupts disabled
/// while user code can enter the dispatcher.
pub unsafe fn install_current_file_context(
    filesystem: &mut KernelFileSystem,
    descriptors: &mut KernelDescriptorTable,
) -> Option<CurrentFileContextGuard> {
    let filesystem = filesystem as *mut KernelFileSystem as usize;
    let descriptors = descriptors as *mut KernelDescriptorTable as usize;
    if CURRENT_FILESYSTEM
        .compare_exchange(0, filesystem, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    CURRENT_FILE_DESCRIPTORS
        .compare_exchange(0, descriptors, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| CurrentFileContextGuard {
            filesystem,
            descriptors,
        })
        .or_else(|| {
            CURRENT_FILESYSTEM.store(0, Ordering::Release);
            None
        })
}

fn with_current_file_context<T>(
    operation: impl FnOnce(&mut KernelFileSystem, &mut KernelDescriptorTable) -> T,
) -> Option<T> {
    let filesystem = CURRENT_FILESYSTEM.load(Ordering::Acquire);
    let descriptors = CURRENT_FILE_DESCRIPTORS.load(Ordering::Acquire);
    if filesystem == 0 || descriptors == 0 {
        return None;
    }
    let filesystem = unsafe { &mut *(filesystem as *mut KernelFileSystem) };
    let descriptors = unsafe { &mut *(descriptors as *mut KernelDescriptorTable) };
    Some(operation(filesystem, descriptors))
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

fn with_current_endpoint_registry<T>(
    operation: impl FnOnce(&mut KernelEndpointRegistry) -> T,
) -> Option<T> {
    let registry = CURRENT_ENDPOINT_REGISTRY.load(Ordering::Acquire);
    if registry == 0 {
        return None;
    }

    let registry = unsafe { &mut *(registry as *mut KernelEndpointRegistry) };
    Some(operation(registry))
}

fn with_current_process_capabilities<T>(
    operation: impl FnOnce(&mut ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>) -> T,
) -> Option<T> {
    let capabilities = CURRENT_PROCESS_CAPABILITIES.load(Ordering::Acquire);
    if capabilities == 0 {
        return None;
    }

    let capabilities =
        unsafe { &mut *(capabilities as *mut ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>) };
    Some(operation(capabilities))
}

fn with_process_capability_registry<T>(
    operation: impl FnOnce(&mut KernelProcessCapabilityRegistry) -> T,
) -> Option<T> {
    let registry = CURRENT_PROCESS_CAPABILITY_REGISTRY.load(Ordering::Acquire);
    if registry == 0 {
        return None;
    }

    let registry = unsafe { &mut *(registry as *mut KernelProcessCapabilityRegistry) };
    Some(operation(registry))
}

fn current_process_id() -> Option<ProcessId> {
    let capabilities = CURRENT_PROCESS_CAPABILITIES.load(Ordering::Acquire);
    if capabilities == 0 {
        return None;
    }

    let capabilities =
        unsafe { &*(capabilities as *const ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>) };
    Some(capabilities.owner())
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

    let capabilities =
        unsafe { &*(capabilities as *const ProcessCapabilitySet<PROCESS_CAPABILITY_CAPACITY>) };
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

#[unsafe(export_name = "phoenix_user_process_active")]
static mut USER_PROCESS_ACTIVE: u64 = 0;

#[unsafe(export_name = "phoenix_user_process_kernel_rsp")]
static mut USER_PROCESS_KERNEL_RSP: u64 = 0;

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

    cmp qword ptr [rsp + 0], 0x102
    jne 1f
    cmp qword ptr [rsp + 72], 0
    jne 1f
    cmp qword ptr [rip + phoenix_user_process_active], 1
    je phoenix_user_process_return_from_syscall
1:
    mov rax, [rsp + 64]
    mov rdx, [rsp + 72]

    add rsp, 80
    pop r11
    pop rcx

    cmp qword ptr [rip + phoenix_user_process_active], 1
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

    mov [rip + phoenix_user_process_kernel_rsp], rsp
    mov qword ptr [rip + phoenix_user_process_active], 1

    push rcx
    push rsi
    push r8
    push rdx
    push rdi
    mov rdi, r9
    iretq
    .size phoenix_user_test_enter, .-phoenix_user_test_enter

phoenix_user_process_return_from_syscall:
    mov qword ptr [rip + phoenix_user_process_active], 0
    mov rsp, [rip + phoenix_user_process_kernel_rsp]

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
    test rdi, rdi
    je 3f
    cmp dword ptr [rdi + 0], 0
    jne 3f
    cmp dword ptr [rdi + 4], 0
    jne 3f
    cmp qword ptr [rdi + 8], 2
    jne 3f
    cmp qword ptr [rdi + 16], 0
    je 3f
    cmp qword ptr [rdi + 24], 1
    jne 3f
    cmp qword ptr [rdi + 32], 0
    je 3f

    mov r12, [rdi + 16]
    cmp qword ptr [r12 + 8], 12
    jne 3f
    mov r13, [r12]
    mov r14, 0x2d78696e656f6870
    cmp qword ptr [r13], r14
    jne 3f
    cmp dword ptr [r13 + 8], 0x74736574
    jne 3f
    cmp qword ptr [r12 + 24], 11
    jne 3f
    mov r13, [r12 + 16]
    mov r14, 0x742d666c65732d2d
    cmp qword ptr [r13], r14
    jne 3f
    cmp word ptr [r13 + 8], 0x7365
    jne 3f
    cmp byte ptr [r13 + 10], 0x74
    jne 3f

    mov r12, [rdi + 32]
    cmp qword ptr [r12 + 8], 9
    jne 3f
    mov r13, [r12]
    mov r14, 0x6d65713d45444f4d
    cmp qword ptr [r13], r14
    jne 3f
    cmp byte ptr [r13 + 8], 0x75
    jne 3f

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
    mov rax, 0x102
    xor rsi, rsi
    xor rdx, rdx
    xor r10, r10
    xor r8, r8
    xor r9, r9
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
        start_info_address: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserProcessRunError {
    InvalidContext(UserReturnError),
    ReturnedWithoutExit,
}

pub fn run_user_process(
    instruction_pointer: u64,
    stack_pointer: u64,
) -> Result<u64, UserProcessRunError> {
    run_user_process_with_start_info(instruction_pointer, stack_pointer, 0)
}

pub fn run_user_process_with_start_info(
    instruction_pointer: u64,
    stack_pointer: u64,
    start_info_address: u64,
) -> Result<u64, UserProcessRunError> {
    let context = UserReturnContext::new(instruction_pointer, stack_pointer, RFLAGS_RESERVED_ONE)
        .map_err(UserProcessRunError::InvalidContext)?;

    USER_PROCESS_EXITED.store(false, Ordering::Release);
    USER_PROCESS_EXIT_STATUS.store(0, Ordering::Release);

    unsafe {
        phoenix_user_test_enter(
            context.instruction_pointer,
            context.stack_pointer,
            u64::from(super::gdt::user_code_selector_raw()),
            u64::from(super::gdt::user_data_selector_raw()),
            context.cpu_flags,
            start_info_address,
        );
    }

    if !USER_PROCESS_EXITED.load(Ordering::Acquire) {
        return Err(UserProcessRunError::ReturnedWithoutExit);
    }

    Ok(USER_PROCESS_EXIT_STATUS.load(Ordering::Acquire))
}

pub fn run_user_mode_self_test(
    instruction_pointer: u64,
    stack_pointer: u64,
    start_info_address: u64,
) -> bool {
    run_user_process_with_start_info(instruction_pointer, stack_pointer, start_info_address)
        == Ok(USER_SELF_TEST_SUCCESS)
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
    match request.number {
        SYSCALL_IPC_SEND => return dispatch_ipc_send(request),
        SYSCALL_IPC_RECEIVE => return dispatch_ipc_receive(request),
        SYSCALL_PROCESS_EXIT => return dispatch_process_exit(request),
        SYSCALL_FILE_OPEN => return dispatch_file_open(request),
        SYSCALL_FILE_READ | SYSCALL_FILE_WRITE => return dispatch_file_io(request),
        SYSCALL_FILE_SEEK => return dispatch_file_seek(request),
        SYSCALL_FILE_CLOSE => return dispatch_file_close(request),
        SELF_TEST_NUMBER => {}
        _ => return syscall_failure(STATUS_UNKNOWN_CALL),
    }

    if request.arguments != SELF_TEST_ARGUMENTS {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    SyscallReturn::success(SELF_TEST_RESULT)
}

fn dispatch_file_open(request: SyscallRequest) -> SyscallReturn {
    let flags = request.arguments[2];
    let known_flags = FILE_OPEN_READ | FILE_OPEN_WRITE | FILE_OPEN_CREATE | FILE_OPEN_TRUNCATE;
    if request.arguments[3..].iter().any(|argument| *argument != 0)
        || flags == 0
        || flags & !known_flags != 0
        || flags & (FILE_OPEN_READ | FILE_OPEN_WRITE) == 0
        || flags & FILE_OPEN_TRUNCATE != 0 && flags & FILE_OPEN_WRITE == 0
    {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    if phoenix_vm::UserBuffer::for_array(request.arguments[0], request.arguments[1], 1, 1).is_err()
    {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }
    let mut path_bytes = [0_u8; FILE_PATH_CAPACITY];
    let path = match with_current_process_address_space(|space| {
        file_user_memory::copy_path(
            space,
            request.arguments[0],
            request.arguments[1],
            &mut path_bytes,
        )
    }) {
        Some(Ok(path)) => path,
        Some(Err(_)) => return syscall_failure(STATUS_BAD_ARGUMENTS),
        None => return syscall_failure(STATUS_OPERATION_NOT_READY),
    };

    let Some(result) = with_current_file_context(
        |filesystem, descriptors| -> Result<FileDescriptor, DescriptorError> {
            let node = match resolve_path(filesystem, path) {
                Ok(node) => node,
                Err(VfsError::NotFound) if flags & FILE_OPEN_CREATE != 0 => {
                    filesystem.create_file(path)?
                }
                Err(error) => return Err(error.into()),
            };
            if flags & FILE_OPEN_TRUNCATE != 0 {
                filesystem.truncate_node(node, 0)?;
            }
            let access = match flags & (FILE_OPEN_READ | FILE_OPEN_WRITE) {
                FILE_OPEN_READ => AccessMode::ReadOnly,
                FILE_OPEN_WRITE => AccessMode::WriteOnly,
                _ => AccessMode::ReadWrite,
            };
            descriptors.open(filesystem, node, access)
        },
    ) else {
        return syscall_failure(STATUS_OPERATION_NOT_READY);
    };

    match result {
        Ok(descriptor) => SyscallReturn::success(
            PackedFileDescriptor::new(descriptor.slot, descriptor.generation).raw(),
        ),
        Err(_) => syscall_failure(STATUS_BAD_ARGUMENTS),
    }
}

fn dispatch_file_io(request: SyscallRequest) -> SyscallReturn {
    if request.arguments[3..].iter().any(|argument| *argument != 0) {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }
    if phoenix_vm::UserBuffer::for_array(request.arguments[1], request.arguments[2], 1, 1).is_err()
    {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }
    let Ok(length) = usize::try_from(request.arguments[2]) else {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    };
    if length > FILE_CAPACITY {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }
    let descriptor = unpack_file_descriptor(request.arguments[0]);
    let mut buffer = [0_u8; FILE_CAPACITY];

    if request.number == SYSCALL_FILE_WRITE {
        let copied = with_current_process_address_space(|space| {
            file_user_memory::copy_from_user(
                space,
                request.arguments[1],
                request.arguments[2],
                &mut buffer,
            )
        });
        let data = match copied {
            Some(Ok(data)) => data,
            Some(Err(_)) => return syscall_failure(STATUS_BAD_ARGUMENTS),
            None => return syscall_failure(STATUS_OPERATION_NOT_READY),
        };
        return match with_current_file_context(|filesystem, descriptors| {
            descriptors.write(filesystem, descriptor, data)
        }) {
            Some(Ok(written)) => SyscallReturn::success(written as u64),
            Some(Err(_)) => syscall_failure(STATUS_BAD_ARGUMENTS),
            None => syscall_failure(STATUS_OPERATION_NOT_READY),
        };
    }

    match with_current_process_address_space(|space| {
        file_user_memory::validate_user_buffer(
            space,
            request.arguments[1],
            request.arguments[2],
        )
    }) {
        Some(Ok(())) => {}
        Some(Err(_)) => return syscall_failure(STATUS_BAD_ARGUMENTS),
        None => return syscall_failure(STATUS_OPERATION_NOT_READY),
    }
    let read = match with_current_file_context(|filesystem, descriptors| {
        descriptors.read(filesystem, descriptor, &mut buffer[..length])
    }) {
        Some(Ok(read)) => read,
        Some(Err(_)) => return syscall_failure(STATUS_BAD_ARGUMENTS),
        None => return syscall_failure(STATUS_OPERATION_NOT_READY),
    };
    match with_current_process_address_space(|space| {
        file_user_memory::copy_to_user(
            space,
            request.arguments[1],
            request.arguments[2],
            &buffer[..read],
        )
    }) {
        Some(Ok(())) => SyscallReturn::success(read as u64),
        Some(Err(_)) => syscall_failure(STATUS_BAD_ARGUMENTS),
        None => syscall_failure(STATUS_OPERATION_NOT_READY),
    }
}

fn dispatch_file_seek(request: SyscallRequest) -> SyscallReturn {
    if request.arguments[3..].iter().any(|argument| *argument != 0)
        || !matches!(
            request.arguments[2],
            FILE_SEEK_START | FILE_SEEK_CURRENT | FILE_SEEK_END
        )
    {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }
    let descriptor = unpack_file_descriptor(request.arguments[0]);
    let origin = match request.arguments[2] {
        FILE_SEEK_START => SeekOrigin::Start,
        FILE_SEEK_CURRENT => SeekOrigin::Current,
        FILE_SEEK_END => SeekOrigin::End,
        _ => unreachable!(),
    };
    match with_current_file_context(|filesystem, descriptors| {
        descriptors.seek(filesystem, descriptor, request.arguments[1] as i64, origin)
    }) {
        Some(Ok(position)) => SyscallReturn::success(position),
        Some(Err(_)) => syscall_failure(STATUS_BAD_ARGUMENTS),
        None => syscall_failure(STATUS_OPERATION_NOT_READY),
    }
}

fn dispatch_file_close(request: SyscallRequest) -> SyscallReturn {
    if request.arguments[1..].iter().any(|argument| *argument != 0) {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }
    let descriptor = unpack_file_descriptor(request.arguments[0]);
    match with_current_file_context(|_, descriptors| descriptors.close(descriptor)) {
        Some(Ok(())) => SyscallReturn::success(0),
        Some(Err(_)) => syscall_failure(STATUS_BAD_ARGUMENTS),
        None => syscall_failure(STATUS_OPERATION_NOT_READY),
    }
}

fn unpack_file_descriptor(raw: u64) -> FileDescriptor {
    let descriptor = PackedFileDescriptor::from_raw(raw);
    FileDescriptor {
        slot: descriptor.slot,
        generation: descriptor.generation,
    }
}

pub fn file_context_operations_self_test(
    path_address: u64,
    path_length: u64,
    input_address: u64,
    output_address: u64,
    buffer_length: u64,
) -> bool {
    let opened = dispatch(SyscallRequest::new(
        SYSCALL_FILE_OPEN,
        [
            path_address,
            path_length,
            FILE_OPEN_READ | FILE_OPEN_WRITE,
            0,
            0,
            0,
        ],
    ));
    if !opened.is_success() {
        return false;
    }
    let write = dispatch(SyscallRequest::new(
        SYSCALL_FILE_WRITE,
        [opened.value, input_address, buffer_length, 0, 0, 0],
    ));
    let seek = dispatch(SyscallRequest::new(
        SYSCALL_FILE_SEEK,
        [opened.value, 0, FILE_SEEK_START, 0, 0, 0],
    ));
    let read = dispatch(SyscallRequest::new(
        SYSCALL_FILE_READ,
        [opened.value, output_address, buffer_length, 0, 0, 0],
    ));
    let close = dispatch(SyscallRequest::new(
        SYSCALL_FILE_CLOSE,
        [opened.value, 0, 0, 0, 0, 0],
    ));
    let stale_close = dispatch(SyscallRequest::new(
        SYSCALL_FILE_CLOSE,
        [opened.value, 0, 0, 0, 0, 0],
    ));
    write.is_success()
        && write.value == buffer_length
        && seek.is_success()
        && seek.value == 0
        && read.is_success()
        && read.value == buffer_length
        && close.is_success()
        && !stale_close.is_success()
}

pub fn file_dispatch_self_test() -> bool {
    let valid_open = SyscallRequest::new(SYSCALL_FILE_OPEN, [0x4000, 4, FILE_OPEN_READ, 0, 0, 0]);
    let invalid_open = SyscallRequest::new(
        SYSCALL_FILE_OPEN,
        [0x4000, 4, FILE_OPEN_TRUNCATE | FILE_OPEN_READ, 0, 0, 0],
    );
    let invalid_seek = SyscallRequest::new(SYSCALL_FILE_SEEK, [1, 0, 3, 0, 0, 0]);
    let reserved_close = SyscallRequest::new(SYSCALL_FILE_CLOSE, [1, 1, 0, 0, 0, 0]);

    dispatch(valid_open).status.raw() == u64::from(STATUS_OPERATION_NOT_READY)
        && dispatch(invalid_open).status.raw() == u64::from(STATUS_BAD_ARGUMENTS)
        && dispatch(invalid_seek).status.raw() == u64::from(STATUS_BAD_ARGUMENTS)
        && dispatch(reserved_close).status.raw() == u64::from(STATUS_BAD_ARGUMENTS)
}

fn dispatch_process_exit(request: SyscallRequest) -> SyscallReturn {
    if request.arguments[1..].iter().any(|argument| *argument != 0) {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    USER_PROCESS_EXIT_STATUS.store(request.arguments[0], Ordering::Release);
    USER_PROCESS_EXITED.store(true, Ordering::Release);
    SyscallReturn::success(0)
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

    let words = match with_current_process_address_space(|space| {
        ipc_user_memory::copy_send_words(space, arguments)
    }) {
        Some(Ok(words)) => words,
        Some(Err(_)) => return syscall_failure(STATUS_BAD_ARGUMENTS),
        None => return syscall_failure(STATUS_OPERATION_NOT_READY),
    };
    let endpoint_id = match resolve_current_endpoint(arguments.endpoint, Rights::WRITE) {
        Ok(endpoint_id) => endpoint_id,
        Err(EndpointCapabilityError::ContextNotInstalled) => {
            return syscall_failure(STATUS_OPERATION_NOT_READY);
        }
        Err(_) => return syscall_failure(STATUS_BAD_ARGUMENTS),
    };
    let Some(sender) = current_process_id() else {
        return syscall_failure(STATUS_OPERATION_NOT_READY);
    };

    let Some(result) = with_current_endpoint_registry(|registry| {
        let endpoint = registry.get_mut(endpoint_id)?;
        let Some(packed_capability) = arguments.transferred_capability else {
            let message = Message::new(sender, words.as_slice())?;
            endpoint.send(message)?;
            return Ok::<_, EndpointOperationError>(message.len());
        };

        let receiver = endpoint
            .owner()
            .ok_or(EndpointOperationError::MissingEndpointOwner)?;
        if receiver == sender {
            return Err(EndpointOperationError::SameProcessTransfer);
        }

        let transfer_handle = CapabilityHandle {
            slot: packed_capability.slot(),
            generation: packed_capability.generation(),
        };
        let transfer = with_current_process_capabilities(|source| {
            with_process_capability_registry(|processes| {
                let target = processes.get_mut(receiver)?;
                send_with_capability(endpoint, source, target, words.as_slice(), transfer_handle)?;
                Ok::<_, EndpointOperationError>(words.len())
            })
            .ok_or(EndpointOperationError::ContextNotInstalled)?
        })
        .ok_or(EndpointOperationError::ContextNotInstalled)?;

        transfer
    }) else {
        return syscall_failure(STATUS_OPERATION_NOT_READY);
    };

    endpoint_operation_result(result)
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
        || phoenix_vm::UserBuffer::for_array(metadata_address, 3, 8, 8).is_err()
    {
        return syscall_failure(STATUS_BAD_ARGUMENTS);
    }

    let endpoint_id = match resolve_current_endpoint(arguments.endpoint, Rights::READ) {
        Ok(endpoint_id) => endpoint_id,
        Err(EndpointCapabilityError::ContextNotInstalled) => {
            return syscall_failure(STATUS_OPERATION_NOT_READY);
        }
        Err(_) => return syscall_failure(STATUS_BAD_ARGUMENTS),
    };
    let Some(receiver) = current_process_id() else {
        return syscall_failure(STATUS_OPERATION_NOT_READY);
    };

    let Some(result) = with_current_endpoint_registry(|registry| {
        let endpoint = registry.get_mut(endpoint_id)?;
        if endpoint.owner().is_some_and(|owner| owner != receiver) {
            return Err(EndpointOperationError::WrongEndpointReceiver);
        }
        let message = endpoint.peek()?;
        let transferred_capability = message
            .transferred_capability()
            .map(|handle| PackedCapabilityHandle::new(handle.slot, handle.generation));
        let metadata = IpcReceiveMetadata::new(
            message.sender.0,
            message.len() as u64,
            transferred_capability,
        );
        with_current_process_address_space(|space| {
            ipc_user_memory::copy_receive_message(space, arguments, message.words(), metadata)
        })
        .ok_or(EndpointOperationError::ContextNotInstalled)??;
        let received = endpoint.receive()?;
        debug_assert_eq!(received, message);
        Ok::<_, EndpointOperationError>(message.len())
    }) else {
        return syscall_failure(STATUS_OPERATION_NOT_READY);
    };

    endpoint_operation_result(result)
}

fn endpoint_operation_result(result: Result<usize, EndpointOperationError>) -> SyscallReturn {
    match result {
        Ok(word_count) => SyscallReturn::success(word_count as u64),
        Err(EndpointOperationError::Transport(IpcError::QueueEmpty | IpcError::QueueFull))
        | Err(EndpointOperationError::Transfer(IpcSendError::Transport(IpcError::QueueFull)))
        | Err(EndpointOperationError::Transfer(IpcSendError::Capability(
            CapabilityError::CapacityExceeded,
        )))
        | Err(EndpointOperationError::ProcessRegistry(ProcessCapabilityRegistryError::NotFound))
        | Err(EndpointOperationError::MissingEndpointOwner)
        | Err(EndpointOperationError::ContextNotInstalled) => {
            syscall_failure(STATUS_OPERATION_NOT_READY)
        }
        Err(EndpointOperationError::Registry(_))
        | Err(EndpointOperationError::ProcessRegistry(_))
        | Err(EndpointOperationError::Transfer(_))
        | Err(EndpointOperationError::SameProcessTransfer)
        | Err(EndpointOperationError::WrongEndpointReceiver)
        | Err(EndpointOperationError::Transport(IpcError::TooManyWords))
        | Err(EndpointOperationError::UserMemory(_)) => syscall_failure(STATUS_BAD_ARGUMENTS),
    }
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

pub fn ipc_capability_transfer_self_test(send: IpcSendArguments) -> bool {
    let send_result = dispatch(SyscallRequest::ipc_send(send));
    send_result.is_success() && send_result.value == send.word_count
}

pub fn ipc_endpoint_operations_self_test(
    send: IpcSendArguments,
    unmapped_send: IpcSendArguments,
    denied_send: IpcSendArguments,
    receive: IpcReceiveArguments,
    expected_endpoint: EndpointId,
) -> bool {
    let send_result = dispatch(SyscallRequest::ipc_send(send));
    let unmapped_result = dispatch(SyscallRequest::ipc_send(unmapped_send));
    let denied_result = dispatch(SyscallRequest::ipc_send(denied_send));
    let receive_result = dispatch(SyscallRequest::ipc_receive(receive));
    let empty_result = dispatch(SyscallRequest::ipc_receive(receive));
    let resolved = resolve_current_endpoint(send.endpoint, Rights::WRITE);

    send_result.is_success()
        && send_result.value == send.word_count
        && unmapped_result.status.raw() == u64::from(STATUS_BAD_ARGUMENTS)
        && denied_result.status.raw() == u64::from(STATUS_BAD_ARGUMENTS)
        && receive_result.is_success()
        && receive_result.value == send.word_count
        && empty_result.status.raw() == u64::from(STATUS_OPERATION_NOT_READY)
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

pub fn process_exit_dispatch_self_test() -> bool {
    let status = 0xfedc_ba98_7654_3210;
    let valid_result = dispatch(SyscallRequest::process_exit(status));
    let invalid_result = dispatch(SyscallRequest::new(
        SYSCALL_PROCESS_EXIT,
        [status, 1, 0, 0, 0, 0],
    ));

    valid_result.is_success()
        && USER_PROCESS_EXITED.load(Ordering::Acquire)
        && USER_PROCESS_EXIT_STATUS.load(Ordering::Acquire) == status
        && invalid_result.status.raw() == u64::from(STATUS_BAD_ARGUMENTS)
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
