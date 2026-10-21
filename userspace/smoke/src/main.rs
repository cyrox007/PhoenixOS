#![no_std]
#![no_main]

use core::panic::PanicInfo;
use phoenix_syscall_abi::{ProcessStartInfo, ProcessStartString};

const SUCCESS: u64 = 0x5553_4552_5f4f_4b21;
const FAILURE_START_INFO: u64 = 1;
const FAILURE_ARGUMENTS: u64 = 2;
const FAILURE_ENVIRONMENT: u64 = 3;
const FAILURE_CONSOLE: u64 = 4;
const FAILURE_PANIC: u64 = 0xffff_ffff_ffff_fffe;

const ARGUMENT_ZERO: &[u8] = b"phoenix-test";
const ARGUMENT_ONE: &[u8] = b"--self-test";
const ENVIRONMENT_ZERO: &[u8] = b"MODE=qemu";

#[unsafe(no_mangle)]
pub extern "C" fn _start(info: *const ProcessStartInfo) -> ! {
    let Some(info_ref) = (unsafe { info.as_ref() }) else {
        phoenix_user_runtime::process_exit(FAILURE_START_INFO);
    };

    phoenix_user_runtime::start_with_info(program_main, info_ref)
}

extern "C" fn program_main(info: *const ProcessStartInfo) -> u64 {
    let Some(info) = (unsafe { info.as_ref() }) else {
        return FAILURE_START_INFO;
    };
    if info.validate().is_err() || info.argument_count != 2 || info.environment_count != 1 {
        return FAILURE_START_INFO;
    }

    if !vector_entry_matches(info.argument_vector, 0, ARGUMENT_ZERO)
        || !vector_entry_matches(info.argument_vector, 1, ARGUMENT_ONE)
    {
        return FAILURE_ARGUMENTS;
    }

    if !vector_entry_matches(info.environment_vector, 0, ENVIRONMENT_ZERO) {
        return FAILURE_ENVIRONMENT;
    }

    if phoenix_user_runtime::console_write(b"userspace console self-test: OK\n").is_err() {
        return FAILURE_CONSOLE;
    }

    SUCCESS
}

fn vector_entry_matches(vector: u64, index: usize, expected: &[u8]) -> bool {
    if vector == 0 {
        return false;
    }

    let entry = unsafe { (vector as *const ProcessStartString).add(index).read() };
    if entry.length != expected.len() as u64 || entry.address == 0 {
        return false;
    }

    let bytes = unsafe { core::slice::from_raw_parts(entry.address as *const u8, expected.len()) };
    bytes == expected
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    phoenix_user_runtime::process_exit(FAILURE_PANIC)
}
