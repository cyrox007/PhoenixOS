#![no_std]

use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use phoenix_syscall_abi::PackedFileDescriptor;
use phoenix_user_runtime::{
    OpenOptions, SeekFrom, errno_from_status, file_close, file_open, file_read, file_seek,
    file_write,
};

const FD_BASE: c_int = 3;
const FD_CAPACITY: usize = 32;
const PATH_CAPACITY: usize = 255;

pub const O_RDONLY: c_int = 0;
pub const O_WRONLY: c_int = 1;
pub const O_RDWR: c_int = 2;
pub const O_CREAT: c_int = 0x40;
pub const O_TRUNC: c_int = 0x200;

pub const SEEK_SET: c_int = 0;
pub const SEEK_CUR: c_int = 1;
pub const SEEK_END: c_int = 2;

pub const EBADF: c_int = 9;
pub const EINVAL: c_int = 22;
pub const EMFILE: c_int = 24;

static ERRNO: AtomicI32 = AtomicI32::new(0);
static DESCRIPTORS: [AtomicU64; FD_CAPACITY] = [const { AtomicU64::new(0) }; FD_CAPACITY];

#[unsafe(no_mangle)]
pub extern "C" fn __phoenix_errno_location() -> *mut c_int {
    ERRNO.as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phoenix_libc_open(path: *const c_char, flags: c_int) -> c_int {
    let Some(length) = (unsafe { c_string_length(path, PATH_CAPACITY) }) else {
        return fail(EINVAL);
    };
    let bytes = unsafe { core::slice::from_raw_parts(path.cast::<u8>(), length) };
    let Ok(path) = core::str::from_utf8(bytes) else {
        return fail(EINVAL);
    };
    let Some(options) = open_options(flags) else {
        return fail(EINVAL);
    };

    match file_open(path, options) {
        Ok(descriptor) => install_descriptor(descriptor),
        Err(error) => fail(errno_from_status(error.status())),
    }
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize {
    if count != 0 && buffer.is_null() {
        return fail_ssize(EINVAL);
    }
    let Some(descriptor) = descriptor(fd) else {
        return fail_ssize(EBADF);
    };
    let output = unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u8>(), count) };

    match file_read(descriptor, output) {
        Ok(read) => read as isize,
        Err(error) => fail_ssize(errno_from_status(error.status())),
    }
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn write(fd: c_int, buffer: *const c_void, count: usize) -> isize {
    if count != 0 && buffer.is_null() {
        return fail_ssize(EINVAL);
    }
    let Some(descriptor) = descriptor(fd) else {
        return fail_ssize(EBADF);
    };
    let input = unsafe { core::slice::from_raw_parts(buffer.cast::<u8>(), count) };

    match file_write(descriptor, input) {
        Ok(written) => written as isize,
        Err(error) => fail_ssize(errno_from_status(error.status())),
    }
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64 {
    let Some(descriptor) = descriptor(fd) else {
        return fail_off(EBADF);
    };
    let from = match whence {
        SEEK_SET => SeekFrom::Start(offset),
        SEEK_CUR => SeekFrom::Current(offset),
        SEEK_END => SeekFrom::End(offset),
        _ => return fail_off(EINVAL),
    };

    match file_seek(descriptor, from) {
        Ok(position) => i64::try_from(position).unwrap_or_else(|_| fail_off(EINVAL)),
        Err(error) => fail_off(errno_from_status(error.status())),
    }
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn close(fd: c_int) -> c_int {
    let Some(index) = descriptor_index(fd) else {
        return fail(EBADF);
    };
    let raw = DESCRIPTORS[index].swap(0, Ordering::AcqRel);
    if raw == 0 {
        return fail(EBADF);
    }
    let descriptor = PackedFileDescriptor::from_raw(raw);

    match file_close(descriptor) {
        Ok(()) => 0,
        Err(error) => {
            DESCRIPTORS[index].store(raw, Ordering::Release);
            fail(errno_from_status(error.status()))
        }
    }
}

fn open_options(flags: c_int) -> Option<OpenOptions> {
    let access = flags & 0x3;
    let mut options = match access {
        O_RDONLY => OpenOptions::read_only(),
        O_WRONLY => OpenOptions::write_only(),
        O_RDWR => OpenOptions::read_write(),
        _ => return None,
    };
    let known = 0x3 | O_CREAT | O_TRUNC;
    if flags & !known != 0 {
        return None;
    }
    if flags & O_CREAT != 0 {
        options = options.create();
    }
    if flags & O_TRUNC != 0 {
        options = options.truncate();
    }
    Some(options)
}

fn install_descriptor(descriptor: PackedFileDescriptor) -> c_int {
    let raw = descriptor.raw();
    for (index, slot) in DESCRIPTORS.iter().enumerate() {
        if slot
            .compare_exchange(0, raw, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return index as c_int + FD_BASE;
        }
    }
    let _ = file_close(descriptor);
    fail(EMFILE)
}

fn descriptor(fd: c_int) -> Option<PackedFileDescriptor> {
    let index = descriptor_index(fd)?;
    let raw = DESCRIPTORS[index].load(Ordering::Acquire);
    (raw != 0).then(|| PackedFileDescriptor::from_raw(raw))
}

fn descriptor_index(fd: c_int) -> Option<usize> {
    let index = fd.checked_sub(FD_BASE)? as usize;
    (index < FD_CAPACITY).then_some(index)
}

unsafe fn c_string_length(path: *const c_char, limit: usize) -> Option<usize> {
    if path.is_null() {
        return None;
    }
    for length in 0..=limit {
        if unsafe { *path.add(length) } == 0 {
            return Some(length);
        }
    }
    None
}

fn set_errno(value: c_int) {
    ERRNO.store(value, Ordering::Release);
}

fn fail(error: c_int) -> c_int {
    set_errno(error);
    -1
}

fn fail_ssize(error: c_int) -> isize {
    set_errno(error);
    -1
}

fn fail_off(error: c_int) -> i64 {
    set_errno(error);
    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_standard_open_flags_to_runtime_options() {
        assert!(open_options(O_RDONLY).is_some());
        assert!(open_options(O_WRONLY | O_CREAT).is_some());
        assert!(open_options(O_RDWR | O_CREAT | O_TRUNC).is_some());
        assert!(open_options(0x4000).is_none());
    }

    #[test]
    fn reserves_standard_stream_numbers() {
        assert_eq!(descriptor_index(0), None);
        assert_eq!(descriptor_index(2), None);
        assert_eq!(descriptor_index(3), Some(0));
        assert_eq!(descriptor_index(34), Some(31));
        assert_eq!(descriptor_index(35), None);
    }
}
