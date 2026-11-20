//! Clean-room `SYSTEM`, `CBL_*`, and `C$*` (`C_*`) built-in runtime subroutines (`Milestone M11`).

use std::ffi::{CStr, c_char};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use coblang_rt_core::accept_env::CobolDateTime;
use coblang_rt_core::with_global_runtime;

use crate::cob_lifecycle_abi::{OpenByteStreamFile, with_c_abi_state};

/// Extracts a trimmed filesystem path or shell command string from `raw_ptr`, honoring `hint_len`
/// when `> 0` (COBOL fixed-width `PIC X(n)` field) or NUL-termination when `hint_len == 0` (C caller).
///
/// # Arguments
///
/// * `raw_ptr` - Candidate byte pointer, or null.
/// * `hint_len` - Known COBOL parameter byte length (`0` when unknown / C caller).
///
/// # Safety
///
/// When non-null, `raw_ptr` must reference either `hint_len` readable bytes (if `hint_len > 0`)
/// or a NUL-terminated C string (if `hint_len == 0`).
#[must_use]
#[expect(
    unsafe_code,
    reason = "Audited string extraction for `SYSTEM`, `CBL_*`, and `C$*` path/command arguments."
)]
unsafe fn extract_trimmed_text(raw_ptr: *const u8, hint_len: usize) -> Option<String> {
    if raw_ptr.is_null() {
        return None;
    }
    let raw_slice = if hint_len > 0 {
        // SAFETY: Caller guarantees `hint_len` readable bytes at `raw_ptr`.
        let full = unsafe { core::slice::from_raw_parts(raw_ptr, hint_len) };
        let nul_pos = full.iter().position(|&b| b == 0).unwrap_or(full.len());
        full.get(..nul_pos).unwrap_or(full)
    } else {
        // SAFETY: Caller guarantees NUL-terminated C string when `hint_len == 0`.
        unsafe { CStr::from_ptr(raw_ptr.cast::<c_char>()) }.to_bytes()
    };
    let text = String::from_utf8_lossy(raw_slice);
    let trimmed = text.trim_end_matches(' ');
    if trimmed.is_empty() {
        None
    } else {
        Some(String::from(trimmed))
    }
}

/// Queries the active call frame's declared byte length for parameter `param_idx` (`0` if absent).
///
/// # Arguments
///
/// * `param_idx` - Zero-based parameter index.
#[must_use]
fn active_param_len(param_idx: usize) -> usize {
    with_c_abi_state(|st| {
        st.module_stack
            .last()
            .and_then(|f| f.param_sizes.get(param_idx).copied())
            .unwrap_or(0)
    })
}

/// Executes a host shell command via `sh -c` and returns its exit code (`SYSTEM`).
///
/// # Arguments
///
/// * `cmd` - Command text pointer (NUL-terminated or space-padded COBOL field).
///
/// # Safety
///
/// When non-null, `cmd` must point to a NUL-terminated string or active COBOL parameter buffer.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `SYSTEM` C-ABI subroutine with null guard."
)]
pub unsafe extern "C" fn SYSTEM(cmd: *const u8) -> i32 {
    let hint = active_param_len(0);
    // SAFETY: Forwarded with audited null check and `hint` length.
    let Some(command_str) = (unsafe { extract_trimmed_text(cmd, hint) }) else {
        return -1;
    };
    let mut child = Command::new("sh");
    child.arg("-c").arg(&command_str);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let _ = child.process_group(0);
    }
    child
        .status()
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1)
}

/// Applies binary bitwise operator `op(src_byte, dst_byte)` across `length` bytes (`CBL_AND`, `CBL_OR`, etc.).
///
/// # Arguments
///
/// * `src` - Read-only source byte buffer.
/// * `dst` - In-place destination byte buffer.
/// * `length` - Byte count to transform.
/// * `op` - Pure byte-wise combiner `(src_byte, dst_byte) -> new_dst_byte`.
///
/// # Safety
///
/// When `length > 0`, non-null `src` and `dst` must point to `length` readable/writable bytes (`src` and `dst` may alias).
#[expect(
    unsafe_code,
    reason = "Shared null-guarded bitwise buffer combinator for `CBL_*` routines."
)]
unsafe fn apply_bitwise_binary(
    src: *const u8,
    dst: *mut u8,
    length: usize,
    op: fn(u8, u8) -> u8,
) -> i32 {
    if length == 0 {
        return 0;
    }
    if src.is_null() || dst.is_null() {
        return -1;
    }
    // SAFETY: Copy `src` first so aliasing `src == dst` is 100% sound.
    let src_copy = unsafe { core::slice::from_raw_parts(src, length) }.to_vec();
    // SAFETY: Validated non-null `dst` and `length > 0` after dropping shared borrow.
    let dst_slice = unsafe { core::slice::from_raw_parts_mut(dst, length) };
    for (d, &s) in dst_slice.iter_mut().zip(src_copy.iter()) {
        *d = op(s, *d);
    }
    0
}

/// Bitwise `dst[i] &= src[i]` across `length` bytes (`CBL_AND`).
///
/// # Arguments
///
/// * `src` - Source byte pointer.
/// * `dst` - Destination byte pointer.
/// * `length` - Byte length.
///
/// # Safety
///
/// Upholds `apply_bitwise_binary` pointer invariants.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `CBL_AND` C-ABI symbol.")]
pub unsafe extern "C" fn CBL_AND(src: *const u8, dst: *mut u8, length: usize) -> i32 {
    // SAFETY: Forwarded to null-guarded `apply_bitwise_binary`.
    unsafe { apply_bitwise_binary(src, dst, length, |s, d| s & d) }
}

/// Bitwise `dst[i] |= src[i]` across `length` bytes (`CBL_OR`).
///
/// # Arguments
///
/// * `src` - Source byte pointer.
/// * `dst` - Destination byte pointer.
/// * `length` - Byte length.
///
/// # Safety
///
/// Upholds `apply_bitwise_binary` pointer invariants.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `CBL_OR` C-ABI symbol.")]
pub unsafe extern "C" fn CBL_OR(src: *const u8, dst: *mut u8, length: usize) -> i32 {
    // SAFETY: Forwarded to null-guarded `apply_bitwise_binary`.
    unsafe { apply_bitwise_binary(src, dst, length, |s, d| s | d) }
}

/// Bitwise NOR `dst[i] = !(src[i] | dst[i])` across `length` bytes (`CBL_NOR`).
///
/// # Arguments
///
/// * `src` - Source byte pointer.
/// * `dst` - Destination byte pointer.
/// * `length` - Byte length.
///
/// # Safety
///
/// Upholds `apply_bitwise_binary` pointer invariants.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `CBL_NOR` C-ABI symbol.")]
pub unsafe extern "C" fn CBL_NOR(src: *const u8, dst: *mut u8, length: usize) -> i32 {
    // SAFETY: Forwarded to null-guarded `apply_bitwise_binary`.
    unsafe { apply_bitwise_binary(src, dst, length, |s, d| !(s | d)) }
}

/// Bitwise `dst[i] ^= src[i]` across `length` bytes (`CBL_XOR`).
///
/// # Arguments
///
/// * `src` - Source byte pointer.
/// * `dst` - Destination byte pointer.
/// * `length` - Byte length.
///
/// # Safety
///
/// Upholds `apply_bitwise_binary` pointer invariants.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `CBL_XOR` C-ABI symbol.")]
pub unsafe extern "C" fn CBL_XOR(src: *const u8, dst: *mut u8, length: usize) -> i32 {
    // SAFETY: Forwarded to null-guarded `apply_bitwise_binary`.
    unsafe { apply_bitwise_binary(src, dst, length, |s, d| s ^ d) }
}

/// Bitwise equivalence `dst[i] = !(src[i] ^ dst[i])` across `length` bytes (`CBL_EQ`).
///
/// # Arguments
///
/// * `src` - Source byte pointer.
/// * `dst` - Destination byte pointer.
/// * `length` - Byte length.
///
/// # Safety
///
/// Upholds `apply_bitwise_binary` pointer invariants.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `CBL_EQ` C-ABI symbol.")]
pub unsafe extern "C" fn CBL_EQ(src: *const u8, dst: *mut u8, length: usize) -> i32 {
    // SAFETY: Forwarded to null-guarded `apply_bitwise_binary`.
    unsafe { apply_bitwise_binary(src, dst, length, |s, d| !(s ^ d)) }
}

/// Bitwise implication `dst[i] = (!src[i]) | dst[i]` across `length` bytes (`CBL_IMP`).
///
/// # Arguments
///
/// * `src` - Source byte pointer.
/// * `dst` - Destination byte pointer.
/// * `length` - Byte length.
///
/// # Safety
///
/// Upholds `apply_bitwise_binary` pointer invariants.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `CBL_IMP` C-ABI symbol.")]
pub unsafe extern "C" fn CBL_IMP(src: *const u8, dst: *mut u8, length: usize) -> i32 {
    // SAFETY: Forwarded to null-guarded `apply_bitwise_binary`.
    unsafe { apply_bitwise_binary(src, dst, length, |s, d| (!s) | d) }
}

/// Bitwise non-implication `dst[i] = src[i] & (!dst[i])` across `length` bytes (`CBL_NIMP`).
///
/// # Arguments
///
/// * `src` - Source byte pointer.
/// * `dst` - Destination byte pointer.
/// * `length` - Byte length.
///
/// # Safety
///
/// Upholds `apply_bitwise_binary` pointer invariants.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `CBL_NIMP` C-ABI symbol.")]
pub unsafe extern "C" fn CBL_NIMP(src: *const u8, dst: *mut u8, length: usize) -> i32 {
    // SAFETY: Forwarded to null-guarded `apply_bitwise_binary`.
    unsafe { apply_bitwise_binary(src, dst, length, |s, d| s & (!d)) }
}

/// Bitwise complement `dst[i] = !dst[i]` across `length` bytes (`CBL_NOT`).
///
/// # Arguments
///
/// * `dst` - Target byte pointer.
/// * `length` - Byte length.
///
/// # Safety
///
/// When `length > 0`, non-null `dst` must point to `length` writable bytes.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `CBL_NOT` C-ABI symbol.")]
pub unsafe extern "C" fn CBL_NOT(dst: *mut u8, length: usize) -> i32 {
    if length == 0 {
        return 0;
    }
    if dst.is_null() {
        return -1;
    }
    // SAFETY: Validated non-null `dst` and `length > 0`.
    let slice = unsafe { core::slice::from_raw_parts_mut(dst, length) };
    for b in slice {
        *b = !*b;
    }
    0
}

/// Converts `length` ASCII bytes at `buf` to uppercase in place (`CBL_TOUPPER`).
///
/// # Arguments
///
/// * `buf` - Mutable character buffer pointer.
/// * `length` - Byte count to uppercase.
///
/// # Safety
///
/// When `length > 0`, non-null `buf` must point to `length` writable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_TOUPPER` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_TOUPPER(buf: *mut u8, length: usize) -> i32 {
    if length == 0 {
        return 0;
    }
    if buf.is_null() {
        return -1;
    }
    // SAFETY: Validated non-null `buf` and `length > 0`.
    unsafe { core::slice::from_raw_parts_mut(buf, length) }.make_ascii_uppercase();
    0
}

/// Converts `length` ASCII bytes at `buf` to lowercase in place (`CBL_TOLOWER`).
///
/// # Arguments
///
/// * `buf` - Mutable character buffer pointer.
/// * `length` - Byte count to lowercase.
///
/// # Safety
///
/// When `length > 0`, non-null `buf` must point to `length` writable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_TOLOWER` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_TOLOWER(buf: *mut u8, length: usize) -> i32 {
    if length == 0 {
        return 0;
    }
    if buf.is_null() {
        return -1;
    }
    // SAFETY: Validated non-null `buf` and `length > 0`.
    unsafe { core::slice::from_raw_parts_mut(buf, length) }.make_ascii_lowercase();
    0
}

/// `C$TOUPPER` alias (`C_TOUPPER`) forwarding to [`CBL_TOUPPER`].
///
/// # Arguments
///
/// * `buf` - Mutable character buffer pointer.
/// * `length` - Byte count to uppercase.
///
/// # Safety
///
/// Upholds [`CBL_TOUPPER`] pointer invariants.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_TOUPPER` (`C$TOUPPER`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_TOUPPER(buf: *mut u8, length: usize) -> i32 {
    // SAFETY: Same preconditions as `CBL_TOUPPER`.
    unsafe { CBL_TOUPPER(buf, length) }
}

/// `C$TOLOWER` alias (`C_TOLOWER`) forwarding to [`CBL_TOLOWER`].
///
/// # Arguments
///
/// * `buf` - Mutable character buffer pointer.
/// * `length` - Byte count to lowercase.
///
/// # Safety
///
/// Upholds [`CBL_TOLOWER`] pointer invariants.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_TOLOWER` (`C$TOLOWER`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_TOLOWER(buf: *mut u8, length: usize) -> i32 {
    // SAFETY: Same preconditions as `CBL_TOLOWER`.
    unsafe { CBL_TOLOWER(buf, length) }
}

/// Justifies ASCII text in `buf` (`'L'` left, `'C'` center, default/`'R'` right) (`C$JUSTIFY` / `C_JUSTIFY`).
///
/// # Arguments
///
/// * `buf` - Mutable character buffer (uses active frame's parameter 0 size if `> 0`, else NUL-terminated length).
/// * `mode_ptr` - Optional 1-byte mode pointer (`b'L'`, `b'R'`, `b'C'`), or null for `'R'`.
///
/// # Safety
///
/// Non-null `buf` must reference writable character bytes; non-null `mode_ptr` must point to 1 readable byte.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_JUSTIFY` (`C$JUSTIFY`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_JUSTIFY(buf: *mut u8, mode_ptr: *const u8) -> i32 {
    if buf.is_null() {
        return -1;
    }
    let frame_len = active_param_len(0);
    let len = if frame_len > 0 {
        frame_len
    } else {
        // SAFETY: NUL-terminated fallback when invoked directly from C.
        unsafe { CStr::from_ptr(buf.cast::<c_char>()) }
            .to_bytes()
            .len()
    };
    if len == 0 {
        return 0;
    }
    let mode = if mode_ptr.is_null() {
        b'R'
    } else {
        // SAFETY: Validated non-null `mode_ptr`.
        (unsafe { *mode_ptr }).to_ascii_uppercase()
    };
    // SAFETY: Validated non-null `buf` and `len > 0`.
    let slice = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    let first_non_space = slice.iter().position(|&b| b != b' ');
    let Some(start) = first_non_space else {
        return 0;
    };
    let end = slice
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(len, |idx| idx.saturating_add(1));
    let core_bytes = slice.get(start..end).unwrap_or(&[]).to_vec();
    let pad_total = len.saturating_sub(core_bytes.len());
    let left_pad = match mode {
        b'L' => 0,
        b'C' => pad_total / 2,
        _ => pad_total,
    };
    slice.fill(b' ');
    // SAFETY: `left_pad + core_bytes.len() <= len` holds by construction (`pad_total = len - core_bytes.len()`).
    unsafe {
        core::ptr::copy_nonoverlapping(core_bytes.as_ptr(), buf.add(left_pad), core_bytes.len());
    }
    0
}

/// Reads a 4-byte native `u32` handle from `handle_ptr`.
///
/// # Arguments
///
/// * `handle_ptr` - Pointer to 4-byte `u32` handle slot.
///
/// # Safety
///
/// Non-null `handle_ptr` must reference 4 readable bytes.
#[expect(unsafe_code, reason = "Audited 4-byte file handle read helper.")]
unsafe fn read_u32_slot(handle_ptr: *const u8) -> Option<u32> {
    if handle_ptr.is_null() {
        return None;
    }
    // SAFETY: Validated non-null `handle_ptr` with 4-byte width (`read_unaligned` handles arbitrary byte alignment).
    Some(unsafe { core::ptr::read_unaligned(handle_ptr.cast::<u32>()) })
}

/// Writes a 4-byte native `u32` value into `slot_ptr`.
///
/// # Arguments
///
/// * `slot_ptr` - Pointer to 4-byte writable slot.
/// * `val` - `u32` value to store.
///
/// # Safety
///
/// Non-null `slot_ptr` must reference 4 writable bytes.
#[expect(unsafe_code, reason = "Audited 4-byte file handle write helper.")]
unsafe fn write_u32_slot(slot_ptr: *mut u8, val: u32) {
    // SAFETY: Caller validates non-null `slot_ptr`.
    let out = unsafe { core::slice::from_raw_parts_mut(slot_ptr, 4) };
    out.copy_from_slice(&val.to_ne_bytes());
}

/// Reads a 64-bit file offset or 32-bit count from `ptr` depending on `width` (`4` or `8`, defaulting to `8` for offsets and `4` for counts).
///
/// # Arguments
///
/// * `ptr` - Pointer to integer storage.
/// * `width` - Byte width (`1`, `2`, `4`, or `8`).
///
/// # Safety
///
/// Non-null `ptr` must reference `width` readable bytes.
#[expect(unsafe_code, reason = "Audited `COMP-5` offset/count integer reader.")]
unsafe fn read_comp5_u64(ptr: *const u8, width: usize) -> Option<u64> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: Caller guarantees `width` readable bytes at `ptr`.
    let s = unsafe { core::slice::from_raw_parts(ptr, width) };
    match *s {
        [b0] => Some(u64::from(b0)),
        [b0, b1] => Some(u64::from(u16::from_ne_bytes([b0, b1]))),
        [b0, b1, b2, b3] => Some(u64::from(u32::from_ne_bytes([b0, b1, b2, b3]))),
        [b0, b1, b2, b3, b4, b5, b6, b7] => {
            Some(u64::from_ne_bytes([b0, b1, b2, b3, b4, b5, b6, b7]))
        }
        _ => None,
    }
}

/// Opens an existing byte-stream file (`CBL_OPEN_FILE`), writing a 4-byte handle into `file_handle`.
///
/// `access_mode`: `1` = input, `2` = output, `3` = read/write.
///
/// # Arguments
///
/// * `file_name` - Path byte pointer.
/// * `access_mode` - Pointer to 1-byte access mode (`1`, `2`, `3`).
/// * `deny_mode` - Unused lock-sharing mode pointer (nullable).
/// * `device` - Unused device byte pointer (nullable).
/// * `file_handle` - Out-pointer receiving 4-byte handle ID.
///
/// # Safety
///
/// Non-null pointers must reference readable/writable storage of their respective field widths.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_OPEN_FILE` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_OPEN_FILE(
    file_name: *const u8,
    access_mode: *const u8,
    deny_mode: *const u8,
    device: *const u8,
    file_handle: *mut u8,
) -> i32 {
    let _ = (deny_mode, device);
    if access_mode.is_null() || file_handle.is_null() {
        return -1;
    }
    let hint = active_param_len(0);
    // SAFETY: Validated non-null pointers above.
    let Some(path_str) = (unsafe { extract_trimmed_text(file_name, hint) }) else {
        return 35;
    };
    // SAFETY: Validated non-null `access_mode`.
    let mode = unsafe { *access_mode };
    let mut opts = OpenOptions::new();
    match mode {
        1 => {
            opts.read(true);
        }
        2 => {
            opts.write(true);
        }
        _ => {
            opts.read(true).write(true);
        }
    }
    let Ok(file) = opts.open(&path_str) else {
        return 35;
    };
    let handle_id = with_c_abi_state(|st| {
        let id = st.next_file_handle;
        st.next_file_handle = st.next_file_handle.wrapping_add(1).max(1);
        st.byte_files.insert(id, OpenByteStreamFile { file });
        id
    });
    // SAFETY: Validated non-null `file_handle`.
    unsafe {
        write_u32_slot(file_handle, handle_id);
    }
    0
}

/// Creates/truncates a byte-stream file for read/write (`CBL_CREATE_FILE`).
///
/// # Arguments
///
/// * `file_name` - Path byte pointer.
/// * `access_mode` - Optional 1-byte access mode pointer.
/// * `deny_mode` - Unused lock-sharing mode pointer.
/// * `device` - Unused device byte pointer.
/// * `file_handle` - Out-pointer receiving 4-byte handle ID.
///
/// # Safety
///
/// Non-null pointers must reference readable/writable storage of their respective field widths.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_CREATE_FILE` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_CREATE_FILE(
    file_name: *const u8,
    access_mode: *const u8,
    deny_mode: *const u8,
    device: *const u8,
    file_handle: *mut u8,
) -> i32 {
    let _ = (access_mode, deny_mode, device);
    if file_handle.is_null() {
        return -1;
    }
    let hint = active_param_len(0);
    // SAFETY: Validated via `extract_trimmed_text`.
    let Some(path_str) = (unsafe { extract_trimmed_text(file_name, hint) }) else {
        return 30;
    };
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path_str)
    else {
        return 30;
    };
    let handle_id = with_c_abi_state(|st| {
        let id = st.next_file_handle;
        st.next_file_handle = st.next_file_handle.wrapping_add(1).max(1);
        st.byte_files.insert(id, OpenByteStreamFile { file });
        id
    });
    // SAFETY: Validated non-null `file_handle`.
    unsafe {
        write_u32_slot(file_handle, handle_id);
    }
    0
}

/// Reads bytes at `file_offset` from `file_handle` into `buffer` (`CBL_READ_FILE`).
///
/// When `*flags == 128`, stores the 64-bit file size into `file_offset` (`8` bytes) without reading `buffer`.
///
/// # Arguments
///
/// * `file_handle` - Pointer to 4-byte handle ID.
/// * `file_offset` - Pointer to 8-byte (or active param width) byte offset.
/// * `byte_count` - Pointer to 4-byte requested byte count.
/// * `flags` - Optional 1-byte flags pointer (`128` queries file size).
/// * `buffer` - Destination byte buffer.
///
/// # Safety
///
/// Non-null pointers must reference valid readable/writable buffers of the specified byte counts.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_READ_FILE` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_READ_FILE(
    file_handle: *const u8,
    file_offset: *const u8,
    byte_count: *mut u8,
    flags: *const u8,
    buffer: *mut u8,
) -> i32 {
    // SAFETY: Validated via `read_u32_slot`.
    let Some(hid) = (unsafe { read_u32_slot(file_handle) }) else {
        return -1;
    };
    let flag_val = if flags.is_null() {
        0_u8
    } else {
        // SAFETY: Validated non-null `flags`.
        unsafe { *flags }
    };
    if flag_val == 128 {
        if file_offset.is_null() {
            return -1;
        }
        let size_opt = with_c_abi_state(|st| {
            st.byte_files
                .get(&hid)
                .and_then(|entry| entry.file.metadata().ok().map(|m| m.len()))
        });
        let Some(fsize) = size_opt else {
            return -1;
        };
        // SAFETY: In `flags == 128` mode, `file_offset` points to an 8-byte writable size buffer.
        let out = unsafe { core::slice::from_raw_parts_mut(file_offset.cast_mut(), 8) };
        out.copy_from_slice(&fsize.to_ne_bytes());
        return 0;
    }
    let off_w = active_param_len(1);
    let cnt_w = active_param_len(2);
    // SAFETY: Validated via `read_comp5_u64`.
    let Some(offset) = (unsafe { read_comp5_u64(file_offset, if off_w > 0 { off_w } else { 8 }) })
    else {
        return -1;
    };
    // SAFETY: Validated via `read_comp5_u64`.
    let Some(req_u64) =
        (unsafe { read_comp5_u64(byte_count.cast_const(), if cnt_w > 0 { cnt_w } else { 4 }) })
    else {
        return -1;
    };
    let req_len = usize::try_from(req_u64).unwrap_or(0);
    if req_len == 0 {
        return 0;
    }
    if buffer.is_null() {
        return -1;
    }
    // SAFETY: Validated non-null `buffer` and `req_len > 0`.
    let dst = unsafe { core::slice::from_raw_parts_mut(buffer, req_len) };
    with_c_abi_state(|st| {
        let Some(entry) = st.byte_files.get_mut(&hid) else {
            return -1;
        };
        let _ = entry.file.seek(SeekFrom::Start(offset));
        match entry.file.read(dst) {
            Ok(0) => 10,
            Ok(_) => 0,
            Err(_) => 30,
        }
    })
}

/// Writes `byte_count` bytes from `buffer` at `file_offset` into `file_handle` (`CBL_WRITE_FILE`).
///
/// # Arguments
///
/// * `file_handle` - Pointer to 4-byte handle ID.
/// * `file_offset` - Pointer to 8-byte (or active param width) byte offset.
/// * `byte_count` - Pointer to 4-byte byte count (`0` truncates file at `file_offset`).
/// * `flags` - Optional 1-byte flags (`128` appends at EOF).
/// * `buffer` - Source byte buffer.
///
/// # Safety
///
/// Non-null pointers must reference valid readable buffers of the specified byte counts.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_WRITE_FILE` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_WRITE_FILE(
    file_handle: *const u8,
    file_offset: *const u8,
    byte_count: *const u8,
    flags: *const u8,
    buffer: *const u8,
) -> i32 {
    // SAFETY: Validated via `read_u32_slot`.
    let Some(hid) = (unsafe { read_u32_slot(file_handle) }) else {
        return -1;
    };
    let off_w = active_param_len(1);
    let cnt_w = active_param_len(2);
    // SAFETY: Validated via `read_comp5_u64`.
    let Some(offset) = (unsafe { read_comp5_u64(file_offset, if off_w > 0 { off_w } else { 8 }) })
    else {
        return -1;
    };
    // SAFETY: Validated via `read_comp5_u64`.
    let Some(cnt_u64) = (unsafe { read_comp5_u64(byte_count, if cnt_w > 0 { cnt_w } else { 4 }) })
    else {
        return -1;
    };
    let flag_val = if flags.is_null() {
        0_u8
    } else {
        // SAFETY: Validated non-null `flags`.
        unsafe { *flags }
    };
    let write_len = usize::try_from(cnt_u64).unwrap_or(0);
    if write_len > 0 && buffer.is_null() {
        return -1;
    }
    let src: &[u8] = if write_len == 0 {
        &[]
    } else {
        // SAFETY: Validated `write_len > 0` and non-null `buffer`.
        unsafe { core::slice::from_raw_parts(buffer, write_len) }
    };
    with_c_abi_state(|st| {
        let Some(entry) = st.byte_files.get_mut(&hid) else {
            return -1;
        };
        let target_pos = if flag_val == 128 {
            SeekFrom::End(0)
        } else {
            SeekFrom::Start(offset)
        };
        let _ = entry.file.seek(target_pos);
        if write_len == 0 {
            return if entry.file.set_len(offset).is_ok() {
                0
            } else {
                30
            };
        }
        if entry.file.write_all(src).is_ok() {
            0
        } else {
            30
        }
    })
}

/// Closes an open byte-stream handle (`CBL_CLOSE_FILE`).
///
/// # Arguments
///
/// * `file_handle` - Pointer to 4-byte handle ID.
///
/// # Safety
///
/// Non-null `file_handle` must point to 4 readable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_CLOSE_FILE` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_CLOSE_FILE(file_handle: *const u8) -> i32 {
    // SAFETY: Validated via `read_u32_slot`.
    let Some(hid) = (unsafe { read_u32_slot(file_handle) }) else {
        return -1;
    };
    with_c_abi_state(|st| {
        if st.byte_files.remove(&hid).is_some() {
            0
        } else {
            -1
        }
    })
}

/// Flushes an open byte-stream handle to disk (`CBL_FLUSH_FILE`).
///
/// # Arguments
///
/// * `file_handle` - Pointer to 4-byte handle ID.
///
/// # Safety
///
/// Non-null `file_handle` must point to 4 readable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_FLUSH_FILE` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_FLUSH_FILE(file_handle: *const u8) -> i32 {
    // SAFETY: Validated via `read_u32_slot`.
    let Some(hid) = (unsafe { read_u32_slot(file_handle) }) else {
        return -1;
    };
    with_c_abi_state(|st| {
        let Some(entry) = st.byte_files.get_mut(&hid) else {
            return -1;
        };
        let _ = entry.file.flush();
        0
    })
}

/// Deletes `file_name` from the filesystem (`CBL_DELETE_FILE`).
///
/// # Arguments
///
/// * `file_name` - Path byte pointer.
///
/// # Safety
///
/// Non-null `file_name` must point to a NUL-terminated or active COBOL path buffer.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_DELETE_FILE` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_DELETE_FILE(file_name: *const u8) -> i32 {
    let hint = active_param_len(0);
    // SAFETY: Validated via `extract_trimmed_text`.
    let Some(path_str) = (unsafe { extract_trimmed_text(file_name, hint) }) else {
        return -1;
    };
    if fs::remove_file(&path_str).is_ok() {
        0
    } else {
        -1
    }
}

/// Copies `src_name` to `dst_name` (`CBL_COPY_FILE`).
///
/// # Arguments
///
/// * `src_name` - Source path byte pointer.
/// * `dst_name` - Destination path byte pointer.
///
/// # Safety
///
/// Non-null pointers must reference NUL-terminated or active COBOL path buffers.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_COPY_FILE` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_COPY_FILE(src_name: *const u8, dst_name: *const u8) -> i32 {
    let h0 = active_param_len(0);
    let h1 = active_param_len(1);
    // SAFETY: Validated via `extract_trimmed_text`.
    let (Some(src), Some(dst)) = (unsafe {
        (
            extract_trimmed_text(src_name, h0),
            extract_trimmed_text(dst_name, h1),
        )
    }) else {
        return -1;
    };
    if fs::copy(&src, &dst).is_ok() { 0 } else { -1 }
}

/// Checks whether `file_name` exists and populates optional 24-byte `file_details` (`CBL_CHECK_FILE_EXIST`).
///
/// # Arguments
///
/// * `file_name` - Path byte pointer.
/// * `file_details` - Optional 24-byte `cblt-fileexist-buf` out-pointer, or null.
///
/// # Safety
///
/// Non-null `file_name` must be readable; non-null `file_details` must point to 24 writable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_CHECK_FILE_EXIST` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_CHECK_FILE_EXIST(file_name: *const u8, file_details: *mut u8) -> i32 {
    let hint = active_param_len(0);
    // SAFETY: Validated via `extract_trimmed_text`.
    let Some(path_str) = (unsafe { extract_trimmed_text(file_name, hint) }) else {
        return 35;
    };
    let Ok(meta) = fs::metadata(&path_str) else {
        return 35;
    };
    if !file_details.is_null() {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .unwrap_or_default();
        let dt = CobolDateTime::from_unix_timestamp(
            i64::try_from(mtime.as_secs()).unwrap_or(0),
            mtime.subsec_micros(),
        );
        let [sz0, sz1, sz2, sz3, sz4, sz5, sz6, sz7] = meta.len().to_be_bytes();
        let [yr0, yr1] = dt.year.to_be_bytes();
        let packet: [u8; 24] = [
            sz0, sz1, sz2, sz3, sz4, sz5, sz6, sz7, dt.day, dt.month, yr0, yr1, dt.hour, dt.minute,
            dt.second, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        // SAFETY: Caller guarantees 24 writable bytes at `file_details`.
        unsafe {
            core::ptr::copy_nonoverlapping(packet.as_ptr(), file_details, 24);
        }
    }
    0
}

/// Renames `old_name` to `new_name` (`CBL_RENAME_FILE`).
///
/// # Arguments
///
/// * `old_name` - Existing path byte pointer.
/// * `new_name` - Target path byte pointer.
///
/// # Safety
///
/// Non-null pointers must reference NUL-terminated or active COBOL path buffers.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_RENAME_FILE` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_RENAME_FILE(old_name: *const u8, new_name: *const u8) -> i32 {
    let h0 = active_param_len(0);
    let h1 = active_param_len(1);
    // SAFETY: Validated via `extract_trimmed_text`.
    let (Some(old_p), Some(new_p)) = (unsafe {
        (
            extract_trimmed_text(old_name, h0),
            extract_trimmed_text(new_name, h1),
        )
    }) else {
        return -1;
    };
    if fs::rename(&old_p, &new_p).is_ok() {
        0
    } else {
        -1
    }
}

/// Deletes a file or empty directory (`C$DELETE` / `C_DELETE`).
///
/// # Arguments
///
/// * `file_name` - Path byte pointer.
/// * `flags` - Optional mode byte (`b'D'`/`b'd'` deletes directory, else file).
///
/// # Safety
///
/// Non-null pointers must reference readable storage.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_DELETE` (`C$DELETE`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_DELETE(file_name: *const u8, flags: *const u8) -> i32 {
    let hint = active_param_len(0);
    // SAFETY: Validated via `extract_trimmed_text`.
    let Some(path_str) = (unsafe { extract_trimmed_text(file_name, hint) }) else {
        return 1;
    };
    let is_dir = !flags.is_null() && {
        // SAFETY: Validated non-null `flags`.
        let ch = unsafe { *flags };
        ch == b'D' || ch == b'd'
    };
    let ok = if is_dir {
        fs::remove_dir(&path_str).is_ok()
    } else {
        fs::remove_file(&path_str).is_ok()
    };
    i32::from(!ok)
}

/// Copies `src_name` to `dst_name` (`C$COPY` / `C_COPY`).
///
/// # Arguments
///
/// * `src_name` - Source path byte pointer.
/// * `dst_name` - Destination path byte pointer.
/// * `flags` - Optional flags pointer (unused/nullable).
///
/// # Safety
///
/// Non-null pointers must reference readable path storage.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_COPY` (`C$COPY`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_COPY(src_name: *const u8, dst_name: *const u8, flags: *const u8) -> i32 {
    let _ = flags;
    // SAFETY: Forwarded to `CBL_COPY_FILE`.
    i32::from(unsafe { CBL_COPY_FILE(src_name, dst_name) } != 0)
}

/// Populates 16-byte `file_info` (`u64` BE size + `u32` BE `YYYYMMDD` + `u32` BE `HHMMSS00`) (`C$FILEINFO` / `C_FILEINFO`).
///
/// # Arguments
///
/// * `file_name` - Path byte pointer.
/// * `file_info` - 16-byte out-buffer pointer.
///
/// # Safety
///
/// Non-null `file_name` must be readable; non-null `file_info` must reference 16 writable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_FILEINFO` (`C$FILEINFO`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_FILEINFO(file_name: *const u8, file_info: *mut u8) -> i32 {
    if file_info.is_null() {
        return 1;
    }
    let hint = active_param_len(0);
    // SAFETY: Validated via `extract_trimmed_text`.
    let Some(path_str) = (unsafe { extract_trimmed_text(file_name, hint) }) else {
        return 1;
    };
    let Ok(meta) = fs::metadata(&path_str) else {
        return 1;
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    let dt = CobolDateTime::from_unix_timestamp(
        i64::try_from(mtime.as_secs()).unwrap_or(0),
        mtime.subsec_micros(),
    );
    let yyyymmdd = u32::from(dt.year) * 10_000 + u32::from(dt.month) * 100 + u32::from(dt.day);
    let hhmmsscc = u32::from(dt.hour) * 1_000_000
        + u32::from(dt.minute) * 10_000
        + u32::from(dt.second) * 100
        + (dt.microsecond / 10_000);
    let [s0, s1, s2, s3, s4, s5, s6, s7] = meta.len().to_be_bytes();
    let [d0, d1, d2, d3] = yyyymmdd.to_be_bytes();
    let [t0, t1, t2, t3] = hhmmsscc.to_be_bytes();
    let packet: [u8; 16] = [
        s0, s1, s2, s3, s4, s5, s6, s7, d0, d1, d2, d3, t0, t1, t2, t3,
    ];
    // SAFETY: Validated non-null `file_info` with 16-byte `FILE-INFO` group layout.
    unsafe {
        core::ptr::copy_nonoverlapping(packet.as_ptr(), file_info, 16);
    }
    0
}

/// Writes the current working directory into `dir_buf[..dir_len]` (`CBL_GET_CURRENT_DIR`).
///
/// # Arguments
///
/// * `flags` - Reserved flags (`0`).
/// * `dir_len` - Capacity of `dir_buf` in bytes.
/// * `dir_buf` - Destination byte buffer.
///
/// # Safety
///
/// When `dir_len > 0`, non-null `dir_buf` must point to `dir_len` writable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_GET_CURRENT_DIR` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_GET_CURRENT_DIR(flags: i32, dir_len: i32, dir_buf: *mut u8) -> i32 {
    let _ = flags;
    let Ok(cap) = usize::try_from(dir_len) else {
        return -1;
    };
    if cap == 0 || dir_buf.is_null() {
        return -1;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let cwd_str = cwd.to_string_lossy();
    let cwd_bytes = cwd_str.as_bytes();
    if cwd_bytes.len() > cap {
        return 1;
    }
    // SAFETY: Validated `cap > 0`, non-null `dir_buf`, and `cwd_bytes.len() <= cap`.
    unsafe {
        core::slice::from_raw_parts_mut(dir_buf, cap).fill(b' ');
        core::ptr::copy_nonoverlapping(cwd_bytes.as_ptr(), dir_buf, cwd_bytes.len());
    }
    0
}

/// Changes the process working directory to `dir_name` (`CBL_CHANGE_DIR`).
///
/// # Arguments
///
/// * `dir_name` - Target directory path pointer.
///
/// # Safety
///
/// Non-null `dir_name` must reference a readable path string.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_CHANGE_DIR` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_CHANGE_DIR(dir_name: *const u8) -> i32 {
    let hint = active_param_len(0);
    // SAFETY: Validated via `extract_trimmed_text`.
    let Some(path_str) = (unsafe { extract_trimmed_text(dir_name, hint) }) else {
        return -1;
    };
    if std::env::set_current_dir(Path::new(&path_str)).is_ok() {
        0
    } else {
        -1
    }
}

/// Creates directory `dir_name` (`CBL_CREATE_DIR`).
///
/// # Arguments
///
/// * `dir_name` - Directory path byte pointer.
///
/// # Safety
///
/// Non-null `dir_name` must reference a readable path string.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_CREATE_DIR` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_CREATE_DIR(dir_name: *const u8) -> i32 {
    let hint = active_param_len(0);
    // SAFETY: Validated via `extract_trimmed_text`.
    let Some(path_str) = (unsafe { extract_trimmed_text(dir_name, hint) }) else {
        return -1;
    };
    if fs::create_dir(&path_str).is_ok() {
        0
    } else {
        -1
    }
}

/// Removes empty directory `dir_name` (`CBL_DELETE_DIR`).
///
/// # Arguments
///
/// * `dir_name` - Directory path byte pointer.
///
/// # Safety
///
/// Non-null `dir_name` must reference a readable path string.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_DELETE_DIR` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_DELETE_DIR(dir_name: *const u8) -> i32 {
    let hint = active_param_len(0);
    // SAFETY: Validated via `extract_trimmed_text`.
    let Some(path_str) = (unsafe { extract_trimmed_text(dir_name, hint) }) else {
        return -1;
    };
    if fs::remove_dir(&path_str).is_ok() {
        0
    } else {
        -1
    }
}

/// Creates directory `dir_name` (`C$MAKEDIR` / `C_MAKEDIR`), returning `0` on success or `1` on failure.
///
/// # Arguments
///
/// * `dir_name` - Directory path byte pointer.
///
/// # Safety
///
/// Non-null `dir_name` must reference a readable path string.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_MAKEDIR` (`C$MAKEDIR`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_MAKEDIR(dir_name: *const u8) -> i32 {
    // SAFETY: Forwarded to `CBL_CREATE_DIR`.
    i32::from(unsafe { CBL_CREATE_DIR(dir_name) } != 0)
}

/// Sleeps for `nanos` nanoseconds parsed from an ASCII digit buffer or 8-byte `u64` (`CBL_OC_NANOSLEEP`).
///
/// # Arguments
///
/// * `nanos_ptr` - Pointer to ASCII digit string or 8-byte native `u64` nanosecond duration.
///
/// # Safety
///
/// Non-null `nanos_ptr` must reference either an active COBOL parameter or 8 readable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_OC_NANOSLEEP` C-ABI symbol."
)]
pub unsafe extern "C" fn CBL_OC_NANOSLEEP(nanos_ptr: *const u8) -> i32 {
    if nanos_ptr.is_null() {
        return -1;
    }
    let hint = active_param_len(0);
    let len = if hint > 0 { hint } else { 8 };
    // SAFETY: Validated non-null `nanos_ptr`.
    let raw = unsafe { core::slice::from_raw_parts(nanos_ptr, len) };
    let nanos = if raw.iter().all(|b| b.is_ascii_digit() || *b == b' ') {
        String::from_utf8_lossy(raw)
            .trim()
            .parse::<u64>()
            .unwrap_or(0)
    } else if let &[b0, b1, b2, b3, b4, b5, b6, b7, ..] = raw {
        u64::from_ne_bytes([b0, b1, b2, b3, b4, b5, b6, b7])
    } else {
        0
    };
    thread::sleep(Duration::from_nanos(nanos));
    0
}

/// Sleeps for `seconds` parsed from ASCII digits or a 4-byte `u32` (`C$SLEEP` / `C_SLEEP`).
///
/// # Arguments
///
/// * `seconds_ptr` - Pointer to ASCII seconds or 4-byte native `u32`.
///
/// # Safety
///
/// Non-null `seconds_ptr` must reference an active COBOL parameter or 4 readable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_SLEEP` (`C$SLEEP`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_SLEEP(seconds_ptr: *const u8) -> i32 {
    if seconds_ptr.is_null() {
        return -1;
    }
    let hint = active_param_len(0);
    let len = if hint > 0 { hint } else { 4 };
    // SAFETY: Validated non-null `seconds_ptr`.
    let raw = unsafe { core::slice::from_raw_parts(seconds_ptr, len) };
    let secs = if raw.iter().all(|b| b.is_ascii_digit() || *b == b' ') {
        String::from_utf8_lossy(raw)
            .trim()
            .parse::<u64>()
            .unwrap_or(0)
    } else if let &[b0, b1, b2, b3, ..] = raw {
        u64::from(u32::from_ne_bytes([b0, b1, b2, b3]))
    } else {
        0
    };
    if std::env::var_os("COBLANG_FAST_SLEEP").is_some() {
        thread::sleep(Duration::from_millis(secs.min(1)));
    } else {
        thread::sleep(Duration::from_secs(secs));
    }
    0
}

/// Writes the caller's parameter count (or main CLI `argc - 1` at root frame) into `out_count` (`C$NARG` / `C_NARG`).
///
/// # Arguments
///
/// * `out_count` - Pointer to 4-byte `i32` (or active COBOL parameter width `1`/`2`/`4`/`8`).
///
/// # Safety
///
/// Non-null `out_count` must point to writable integer storage.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_NARG` (`C$NARG`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_NARG(out_count: *mut i32) -> i32 {
    if out_count.is_null() {
        return -1;
    }
    let count = with_c_abi_state(|st| {
        if let Some(top) = st.module_stack.last() {
            i32::try_from(top.param_ptrs.len()).unwrap_or(0)
        } else {
            with_global_runtime(|rt| i32::try_from(rt.args().len().saturating_sub(1)).unwrap_or(0))
        }
    });
    // SAFETY: Validated non-null `out_count`.
    unsafe {
        *out_count = count;
    }
    0
}

/// Returns the byte size of 1-based parameter `*param_idx_ptr` in the active module frame (`C$PARAMSIZE` / `C_PARAMSIZE`).
///
/// # Arguments
///
/// * `param_idx_ptr` - Pointer to 1-based parameter index (`i32`).
///
/// # Safety
///
/// Non-null `param_idx_ptr` must point to a readable `i32`.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_PARAMSIZE` (`C$PARAMSIZE`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_PARAMSIZE(param_idx_ptr: *const i32) -> i32 {
    if param_idx_ptr.is_null() {
        return -1;
    }
    // SAFETY: Validated non-null `param_idx_ptr`.
    let idx_1 = unsafe { *param_idx_ptr };
    if idx_1 <= 0 {
        return -1;
    }
    let idx0 = (idx_1 - 1).cast_unsigned() as usize;
    let sz = with_c_abi_state(|st| {
        st.module_stack
            .last()
            .and_then(|f| f.param_sizes.get(idx0).copied())
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(-1)
    });
    with_global_runtime(|rt| rt.set_return_code(sz));
    sz
}

/// Writes the caller's `PROGRAM-ID` into `out_buf[..buf_len]` (`C$CALLEDBY` / `C_CALLEDBY`),
/// returning `1` when called by a parent COBOL module or `0` at the top-level program.
///
/// # Arguments
///
/// * `out_buf` - Destination character buffer.
/// * `buf_len` - Buffer capacity (`0` falls back to active frame parameter 0 length).
///
/// # Safety
///
/// When effective length `> 0`, non-null `out_buf` must point to `eff_len` writable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_CALLEDBY` (`C$CALLEDBY`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_CALLEDBY(out_buf: *mut u8, buf_len: usize) -> i32 {
    if out_buf.is_null() {
        return -1;
    }
    let eff_len = if buf_len > 0 {
        buf_len
    } else {
        active_param_len(0)
    };
    if eff_len == 0 {
        return 0;
    }
    let caller_opt = with_c_abi_state(|st| {
        if st.module_stack.len() >= 2 {
            st.module_stack
                .get(st.module_stack.len() - 2)
                .map(|f| f.module_name.clone())
                .filter(|s| !s.is_empty())
        } else {
            None
        }
    });
    // SAFETY: Validated non-null `out_buf` and `eff_len > 0`.
    unsafe {
        core::slice::from_raw_parts_mut(out_buf, eff_len).fill(b' ');
    }
    if let Some(caller) = caller_opt {
        let cb = caller.as_bytes();
        let n = cb.len().min(eff_len);
        // SAFETY: Validated non-null `out_buf` and `n <= cb.len().min(eff_len)`.
        unsafe {
            core::ptr::copy_nonoverlapping(cb.as_ptr(), out_buf, n);
        }
        1
    } else {
        0
    }
}

/// Returns the current OS process identifier (`C$GETPID` / `C_GETPID`).
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_GETPID` (`C$GETPID`) C-ABI symbol."
)]
pub extern "C" fn C_GETPID() -> i32 {
    std::process::id().cast_signed()
}

/// Replaces non-printable bytes outside `0x20..=0x7E` in `buf` with `*repl_ptr` (or `b'.'`) (`C$PRINTABLE` / `C_PRINTABLE`).
///
/// # Arguments
///
/// * `buf` - Mutable character buffer (uses active frame's parameter 0 size if `> 0`, else NUL-terminated length).
/// * `repl_ptr` - Optional 1-byte replacement character pointer, or null for `b'.'`.
///
/// # Safety
///
/// Non-null `buf` must reference writable bytes; non-null `repl_ptr` must point to 1 readable byte.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `C_PRINTABLE` (`C$PRINTABLE`) C-ABI symbol."
)]
pub unsafe extern "C" fn C_PRINTABLE(buf: *mut u8, repl_ptr: *const u8) -> i32 {
    if buf.is_null() {
        return -1;
    }
    let frame_len = active_param_len(0);
    let len = if frame_len > 0 {
        frame_len
    } else {
        // SAFETY: NUL-terminated fallback when invoked directly from C.
        unsafe { CStr::from_ptr(buf.cast::<c_char>()) }
            .to_bytes()
            .len()
    };
    let repl = if repl_ptr.is_null() {
        b'.'
    } else {
        // SAFETY: Validated non-null `repl_ptr`.
        unsafe { *repl_ptr }
    };
    // SAFETY: Validated non-null `buf` across `len` bytes.
    let slice = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    for b in slice {
        if !matches!(*b, 0x20..=0x7E) {
            *b = repl;
        }
    }
    0
}

/// Writes active parameter count into `out_ptr` honoring COBOL integer byte width (`1`/`2`/`4`/`8`) and endianness.
///
/// # Arguments
///
/// * `out_ptr` - Destination integer buffer.
/// * `width` - Byte width (`1`, `2`, `8`, or default `4`).
/// * `big_endian` - `true` for big-endian `USAGE BINARY` / `COMP`, `false` for native `BINARY-LONG` / `COMP-5`.
///
/// # Safety
///
/// Non-null `out_ptr` must reference `width.max(1)` writable bytes.
#[expect(unsafe_code, reason = "Audited width-aware `C$NARG` output write.")]
unsafe fn write_narg_sized(out_ptr: *mut u8, width: usize, big_endian: bool) -> i32 {
    if out_ptr.is_null() {
        return -1;
    }
    let count = with_c_abi_state(|st| {
        if let Some(top) = st.module_stack.last() {
            i32::try_from(top.param_ptrs.len()).unwrap_or(0)
        } else {
            with_global_runtime(|rt| i32::try_from(rt.args().len().saturating_sub(1)).unwrap_or(0))
        }
    });
    match width {
        1 => {
            let [b0, ..] = count.to_le_bytes();
            // SAFETY: Validated non-null `out_ptr` for 1 byte.
            unsafe { *out_ptr = b0 };
        }
        2 => {
            let v = i16::try_from(count).unwrap_or(0);
            let bytes = if big_endian {
                v.to_be_bytes()
            } else {
                v.to_ne_bytes()
            };
            // SAFETY: Validated non-null `out_ptr` for 2 bytes.
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr, 2) };
        }
        8 => {
            let v = i64::from(count);
            let bytes = if big_endian {
                v.to_be_bytes()
            } else {
                v.to_ne_bytes()
            };
            // SAFETY: Validated non-null `out_ptr` for 8 bytes.
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr, 8) };
        }
        _ => {
            let bytes = if big_endian {
                count.to_be_bytes()
            } else {
                count.to_ne_bytes()
            };
            // SAFETY: Validated non-null `out_ptr` for 4 bytes.
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr, 4) };
        }
    }
    0
}

/// Reads a 1-based parameter index from `ptr` honoring COBOL integer byte width (`1`/`2`/`4`/`8`) and endianness.
///
/// # Arguments
///
/// * `ptr` - Source integer buffer.
/// * `width` - Byte width (`1`, `2`, `8`, or default `4`).
/// * `big_endian` - `true` for big-endian `USAGE BINARY` / `COMP`.
///
/// # Safety
///
/// Non-null `ptr` must reference `width.max(1)` readable bytes.
#[expect(unsafe_code, reason = "Audited width-aware `C$PARAMSIZE` index read.")]
unsafe fn read_param_idx_sized(ptr: *const u8, width: usize, big_endian: bool) -> i32 {
    if ptr.is_null() {
        return -1;
    }
    let idx_val = match width {
        1 => {
            // SAFETY: Validated non-null `ptr` for 1 byte.
            i32::from(unsafe { *ptr })
        }
        2 => {
            let mut b = [0_u8; 2];
            // SAFETY: Validated non-null `ptr` for 2 bytes.
            unsafe { core::ptr::copy_nonoverlapping(ptr, b.as_mut_ptr(), 2) };
            i32::from(if big_endian {
                i16::from_be_bytes(b)
            } else {
                i16::from_ne_bytes(b)
            })
        }
        8 => {
            let mut b = [0_u8; 8];
            // SAFETY: Validated non-null `ptr` for 8 bytes.
            unsafe { core::ptr::copy_nonoverlapping(ptr, b.as_mut_ptr(), 8) };
            let v = if big_endian {
                i64::from_be_bytes(b)
            } else {
                i64::from_ne_bytes(b)
            };
            i32::try_from(v).unwrap_or(-1)
        }
        _ => {
            let mut b = [0_u8; 4];
            // SAFETY: Validated non-null `ptr` for 4 bytes.
            unsafe { core::ptr::copy_nonoverlapping(ptr, b.as_mut_ptr(), 4) };
            if big_endian {
                i32::from_be_bytes(b)
            } else {
                i32::from_ne_bytes(b)
            }
        }
    };
    // SAFETY: Stack `idx_val` is valid for 1 `i32` read.
    unsafe { C_PARAMSIZE(&raw const idx_val) }
}

/// Dispatches built-in `SYSTEM`, `CBL_*`, and `C$*` subroutines with per-argument byte sizes and endian flags.
///
/// # Arguments
///
/// * `name` - Target subroutine name.
/// * `args` - Raw `usize` argument words.
/// * `sizes` - Per-argument byte sizes (`0` when unknown).
/// * `be_flags` - Per-argument big-endian binary flags (`1` = big-endian `COMP`, `0` = native).
///
/// # Safety
///
/// Caller guarantees `args` and `sizes` describe valid memory ranges matching the target subroutine's ABI.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Dispatches validated `CALL` argument pointers and widths to built-in `C$*` / `CBL_*` symbols."
)]
pub unsafe fn try_dispatch_builtin_with_meta(
    name: &str,
    args: &[usize],
    sizes: &[usize],
    be_flags: &[u8],
) -> Option<i32> {
    let upper = name.trim().to_ascii_uppercase();
    let a0 = args.first().copied().unwrap_or(0);
    let a1 = args.get(1).copied().unwrap_or(0);
    let a2 = args.get(2).copied().unwrap_or(0);
    let sz0 = sizes.first().copied().unwrap_or(0);
    let be0 = be_flags.first().copied().unwrap_or(0) != 0;

    // Call-frame introspection builtins inspect the caller's own `ActiveModuleFrame`.
    match upper.as_str() {
        "C$NARG" | "C_NARG" => {
            // SAFETY: Caller guarantees `a0` points to `sz0` writable bytes when non-null.
            return Some(unsafe { write_narg_sized(a0 as *mut u8, sz0, be0) });
        }
        "C$PARAMSIZE" | "C_PARAMSIZE" => {
            // SAFETY: Caller guarantees `a0` points to `sz0` readable bytes when non-null.
            return Some(unsafe { read_param_idx_sized(a0 as *const u8, sz0, be0) });
        }
        "C$CALLEDBY" | "C_CALLEDBY" => {
            let eff_len = if a1 > 0 { a1 } else { sz0 };
            // SAFETY: Caller guarantees `a0` points to `eff_len` writable bytes when non-null.
            return Some(unsafe { C_CALLEDBY(a0 as *mut u8, eff_len) });
        }
        _ => {}
    }

    let push_hint_frame = !sizes.is_empty();
    if push_hint_frame {
        with_c_abi_state(|st| {
            st.module_stack.push(crate::ActiveModuleFrame {
                module_name: upper.clone(),
                param_ptrs: args.to_vec(),
                param_sizes: sizes.to_vec(),
                ..crate::ActiveModuleFrame::default()
            });
        });
    }
    // SAFETY: Caller guarantees argument pointers match target built-in subroutine contract.
    let out = unsafe {
        match upper.as_str() {
            "SYSTEM" => Some(SYSTEM(a0 as *const u8)),
            "C$JUSTIFY" | "C_JUSTIFY" => Some(C_JUSTIFY(a0 as *mut u8, a1 as *const u8)),
            "C$PRINTABLE" | "C_PRINTABLE" => Some(C_PRINTABLE(a0 as *mut u8, a1 as *const u8)),
            "C$GETPID" | "C_GETPID" => Some(C_GETPID()),
            "C$TOUPPER" | "C_TOUPPER" | "CBL_TOUPPER" => Some(CBL_TOUPPER(a0 as *mut u8, a1)),
            "C$TOLOWER" | "C_TOLOWER" | "CBL_TOLOWER" => Some(CBL_TOLOWER(a0 as *mut u8, a1)),
            "CBL_AND" => Some(CBL_AND(a0 as *const u8, a1 as *mut u8, a2)),
            "CBL_OR" => Some(CBL_OR(a0 as *const u8, a1 as *mut u8, a2)),
            "CBL_NOR" => Some(CBL_NOR(a0 as *const u8, a1 as *mut u8, a2)),
            "CBL_XOR" => Some(CBL_XOR(a0 as *const u8, a1 as *mut u8, a2)),
            "CBL_EQ" => Some(CBL_EQ(a0 as *const u8, a1 as *mut u8, a2)),
            "CBL_IMP" => Some(CBL_IMP(a0 as *const u8, a1 as *mut u8, a2)),
            "CBL_NIMP" => Some(CBL_NIMP(a0 as *const u8, a1 as *mut u8, a2)),
            "CBL_NOT" => Some(CBL_NOT(a0 as *mut u8, a1)),
            "C$MAKEDIR" | "C_MAKEDIR" => Some(C_MAKEDIR(a0 as *const u8)),
            "C$DELETE" | "C_DELETE" => Some(C_DELETE(a0 as *const u8, a1 as *const u8)),
            "C$COPY" | "C_COPY" => Some(C_COPY(a0 as *const u8, a1 as *const u8, a2 as *const u8)),
            "C$FILEINFO" | "C_FILEINFO" => Some(C_FILEINFO(a0 as *const u8, a1 as *mut u8)),
            "C$SLEEP" | "C_SLEEP" => Some(C_SLEEP(a0 as *const u8)),
            "CBL_GC_WINDOW" => Some(crate::screen_abi::CBL_GC_WINDOW(
                a0 as *const u8,
                a1 as *mut u8,
            )),
            "CBL_GC_SET_SCR_SIZE" => {
                let r = i32::try_from(a0).unwrap_or(24);
                let c = i32::try_from(a1).unwrap_or(80);
                Some(crate::screen_abi::CBL_GC_SET_SCR_SIZE(r, c))
            }
            "CBL_GET_CSR_POS" => Some(crate::screen_abi::CBL_GET_CSR_POS(a0 as *mut u8)),
            "POSIX-ERRNO" => {
                let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                let sz1 = sizes.get(1).copied().unwrap_or(0);
                if a0 != 0 && sz0 >= 4 {
                    let bytes = code.to_ne_bytes();
                    core::ptr::copy_nonoverlapping(bytes.as_ptr(), a0 as *mut u8, 4);
                }
                if a1 != 0 && sz1 > 0 && sz1 <= 65_536 {
                    let rendered = std::io::Error::from_raw_os_error(code).to_string();
                    let rb = rendered.as_bytes();
                    let copy_n = rb.len().min(sz1);
                    core::slice::from_raw_parts_mut(a1 as *mut u8, sz1).fill(b' ');
                    core::ptr::copy_nonoverlapping(rb.as_ptr(), a1 as *mut u8, copy_n);
                }
                Some(code)
            }
            "POSIX_ERRNO" => Some(std::io::Error::last_os_error().raw_os_error().unwrap_or(0)),
            "STRERROR" => Some(0),
            _ => None,
        }
    };
    if push_hint_frame {
        with_c_abi_state(|st| {
            let _ = st.module_stack.pop();
        });
    }
    out
}

/// Dispatches built-in `SYSTEM`, `CBL_*`, and `C$*` subroutines when invoked via [`crate::cob_call`]
/// using exact `"C$*"` or `"CBL_*"` names.
///
/// # Arguments
///
/// * `name` - Target subroutine name.
/// * `args` - Raw `usize` argument words supplied to `cob_call`.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Dispatches validated `cob_call` argument pointers to built-in `C$*` / `CBL_*` symbols."
)]
pub fn try_dispatch_builtin_c_abi(name: &str, args: &[usize]) -> Option<i32> {
    // SAFETY: `cob_call` caller guarantees argument words match the target built-in subroutine's C-ABI contract.
    unsafe { try_dispatch_builtin_with_meta(name, args, &[], &[]) }
}
