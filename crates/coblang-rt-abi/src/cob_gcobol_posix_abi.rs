//! Clean-room `libgcobol` (`__gg__*`) compatibility entrypoints and portable POSIX C-ABI shims
//! (`posix_errno`, `posix_exit`, `posix_open`, `posix_read`, `posix_write`, `posix_close`, `posix_stat`).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::IntoRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use crate::cob_field_decimal_abi::{
    cob_cmp, cob_decimal_add, cob_decimal_div, cob_decimal_get_field, cob_decimal_mul,
    cob_decimal_set_field, cob_decimal_sub, cob_move,
};
use crate::cob_lifecycle_abi::{cob_call, with_c_abi_state};
use crate::cob_types::{CobDecimal, CobField};

#[expect(
    unsafe_code,
    reason = "Audited POSIX `read`, `write`, and `close` declarations from system `libc`."
)]
unsafe extern "C" {
    /// Reads up to `nbyte` bytes from POSIX file descriptor `fd` into `buf`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Open POSIX file descriptor.
    /// * `buf` - Destination buffer pointer.
    /// * `nbyte` - Maximum byte count to read.
    fn read(fd: c_int, buf: *mut c_void, nbyte: usize) -> isize;
    /// Writes up to `nbyte` bytes from `buf` to POSIX file descriptor `fd`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Open POSIX file descriptor.
    /// * `buf` - Source buffer pointer.
    /// * `nbyte` - Byte count to write.
    fn write(fd: c_int, buf: *const c_void, nbyte: usize) -> isize;
    /// Closes POSIX file descriptor `fd`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Open POSIX file descriptor to release.
    fn close(fd: c_int) -> c_int;
}

/// Portable `O_ACCMODE` mask (`0x0003`) for [`posix_open`].
pub const COB_POSIX_O_ACCMODE: i32 = 0x0003;
/// Portable `O_RDONLY` flag (`0x0000`) for [`posix_open`].
pub const COB_POSIX_O_RDONLY: i32 = 0x0000;
/// Portable `O_WRONLY` flag (`0x0001`) for [`posix_open`].
pub const COB_POSIX_O_WRONLY: i32 = 0x0001;
/// Portable `O_RDWR` flag (`0x0002`) for [`posix_open`].
pub const COB_POSIX_O_RDWR: i32 = 0x0002;
/// Portable `O_CREAT` flag (`0x0200` on macOS, `0x0040` on Linux).
#[cfg(target_os = "macos")]
pub const COB_POSIX_O_CREAT: i32 = 0x0200;
/// Portable `O_CREAT` flag (`0x0040` on Linux/ELF).
#[cfg(not(target_os = "macos"))]
pub const COB_POSIX_O_CREAT: i32 = 0x0040;
/// Portable `O_TRUNC` flag (`0x0400` on macOS, `0x0200` on Linux).
#[cfg(target_os = "macos")]
pub const COB_POSIX_O_TRUNC: i32 = 0x0400;
/// Portable `O_TRUNC` flag (`0x0200` on Linux/ELF).
#[cfg(not(target_os = "macos"))]
pub const COB_POSIX_O_TRUNC: i32 = 0x0200;
/// Portable `O_APPEND` flag (`0x0008` on macOS, `0x0400` on Linux).
#[cfg(target_os = "macos")]
pub const COB_POSIX_O_APPEND: i32 = 0x0008;
/// Portable `O_APPEND` flag (`0x0400` on Linux/ELF).
#[cfg(not(target_os = "macos"))]
pub const COB_POSIX_O_APPEND: i32 = 0x0400;

/// Portable 13-field `#[repr(C)]` POSIX `stat` buffer matching `statbuf.cpy` (`104` bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CobPosixStatBuf {
    /// Device ID containing the file.
    pub st_dev: u64,
    /// Inode number.
    pub st_ino: u64,
    /// File type and permission mode bits.
    pub st_mode: u64,
    /// Hard-link count.
    pub st_nlink: u64,
    /// Owner user ID.
    pub st_uid: u64,
    /// Owner group ID.
    pub st_gid: u64,
    /// Device ID (if special file).
    pub st_rdev: u64,
    /// File size in bytes.
    pub st_size: i64,
    /// Preferred filesystem I/O block size.
    pub st_blksize: i64,
    /// Number of 512B blocks allocated.
    pub st_blocks: i64,
    /// Last access timestamp (seconds since Unix epoch).
    pub st_atime: i64,
    /// Last modification timestamp (seconds since Unix epoch).
    pub st_mtime: i64,
    /// Last status-change timestamp (seconds since Unix epoch).
    pub st_ctime: i64,
}

/// Writes `len` bytes at `ptr` (plus optional newline) to `writer` (`__gg__display` helper).
///
/// # Arguments
///
/// * `ptr` - Byte buffer pointer, or null.
/// * `len` - Byte length.
/// * `newline` - Non-zero to append `0x0A`.
/// * `writer` - Destination output stream.
///
/// # Safety
///
/// When `len > 0` and `ptr` is non-null, `ptr` must reference `len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited `__gg__display` writer helper shared with unit tests."
)]
pub unsafe fn gg_display_to_writer(
    ptr: *const u8,
    len: usize,
    newline: i32,
    writer: &mut dyn Write,
) {
    if len > 0 && !ptr.is_null() {
        // SAFETY: Validated `len > 0` and non-null `ptr`.
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
        let _ = writer.write_all(bytes);
    }
    if newline != 0 {
        let _ = writer.write_all(&[0x0A]);
    }
    let _ = writer.flush();
}

/// `libgcobol` compatibility `DISPLAY` shim (`__gg__display`).
///
/// # Arguments
///
/// * `ptr` - Byte buffer pointer, or null.
/// * `len` - Byte count.
/// * `newline` - Non-zero to append `0x0A`.
///
/// # Safety
///
/// When `len > 0` and `ptr` is non-null, `ptr` must reference `len` readable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `__gg__display` C-ABI symbol."
)]
pub unsafe extern "C" fn __gg__display(ptr: *const u8, len: usize, newline: i32) {
    let mut out = io::stdout();
    // SAFETY: Forwarded to null-guarded `gg_display_to_writer`.
    unsafe {
        gg_display_to_writer(ptr, len, newline, &mut out);
    }
}

/// `libgcobol` compatibility `MOVE` shim (`__gg__move`).
///
/// # Arguments
///
/// * `dst` - Destination [`CobField`] pointer.
/// * `src` - Source [`CobField`] pointer.
///
/// # Safety
///
/// Upholds [`cob_move`] pointer invariants.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `__gg__move` C-ABI symbol.")]
pub unsafe extern "C" fn __gg__move(dst: *mut CobField, src: *const CobField) {
    // SAFETY: Forwarded to null-guarded `cob_move`.
    unsafe {
        cob_move(src, dst);
    }
}

/// Shared binary arithmetic helper for `__gg__add`, `__gg__subtract`, `__gg__multiply`, and `__gg__divide`.
///
/// # Arguments
///
/// * `dst` - In/out accumulator [`CobField`].
/// * `src` - Right-hand operand [`CobField`].
/// * `store_opt` - `COB_STORE_*` rounding/overflow bitmask.
/// * `op` - Decimal binary operator (`cob_decimal_add`, `cob_decimal_sub`, etc.).
///
/// # Safety
///
/// Non-null `dst` and `src` must reference valid [`CobField`] descriptors.
#[expect(
    unsafe_code,
    reason = "Shared null-guarded decimal binary helper for `__gg__*` arithmetic shims."
)]
unsafe fn gg_apply_field_arith(
    dst: *mut CobField,
    src: *const CobField,
    store_opt: i32,
    op: unsafe extern "C" fn(*mut CobDecimal, *const CobDecimal),
) -> i32 {
    if dst.is_null() || src.is_null() {
        return 1;
    }
    let mut d_acc = CobDecimal::default();
    let mut d_rhs = CobDecimal::default();
    // SAFETY: Stack-allocated `CobDecimal` registers and non-null `dst`/`src`.
    unsafe {
        cob_decimal_set_field(&raw mut d_acc, dst.cast_const());
        cob_decimal_set_field(&raw mut d_rhs, src);
        op(&raw mut d_acc, &raw const d_rhs);
        cob_decimal_get_field(&raw const d_acc, dst, store_opt)
    }
}

/// `libgcobol` compatibility `ADD` shim (`dst += src`) (`__gg__add`).
///
/// # Arguments
///
/// * `dst` - In/out accumulator [`CobField`].
/// * `src` - Addend [`CobField`].
/// * `round_opt` - `COB_STORE_*` option bitmask.
///
/// # Safety
///
/// Non-null `dst` and `src` must reference valid [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `__gg__add` C-ABI symbol.")]
pub unsafe extern "C" fn __gg__add(
    dst: *mut CobField,
    src: *const CobField,
    round_opt: i32,
) -> i32 {
    // SAFETY: Forwarded to null-guarded `gg_apply_field_arith`.
    unsafe { gg_apply_field_arith(dst, src, round_opt, cob_decimal_add) }
}

/// `libgcobol` compatibility `SUBTRACT` shim (`dst -= src`) (`__gg__subtract`).
///
/// # Arguments
///
/// * `dst` - In/out accumulator [`CobField`].
/// * `src` - Subtrahend [`CobField`].
/// * `round_opt` - `COB_STORE_*` option bitmask.
///
/// # Safety
///
/// Non-null `dst` and `src` must reference valid [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `__gg__subtract` C-ABI symbol."
)]
pub unsafe extern "C" fn __gg__subtract(
    dst: *mut CobField,
    src: *const CobField,
    round_opt: i32,
) -> i32 {
    // SAFETY: Forwarded to null-guarded `gg_apply_field_arith`.
    unsafe { gg_apply_field_arith(dst, src, round_opt, cob_decimal_sub) }
}

/// `libgcobol` compatibility `MULTIPLY` shim (`dst *= src`) (`__gg__multiply`).
///
/// # Arguments
///
/// * `dst` - In/out accumulator [`CobField`].
/// * `src` - Multiplier [`CobField`].
/// * `round_opt` - `COB_STORE_*` option bitmask.
///
/// # Safety
///
/// Non-null `dst` and `src` must reference valid [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `__gg__multiply` C-ABI symbol."
)]
pub unsafe extern "C" fn __gg__multiply(
    dst: *mut CobField,
    src: *const CobField,
    round_opt: i32,
) -> i32 {
    // SAFETY: Forwarded to null-guarded `gg_apply_field_arith`.
    unsafe { gg_apply_field_arith(dst, src, round_opt, cob_decimal_mul) }
}

/// `libgcobol` compatibility `DIVIDE` shim (`dst /= src`) (`__gg__divide`).
///
/// # Arguments
///
/// * `dst` - In/out dividend/quotient [`CobField`].
/// * `src` - Divisor [`CobField`].
/// * `round_opt` - `COB_STORE_*` option bitmask.
///
/// # Safety
///
/// Non-null `dst` and `src` must reference valid [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `__gg__divide` C-ABI symbol."
)]
pub unsafe extern "C" fn __gg__divide(
    dst: *mut CobField,
    src: *const CobField,
    round_opt: i32,
) -> i32 {
    // SAFETY: Forwarded to null-guarded `gg_apply_field_arith`.
    unsafe { gg_apply_field_arith(dst, src, round_opt, cob_decimal_div) }
}

/// `libgcobol` compatibility relational compare shim (`__gg__compare`).
///
/// # Arguments
///
/// * `lhs` - Left-hand [`CobField`].
/// * `rhs` - Right-hand [`CobField`].
///
/// # Safety
///
/// Non-null `lhs` and `rhs` must reference readable [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `__gg__compare` C-ABI symbol."
)]
pub unsafe extern "C" fn __gg__compare(lhs: *const CobField, rhs: *const CobField) -> i32 {
    // SAFETY: Forwarded to null-guarded `cob_cmp`.
    unsafe { cob_cmp(lhs, rhs) }
}

/// `libgcobol` compatibility dynamic subprogram `CALL` shim (`__gg__call`).
///
/// # Arguments
///
/// * `name` - Nullable NUL-terminated subprogram name.
/// * `arg_count` - Argument count.
/// * `arg_values` - Pointer to `arg_count` `*mut c_void` arguments, or null.
///
/// # Safety
///
/// Upholds [`cob_call`] pointer invariants.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `__gg__call` C-ABI symbol.")]
pub unsafe extern "C" fn __gg__call(
    name: *const c_char,
    arg_count: i32,
    arg_values: *const *mut c_void,
) -> i32 {
    // SAFETY: Forwarded to null-guarded `cob_call`.
    unsafe { cob_call(name, arg_count, arg_values) }
}

/// Queries the current thread's OS `errno` and writes a space-padded human-readable message
/// into `msg_buf[..msg_len]` when `msg_buf` is non-null (`posix_errno`).
///
/// # Arguments
///
/// * `msg_buf` - Optional message buffer pointer, or null.
/// * `msg_len` - Capacity of `msg_buf` in bytes.
///
/// # Safety
///
/// When `msg_len > 0` and `msg_buf` is non-null, `msg_buf` must reference `msg_len` writable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `posix_errno` C-ABI symbol."
)]
pub unsafe extern "C" fn posix_errno(msg_buf: *mut u8, msg_len: usize) -> i32 {
    let code = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    if msg_len > 0 && !msg_buf.is_null() {
        let rendered = io::Error::from_raw_os_error(code).to_string();
        let bytes = rendered.as_bytes();
        let copy_n = bytes.len().min(msg_len);
        // SAFETY: Validated `msg_len > 0`, non-null `msg_buf`, and `copy_n <= bytes.len().min(msg_len)`.
        unsafe {
            core::slice::from_raw_parts_mut(msg_buf, msg_len).fill(b' ');
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), msg_buf, copy_n);
        }
    }
    code
}

/// Terminates the process with `status` when `stop_run_exit_enabled` is active (`posix_exit`).
///
/// # Arguments
///
/// * `status` - Exit status code.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `posix_exit` C-ABI symbol.")]
pub extern "C" fn posix_exit(status: i32) {
    let should_exit = with_c_abi_state(|st| st.stop_run_exit_enabled);
    if should_exit {
        std::process::exit(status);
    }
}

/// Opens `path` using portable POSIX `flags` and `mode`, returning a raw file descriptor or `-1` (`posix_open`).
///
/// # Arguments
///
/// * `path` - NUL-terminated filesystem path pointer.
/// * `flags` - Bitmask of `COB_POSIX_O_*` flags.
/// * `mode` - Creation permission mode (e.g. `0o644`).
///
/// # Safety
///
/// When non-null, `path` must reference a valid NUL-terminated C string.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `posix_open` C-ABI symbol.")]
pub unsafe extern "C" fn posix_open(path: *const c_char, flags: i32, mode: u32) -> i32 {
    if path.is_null() {
        return -1;
    }
    // SAFETY: Validated non-null `path`.
    let path_str = unsafe { CStr::from_ptr(path) }.to_string_lossy();
    let acc = flags & COB_POSIX_O_ACCMODE;
    let mut opts = OpenOptions::new();
    match acc {
        COB_POSIX_O_WRONLY => {
            opts.write(true);
        }
        COB_POSIX_O_RDWR => {
            opts.read(true).write(true);
        }
        _ => {
            opts.read(true);
        }
    }
    if (flags & COB_POSIX_O_CREAT) != 0 {
        opts.create(true);
    }
    if (flags & COB_POSIX_O_TRUNC) != 0 {
        opts.truncate(true);
    }
    if (flags & COB_POSIX_O_APPEND) != 0 {
        opts.append(true);
    }
    if mode != 0 {
        opts.mode(mode);
    }
    match opts.open(path_str.as_ref()) {
        Ok(file) => file.into_raw_fd(),
        Err(_) => -1,
    }
}

/// Reads up to `count` bytes from POSIX file descriptor `fd` into `buf` (`posix_read`).
///
/// # Arguments
///
/// * `fd` - Open POSIX file descriptor.
/// * `buf` - Destination byte pointer.
/// * `count` - Maximum byte count to read.
///
/// # Safety
///
/// When `count > 0`, non-null `buf` must point to `count` writable bytes.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `posix_read` C-ABI symbol.")]
pub unsafe extern "C" fn posix_read(fd: i32, buf: *mut u8, count: usize) -> isize {
    if count == 0 {
        return 0;
    }
    if fd < 0 || buf.is_null() {
        return -1;
    }
    // SAFETY: Validated `fd >= 0`, non-null `buf`, and `count > 0`.
    unsafe { read(fd, buf.cast::<c_void>(), count) }
}

/// Writes up to `count` bytes from `buf` to POSIX file descriptor `fd` (`posix_write`).
///
/// # Arguments
///
/// * `fd` - Open POSIX file descriptor.
/// * `buf` - Source byte pointer.
/// * `count` - Byte count to write.
///
/// # Safety
///
/// When `count > 0`, non-null `buf` must point to `count` readable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `posix_write` C-ABI symbol."
)]
pub unsafe extern "C" fn posix_write(fd: i32, buf: *const u8, count: usize) -> isize {
    if count == 0 {
        return 0;
    }
    if fd < 0 || buf.is_null() {
        return -1;
    }
    // SAFETY: Validated `fd >= 0`, non-null `buf`, and `count > 0`.
    unsafe { write(fd, buf.cast::<c_void>(), count) }
}

/// Closes POSIX file descriptor `fd` (`posix_close`).
///
/// # Arguments
///
/// * `fd` - Open POSIX file descriptor.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `posix_close` C-ABI symbol."
)]
pub extern "C" fn posix_close(fd: i32) -> i32 {
    if fd < 0 {
        return -1;
    }
    // SAFETY: Validated `fd >= 0`; OS `close` handles invalid descriptors by returning `-1`.
    unsafe { close(fd) }
}

/// Queries filesystem metadata of `path` into portable [`CobPosixStatBuf`] (`posix_stat`).
///
/// # Arguments
///
/// * `path` - NUL-terminated filesystem path pointer.
/// * `stat_out` - Destination [`CobPosixStatBuf`] pointer.
/// * `buf_size` - Byte capacity at `stat_out` (must be `>= size_of::<CobPosixStatBuf>()`).
///
/// # Safety
///
/// Non-null `path` must be NUL-terminated; non-null `stat_out` with `buf_size >= size_of::<CobPosixStatBuf>()`
/// must reference writable aligned [`CobPosixStatBuf`] storage.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `posix_stat` C-ABI symbol.")]
pub unsafe extern "C" fn posix_stat(
    path: *const c_char,
    stat_out: *mut CobPosixStatBuf,
    buf_size: usize,
) -> i32 {
    if path.is_null() || stat_out.is_null() || buf_size < size_of::<CobPosixStatBuf>() {
        return -1;
    }
    // SAFETY: Validated non-null `path`.
    let path_str = unsafe { CStr::from_ptr(path) }.to_string_lossy();
    let Ok(meta) = fs::metadata(path_str.as_ref()) else {
        return -1;
    };
    let populated = CobPosixStatBuf {
        st_dev: meta.dev(),
        st_ino: meta.ino(),
        st_mode: u64::from(meta.mode()),
        st_nlink: meta.nlink(),
        st_uid: u64::from(meta.uid()),
        st_gid: u64::from(meta.gid()),
        st_rdev: meta.rdev(),
        st_size: i64::try_from(meta.size()).unwrap_or(i64::MAX),
        st_blksize: i64::try_from(meta.blksize()).unwrap_or(4096),
        st_blocks: i64::try_from(meta.blocks()).unwrap_or(i64::MAX),
        st_atime: meta.atime(),
        st_mtime: meta.mtime(),
        st_ctime: meta.ctime(),
    };
    // SAFETY: Validated non-null `stat_out` and `buf_size >= size_of::<CobPosixStatBuf>()`.
    unsafe {
        *stat_out = populated;
    }
    0
}
