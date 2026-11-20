//! Clean-room C-ABI entrypoints for `CobField` move/compare/sign/integer operations and
//! the 256-bit `CobDecimal` fixed-point engine (`cob_move`, `cob_cmp*`, `cob_decimal_*`).

use core::cmp::Ordering;

use coblang_common::Result;
use coblang_decimal::{FixedDecimal, HighOrderPolicy, RoundingMode, SizeErrorStatus, Uint256};
use coblang_hir::HirFigurative;
use coblang_pic::{MoveSource, decode_numeric_field, execute_move, store_decimal_to_field};

use crate::cob_types::{
    COB_FLAG_HAVE_SIGN, COB_FLAG_SIGN_LEADING, COB_FLAG_SIGN_SEPARATE, COB_STORE_KEEP_ON_OVERFLOW,
    COB_STORE_ROUND, COB_STORE_TRUNC_ON_OVERFLOW, COB_TYPE_ALPHANUMERIC_ALL, CobDecimal, CobField,
    is_numeric_cob_type, validate_cob_field_read, validate_cob_field_write,
};

/// Maps [`Ordering`] to `-1`, `0`, or `1` across the C-ABI boundary.
///
/// # Arguments
///
/// * `ord` - Rust [`Ordering`] result.
#[must_use]
const fn ordering_to_c_int(ord: Ordering) -> i32 {
    match ord {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// Compares two ASCII/alphanumeric byte slices with ISO COBOL trailing-space padding.
///
/// # Arguments
///
/// * `lhs` - Left-hand byte slice.
/// * `rhs` - Right-hand byte slice.
#[must_use]
fn cmp_alphanumeric_space_padded(lhs: &[u8], rhs: &[u8]) -> Ordering {
    let max_len = lhs.len().max(rhs.len());
    for idx in 0..max_len {
        let lb = lhs.get(idx).copied().unwrap_or(b' ');
        let rb = rhs.get(idx).copied().unwrap_or(b' ');
        if lb != rb {
            return lb.cmp(&rb);
        }
    }
    Ordering::Equal
}

/// Decodes a readable [`CobField`] into a [`FixedDecimal`] (falling back to parsed ASCII digits for alphanumeric fields).
///
/// # Arguments
///
/// * `field_ptr` - Candidate `*const CobField`.
/// * `symbol` - Exporting C-ABI symbol name.
/// * `param_name` - Parameter name.
///
/// # Errors
///
/// Returns an error if `field_ptr` is null/invalid or cannot be decoded numerically.
///
/// # Safety
///
/// Caller must uphold [`validate_cob_field_read`] pointer invariants.
#[expect(
    unsafe_code,
    reason = "Forwards audited `CobField` read pointer to `validate_cob_field_read`."
)]
unsafe fn decode_cob_field_decimal(
    field_ptr: *const CobField,
    symbol: &'static str,
    param_name: &'static str,
) -> Result<FixedDecimal> {
    // SAFETY: Caller upholds `validate_cob_field_read` preconditions.
    let (bytes, desc, attr) = unsafe { validate_cob_field_read(field_ptr, symbol, param_name) }?;
    if is_numeric_cob_type(attr.field_type) {
        decode_numeric_field(bytes, &desc)
    } else {
        let text = String::from_utf8_lossy(bytes);
        FixedDecimal::parse_str(text.trim())
    }
}

/// Executes ISO COBOL `MOVE` from `src` into `dst` (`cob_move`).
///
/// # Arguments
///
/// * `src` - Source [`CobField`] descriptor pointer.
/// * `dst` - Destination [`CobField`] descriptor pointer.
///
/// # Safety
///
/// Non-null `src` and `dst` must reference valid [`CobField`] descriptors with non-overlapping
/// writable `dst` storage (or identical/overlapping windows handled via temporary copy).
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_move` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_move(src: *const CobField, dst: *mut CobField) {
    // SAFETY: Validated non-null `src` via `validate_cob_field_read`; copied into `src_owned` before borrowing `dst` mutably to stay sound even when `src` and `dst` alias!
    let Ok((src_slice, src_desc, src_attr)) =
        (unsafe { validate_cob_field_read(src, "cob_move", "src") })
    else {
        return;
    };
    let src_owned = src_slice.to_vec();
    // SAFETY: Validated non-null `dst` via `validate_cob_field_write` after dropping `src_slice`.
    let Ok((dst_slice, dst_desc, _)) =
        (unsafe { validate_cob_field_write(dst, "cob_move", "dst") })
    else {
        return;
    };
    if src_attr.field_type == COB_TYPE_ALPHANUMERIC_ALL {
        let _ = execute_move(
            MoveSource::Figurative(HirFigurative::All(src_owned)),
            dst_slice,
            &dst_desc,
        );
    } else {
        let _ = execute_move(
            MoveSource::Field {
                bytes: &src_owned,
                desc: &src_desc,
            },
            dst_slice,
            &dst_desc,
        );
    }
}

/// Compares `f1` and `f2` (`< 0`, `0`, `> 0`) using numeric comparison when both fields are numeric,
/// or space-padded alphanumeric comparison otherwise (`cob_cmp`).
///
/// # Arguments
///
/// * `f1` - Left-hand [`CobField`] pointer.
/// * `f2` - Right-hand [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f1` and `f2` must point to valid readable [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_cmp` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_cmp(f1: *const CobField, f2: *const CobField) -> i32 {
    // SAFETY: Validated via `validate_cob_field_read`.
    let Ok((b1, _, a1)) = (unsafe { validate_cob_field_read(f1, "cob_cmp", "f1") }) else {
        return 0;
    };
    // SAFETY: Validated via `validate_cob_field_read`.
    let Ok((b2, _, a2)) = (unsafe { validate_cob_field_read(f2, "cob_cmp", "f2") }) else {
        return 0;
    };
    if is_numeric_cob_type(a1.field_type) && is_numeric_cob_type(a2.field_type) {
        // SAFETY: Both `f1` and `f2` were validated above.
        return unsafe { cob_numeric_cmp(f1, f2) };
    }
    ordering_to_c_int(cmp_alphanumeric_space_padded(b1, b2))
}

/// Compares two numeric [`CobField`] descriptors algebraically (`cob_numeric_cmp`).
///
/// # Arguments
///
/// * `f1` - Left-hand numeric [`CobField`] pointer.
/// * `f2` - Right-hand numeric [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f1` and `f2` must point to valid readable [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_numeric_cmp` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_numeric_cmp(f1: *const CobField, f2: *const CobField) -> i32 {
    // SAFETY: Validated via `decode_cob_field_decimal`.
    let Ok(d1) = (unsafe { decode_cob_field_decimal(f1, "cob_numeric_cmp", "f1") }) else {
        return 0;
    };
    // SAFETY: Validated via `decode_cob_field_decimal`.
    let Ok(d2) = (unsafe { decode_cob_field_decimal(f2, "cob_numeric_cmp", "f2") }) else {
        return 0;
    };
    ordering_to_c_int(d1.cmp_numeric(&d2))
}

/// Compares `f` algebraically against signed 32-bit integer `val` (`cob_cmp_int`).
///
/// # Arguments
///
/// * `f` - Numeric [`CobField`] pointer.
/// * `val` - Signed 32-bit integer comparand.
///
/// # Safety
///
/// Non-null `f` must point to a valid readable [`CobField`] descriptor.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_cmp_int` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_cmp_int(f: *const CobField, val: i32) -> i32 {
    // SAFETY: Validated via `decode_cob_field_decimal`.
    let Ok(d) = (unsafe { decode_cob_field_decimal(f, "cob_cmp_int", "f") }) else {
        return 0;
    };
    ordering_to_c_int(d.cmp_numeric(&FixedDecimal::from_i64(i64::from(val))))
}

/// Compares `f` algebraically against unsigned 32-bit integer `val` (`cob_cmp_uint`).
///
/// # Arguments
///
/// * `f` - Numeric [`CobField`] pointer.
/// * `val` - Unsigned 32-bit integer comparand.
///
/// # Safety
///
/// Non-null `f` must point to a valid readable [`CobField`] descriptor.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_cmp_uint` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_cmp_uint(f: *const CobField, val: u32) -> i32 {
    // SAFETY: Validated via `decode_cob_field_decimal`.
    let Ok(d) = (unsafe { decode_cob_field_decimal(f, "cob_cmp_uint", "f") }) else {
        return 0;
    };
    ordering_to_c_int(d.cmp_numeric(&FixedDecimal::from_i64(i64::from(val))))
}

/// Extracts a truncated signed 32-bit integer from `f` (`cob_get_int`).
///
/// # Arguments
///
/// * `f` - Readable [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid readable [`CobField`] descriptor.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_get_int` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_get_int(f: *const CobField) -> i32 {
    // SAFETY: Validated via `decode_cob_field_decimal`.
    let Ok(d) = (unsafe { decode_cob_field_decimal(f, "cob_get_int", "f") }) else {
        return 0;
    };
    let (unscaled, _) = d
        .rescale(0, RoundingMode::Truncation)
        .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Exact));
    let v128 = unscaled.mantissa().to_i128().unwrap_or(i128::MAX);
    i32::try_from(v128.clamp(i128::from(i32::MIN), i128::from(i32::MAX))).unwrap_or(0)
}

/// Stores signed 32-bit integer `val` into `f` (`cob_set_int`).
///
/// # Arguments
///
/// * `f` - Writable [`CobField`] pointer.
/// * `val` - Signed 32-bit integer value to store.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`] descriptor.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_set_int` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_set_int(f: *mut CobField, val: i32) {
    // SAFETY: Validated via `validate_cob_field_write`.
    let Ok((dst_bytes, dst_desc, dst_attr)) =
        (unsafe { validate_cob_field_write(f, "cob_set_int", "f") })
    else {
        return;
    };
    let dec = FixedDecimal::from_i64(i64::from(val));
    if is_numeric_cob_type(dst_attr.field_type) {
        let _ = store_decimal_to_field(
            &dec,
            dst_bytes,
            &dst_desc,
            RoundingMode::Truncation,
            HighOrderPolicy::ModuloTruncate,
        );
    } else {
        let formatted = dec.to_string();
        let _ = execute_move(
            MoveSource::AlphanumericLiteral(formatted.as_bytes()),
            dst_bytes,
            &dst_desc,
        );
    }
}

/// Extracts and normalizes the operational sign of zoned/separate-sign field `f` (`cob_real_get_sign`).
///
/// Returns `-1` when negative, `1` when signed non-negative, or `0` when unsigned/null.
///
/// # Arguments
///
/// * `f` - Writable [`CobField`] pointer whose embedded overpunch byte (if any) is normalized to `'0'..'9'`.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`] descriptor.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_real_get_sign` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_real_get_sign(f: *mut CobField) -> i32 {
    // SAFETY: Validated via `validate_cob_field_write`.
    let Ok((bytes, _, attr)) = (unsafe { validate_cob_field_write(f, "cob_real_get_sign", "f") })
    else {
        return 0;
    };
    if (attr.flags & COB_FLAG_HAVE_SIGN) == 0 {
        return 0;
    }
    let leading = (attr.flags & COB_FLAG_SIGN_LEADING) != 0;
    let Some(slot) = (if leading {
        bytes.first_mut()
    } else {
        bytes.last_mut()
    }) else {
        return 0;
    };
    if (attr.flags & COB_FLAG_SIGN_SEPARATE) != 0 {
        return if *slot == b'-' { -1 } else { 1 };
    }
    let b = *slot;
    if (b'p'..=b'y').contains(&b) {
        *slot = b'0' + (b - b'p');
        -1
    } else if b == b'}' {
        *slot = b'0';
        -1
    } else if (b'J'..=b'R').contains(&b) {
        *slot = b'1' + (b - b'J');
        -1
    } else if b == b'{' {
        *slot = b'0';
        1
    } else if (b'A'..=b'I').contains(&b) {
        *slot = b'1' + (b - b'A');
        1
    } else {
        1
    }
}

/// Writes operational sign `sign` (`< 0` negative, `>= 0` non-negative) onto zoned/separate-sign field `f` (`cob_real_put_sign`).
///
/// # Arguments
///
/// * `f` - Writable [`CobField`] pointer.
/// * `sign` - Negative (`< 0`) vs non-negative (`>= 0`) sign indicator.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`] descriptor.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_real_put_sign` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_real_put_sign(f: *mut CobField, sign: i32) {
    // SAFETY: Validated via `validate_cob_field_write`.
    let Ok((bytes, _, attr)) = (unsafe { validate_cob_field_write(f, "cob_real_put_sign", "f") })
    else {
        return;
    };
    if (attr.flags & COB_FLAG_HAVE_SIGN) == 0 {
        return;
    }
    let leading = (attr.flags & COB_FLAG_SIGN_LEADING) != 0;
    let Some(slot) = (if leading {
        bytes.first_mut()
    } else {
        bytes.last_mut()
    }) else {
        return;
    };
    if (attr.flags & COB_FLAG_SIGN_SEPARATE) != 0 {
        *slot = if sign < 0 { b'-' } else { b'+' };
    } else if sign < 0 && slot.is_ascii_digit() {
        *slot = b'p' + (*slot - b'0');
    } else if sign >= 0 && (b'p'..=b'y').contains(slot) {
        *slot = b'0' + (*slot - b'p');
    }
}

/// Initializes a [`CobDecimal`] register to zero (`cob_decimal_init`).
///
/// # Arguments
///
/// * `d` - Destination [`CobDecimal`] pointer.
///
/// # Safety
///
/// When non-null, `d` must point to writable storage for one [`CobDecimal`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_init` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn cob_decimal_init(d: *mut CobDecimal) {
    if !d.is_null() {
        // SAFETY: Validated non-null `d`.
        unsafe {
            (*d).clear_zero();
        }
    }
}

/// Resets a [`CobDecimal`] register to zero (`cob_decimal_clear`).
///
/// # Arguments
///
/// * `d` - Target [`CobDecimal`] pointer.
///
/// # Safety
///
/// When non-null, `d` must point to writable storage for one [`CobDecimal`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_clear` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn cob_decimal_clear(d: *mut CobDecimal) {
    // SAFETY: Same contract as `cob_decimal_init`.
    unsafe {
        cob_decimal_init(d);
    }
}

/// Loads `f` into decimal register `d` (`cob_decimal_set_field`).
///
/// # Arguments
///
/// * `d` - Destination [`CobDecimal`] pointer.
/// * `f` - Source [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `d` and `f` must point to valid [`CobDecimal`] and [`CobField`] instances.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_set_field` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_decimal_set_field(d: *mut CobDecimal, f: *const CobField) {
    if d.is_null() {
        return;
    }
    // SAFETY: Validated non-null `d`; `f` validated by `decode_cob_field_decimal`.
    unsafe {
        match decode_cob_field_decimal(f, "cob_decimal_set_field", "f") {
            Ok(val) => (*d).set_from_fixed_decimal(&val),
            Err(_) => (*d).set_nan(),
        }
    }
}

/// Stores decimal register `d` into `f` using `opt` (`COB_STORE_*`), returning `0` on success or `1` on overflow/NaN (`cob_decimal_get_field`).
///
/// # Arguments
///
/// * `d` - Source [`CobDecimal`] pointer.
/// * `f` - Destination [`CobField`] pointer.
/// * `opt` - Bitmask of `COB_STORE_ROUND`, `COB_STORE_KEEP_ON_OVERFLOW`, `COB_STORE_TRUNC_ON_OVERFLOW`.
///
/// # Safety
///
/// Non-null `d` and `f` must point to valid [`CobDecimal`] and writable [`CobField`] instances.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_get_field` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_decimal_get_field(
    d: *const CobDecimal,
    f: *mut CobField,
    opt: i32,
) -> i32 {
    if d.is_null() {
        return 1;
    }
    // SAFETY: Validated non-null `d`.
    let Ok(val) = (unsafe { (*d).to_fixed_decimal() }) else {
        return 1;
    };
    // SAFETY: Validated via `validate_cob_field_write`.
    let Ok((dst_bytes, dst_desc, dst_attr)) =
        (unsafe { validate_cob_field_write(f, "cob_decimal_get_field", "f") })
    else {
        return 1;
    };
    if !is_numeric_cob_type(dst_attr.field_type) {
        let formatted = val.to_string();
        return i32::from(
            execute_move(
                MoveSource::AlphanumericLiteral(formatted.as_bytes()),
                dst_bytes,
                &dst_desc,
            )
            .is_err(),
        );
    }
    let round_mode = if (opt & COB_STORE_ROUND) != 0 {
        RoundingMode::NearestAwayFromZero
    } else {
        RoundingMode::Truncation
    };
    let high_order = if (opt & COB_STORE_TRUNC_ON_OVERFLOW) != 0 {
        HighOrderPolicy::ModuloTruncate
    } else {
        HighOrderPolicy::FlagOverflow
    };
    let backup = dst_bytes.to_vec();
    let status = store_decimal_to_field(&val, dst_bytes, &dst_desc, round_mode, high_order)
        .unwrap_or(SizeErrorStatus::Overflowed);
    if status == SizeErrorStatus::Exact {
        0
    } else {
        if (opt & COB_STORE_KEEP_ON_OVERFLOW) != 0 {
            dst_bytes.copy_from_slice(&backup);
        } else {
            let _ = store_decimal_to_field(
                &val,
                dst_bytes,
                &dst_desc,
                round_mode,
                HighOrderPolicy::ModuloTruncate,
            );
        }
        1
    }
}

/// Assigns signed 64-bit integer `val` to decimal register `d` (`cob_decimal_set_llint`).
///
/// # Arguments
///
/// * `d` - Destination [`CobDecimal`] pointer.
/// * `val` - Signed 64-bit integer value.
///
/// # Safety
///
/// When non-null, `d` must point to writable storage for one [`CobDecimal`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_set_llint` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn cob_decimal_set_llint(d: *mut CobDecimal, val: i64) {
    if !d.is_null() {
        let dec = FixedDecimal::from_i64(val);
        // SAFETY: Validated non-null `d`.
        unsafe {
            (*d).set_from_fixed_decimal(&dec);
        }
    }
}

/// Assigns unsigned 64-bit integer `val` to decimal register `d` (`cob_decimal_set_ullint`).
///
/// # Arguments
///
/// * `d` - Destination [`CobDecimal`] pointer.
/// * `val` - Unsigned 64-bit integer value.
///
/// # Safety
///
/// When non-null, `d` must point to writable storage for one [`CobDecimal`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_set_ullint` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn cob_decimal_set_ullint(d: *mut CobDecimal, val: u64) {
    if !d.is_null() {
        let limbs = Uint256::from_u64(val).limbs();
        // SAFETY: Validated non-null `d`.
        unsafe {
            (*d).limbs = limbs;
            (*d).sign = i32::from(val != 0);
            (*d).scale = 0;
        }
    }
}

/// Binary decimal operator signature shared across [`apply_binary_decimal_op`].
type DecimalBinOpFn = fn(&FixedDecimal, &FixedDecimal) -> Result<(FixedDecimal, SizeErrorStatus)>;

/// Evaluates `lhs / rhs` at 38 fractional digits with truncation.
///
/// # Arguments
///
/// * `lhs` - Dividend decimal.
/// * `rhs` - Divisor decimal.
fn decimal_div_trunc(
    lhs: &FixedDecimal,
    rhs: &FixedDecimal,
) -> Result<(FixedDecimal, SizeErrorStatus)> {
    lhs.checked_div(
        rhs,
        FixedDecimal::DEFAULT_DIV_SCALE,
        RoundingMode::Truncation,
    )
}

/// Evaluates `lhs ** rhs` at 38 fractional digits with nearest-away-from-zero rounding.
///
/// # Arguments
///
/// * `lhs` - Base decimal.
/// * `rhs` - Exponent decimal.
fn decimal_pow_rounded(
    lhs: &FixedDecimal,
    rhs: &FixedDecimal,
) -> Result<(FixedDecimal, SizeErrorStatus)> {
    lhs.checked_pow(
        rhs,
        FixedDecimal::DEFAULT_DIV_SCALE,
        RoundingMode::NearestAwayFromZero,
    )
}

/// Evaluates `d1 = op(d1, d2)` across two [`CobDecimal`] pointers, marking `d1` NaN on domain error.
///
/// # Arguments
///
/// * `d1` - In-place accumulator [`CobDecimal`] pointer.
/// * `d2` - Right-hand [`CobDecimal`] pointer.
/// * `op` - Pure arithmetic function returning `(FixedDecimal, SizeErrorStatus)`.
///
/// # Safety
///
/// Non-null `d1` and `d2` must point to valid [`CobDecimal`] instances (`d1` and `d2` may alias).
#[expect(
    unsafe_code,
    reason = "Shared null-guarded binary `CobDecimal` update helper."
)]
unsafe fn apply_binary_decimal_op(d1: *mut CobDecimal, d2: *const CobDecimal, op: DecimalBinOpFn) {
    if d1.is_null() || d2.is_null() {
        return;
    }
    // SAFETY: Read both `*d1` and `*d2` by value before mutating `*d1` so aliasing `d1 == d2` is sound.
    let (lhs_raw, rhs_raw) = unsafe { (*d1, *d2) };
    let (Ok(lhs), Ok(rhs)) = (lhs_raw.to_fixed_decimal(), rhs_raw.to_fixed_decimal()) else {
        // SAFETY: Validated non-null `d1`.
        unsafe {
            (*d1).set_nan();
        }
        return;
    };
    // SAFETY: Validated non-null `d1`.
    unsafe {
        match op(&lhs, &rhs) {
            Ok((res, SizeErrorStatus::Exact)) => (*d1).set_from_fixed_decimal(&res),
            Ok((_, _)) | Err(_) => (*d1).set_nan(),
        }
    }
}

/// Computes `d1 = d1 + d2` (`cob_decimal_add`).
///
/// # Arguments
///
/// * `d1` - Accumulator [`CobDecimal`] pointer.
/// * `d2` - Addend [`CobDecimal`] pointer.
///
/// # Safety
///
/// Non-null `d1` and `d2` must point to valid [`CobDecimal`] instances.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_add` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_decimal_add(d1: *mut CobDecimal, d2: *const CobDecimal) {
    // SAFETY: Forwarded to null-guarded `apply_binary_decimal_op`.
    unsafe {
        apply_binary_decimal_op(d1, d2, FixedDecimal::checked_add);
    }
}

/// Computes `d1 = d1 - d2` (`cob_decimal_sub`).
///
/// # Arguments
///
/// * `d1` - Accumulator [`CobDecimal`] pointer.
/// * `d2` - Subtrahend [`CobDecimal`] pointer.
///
/// # Safety
///
/// Non-null `d1` and `d2` must point to valid [`CobDecimal`] instances.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_sub` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_decimal_sub(d1: *mut CobDecimal, d2: *const CobDecimal) {
    // SAFETY: Forwarded to null-guarded `apply_binary_decimal_op`.
    unsafe {
        apply_binary_decimal_op(d1, d2, FixedDecimal::checked_sub);
    }
}

/// Computes `d1 = d1 * d2` (`cob_decimal_mul`).
///
/// # Arguments
///
/// * `d1` - Accumulator [`CobDecimal`] pointer.
/// * `d2` - Multiplier [`CobDecimal`] pointer.
///
/// # Safety
///
/// Non-null `d1` and `d2` must point to valid [`CobDecimal`] instances.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_mul` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_decimal_mul(d1: *mut CobDecimal, d2: *const CobDecimal) {
    // SAFETY: Forwarded to null-guarded `apply_binary_decimal_op`.
    unsafe {
        apply_binary_decimal_op(d1, d2, FixedDecimal::checked_mul);
    }
}

/// Computes `d1 = d1 / d2` at 38 fractional digits (`cob_decimal_div`), marking `d1` NaN on zero divisor.
///
/// # Arguments
///
/// * `d1` - Dividend/result [`CobDecimal`] pointer.
/// * `d2` - Divisor [`CobDecimal`] pointer.
///
/// # Safety
///
/// Non-null `d1` and `d2` must point to valid [`CobDecimal`] instances.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_div` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_decimal_div(d1: *mut CobDecimal, d2: *const CobDecimal) {
    // SAFETY: Forwarded to null-guarded `apply_binary_decimal_op`.
    unsafe {
        apply_binary_decimal_op(d1, d2, decimal_div_trunc);
    }
}

/// Computes `d1 = d1 ** d2` at 38 fractional digits (`cob_decimal_pow`).
///
/// # Arguments
///
/// * `d1` - Base/result [`CobDecimal`] pointer.
/// * `d2` - Exponent [`CobDecimal`] pointer.
///
/// # Safety
///
/// Non-null `d1` and `d2` must point to valid [`CobDecimal`] instances.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_pow` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_decimal_pow(d1: *mut CobDecimal, d2: *const CobDecimal) {
    // SAFETY: Forwarded to null-guarded `apply_binary_decimal_op`.
    unsafe {
        apply_binary_decimal_op(d1, d2, decimal_pow_rounded);
    }
}

/// Compares two [`CobDecimal`] registers algebraically (`-1`, `0`, `1`; `0` on null/NaN) (`cob_decimal_cmp`).
///
/// # Arguments
///
/// * `d1` - Left-hand [`CobDecimal`] pointer.
/// * `d2` - Right-hand [`CobDecimal`] pointer.
///
/// # Safety
///
/// Non-null `d1` and `d2` must point to valid [`CobDecimal`] instances.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_decimal_cmp` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_decimal_cmp(d1: *const CobDecimal, d2: *const CobDecimal) -> i32 {
    if d1.is_null() || d2.is_null() {
        return 0;
    }
    // SAFETY: Validated non-null `d1` and `d2`.
    let (Ok(lhs), Ok(rhs)) = (unsafe { ((*d1).to_fixed_decimal(), (*d2).to_fixed_decimal()) })
    else {
        return 0;
    };
    ordering_to_c_int(lhs.cmp_numeric(&rhs))
}

/// Adds signed 32-bit integer `val` to numeric field `f` (`cob_add_int`), returning `0` or `1` on overflow.
///
/// # Arguments
///
/// * `f` - Target numeric [`CobField`] pointer.
/// * `val` - Signed 32-bit integer addend.
/// * `opt` - `COB_STORE_*` store options.
///
/// # Safety
///
/// Non-null `f` must point to a valid readable/writable [`CobField`] descriptor.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_add_int` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_add_int(f: *mut CobField, val: i32, opt: i32) -> i32 {
    let mut acc = CobDecimal::default();
    let mut delta = CobDecimal::default();
    // SAFETY: `acc` and `delta` are stack-allocated; `f` is null-guarded by `cob_decimal_set_field`/`cob_decimal_get_field`.
    unsafe {
        cob_decimal_set_field(&raw mut acc, f.cast_const());
        cob_decimal_set_llint(&raw mut delta, i64::from(val));
        cob_decimal_add(&raw mut acc, &raw const delta);
        cob_decimal_get_field(&raw const acc, f, opt)
    }
}

/// Subtracts signed 32-bit integer `val` from numeric field `f` (`cob_sub_int`), returning `0` or `1` on overflow.
///
/// # Arguments
///
/// * `f` - Target numeric [`CobField`] pointer.
/// * `val` - Signed 32-bit integer subtrahend.
/// * `opt` - `COB_STORE_*` store options.
///
/// # Safety
///
/// Non-null `f` must point to a valid readable/writable [`CobField`] descriptor.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_sub_int` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_sub_int(f: *mut CobField, val: i32, opt: i32) -> i32 {
    let mut acc = CobDecimal::default();
    let mut delta = CobDecimal::default();
    // SAFETY: `acc` and `delta` are stack-allocated; `f` is null-guarded by `cob_decimal_set_field`/`cob_decimal_get_field`.
    unsafe {
        cob_decimal_set_field(&raw mut acc, f.cast_const());
        cob_decimal_set_llint(&raw mut delta, i64::from(val));
        cob_decimal_sub(&raw mut acc, &raw const delta);
        cob_decimal_get_field(&raw const acc, f, opt)
    }
}
