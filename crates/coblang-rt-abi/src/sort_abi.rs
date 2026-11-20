//! Audited clean-room C-ABI entrypoints for Milestone M16 (`SORT`, `MERGE`, `RELEASE`, `RETURN`, and `SORT-RETURN`).

use std::sync::Mutex;

use coblang_common::{DigitCount, Result, ScaleFactor};
use coblang_decimal::{
    BinaryByteOrder, BinaryLayout, BinarySignMode, BinaryTruncMode, BlankZeroMode,
    OverpunchDialect, PackedKind, PackedLayout, SignPlacement, ZonedLayout,
};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, FloatBinaryWidth, FloatDecimalWidth,
    HirSortDirection, HirType, NumericSignSpec, Signedness, TruncationPolicy,
};
use coblang_pic::MoveFieldDesc;
use coblang_rt_core::desc_codec::{DecodedTableSortPlan, decode_table_sort_plan};
use coblang_sort::{
    AlphabetTable, DuplicatesOrder, KeyDirection, RecordLengthBounds, SortComparator, SortKeySpec,
    SortKeyType, SortMemoryLimit, SortReturnStatus, SortSession, SortSessionConfig,
    SortSessionState,
};

use crate::field_abi::read_raw_slice;

/// Bit flag on `coblang_rt_sort_begin` selecting `MERGE` instead of `SORT`.
pub const SORT_FLAG_IS_MERGE: i32 = 1 << 0;

/// Bit flag on `coblang_rt_sort_begin` enabling `WITH DUPLICATES IN ORDER`.
pub const SORT_FLAG_DUPLICATES_IN_ORDER: i32 = 1 << 1;

/// Return sentinel from `coblang_rt_sort_return` indicating normal `AT END` exhaustion.
pub const SORT_RETURN_AT_END: i64 = -1;

/// Return sentinel from `coblang_rt_sort_return` indicating `SORT-RETURN = 16` abort/failure.
pub const SORT_RETURN_ABORTED: i64 = -2;

/// Global stack of active `SORT` / `MERGE` sessions for the COBOL run unit.
static ACTIVE_SORT_SESSIONS: Mutex<Vec<SortSession>> = Mutex::new(Vec::new());

/// Executes `f` with exclusive access to [`ACTIVE_SORT_SESSIONS`].
///
/// # Arguments
///
/// * `f` - Closure mutating the active `SORT` / `MERGE` session stack.
fn with_sort_sessions_mut<R>(f: impl FnOnce(&mut Vec<SortSession>) -> R) -> R {
    let mut guard = ACTIVE_SORT_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut guard)
}

/// Aborts and drops all active `SORT` / `MERGE` sessions on `STOP RUN` or `cob_tidy`.
pub fn reset_all_sort_sessions() {
    with_sort_sessions_mut(|stack| {
        for sess in stack.iter_mut() {
            sess.abort();
        }
        stack.clear();
    });
}

/// Opens a new `SORT` or `MERGE` session and pushes it onto the active session stack.
///
/// # Arguments
///
/// * `plan_ptr` - Pointer to serialized [`DecodedTableSortPlan`] bytes.
/// * `plan_len` - Byte length of `plan_ptr`.
/// * `min_rec_len` - Minimum `SD` record byte length (`>= 1`).
/// * `max_rec_len` - Maximum `SD` record byte length (`>= min_rec_len`).
/// * `flags` - Bitmask of [`SORT_FLAG_IS_MERGE`] and [`SORT_FLAG_DUPLICATES_IN_ORDER`].
///
/// # Returns
///
/// `0` on success (`SORT-RETURN = 0`), or `16` on invalid plan / configuration failure.
///
/// # Safety
///
/// When `plan_len > 0` and `!plan_ptr.is_null()`, `plan_ptr` must point to `plan_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited C-ABI entrypoint for COBOL `SORT`/`MERGE` session setup."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_sort_begin(
    plan_ptr: *const u8,
    plan_len: usize,
    min_rec_len: usize,
    max_rec_len: usize,
    flags: i32,
) -> i32 {
    // SAFETY: Caller guarantees `plan_ptr[0..plan_len]` is readable when non-null.
    let Ok(plan_bytes) =
        (unsafe { read_raw_slice(plan_ptr, plan_len, "coblang_rt_sort_begin", "plan_ptr") })
    else {
        return 16;
    };
    let Ok(sess) = build_session_from_plan(plan_bytes, min_rec_len, max_rec_len, flags) else {
        return 16;
    };
    with_sort_sessions_mut(|stack| stack.push(sess));
    0
}

/// Starts a new pre-sorted `USING` input run on the active `MERGE` session.
///
/// # Returns
///
/// `0` on success, or `16` if no active `MERGE` session exists orspill flush fails.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_sort_begin_merge_stream() -> i32 {
    with_sort_sessions_mut(|stack| {
        let Some(sess) = stack.last_mut() else {
            return 16;
        };
        if sess.begin_merge_run().is_ok() {
            0
        } else {
            sess.abort();
            16
        }
    })
}

/// Releases one record (`RELEASE` or `USING` pump) into the active `SORT` or `MERGE` session.
///
/// # Arguments
///
/// * `rec_ptr` - Pointer to record payload bytes.
/// * `rec_len` - Record byte length.
/// * `sort_return_val` - Current COBOL `SORT-RETURN` special register value (`16` triggers early abort).
///
/// # Returns
///
/// `0` on success, or `16` if aborted via `SORT-RETURN` or record/spill error.
///
/// # Safety
///
/// When `rec_len > 0` and `!rec_ptr.is_null()`, `rec_ptr` must point to `rec_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited C-ABI entrypoint for COBOL `RELEASE` and `SORT`/`MERGE` `USING` ingestion."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_sort_release(
    rec_ptr: *const u8,
    rec_len: usize,
    sort_return_val: i64,
) -> i32 {
    // SAFETY: Caller guarantees `rec_ptr[0..rec_len]` is readable when non-null.
    let Ok(raw_rec) =
        (unsafe { read_raw_slice(rec_ptr, rec_len, "coblang_rt_sort_release", "rec_ptr") })
    else {
        return 16;
    };
    with_sort_sessions_mut(|stack| {
        let Some(sess) = stack.last_mut() else {
            return 16;
        };
        if sort_return_val != 0 {
            sess.set_sort_return(SortReturnStatus::Failed16);
            return 16;
        }
        let clamped_rec = clamp_or_pad_to_bounds(raw_rec, sess.config().record_bounds());
        let res = match sess.state() {
            SortSessionState::AcceptingInput => sess.release(&clamped_rec),
            SortSessionState::AcceptingMergeRuns => sess.push_merge_record(&clamped_rec),
            _ => {
                sess.abort();
                return 16;
            }
        };
        if res.is_ok() {
            0
        } else {
            sess.abort();
            16
        }
    })
}

/// Completes input ingestion on the active `SORT`/`MERGE` session and prepares output retrieval.
///
/// # Arguments
///
/// * `sort_return_val` - Current COBOL `SORT-RETURN` special register value.
///
/// # Returns
///
/// `0` when ready to yield sorted records, or `16` if aborted or spill-merge failed.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_sort_finish_input(sort_return_val: i64) -> i32 {
    with_sort_sessions_mut(|stack| {
        let Some(sess) = stack.last_mut() else {
            return 16;
        };
        if sort_return_val != 0 {
            sess.set_sort_return(SortReturnStatus::Failed16);
            return 16;
        }
        if sess.finish_input().is_ok() {
            0
        } else {
            sess.abort();
            16
        }
    })
}

/// Retrieves the next sorted/merged record into `rec_ptr[0..max_len]` (`RETURN` verb or `GIVING` pump).
///
/// # Arguments
///
/// * `rec_ptr` - Destination `SD` / `FD` record buffer pointer.
/// * `max_len` - Capacity of `rec_ptr` in bytes.
/// * `pad_byte` - Trailing pad byte (`0x20` ASCII space by default) when returned record is shorter than `max_len`.
/// * `sort_return_val` - Current COBOL `SORT-RETURN` special register value.
///
/// # Returns
///
/// Non-negative actual record byte length (`>= 0`) when a record is returned,
/// [`SORT_RETURN_AT_END`] (`-1`) at end-of-stream, or [`SORT_RETURN_ABORTED`] (`-2`) on abort/error.
///
/// # Safety
///
/// When `max_len > 0`, `rec_ptr` must be non-null and point to `max_len` writable bytes.
#[expect(
    unsafe_code,
    reason = "Audited C-ABI entrypoint writing sorted record bytes into caller buffer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_sort_return(
    rec_ptr: *mut u8,
    max_len: usize,
    pad_byte: i32,
    sort_return_val: i64,
) -> i64 {
    if max_len > 0 && rec_ptr.is_null() {
        return SORT_RETURN_ABORTED;
    }
    with_sort_sessions_mut(|stack| {
        let Some(sess) = stack.last_mut() else {
            return SORT_RETURN_ABORTED;
        };
        if sort_return_val != 0 {
            sess.set_sort_return(SortReturnStatus::Failed16);
            return SORT_RETURN_ABORTED;
        }
        match sess.return_next() {
            Ok(Some(rec)) => {
                if max_len > 0 {
                    // SAFETY: Verified `rec_ptr` is non-null for `max_len > 0`.
                    let dst = unsafe { core::slice::from_raw_parts_mut(rec_ptr, max_len) };
                    let fill = u8::try_from(pad_byte & 0xFF).unwrap_or(b' ');
                    let copy_n = rec.len().min(max_len);
                    for (d, &s) in dst.iter_mut().zip(rec.iter().take(copy_n)) {
                        *d = s;
                    }
                    for slot in dst.iter_mut().skip(copy_n) {
                        *slot = fill;
                    }
                }
                i64::try_from(rec.len()).unwrap_or(0)
            }
            Ok(None) => SORT_RETURN_AT_END,
            Err(_) => {
                sess.abort();
                SORT_RETURN_ABORTED
            }
        }
    })
}

/// Closes and pops the active `SORT` / `MERGE` session, returning the final `SORT-RETURN` (`0` or `16`).
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_sort_end() -> i32 {
    with_sort_sessions_mut(|stack| {
        let Some(mut sess) = stack.pop() else {
            return 16;
        };
        let code = sess.sort_return().as_code();
        sess.close();
        code
    })
}

/// Decodes `plan_bytes` and constructs a configured [`SortSession`].
///
/// # Arguments
///
/// * `plan_bytes` - Binary [`DecodedTableSortPlan`] slice.
/// * `min_rec_len` - Minimum `SD` record length.
/// * `max_rec_len` - Maximum `SD` record length.
/// * `flags` - `SORT_FLAG_*` bitmask.
fn build_session_from_plan(
    plan_bytes: &[u8],
    min_rec_len: usize,
    max_rec_len: usize,
    flags: i32,
) -> Result<SortSession> {
    let decoded: DecodedTableSortPlan = decode_table_sort_plan(plan_bytes)?;
    let eff_min = min_rec_len.max(1);
    let eff_max = max_rec_len.max(eff_min).max(decoded.stride.max(1) as usize);
    let bounds = RecordLengthBounds::try_new(eff_min, eff_max)?;

    let alphabet = match decoded.collating_ranks {
        Some(ranks) => AlphabetTable::from_ranks(ranks),
        None => AlphabetTable::native_ascii(),
    };
    let duplicates = if (flags & SORT_FLAG_DUPLICATES_IN_ORDER) != 0 {
        DuplicatesOrder::InOrder
    } else {
        DuplicatesOrder::Arbitrary
    };

    let mut key_specs = Vec::with_capacity(decoded.keys.len().max(1));
    if decoded.keys.is_empty() {
        key_specs.push(SortKeySpec::try_new(
            0,
            eff_min,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        )?);
    } else {
        for k in &decoded.keys {
            let dir = match k.direction {
                HirSortDirection::Ascending => KeyDirection::Ascending,
                HirSortDirection::Descending => KeyDirection::Descending,
            };
            let klen = k.byte_len as usize;
            let kty = map_desc_to_sort_key_type(&k.spec.desc, klen);
            key_specs.push(SortKeySpec::try_new(k.rel_offset as usize, klen, dir, kty)?);
        }
    }

    let comparator = SortComparator::try_new(key_specs, alphabet, duplicates)?;
    let mem_limit = SortMemoryLimit::from_env().unwrap_or(SortMemoryLimit::DEFAULT);
    let config = SortSessionConfig::new(bounds).with_memory_limit(mem_limit);

    Ok(if (flags & SORT_FLAG_IS_MERGE) != 0 {
        SortSession::new_merge(comparator, config)
    } else {
        SortSession::new_sort(comparator, config)
    })
}

/// Normalizes an incoming `USING` / `RELEASE` record slice so it fits within `SD` `bounds`.
///
/// In COBOL, when a `USING` file has shorter fixed/variable records than `SD` `min_bytes`,
/// records are space-padded to `min_bytes`; if longer than `max_bytes`, truncated to `max_bytes`.
///
/// # Arguments
///
/// * `raw` - Source record slice.
/// * `bounds` - Target `SD` [`RecordLengthBounds`].
fn clamp_or_pad_to_bounds(raw: &[u8], bounds: RecordLengthBounds) -> Vec<u8> {
    if raw.len() < bounds.min_bytes() {
        let mut padded = vec![b' '; bounds.min_bytes()];
        for (d, &s) in padded.iter_mut().zip(raw) {
            *d = s;
        }
        padded
    } else if raw.len() > bounds.max_bytes() {
        raw.iter().take(bounds.max_bytes()).copied().collect()
    } else {
        raw.to_vec()
    }
}

/// Maps a runtime [`MoveFieldDesc`] to the exact [`SortKeyType`] used by [`SortComparator`].
///
/// # Arguments
///
/// * `desc` - Key field [`MoveFieldDesc`].
/// * `byte_len` - Declared key byte width.
fn map_desc_to_sort_key_type(desc: &MoveFieldDesc, byte_len: usize) -> SortKeyType {
    let space_byte = if desc.overpunch == OverpunchDialect::EbcdicIbm {
        0x40
    } else {
        b' '
    };
    match &desc.ty {
        HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            blank_zero,
        } => map_zoned_sort_key(
            *digits,
            *scale,
            *sign,
            *blank_zero,
            desc.overpunch,
            byte_len,
            space_byte,
        ),
        HirType::PackedDecimal {
            digits,
            scale,
            sign,
            unsigned_comp6,
        } => {
            let layout = PackedLayout {
                digits: *digits,
                scale: *scale,
                kind: match (unsigned_comp6, sign) {
                    (Comp6Mode::UnsignedComp6, _) => PackedKind::Comp6Unsigned,
                    (Comp6Mode::StandardComp3, Signedness::Signed) => PackedKind::Comp3Signed,
                    (Comp6Mode::StandardComp3, Signedness::Unsigned) => PackedKind::Comp3Unsigned,
                },
            };
            if layout.byte_len() == byte_len {
                SortKeyType::Packed(layout)
            } else {
                SortKeyType::Alphanumeric { space_byte }
            }
        }
        HirType::BinaryInt {
            byte_width,
            digits,
            scale,
            sign,
            endian,
            trunc,
        } if usize::from(*byte_width) == byte_len => SortKeyType::Binary(BinaryLayout {
            byte_width: *byte_width,
            digits: *digits,
            scale: *scale,
            sign: match sign {
                Signedness::Signed => BinarySignMode::Signed,
                Signedness::Unsigned => BinarySignMode::Unsigned,
            },
            byte_order: match endian {
                BinaryEndianness::BigEndian => BinaryByteOrder::BigEndian,
                BinaryEndianness::Native => BinaryByteOrder::NativeEndian,
            },
            trunc: match trunc {
                TruncationPolicy::TruncateToPic => BinaryTruncMode::TruncateToPic,
                TruncationPolicy::FullBinaryRange => BinaryTruncMode::FullBinaryRange,
            },
        }),
        HirType::Index if (1..=16).contains(&byte_len) => SortKeyType::Binary(BinaryLayout {
            byte_width: u8::try_from(byte_len).unwrap_or(4),
            digits: DigitCount::ONE,
            scale: ScaleFactor::ZERO,
            sign: BinarySignMode::Signed,
            byte_order: BinaryByteOrder::NativeEndian,
            trunc: BinaryTruncMode::FullBinaryRange,
        }),
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits32,
        } if byte_len == 4 => SortKeyType::FloatShort,
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits64,
        } if byte_len == 8 => SortKeyType::FloatLong,
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits128,
        } if byte_len == 16 => SortKeyType::FloatExtended,
        HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal64,
        } if byte_len == 8 => SortKeyType::FloatDecimal16,
        HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal128,
        } if byte_len == 16 => SortKeyType::FloatDecimal34,
        HirType::National { .. } if byte_len.is_multiple_of(2) => SortKeyType::NationalUtf16Be,
        _ => SortKeyType::Alphanumeric { space_byte },
    }
}

/// Maps a zoned decimal HIR type to [`SortKeyType::Zoned`] (or `Alphanumeric` on width mismatch).
///
/// # Arguments
///
/// * `digits` - Declared digit count.
/// * `scale` - Declared scale factor.
/// * `sign` - Sign placement specification.
/// * `blank_zero` - `BLANK WHEN ZERO` policy.
/// * `dialect` - Overpunch dialect.
/// * `byte_len` - Declared field byte width.
/// * `space_byte` - Fallback alphanumeric padding byte.
fn map_zoned_sort_key(
    digits: DigitCount,
    scale: ScaleFactor,
    sign: NumericSignSpec,
    blank_zero: BlankZeroSpec,
    dialect: OverpunchDialect,
    byte_len: usize,
    space_byte: u8,
) -> SortKeyType {
    let layout = ZonedLayout {
        digits,
        scale,
        sign: match sign {
            NumericSignSpec::Unsigned => SignPlacement::Unsigned,
            NumericSignSpec::EmbeddedLeading => SignPlacement::EmbeddedLeading,
            NumericSignSpec::EmbeddedTrailing => SignPlacement::EmbeddedTrailing,
            NumericSignSpec::SeparateLeading => SignPlacement::SeparateLeading,
            NumericSignSpec::SeparateTrailing => SignPlacement::SeparateTrailing,
        },
        dialect,
        blank_zero: match blank_zero {
            BlankZeroSpec::RetainZero => BlankZeroMode::RetainZero,
            BlankZeroSpec::BlankWhenZero => BlankZeroMode::BlankWhenZero,
        },
    };
    if layout.byte_len() == byte_len {
        SortKeyType::Zoned(layout)
    } else {
        SortKeyType::Alphanumeric { space_byte }
    }
}
