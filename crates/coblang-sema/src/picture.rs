//! Clean-room COBOL `PICTURE` parser, validator, and `PicEditOp` bytecode compiler.

use coblang_common::{DigitCount, ScaleFactor, SourceSpan};
use coblang_diag::{
    BinaryByteOrder, BinarySizePolicy, Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig,
    TruncationMode,
};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, FloatBinaryWidth, FloatDecimalWidth, HirType,
    JustifiedSpec, NumericSignSpec, PicEditOp, Signedness, TruncationPolicy,
};
use coblang_syntax::ast::{
    BlankWhenZeroMode, DecimalPointKind, JustificationMode, SignClause, SignPlacement,
    SignSeparation, UsageKind,
};

/// Environment parameters governing `PICTURE` interpretation in the current unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureContext {
    /// Active decimal separator (`Period` vs `Comma`).
    pub decimal_point: DecimalPointKind,
    /// Active primary currency picture symbol (`'$'` by default).
    pub currency_symbol: char,
}

impl Default for PictureContext {
    /// Returns standard `'.'` decimal-point and `'$'` currency symbol context.
    fn default() -> Self {
        Self {
            decimal_point: DecimalPointKind::Period,
            currency_symbol: '$',
        }
    }
}

/// Bundled Data Division item clauses paired with `PICTURE` during elementary type compilation.
#[derive(Debug, Clone, Copy)]
pub struct ElementaryClauses<'a> {
    /// Optional raw `PICTURE` character-string and its span.
    pub raw_pic: Option<(&'a str, SourceSpan)>,
    /// Span of the enclosing data entry.
    pub entry_span: SourceSpan,
    /// Active `DECIMAL-POINT` and `CURRENCY` context.
    pub pic_ctx: PictureContext,
    /// Optional explicit or group-inherited `USAGE`.
    pub usage: Option<&'a UsageKind>,
    /// Optional explicit `SIGN` clause.
    pub sign_clause: Option<SignClause>,
    /// `JUSTIFIED` mode.
    pub justified: JustificationMode,
    /// `BLANK WHEN ZERO` mode.
    pub blank_zero: BlankWhenZeroMode,
}

/// Intermediate classification of an expanded `PICTURE` string prior to `USAGE` combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPicture {
    /// Compiled editing/slot operations.
    pub ops: Vec<PicEditOp>,
    /// Total display character/byte positions.
    pub display_len: u32,
    /// Total numeric digit positions (`9`, `Z`, `*`, floating sign/currency digits).
    pub digit_positions: u32,
    /// Effective decimal scale (`-38..=38`).
    pub scale: i16,
    /// Whether `S` or editing signs (`+`, `-`, `CR`, `DB`) are present.
    pub signed: Signedness,
    /// Dominant character class.
    pub kind: PictureFlavor,
}

/// Syntactic category of a `PICTURE` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureFlavor {
    /// Pure `A(...)`.
    Alphabetic,
    /// Pure `X(...)` or mixed `A`/`X`/`9` without editing symbols.
    Alphanumeric,
    /// `A`/`X`/`9` combined with `B`, `0`, or `/`.
    AlphanumericEdited,
    /// Pure `[S][P...]9(...)[V9(...)][P...]`.
    Numeric,
    /// Numeric with editing symbols (`Z`, `*`, `.`, `,`, `+`, `-`, `CR`, `DB`, `$`, `B`, `0`, `/`).
    NumericEdited,
    /// Pure `N(...)`.
    National,
    /// `N` with `B`, `0`, `/`.
    NationalEdited,
    /// Pure `U(...)`.
    Utf8,
    /// Pure `1(...)` bit/boolean.
    Bit,
}

/// Expands, validates, and compiles [`ElementaryClauses`] into a canonical [`HirType`].
///
/// # Arguments
///
/// * `clauses` - Bundled item clauses (`PICTURE`, `USAGE`, `SIGN`, `JUSTIFIED`, `BLANK WHEN ZERO`).
/// * `dialect` - Active [`DialectConfig`].
/// * `sink` - Diagnostic sink receiving `E0006InvalidPicture` / `E0007InvalidLayout` errors.
#[must_use]
pub fn compile_elementary_type(
    clauses: &ElementaryClauses<'_>,
    dialect: &DialectConfig,
    sink: &mut DiagnosticSink,
) -> HirType {
    let Some((pic_str, pic_span)) = clauses.raw_pic else {
        return resolve_pictureless_usage(clauses.usage, clauses.entry_span, dialect, sink);
    };

    if let Some(u) = clauses.usage
        && usage_forbids_picture(u)
    {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0006InvalidPicture,
            Some(pic_span),
            format!("PICTURE clause is not permitted with `USAGE {u:?}`"),
        ));
    }

    let parsed = parse_picture_string(pic_str, pic_span, clauses.pic_ctx, dialect, sink);
    combine_picture_and_usage(parsed, pic_span, clauses, dialect, sink)
}

/// Resolves an elementary item that has no `PICTURE` clause based on its `USAGE`.
///
/// # Arguments
///
/// * `usage` - Optional explicit or inherited [`UsageKind`].
/// * `span` - Entry span for diagnostics.
/// * `dialect` - Active [`DialectConfig`].
/// * `sink` - Diagnostic sink.
fn resolve_pictureless_usage(
    usage: Option<&UsageKind>,
    span: SourceSpan,
    dialect: &DialectConfig,
    sink: &mut DiagnosticSink,
) -> HirType {
    let endian = dialect_binary_endian(dialect);
    let Some(u) = usage else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(span),
            String::from(
                "elementary data item requires a `PICTURE` or computational `USAGE` clause",
            ),
        ));
        return HirType::Alphanumeric {
            byte_len: 1,
            justified: JustifiedSpec::Left,
        };
    };
    match u {
        UsageKind::FloatShort => HirType::FloatBinary {
            width: FloatBinaryWidth::Bits32,
        },
        UsageKind::FloatLong => HirType::FloatBinary {
            width: FloatBinaryWidth::Bits64,
        },
        UsageKind::FloatExtended => HirType::FloatBinary {
            width: FloatBinaryWidth::Bits128,
        },
        UsageKind::FloatDecimal16 => HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal64,
        },
        UsageKind::FloatDecimal34 => HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal128,
        },
        UsageKind::Index => HirType::Index,
        UsageKind::Pointer => HirType::Pointer,
        UsageKind::ProgramPointer | UsageKind::FunctionPointer => HirType::ProgramPointer,
        UsageKind::ObjectReference(cls) => HirType::ObjectRef { class_sym: *cls },
        UsageKind::BinaryCharSigned => fixed_bin_int(1, 3, Signedness::Signed, endian),
        UsageKind::BinaryCharUnsigned => fixed_bin_int(1, 3, Signedness::Unsigned, endian),
        UsageKind::BinaryShortSigned => fixed_bin_int(2, 5, Signedness::Signed, endian),
        UsageKind::BinaryShortUnsigned => fixed_bin_int(2, 5, Signedness::Unsigned, endian),
        UsageKind::BinaryLongSigned => fixed_bin_int(4, 10, Signedness::Signed, endian),
        UsageKind::BinaryLongUnsigned => fixed_bin_int(4, 10, Signedness::Unsigned, endian),
        UsageKind::BinaryDoubleSigned | UsageKind::BinaryCLongSigned => {
            fixed_bin_int(8, 20, Signedness::Signed, endian)
        }
        UsageKind::BinaryDoubleUnsigned | UsageKind::BinaryCLongUnsigned => {
            fixed_bin_int(8, 20, Signedness::Unsigned, endian)
        }
        _ => {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(span),
                format!("`USAGE {u:?}` requires a `PICTURE` clause"),
            ));
            HirType::Alphanumeric {
                byte_len: 1,
                justified: JustifiedSpec::Left,
            }
        }
    }
}

/// Combines a [`ParsedPicture`] with [`ElementaryClauses`] into a [`HirType`].
///
/// # Arguments
///
/// * `parsed` - Expanded [`ParsedPicture`].
/// * `span` - Span of the `PICTURE` clause.
/// * `clauses` - Item clauses.
/// * `dialect` - Active [`DialectConfig`].
/// * `sink` - Diagnostic sink.
fn combine_picture_and_usage(
    parsed: ParsedPicture,
    span: SourceSpan,
    clauses: &ElementaryClauses<'_>,
    dialect: &DialectConfig,
    sink: &mut DiagnosticSink,
) -> HirType {
    let justified = match clauses.justified {
        JustificationMode::StandardLeft => JustifiedSpec::Left,
        JustificationMode::JustifiedRight => JustifiedSpec::Right,
    };
    let blank_zero = match clauses.blank_zero {
        BlankWhenZeroMode::Normal => BlankZeroSpec::RetainZero,
        BlankWhenZeroMode::BlankWhenZero => BlankZeroSpec::BlankWhenZero,
    };

    if justified == JustifiedSpec::Right
        && matches!(
            parsed.kind,
            PictureFlavor::Numeric | PictureFlavor::NumericEdited
        )
    {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(span),
            String::from("`JUSTIFIED` clause is not permitted on numeric or numeric-edited items"),
        ));
    }

    match parsed.kind {
        PictureFlavor::Alphabetic => HirType::Alphabetic {
            byte_len: parsed.display_len.max(1),
            justified,
        },
        PictureFlavor::Alphanumeric => {
            resolve_alphanumeric_with_usage(parsed.display_len, clauses.usage, justified)
        }
        PictureFlavor::AlphanumericEdited => HirType::AlphanumericEdited {
            byte_len: parsed.display_len.max(1),
            edit_ops: parsed.ops,
        },
        PictureFlavor::National => HirType::National {
            char_len: parsed.display_len.max(1),
            justified,
        },
        PictureFlavor::NationalEdited => HirType::NationalEdited {
            char_len: parsed.display_len.max(1),
            edit_ops: parsed.ops,
        },
        PictureFlavor::Utf8 => HirType::Utf8 {
            byte_len: parsed.display_len.max(1),
        },
        PictureFlavor::Bit => HirType::Bit {
            bit_len: parsed.display_len.max(1),
        },
        PictureFlavor::NumericEdited => {
            if let Some(u) = clauses.usage
                && !matches!(u, UsageKind::Display | UsageKind::National)
            {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0006InvalidPicture,
                    Some(span),
                    format!("numeric-edited PICTURE requires `USAGE DISPLAY`, found `{u:?}`"),
                ));
            }
            let digits = clamp_digits(parsed.digit_positions, dialect.max_pic_digits, span, sink);
            let scale = ScaleFactor::try_new(parsed.scale).unwrap_or(ScaleFactor::ZERO);
            HirType::NumericEdited {
                byte_len: parsed.display_len.max(1),
                digits,
                scale,
                sign_capability: parsed.signed,
                edit_ops: parsed.ops,
                blank_zero,
            }
        }
        PictureFlavor::Numeric => {
            let digits = clamp_digits(parsed.digit_positions, dialect.max_pic_digits, span, sink);
            let scale = ScaleFactor::try_new(parsed.scale).unwrap_or(ScaleFactor::ZERO);
            resolve_numeric_storage(
                (digits, scale, parsed.signed),
                clauses,
                blank_zero,
                span,
                dialect,
                sink,
            )
        }
    }
}

/// Resolves `PIC X(...)` paired with `DISPLAY`, `COMP-X`/`COMP-N`, `NATIONAL`, or `UTF-8`.
///
/// # Arguments
///
/// * `display_len` - Character/byte length.
/// * `usage` - Optional `USAGE`.
/// * `justified` - Justification spec.
#[must_use]
fn resolve_alphanumeric_with_usage(
    display_len: u32,
    usage: Option<&UsageKind>,
    justified: JustifiedSpec,
) -> HirType {
    match usage {
        Some(UsageKind::CompX | UsageKind::CompN) => {
            let bw = u8::try_from(display_len.clamp(1, 16)).unwrap_or(8);
            let dc = DigitCount::try_new((bw.saturating_mul(2)).min(38)).unwrap_or(DigitCount::ONE);
            let endian = if matches!(usage, Some(UsageKind::CompX)) {
                BinaryEndianness::BigEndian
            } else {
                BinaryEndianness::Native
            };
            HirType::BinaryInt {
                byte_width: bw,
                digits: dc,
                scale: ScaleFactor::ZERO,
                sign: Signedness::Unsigned,
                endian,
                trunc: TruncationPolicy::FullBinaryRange,
            }
        }
        Some(UsageKind::National) => HirType::National {
            char_len: display_len.max(1),
            justified,
        },
        Some(UsageKind::Utf8) => HirType::Utf8 {
            byte_len: display_len.max(1),
        },
        _ => HirType::Alphanumeric {
            byte_len: display_len.max(1),
            justified,
        },
    }
}

/// Maps a pure numeric `PICTURE` + `USAGE` into `ZonedDecimal`, `PackedDecimal`, or `BinaryInt`.
///
/// # Arguments
///
/// * `num_spec` - Validated `(digits, scale, signed)` tuple.
/// * `clauses` - Item clauses.
/// * `blank_zero` - `BLANK WHEN ZERO` spec.
/// * `span` - Clause span.
/// * `dialect` - Active dialect config.
/// * `sink` - Diagnostic sink.
fn resolve_numeric_storage(
    num_spec: (DigitCount, ScaleFactor, Signedness),
    clauses: &ElementaryClauses<'_>,
    blank_zero: BlankZeroSpec,
    span: SourceSpan,
    dialect: &DialectConfig,
    sink: &mut DiagnosticSink,
) -> HirType {
    let (digits, scale, signed) = num_spec;
    match clauses.usage.unwrap_or(&UsageKind::Display) {
        UsageKind::PackedDecimal => HirType::PackedDecimal {
            digits,
            scale,
            sign: signed,
            unsigned_comp6: Comp6Mode::StandardComp3,
        },
        UsageKind::Comp6 => {
            if signed == Signedness::Signed {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0006InvalidPicture,
                    Some(span),
                    String::from("`COMP-6` item cannot be signed (`S` in PICTURE)"),
                ));
            }
            HirType::PackedDecimal {
                digits,
                scale,
                sign: Signedness::Unsigned,
                unsigned_comp6: Comp6Mode::UnsignedComp6,
            }
        }
        UsageKind::Binary | UsageKind::Comp5 | UsageKind::CompX | UsageKind::CompN => {
            let is_comp5 = matches!(clauses.usage, Some(UsageKind::Comp5));
            let is_raw = matches!(clauses.usage, Some(UsageKind::CompX | UsageKind::CompN));
            let endian = if is_comp5 || matches!(clauses.usage, Some(UsageKind::CompN)) {
                BinaryEndianness::Native
            } else if matches!(clauses.usage, Some(UsageKind::CompX)) {
                BinaryEndianness::BigEndian
            } else {
                dialect_binary_endian(dialect)
            };
            let trunc = if is_comp5 || is_raw || dialect.binary_truncate == TruncationMode::NoTrunc
            {
                TruncationPolicy::FullBinaryRange
            } else {
                TruncationPolicy::TruncateToPic
            };
            let byte_width = compute_binary_byte_width(digits.get(), dialect.binary_size);
            HirType::BinaryInt {
                byte_width,
                digits,
                scale,
                sign: signed,
                endian,
                trunc,
            }
        }
        _ => resolve_zoned_sign(
            digits,
            scale,
            signed,
            clauses.sign_clause,
            blank_zero,
            span,
            sink,
        ),
    }
}

/// Resolves `USAGE DISPLAY` zoned-decimal sign representation and `BLANK WHEN ZERO` rules.
///
/// # Arguments
///
/// * `digits` - Validated digit count.
/// * `scale` - Validated scale factor.
/// * `signed` - Signedness from `PIC S`.
/// * `sign_clause` - Optional `SIGN IS` clause.
/// * `blank_zero` - `BLANK WHEN ZERO` spec.
/// * `span` - Clause span.
/// * `sink` - Diagnostic sink.
fn resolve_zoned_sign(
    digits: DigitCount,
    scale: ScaleFactor,
    signed: Signedness,
    sign_clause: Option<SignClause>,
    blank_zero: BlankZeroSpec,
    span: SourceSpan,
    sink: &mut DiagnosticSink,
) -> HirType {
    let sign = match (signed, sign_clause) {
        (Signedness::Unsigned, Some(_)) => {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0006InvalidPicture,
                Some(span),
                String::from("`SIGN` clause requires `S` in numeric PICTURE"),
            ));
            NumericSignSpec::Unsigned
        }
        (Signedness::Unsigned, None) => NumericSignSpec::Unsigned,
        (Signedness::Signed, None) => NumericSignSpec::EmbeddedTrailing,
        (Signedness::Signed, Some(sc)) => match (sc.placement, sc.separation) {
            (SignPlacement::Trailing, SignSeparation::Embedded) => {
                NumericSignSpec::EmbeddedTrailing
            }
            (SignPlacement::Leading, SignSeparation::Embedded) => NumericSignSpec::EmbeddedLeading,
            (SignPlacement::Trailing, SignSeparation::SeparateCharacter) => {
                NumericSignSpec::SeparateTrailing
            }
            (SignPlacement::Leading, SignSeparation::SeparateCharacter) => {
                NumericSignSpec::SeparateLeading
            }
        },
    };
    if blank_zero == BlankZeroSpec::BlankWhenZero && signed == Signedness::Signed {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0006InvalidPicture,
            Some(span),
            String::from("`BLANK WHEN ZERO` is not permitted on signed `PIC S9` items"),
        ));
    }
    HirType::ZonedDecimal {
        digits,
        scale,
        sign,
        blank_zero,
    }
}

/// Parses and validates a raw `PICTURE` string, expanding `(n)` repetition factors.
///
/// # Arguments
///
/// * `raw` - Raw `PICTURE` text.
/// * `span` - Source span for error reporting.
/// * `ctx` - Active [`PictureContext`].
/// * `dialect` - Active [`DialectConfig`].
/// * `sink` - Diagnostic sink.
#[must_use]
pub fn parse_picture_string(
    raw: &str,
    span: SourceSpan,
    ctx: PictureContext,
    dialect: &DialectConfig,
    sink: &mut DiagnosticSink,
) -> ParsedPicture {
    let expanded = expand_picture_repetitions(raw, span, sink);
    let max_pic_chars = if dialect.base_dialect == coblang_common::Dialect::Cobol85 {
        50_usize
    } else {
        255_usize
    };
    if raw.len() > max_pic_chars {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0006InvalidPicture,
            Some(span),
            format!(
                "PICTURE character-string length {} exceeds maximum {max_pic_chars}",
                raw.len()
            ),
        ));
    }

    let chars: Vec<char> = expanded.chars().collect();
    let env = PicCharEnv {
        dec_ch: match ctx.decimal_point {
            DecimalPointKind::Period => '.',
            DecimalPointKind::Comma => ',',
        },
        grp_ch: match ctx.decimal_point {
            DecimalPointKind::Period => ',',
            DecimalPointKind::Comma => '.',
        },
        cur_ch: ctx.currency_symbol.to_ascii_uppercase(),
        total_len: chars.len(),
        total_plus: chars.iter().filter(|&&c| c == '+').count(),
        total_minus: chars.iter().filter(|&&c| c == '-').count(),
        total_cur: chars
            .iter()
            .filter(|&&c| c.eq_ignore_ascii_case(&ctx.currency_symbol))
            .count(),
        span,
    };

    let mut state = PicScanState::default();
    let mut idx = 0;
    while let Some(&raw_c) = chars.get(idx) {
        let c = raw_c.to_ascii_uppercase();
        if (c == 'C'
            && chars
                .get(idx + 1)
                .is_some_and(|n| n.eq_ignore_ascii_case(&'R')))
            || (c == 'D'
                && chars
                    .get(idx + 1)
                    .is_some_and(|n| n.eq_ignore_ascii_case(&'B')))
        {
            if idx + 2 != chars.len() || state.has(PIC_SIGN) {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0006InvalidPicture,
                    Some(span),
                    String::from(
                        "`CR`/`DB` must appear once at the end of a numeric-edited PICTURE",
                    ),
                ));
            }
            state.set(PIC_SIGN | PIC_NUM_EDIT);
            state.display_len += 2;
            state.ops.push(if c == 'C' {
                PicEditOp::FixedCreditCr
            } else {
                PicEditOp::FixedDebitDb
            });
            idx += 2;
            continue;
        }

        scan_single_pic_char(c, idx, &env, &mut state, sink);
        idx += 1;
    }

    finalize_parsed_picture(state, span, sink)
}

/// Precomputed per-string character totals for `scan_single_pic_char`.
#[derive(Debug, Clone, Copy)]
struct PicCharEnv {
    /// Decimal point character.
    dec_ch: char,
    /// Grouping separator character.
    grp_ch: char,
    /// Uppercase currency character.
    cur_ch: char,
    /// Expanded string length.
    total_len: usize,
    /// Total `'+'` count.
    total_plus: usize,
    /// Total `'-'` count.
    total_minus: usize,
    /// Total currency symbol count.
    total_cur: usize,
    /// Clause span.
    span: SourceSpan,
}

/// Bitflag for `'A'` seen in `PICTURE`.
const PIC_A: u16 = 1 << 0;
/// Bitflag for `'X'` seen in `PICTURE`.
const PIC_X: u16 = 1 << 1;
/// Bitflag for `'9'` seen in `PICTURE`.
const PIC_9: u16 = 1 << 2;
/// Bitflag for `'N'` seen in `PICTURE`.
const PIC_N: u16 = 1 << 3;
/// Bitflag for `'U'` seen in `PICTURE`.
const PIC_U: u16 = 1 << 4;
/// Bitflag for `'1'` seen in `PICTURE`.
const PIC_BIT: u16 = 1 << 5;
/// Bitflag for `B`, `0`, or `/` seen in `PICTURE`.
const PIC_SIMPLE_INS: u16 = 1 << 6;
/// Bitflag for numeric editing (`Z`, `*`, `.`, `,`, `+`, `-`, `CR`, `DB`, `$`).
const PIC_NUM_EDIT: u16 = 1 << 7;
/// Bitflag for any sign indicator (`S`, `+`, `-`, `CR`, `DB`).
const PIC_SIGN: u16 = 1 << 8;
/// Bitflag for explicit `'S'`.
const PIC_S: u16 = 1 << 9;
/// Bitflag for decimal point (`V` or `.`/`,`).
const PIC_DEC: u16 = 1 << 10;

/// Mutable accumulator used while scanning expanded `PICTURE` characters.
#[derive(Debug, Default)]
struct PicScanState {
    /// Compiled editing ops.
    ops: Vec<PicEditOp>,
    /// Display byte/char count.
    display_len: u32,
    /// Digit slots before decimal point.
    int_digits: u32,
    /// Digit slots after decimal point.
    frac_digits: u32,
    /// Leading `P` scaling count.
    leading_p: i16,
    /// Trailing `P` scaling count.
    trailing_p: i16,
    /// Packed `PIC_*` bitflags.
    flags: u16,
    /// Count of floating `+` occurrences.
    floating_plus: u32,
    /// Count of floating `-` occurrences.
    floating_minus: u32,
    /// Count of floating currency occurrences.
    floating_cur: u32,
}

impl PicScanState {
    /// Returns `true` if any bit in `mask` is set.
    ///
    /// # Arguments
    ///
    /// * `mask` - `PIC_*` bitmask to test.
    #[must_use]
    const fn has(&self, mask: u16) -> bool {
        (self.flags & mask) != 0
    }

    /// Sets `mask` bits in `self.flags`.
    ///
    /// # Arguments
    ///
    /// * `mask` - `PIC_*` bitmask to set.
    fn set(&mut self, mask: u16) {
        self.flags |= mask;
    }

    /// Increments `frac_digits` if past decimal point, else `int_digits`.
    fn add_digit_slot(&mut self) {
        if self.has(PIC_DEC) {
            self.frac_digits += 1;
        } else {
            self.int_digits += 1;
        }
    }
}

/// Processes one uppercase character in an expanded `PICTURE` string.
///
/// # Arguments
///
/// * `c` - Uppercase character.
/// * `idx` - Character index.
/// * `env` - Precomputed [`PicCharEnv`].
/// * `st` - Mutable scan state.
/// * `sink` - Diagnostic sink.
fn scan_single_pic_char(
    c: char,
    idx: usize,
    env: &PicCharEnv,
    st: &mut PicScanState,
    sink: &mut DiagnosticSink,
) {
    match c {
        'S' => {
            if idx != 0 {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0006InvalidPicture,
                    Some(env.span),
                    String::from("`S` may only appear once as the first character of PICTURE"),
                ));
            }
            st.set(PIC_S | PIC_SIGN);
        }
        'V' => {
            if st.has(PIC_DEC) {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0006InvalidPicture,
                    Some(env.span),
                    String::from("multiple decimal points (`V`/`.`) in PICTURE"),
                ));
            }
            st.set(PIC_DEC);
            st.ops.push(PicEditOp::ImpliedDecimal);
        }
        'P' => scan_p_scaling(env.span, st, sink),
        '9' => {
            st.set(PIC_9);
            st.display_len += 1;
            st.add_digit_slot();
            st.ops.push(PicEditOp::DigitSlot);
        }
        'A' | 'X' | 'N' | 'U' | '1' => {
            let (flag, op) = match c {
                'A' => (PIC_A, PicEditOp::AlphaSlot),
                'X' => (PIC_X, PicEditOp::AnyCharSlot),
                'N' => (PIC_N, PicEditOp::NationalSlot),
                'U' => (PIC_U, PicEditOp::Utf8Slot),
                _ => (PIC_BIT, PicEditOp::BitSlot),
            };
            st.set(flag);
            st.display_len += 1;
            st.ops.push(op);
        }
        'Z' | '*' => {
            st.set(PIC_NUM_EDIT);
            st.display_len += 1;
            st.add_digit_slot();
            st.ops.push(if c == 'Z' {
                PicEditOp::ZeroSuppressSpace
            } else {
                PicEditOp::ZeroSuppressAsterisk
            });
        }
        'B' | '0' | '/' => {
            st.set(PIC_SIMPLE_INS);
            st.display_len += 1;
            st.ops.push(match c {
                'B' => PicEditOp::InsertBlank,
                '0' => PicEditOp::InsertZero,
                _ => PicEditOp::InsertSlash,
            });
        }
        _ => scan_sign_or_currency_char(c, idx, env, st, sink),
    }
}

/// Processes `'P'` scaling position inside `PICTURE`.
///
/// # Arguments
///
/// * `span` - Clause span.
/// * `st` - Mutable scan state.
/// * `sink` - Diagnostic sink.
fn scan_p_scaling(span: SourceSpan, st: &mut PicScanState, sink: &mut DiagnosticSink) {
    if st.has(PIC_DEC) {
        if st.frac_digits == 0 {
            st.leading_p = st.leading_p.saturating_add(1);
        } else {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0006InvalidPicture,
                Some(span),
                String::from("invalid `P` scaling position in PICTURE"),
            ));
        }
    } else if st.int_digits > 0 {
        st.trailing_p = st.trailing_p.saturating_add(1);
    } else {
        st.set(PIC_DEC);
        st.leading_p = st.leading_p.saturating_add(1);
    }
    st.ops.push(PicEditOp::ScalingPosition);
}

/// Processes `.`/`,`/`+`/`-`/currency/invalid characters in `PICTURE`.
///
/// # Arguments
///
/// * `c` - Uppercase character.
/// * `idx` - Character index.
/// * `env` - Precomputed [`PicCharEnv`].
/// * `st` - Mutable scan state.
/// * `sink` - Diagnostic sink.
fn scan_sign_or_currency_char(
    c: char,
    idx: usize,
    env: &PicCharEnv,
    st: &mut PicScanState,
    sink: &mut DiagnosticSink,
) {
    if c == env.dec_ch {
        if st.has(PIC_DEC) {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0006InvalidPicture,
                Some(env.span),
                String::from("multiple decimal points in PICTURE"),
            ));
        }
        st.set(PIC_DEC | PIC_NUM_EDIT);
        st.display_len += 1;
        st.ops.push(PicEditOp::InsertDecimalPoint);
    } else if c == env.grp_ch {
        st.set(PIC_NUM_EDIT);
        st.display_len += 1;
        st.ops.push(PicEditOp::InsertGroupSep);
    } else if c == '+' || c == '-' {
        st.set(PIC_NUM_EDIT | PIC_SIGN);
        st.display_len += 1;
        let is_plus = c == '+';
        let total = if is_plus {
            env.total_plus
        } else {
            env.total_minus
        };
        if total > 1 {
            let cnt = if is_plus {
                st.floating_plus += 1;
                st.floating_plus
            } else {
                st.floating_minus += 1;
                st.floating_minus
            };
            if cnt > 1 {
                st.add_digit_slot();
            }
            st.ops.push(if is_plus {
                PicEditOp::FloatingSignPlus
            } else {
                PicEditOp::FloatingSignMinus
            });
        } else {
            if idx != 0 && idx + 1 != env.total_len {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0006InvalidPicture,
                    Some(env.span),
                    format!("fixed `{c}` sign must be leading or trailing in PICTURE"),
                ));
            }
            st.ops.push(if is_plus {
                PicEditOp::FixedSignPlus
            } else {
                PicEditOp::FixedSignMinus
            });
        }
    } else if c == env.cur_ch {
        st.set(PIC_NUM_EDIT);
        st.display_len += 1;
        if env.total_cur > 1 {
            st.floating_cur += 1;
            if st.floating_cur > 1 {
                st.add_digit_slot();
            }
            st.ops.push(PicEditOp::FloatingCurrency(env.cur_ch));
        } else {
            st.ops.push(PicEditOp::FixedCurrency(env.cur_ch));
        }
    } else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0006InvalidPicture,
            Some(env.span),
            format!("invalid symbol `{c}` in PICTURE character-string"),
        ));
    }
}

/// Validates cross-category combinations and produces [`ParsedPicture`].
///
/// # Arguments
///
/// * `st` - Accumulated scan state.
/// * `span` - Clause span.
/// * `sink` - Diagnostic sink.
fn finalize_parsed_picture(
    st: PicScanState,
    span: SourceSpan,
    sink: &mut DiagnosticSink,
) -> ParsedPicture {
    if st.has(PIC_A | PIC_X) && (st.has(PIC_S | PIC_DEC | PIC_NUM_EDIT) || st.trailing_p > 0) {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0006InvalidPicture,
            Some(span),
            String::from("alphanumeric `A`/`X` symbols cannot combine with `S`, `V`, `P`, or numeric editing"),
        ));
    }
    if st.has(PIC_N) && st.has(PIC_A | PIC_X | PIC_9 | PIC_U | PIC_BIT) {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0006InvalidPicture,
            Some(span),
            String::from("national `N` symbol cannot mix with `A`, `X`, `9`, `U`, or `1`"),
        ));
    }
    if st.has(PIC_S) && st.has(PIC_NUM_EDIT) {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0006InvalidPicture,
            Some(span),
            String::from("`S` cannot combine with numeric editing symbols in PICTURE"),
        ));
    }

    let digit_positions = st.int_digits + st.frac_digits;
    let kind = classify_picture_flavor(&st, digit_positions, span, sink);
    let frac_i16 = i16::try_from(st.frac_digits).unwrap_or(38);
    let raw_scale = if st.trailing_p > 0 {
        -st.trailing_p
    } else {
        frac_i16.saturating_add(st.leading_p)
    };
    let clamped_scale = raw_scale.clamp(-38, 38);
    let signed = if st.has(PIC_SIGN) {
        Signedness::Signed
    } else {
        Signedness::Unsigned
    };

    ParsedPicture {
        ops: st.ops,
        display_len: st.display_len,
        digit_positions,
        scale: clamped_scale,
        signed,
        kind,
    }
}

/// Classifies the [`PictureFlavor`] from accumulated flags and emits zero-digit errors.
///
/// # Arguments
///
/// * `st` - Scan state.
/// * `digit_positions` - Total digit count.
/// * `span` - Clause span.
/// * `sink` - Diagnostic sink.
fn classify_picture_flavor(
    st: &PicScanState,
    digit_positions: u32,
    span: SourceSpan,
    sink: &mut DiagnosticSink,
) -> PictureFlavor {
    if st.has(PIC_N) {
        if st.has(PIC_SIMPLE_INS) {
            PictureFlavor::NationalEdited
        } else {
            PictureFlavor::National
        }
    } else if st.has(PIC_U) {
        PictureFlavor::Utf8
    } else if st.has(PIC_BIT) {
        PictureFlavor::Bit
    } else if st.has(PIC_X) || (st.has(PIC_A) && st.has(PIC_9)) {
        if st.has(PIC_SIMPLE_INS) {
            PictureFlavor::AlphanumericEdited
        } else {
            PictureFlavor::Alphanumeric
        }
    } else if st.has(PIC_A) {
        if st.has(PIC_SIMPLE_INS) {
            PictureFlavor::AlphanumericEdited
        } else {
            PictureFlavor::Alphabetic
        }
    } else if st.has(PIC_NUM_EDIT | PIC_SIMPLE_INS) {
        if digit_positions == 0 {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0006InvalidPicture,
                Some(span),
                String::from("numeric-edited PICTURE must represent at least one digit position"),
            ));
        }
        PictureFlavor::NumericEdited
    } else {
        if digit_positions == 0 {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0006InvalidPicture,
                Some(span),
                String::from("empty or zero-digit PICTURE character-string"),
            ));
        }
        PictureFlavor::Numeric
    }
}

/// Hard safeguard cap on total expanded `PICTURE` characters (`65_535`).
const MAX_EXPANDED_PICTURE_LEN: usize = 65_535;

/// Expands `char(count)` repetition factors inside `raw` (e.g. `S9(4)V9(2)` -> `S9999V99`).
///
/// # Arguments
///
/// * `raw` - Raw `PICTURE` string.
/// * `span` - Source span for malformed repetition diagnostics.
/// * `sink` - Diagnostic sink.
#[must_use]
fn expand_picture_repetitions(raw: &str, span: SourceSpan, sink: &mut DiagnosticSink) -> String {
    let mut out = String::with_capacity(raw.len() * 2);
    let chars: Vec<char> = raw.chars().collect();
    let mut idx = 0;
    while let Some(&c) = chars.get(idx) {
        if c == '(' {
            let Some(prev) = out.chars().last() else {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0006InvalidPicture,
                    Some(span),
                    String::from("repetition factor `(...)` has no preceding PICTURE symbol"),
                ));
                idx += 1;
                continue;
            };
            let mut end = idx + 1;
            while chars.get(end).is_some_and(|&ch| ch != ')') {
                end += 1;
            }
            if chars.get(end) != Some(&')') {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0006InvalidPicture,
                    Some(span),
                    String::from("unclosed `(` in PICTURE repetition factor"),
                ));
                break;
            }
            let num_str: String = chars.get(idx + 1..end).into_iter().flatten().collect();
            match num_str.parse::<usize>() {
                Ok(n) if (1..=MAX_EXPANDED_PICTURE_LEN).contains(&n) => {
                    let extra = n - 1;
                    if out.len().saturating_add(extra) > MAX_EXPANDED_PICTURE_LEN {
                        sink.emit(Diagnostic::error(
                            DiagnosticCode::E0006InvalidPicture,
                            Some(span),
                            format!(
                                "expanded PICTURE exceeds maximum length {MAX_EXPANDED_PICTURE_LEN}"
                            ),
                        ));
                        let remaining = MAX_EXPANDED_PICTURE_LEN.saturating_sub(out.len());
                        for _ in 0..remaining {
                            out.push(prev);
                        }
                        break;
                    }
                    for _ in 0..extra {
                        out.push(prev);
                    }
                }
                _ => {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0006InvalidPicture,
                        Some(span),
                        format!("invalid PICTURE repetition count `({num_str})`"),
                    ));
                }
            }
            idx = end + 1;
        } else {
            if out.len() >= MAX_EXPANDED_PICTURE_LEN {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0006InvalidPicture,
                    Some(span),
                    format!("expanded PICTURE exceeds maximum length {MAX_EXPANDED_PICTURE_LEN}"),
                ));
                break;
            }
            out.push(c);
            idx += 1;
        }
    }
    out
}

/// Validates and clamps `digit_positions` to `1..=max_digits`, emitting `E0006InvalidPicture` on overflow.
///
/// # Arguments
///
/// * `digit_positions` - Parsed digit count.
/// * `max_digits` - Dialect maximum digit count (`18` or `38`).
/// * `span` - Source span.
/// * `sink` - Diagnostic sink.
fn clamp_digits(
    digit_positions: u32,
    max_digits: u8,
    span: SourceSpan,
    sink: &mut DiagnosticSink,
) -> DigitCount {
    if digit_positions > u32::from(max_digits) {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0006InvalidPicture,
            Some(span),
            format!("numeric PICTURE has {digit_positions} digits (maximum is {max_digits})"),
        ));
    }
    let clamped = u8::try_from(digit_positions.clamp(1, 38)).unwrap_or(1);
    DigitCount::try_new(clamped).unwrap_or(DigitCount::ONE)
}

/// Constructs a pictureless `HirType::BinaryInt` for `BINARY-CHAR/SHORT/LONG/DOUBLE`.
///
/// # Arguments
///
/// * `byte_width` - Storage width (`1`, `2`, `4`, or `8`).
/// * `digits_raw` - Equivalent decimal precision.
/// * `sign` - Signed vs unsigned.
/// * `endian` - Target byte order.
#[must_use]
fn fixed_bin_int(
    byte_width: u8,
    digits_raw: u8,
    sign: Signedness,
    endian: BinaryEndianness,
) -> HirType {
    let digits = DigitCount::try_new(digits_raw).unwrap_or(DigitCount::ONE);
    HirType::BinaryInt {
        byte_width,
        digits,
        scale: ScaleFactor::ZERO,
        sign,
        endian,
        trunc: TruncationPolicy::FullBinaryRange,
    }
}

/// Maps `DialectConfig::binary_byteorder` to [`BinaryEndianness`].
///
/// # Arguments
///
/// * `dialect` - Active dialect config.
#[must_use]
const fn dialect_binary_endian(dialect: &DialectConfig) -> BinaryEndianness {
    match dialect.binary_byteorder {
        BinaryByteOrder::Native => BinaryEndianness::Native,
        BinaryByteOrder::BigEndian => BinaryEndianness::BigEndian,
    }
}

/// Computes `BINARY`/`COMP` byte width from decimal `digits` and [`BinarySizePolicy`].
///
/// # Arguments
///
/// * `digits` - Decimal digit count (`1..=38`).
/// * `policy` - Dialect `binary-size` policy.
#[must_use]
const fn compute_binary_byte_width(digits: u8, policy: BinarySizePolicy) -> u8 {
    match policy {
        BinarySizePolicy::TwoFourEight => match digits {
            1..=4 => 2,
            5..=9 => 4,
            10..=18 => 8,
            _ => 16,
        },
        BinarySizePolicy::OneTwoFourEight => match digits {
            1..=2 => 1,
            3..=4 => 2,
            5..=9 => 4,
            10..=18 => 8,
            _ => 16,
        },
        BinarySizePolicy::OneToEight => match digits {
            1..=2 => 1,
            3..=4 => 2,
            5..=7 => 3,
            8..=9 => 4,
            10..=12 => 5,
            13..=14 => 6,
            15..=16 => 7,
            17..=18 => 8,
            _ => 16,
        },
    }
}

/// Returns `true` for `USAGE` representations that forbid an accompanying `PICTURE` clause.
///
/// # Arguments
///
/// * `u` - Usage kind.
#[must_use]
const fn usage_forbids_picture(u: &UsageKind) -> bool {
    matches!(
        u,
        UsageKind::FloatShort
            | UsageKind::FloatLong
            | UsageKind::FloatExtended
            | UsageKind::FloatDecimal16
            | UsageKind::FloatDecimal34
            | UsageKind::BinaryCharSigned
            | UsageKind::BinaryCharUnsigned
            | UsageKind::BinaryShortSigned
            | UsageKind::BinaryShortUnsigned
            | UsageKind::BinaryLongSigned
            | UsageKind::BinaryLongUnsigned
            | UsageKind::BinaryDoubleSigned
            | UsageKind::BinaryDoubleUnsigned
            | UsageKind::BinaryCLongSigned
            | UsageKind::BinaryCLongUnsigned
            | UsageKind::Index
            | UsageKind::Pointer
            | UsageKind::ProgramPointer
            | UsageKind::FunctionPointer
            | UsageKind::ObjectReference(_)
    )
}
