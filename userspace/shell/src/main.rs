#![no_std]
#![no_main]

use core::panic::PanicInfo;

use phoenix_shell::{ShellIo, execute, parse};
use phoenix_syscall_abi::{ProcessStartInfo, ProcessStartString};
use phoenix_user_runtime::{OpenOptions, console_write, file_close, file_open, file_read};

const FAILURE_START_INFO: u64 = 2;
const FAILURE_USAGE: u64 = 2;
const FAILURE_UTF8: u64 = 2;
const FAILURE_PANIC: u64 = 126;

struct RuntimeIo;

impl ShellIo for RuntimeIo {
    fn write(&mut self, bytes: &[u8]) -> Result<(), ()> {
        console_write(bytes).map(|_| ()).map_err(|_| ())
    }

    fn cat(&mut self, path: &str) -> Result<(), ()> {
        let descriptor = file_open(path, OpenOptions::read_only()).map_err(|_| ())?;
        let mut buffer = [0_u8; 256];
        let result = loop {
            let read = file_read(descriptor, &mut buffer).map_err(|_| ())?;
            if read == 0 {
                break Ok(());
            }
            console_write(&buffer[..read]).map_err(|_| ())?;
        };
        let close = file_close(descriptor).map_err(|_| ());
        result.and(close)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(info: *const ProcessStartInfo) -> ! {
    let Some(info) = (unsafe { info.as_ref() }) else {
        phoenix_user_runtime::process_exit(FAILURE_START_INFO);
    };
    phoenix_user_runtime::start_with_info(program_main, info)
}

extern "C" fn program_main(info: *const ProcessStartInfo) -> u64 {
    let Some(info) = (unsafe { info.as_ref() }) else {
        return FAILURE_START_INFO;
    };
    if info.validate().is_err() || info.argument_count != 3 {
        return FAILURE_USAGE;
    }

    let Ok(option) = process_argument(info, 1) else {
        return FAILURE_UTF8;
    };
    if option != "-c" {
        return FAILURE_USAGE;
    }
    let Ok(command_line) = process_argument(info, 2) else {
        return FAILURE_UTF8;
    };
    let Ok(command) = parse(command_line) else {
        return FAILURE_USAGE;
    };

    let mut io = RuntimeIo;
    execute(command, &mut io) as u64
}

fn process_argument(info: &ProcessStartInfo, index: usize) -> Result<&str, ()> {
    if index >= info.argument_count as usize {
        return Err(());
    }
    let entry = unsafe { (info.argument_vector as *const ProcessStartString).add(index).read() };
    if entry.address == 0 {
        return Err(());
    }
    let bytes = unsafe { core::slice::from_raw_parts(entry.address as *const u8, entry.length as usize) };
    core::str::from_utf8(bytes).map_err(|_| ())
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    phoenix_user_runtime::process_exit(FAILURE_PANIC)
}
