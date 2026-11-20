//! Milestone M17 MLIR lowering for `NATIONAL` (`UTF-16`), `UTF-8` (`PIC U`), and `-finternal-ebcdic`
//! storage initialization, console transcoding, and character-indexed reference modification.

#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing)]

use coblang_common::{FeatureTag, Result, SourceSpan};
use coblang_decimal::{Codepage, UnmappedCharPolicy, transcode_bytes};
use coblang_diag::{SourceMap, SymbolInterner};
use coblang_hir::{
    HirConstValue, HirFigurative, HirInternalCharset, HirProgramFormatting, HirType,
};
use coblang_pic::MoveFieldDesc;
use coblang_rt_core::field_ops::{RuntimeMoveSourceKind, swap_utf16be_to_le};
use melior::dialect::arith;
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::llvm;
use melior::ir::attribute::IntegerAttribute;
use melior::ir::block::{BlockLike, BlockRef};
use melior::ir::r#type::IntegerType;
use melior::ir::{Block, Location, Operation, Type, Value};

use crate::lower_from_hir::{
    ModuleLowerState, ProgramLowerCtx, append_val, emit_const_slice, emit_i32, emit_i64, unimpl,
};
use crate::ops::{CharByteWidth, llvm_ptr_type};
use crate::storage_init::{format_hir_decimal_ascii, parse_scientific_or_decimal};

/// Returns the [`CharByteWidth`] governing `CobolRefModOp` character slicing on `ty`.
///
/// # Arguments
///
/// * `ty` - Base [`HirType`] of the reference-modified item.
#[must_use]
pub const fn char_width_for_hir_type(ty: &HirType) -> CharByteWidth {
    match ty {
        HirType::National { .. } | HirType::NationalEdited { .. } => CharByteWidth::Utf16TwoByte,
        HirType::Utf8 { .. } => CharByteWidth::Utf8Variable,
        _ => CharByteWidth::SingleByte,
    }
}

/// Returns `(space_byte, zero_byte, quote_byte)` for `fmt` (`0x40`/`0xF0`/`0x7F` under EBCDIC, `0x20`/`0x30`/`0x22` under ASCII).
///
/// # Arguments
///
/// * `fmt` - Program formatting conventions.
#[must_use]
pub const fn single_byte_figuratives_for_fmt(fmt: HirProgramFormatting) -> (u8, u8, u8) {
    match fmt.internal_charset {
        HirInternalCharset::Ebcdic => (0x40, 0xF0, 0x7F),
        HirInternalCharset::Ascii => (b' ', b'0', b'"'),
    }
}

/// Resolves a 1-byte [`HirFigurative`] value under `fmt` (using ASCII bytes when `unicode_mode` is `true`).
///
/// # Arguments
///
/// * `fig` - Figurative constant.
/// * `fmt` - Program formatting conventions.
/// * `unicode_mode` - `true` when the enclosing `STRING`/`INSPECT` subject is `NATIONAL` or `UTF-8`.
#[must_use]
pub const fn figurative_single_byte_m17(
    fig: &HirFigurative,
    fmt: HirProgramFormatting,
    unicode_mode: bool,
) -> Option<u8> {
    let (sp, zr, qt) = if unicode_mode {
        (b' ', b'0', b'"')
    } else {
        single_byte_figuratives_for_fmt(fmt)
    };
    match fig {
        HirFigurative::Zero => Some(zr),
        HirFigurative::Space => Some(sp),
        HirFigurative::HighValue => Some(0xFF),
        HirFigurative::LowValue | HirFigurative::Null => Some(0x00),
        HirFigurative::Quote => Some(qt),
        HirFigurative::All(pat) => match pat.as_slice() {
            [single] => Some(*single),
            _ => None,
        },
    }
}

/// Returns `true` when `ty` requires `DispPushField` formatting/transcoding rather than raw byte push on `DISPLAY`.
///
/// # Arguments
///
/// * `ty` - Displayed [`HirType`].
/// * `fmt` - Program formatting conventions.
#[must_use]
pub const fn requires_formatted_display_push(ty: &HirType, fmt: HirProgramFormatting) -> bool {
    if matches!(fmt.internal_charset, HirInternalCharset::Ebcdic) {
        return true;
    }
    !matches!(
        ty,
        HirType::Alphanumeric { .. }
            | HirType::Alphabetic { .. }
            | HirType::AlphanumericEdited { .. }
            | HirType::Utf8 { .. }
            | HirType::Group { .. }
    )
}

/// Writes default Working-Storage bytes into `slice` for `National` (`[0x20, 0x00]` pairs) and `Utf8` (`0x20`).
///
/// Returns `true` if `desc.ty` was handled as an M17 `National`/`NationalEdited`/`Utf8` type.
///
/// # Arguments
///
/// * `slice` - Mutable target element bytes.
/// * `desc` - Target [`MoveFieldDesc`].
pub fn encode_default_m17_category_bytes(slice: &mut [u8], desc: &MoveFieldDesc) -> bool {
    match desc.ty {
        HirType::National { .. } | HirType::NationalEdited { .. } => {
            slice.fill(0);
            for pair in slice.as_chunks_mut::<2>().0 {
                *pair = [b' ', 0x00];
            }
            true
        }
        HirType::Utf8 { .. } => {
            slice.fill(b' ');
            true
        }
        _ => false,
    }
}

/// Formats a compile-time [`HirConstValue`] into UTF-8/ASCII console `DISPLAY` bytes
/// (transcoding `NATIONAL` UTF-16BE and `-finternal-ebcdic` literals at compile time).
///
/// # Arguments
///
/// * `value` - Constant HIR value.
/// * `fmt` - Program formatting conventions.
/// * `interner` - Symbol interner.
#[must_use]
pub fn const_to_display_bytes_m17(
    value: &HirConstValue,
    fmt: HirProgramFormatting,
    interner: &SymbolInterner,
) -> Vec<u8> {
    match value {
        HirConstValue::Bytes(b) => {
            if fmt.internal_charset == HirInternalCharset::Ebcdic {
                transcode_bytes(
                    b,
                    Codepage::Cp1140,
                    Codepage::Iso8859_1,
                    UnmappedCharPolicy::Substitute('?'),
                )
                .unwrap_or_else(|_| b.clone())
            } else {
                b.clone()
            }
        }
        HirConstValue::NationalBytes(be) => {
            let le = swap_utf16be_to_le(be);
            transcode_bytes(
                &le,
                Codepage::Utf16Le,
                Codepage::Utf8,
                UnmappedCharPolicy::Substitute('\u{FFFD}'),
            )
            .unwrap_or_default()
        }
        HirConstValue::Utf8Bytes(u) => u.clone(),
        HirConstValue::Figurative(fig) => match fig {
            HirFigurative::Zero => vec![b'0'],
            HirFigurative::Space => vec![b' '],
            HirFigurative::HighValue => vec![0xFF],
            HirFigurative::LowValue | HirFigurative::Null => vec![0x00],
            HirFigurative::Quote => vec![b'"'],
            HirFigurative::All(p) => {
                if fmt.internal_charset == HirInternalCharset::Ebcdic {
                    transcode_bytes(
                        p,
                        Codepage::Cp1140,
                        Codepage::Iso8859_1,
                        UnmappedCharPolicy::Substitute('?'),
                    )
                    .unwrap_or_else(|_| p.clone())
                } else {
                    p.clone()
                }
            }
        },
        HirConstValue::Decimal { text, scale, sign } => {
            let raw = interner.resolve_original(*text).unwrap_or("0");
            format_hir_decimal_ascii(raw, *scale, *sign).into_bytes()
        }
        HirConstValue::Float(sym) => interner
            .resolve_original(*sym)
            .unwrap_or("0")
            .as_bytes()
            .to_vec(),
    }
}

/// Converts a compile-time [`HirConstValue`] into an ASCII decimal string for `DecPushAscii`.
///
/// # Arguments
///
/// * `value` - Constant HIR value.
/// * `fmt` - Program formatting conventions.
/// * `span` - Expression source span.
/// * `source_map` - Source map for diagnostics.
/// * `interner` - Symbol interner.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if `value` is a non-ZERO figurative or malformed float.
pub fn const_to_decimal_ascii_m17(
    value: &HirConstValue,
    fmt: HirProgramFormatting,
    span: SourceSpan,
    source_map: &SourceMap,
    interner: &SymbolInterner,
) -> Result<String> {
    match value {
        HirConstValue::Figurative(HirFigurative::Zero) => Ok(String::from("0")),
        HirConstValue::Decimal { text, scale, sign } => {
            let raw = interner.resolve_original(*text).unwrap_or("0");
            Ok(format_hir_decimal_ascii(raw, *scale, *sign))
        }
        HirConstValue::Float(sym) => {
            let raw = interner.resolve_original(*sym).unwrap_or("0");
            Ok(parse_scientific_or_decimal(raw)?.to_string())
        }
        HirConstValue::Bytes(b) => {
            if fmt.internal_charset == HirInternalCharset::Ebcdic {
                let asc = transcode_bytes(
                    b,
                    Codepage::Cp1140,
                    Codepage::Iso8859_1,
                    UnmappedCharPolicy::Substitute('?'),
                )
                .unwrap_or_else(|_| b.clone());
                Ok(String::from_utf8_lossy(&asc).trim().to_owned())
            } else {
                Ok(String::from_utf8_lossy(b).trim().to_owned())
            }
        }
        HirConstValue::NationalBytes(be) => {
            let le = swap_utf16be_to_le(be);
            let utf8 = transcode_bytes(
                &le,
                Codepage::Utf16Le,
                Codepage::Utf8,
                UnmappedCharPolicy::Substitute('?'),
            )
            .unwrap_or_default();
            Ok(String::from_utf8_lossy(&utf8).trim().to_owned())
        }
        HirConstValue::Utf8Bytes(u) => Ok(String::from_utf8_lossy(u).trim().to_owned()),
        HirConstValue::Figurative(_) => Err(unimpl(
            FeatureTag::FeatureArith,
            "non-ZERO figurative in arithmetic expression",
            source_map,
            span,
        )),
    }
}

/// Lowers `HirConstValue::NationalBytes` or `Utf8Bytes` into `(kind_i32, ptr, len, empty_ptr, zero_i64)`
/// for `coblang_rt_move_desc` / `coblang_rt_cmp_alpha_ex`.
///
/// # Arguments
///
/// * `bytes` - Literal payload bytes.
/// * `kind` - `NationalLiteral` or `Utf8Literal`.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if constant slice emission fails.
pub(crate) fn emit_m17_const_move_args<'ctx, 'b, 'e: 'b>(
    bytes: &[u8],
    kind: RuntimeMoveSourceKind,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
)> {
    let (empty_ptr, zero_i64) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
    let (p, l) = emit_const_slice(bytes, block, prog_ctx, state, loc)?;
    let k = emit_i32(kind.to_abi_code(), block, prog_ctx, state, loc);
    Ok((k, p, l, empty_ptr, zero_i64))
}

/// Computes the character-unit capacity SSA `i64` for `CheckRefMod` (`byte_len / 2` for `NATIONAL`).
///
/// # Arguments
///
/// * `base_ty` - Base item [`HirType`].
/// * `base_byte_len` - Base item byte length `i64` SSA value.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn compute_refmod_max_units<'ctx, 'b, 'e: 'b>(
    base_ty: &HirType,
    base_byte_len: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    if matches!(
        base_ty,
        HirType::National { .. } | HirType::NationalEdited { .. }
    ) {
        let two = emit_i64(2, block, prog_ctx, state, loc);
        append_val(
            block,
            arith::divui(base_byte_len, two, loc),
            prog_ctx.fallback_val,
        )
    } else {
        base_byte_len
    }
}

/// Appends `op` to `block` and returns result `0` (or `fallback` if resultless).
///
/// # Arguments
///
/// * `block` - Target [`BlockRef`].
/// * `op` - Constructed [`Operation`].
/// * `fallback` - Dominated fallback SSA value.
fn append_block_res0<'ctx, 'b>(
    block: BlockRef<'ctx, 'b>,
    op: Operation<'ctx>,
    fallback: Value<'ctx, 'b>,
) -> Value<'ctx, 'b> {
    let op_ref = block.append_operation(op);
    op_ref.result(0).map_or(fallback, Into::into)
}

/// Lowers `CharByteWidth::Utf16TwoByte` (`"2"`) reference modification into `(sliced_ptr, byte_len)`.
///
/// # Arguments
///
/// * `raw` - Raw Melior [`melior::Context`].
/// * `block` - Target LLVM block.
/// * `inputs` - `(view_ptr, base_len, one_based_char, opt_char_len)`.
/// * `ctx` - Owning [`crate::context::CobolMlirContext`].
/// * `fallback` - Fallback SSA value.
/// * `loc` - Source location.
pub(crate) fn lower_utf16_refmod_llvm<'ctx, 'b>(
    raw: &'ctx melior::Context,
    block: BlockRef<'ctx, 'b>,
    inputs: (
        Value<'ctx, 'b>,
        Value<'ctx, 'b>,
        Value<'ctx, 'b>,
        Option<Value<'ctx, 'b>>,
    ),
    ctx: &'ctx crate::context::CobolMlirContext,
    fallback: Value<'ctx, 'b>,
    loc: Location<'ctx>,
) -> (Value<'ctx, 'b>, Value<'ctx, 'b>) {
    let (view_ptr, base_len, one_based, opt_char_len) = inputs;
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let ptr_ty: Type<'ctx> = llvm_ptr_type(ctx);

    let z64 = append_block_res0(
        block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, 0).into(), loc),
        fallback,
    );
    let one_64 = append_block_res0(
        block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, 1).into(), loc),
        fallback,
    );
    let two_64 = append_block_res0(
        block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, 2).into(), loc),
        fallback,
    );

    let char_off = append_block_res0(block, arith::subi(one_based, one_64, loc), z64);
    let raw_byte_off = append_block_res0(block, arith::muli(char_off, two_64, loc), z64);
    let is_neg = append_block_res0(
        block,
        arith::cmpi(raw, CmpiPredicate::Slt, raw_byte_off, z64, loc),
        z64,
    );
    let non_neg_off = append_block_res0(block, arith::select(is_neg, z64, raw_byte_off, loc), z64);
    let over_base = append_block_res0(
        block,
        arith::cmpi(raw, CmpiPredicate::Sgt, non_neg_off, base_len, loc),
        z64,
    );
    let clamped_off = append_block_res0(
        block,
        arith::select(over_base, base_len, non_neg_off, loc),
        z64,
    );

    let gep = llvm::get_element_ptr_dynamic(raw, view_ptr, &[clamped_off], i8_ty, ptr_ty, loc);
    let sliced_ptr = append_block_res0(block, gep, view_ptr);

    let rem = append_block_res0(block, arith::subi(base_len, clamped_off, loc), z64);
    let rem_neg = append_block_res0(
        block,
        arith::cmpi(raw, CmpiPredicate::Slt, rem, z64, loc),
        z64,
    );
    let max_rem = append_block_res0(block, arith::select(rem_neg, z64, rem, loc), z64);

    let final_len = if let Some(req_chars) = opt_char_len {
        let req_bytes = append_block_res0(block, arith::muli(req_chars, two_64, loc), z64);
        let len_neg = append_block_res0(
            block,
            arith::cmpi(raw, CmpiPredicate::Slt, req_bytes, z64, loc),
            z64,
        );
        let non_neg_len =
            append_block_res0(block, arith::select(len_neg, z64, req_bytes, loc), z64);
        let len_over = append_block_res0(
            block,
            arith::cmpi(raw, CmpiPredicate::Sgt, non_neg_len, max_rem, loc),
            z64,
        );
        append_block_res0(
            block,
            arith::select(len_over, max_rem, non_neg_len, loc),
            z64,
        )
    } else {
        max_rem
    };
    (sliced_ptr, final_len)
}
