//! Audited clean-room C-ABI entrypoints for Milestone M11 (`ALLOCATE`, `FREE`, `BASED`/`LINKAGE`
//! null-dereference checks, `SET prog-ptr TO ENTRY`, `cob_malloc`, and `cob_free`).

use std::ffi::c_void;
use std::io::{self, Write as _};

use coblang_dynload::resolve_and_load_module;
use coblang_rt_core::{field_ops::evaluate_based_pointer, with_global_runtime};

use crate::{build_module_search_dirs, read_raw_slice, with_loaded_modules};

/// Maximum single COBOL heap or `EXTERNAL` segment allocation (`1 TiB`), rejecting bogus sizes cleanly.
const MAX_HEAP_ALLOC_BYTES: i64 = 1_i64 << 40;

/// 16-byte-aligned storage quantum backing `ALLOCATE`, `cob_malloc`, and `IS EXTERNAL` buffers.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct AlignedChunk([u8; 16]);

/// Allocates `alloc_len` (`>= 1`) bytes rounded up to 16-byte [`AlignedChunk`]s filled with `fill`.
///
/// # Arguments
///
/// * `alloc_len` - Positive byte length (`>= 1`).
/// * `fill` - Initial fill byte.
fn alloc_aligned_chunks(alloc_len: usize, fill: u8) -> *mut u8 {
    let chunks = alloc_len.div_ceil(16);
    let boxed: Box<[AlignedChunk]> = vec![AlignedChunk([fill; 16]); chunks].into_boxed_slice();
    Box::into_raw(boxed).cast::<u8>()
}

/// Reclaims a heap block previously allocated by [`alloc_aligned_chunks`] with `alloc_len`.
///
/// # Arguments
///
/// * `ptr` - Opaque base pointer originally returned by [`alloc_aligned_chunks`].
/// * `alloc_len` - Exact `alloc_len` recorded at allocation time.
///
/// # Safety
///
/// `(ptr, alloc_len)` must have been allocated by [`alloc_aligned_chunks`] and must be freed at most once.
#[expect(
    unsafe_code,
    reason = "Reconstructs `Box<[AlignedChunk]>` from tracked `(ptr, alloc_len)` pair."
)]
unsafe fn dealloc_aligned_chunks(ptr: *mut c_void, alloc_len: usize) {
    let chunks = alloc_len.div_ceil(16);
    let slice_ptr = core::ptr::slice_from_raw_parts_mut(ptr.cast::<AlignedChunk>(), chunks);
    // SAFETY: Guaranteed by caller and `HeapTracker` single-removal invariant.
    unsafe {
        drop(Box::from_raw(slice_ptr));
    }
}

/// Allocates a 16-byte-aligned heap buffer of `size.max(1)` bytes filled with `fill_byte`,
/// registering the block in the run-unit [`coblang_rt_core::heap::HeapTracker`].
///
/// Returns null if `size > MAX_HEAP_ALLOC_BYTES`.
///
/// # Arguments
///
/// * `size` - Requested byte count (`<= 0` clamped to `1`).
/// * `fill_byte` - Initial fill byte (`0..=255`).
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for COBOL `ALLOCATE` and `cob_malloc`."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_alloc(size: i64, fill_byte: i32) -> *mut u8 {
    if size > MAX_HEAP_ALLOC_BYTES {
        return core::ptr::null_mut();
    }
    let alloc_len = usize::try_from(size.max(1)).unwrap_or(1);
    let [fill, _, _, _] = fill_byte.clamp(0, 255).to_le_bytes();
    let ptr = alloc_aligned_chunks(alloc_len, fill);
    with_global_runtime(|rt| {
        rt.exceptions_mut().clear_last_exception();
        rt.heap_mut().record(ptr as usize, alloc_len);
    });
    ptr
}

/// Allocates `size.max(1)` bytes and initializes them from `(pat_ptr, pat_len)` (`INITIALIZED TO expr`).
///
/// When `pad_byte >= 0`, copies `pat[..min(pat_len, alloc_len)]` once and pads remaining bytes with
/// `pad_byte as u8`. When `pad_byte < 0`, repeats `pat` cyclically across `0..alloc_len` (`ALL "Z"`).
///
/// # Arguments
///
/// * `size` - Requested byte count (`<= 0` clamped to `1`).
/// * `pat_ptr` - Source pattern byte pointer (may be null when `pat_len == 0`).
/// * `pat_len` - Source pattern byte length.
/// * `pad_byte` - Non-negative padding byte (`0..=255`), or negative (`-1`) for cyclic `ALL` repeat.
///
/// # Safety
///
/// When `pat_len > 0` and `!pat_ptr.is_null()`, `pat_ptr` must reference `pat_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited `ALLOCATE ... CHARACTERS INITIALIZED TO` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_alloc_fill_pattern(
    size: i64,
    pat_ptr: *const u8,
    pat_len: usize,
    pad_byte: i32,
) -> *mut u8 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(pat) =
        (unsafe { read_raw_slice(pat_ptr, pat_len, "coblang_rt_alloc_fill_pattern", "pat_ptr") })
    else {
        return core::ptr::null_mut();
    };
    let default_fill = if pad_byte >= 0 { pad_byte } else { 0 };
    let ptr = coblang_rt_alloc(size, default_fill);
    if ptr.is_null() || pat.is_empty() {
        return ptr;
    }
    let alloc_len = usize::try_from(size.max(1)).unwrap_or(1);
    // SAFETY: `ptr` was freshly allocated by `coblang_rt_alloc` with `alloc_len` writable bytes.
    let dst = unsafe { core::slice::from_raw_parts_mut(ptr, alloc_len) };
    if pad_byte >= 0 {
        for (d, &s) in dst.iter_mut().zip(pat.iter()) {
            *d = s;
        }
    } else {
        for (d, &s) in dst.iter_mut().zip(pat.iter().cycle()) {
            *d = s;
        }
    }
    ptr
}

/// Releases a heap block previously allocated via [`coblang_rt_alloc`] or [`cob_malloc`].
///
/// Passing a null pointer is a safe ISO 2023 no-op returning `0`. Passing an untracked non-null
/// pointer records `"EC-STORAGE-NOT-ALLOC"` and returns `1` without deallocating.
///
/// # Arguments
///
/// * `ptr` - Heap block pointer to release, or null.
///
/// # Safety
///
/// If `ptr` matches an active allocation tracked by [`coblang_rt_core::heap::HeapTracker`], no
/// live references to that block may exist after this call.
#[expect(
    unsafe_code,
    reason = "Audited heap deallocation matching tracked `HeapTracker` layout."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_free(ptr: *mut u8) -> i32 {
    if ptr.is_null() {
        return 0;
    }
    let taken = with_global_runtime(|rt| rt.heap_mut().take(ptr as usize));
    let Some(alloc_len) = taken else {
        with_global_runtime(|rt| rt.record_exception("EC-STORAGE-NOT-ALLOC"));
        return 1;
    };
    // SAFETY: `(ptr, alloc_len)` was recorded by `coblang_rt_alloc` and removed once via `take`.
    unsafe {
        dealloc_aligned_chunks(ptr.cast::<c_void>(), alloc_len);
    }
    0
}

/// Releases all remaining tracked `ALLOCATE`, `cob_malloc`, and `IS EXTERNAL` blocks during `cob_tidy`.
pub(crate) fn free_all_tracked_heap_blocks() {
    let remaining = with_global_runtime(|rt| rt.heap_mut().drain_all());
    for (addr, alloc_len) in remaining {
        #[expect(
            unsafe_code,
            reason = "Audited `cob_tidy` teardown of live `HeapTracker` allocations."
        )]
        // SAFETY: `(addr, alloc_len)` was allocated via `alloc_aligned_chunks` and drained once.
        unsafe {
            dealloc_aligned_chunks(addr as *mut c_void, alloc_len);
        }
    }
}

/// Clean-room `libcob`-compatible zero-initialized heap allocator (`cob_malloc`).
///
/// # Arguments
///
/// * `size` - Byte count to allocate.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `cob_malloc` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn cob_malloc(size: usize) -> *mut c_void {
    let sz_i64 = i64::try_from(size).unwrap_or(i64::MAX);
    coblang_rt_alloc(sz_i64, 0).cast::<c_void>()
}

/// Clean-room `libcob`-compatible heap deallocator (`cob_free`).
///
/// # Arguments
///
/// * `ptr` - Pointer returned by [`cob_malloc`], or null.
///
/// # Safety
///
/// Must uphold the pointer validity contract of [`coblang_rt_free`].
#[expect(
    unsafe_code,
    reason = "Audited clean-room `cob_free` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cob_free(ptr: *mut c_void) {
    // SAFETY: Forwarded directly from caller's `cob_free` preconditions.
    let _ = unsafe { coblang_rt_free(ptr.cast::<u8>()) };
}

/// Validates that a `BASED` or `LINKAGE` base pointer `ptr` is non-null before dereference (`EC-DATA-PTR-NULL`).
///
/// Returns `0` when `ptr` is non-null, `1` when `ptr` is null (after printing diagnostic and running `cob_tidy`),
/// or `-1` if metadata slice pointers are invalid.
///
/// # Arguments
///
/// * `ptr` - Candidate base pointer to dereference.
/// * `loc_ptr` - UTF-8 `file:line` location bytes.
/// * `loc_len` - Byte length of `loc_ptr`.
/// * `root_ptr` - UTF-8 `01`/`77` root item name bytes.
/// * `root_len` - Byte length of `root_ptr`.
/// * `item_ptr` - UTF-8 accessed item name bytes.
/// * `item_len` - Byte length of `item_ptr`.
///
/// # Safety
///
/// Non-empty `(loc_ptr, loc_len)`, `(root_ptr, root_len)`, and `(item_ptr, item_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited `BASED`/`LINKAGE` null-dereference guard C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_check_based(
    ptr: *const u8,
    loc_ptr: *const u8,
    loc_len: usize,
    root_ptr: *const u8,
    root_len: usize,
    item_ptr: *const u8,
    item_len: usize,
) -> i32 {
    if !ptr.is_null() {
        return 0;
    }
    // SAFETY: Forwarded from caller preconditions.
    let Ok(loc_b) = (unsafe { read_raw_slice(loc_ptr, loc_len, "coblang_rt_check_based", "loc") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(root_b) =
        (unsafe { read_raw_slice(root_ptr, root_len, "coblang_rt_check_based", "root") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(item_b) =
        (unsafe { read_raw_slice(item_ptr, item_len, "coblang_rt_check_based", "item") })
    else {
        return -1;
    };
    let _ = io::stdout().flush();
    let _ = evaluate_based_pointer(
        0,
        &String::from_utf8_lossy(loc_b),
        &String::from_utf8_lossy(root_b),
        &String::from_utf8_lossy(item_b),
        &mut io::stderr(),
    );
    with_global_runtime(|rt| {
        rt.record_exception("EC-DATA-PTR-NULL");
        rt.set_return_code(1);
    });
    free_all_tracked_heap_blocks();
    1
}

/// Resolves a subprogram or `ENTRY` name for `SET prog-ptr TO ENTRY / ADDRESS OF PROGRAM`,
/// returning its function address or null (`EC-PROGRAM-NOT-FOUND`) when not found.
///
/// # Arguments
///
/// * `name_ptr` - UTF-8 / ASCII module name bytes.
/// * `name_len` - Byte length of `name_ptr`.
///
/// # Safety
///
/// When `name_len > 0` and `!name_ptr.is_null()`, `name_ptr` must reference `name_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited `SET prog-ptr TO ENTRY` dynamic symbol resolver C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_resolve_entry(
    name_ptr: *const u8,
    name_len: usize,
) -> *const u8 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(raw_bytes) =
        (unsafe { read_raw_slice(name_ptr, name_len, "coblang_rt_resolve_entry", "name_ptr") })
    else {
        return core::ptr::null();
    };
    let name = String::from_utf8_lossy(raw_bytes);
    let env_val = std::env::var("COB_LIBRARY_PATH").ok();
    let dirs = build_module_search_dirs(env_val.as_deref());
    let Ok((handle, entry)) = resolve_and_load_module(&name, &dirs) else {
        with_global_runtime(|rt| rt.record_exception("EC-PROGRAM-NOT-FOUND"));
        return core::ptr::null();
    };
    let fn_ptr = entry.as_fn_ptr();
    with_loaded_modules(|cache| cache.push(handle));
    fn_ptr
}

/// Resolves or creates a run-unit shared `IS EXTERNAL` storage segment identified by `(name_ptr, name_len)`.
///
/// On first lookup in the run unit, allocates `size.max(1)` 16-byte-aligned bytes initialized from
/// `(init_ptr, init_len)` (zero-padding any remainder). Subsequent lookups across modules return
/// the exact same base pointer.
///
/// # Arguments
///
/// * `name_ptr` - Canonical or explicit `AS` segment name bytes.
/// * `name_len` - Byte length of `name_ptr`.
/// * `size` - Required segment byte size (`<= 0` clamped to `1`).
/// * `init_ptr` - Initial template bytes for first-time allocation (may be null when `init_len == 0`).
/// * `init_len` - Byte length of `init_ptr`.
///
/// # Safety
///
/// Non-empty `(name_ptr, name_len)` and `(init_ptr, init_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited `IS EXTERNAL` shared storage resolver C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_resolve_external(
    name_ptr: *const u8,
    name_len: usize,
    size: i64,
    init_ptr: *const u8,
    init_len: usize,
) -> *mut u8 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(name) = (unsafe {
        read_raw_slice(
            name_ptr,
            name_len,
            "coblang_rt_resolve_external",
            "name_ptr",
        )
    }) else {
        return core::ptr::null_mut();
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(init_bytes) = (unsafe {
        read_raw_slice(
            init_ptr,
            init_len,
            "coblang_rt_resolve_external",
            "init_ptr",
        )
    }) else {
        return core::ptr::null_mut();
    };
    if let Some(existing) = with_global_runtime(|rt| rt.heap_mut().lookup_external(name)) {
        return existing as *mut u8;
    }
    if size > MAX_HEAP_ALLOC_BYTES {
        return core::ptr::null_mut();
    }
    let alloc_len = usize::try_from(size.max(1)).unwrap_or(1);
    let ptr = alloc_aligned_chunks(alloc_len, 0);
    // SAFETY: `ptr` owns `alloc_len` freshly allocated writable bytes.
    let dst = unsafe { core::slice::from_raw_parts_mut(ptr, alloc_len) };
    for (d, &s) in dst.iter_mut().zip(init_bytes.iter()) {
        *d = s;
    }
    with_global_runtime(|rt| {
        rt.heap_mut()
            .record_external(name.to_vec(), ptr as usize, alloc_len);
    });
    ptr
}
