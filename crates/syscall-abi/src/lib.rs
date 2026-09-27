#![no_std]

pub const ABI_VERSION: u32 = 0;
pub const SYSCALL_ARGUMENT_COUNT: usize = 6;

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
