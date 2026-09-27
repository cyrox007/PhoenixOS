#![no_std]

pub const ABI_VERSION: u32 = 0;
pub const SYSCALL_ARGUMENT_COUNT: usize = 6;
pub const SYSCALL_IPC_SEND: u64 = 0x100;
pub const SYSCALL_IPC_RECEIVE: u64 = 0x101;
pub const SYSCALL_PROCESS_EXIT: u64 = 0x102;
pub const NO_TRANSFERRED_CAPABILITY: u64 = u64::MAX;

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedCapabilityHandle(u64);

impl PackedCapabilityHandle {
    pub const fn new(slot: u32, generation: u32) -> Self {
        Self((generation as u64) << 32 | slot as u64)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn slot(self) -> u32 {
        self.0 as u32
    }

    pub const fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcSendArguments {
    pub endpoint: PackedCapabilityHandle,
    pub words_address: u64,
    pub word_count: u64,
    pub transferred_capability: Option<PackedCapabilityHandle>,
    pub flags: u64,
}

impl IpcSendArguments {
    pub const fn registers(self) -> [u64; SYSCALL_ARGUMENT_COUNT] {
        [
            self.endpoint.raw(),
            self.words_address,
            self.word_count,
            match self.transferred_capability {
                Some(handle) => handle.raw(),
                None => NO_TRANSFERRED_CAPABILITY,
            },
            self.flags,
            0,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcReceiveArguments {
    pub endpoint: PackedCapabilityHandle,
    pub words_address: u64,
    pub word_capacity: u64,
    pub metadata_address: u64,
    pub flags: u64,
}

impl IpcReceiveArguments {
    pub const fn registers(self) -> [u64; SYSCALL_ARGUMENT_COUNT] {
        [
            self.endpoint.raw(),
            self.words_address,
            self.word_capacity,
            self.metadata_address,
            self.flags,
            0,
        ]
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcReceiveMetadata {
    pub sender_process_id: u64,
    pub word_count: u64,
    pub transferred_capability: u64,
}

impl IpcReceiveMetadata {
    pub const fn new(
        sender_process_id: u64,
        word_count: u64,
        transferred_capability: Option<PackedCapabilityHandle>,
    ) -> Self {
        Self {
            sender_process_id,
            word_count,
            transferred_capability: match transferred_capability {
                Some(handle) => handle.raw(),
                None => NO_TRANSFERRED_CAPABILITY,
            },
        }
    }

    pub const fn words(self) -> [u64; 3] {
        [
            self.sender_process_id,
            self.word_count,
            self.transferred_capability,
        ]
    }
}

/// x86-64 register contract for entering a PhoenixOS system call.
///
/// Hardware mapping:
/// - RAX: syscall number;
/// - RDI, RSI, RDX, R10, R8, R9: arguments 0..5.
///
/// RCX and R11 are clobbered by the x86-64 SYSCALL/SYSRET mechanism and are not
/// part of the argument set.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyscallRequest {
    pub number: u64,
    pub arguments: [u64; SYSCALL_ARGUMENT_COUNT],
}

impl SyscallRequest {
    pub const fn new(number: u64, arguments: [u64; SYSCALL_ARGUMENT_COUNT]) -> Self {
        Self { number, arguments }
    }

    pub const fn ipc_send(arguments: IpcSendArguments) -> Self {
        Self::new(SYSCALL_IPC_SEND, arguments.registers())
    }

    pub const fn ipc_receive(arguments: IpcReceiveArguments) -> Self {
        Self::new(SYSCALL_IPC_RECEIVE, arguments.registers())
    }

    pub const fn process_exit(status: u64) -> Self {
        Self::new(SYSCALL_PROCESS_EXIT, [status, 0, 0, 0, 0, 0])
    }

    pub const fn argument(&self, index: usize) -> Option<u64> {
        if index < SYSCALL_ARGUMENT_COUNT {
            Some(self.arguments[index])
        } else {
            None
        }
    }
}

/// Result returned by a PhoenixOS system call.
///
/// Hardware mapping on x86-64:
/// - RAX: value;
/// - RDX: status.
///
/// Status zero means success. A non-zero status is an error code. Keeping value
/// and status in separate registers avoids reserving part of the 64-bit value
/// space for negative/error sentinels.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyscallReturn {
    pub value: u64,
    pub status: SyscallStatus,
}

impl SyscallReturn {
    pub const fn success(value: u64) -> Self {
        Self {
            value,
            status: SyscallStatus::OK,
        }
    }

    pub const fn failure(status: SyscallStatus) -> Self {
        Self { value: 0, status }
    }

    pub const fn is_success(self) -> bool {
        self.status.is_ok()
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyscallStatus(u64);

impl SyscallStatus {
    pub const OK: Self = Self(0);

    pub const fn from_error_code(code: u32) -> Option<Self> {
        if code == 0 {
            None
        } else {
            Some(Self(code as u64))
        }
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn is_ok(self) -> bool {
        self.0 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_keeps_six_arguments_in_abi_order() {
        let request = SyscallRequest::new(42, [10, 20, 30, 40, 50, 60]);

        assert_eq!(request.number, 42);
        assert_eq!(request.argument(0), Some(10));
        assert_eq!(request.argument(3), Some(40));
        assert_eq!(request.argument(5), Some(60));
        assert_eq!(request.argument(6), None);
    }

    #[test]
    fn process_exit_uses_status_and_zeroes_reserved_arguments() {
        let request = SyscallRequest::process_exit(0x1234_5678_9abc_def0);

        assert_eq!(request.number, SYSCALL_PROCESS_EXIT);
        assert_eq!(request.arguments, [0x1234_5678_9abc_def0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn capability_handle_has_stable_register_encoding() {
        let handle = PackedCapabilityHandle::new(0x1122_3344, 0x5566_7788);

        assert_eq!(handle.raw(), 0x5566_7788_1122_3344);
        assert_eq!(handle.slot(), 0x1122_3344);
        assert_eq!(handle.generation(), 0x5566_7788);
        assert_eq!(PackedCapabilityHandle::from_raw(handle.raw()), handle);
    }

    #[test]
    fn ipc_send_uses_six_register_contract() {
        let request = SyscallRequest::ipc_send(IpcSendArguments {
            endpoint: PackedCapabilityHandle::new(3, 7),
            words_address: 0x4000,
            word_count: 6,
            transferred_capability: None,
            flags: 2,
        });

        assert_eq!(request.number, SYSCALL_IPC_SEND);
        assert_eq!(
            request.arguments,
            [
                0x0000_0007_0000_0003,
                0x4000,
                6,
                NO_TRANSFERRED_CAPABILITY,
                2,
                0,
            ]
        );
    }

    #[test]
    fn ipc_receive_keeps_metadata_pointer_and_reserved_zero() {
        let request = SyscallRequest::ipc_receive(IpcReceiveArguments {
            endpoint: PackedCapabilityHandle::new(9, 4),
            words_address: 0x5000,
            word_capacity: 6,
            metadata_address: 0x6000,
            flags: 1,
        });

        assert_eq!(request.number, SYSCALL_IPC_RECEIVE);
        assert_eq!(
            request.arguments,
            [0x0000_0004_0000_0009, 0x5000, 6, 0x6000, 1, 0]
        );
    }

    #[test]
    fn ipc_receive_metadata_has_stable_c_layout_words() {
        let metadata = IpcReceiveMetadata::new(
            0x1122_3344_5566_7788,
            2,
            Some(PackedCapabilityHandle::new(3, 7)),
        );

        assert_eq!(core::mem::size_of::<IpcReceiveMetadata>(), 24);
        assert_eq!(
            metadata.words(),
            [0x1122_3344_5566_7788, 2, 0x0000_0007_0000_0003]
        );
        assert_eq!(
            IpcReceiveMetadata::new(1, 0, None).transferred_capability,
            NO_TRANSFERRED_CAPABILITY
        );
    }

    #[test]
    fn success_keeps_full_value_space() {
        let result = SyscallReturn::success(u64::MAX);

        assert!(result.is_success());
        assert_eq!(result.value, u64::MAX);
        assert_eq!(result.status.raw(), 0);
    }

    #[test]
    fn failure_uses_separate_nonzero_status() {
        let status = SyscallStatus::from_error_code(7).unwrap();
        let result = SyscallReturn::failure(status);

        assert!(!result.is_success());
        assert_eq!(result.value, 0);
        assert_eq!(result.status.raw(), 7);
        assert_eq!(SyscallStatus::from_error_code(0), None);
    }
}
