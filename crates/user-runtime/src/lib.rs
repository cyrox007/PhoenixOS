#![no_std]

use core::arch::asm;

use phoenix_syscall_abi::{
    IpcReceiveArguments, IpcReceiveMetadata, IpcSendArguments, PackedCapabilityHandle,
    SyscallRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyscallError {
    status: u64,
}

impl SyscallError {
    pub const fn status(self) -> u64 {
        self.status
    }
}

pub fn ipc_send(
    endpoint: PackedCapabilityHandle,
    words: &[u64],
    transferred_capability: Option<PackedCapabilityHandle>,
) -> Result<u64, SyscallError> {
    invoke(build_ipc_send_request(
        endpoint,
        words,
        transferred_capability,
    ))
}

pub fn ipc_receive(
    endpoint: PackedCapabilityHandle,
    words: &mut [u64],
    metadata: &mut IpcReceiveMetadata,
) -> Result<u64, SyscallError> {
    invoke(build_ipc_receive_request(endpoint, words, metadata))
}

fn build_ipc_send_request(
    endpoint: PackedCapabilityHandle,
    words: &[u64],
    transferred_capability: Option<PackedCapabilityHandle>,
) -> SyscallRequest {
    SyscallRequest::ipc_send(IpcSendArguments {
        endpoint,
        words_address: words.as_ptr() as u64,
        word_count: words.len() as u64,
        transferred_capability,
        flags: 0,
    })
}

fn build_ipc_receive_request(
    endpoint: PackedCapabilityHandle,
    words: &mut [u64],
    metadata: &mut IpcReceiveMetadata,
) -> SyscallRequest {
    SyscallRequest::ipc_receive(IpcReceiveArguments {
        endpoint,
        words_address: words.as_mut_ptr() as u64,
        word_capacity: words.len() as u64,
        metadata_address: metadata as *mut IpcReceiveMetadata as u64,
        flags: 0,
    })
}

#[inline(always)]
fn invoke(request: SyscallRequest) -> Result<u64, SyscallError> {
    let arguments = request.arguments;
    let mut value = request.number;
    let mut status = arguments[2];

    unsafe {
        asm!(
            "syscall",
            inlateout("rax") value,
            in("rdi") arguments[0],
            in("rsi") arguments[1],
            inlateout("rdx") status,
            in("r10") arguments[3],
            in("r8") arguments[4],
            in("r9") arguments[5],
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }

    if status == 0 {
        return Ok(value);
    }

    Err(SyscallError { status })
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoenix_syscall_abi::{NO_TRANSFERRED_CAPABILITY, SYSCALL_IPC_RECEIVE, SYSCALL_IPC_SEND};

    #[test]
    fn send_wrapper_builds_stable_register_contract() {
        let words = [0x11_u64, 0x22];
        let endpoint = PackedCapabilityHandle::new(3, 7);
        let transfer = PackedCapabilityHandle::new(5, 9);
        let request = build_ipc_send_request(endpoint, &words, Some(transfer));

        assert_eq!(request.number, SYSCALL_IPC_SEND);
        assert_eq!(request.arguments[0], endpoint.raw());
        assert_eq!(request.arguments[1], words.as_ptr() as u64);
        assert_eq!(request.arguments[2], 2);
        assert_eq!(request.arguments[3], transfer.raw());
        assert_eq!(request.arguments[4], 0);
        assert_eq!(request.arguments[5], 0);
    }

    #[test]
    fn send_wrapper_marks_missing_transfer_explicitly() {
        let words = [1_u64];
        let request = build_ipc_send_request(PackedCapabilityHandle::new(1, 1), &words, None);

        assert_eq!(request.arguments[3], NO_TRANSFERRED_CAPABILITY);
    }

    #[test]
    fn receive_wrapper_passes_mutable_buffers_and_metadata() {
        let endpoint = PackedCapabilityHandle::new(4, 8);
        let mut words = [0_u64; 6];
        let mut metadata = IpcReceiveMetadata::new(0, 0, None);
        let words_address = words.as_mut_ptr() as u64;
        let metadata_address = &mut metadata as *mut IpcReceiveMetadata as u64;
        let request = build_ipc_receive_request(endpoint, &mut words, &mut metadata);

        assert_eq!(request.number, SYSCALL_IPC_RECEIVE);
        assert_eq!(request.arguments[0], endpoint.raw());
        assert_eq!(request.arguments[1], words_address);
        assert_eq!(request.arguments[2], 6);
        assert_eq!(request.arguments[3], metadata_address);
        assert_eq!(request.arguments[4], 0);
        assert_eq!(request.arguments[5], 0);
    }
}
