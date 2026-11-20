//! Unit and 100% branch-coverage tests for `coblang-rt-core`.

use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use coblang_common::Result;
use coblang_rt_core::{
    CobolRuntimeContext, FieldDisplayKind, LineAdvancing, OutputDevice, RuntimeDisplayItem,
    TrimDirection, decode_be_i64, decode_be_u64, decode_ne_i64, decode_ne_u64, format_display_item,
    global_runtime, trim_ascii_spaces, with_global_runtime, write_display_items,
};

/// Mock writer that deterministically fails on `write`, `newline`, or `flush`.
struct FailingWriter {
    /// Fail after this many `write` calls (`0` fails immediately).
    fail_on_write_call: Option<usize>,
    /// Whether `flush` should return an error.
    fail_on_flush: bool,
    /// Observed write call counter.
    writes: usize,
}

impl Write for FailingWriter {
    /// Writes `buf` or simulates an I/O failure on the configured call index.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let current = self.writes;
        self.writes = self.writes.saturating_add(1);
        if self.fail_on_write_call == Some(current) {
            return Err(io::Error::other("simulated write failure"));
        }
        Ok(buf.len())
    }

    /// Flushes the stream or simulates a flush error when `fail_on_flush` is set.
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_on_flush {
            return Err(io::Error::other("simulated flush failure"));
        }
        Ok(())
    }
}

/// Verifies ABI round-trips for [`OutputDevice`], [`LineAdvancing`], [`TrimDirection`], and [`FieldDisplayKind`].
#[test]
fn abi_enum_round_trips() {
    assert_eq!(OutputDevice::from_abi_code(0), OutputDevice::Stdout);
    assert_eq!(OutputDevice::from_abi_code(1), OutputDevice::Stderr);
    assert_eq!(OutputDevice::Stdout.to_abi_code(), 0);
    assert_eq!(OutputDevice::Stderr.to_abi_code(), 1);

    assert_eq!(LineAdvancing::from_abi_code(0), LineAdvancing::NoAdvancing);
    assert_eq!(LineAdvancing::from_abi_code(1), LineAdvancing::AdvanceLine);
    assert_eq!(LineAdvancing::NoAdvancing.to_abi_code(), 0);
    assert_eq!(LineAdvancing::AdvanceLine.to_abi_code(), 1);

    assert_eq!(TrimDirection::from_abi_code(0), TrimDirection::Both);
    assert_eq!(TrimDirection::from_abi_code(1), TrimDirection::Leading);
    assert_eq!(TrimDirection::from_abi_code(2), TrimDirection::Trailing);
    assert_eq!(TrimDirection::from_abi_code(99), TrimDirection::Both);
    assert_eq!(TrimDirection::Both.to_abi_code(), 0);
    assert_eq!(TrimDirection::Leading.to_abi_code(), 1);
    assert_eq!(TrimDirection::Trailing.to_abi_code(), 2);

    for kind in [
        FieldDisplayKind::RawBytes,
        FieldDisplayKind::SignedInt32,
        FieldDisplayKind::UnsignedInt32,
        FieldDisplayKind::SignedBigEndianInt,
        FieldDisplayKind::UnsignedBigEndianInt,
    ] {
        assert_eq!(FieldDisplayKind::from_abi_tag(kind.to_abi_tag()), kind);
    }
    assert_eq!(
        FieldDisplayKind::from_abi_tag(250),
        FieldDisplayKind::RawBytes
    );
}

/// Verifies [`CobolRuntimeContext`] state transitions and poisoned-mutex recovery on [`global_runtime`].
#[test]
fn runtime_context_and_global_singleton_operations() {
    let mut ctx = CobolRuntimeContext::default();
    assert_eq!(ctx.return_code(), 0);
    assert_eq!(ctx.call_depth(), 0);
    assert_eq!(ctx.args().len(), 0);

    ctx.initialize(vec![String::from("prog"), String::from("--arg")]);
    assert_eq!(ctx.args().len(), 2);
    ctx.set_return_code(42);
    assert_eq!(ctx.return_code(), 42);
    ctx.enter_call();
    assert_eq!(ctx.call_depth(), 1);
    ctx.leave_call();
    assert_eq!(ctx.call_depth(), 0);
    ctx.tidy();
    assert_eq!(ctx.args().len(), 0);

    with_global_runtime(|rt| {
        rt.initialize(Vec::new());
        rt.set_return_code(7);
    });
    let observed = with_global_runtime(|rt| rt.return_code());
    assert_eq!(observed, 7);

    // Exercise poisoned lock recovery path on `with_global_runtime`.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = global_runtime().lock();
        std::panic::resume_unwind(Box::new("simulated poison"));
    }));
    let recovered = with_global_runtime(|rt| {
        rt.set_return_code(0);
        rt.return_code()
    });
    assert_eq!(recovered, 0);
}

/// Verifies [`trim_ascii_spaces`] across `Both`, `Leading`, `Trailing`, all-spaces, and empty inputs.
#[test]
fn trim_ascii_spaces_all_directions() {
    assert_eq!(
        trim_ascii_spaces(b"   Marty   ", TrimDirection::Both),
        b"Marty"
    );
    assert_eq!(
        trim_ascii_spaces(b"   Marty   ", TrimDirection::Leading),
        b"Marty   "
    );
    assert_eq!(
        trim_ascii_spaces(b"   Marty   ", TrimDirection::Trailing),
        b"   Marty"
    );
    assert_eq!(trim_ascii_spaces(b"     ", TrimDirection::Both), b"");
    assert_eq!(trim_ascii_spaces(b"     ", TrimDirection::Trailing), b"");
    assert_eq!(trim_ascii_spaces(b"", TrimDirection::Leading), b"");
}

/// Verifies [`decode_be_u64`], [`decode_be_i64`], [`decode_ne_i64`], [`decode_ne_u64`], and [`format_display_item`].
#[test]
fn display_formatting_branches() {
    assert_eq!(decode_be_u64(&[]), 0);
    assert_eq!(decode_be_u64(&[0x01, 0x02]), 0x0102);
    assert_eq!(decode_be_i64(&[]), 0);
    assert_eq!(decode_be_i64(&[0xFF, 0xFE]), -2);
    assert_eq!(decode_be_i64(&[0x00, 0x05]), 5);

    assert_eq!(decode_ne_i64(&(-3_i8).to_ne_bytes()), -3);
    assert_eq!(decode_ne_i64(&(-300_i16).to_ne_bytes()), -300);
    assert_eq!(decode_ne_i64(&(-70000_i32).to_ne_bytes()), -70000);
    assert_eq!(decode_ne_i64(&(-9_i64).to_ne_bytes()), -9);
    assert_eq!(decode_ne_i64(&[1, 2, 3]), 0);

    assert_eq!(decode_ne_u64(&7_u8.to_ne_bytes()), 7);
    assert_eq!(decode_ne_u64(&700_u16.to_ne_bytes()), 700);
    assert_eq!(decode_ne_u64(&70000_u32.to_ne_bytes()), 70000);
    assert_eq!(decode_ne_u64(&79_u64.to_ne_bytes()), 79);
    assert_eq!(decode_ne_u64(&[1, 2, 3]), 0);

    let neg_i32 = (-12_i32).to_ne_bytes();
    assert_eq!(
        format_display_item(&RuntimeDisplayItem {
            kind: FieldDisplayKind::SignedInt32,
            scale: 5,
            bytes: &neg_i32,
        }),
        b"-0.00012"
    );
    let pos_i32 = 12345_i32.to_ne_bytes();
    assert_eq!(
        format_display_item(&RuntimeDisplayItem {
            kind: FieldDisplayKind::SignedInt32,
            scale: 2,
            bytes: &pos_i32,
        }),
        b"123.45"
    );
    assert_eq!(
        format_display_item(&RuntimeDisplayItem {
            kind: FieldDisplayKind::SignedInt32,
            scale: -2,
            bytes: &pos_i32,
        }),
        b"1234500"
    );
    let zero_i32 = 0_i32.to_ne_bytes();
    assert_eq!(
        format_display_item(&RuntimeDisplayItem {
            kind: FieldDisplayKind::SignedInt32,
            scale: -2,
            bytes: &zero_i32,
        }),
        b"0"
    );
    assert_eq!(
        format_display_item(&RuntimeDisplayItem {
            kind: FieldDisplayKind::SignedInt32,
            scale: 0,
            bytes: &[1, 2, 3],
        }),
        b"0"
    );

    let u32_bytes = 99_u32.to_ne_bytes();
    assert_eq!(
        format_display_item(&RuntimeDisplayItem {
            kind: FieldDisplayKind::UnsignedInt32,
            scale: 0,
            bytes: &u32_bytes,
        }),
        b"99"
    );
    assert_eq!(
        format_display_item(&RuntimeDisplayItem {
            kind: FieldDisplayKind::UnsignedInt32,
            scale: 0,
            bytes: &[1, 2, 3],
        }),
        b"0"
    );
    assert_eq!(
        format_display_item(&RuntimeDisplayItem {
            kind: FieldDisplayKind::SignedBigEndianInt,
            scale: 0,
            bytes: &[0xFF, 0xFB],
        }),
        b"-5"
    );
    assert_eq!(
        format_display_item(&RuntimeDisplayItem {
            kind: FieldDisplayKind::UnsignedBigEndianInt,
            scale: 0,
            bytes: &[0x00, 0x2A],
        }),
        b"42"
    );
}

/// Verifies [`write_display_items`] across `AdvanceLine`, `NoAdvancing`, and all three I/O failure branches.
#[test]
fn write_display_items_and_io_error_branches() -> Result<()> {
    let items = [RuntimeDisplayItem {
        kind: FieldDisplayKind::RawBytes,
        scale: 0,
        bytes: b"HELLO",
    }];
    let mut buf = Vec::new();
    write_display_items(&items, LineAdvancing::AdvanceLine, &mut buf)?;
    assert_eq!(buf, [b'H', b'E', b'L', b'L', b'O', 0x0A]);

    buf.clear();
    write_display_items(&items, LineAdvancing::NoAdvancing, &mut buf)?;
    assert_eq!(buf, b"HELLO");

    // Exercise all 3 I/O error branches (`item write`, `newline write`, `flush`).
    let mut fail_item = FailingWriter {
        fail_on_write_call: Some(0),
        fail_on_flush: false,
        writes: 0,
    };
    assert!(write_display_items(&items, LineAdvancing::AdvanceLine, &mut fail_item).is_err());

    let mut fail_nl = FailingWriter {
        fail_on_write_call: Some(1),
        fail_on_flush: false,
        writes: 0,
    };
    assert!(write_display_items(&items, LineAdvancing::AdvanceLine, &mut fail_nl).is_err());

    let mut fail_flush = FailingWriter {
        fail_on_write_call: None,
        fail_on_flush: true,
        writes: 0,
    };
    assert!(write_display_items(&items, LineAdvancing::NoAdvancing, &mut fail_flush).is_err());
    Ok(())
}

/// Builds a list of all 23 `PicEditOp` variants for `desc_codec` round-trip testing.
fn sample_all_pic_edit_ops() -> Vec<coblang_hir::PicEditOp> {
    use coblang_hir::PicEditOp;
    vec![
        PicEditOp::AlphaSlot,
        PicEditOp::AnyCharSlot,
        PicEditOp::DigitSlot,
        PicEditOp::ImpliedDecimal,
        PicEditOp::ScalingPosition,
        PicEditOp::ZeroSuppressSpace,
        PicEditOp::ZeroSuppressAsterisk,
        PicEditOp::FloatingSignPlus,
        PicEditOp::FloatingSignMinus,
        PicEditOp::FloatingCurrency('$'),
        PicEditOp::FixedSignPlus,
        PicEditOp::FixedSignMinus,
        PicEditOp::FixedCreditCr,
        PicEditOp::FixedDebitDb,
        PicEditOp::FixedCurrency('€'),
        PicEditOp::InsertBlank,
        PicEditOp::InsertZero,
        PicEditOp::InsertSlash,
        PicEditOp::InsertGroupSep,
        PicEditOp::InsertDecimalPoint,
        PicEditOp::BitSlot,
        PicEditOp::NationalSlot,
        PicEditOp::Utf8Slot,
    ]
}

/// Builds representative `HirType` variants covering tags `0..=17` and all flag combinations.
///
/// # Arguments
///
/// * `ops` - Sample `PicEditOp` slice.
fn sample_all_hir_types(ops: &[coblang_hir::PicEditOp]) -> Result<Vec<coblang_hir::HirType>> {
    use coblang_common::{DigitCount, ScaleFactor, SymbolId};
    use coblang_hir::{
        DynamicOdoPresence, FloatBinaryWidth, FloatDecimalWidth, HirType, JustifiedSpec,
    };
    let d3 = DigitCount::try_new(3)?;
    let sc = ScaleFactor::try_new(1)?;
    let mut tys = vec![
        HirType::Alphabetic {
            byte_len: 2,
            justified: JustifiedSpec::Left,
        },
        HirType::Alphabetic {
            byte_len: 2,
            justified: JustifiedSpec::Right,
        },
        HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        },
        HirType::AlphanumericEdited {
            byte_len: 4,
            edit_ops: ops.to_vec(),
        },
        HirType::Pointer,
        HirType::ProgramPointer,
        HirType::Index,
        HirType::Group {
            static_byte_len: 8,
            odo_presence: DynamicOdoPresence::StaticSize,
        },
        HirType::Group {
            static_byte_len: 8,
            odo_presence: DynamicOdoPresence::HasDynamicOdo,
        },
        HirType::National {
            char_len: 3,
            justified: JustifiedSpec::Right,
        },
        HirType::NationalEdited {
            char_len: 3,
            edit_ops: vec![coblang_hir::PicEditOp::NationalSlot],
        },
        HirType::Utf8 { byte_len: 5 },
        HirType::Bit { bit_len: 4 },
        HirType::ObjectRef {
            class_sym: Some(SymbolId::new(9)),
        },
        HirType::ObjectRef { class_sym: None },
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits32,
        },
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits64,
        },
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits128,
        },
        HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal64,
        },
        HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal128,
        },
    ];
    append_numeric_sample_types(&mut tys, d3, sc, ops);
    Ok(tys)
}

/// Appends numeric/edited `HirType` combinations to `tys`.
///
/// # Arguments
///
/// * `tys` - Destination vector of `HirType`s.
/// * `d3` - 3-digit `DigitCount`.
/// * `sc` - 1-place `ScaleFactor`.
/// * `ops` - Sample `PicEditOp` slice.
fn append_numeric_sample_types(
    tys: &mut Vec<coblang_hir::HirType>,
    d3: coblang_common::DigitCount,
    sc: coblang_common::ScaleFactor,
    ops: &[coblang_hir::PicEditOp],
) {
    use coblang_hir::{
        BinaryEndianness, BlankZeroSpec, Comp6Mode, HirType, NumericSignSpec, Signedness,
        TruncationPolicy,
    };
    for (idx, sign) in [
        NumericSignSpec::Unsigned,
        NumericSignSpec::EmbeddedTrailing,
        NumericSignSpec::EmbeddedLeading,
        NumericSignSpec::SeparateTrailing,
        NumericSignSpec::SeparateLeading,
    ]
    .into_iter()
    .enumerate()
    {
        tys.push(HirType::ZonedDecimal {
            digits: d3,
            scale: sc,
            sign,
            blank_zero: if idx % 2 == 0 {
                BlankZeroSpec::RetainZero
            } else {
                BlankZeroSpec::BlankWhenZero
            },
        });
    }
    for (sign, comp6) in [
        (Signedness::Unsigned, Comp6Mode::StandardComp3),
        (Signedness::Signed, Comp6Mode::UnsignedComp6),
    ] {
        tys.push(HirType::PackedDecimal {
            digits: d3,
            scale: sc,
            sign,
            unsigned_comp6: comp6,
        });
    }
    for (sign, endian, trunc) in [
        (
            Signedness::Unsigned,
            BinaryEndianness::Native,
            TruncationPolicy::TruncateToPic,
        ),
        (
            Signedness::Signed,
            BinaryEndianness::BigEndian,
            TruncationPolicy::FullBinaryRange,
        ),
    ] {
        tys.push(HirType::BinaryInt {
            byte_width: 4,
            digits: d3,
            scale: sc,
            sign,
            endian,
            trunc,
        });
    }
    for (sign_capability, blank_zero) in [
        (Signedness::Unsigned, BlankZeroSpec::RetainZero),
        (Signedness::Signed, BlankZeroSpec::BlankWhenZero),
    ] {
        tys.push(HirType::NumericEdited {
            byte_len: 5,
            digits: d3,
            scale: sc,
            sign_capability,
            edit_ops: ops.to_vec(),
            blank_zero,
        });
    }
}

/// Verifies `desc_codec` round-trips, prefix truncations, invalid tags/scalars, and category codes.
#[test]
fn desc_codec_round_trips_and_malformed_inputs() -> Result<()> {
    use coblang_decimal::{OverpunchDialect, PackedSignPolicy};
    use coblang_hir::HirDisplayStyle;
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, decode_field_spec, encode_field_spec};

    let ops = sample_all_pic_edit_ops();
    let all_tys = sample_all_hir_types(&ops)?;
    let dialects = [
        OverpunchDialect::AsciiGnu,
        OverpunchDialect::Cobol85Standard,
        OverpunchDialect::EbcdicIbm,
        OverpunchDialect::MicroFocus,
    ];
    for (i, ty) in all_tys.iter().enumerate() {
        let spec = RuntimeFieldSpec {
            desc: MoveFieldDesc {
                ty: ty.clone(),
                overpunch: dialects
                    .get(i % dialects.len())
                    .copied()
                    .unwrap_or(OverpunchDialect::AsciiGnu),
                decimal_char: if i % 2 == 0 { b'.' } else { b',' },
                currency_symbol: '$',
                display_style: if i % 2 == 0 {
                    HirDisplayStyle::GnuPrettyWidth
                } else {
                    HirDisplayStyle::IsoPicWidth
                },
            },
            packed_sign_rule: if i % 2 == 0 {
                PackedSignPolicy::Gnu
            } else {
                PackedSignPolicy::Ibm
            },
        };
        let encoded = encode_field_spec(&spec);
        assert_eq!(decode_field_spec(&encoded)?, spec);
        // Every truncated prefix must fail cleanly without panicking.
        for cut in 0..encoded.len() {
            assert!(decode_field_spec(encoded.get(..cut).unwrap_or(&[])).is_err());
        }
    }

    // Invalid UTF-32 surrogate in header `currency_symbol` (`0xD800`).
    let mut bad_cur = vec![0_u8, b'.'];
    bad_cur.extend_from_slice(&0xD800_u32.to_le_bytes());
    bad_cur.push(9);
    assert!(decode_field_spec(&bad_cur).is_err());

    // Unknown HirType tag (`99`), unknown PicEditOp tag (`99`), and invalid currency char inside `PicEditOp`.
    let mut bad_ty = vec![0_u8, b'.'];
    bad_ty.extend_from_slice(&u32::from(b'$').to_le_bytes());
    bad_ty.push(99);
    assert!(decode_field_spec(&bad_ty).is_err());

    for bad_op_tag in [9_u8, 14, 99] {
        let mut bad_op = vec![0_u8, b'.'];
        bad_op.extend_from_slice(&u32::from(b'$').to_le_bytes());
        bad_op.push(2); // AlphanumericEdited
        bad_op.extend_from_slice(&1_u32.to_le_bytes());
        bad_op.extend_from_slice(&1_u16.to_le_bytes());
        bad_op.push(bad_op_tag);
        if bad_op_tag != 99 {
            bad_op.extend_from_slice(&0xD800_u32.to_le_bytes());
        }
        assert!(decode_field_spec(&bad_op).is_err());
    }

    // Invalid `DigitCount(0)` and invalid `ScaleFactor(99)` across numeric tags 3..=6.
    for tag in [3_u8, 4, 5, 6] {
        for bad_scale in [false, true] {
            let mut buf = vec![0_u8, b'.'];
            buf.extend_from_slice(&u32::from(b'$').to_le_bytes());
            buf.push(tag);
            if tag == 5 {
                buf.push(4); // byte_width
            } else if tag == 6 {
                buf.extend_from_slice(&4_u32.to_le_bytes()); // byte_len
            }
            buf.push(if bad_scale { 3 } else { 0 }); // digits
            buf.extend_from_slice(&(if bad_scale { 99_i16 } else { 0_i16 }).to_le_bytes());
            buf.extend_from_slice(&[0, 0, 0, 0]);
            assert!(decode_field_spec(&buf).is_err());
        }
    }
    Ok(())
}

/// Verifies `DecodedInitPlan` round-trips/truncations and `DataCategory` ABI encoding.
#[test]
fn desc_codec_init_plan_and_category_codes() -> Result<()> {
    use coblang_hir::DataCategory;
    use coblang_pic::{DefaultInitMode, FillerPolicy, InitCategoryMask, MoveFieldDesc};
    use coblang_rt_core::desc_codec::{
        DecodedInitPlan, FillerMembership, RedefinesMembership, RuntimeInitEntry,
        decode_data_category, decode_init_plan, encode_data_category, encode_init_plan,
    };

    let all_tys = sample_all_hir_types(&sample_all_pic_edit_ops())?;
    // `DecodedInitPlan` round-trip + every prefix truncation.
    for (fp, dm) in [
        (FillerPolicy::SkipFiller, DefaultInitMode::StandardCobol),
        (
            FillerPolicy::IncludeFiller,
            DefaultInitMode::IncludePointersAndBit,
        ),
    ] {
        let sample_ty = all_tys
            .first()
            .cloned()
            .unwrap_or(coblang_hir::HirType::Pointer);
        let plan = DecodedInitPlan {
            filler_policy: fp,
            to_value_mask: InitCategoryMask::ALL,
            default_mode: dm,
            entries: vec![
                RuntimeInitEntry {
                    offset: 0,
                    dims: Vec::new(),
                    byte_len: 2,
                    desc: MoveFieldDesc::standard_gnu(sample_ty.clone()),
                    filler: FillerMembership::NamedField,
                    redefines: RedefinesMembership::PrimaryStorage,
                    value_bytes: None,
                },
                RuntimeInitEntry {
                    offset: 2,
                    dims: vec![(2, 2)],
                    byte_len: 2,
                    desc: MoveFieldDesc::standard_gnu(sample_ty),
                    filler: FillerMembership::FillerField,
                    redefines: RedefinesMembership::RedefinesAlias,
                    value_bytes: Some(vec![b'A', b'B']),
                },
            ],
        };
        let pbytes = encode_init_plan(&plan);
        assert_eq!(decode_init_plan(&pbytes)?, plan);
        for cut in 0..pbytes.len() {
            assert!(decode_init_plan(pbytes.get(..cut).unwrap_or(&[])).is_err());
        }
    }

    // `DataCategory` ABI round-trip + invalid code.
    for cat in [
        DataCategory::Alphabetic,
        DataCategory::Alphanumeric,
        DataCategory::AlphanumericEdited,
        DataCategory::Numeric,
        DataCategory::NumericEdited,
        DataCategory::National,
        DataCategory::NationalEdited,
        DataCategory::Utf8,
        DataCategory::BooleanBit,
        DataCategory::Pointer,
        DataCategory::Index,
        DataCategory::ObjectReference,
        DataCategory::Group,
    ] {
        assert_eq!(decode_data_category(encode_data_category(cat))?, cat);
    }
    assert!(decode_data_category(99).is_err());
    Ok(())
}

/// Verifies `field_ops` ABI enum round-trips and out-of-range defaults.
#[test]
fn field_ops_abi_enums_coverage() {
    use coblang_decimal::RoundingMode;
    use coblang_rt_core::field_ops::{
        ClassTestKind, DecimalArithOp, NegZeroPolicy, RelCmpOp, RuntimeMoveSourceKind,
        rounding_from_abi_code, rounding_to_abi_code,
    };

    for kind in [
        RuntimeMoveSourceKind::Field,
        RuntimeMoveSourceKind::AlphanumericLiteral,
        RuntimeMoveSourceKind::FigZero,
        RuntimeMoveSourceKind::FigSpace,
        RuntimeMoveSourceKind::FigHigh,
        RuntimeMoveSourceKind::FigLow,
        RuntimeMoveSourceKind::FigQuote,
        RuntimeMoveSourceKind::FigNull,
        RuntimeMoveSourceKind::FigAll,
        RuntimeMoveSourceKind::DecimalAscii,
    ] {
        assert_eq!(
            RuntimeMoveSourceKind::from_abi_code(kind.to_abi_code()),
            kind
        );
    }
    assert_eq!(
        RuntimeMoveSourceKind::from_abi_code(99),
        RuntimeMoveSourceKind::AlphanumericLiteral
    );

    for op in [
        DecimalArithOp::Add,
        DecimalArithOp::Sub,
        DecimalArithOp::Mul,
        DecimalArithOp::Div,
        DecimalArithOp::Pow,
    ] {
        assert_eq!(DecimalArithOp::from_abi_code(op.to_abi_code()), op);
    }
    assert_eq!(DecimalArithOp::from_abi_code(99), DecimalArithOp::Add);

    for rel in [
        RelCmpOp::Eq,
        RelCmpOp::Ne,
        RelCmpOp::Lt,
        RelCmpOp::Le,
        RelCmpOp::Gt,
        RelCmpOp::Ge,
    ] {
        assert_eq!(RelCmpOp::from_abi_code(rel.to_abi_code()), rel);
        let _ = rel.matches_ordering(core::cmp::Ordering::Less);
        let _ = rel.matches_ordering(core::cmp::Ordering::Equal);
        let _ = rel.matches_ordering(core::cmp::Ordering::Greater);
    }
    assert_eq!(RelCmpOp::from_abi_code(99), RelCmpOp::Eq);

    for ck in [
        ClassTestKind::Numeric,
        ClassTestKind::Alphabetic,
        ClassTestKind::AlphabeticLower,
        ClassTestKind::AlphabeticUpper,
    ] {
        assert_eq!(ClassTestKind::from_abi_code(ck.to_abi_code()), ck);
    }
    assert_eq!(ClassTestKind::from_abi_code(99), ClassTestKind::Numeric);

    for nz in [
        NegZeroPolicy::NormalizePositiveZero,
        NegZeroPolicy::PreserveExistingNegZero,
    ] {
        assert_eq!(NegZeroPolicy::from_abi_code(nz.to_abi_code()), nz);
    }

    for rm in [
        RoundingMode::Truncation,
        RoundingMode::NearestAwayFromZero,
        RoundingMode::NearestEven,
        RoundingMode::NearestTowardZero,
        RoundingMode::Prohibited,
        RoundingMode::TowardGreater,
        RoundingMode::TowardLesser,
        RoundingMode::AwayFromZero,
    ] {
        assert_eq!(rounding_from_abi_code(rounding_to_abi_code(rm)), rm);
    }
    assert_eq!(rounding_from_abi_code(99), RoundingMode::Truncation);
}

/// Verifies `runtime_execute_move`, `runtime_compare_alphanumeric`, and `runtime_check_class`.
#[test]
fn field_ops_moves_comparisons_and_class_checks() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{BlankZeroSpec, HirType, JustifiedSpec, NumericSignSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::field_ops::{
        ClassTestKind, RelCmpOp, RuntimeMoveSourceKind, runtime_check_class,
        runtime_compare_alphanumeric, runtime_execute_move,
    };

    let alpha_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let zd_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(3)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });

    let mut dst4 = [0_u8; 4];
    for (k, p, sp, dp) in [
        (
            RuntimeMoveSourceKind::AlphanumericLiteral,
            b"HI".as_slice(),
            &[][..],
            &[][..],
        ),
        (
            RuntimeMoveSourceKind::Field,
            b"HI  ",
            &[],
            alpha_spec.as_slice(),
        ),
    ] {
        assert!(runtime_execute_move(k, p, sp, &mut dst4, dp).is_err());
    }
    runtime_execute_move(
        RuntimeMoveSourceKind::Field,
        b"ABCD",
        &alpha_spec,
        &mut dst4,
        &alpha_spec,
    )?;
    assert_eq!(&dst4, b"ABCD");

    for (kind, payload) in [
        (RuntimeMoveSourceKind::AlphanumericLiteral, b"XY".as_slice()),
        (RuntimeMoveSourceKind::FigZero, b""),
        (RuntimeMoveSourceKind::FigSpace, b""),
        (RuntimeMoveSourceKind::FigHigh, b""),
        (RuntimeMoveSourceKind::FigLow, b""),
        (RuntimeMoveSourceKind::FigQuote, b""),
        (RuntimeMoveSourceKind::FigNull, b""),
        (RuntimeMoveSourceKind::FigAll, b"AB"),
    ] {
        runtime_execute_move(kind, payload, &[], &mut dst4, &alpha_spec)?;
    }
    let mut dst3 = [0_u8; 3];
    runtime_execute_move(
        RuntimeMoveSourceKind::DecimalAscii,
        b"42",
        &[],
        &mut dst3,
        &zd_spec,
    )?;
    assert_eq!(&dst3, b"042");
    assert!(
        runtime_execute_move(
            RuntimeMoveSourceKind::DecimalAscii,
            &[0xFF],
            &[],
            &mut dst3,
            &zd_spec
        )
        .is_err()
    );
    assert!(
        runtime_execute_move(
            RuntimeMoveSourceKind::DecimalAscii,
            b"not-a-num",
            &[],
            &mut dst3,
            &zd_spec
        )
        .is_err()
    );
    let _ = (
        ClassTestKind::Numeric,
        RelCmpOp::Eq,
        runtime_check_class,
        runtime_compare_alphanumeric,
    );
    Ok(())
}

/// Verifies `runtime_compare_alphanumeric` and `runtime_check_class` across all operand kinds.
#[test]
fn field_ops_comparisons_and_class_checks_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{BlankZeroSpec, HirType, JustifiedSpec, NumericSignSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::field_ops::{
        ClassTestKind, RelCmpOp, RuntimeMoveSourceKind, runtime_check_class,
        runtime_compare_alphanumeric,
    };

    let alpha_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let zd_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(3)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });

    // Alphanumeric comparisons across all virtual-byte kinds.
    assert!(runtime_compare_alphanumeric(
        (RuntimeMoveSourceKind::AlphanumericLiteral, b"AB"),
        (RuntimeMoveSourceKind::Field, b"AB  "),
        RelCmpOp::Eq
    ));
    assert!(runtime_compare_alphanumeric(
        (RuntimeMoveSourceKind::DecimalAscii, b"00"),
        (RuntimeMoveSourceKind::FigZero, b""),
        RelCmpOp::Eq
    ));
    for k in [
        RuntimeMoveSourceKind::FigSpace,
        RuntimeMoveSourceKind::FigHigh,
        RuntimeMoveSourceKind::FigLow,
        RuntimeMoveSourceKind::FigNull,
        RuntimeMoveSourceKind::FigQuote,
        RuntimeMoveSourceKind::FigAll,
    ] {
        let _ = runtime_compare_alphanumeric(
            (k, b""),
            (RuntimeMoveSourceKind::FigAll, b"Z"),
            RelCmpOp::Lt,
        );
    }

    // Class checks.
    assert!(runtime_check_class(b"123", &[], ClassTestKind::Numeric).is_err());
    assert!(runtime_check_class(
        b"123",
        &zd_spec,
        ClassTestKind::Numeric
    )?);
    assert!(runtime_check_class(
        b"Ab c",
        &alpha_spec,
        ClassTestKind::Alphabetic
    )?);
    assert!(!runtime_check_class(
        b"Ab1c",
        &alpha_spec,
        ClassTestKind::Alphabetic
    )?);
    assert!(runtime_check_class(
        b"ab c",
        &alpha_spec,
        ClassTestKind::AlphabeticLower
    )?);
    assert!(!runtime_check_class(
        b"Ab c",
        &alpha_spec,
        ClassTestKind::AlphabeticLower
    )?);
    assert!(runtime_check_class(
        b"AB C",
        &alpha_spec,
        ClassTestKind::AlphabeticUpper
    )?);
    assert!(!runtime_check_class(
        b"Ab C",
        &alpha_spec,
        ClassTestKind::AlphabeticUpper
    )?);
    Ok(())
}

/// Verifies `DecimalEvalStack` arithmetic, sticky error latching, scale-limit errors, and `-0` preservation.
#[test]
fn decimal_eval_stack_arith_and_store_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::{HighOrderPolicy, PackedSignPolicy, RoundingMode, SizeErrorStatus};
    use coblang_hir::{BlankZeroSpec, Comp6Mode, HirType, NumericSignSpec, Signedness};
    use coblang_pic::{MoveFieldDesc, encode_zoned_negative_zero};
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::field_ops::{DecimalArithOp, DecimalEvalStack, NegZeroPolicy, RelCmpOp};

    let mut ctx = CobolRuntimeContext::new();
    ctx.decimal_stack_mut().begin();
    ctx.display_staging_mut().clear();
    ctx.init_staging_mut().clear();

    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    let zd_s2_desc = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: d2,
        scale: sc0,
        sign: NumericSignSpec::EmbeddedTrailing,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let zd_s2 = encode_field_spec(&RuntimeFieldSpec {
        desc: zd_s2_desc.clone(),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });

    let mut stack = DecimalEvalStack::new();
    stack.negate_top();
    assert!(stack.apply_binop(DecimalArithOp::Add).is_err());
    assert!(stack.compare_top_two(RelCmpOp::Eq).is_err());
    assert!(
        stack
            .store_top(
                &mut [0; 2],
                &zd_s2,
                RoundingMode::Truncation,
                HighOrderPolicy::ModuloTruncate,
                NegZeroPolicy::NormalizePositiveZero,
            )
            .is_err()
    );
    stack.push_ascii(b"10")?;
    assert!(stack.apply_binop(DecimalArithOp::Add).is_err());
    stack.push_ascii(b"10")?;
    assert!(stack.compare_top_two(RelCmpOp::Eq).is_err());
    assert!(stack.push_ascii(&[0xFF]).is_err());
    assert!(stack.push_ascii(b"bad").is_err());
    assert!(stack.push_field(b"12", &[]).is_err());
    let pd_bad = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::PackedDecimal {
            digits: d2,
            scale: sc0,
            sign: Signedness::Signed,
            unsigned_comp6: Comp6Mode::StandardComp3,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    assert!(stack.push_field(&[], &pd_bad).is_err());

    stack.begin();
    stack.push_field(b"20", &zd_s2)?;
    stack.push_ascii(b"5")?;
    stack.negate_top();
    stack.apply_binop(DecimalArithOp::Add)?;
    stack.push_ascii(b"3")?;
    stack.apply_binop(DecimalArithOp::Sub)?;
    stack.push_ascii(b"2")?;
    stack.apply_binop(DecimalArithOp::Mul)?;
    stack.push_ascii(b"4")?;
    stack.apply_binop(DecimalArithOp::Div)?;
    let mut out_b = [0_u8; 2];
    assert!(
        stack
            .store_top(
                &mut out_b,
                &[],
                RoundingMode::Truncation,
                HighOrderPolicy::ModuloTruncate,
                NegZeroPolicy::NormalizePositiveZero,
            )
            .is_err()
    );
    stack.begin();
    stack.push_ascii(b"6")?;
    stack.push_ascii(b"2")?;
    stack.apply_binop(DecimalArithOp::Pow)?;
    assert_eq!(
        stack.store_top(
            &mut out_b,
            &zd_s2,
            RoundingMode::Truncation,
            HighOrderPolicy::ModuloTruncate,
            NegZeroPolicy::NormalizePositiveZero,
        )?,
        SizeErrorStatus::Exact
    );
    assert_eq!(&out_b, b"36");
    stack.push_ascii(b"36")?;
    assert!(stack.compare_top_two(RelCmpOp::Eq)?);
    let _ = encode_zoned_negative_zero;
    Ok(())
}

/// Verifies `DecimalEvalStack` sticky error latching, scale-limit domain errors, and `-0` preservation.
#[test]
fn decimal_eval_stack_sticky_and_neg_zero_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::{HighOrderPolicy, PackedSignPolicy, RoundingMode, SizeErrorStatus};
    use coblang_hir::{BlankZeroSpec, HirType, NumericSignSpec};
    use coblang_pic::{MoveFieldDesc, encode_zoned_negative_zero};
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::field_ops::{DecimalArithOp, DecimalEvalStack, NegZeroPolicy};

    let zd_s2_desc = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: DigitCount::try_new(2)?,
        scale: ScaleFactor::ZERO,
        sign: NumericSignSpec::EmbeddedTrailing,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let zd_s2 = encode_field_spec(&RuntimeFieldSpec {
        desc: zd_s2_desc.clone(),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let mut stack = DecimalEvalStack::new();
    let mut out_b = [0_u8; 2];

    stack.begin();
    stack.push_ascii(b"10")?;
    stack.push_ascii(b"0")?;
    stack.apply_binop(DecimalArithOp::Div)?;
    stack.push_ascii(b"0")?;
    stack.apply_binop(DecimalArithOp::Pow)?;
    assert_eq!(
        stack.store_top(
            &mut out_b,
            &zd_s2,
            RoundingMode::Truncation,
            HighOrderPolicy::FlagOverflow,
            NegZeroPolicy::NormalizePositiveZero,
        )?,
        SizeErrorStatus::DivisionByZero
    );

    // Extreme `PIC 9P(38)` multiplication (`10^38 * 10^38 * 10^38 = 10^114`) latches `SizeErrorStatus::Overflowed`.
    let p38_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::ONE,
            scale: ScaleFactor::try_new(-38)?,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    stack.begin();
    stack.push_field(b"1", &p38_spec)?;
    stack.push_field(b"1", &p38_spec)?;
    stack.apply_binop(DecimalArithOp::Mul)?;
    stack.push_field(b"1", &p38_spec)?;
    stack.apply_binop(DecimalArithOp::Mul)?;
    assert_eq!(
        stack.store_top(
            &mut out_b,
            &zd_s2,
            RoundingMode::Truncation,
            HighOrderPolicy::FlagOverflow,
            NegZeroPolicy::NormalizePositiveZero,
        )?,
        SizeErrorStatus::Overflowed
    );
    let _ = encode_zoned_negative_zero;
    Ok(())
}

/// Verifies `NegZeroPolicy::PreserveExistingNegZero` across `-0`, `+0`, non-zero, and rounded stores.
#[test]
fn decimal_eval_stack_preserve_neg_zero_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::{HighOrderPolicy, PackedSignPolicy, RoundingMode, SizeErrorStatus};
    use coblang_hir::{BlankZeroSpec, HirType, NumericSignSpec};
    use coblang_pic::{MoveFieldDesc, encode_zoned_negative_zero};
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::field_ops::{DecimalEvalStack, NegZeroPolicy};

    let zd_s2_desc = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: DigitCount::try_new(2)?,
        scale: ScaleFactor::ZERO,
        sign: NumericSignSpec::EmbeddedTrailing,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let zd_s2 = encode_field_spec(&RuntimeFieldSpec {
        desc: zd_s2_desc.clone(),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let mut stack = DecimalEvalStack::new();
    stack.begin();
    stack.push_ascii(b"0")?;
    let mut nz_buf = [0_u8; 2];
    encode_zoned_negative_zero(&mut nz_buf, &zd_s2_desc);
    assert_eq!(
        stack.store_top(
            &mut nz_buf,
            &zd_s2,
            RoundingMode::Truncation,
            HighOrderPolicy::ModuloTruncate,
            NegZeroPolicy::PreserveExistingNegZero,
        )?,
        SizeErrorStatus::Exact
    );
    assert_eq!(&nz_buf, b"0p");
    // Positive zero (`"00"`) and non-zero (`5`) with `PreserveExistingNegZero`.
    let mut pos_zero = *b"00";
    assert_eq!(
        stack.store_top(
            &mut pos_zero,
            &zd_s2,
            RoundingMode::Truncation,
            HighOrderPolicy::ModuloTruncate,
            NegZeroPolicy::PreserveExistingNegZero,
        )?,
        SizeErrorStatus::Exact
    );
    stack.begin();
    stack.push_ascii(b"5")?;
    assert_eq!(
        stack.store_top(
            &mut nz_buf,
            &zd_s2,
            RoundingMode::Truncation,
            HighOrderPolicy::ModuloTruncate,
            NegZeroPolicy::PreserveExistingNegZero,
        )?,
        SizeErrorStatus::Exact
    );
    assert_eq!(
        stack.store_top(
            &mut nz_buf,
            &zd_s2,
            RoundingMode::NearestAwayFromZero,
            HighOrderPolicy::ModuloTruncate,
            NegZeroPolicy::PreserveExistingNegZero,
        )?,
        SizeErrorStatus::Exact
    );
    assert_ne!(&nz_buf, b"0p");
    Ok(())
}

/// Verifies `DecimalEvalStack::divide_remainder` across all quotient capacities and overflow/zero-divide paths.
#[test]
fn decimal_eval_stack_divide_remainder_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::{HighOrderPolicy, PackedSignPolicy, RoundingMode, SizeErrorStatus};
    use coblang_hir::{
        BinaryEndianness, BlankZeroSpec, HirType, NumericSignSpec, Signedness, TruncationPolicy,
    };
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::field_ops::{DecimalArithOp, DecimalEvalStack};

    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    let zd_s2_vec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: d2,
            scale: sc0,
            sign: NumericSignSpec::EmbeddedTrailing,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let bad_bin0_vec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::BinaryInt {
            byte_width: 0,
            digits: DigitCount::ONE,
            scale: sc0,
            sign: Signedness::Unsigned,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::TruncateToPic,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let z2 = zd_s2_vec.as_slice();
    let bad0 = bad_bin0_vec.as_slice();
    let mut stack = DecimalEvalStack::new();
    let rm = RoundingMode::Truncation;
    let fo = HighOrderPolicy::FlagOverflow;
    let mt = HighOrderPolicy::ModuloTruncate;

    stack.begin();
    assert!(
        stack
            .divide_remainder((b"00", z2, rm), (b"00", z2), fo)
            .is_err()
    );
    stack.push_ascii(b"10")?;
    assert!(
        stack
            .divide_remainder((b"00", z2, rm), (b"00", z2), fo)
            .is_err()
    );
    stack.begin();
    stack.push_ascii(b"1")?;
    stack.push_ascii(b"0")?;
    stack.apply_binop(DecimalArithOp::Div)?;
    stack.push_ascii(b"2")?;
    assert_eq!(
        stack
            .divide_remainder((b"00", z2, rm), (b"00", z2), fo)?
            .status,
        SizeErrorStatus::DivisionByZero
    );

    stack.begin();
    let b00 = b"00".as_slice();
    let empty: &[u8] = &[];
    for (num, q_b, q_s, r_b, r_s, ho) in [
        (b"10".as_slice(), b00, empty, b00, z2, fo),
        (b"10", b00, z2, b00, empty, fo),
        (b"10", b00, bad0, b00, z2, fo),
        (b"10", b00, z2, b00, bad0, fo),
        (b"50", b00, bad0, b00, z2, mt),
    ] {
        stack.push_ascii(num)?;
        stack.push_ascii(b"2")?;
        assert!(
            stack
                .divide_remainder((q_b, q_s, rm), (r_b, r_s), ho)
                .is_err()
        );
    }
    Ok(())
}

/// Verifies `DecimalEvalStack::divide_remainder` overflow, `Prohibited` rounding, and zero-divide status.
#[test]
fn decimal_eval_stack_div_rem_overflows_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::{HighOrderPolicy, PackedSignPolicy, RoundingMode, SizeErrorStatus};
    use coblang_hir::{
        BinaryEndianness, BlankZeroSpec, HirType, NumericSignSpec, Signedness, TruncationPolicy,
    };
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::field_ops::DecimalEvalStack;

    let zd_s2_vec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(2)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::EmbeddedTrailing,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let one_digit_vec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::ONE,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let i8_c5_vec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::BinaryInt {
            byte_width: 1,
            digits: DigitCount::try_new(2)?,
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let zd_s2 = zd_s2_vec.as_slice();
    let one_digit = one_digit_vec.as_slice();
    let i8_c5 = i8_c5_vec.as_slice();
    let mut stack = DecimalEvalStack::new();

    for ho in [
        HighOrderPolicy::ModuloTruncate,
        HighOrderPolicy::FlagOverflow,
    ] {
        stack.begin();
        stack.push_ascii(b"50")?;
        stack.push_ascii(b"2")?;
        assert_eq!(
            stack
                .divide_remainder(
                    (b"0", one_digit, RoundingMode::Truncation),
                    (b"00", zd_s2),
                    ho
                )?
                .status,
            SizeErrorStatus::Overflowed
        );
        stack.begin();
        stack.push_ascii(b"3")?;
        stack.push_ascii(b"2")?;
        assert_eq!(
            stack
                .divide_remainder((b"00", zd_s2, RoundingMode::Prohibited), (b"00", zd_s2), ho)?
                .status,
            SizeErrorStatus::Overflowed
        );
        // `200 / 1` into 1-byte signed `COMP-5` passes 3-digit capacity (`200 <= 999`) then overflows `i8::MAX` on `FlagOverflow`.
        stack.begin();
        stack.push_ascii(b"200")?;
        stack.push_ascii(b"1")?;
        let q_c5 =
            stack.divide_remainder((&[0], i8_c5, RoundingMode::Truncation), (b"00", zd_s2), ho)?;
        assert_eq!(q_c5.status.is_error(), ho == HighOrderPolicy::FlagOverflow);
        // `179 / 100` (`q = 1`, `rem = 79`) overflows 1-digit remainder on `FlagOverflow`.
        stack.begin();
        stack.push_ascii(b"179")?;
        stack.push_ascii(b"100")?;
        let r_ov = stack.divide_remainder(
            (b"00", zd_s2, RoundingMode::Truncation),
            (b"0", one_digit),
            ho,
        )?;
        assert_eq!(r_ov.status.is_error(), ho == HighOrderPolicy::FlagOverflow);
    }
    stack.begin();
    stack.push_ascii(b"10")?;
    stack.push_ascii(b"0")?;
    assert_eq!(
        stack
            .divide_remainder(
                (b"00", zd_s2, RoundingMode::Truncation),
                (b"00", zd_s2),
                HighOrderPolicy::ModuloTruncate,
            )?
            .status,
        SizeErrorStatus::DivisionByZero
    );
    Ok(())
}

/// Verifies `DecimalEvalStack::divide_remainder` quotient capacities across Packed, `NumericEdited`, Binary, and Float.
#[test]
fn decimal_eval_stack_div_rem_capacities_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::{HighOrderPolicy, PackedSignPolicy, RoundingMode, SizeErrorStatus};
    use coblang_hir::{
        BinaryEndianness, BlankZeroSpec, Comp6Mode, FloatBinaryWidth, HirType, NumericSignSpec,
        PicEditOp, Signedness, TruncationPolicy,
    };
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::field_ops::DecimalEvalStack;

    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    let zd_s2_vec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: d2,
            scale: sc0,
            sign: NumericSignSpec::EmbeddedTrailing,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let zd_s2 = zd_s2_vec.as_slice();
    let mut stack = DecimalEvalStack::new();

    let mut cap_tys = vec![
        (
            HirType::PackedDecimal {
                digits: d2,
                scale: sc0,
                sign: Signedness::Signed,
                unsigned_comp6: Comp6Mode::StandardComp3,
            },
            2_usize,
        ),
        (
            HirType::PackedDecimal {
                digits: d2,
                scale: sc0,
                sign: Signedness::Unsigned,
                unsigned_comp6: Comp6Mode::StandardComp3,
            },
            2,
        ),
        (
            HirType::NumericEdited {
                byte_len: 2,
                digits: d2,
                scale: sc0,
                sign_capability: Signedness::Unsigned,
                edit_ops: vec![PicEditOp::DigitSlot, PicEditOp::DigitSlot],
                blank_zero: BlankZeroSpec::RetainZero,
            },
            2,
        ),
        (
            HirType::BinaryInt {
                byte_width: 2,
                digits: d2,
                scale: sc0,
                sign: Signedness::Unsigned,
                endian: BinaryEndianness::Native,
                trunc: TruncationPolicy::TruncateToPic,
            },
            2,
        ),
        (
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits64,
            },
            8,
        ),
    ];
    for bw in [1_u8, 2, 3, 4, 6, 8, 16] {
        cap_tys.push((
            HirType::BinaryInt {
                byte_width: bw,
                digits: d2,
                scale: sc0,
                sign: Signedness::Signed,
                endian: BinaryEndianness::Native,
                trunc: TruncationPolicy::FullBinaryRange,
            },
            usize::from(bw),
        ));
    }
    for (ty, qlen) in cap_tys {
        let qspec = encode_field_spec(&RuntimeFieldSpec {
            desc: MoveFieldDesc::standard_gnu(ty),
            packed_sign_rule: PackedSignPolicy::Gnu,
        });
        let qbuf = vec![0_u8; qlen];
        stack.begin();
        stack.push_ascii(b"7")?;
        stack.push_ascii(b"2")?;
        let ok = stack.divide_remainder(
            (&qbuf, &qspec, RoundingMode::Truncation),
            (b"00", zd_s2),
            HighOrderPolicy::FlagOverflow,
        )?;
        assert_eq!(ok.status, SizeErrorStatus::Exact);
    }
    Ok(())
}

/// Verifies `DisplayStagingBuffer` and `InitStagingState` across valid plans, table strides, and error paths.
#[test]
fn display_staging_and_init_staging_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{BlankZeroSpec, DataCategory, HirType, JustifiedSpec, NumericSignSpec};
    use coblang_pic::{DefaultInitMode, FillerPolicy, InitCategoryMask, MoveFieldDesc};
    use coblang_rt_core::desc_codec::{
        DecodedInitPlan, FillerMembership, RedefinesMembership, RuntimeFieldSpec, RuntimeInitEntry,
        encode_field_spec, encode_init_plan,
    };
    use coblang_rt_core::field_ops::{
        DisplayStagingBuffer, InitStagingState, RuntimeMoveSourceKind,
    };

    let zd_s2_vec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(2)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::EmbeddedTrailing,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let zd_s2 = zd_s2_vec.as_slice();

    // `DisplayStagingBuffer`.
    let mut disp = DisplayStagingBuffer::new();
    let mut out = Vec::new();
    disp.flush_to(LineAdvancing::NoAdvancing, &mut out)?;
    assert_eq!(out.len(), 0);
    disp.push_raw(b"A=");
    disp.push_field(b"42", zd_s2)?;
    assert!(disp.push_field(b"42", &[]).is_err());
    disp.push_hex_of(&[0x0A, 0xBC]);
    disp.flush_to(LineAdvancing::AdvanceLine, &mut out)?;
    assert_eq!(out, b"A=+420ABC\n");

    disp.push_raw(b"ERR");
    assert!(
        disp.flush_to(
            LineAdvancing::AdvanceLine,
            &mut FailingWriter {
                fail_on_write_call: Some(0),
                fail_on_flush: false,
                writes: 0,
            }
        )
        .is_err()
    );
    assert!(
        disp.flush_to(
            LineAdvancing::AdvanceLine,
            &mut FailingWriter {
                fail_on_write_call: Some(1),
                fail_on_flush: false,
                writes: 0,
            }
        )
        .is_err()
    );
    disp.clear();
    assert!(
        disp.flush_to(
            LineAdvancing::NoAdvancing,
            &mut FailingWriter {
                fail_on_write_call: None,
                fail_on_flush: true,
                writes: 0,
            }
        )
        .is_err()
    );
    let _ = (
        DataCategory::Alphanumeric,
        JustifiedSpec::Left,
        DefaultInitMode::StandardCobol,
        FillerPolicy::IncludeFiller,
        InitCategoryMask::NONE,
        DecodedInitPlan {
            filler_policy: FillerPolicy::SkipFiller,
            to_value_mask: InitCategoryMask::NONE,
            default_mode: DefaultInitMode::StandardCobol,
            entries: Vec::new(),
        },
        FillerMembership::NamedField,
        RedefinesMembership::PrimaryStorage,
        RuntimeInitEntry {
            offset: 0,
            dims: Vec::new(),
            byte_len: 1,
            desc: MoveFieldDesc::standard_gnu(HirType::Pointer),
            filler: FillerMembership::NamedField,
            redefines: RedefinesMembership::PrimaryStorage,
            value_bytes: None,
        },
        encode_init_plan,
        InitStagingState::new(),
        RuntimeMoveSourceKind::Field,
    );
    Ok(())
}

/// Verifies `InitStagingState` across all replacement kinds, table strides, and out-of-bounds errors.
#[test]
fn init_staging_state_coverage() -> Result<()> {
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{DataCategory, HirType, JustifiedSpec};
    use coblang_pic::{DefaultInitMode, FillerPolicy, InitCategoryMask, MoveFieldDesc};
    use coblang_rt_core::desc_codec::{
        DecodedInitPlan, FillerMembership, RedefinesMembership, RuntimeFieldSpec, RuntimeInitEntry,
        encode_field_spec, encode_init_plan,
    };
    use coblang_rt_core::field_ops::{InitStagingState, RuntimeMoveSourceKind};

    // `InitStagingState` across all replacement kinds, scalar/table/pointer entries, and error paths.
    let mut init_st = InitStagingState::new();
    for (cat, k, p) in [
        (
            DataCategory::Alphanumeric,
            RuntimeMoveSourceKind::Field,
            vec![b'X'],
        ),
        (
            DataCategory::Numeric,
            RuntimeMoveSourceKind::DecimalAscii,
            vec![0xFF],
        ),
        (
            DataCategory::Numeric,
            RuntimeMoveSourceKind::DecimalAscii,
            b"nope".to_vec(),
        ),
    ] {
        assert!(init_st.push_replacement(cat, k, p, &[]).is_err());
    }

    let alpha2_desc = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    let alpha2_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: alpha2_desc.clone(),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    push_all_sample_replacements(&mut init_st, &alpha2_spec)?;

    let plan_ok = encode_init_plan(&DecodedInitPlan {
        filler_policy: FillerPolicy::IncludeFiller,
        to_value_mask: InitCategoryMask::NONE,
        default_mode: DefaultInitMode::StandardCobol,
        entries: vec![
            RuntimeInitEntry {
                offset: 0,
                dims: Vec::new(),
                byte_len: 2,
                desc: alpha2_desc.clone(),
                filler: FillerMembership::NamedField,
                redefines: RedefinesMembership::PrimaryStorage,
                value_bytes: None,
            },
            RuntimeInitEntry {
                offset: 2,
                dims: vec![(2, 2)],
                byte_len: 2,
                desc: MoveFieldDesc::standard_gnu(HirType::Alphabetic {
                    byte_len: 2,
                    justified: JustifiedSpec::Left,
                }),
                filler: FillerMembership::FillerField,
                redefines: RedefinesMembership::PrimaryStorage,
                value_bytes: None,
            },
            RuntimeInitEntry {
                offset: 6,
                dims: vec![(2, 2)],
                byte_len: 2,
                desc: MoveFieldDesc::standard_gnu(HirType::Index),
                filler: FillerMembership::NamedField,
                redefines: RedefinesMembership::PrimaryStorage,
                value_bytes: None,
            },
        ],
    });
    let mut target_buf = *b"??????????";
    assert!(init_st.execute_plan(&mut target_buf, &[]).is_err());
    init_st.execute_plan(&mut target_buf, &plan_ok)?;
    assert_eq!(&target_buf[..6], b"CDABAB");
    // Unmatched `Index` occurrences stay `????` (`pattern == before_first` shortcut!).
    assert_eq!(&target_buf[6..], b"????");

    // Out-of-bounds first element and out-of-bounds table stride errors.
    assert!(init_st.execute_plan(&mut [0_u8; 1], &plan_ok).is_err());
    assert!(init_st.execute_plan(&mut [0_u8; 5], &plan_ok).is_err());
    let _ = DefaultInitMode::IncludePointersAndBit;
    Ok(())
}

/// Verifies `SkipFiller`, `RedefinesAlias`, `IncludePointersAndBit` 2D `Index` splatting, and 2D stride errors.
#[test]
fn init_staging_skip_filler_and_2d_index_coverage() -> Result<()> {
    use coblang_hir::{HirType, JustifiedSpec};
    use coblang_pic::{DefaultInitMode, FillerPolicy, InitCategoryMask, MoveFieldDesc};
    use coblang_rt_core::desc_codec::{
        DecodedInitPlan, FillerMembership, RedefinesMembership, RuntimeInitEntry, encode_init_plan,
    };
    use coblang_rt_core::field_ops::InitStagingState;

    let mut init_st = InitStagingState::new();
    let alpha2_desc = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    let skip_plan = encode_init_plan(&DecodedInitPlan {
        filler_policy: FillerPolicy::SkipFiller,
        to_value_mask: InitCategoryMask::NONE,
        default_mode: DefaultInitMode::IncludePointersAndBit,
        entries: vec![
            RuntimeInitEntry {
                offset: 0,
                dims: Vec::new(),
                byte_len: 2,
                desc: alpha2_desc.clone(),
                filler: FillerMembership::FillerField,
                redefines: RedefinesMembership::PrimaryStorage,
                value_bytes: None,
            },
            RuntimeInitEntry {
                offset: 0,
                dims: Vec::new(),
                byte_len: 2,
                desc: alpha2_desc.clone(),
                filler: FillerMembership::NamedField,
                redefines: RedefinesMembership::RedefinesAlias,
                value_bytes: None,
            },
            RuntimeInitEntry {
                offset: 0,
                dims: vec![(2, 4), (2, 2)],
                byte_len: 2,
                desc: MoveFieldDesc::standard_gnu(HirType::Index),
                filler: FillerMembership::NamedField,
                redefines: RedefinesMembership::PrimaryStorage,
                value_bytes: None,
            },
        ],
    });
    let mut z8 = [0_u8; 8];
    init_st.execute_plan(&mut z8, &skip_plan)?;
    assert!(init_st.execute_plan(&mut [0_u8; 3], &skip_plan).is_err());
    Ok(())
}

/// Pushes all 10 `RuntimeMoveSourceKind` replacement variants into `init_st`.
///
/// # Arguments
///
/// * `init_st` - Target `InitStagingState`.
/// * `alpha2_spec` - Encoded `PIC X(2)` field descriptor.
fn push_all_sample_replacements(
    init_st: &mut coblang_rt_core::field_ops::InitStagingState,
    alpha2_spec: &[u8],
) -> Result<()> {
    use coblang_hir::DataCategory;
    use coblang_rt_core::field_ops::RuntimeMoveSourceKind;

    init_st.push_replacement(
        DataCategory::Alphabetic,
        RuntimeMoveSourceKind::Field,
        b"AB".to_vec(),
        alpha2_spec,
    )?;
    for (cat, kind, payload) in [
        (
            DataCategory::Alphanumeric,
            RuntimeMoveSourceKind::AlphanumericLiteral,
            b"CD".to_vec(),
        ),
        (
            DataCategory::AlphanumericEdited,
            RuntimeMoveSourceKind::FigZero,
            Vec::new(),
        ),
        (
            DataCategory::National,
            RuntimeMoveSourceKind::FigSpace,
            Vec::new(),
        ),
        (
            DataCategory::NationalEdited,
            RuntimeMoveSourceKind::FigHigh,
            Vec::new(),
        ),
        (
            DataCategory::Utf8,
            RuntimeMoveSourceKind::FigLow,
            Vec::new(),
        ),
        (
            DataCategory::Group,
            RuntimeMoveSourceKind::FigQuote,
            Vec::new(),
        ),
        (
            DataCategory::Pointer,
            RuntimeMoveSourceKind::FigNull,
            Vec::new(),
        ),
        (
            DataCategory::BooleanBit,
            RuntimeMoveSourceKind::FigAll,
            vec![b'1'],
        ),
        (
            DataCategory::Numeric,
            RuntimeMoveSourceKind::DecimalAscii,
            b"77".to_vec(),
        ),
    ] {
        init_st.push_replacement(cat, kind, payload, &[])?;
    }
    Ok(())
}

/// Verifies M8 switch states, collated alphanumeric comparisons, and custom class bitmaps.
#[test]
fn m8_switches_collated_cmp_and_custom_class_coverage() {
    use coblang_rt_core::field_ops::{
        AlphaCmpOperand, RelCmpOp, RuntimeMoveSourceKind, runtime_check_custom_class,
        runtime_compare_alphanumeric_ex,
    };
    use coblang_rt_core::{CobolRuntimeContext, SwitchState, parse_env_switch_state};

    assert_eq!(SwitchState::from_abi_code(0), SwitchState::Off);
    assert_eq!(SwitchState::from_abi_code(7), SwitchState::On);
    assert_eq!(SwitchState::Off.to_abi_code(), 0);
    assert_eq!(SwitchState::On.to_abi_code(), 1);
    assert_eq!(parse_env_switch_state(" yes "), SwitchState::On);
    assert_eq!(parse_env_switch_state("0"), SwitchState::Off);

    let mut ctx = CobolRuntimeContext::new();
    ctx.initialize_with_env(Vec::new(), |key| match key {
        "COB_SWITCH_1" => Some(String::from("ON")),
        "COB_SWITCH_2" => Some(String::from("OFF")),
        _ => None,
    });
    assert_eq!(ctx.get_switch(1), SwitchState::On);
    assert_eq!(ctx.get_switch(2), SwitchState::Off);
    assert_eq!(ctx.get_switch(99), SwitchState::Off);
    ctx.set_switch(2, SwitchState::On);
    assert_eq!(ctx.get_switch(2), SwitchState::On);
    ctx.set_switch(2, SwitchState::Off);
    assert_eq!(ctx.get_switch(2), SwitchState::Off);
    ctx.set_switch(99, SwitchState::On);

    let mut ranks = [0_u8; 256];
    for (i, slot) in ranks.iter_mut().enumerate() {
        *slot = match i {
            65 => 2,
            66 => 1,
            other => u8::try_from(other).unwrap_or(0),
        };
    }
    assert!(runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::AlphanumericLiteral,
            bytes: b"B",
            spec_bytes: &[],
        },
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::AlphanumericLiteral,
            bytes: b"A",
            spec_bytes: &[],
        },
        RelCmpOp::Lt,
        Some(&ranks),
    ));

    let mut bm = [0_u8; 32];
    let mut dummy = 0_u8;
    *bm.get_mut(usize::from(b'X' >> 3)).unwrap_or(&mut dummy) |= 1_u8 << (b'X' & 7);
    assert!(runtime_check_custom_class(b"XXX", &bm));
    assert!(!runtime_check_custom_class(b"XXY", &bm));
}

/// Verifies M8 numeric-to-alphanumeric comparison coercion (`BinaryInt`, `Index`, and `ZonedDecimal` signs).
#[test]
fn m8_numeric_to_alphanumeric_cmp_coercion_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{
        BinaryEndianness, BlankZeroSpec, HirType, JustifiedSpec, NumericSignSpec, Signedness,
        TruncationPolicy,
    };
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::field_ops::{
        AlphaCmpOperand, RelCmpOp, RuntimeMoveSourceKind, runtime_compare_alphanumeric_ex,
    };

    let mk_spec = |ty: HirType| {
        encode_field_spec(&RuntimeFieldSpec {
            desc: MoveFieldDesc::standard_gnu(ty),
            packed_sign_rule: PackedSignPolicy::Gnu,
        })
    };
    let bin_narrow = mk_spec(HirType::BinaryInt {
        byte_width: 2,
        digits: DigitCount::ONE,
        scale: ScaleFactor::ZERO,
        sign: Signedness::Unsigned,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::FullBinaryRange,
    });
    let bin_wide = mk_spec(HirType::BinaryInt {
        byte_width: 2,
        digits: DigitCount::try_new(4)?,
        scale: ScaleFactor::ZERO,
        sign: Signedness::Unsigned,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::FullBinaryRange,
    });
    let idx_spec = mk_spec(HirType::Index);
    let alpha_spec = mk_spec(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    let b25 = 25_u16.to_ne_bytes();
    let i7 = 7_i64.to_ne_bytes();
    for (raw, sp, exp) in [
        (b25.as_slice(), bin_narrow.as_slice(), b"5".as_slice()),
        (&b25, &bin_wide, b"0025"),
        (&i7, &idx_spec, b"0000000007"),
        (b"XY", &alpha_spec, b"XY"),
    ] {
        assert!(runtime_compare_alphanumeric_ex(
            &AlphaCmpOperand {
                kind: RuntimeMoveSourceKind::Field,
                bytes: raw,
                spec_bytes: sp,
            },
            &AlphaCmpOperand {
                kind: RuntimeMoveSourceKind::AlphanumericLiteral,
                bytes: exp,
                spec_bytes: &[],
            },
            RelCmpOp::Eq,
            None,
        ));
    }

    for (sign, raw, exp) in [
        (
            NumericSignSpec::Unsigned,
            b"0 0".as_slice(),
            b"0 0".as_slice(),
        ),
        (NumericSignSpec::SeparateLeading, b"-42", b"42"),
        (NumericSignSpec::SeparateTrailing, b"42+", b"42"),
        (NumericSignSpec::EmbeddedLeading, b"{9", b"09"),
        (NumericSignSpec::EmbeddedLeading, b"", b""),
        (NumericSignSpec::EmbeddedTrailing, b"1A", b"11"),
        (NumericSignSpec::EmbeddedTrailing, b"1J", b"11"),
        (NumericSignSpec::EmbeddedTrailing, b"1q", b"11"),
        (NumericSignSpec::EmbeddedTrailing, b"1p", b"10"),
        (NumericSignSpec::EmbeddedTrailing, b"15", b"15"),
        (NumericSignSpec::EmbeddedTrailing, b"", b""),
    ] {
        let z_spec = mk_spec(HirType::ZonedDecimal {
            digits: DigitCount::try_new(2)?,
            scale: ScaleFactor::ZERO,
            sign,
            blank_zero: BlankZeroSpec::RetainZero,
        });
        assert!(runtime_compare_alphanumeric_ex(
            &AlphaCmpOperand {
                kind: RuntimeMoveSourceKind::Field,
                bytes: raw,
                spec_bytes: &z_spec,
            },
            &AlphaCmpOperand {
                kind: RuntimeMoveSourceKind::AlphanumericLiteral,
                bytes: exp,
                spec_bytes: &[],
            },
            RelCmpOp::Eq,
            None,
        ));
    }
    Ok(())
}

/// Exercises M9 runtime bounds evaluators (`EC-BOUND-ODO`, `EC-BOUND-SUBSCRIPT`, `EC-BOUND-REF-MOD`) and exception state.
#[test]
fn m9_bounds_evaluators_coverage() {
    use coblang_rt_core::field_ops::{
        BoundCheckDisposition, RefModLengthArg, RuntimeBoundFailureMode, evaluate_odo_bound,
        evaluate_refmod_bound, evaluate_subscript_bound,
    };

    let mut rt = CobolRuntimeContext::new();
    assert_eq!(rt.last_exception(), "");
    rt.record_exception("EC-BOUND-SUBSCRIPT");
    assert_eq!(rt.last_exception(), "EC-BOUND-SUBSCRIPT");

    let mut err_buf = Vec::new();
    let rec = RuntimeBoundFailureMode::RecordOnly;
    let fat = RuntimeBoundFailureMode::FatalAbort;
    assert_eq!(
        evaluate_odo_bound(2, 1, 5, rec, "t:1", &mut err_buf),
        BoundCheckDisposition::Pass
    );
    assert_eq!(
        evaluate_odo_bound(0, 1, 5, rec, "t:1", &mut err_buf),
        BoundCheckDisposition::RecordedNonFatal("EC-BOUND-ODO")
    );
    assert_eq!(
        evaluate_odo_bound(6, 1, 5, fat, "t:2", &mut err_buf),
        BoundCheckDisposition::FatalAbortExitOne("EC-BOUND-ODO")
    );
    assert!(err_buf.len() > 5);

    assert_eq!(
        evaluate_subscript_bound(3, 5, rec, "t:3", &mut err_buf),
        BoundCheckDisposition::Pass
    );
    assert_eq!(
        evaluate_subscript_bound(0, 5, rec, "t:3", &mut err_buf),
        BoundCheckDisposition::RecordedNonFatal("EC-BOUND-SUBSCRIPT")
    );
    assert_eq!(
        evaluate_subscript_bound(6, 5, fat, "t:3", &mut err_buf),
        BoundCheckDisposition::FatalAbortExitOne("EC-BOUND-SUBSCRIPT")
    );

    for (off, len_arg, mode, exp) in [
        (
            2,
            RefModLengthArg::Omitted,
            rec,
            BoundCheckDisposition::Pass,
        ),
        (
            2,
            RefModLengthArg::Explicit(3),
            rec,
            BoundCheckDisposition::Pass,
        ),
        (
            0,
            RefModLengthArg::Omitted,
            rec,
            BoundCheckDisposition::RecordedNonFatal("EC-BOUND-REF-MOD"),
        ),
        (
            5,
            RefModLengthArg::Omitted,
            fat,
            BoundCheckDisposition::FatalAbortExitOne("EC-BOUND-REF-MOD"),
        ),
        (
            2,
            RefModLengthArg::Explicit(0),
            rec,
            BoundCheckDisposition::RecordedNonFatal("EC-BOUND-REF-MOD"),
        ),
        (
            2,
            RefModLengthArg::Explicit(4),
            rec,
            BoundCheckDisposition::RecordedNonFatal("EC-BOUND-REF-MOD"),
        ),
    ] {
        assert_eq!(
            evaluate_refmod_bound(4, off, len_arg, mode, "t:4", &mut err_buf),
            exp
        );
    }
}

/// Exercises M9 `DecodedTableSortPlan` encoding/decoding and `runtime_sort_table` execution across key categories.
#[test]
fn m9_table_sort_runtime_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{BlankZeroSpec, HirSortDirection, HirType, JustifiedSpec, NumericSignSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{
        DecodedTableSortPlan, RuntimeFieldSpec, RuntimeTableSortKey, decode_table_sort_plan,
        encode_table_sort_plan,
    };
    use coblang_rt_core::field_ops::runtime_sort_table;

    let alpha_spec = RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 1,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    };
    let num_spec = RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(2)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    };
    let idx_spec = RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Index),
        packed_sign_rule: PackedSignPolicy::Gnu,
    };

    let empty_plan_bytes = encode_table_sort_plan(&DecodedTableSortPlan {
        stride: 3,
        collating_ranks: None,
        keys: Vec::new(),
    });
    assert_eq!(decode_table_sort_plan(&empty_plan_bytes)?.keys.len(), 0);
    let mut small = *b"CCBBAA";
    runtime_sort_table(&mut small, 1, &empty_plan_bytes)?;
    runtime_sort_table(&mut small, 2, &empty_plan_bytes)?;
    assert!(runtime_sort_table(&mut small, 3, &empty_plan_bytes).is_err());

    let mut rev_ranks = [0_u8; 256];
    for (i, slot) in rev_ranks.iter_mut().enumerate() {
        *slot = 255_u8.saturating_sub(u8::try_from(i).unwrap_or(0));
    }
    let ranked_bytes = encode_table_sort_plan(&DecodedTableSortPlan {
        stride: 3,
        collating_ranks: Some(rev_ranks),
        keys: vec![
            RuntimeTableSortKey {
                direction: HirSortDirection::Ascending,
                rel_offset: 0,
                byte_len: 1,
                spec: alpha_spec.clone(),
            },
            RuntimeTableSortKey {
                direction: HirSortDirection::Descending,
                rel_offset: 1,
                byte_len: 2,
                spec: num_spec,
            },
        ],
    });
    let mut tbl = *b"A10B05A20A20";
    runtime_sort_table(&mut tbl, 4, &ranked_bytes)?;
    assert_eq!(&tbl, b"B05A20A20A10");
    let k_alpha = RuntimeTableSortKey {
        direction: HirSortDirection::Ascending,
        rel_offset: 0,
        byte_len: 2,
        spec: alpha_spec,
    };
    let k_idx = RuntimeTableSortKey {
        direction: HirSortDirection::Ascending,
        rel_offset: 0,
        byte_len: 8,
        spec: idx_spec,
    };
    let plain_alpha_bytes = encode_table_sort_plan(&DecodedTableSortPlan {
        stride: 8,
        collating_ranks: None,
        keys: vec![k_alpha, k_idx],
    });
    let mut plain_buf = *b"BB000000AA000000";
    runtime_sort_table(&mut plain_buf, 2, &plain_alpha_bytes)?;
    assert_eq!(&plain_buf, b"AA000000BB000000");

    for bad in [
        &[1_u8, 0, 0][..],
        &[1, 0, 0, 0],
        &[1, 0, 0, 0, 1, 99],
        &[1, 0, 0, 0, 2],
        &[1, 0, 0, 0, 0, 1],
        &[1, 0, 0, 0, 0, 1, 0],
        &[1, 0, 0, 0, 0, 1, 0, 9],
        &[1, 0, 0, 0, 0, 1, 0, 0, 1, 2],
        &[1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1],
        &[1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0],
    ] {
        assert!(decode_table_sort_plan(bad).is_err());
    }
    Ok(())
}

/// Exercises M10 `CobolDateTime` calendar math, ABI code round-trips, and `parse_current_date_override`.
#[test]
fn m10_datetime_and_override_parser_coverage() {
    use coblang_rt_core::accept_env::{
        AcceptFlowOutcome, CobolDateTime, EnvDisplayTarget, RuntimeAcceptSource,
        parse_current_date_override,
    };

    assert_eq!(AcceptFlowOutcome::Succeeded.to_abi_code(), 0);
    assert_eq!(AcceptFlowOutcome::Exception.to_abi_code(), 1);
    for code in 0..=14 {
        let src = RuntimeAcceptSource::from_abi_code(code);
        let expected = if code <= 13 { code } else { 0 };
        assert_eq!(src.to_abi_code(), expected);
    }
    for code in 0..=4 {
        let t = EnvDisplayTarget::from_abi_code(code);
        assert_eq!(t.to_abi_code(), if code <= 3 { code } else { 0 });
    }

    assert!(CobolDateTime::is_leap_year(2000));
    assert!(CobolDateTime::is_leap_year(2024));
    assert!(!CobolDateTime::is_leap_year(1900));
    assert!(!CobolDateTime::is_leap_year(2023));
    assert_eq!(CobolDateTime::days_in_month(2024, 2), 29);
    assert_eq!(CobolDateTime::days_in_month(2023, 2), 28);
    assert_eq!(CobolDateTime::days_in_month(2023, 4), 30);
    assert_eq!(CobolDateTime::days_in_month(2023, 13), 0);

    let pre_epoch = CobolDateTime::from_unix_timestamp(-86_400 * 400, 123_456);
    assert!(pre_epoch.iso_day_of_week() >= 1);
    let epoch_jan = CobolDateTime::from_unix_timestamp(0, 0);
    assert_eq!(
        (epoch_jan.year, epoch_jan.month, epoch_jan.day),
        (1970, 1, 1)
    );
    let yr1_jan = CobolDateTime {
        year: 1,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
        microsecond: 0,
    };
    assert_eq!(yr1_jan.iso_day_of_week(), 1);

    let fb = CobolDateTime::now_system();
    let easter = parse_current_date_override("2015/04/05 18:45:22.123400000", fb);
    assert_eq!(
        easter.map(|d| (
            d.day_of_year(),
            d.iso_day_of_week(),
            d.format_for_accept(RuntimeAcceptSource::DateYymmdd),
            d.format_for_accept(RuntimeAcceptSource::DateYyyymmdd),
            d.format_for_accept(RuntimeAcceptSource::DayYyddd),
            d.format_for_accept(RuntimeAcceptSource::DayYyyyddd),
            d.format_for_accept(RuntimeAcceptSource::DayOfWeek),
            d.format_for_accept(RuntimeAcceptSource::TimeStandard8),
            d.format_for_accept(RuntimeAcceptSource::MicrosecondTime12),
            d.format_for_accept(RuntimeAcceptSource::ConsoleLine),
        )),
        Some((
            95,
            7,
            String::from("150405"),
            String::from("20150405"),
            String::from("15095"),
            String::from("2015095"),
            String::from("7"),
            String::from("18452212"),
            String::from("184522123400"),
            String::from("184522123400"),
        ))
    );
    assert!(parse_current_date_override("2020-02-29", fb).is_some());
    assert!(parse_current_date_override("2020-12-31T23:59:60,5Z", fb).is_some());
    assert!(parse_current_date_override("YYYYMMDDHHMMSS+02", fb).is_some());
    assert!(parse_current_date_override("YYYYMMDDHHMMSS+0100", fb).is_some());
    assert!(parse_current_date_override("YYYYMMDDHHMMSS-05:30", fb).is_some());

    for invalid in [
        "",
        "201504",
        "00000101",
        "20151301",
        "20150400",
        "20150431",
        "2015040512",
        "20150405240000",
        "20150405126000",
        "20150405120061",
        "20150405120000.1234567890",
        "20150405120000?",
        "YYYYMMDDHHMMSSYYYYYYYYYYYY",
        "YYYYMMDDHHMMSS-011569",
        "YYYYMMDDHHMMSS+0A",
        "YYYYMMDDHHMMSS+010A",
        "YYYYMMDDHHMMSS+01000",
        "YYYYMMDDHHMMSS+01:0A",
        "YYYYMMDDHHMMSS+2500",
        "YYYYMMDDHHMMSS+0160",
    ] {
        assert!(parse_current_date_override(invalid, fb).is_none());
    }
}

/// Builds serialized `(alphanumeric_14, alphanumeric_6, zoned_14)` specs for M10 `ACCEPT` tests.
fn m10_test_specs() -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{BlankZeroSpec, HirType, JustifiedSpec, NumericSignSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};

    let mk_a = |byte_len| {
        encode_field_spec(&RuntimeFieldSpec {
            desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
                byte_len,
                justified: JustifiedSpec::Left,
            }),
            packed_sign_rule: PackedSignPolicy::Gnu,
        })
    };
    let n14 = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(14)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    Ok((mk_a(14), mk_a(6), n14))
}

/// Exercises M10 console `ACCEPT` operations (typed field, numeric input, EOF, and malformed spec).
#[test]
fn m10_accept_console_coverage() -> Result<()> {
    use coblang_rt_core::accept_env::{AcceptFlowOutcome, AcceptTargetDest, RuntimeAcceptSource};

    let (a14, _, n14) = m10_test_specs()?;
    let mut rt = CobolRuntimeContext::new();
    let (mut err, mut b14, mut num14) = (Vec::new(), [b' '; 14], [b'9'; 14]);
    let mut stdin_cur = io::Cursor::new(&b"hello\r\n   \n42\nbadline\n"[..]);
    let mut no_env = |_: &str| None;

    assert_eq!(
        rt.execute_accept(
            RuntimeAcceptSource::ConsoleLine,
            AcceptTargetDest::field(&mut b14, &a14),
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )?,
        AcceptFlowOutcome::Succeeded
    );
    assert_eq!(&b14[..6], b"hello ");
    let _ = rt.execute_accept(
        RuntimeAcceptSource::ConsoleLine,
        AcceptTargetDest::field(&mut num14, &n14),
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert_eq!(&num14, b"00000000000000");
    let _ = rt.execute_accept(
        RuntimeAcceptSource::ConsoleLine,
        AcceptTargetDest::field(&mut num14, &n14),
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert_eq!(&n14_slice(&num14), b"42");
    assert!(
        rt.execute_accept(
            RuntimeAcceptSource::ConsoleLine,
            AcceptTargetDest::field(&mut b14, &[]),
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )
        .is_err()
    );
    assert!(
        rt.execute_accept(
            RuntimeAcceptSource::ConsoleLine,
            AcceptTargetDest::field(&mut b14, &[]),
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )
        .is_err()
    );
    assert_eq!(
        rt.execute_accept(
            RuntimeAcceptSource::ConsoleLine,
            AcceptTargetDest::Omitted,
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )?,
        AcceptFlowOutcome::Exception
    );
    Ok(())
}

/// Exercises M10 CLI-argument (`ARGUMENT-NUMBER` and `COMMAND-LINE`) `ACCEPT` operations.
#[test]
fn m10_accept_cli_args_coverage() -> Result<()> {
    use coblang_rt_core::accept_env::{AcceptTargetDest, RuntimeAcceptSource};

    let (a14, _, n14) = m10_test_specs()?;
    let mut rt = CobolRuntimeContext::new();
    rt.initialize_with_env(
        vec![
            String::from("./p"),
            String::from("one"),
            String::from("two"),
        ],
        |_| None,
    );
    let (mut err, mut b14, mut num14) = (Vec::new(), [b' '; 14], [b'9'; 14]);
    let mut stdin_cur = io::Cursor::new(&b""[..]);
    let mut no_env = |_: &str| None;

    let _ = rt.execute_accept(
        RuntimeAcceptSource::ArgumentNumber,
        AcceptTargetDest::Omitted,
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert!(
        rt.execute_accept(
            RuntimeAcceptSource::ArgumentNumber,
            AcceptTargetDest::field(&mut num14, &[]),
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )
        .is_err()
    );
    let _ = rt.execute_accept(
        RuntimeAcceptSource::ArgumentNumber,
        AcceptTargetDest::field(&mut num14, &n14),
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert_eq!(&num14[13..], b"2");
    let _ = rt.execute_accept(
        RuntimeAcceptSource::CommandLine,
        AcceptTargetDest::Omitted,
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert!(
        rt.execute_accept(
            RuntimeAcceptSource::CommandLine,
            AcceptTargetDest::field(&mut b14, &[]),
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )
        .is_err()
    );
    let _ = rt.execute_accept(
        RuntimeAcceptSource::CommandLine,
        AcceptTargetDest::field(&mut b14, &a14),
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert_eq!(&b14[..7], b"one two");
    Ok(())
}

/// Returns the last 2 bytes of a 14-byte zoned buffer.
///
/// # Arguments
///
/// * `b` - 14-byte zoned buffer.
const fn n14_slice(b: &[u8; 14]) -> [u8; 2] {
    [b[12], b[13]]
}

/// Exercises M10 `ENVIRONMENT-NAME` and `ENVIRONMENT-VALUE` `DISPLAY`/`ACCEPT` operations.
#[test]
fn m10_accept_env_name_value_coverage() -> Result<()> {
    use coblang_rt_core::accept_env::{
        AcceptFlowOutcome, AcceptTargetDest, EnvDisplayTarget, RuntimeAcceptSource,
    };

    let (a14, _, _) = m10_test_specs()?;
    let mut rt = CobolRuntimeContext::new();
    let (mut err, mut b14) = (Vec::new(), [b' '; 14]);
    let mut stdin_cur = io::Cursor::new(&b""[..]);
    let mut no_env = |_: &str| None;

    rt.display_staging_mut().push_raw(b"ignored");
    rt.flush_display_to_env_target(EnvDisplayTarget::EnvironmentValue);
    rt.display_staging_mut().push_raw(b"  MY_KEY  ");
    rt.flush_display_to_env_target(EnvDisplayTarget::EnvironmentName);
    let _ = rt.execute_accept(
        RuntimeAcceptSource::EnvironmentName,
        AcceptTargetDest::Omitted,
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert!(
        rt.execute_accept(
            RuntimeAcceptSource::EnvironmentName,
            AcceptTargetDest::field(&mut b14, &[]),
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )
        .is_err()
    );
    let _ = rt.execute_accept(
        RuntimeAcceptSource::EnvironmentName,
        AcceptTargetDest::field(&mut b14, &a14),
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert_eq!(
        rt.execute_accept(
            RuntimeAcceptSource::EnvironmentValue,
            AcceptTargetDest::Omitted,
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )?,
        AcceptFlowOutcome::Exception
    );
    rt.display_staging_mut().push_raw(b"VAL1   ");
    rt.flush_display_to_env_target(EnvDisplayTarget::EnvironmentValue);
    assert!(
        rt.execute_accept(
            RuntimeAcceptSource::EnvironmentValue,
            AcceptTargetDest::field(&mut b14, &[]),
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )
        .is_err()
    );
    let _ = rt.execute_accept(
        RuntimeAcceptSource::EnvironmentValue,
        AcceptTargetDest::field(&mut b14, &a14),
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert_eq!(&b14[..5], b"VAL1 ");
    Ok(())
}

/// Exercises M10 `ARGUMENT-VALUE` cursor stepping and `DISPLAY UPON COMMAND-LINE` operations.
#[test]
fn m10_accept_arg_value_and_cmdline_display_coverage() -> Result<()> {
    use coblang_rt_core::accept_env::{
        AcceptFlowOutcome, AcceptTargetDest, EnvDisplayTarget, RuntimeAcceptSource,
    };

    let (a14, _, _) = m10_test_specs()?;
    let mut rt = CobolRuntimeContext::new();
    rt.initialize_with_env(
        vec![
            String::from("./p"),
            String::from("one"),
            String::from("two"),
        ],
        |_| None,
    );
    let (mut err, mut b14) = (Vec::new(), [b' '; 14]);
    let mut stdin_cur = io::Cursor::new(&b""[..]);
    let mut no_env = |_: &str| None;

    rt.display_staging_mut().push_raw(b"bad");
    rt.flush_display_to_env_target(EnvDisplayTarget::ArgumentNumber);
    rt.display_staging_mut().push_raw(b"1");
    rt.flush_display_to_env_target(EnvDisplayTarget::ArgumentNumber);
    assert!(
        rt.execute_accept(
            RuntimeAcceptSource::ArgumentValue,
            AcceptTargetDest::field(&mut b14, &[]),
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )
        .is_err()
    );
    let _ = rt.execute_accept(
        RuntimeAcceptSource::ArgumentValue,
        AcceptTargetDest::field(&mut b14, &a14),
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    let _ = rt.execute_accept(
        RuntimeAcceptSource::ArgumentValue,
        AcceptTargetDest::Omitted,
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert_eq!(
        rt.execute_accept(
            RuntimeAcceptSource::ArgumentValue,
            AcceptTargetDest::Omitted,
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )?,
        AcceptFlowOutcome::Exception
    );
    rt.display_staging_mut().push_raw(b"alpha beta");
    rt.flush_display_to_env_target(EnvDisplayTarget::CommandLine);
    Ok(())
}

/// Exercises M10 `ACCEPT` date/time sources (`COB_CURRENT_DATE`, `GCOBOL_CURRENT_DATE`, ACU width rules, warnings).
#[test]
fn m10_accept_datetime_sources_coverage() -> Result<()> {
    use coblang_rt_core::accept_env::{AcceptTargetDest, RuntimeAcceptSource};

    let (a14, a6, n14) = m10_test_specs()?;
    let mut rt = CobolRuntimeContext::new();
    let (mut err, mut b14, mut b6, mut num14) = (Vec::new(), [b' '; 14], [b' '; 6], [b'0'; 14]);
    let mut stdin_cur = io::Cursor::new(&b""[..]);
    let mut no_env = |_: &str| None;

    let _ = rt.execute_accept(
        RuntimeAcceptSource::TimeAcu12,
        AcceptTargetDest::Omitted,
        &mut stdin_cur,
        &mut err,
        &mut no_env,
    )?;
    assert!(
        rt.execute_accept(
            RuntimeAcceptSource::DateYyyymmdd,
            AcceptTargetDest::field(&mut b14, &[]),
            &mut stdin_cur,
            &mut err,
            &mut no_env
        )
        .is_err()
    );
    let mut gcob_bad = |k: &str| (k == "GCOBOL_CURRENT_DATE").then(|| String::from("BAD-GCOBOL"));
    let _ = rt.execute_accept(
        RuntimeAcceptSource::DateYyyymmdd,
        AcceptTargetDest::Omitted,
        &mut stdin_cur,
        &mut err,
        &mut gcob_bad,
    )?;
    let mut gcob_ok =
        |k: &str| (k == "GCOBOL_CURRENT_DATE").then(|| String::from("20150405184522"));
    let _ = rt.execute_accept(
        RuntimeAcceptSource::TimeAcu12,
        AcceptTargetDest::field(&mut b6, &a6),
        &mut stdin_cur,
        &mut err,
        &mut gcob_ok,
    )?;
    let mut cob_ok =
        |k: &str| (k == "COB_CURRENT_DATE").then(|| String::from("2015/04/05 18:45:22.123400"));
    let _ = rt.execute_accept(
        RuntimeAcceptSource::TimeAcu12,
        AcceptTargetDest::field(&mut num14, &n14),
        &mut stdin_cur,
        &mut err,
        &mut cob_ok,
    )?;
    assert_eq!(&num14, b"00184522123400");
    let mut cob_bad = |k: &str| (k == "COB_CURRENT_DATE").then(|| String::from("BAD-DATE"));
    for _ in 0..2 {
        let _ = rt.execute_accept(
            RuntimeAcceptSource::DateYyyymmdd,
            AcceptTargetDest::field(&mut b14, &a14),
            &mut stdin_cur,
            &mut err,
            &mut cob_bad,
        )?;
    }
    assert_eq!(
        String::from_utf8_lossy(&err),
        "libcob: warning: COB_CURRENT_DATE 'BAD-DATE' is invalid\n"
    );
    Ok(())
}

/// Exercises `M10StringInspectStaging` `STRING` and `UNSTRING` methods directly.
#[test]
fn m10_staging_string_and_unstring_coverage() -> Result<()> {
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{HirType, JustifiedSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::m10_staging::{
        M10StringInspectStaging, StagedStringDelim, StringFlowOutcome, UnstringMatchMode,
    };

    let a2_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 2,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let a1_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 1,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });

    let mut st = M10StringInspectStaging::new();
    st.string_push_src(b"A", None)?;
    st.string_begin();
    st.string_set_delim(StagedStringDelim::from_raw(1, b"|", Some(&a1_spec))?);
    st.string_push_src(b"AB|Z", Some(&a2_spec))?;
    st.string_set_delim(StagedStringDelim::from_raw(2, b" ", None)?);
    st.string_push_src(b"C D", None)?;
    st.string_set_delim(StagedStringDelim::from_raw(0, b"", None)?);
    let mut dst = *b"....";
    assert_eq!(st.string_exec(&mut dst, 1), StringFlowOutcome::Completed);
    assert_eq!((&dst, st.last_pointer()), (b"ABC.", 4));

    st.unstring_begin(b"X--Y", Some(&a2_spec), 0)?;
    assert_eq!(st.unstring_step_into(&mut [0; 2], &a2_spec, None)?, None);
    assert_eq!(st.unstring_finish(), StringFlowOutcome::Overflowed);

    st.unstring_begin(b"A-B", None, 1)?;
    st.unstring_push_delim(UnstringMatchMode::AllRepeated, b"-", None)?;
    let (mut u1, mut u2, mut d1) = (*b"  ", *b"  ", [b' '; 1]);
    st.unstring_set_delim_in_spec(&a1_spec);
    assert_eq!(
        st.unstring_step_into(&mut u1, &a2_spec, Some(&mut d1))?,
        Some(1)
    );
    assert_eq!(st.unstring_step_into(&mut u2, &a2_spec, None)?, Some(1));
    assert_eq!(st.unstring_step_into(&mut u2, &a2_spec, None)?, None);
    assert_eq!(st.unstring_finish(), StringFlowOutcome::Completed);
    assert_eq!(st.last_unstring_tally_delta(), 2);
    Ok(())
}

/// Exercises `M10StringInspectStaging` `INSPECT` (`TALLYING`, `REPLACING`, `CONVERTING`) methods directly.
#[test]
fn m10_staging_inspect_coverage() -> Result<()> {
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{HirType, JustifiedSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
    use coblang_rt_core::m10_staging::{
        InspectMatchKind, InspectRegSlot, InspectWindowEdge, M10StringInspectStaging,
        StagedInspectOperand, decode_inspect_match_kind, encode_inspect_match_kind,
    };

    for (k, c) in [
        (InspectMatchKind::Characters, 0),
        (InspectMatchKind::All, 1),
        (InspectMatchKind::Leading, 2),
        (InspectMatchKind::First, 3),
        (InspectMatchKind::Trailing, 4),
    ] {
        assert_eq!(encode_inspect_match_kind(k), c);
        assert_eq!(decode_inspect_match_kind(c), k);
    }
    assert_eq!(decode_inspect_match_kind(99), InspectMatchKind::Characters);
    assert_eq!(
        StagedInspectOperand::default(),
        StagedInspectOperand::Bytes(Vec::new())
    );

    let a2_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 2,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });

    let mut st = M10StringInspectStaging::new();
    st.inspect_begin(1);
    let mut ibuf = *b"AABZ";
    st.inspect_exec_tally_replace(&mut ibuf, None)?;
    st.inspect_set_window(
        InspectWindowEdge::After,
        StagedInspectOperand::from_raw(0, b"A", None)?,
    );
    st.inspect_set_window(
        InspectWindowEdge::Before,
        StagedInspectOperand::from_raw(0, b"Z", None)?,
    );
    st.inspect_set_operand(
        InspectRegSlot::PatternOrFrom,
        StagedInspectOperand::from_raw(0, b"A", None)?,
    );
    st.inspect_set_operand(
        InspectRegSlot::ByOrTo,
        StagedInspectOperand::from_raw(1, b"-", None)?,
    );
    st.inspect_push_tally_from_regs(0, InspectMatchKind::All);
    st.inspect_push_replace_from_regs(InspectMatchKind::All);
    st.inspect_exec_tally_replace(&mut ibuf, Some(&a2_spec))?;
    assert_eq!(
        (&ibuf, st.inspect_tally_delta(0), st.inspect_tally_delta(9)),
        (b"A-BZ", 1, 0)
    );
    st.inspect_set_window(
        InspectWindowEdge::After,
        StagedInspectOperand::from_raw(0, b"A", None)?,
    );
    st.inspect_set_window(
        InspectWindowEdge::Before,
        StagedInspectOperand::from_raw(0, b"Z", None)?,
    );
    st.inspect_set_operand(
        InspectRegSlot::PatternOrFrom,
        StagedInspectOperand::from_raw(0, b"-B", None)?,
    );
    st.inspect_set_operand(
        InspectRegSlot::ByOrTo,
        StagedInspectOperand::from_raw(0, b"12", None)?,
    );
    st.inspect_exec_converting_from_regs(&mut ibuf, Some(&a2_spec))?;
    assert_eq!(&ibuf, b"A12Z");
    st.clear();
    Ok(())
}

/// Verifies [`coblang_rt_core::heap::HeapTracker`] and [`coblang_rt_core::field_ops::evaluate_based_pointer`] across all branches.
#[test]
fn m11_heap_tracker_and_based_pointer_evaluator_coverage() {
    use coblang_rt_core::field_ops::{BoundCheckDisposition, evaluate_based_pointer};
    use coblang_rt_core::heap::HeapTracker;

    let mut tracker = HeapTracker::new();
    assert_eq!(tracker.active_count(), 0);
    tracker.record(0, 16);
    assert_eq!(tracker.active_count(), 0);
    tracker.record(0x1000, 32);
    tracker.record(0x2000, 64);
    assert_eq!(tracker.active_count(), 2);
    assert_eq!(tracker.take(0x9999), None);
    assert_eq!(tracker.take(0x1000), Some(32));
    assert_eq!(tracker.active_count(), 1);

    tracker.record_external(b"SEG_ZERO".to_vec(), 0, 8);
    assert_eq!(tracker.lookup_external(b"SEG_ZERO"), None);
    tracker.record_external(b"SEG_ONE".to_vec(), 0x3000, 128);
    assert_eq!(tracker.lookup_external(b"SEG_ONE"), Some(0x3000));

    let drained = tracker.drain_all();
    assert_eq!(drained, vec![(0x2000, 64), (0x3000, 128)]);
    assert_eq!(tracker.active_count(), 0);

    let mut err_buf = Vec::new();
    assert_eq!(
        evaluate_based_pointer(0x1234, "p.cob:4", "ROOT-REC", "", &mut err_buf),
        BoundCheckDisposition::Pass
    );
    assert_eq!(err_buf.len(), 0);

    assert_eq!(
        evaluate_based_pointer(0, "p.cob:5", "ROOT-REC", "", &mut err_buf),
        BoundCheckDisposition::FatalAbortExitOne("EC-DATA-PTR-NULL")
    );
    assert!(String::from_utf8_lossy(&err_buf).contains("item 'ROOT-REC' has NULL address"));
    err_buf.clear();

    assert_eq!(
        evaluate_based_pointer(0, "p.cob:6", "ROOT-REC", "ROOT-REC", &mut err_buf),
        BoundCheckDisposition::FatalAbortExitOne("EC-DATA-PTR-NULL")
    );
    assert!(String::from_utf8_lossy(&err_buf).contains("item 'ROOT-REC' has NULL address"));
    err_buf.clear();

    assert_eq!(
        evaluate_based_pointer(0, "p.cob:7", "ROOT-REC", "SUB-FLD", &mut err_buf),
        BoundCheckDisposition::FatalAbortExitOne("EC-DATA-PTR-NULL")
    );
    assert!(String::from_utf8_lossy(&err_buf).contains("(accessed by 'SUB-FLD')"));
}

/// Verifies all 140 ISO 2023 `ExceptionCode` entries, `>>TURN` masks, `EXCEPTION-*` buffers,
/// declarative re-entrancy guard, and NIST CCVS-85 `DEBUG-ITEM` formatting (`Milestone M13`).
#[test]
fn m13_iso2023_exception_tree_turn_masks_and_debug_item_coverage() -> Result<()> {
    use coblang_rt_core::exceptions::{
        DeclarativeGuardState, ExceptionCode, ExceptionLevel, ExceptionRecord,
        ExceptionRuntimeState, TurnCheckingMode,
    };

    for (idx, spec) in ExceptionCode::catalog().iter().enumerate() {
        let raw_id = u16::try_from(idx).unwrap_or(0);
        assert_eq!(spec.code.to_abi_id(), raw_id);
        assert_eq!(ExceptionCode::from_abi_id(raw_id)?, spec.code);
        assert_eq!(
            ExceptionCode::from_cobol_name(&spec.name.to_ascii_lowercase())?,
            spec.code
        );
        assert_eq!(spec.code.cobol_name(), spec.name);
        assert_eq!(spec.code.category(), spec.category);
        assert_eq!(spec.code.level(), spec.level);
        assert!(spec.code.matches_selector(ExceptionCode::EcAll));
        assert!(spec.code.matches_selector(spec.code));
        assert_eq!(spec.code.status_buffer_31().len(), 31);
    }
    assert!(ExceptionCode::from_abi_id(999).is_err());
    assert!(ExceptionCode::from_cobol_name("EC-NO-SUCH-CODE").is_err());
    assert_eq!(
        ExceptionCode::EcSizeZeroDivide.level(),
        ExceptionLevel::Leaf
    );
    assert!(ExceptionCode::EcSizeZeroDivide.matches_selector(ExceptionCode::EcSize));
    assert!(!ExceptionCode::EcSizeZeroDivide.matches_selector(ExceptionCode::EcBound));
    assert!(!ExceptionCode::EcSizeZeroDivide.matches_selector(ExceptionCode::EcSizeOverflow));

    let mut ex = ExceptionRuntimeState::default();
    assert_eq!(ex.last_record(), None);
    assert_eq!(ex.exception_status_31(), [b' '; 31]);
    assert_eq!(ex.exception_statement_31(), [b' '; 31]);
    assert_eq!(ex.exception_location(), "");
    assert_eq!(ex.exception_file(), "");
    assert!(!ex.is_enabled(ExceptionCode::EcSizeZeroDivide));

    ex.apply_turn(ExceptionCode::EcAll, TurnCheckingMode::Enabled);
    assert!(ex.is_enabled(ExceptionCode::EcSizeZeroDivide));
    assert!(ex.is_enabled(ExceptionCode::EcValidateVarying));
    ex.apply_turn(ExceptionCode::EcSize, TurnCheckingMode::Disabled);
    assert!(!ex.is_enabled(ExceptionCode::EcSizeZeroDivide));
    assert!(ex.is_enabled(ExceptionCode::EcBoundSubscript));
    ex.apply_turn(ExceptionCode::EcBoundSubscript, TurnCheckingMode::Disabled);
    assert!(!ex.is_enabled(ExceptionCode::EcBoundSubscript));

    ex.raise(ExceptionRecord {
        code: ExceptionCode::EcIoPermanentError,
        statement: String::from("read"),
        location: String::from("MAIN; P1; 42"),
        file_name: String::from("IN-FILE"),
    });
    assert!(ex.last_record().is_some());
    assert!(
        ex.exception_status_31()
            .starts_with(b"EC-I-O-PERMANENT-ERROR ")
    );
    assert!(ex.exception_statement_31().starts_with(b"READ "));
    assert_eq!(ex.exception_location(), "MAIN; P1; 42");
    assert_eq!(ex.exception_file(), "IN-FILE");

    assert_eq!(ex.declarative_guard(), DeclarativeGuardState::Idle);
    ex.enter_declarative()?;
    assert_eq!(ex.declarative_guard(), DeclarativeGuardState::InDeclarative);
    assert!(ex.enter_declarative().is_err());
    assert_eq!(
        ex.last_record().map(|r| r.code),
        Some(ExceptionCode::EcFlowUse)
    );
    ex.leave_declarative();
    assert_eq!(ex.declarative_guard(), DeclarativeGuardState::Idle);
    ex.clear_last_exception();
    assert_eq!(ex.last_record(), None);
    ex.reset();
    Ok(())
}

/// Verifies NIST CCVS-85 `DEBUG-ITEM` record formatting and `CobolRuntimeContext` exception integration.
#[test]
fn m13_debug_item_and_runtime_context_exceptions_coverage() {
    use coblang_rt_core::exceptions::{DebugItemState, ExceptionCategory, ExceptionCode};

    let mut dbg = DebugItemState::default();
    dbg.populate(
        "000042",
        "MY-TABLE-FIELD",
        [Some(3), Some(-12), None],
        b"START PROGRAM",
    );
    let rec = dbg.to_record_bytes(16);
    assert_eq!(rec.len(), 72);
    assert!(rec.starts_with(b"000042"));
    assert!(String::from_utf8_lossy(&rec).contains("+0003 -0012       START PROGRAM   "));
    dbg.clear();
    assert_eq!(dbg.to_record_bytes(4).len(), 60);

    let mut rt = CobolRuntimeContext::new();
    rt.record_exception("EC-SIZE-OVERFLOW");
    assert_eq!(
        rt.exceptions_mut().last_record().map(|r| r.code),
        Some(ExceptionCode::EcSizeOverflow)
    );
    rt.record_exception("");
    assert_eq!(rt.exceptions_mut().last_record(), None);
    rt.debug_item_mut().populate("1", "P", [None; 3], b"X");
    let _ = rt.m10_staging_mut().string_push_src(b"A!", None);
    let _ = rt.m10_staging_mut().string_exec(&mut [b' '; 2], 1);
    assert_eq!(rt.heap_mut().active_count(), 0);
    assert_eq!(
        rt.exceptions_mut().last_record().map(|r| r.code.category()),
        None::<ExceptionCategory>
    );
    rt.tidy();
}

/// Verifies `evaluate_based_pointer` `"L:<root>"` `LINKAGE` not-passed diagnostic formatting.
#[test]
fn evaluate_based_pointer_linkage_prefix_coverage() {
    use coblang_rt_core::field_ops::{BoundCheckDisposition, evaluate_based_pointer};

    let mut err_root = Vec::new();
    assert_ne!(
        evaluate_based_pointer(0, "sub.cob:9", "L:X", "", &mut err_root),
        BoundCheckDisposition::Pass
    );
    assert!(String::from_utf8_lossy(&err_root).contains("LINKAGE item 'X' not passed by caller"));

    let mut err_same = Vec::new();
    assert_ne!(
        evaluate_based_pointer(0, "sub.cob:9", "L:X", "X", &mut err_same),
        BoundCheckDisposition::Pass
    );
    assert!(String::from_utf8_lossy(&err_same).contains("LINKAGE item 'X' not passed by caller"));

    let mut err_child = Vec::new();
    assert_ne!(
        evaluate_based_pointer(0, "sub.cob:9", "L:X", "Y", &mut err_child),
        BoundCheckDisposition::Pass
    );
    assert!(
        String::from_utf8_lossy(&err_child)
            .contains("LINKAGE item 'X' (accessed by 'Y') not passed by caller")
    );
}
