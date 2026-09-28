#![no_std]

pub const ABI_VERSION: u32 = 0;
pub const SYSCALL_ARGUMENT_COUNT: usize = 6;
pub const SYSCALL_IPC_SEND: u64 = 0x100;
pub const SYSCALL_IPC_RECEIVE: u64 = 0x101;
pub const SYSCALL_PROCESS_EXIT: u64 = 0x102;
pub const SYSCALL_FILE_OPEN: u64 = 0x110;
pub const SYSCALL_FILE_READ: u64 = 0x111;
pub const SYSCALL_FILE_WRITE: u64 = 0x112;
pub const SYSCALL_FILE_SEEK: u64 = 0x113;
pub const SYSCALL_FILE_CLOSE: u64 = 0x114;

pub const FILE_OPEN_READ: u64 = 1 << 0;
pub const FILE_OPEN_WRITE: u64 = 1 << 1;
pub const FILE_OPEN_CREATE: u64 = 1 << 2;
pub const FILE_OPEN_TRUNCATE: u64 = 1 << 3;

pub const FILE_SEEK_START: u64 = 0;
pub const FILE_SEEK_CURRENT: u64 = 1;
pub const FILE_SEEK_END: u64 = 2;
pub const NO_TRANSFERRED_CAPABILITY: u64 = u64::MAX;
pub const PROCESS_START_ABI_VERSION: u32 = 0;
pub const PROCESS_START_MAX_ENTRIES: u64 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStartInfoError {
    UnsupportedVersion,
    UnsupportedFlags,
    TooManyArguments,
    TooManyEnvironmentEntries,
    MissingArgumentVector,
    MissingEnvironmentVector,
    UnalignedArgumentVector,
    UnalignedEnvironmentVector,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessStartString {
    pub address: u64,
    pub length: u64,
}

impl ProcessStartString {
    pub const fn new(address: u64, length: u64) -> Self {
        Self { address, length }
    }
}

/// Stable process-entry block passed in RDI on x86-64.
///
/// Argument and environment vectors point to arrays of `ProcessStartString`.
/// Strings are byte spans and do not require a trailing NUL byte.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessStartInfo {
    pub abi_version: u32,
    pub flags: u32,
    pub argument_count: u64,
    pub argument_vector: u64,
    pub environment_count: u64,
    pub environment_vector: u64,
}

impl ProcessStartInfo {
    pub const fn empty() -> Self {
        Self {
            abi_version: PROCESS_START_ABI_VERSION,
            flags: 0,
            argument_count: 0,
            argument_vector: 0,
            environment_count: 0,
            environment_vector: 0,
        }
    }

    pub const fn new(
        argument_count: u64,
        argument_vector: u64,
        environment_count: u64,
        environment_vector: u64,
    ) -> Result<Self, ProcessStartInfoError> {
        let info = Self {
            abi_version: PROCESS_START_ABI_VERSION,
            flags: 0,
            argument_count,
            argument_vector,
            environment_count,
            environment_vector,
        };
        match info.validate() {
            Ok(()) => Ok(info),
            Err(error) => Err(error),
        }
    }

    pub const fn validate(self) -> Result<(), ProcessStartInfoError> {
        if self.abi_version != PROCESS_START_ABI_VERSION {
            return Err(ProcessStartInfoError::UnsupportedVersion);
        }
        if self.flags != 0 {
            return Err(ProcessStartInfoError::UnsupportedFlags);
        }
        if self.argument_count > PROCESS_START_MAX_ENTRIES {
            return Err(ProcessStartInfoError::TooManyArguments);
        }
        if self.environment_count > PROCESS_START_MAX_ENTRIES {
            return Err(ProcessStartInfoError::TooManyEnvironmentEntries);
        }
        if self.argument_count != 0 && self.argument_vector == 0 {
            return Err(ProcessStartInfoError::MissingArgumentVector);
        }
        if self.environment_count != 0 && self.environment_vector == 0 {
            return Err(ProcessStartInfoError::MissingEnvironmentVector);
        }
        if self.argument_vector & 7 != 0 {
            return Err(ProcessStartInfoError::UnalignedArgumentVector);
        }
        if self.environment_vector & 7 != 0 {
            return Err(ProcessStartInfoError::UnalignedEnvironmentVector);
        }
        Ok(())
    }

    pub const fn words(self) -> [u64; 5] {
        [
            (self.flags as u64) << 32 | self.abi_version as u64,
            self.argument_count,
            self.argument_vector,
            self.environment_count,
            self.environment_vector,
        ]
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedFileDescriptor {
    pub slot: u32,
    pub generation: u32,
}

impl PackedFileDescriptor {
    pub const fn new(slot: u32, generation: u32) -> Self {
        Self { slot, generation }
    }

    pub const fn raw(self) -> u64 {
        (self.generation as u64) << 32 | self.slot as u64
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self {
            slot: raw as u32,
            generation: (raw >> 32) as u32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileOpenArguments {
    pub path_address: u64,
    pub path_length: u64,
    pub flags: u64,
}

impl FileOpenArguments {
    pub const fn registers(self) -> [u64; SYSCALL_ARGUMENT_COUNT] {
        [self.path_address, self.path_length, self.flags, 0, 0, 0]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIoArguments {
    pub descriptor: PackedFileDescriptor,
    pub buffer_address: u64,
    pub buffer_length: u64,
}

impl FileIoArguments {
    pub const fn registers(self) -> [u64; SYSCALL_ARGUMENT_COUNT] {
        [
            self.descriptor.raw(),
            self.buffer_address,
            self.buffer_length,
            0,
            0,
            0,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSeekArguments {
    pub descriptor: PackedFileDescriptor,
    pub offset: i64,
    pub origin: u64,
}

impl FileSeekArguments {
    pub const fn registers(self) -> [u64; SYSCALL_ARGUMENT_COUNT] {
        [
            self.descriptor.raw(),
            self.offset as u64,
            self.origin,
            0,
            0,
            0,
        ]
    }
}

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

    pub const fn file_open(arguments: FileOpenArguments) -> Self {
        Self::new(SYSCALL_FILE_OPEN, arguments.registers())
    }

    pub const fn file_read(arguments: FileIoArguments) -> Self {
        Self::new(SYSCALL_FILE_READ, arguments.registers())
    }

    pub const fn file_write(arguments: FileIoArguments) -> Self {
        Self::new(SYSCALL_FILE_WRITE, arguments.registers())
    }

    pub const fn file_seek(arguments: FileSeekArguments) -> Self {
        Self::new(SYSCALL_FILE_SEEK, arguments.registers())
    }

    pub const fn file_close(descriptor: PackedFileDescriptor) -> Self {
        Self::new(SYSCALL_FILE_CLOSE, [descriptor.raw(), 0, 0, 0, 0, 0])
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
    fn process_start_info_has_stable_c_layout() {
        assert_eq!(core::mem::size_of::<ProcessStartString>(), 16);
        assert_eq!(core::mem::align_of::<ProcessStartString>(), 8);
        assert_eq!(core::mem::size_of::<ProcessStartInfo>(), 40);
        assert_eq!(core::mem::align_of::<ProcessStartInfo>(), 8);

        let info = ProcessStartInfo::new(2, 0x4000, 1, 0x5000).unwrap();
        assert_eq!(info.words(), [0, 2, 0x4000, 1, 0x5000]);
    }

    #[test]
    fn process_start_info_rejects_invalid_vectors_and_limits() {
        assert_eq!(
            ProcessStartInfo::new(1, 0, 0, 0),
            Err(ProcessStartInfoError::MissingArgumentVector)
        );
        assert_eq!(
            ProcessStartInfo::new(0, 0, 1, 0),
            Err(ProcessStartInfoError::MissingEnvironmentVector)
        );
        assert_eq!(
            ProcessStartInfo::new(PROCESS_START_MAX_ENTRIES + 1, 8, 0, 0),
            Err(ProcessStartInfoError::TooManyArguments)
        );
        assert_eq!(
            ProcessStartInfo::new(1, 3, 0, 0),
            Err(ProcessStartInfoError::UnalignedArgumentVector)
        );
    }

    #[test]
    fn process_exit_uses_status_and_zeroes_reserved_arguments() {
        let request = SyscallRequest::process_exit(0x1234_5678_9abc_def0);

        assert_eq!(request.number, SYSCALL_PROCESS_EXIT);
        assert_eq!(request.arguments, [0x1234_5678_9abc_def0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn file_descriptor_has_stable_c_encoding() {
        let descriptor = PackedFileDescriptor::new(0x1122_3344, 0x5566_7788);

        assert_eq!(core::mem::size_of::<PackedFileDescriptor>(), 8);
        assert_eq!(descriptor.raw(), 0x5566_7788_1122_3344);
        assert_eq!(PackedFileDescriptor::from_raw(descriptor.raw()), descriptor);
    }

    #[test]
    fn file_syscalls_keep_reserved_registers_zero() {
        let descriptor = PackedFileDescriptor::new(3, 7);
        assert_eq!(
            SyscallRequest::file_open(FileOpenArguments {
                path_address: 0x4000,
                path_length: 9,
                flags: FILE_OPEN_READ | FILE_OPEN_WRITE,
            }).arguments,
            [0x4000, 9, 3, 0, 0, 0]
        );
        assert_eq!(
            SyscallRequest::file_read(FileIoArguments {
                descriptor,
                buffer_address: 0x5000,
                buffer_length: 32,
            }).arguments,
            [descriptor.raw(), 0x5000, 32, 0, 0, 0]
        );
        assert_eq!(
            SyscallRequest::file_seek(FileSeekArguments {
                descriptor,
                offset: -4,
                origin: FILE_SEEK_END,
            }).arguments,
            [descriptor.raw(), (-4_i64) as u64, FILE_SEEK_END, 0, 0, 0]
        );
        assert_eq!(
            SyscallRequest::file_close(descriptor).arguments,
            [descriptor.raw(), 0, 0, 0, 0, 0]
        );
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
