//! Semantic validation and literal/alphabet/symbolic-character encoding for Milestone M17
//! (`NATIONAL` UTF-16, `UTF-8`, and `-finternal-ebcdic` / `-febcdic-table=`).

#![deny(clippy::indexing_slicing)]

use coblang_common::{Dialect, ScopeId, SourceSpan, SymbolId};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, DisplayFormatMode, DisplaySignMode,
    InternalCharsetMode,
};
use coblang_hir::{
    DataCategory, HirAlphabetTable, HirConstValue, HirDecimalSign, HirDisplayStyle, HirFigurative,
    HirInternalCharset, HirProgramFormatting, HirSignConvention, HirType,
};
use coblang_syntax::StringPrefix;
use coblang_syntax::ast::{AlphabetKind, DecimalPointKind, FigurativeValue, ValueAtom};

use crate::picture::PictureContext;
use crate::symbols::{ResolvedSymbol, SymbolTable};

/// Derives [`HirProgramFormatting`] (including [`HirInternalCharset`] and [`HirSignConvention::EbcdicNative`])
/// from `SPECIAL-NAMES` `PictureContext` and active [`DialectConfig`].
///
/// # Arguments
///
/// * `pic_ctx` - Active `SPECIAL-NAMES` picture context.
/// * `dialect` - Active dialect configuration.
#[must_use]
pub fn build_m17_program_formatting(
    pic_ctx: PictureContext,
    dialect: &DialectConfig,
) -> HirProgramFormatting {
    let decimal_char = match pic_ctx.decimal_point {
        DecimalPointKind::Period => b'.',
        DecimalPointKind::Comma => b',',
    };
    let (internal_charset, sign_convention) = match dialect.internal_charset {
        InternalCharsetMode::Ebcdic => {
            (HirInternalCharset::Ebcdic, HirSignConvention::EbcdicNative)
        }
        InternalCharsetMode::Ascii => {
            let sign = if dialect.base_dialect == Dialect::Ibm {
                HirSignConvention::IbmMainframe
            } else {
                match dialect.display_sign {
                    DisplaySignMode::Ascii => HirSignConvention::AsciiGnu,
                    DisplaySignMode::Ebcdic => HirSignConvention::EbcdicOverpunch,
                }
            };
            (HirInternalCharset::Ascii, sign)
        }
    };
    let display_style = match dialect.pretty_display {
        DisplayFormatMode::IsoStandard => HirDisplayStyle::IsoPicWidth,
        DisplayFormatMode::GnuPretty => HirDisplayStyle::GnuPrettyWidth,
    };
    HirProgramFormatting {
        decimal_char,
        currency_symbol: pic_ctx.currency_symbol,
        sign_convention,
        display_style,
        default_byte: dialect.default_byte,
        internal_charset,
    }
}

/// Encodes a parsed string literal [`ValueAtom::String`] into [`HirConstValue`] honoring `prefix`
/// (`N`/`NX` UTF-16BE, `U`/`UX` UTF-8, `X`/`H`/`B`/`BX` raw bytes, and `-finternal-ebcdic` transcoding).
///
/// # Arguments
///
/// * `bytes` - Lexer-decoded literal payload bytes.
/// * `prefix` - String literal prefix (`Plain`, `NullTermZ`, `NationalN`, `Utf8U`, `HexX`, etc.).
/// * `dialect` - Active dialect configuration.
#[must_use]
pub fn encode_string_literal_m17(
    bytes: &[u8],
    prefix: StringPrefix,
    dialect: &DialectConfig,
) -> HirConstValue {
    match prefix {
        StringPrefix::NationalN | StringPrefix::NationalHexNX => {
            HirConstValue::NationalBytes(bytes.to_vec())
        }
        StringPrefix::Utf8U | StringPrefix::Utf8HexUX => HirConstValue::Utf8Bytes(bytes.to_vec()),
        StringPrefix::NullTermZ => {
            if dialect.internal_charset == InternalCharsetMode::Ebcdic {
                let body_len = bytes.len().saturating_sub(1);
                let body = bytes.get(..body_len).unwrap_or(&[]);
                let mut ebc = dialect.ebcdic_tables.transcode_ascii_to_ebcdic(body);
                ebc.push(0x00);
                HirConstValue::Bytes(ebc)
            } else {
                HirConstValue::Bytes(bytes.to_vec())
            }
        }
        StringPrefix::Plain => {
            if dialect.internal_charset == InternalCharsetMode::Ebcdic {
                HirConstValue::Bytes(dialect.ebcdic_tables.transcode_ascii_to_ebcdic(bytes))
            } else {
                HirConstValue::Bytes(bytes.to_vec())
            }
        }
        StringPrefix::HexX | StringPrefix::HexH | StringPrefix::BitB | StringPrefix::BitHexBX => {
            HirConstValue::Bytes(bytes.to_vec())
        }
    }
}

/// Converts a [`ValueAtom`] into a [`HirConstValue`] under `dialect` (applying M17 `NATIONAL`,
/// `UTF-8`, and `-finternal-ebcdic` literal/figurative encoding).
///
/// # Arguments
///
/// * `atom` - AST value atom.
/// * `scope` - Lookup scope for `78`/symbolic constant identifiers.
/// * `symtab` - Symbol table.
/// * `dialect` - Active dialect configuration.
#[must_use]
pub fn atom_to_hir_const_m17(
    atom: &ValueAtom,
    scope: ScopeId,
    symtab: &SymbolTable,
    dialect: &DialectConfig,
) -> Option<HirConstValue> {
    match atom {
        ValueAtom::String { bytes, prefix } => {
            Some(encode_string_literal_m17(bytes, *prefix, dialect))
        }
        ValueAtom::Integer(sym) => Some(HirConstValue::Decimal {
            text: *sym,
            scale: coblang_common::ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        }),
        ValueAtom::Decimal {
            digits,
            scale,
            sign,
        } => {
            let hsign = match sign {
                coblang_syntax::LiteralSign::Unsigned => HirDecimalSign::UnsignedOrEmbedded,
                coblang_syntax::LiteralSign::Positive => HirDecimalSign::Positive,
                coblang_syntax::LiteralSign::Negative => HirDecimalSign::Negative,
            };
            Some(HirConstValue::Decimal {
                text: *digits,
                scale: *scale,
                sign: hsign,
            })
        }
        ValueAtom::Float(sym) => Some(HirConstValue::Float(*sym)),
        ValueAtom::Figurative(fig) => {
            let hf = match fig {
                FigurativeValue::Zero => HirFigurative::Zero,
                FigurativeValue::Space => HirFigurative::Space,
                FigurativeValue::HighValue => HirFigurative::HighValue,
                FigurativeValue::LowValue => HirFigurative::LowValue,
                FigurativeValue::Quote => HirFigurative::Quote,
                FigurativeValue::Null => HirFigurative::Null,
                FigurativeValue::All(pat) => {
                    if dialect.internal_charset == InternalCharsetMode::Ebcdic {
                        HirFigurative::All(dialect.ebcdic_tables.transcode_ascii_to_ebcdic(pat))
                    } else {
                        HirFigurative::All(pat.clone())
                    }
                }
            };
            Some(HirConstValue::Figurative(hf))
        }
        ValueAtom::Identifier(sym) => symtab
            .scope(scope)
            .and_then(|s| s.by_name.get(sym))
            .and_then(|list| list.first())
            .and_then(|cand| match &cand.target {
                ResolvedSymbol::Constant(c) => Some(c.clone()),
                _ => None,
            }),
    }
}

/// Returns the [`DataCategory`] of a resolved [`HirConstValue`].
///
/// # Arguments
///
/// * `val` - Constant value.
#[must_use]
pub const fn const_value_category(val: &HirConstValue) -> DataCategory {
    match val {
        HirConstValue::Decimal { .. } | HirConstValue::Float(_) => DataCategory::Numeric,
        HirConstValue::NationalBytes(_) => DataCategory::National,
        HirConstValue::Utf8Bytes(_) => DataCategory::Utf8,
        HirConstValue::Bytes(_) | HirConstValue::Figurative(_) => DataCategory::Alphanumeric,
    }
}

/// Compiles a built-in (`NATIVE`, `STANDARD-1`, `STANDARD-2`, `EBCDIC`) `ALPHABET` rank table
/// relative to `dialect.internal_charset` and `dialect.ebcdic_tables`.
///
/// Returns `None` for [`AlphabetKind::Custom`].
///
/// # Arguments
///
/// * `kind` - Parsed [`AlphabetKind`].
/// * `dialect` - Active dialect configuration.
#[must_use]
pub fn compile_builtin_alphabet_ranks(
    kind: &AlphabetKind,
    dialect: &DialectConfig,
) -> Option<Vec<u8>> {
    match kind {
        AlphabetKind::Native => Some((0_u8..=255_u8).collect()),
        AlphabetKind::Standard1 | AlphabetKind::Standard2 => match dialect.internal_charset {
            InternalCharsetMode::Ascii => Some((0_u8..=255_u8).collect()),
            InternalCharsetMode::Ebcdic => Some(dialect.ebcdic_tables.ebcdic_to_ascii.to_vec()),
        },
        AlphabetKind::Ebcdic => match dialect.internal_charset {
            InternalCharsetMode::Ascii => Some(dialect.ebcdic_tables.ascii_to_ebcdic.to_vec()),
            InternalCharsetMode::Ebcdic => Some((0_u8..=255_u8).collect()),
        },
        AlphabetKind::Custom(_) => None,
    }
}

/// Resolves a 1-based `SYMBOLIC CHARACTERS` ordinal (`1..=256`) optionally qualified by `IN alphabet`
/// into its single-byte native storage representation.
///
/// # Arguments
///
/// * `ordinal_one_based` - 1-based ordinal value from the `SYMBOLIC CHARACTERS` clause.
/// * `in_alphabet` - Optional `IN <alphabet-name>` symbol.
/// * `alphabets` - Compiled `SPECIAL-NAMES` alphabet tables.
#[must_use]
pub fn resolve_symbolic_char_byte(
    ordinal_one_based: u16,
    in_alphabet: Option<SymbolId>,
    alphabets: &[HirAlphabetTable],
) -> u8 {
    let clamped = ordinal_one_based.clamp(1, 256);
    let target_pos = u8::try_from(clamped - 1).unwrap_or(0);
    if let Some(alpha_sym) = in_alphabet
        && let Some(alpha) = alphabets.iter().find(|a| a.name == alpha_sym)
        && let Some(native_idx) = alpha.ranks.iter().position(|&r| r == target_pos)
    {
        return u8::try_from(native_idx).unwrap_or(target_pos);
    }
    target_pos
}

/// Validates ISO COBOL / `GnuCOBOL` `MOVE` category rules involving `National`, `NationalEdited`, and `Utf8`.
///
/// Returns `true` when an incompatible `MOVE` error was emitted.
///
/// # Arguments
///
/// * `src_cat` - Source [`DataCategory`].
/// * `dst_cat` - Destination [`DataCategory`].
/// * `span` - `MOVE` statement span.
/// * `sink` - Diagnostic sink.
pub fn validate_m17_move_categories(
    src_cat: DataCategory,
    dst_cat: DataCategory,
    span: SourceSpan,
    sink: &mut DiagnosticSink,
) -> bool {
    let illegal = matches!(
        (src_cat, dst_cat),
        (
            DataCategory::National,
            DataCategory::Alphabetic
                | DataCategory::Alphanumeric
                | DataCategory::AlphanumericEdited
        ) | (
            DataCategory::NationalEdited,
            DataCategory::Alphabetic
                | DataCategory::Alphanumeric
                | DataCategory::AlphanumericEdited
                | DataCategory::Numeric
                | DataCategory::NumericEdited
        )
    );
    if illegal {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!(
                "invalid MOVE statement: incompatible `MOVE` from `{src_cat:?}` to `{dst_cat:?}`"
            ),
        ));
    }
    illegal
}

/// Returns the static maximum character/unit position count for reference-modification bounds checking.
///
/// # Arguments
///
/// * `ty` - Base item [`HirType`].
/// * `byte_length` - Base item static byte length.
#[must_use]
pub const fn refmod_max_positions(ty: &HirType, byte_length: u32) -> u32 {
    match ty {
        HirType::National { char_len, .. } | HirType::NationalEdited { char_len, .. } => *char_len,
        _ => byte_length,
    }
}
