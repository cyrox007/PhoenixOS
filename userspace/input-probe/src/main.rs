#![no_std]
#![no_main]

//! Проверка передачи реальных USB-событий из ядра пользовательской программе.
//! Специальный исполняемый образ для QEMU, не постоянно работающая служба.

use core::panic::PanicInfo;

use phoenix_syscall_abi::{
    INPUT_DEVICE_CONNECTED, INPUT_DEVICE_DISCONNECTED, INPUT_EVENT_DEVICE, INPUT_EVENT_KEY,
    INPUT_EVENT_MOUSE, InputEventRecord, PackedCapabilityHandle, ProcessStartInfo,
    ProcessStartString, SYSCALL_STATUS_ACCESS_DENIED, SYSCALL_STATUS_OPERATION_NOT_READY,
};
use phoenix_user_runtime::{console_write, input_read, process_exit};

const SUCCESS: u64 = 0x5553_4552_5f4f_4b21;
const FAILURE_ARGUMENTS: u64 = 1;
const FAILURE_PERMISSIONS: u64 = 2;
const FAILURE_RECEIVE: u64 = 3;
const FAILURE_CONTENT: u64 = 4;
const FAILURE_EMPTY: u64 = 5;
const FAILURE_CONSOLE: u64 = 6;
const FAILURE_PANIC: u64 = 126;

#[unsafe(no_mangle)]
pub extern "C" fn _start(info: *const ProcessStartInfo) -> ! {
    let Some(info) = (unsafe { info.as_ref() }) else {
        process_exit(FAILURE_ARGUMENTS);
    };
    phoenix_user_runtime::start_with_info(program_main, info)
}

extern "C" fn program_main(info: *const ProcessStartInfo) -> u64 {
    let Some(info) = (unsafe { info.as_ref() }) else {
        return FAILURE_ARGUMENTS;
    };
    if info.validate().is_err() || info.argument_count != 1 || info.environment_count != 0 {
        return FAILURE_ARGUMENTS;
    }
    let Some(capability) = parse_input_capability(info) else {
        return FAILURE_ARGUMENTS;
    };

    let mut event = InputEventRecord::EMPTY;
    let forbidden = PackedCapabilityHandle::new(u32::MAX, u32::MAX);
    match input_read(forbidden, &mut event) {
        Err(error) if error.status() == u64::from(SYSCALL_STATUS_ACCESS_DENIED) => {}
        _ => return FAILURE_PERMISSIONS,
    }
    if console_write(b"input userspace denied: OK\n").is_err() {
        return FAILURE_CONSOLE;
    }

    let mut received = [InputEventRecord::EMPTY; 5];
    for event in &mut received {
        if input_read(capability, event).is_err() {
            return FAILURE_RECEIVE;
        }
    }
    for (index, event) in received.iter().enumerate() {
        if event.sequence != index as u64 + 1 || event.lost != 0 {
            return FAILURE_CONTENT;
        }
    }

    let disconnect = received[0];
    let connect = received[1];
    let down = received[2];
    let up = received[3];
    let mouse = received[4];

    if disconnect.kind != INPUT_EVENT_DEVICE
        || disconnect.code != INPUT_DEVICE_DISCONNECTED
        || connect.kind != INPUT_EVENT_DEVICE
        || connect.code != INPUT_DEVICE_CONNECTED
        || disconnect.source != connect.source
        || down.kind != INPUT_EVENT_KEY
        || up.kind != INPUT_EVENT_KEY
        || down.source != connect.source
        || up.source != connect.source
        || down.code != 4
        || up.code != 4
        || down.value != 1
        || up.value != 0
        || mouse.kind != INPUT_EVENT_MOUSE
        || mouse.source == connect.source
        || mouse.x != 12
        || mouse.y != -6
    {
        return FAILURE_CONTENT;
    }
    if console_write(b"input userspace events: OK count=5 key=4 dx=12 dy=-6\n").is_err() {
        return FAILURE_CONSOLE;
    }

    match input_read(capability, &mut event) {
        Err(error) if error.status() == u64::from(SYSCALL_STATUS_OPERATION_NOT_READY) => {}
        _ => return FAILURE_EMPTY,
    }
    if console_write(b"input userspace empty: OK\n").is_err() {
        return FAILURE_CONSOLE;
    }
    SUCCESS
}

fn parse_input_capability(info: &ProcessStartInfo) -> Option<PackedCapabilityHandle> {
    let pointer = info.argument_vector as *const ProcessStartString;
    let argument = unsafe { pointer.read() };
    if argument.length != 16 || argument.address == 0 {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(argument.address as *const u8, 16) };
    let mut raw = 0_u64;
    for &digit in bytes {
        let nibble = match digit {
            b'0'..=b'9' => u64::from(digit - b'0'),
            b'a'..=b'f' => u64::from(digit - b'a' + 10),
            b'A'..=b'F' => u64::from(digit - b'A' + 10),
            _ => return None,
        };
        raw = (raw << 4) | nibble;
    }
    Some(PackedCapabilityHandle::from_raw(raw))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILURE_PANIC)
}
