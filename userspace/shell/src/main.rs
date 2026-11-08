#![no_std]
#![no_main]

use core::panic::PanicInfo;

use phoenix_shell::{ShellIo, execute, parse};
use phoenix_syscall_abi::{
    InputEventRecord, PackedCapabilityHandle, ProcessStartInfo, ProcessStartString,
};
use phoenix_user_runtime::{
    OpenOptions, console_write, file_close, file_open, file_read, input_read,
};

const FAILURE_START_INFO: u64 = 2;
const FAILURE_USAGE: u64 = 2;
const FAILURE_UTF8: u64 = 2;
const FAILURE_PANIC: u64 = 126;

struct RuntimeIo {
    input_capability: Option<PackedCapabilityHandle>,
}

impl ShellIo for RuntimeIo {
    fn write(&mut self, bytes: &[u8]) -> Result<(), ()> {
        console_write(bytes).map(|_| ()).map_err(|_| ())
    }

    fn cat(&mut self, path: &str) -> Result<(), ()> {
        let descriptor = file_open(path, OpenOptions::read_only()).map_err(|_| ())?;
        let mut buffer = [0_u8; 256];
        let mut result = Ok(());

        loop {
            let read = match file_read(descriptor, &mut buffer) {
                Ok(read) => read,
                Err(_) => {
                    result = Err(());
                    break;
                }
            };
            if read == 0 {
                break;
            }
            if console_write(&buffer[..read]).is_err() {
                result = Err(());
                break;
            }
        }

        let close = file_close(descriptor).map_err(|_| ());
        result.and(close)
    }

    fn read_input(&mut self) -> Result<InputEventRecord, ()> {
        let capability = self.input_capability.ok_or(())?;
        let mut event = InputEventRecord::EMPTY;
        input_read(capability, &mut event).map_err(|_| ())?;
        Ok(event)
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

    let mut io = RuntimeIo {
        input_capability: input_capability_from_environment(info),
    };
    execute(command, &mut io) as u64
}

/// Возможность чтения передаётся только при запуске авторизованной
/// оболочки. Наличие имени переменной само по себе не даёт полномочий:
/// ядро повторно проверяет поколение, тип объекта и право READ.
fn input_capability_from_environment(info: &ProcessStartInfo) -> Option<PackedCapabilityHandle> {
    if info.environment_count == 0 || info.environment_vector == 0 {
        return None;
    }
    for index in 0..info.environment_count as usize {
        let argument = unsafe {
            (info.environment_vector as *const ProcessStartString)
                .add(index)
                .read()
        };
        if argument.address == 0 || argument.length > 128 {
            continue;
        }
        let bytes =
            unsafe { core::slice::from_raw_parts(argument.address as *const u8, argument.length as usize) };
        if let Some(value) = bytes.strip_prefix(b"PHOENIX_INPUT_CAP=") {
            return PackedCapabilityHandle::parse_hex(value);
        }
    }
    None
}

fn process_argument(info: &ProcessStartInfo, index: usize) -> Result<&str, ()> {
    if index >= info.argument_count as usize {
        return Err(());
    }
    let entry = unsafe {
        (info.argument_vector as *const ProcessStartString)
            .add(index)
            .read()
    };
    if entry.address == 0 {
        return Err(());
    }
    let bytes =
        unsafe { core::slice::from_raw_parts(entry.address as *const u8, entry.length as usize) };
    core::str::from_utf8(bytes).map_err(|_| ())
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    phoenix_user_runtime::process_exit(FAILURE_PANIC)
}
