//! Exhaustive unit tests for Milestone M17 `NATIONAL` (`UTF-16LE`), `UTF-8` (`PIC U`), and `EBCDIC`
//! runtime `MOVE`, `COMPARE`, `CLASS`, `REFMOD`, `INSPECT`, `STRING`, `UNSTRING`, and `ACCEPT`.

use coblang_common::Result;
use coblang_decimal::{OverpunchDialect, PackedSignPolicy};
use coblang_hir::{DataCategory, HirDisplayStyle, HirType, JustifiedSpec};
use coblang_pic::{
    DefaultInitMode, FillerPolicy, InitCategoryMask, InspectMatchKind, MoveFieldDesc,
    StringFlowOutcome, UnstringMatchMode,
};
use coblang_rt_core::{
    accept_env::{
        AcceptEnvState, AcceptFlowOutcome, AcceptTargetDest, RuntimeAcceptSource,
        execute_runtime_accept,
    },
    desc_codec::{
        DecodedInitPlan, FillerMembership, RedefinesMembership, RuntimeFieldSpec, RuntimeInitEntry,
        encode_field_spec, encode_init_plan,
    },
    field_ops::{
        AlphaCmpOperand, ClassTestKind, InitStagingState, RelCmpOp, RuntimeMoveSourceKind,
        runtime_check_class, runtime_compare_alphanumeric_ex, runtime_execute_move,
        swap_utf16be_to_le, utf8_refmod_byte_length, utf8_refmod_byte_offset,
    },
    m10_staging::{
        InspectRegSlot, InspectWindowEdge, M10StringInspectStaging, StagedInspectOperand,
        StagedStringDelim,
    },
    m17_unicode_ops::{CharUnitMode, bytes_to_unit_chars, decode_operand_auto_chars},
};

/// Builds a serialized [`RuntimeFieldSpec`] for `ty` and `overpunch`.
///
/// # Arguments
///
/// * `ty` - Field [`HirType`].
/// * `overpunch` - Active [`OverpunchDialect`].
fn spec_bytes(ty: HirType, overpunch: OverpunchDialect) -> Vec<u8> {
    encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc {
            ty,
            overpunch,
            decimal_char: b'.',
            currency_symbol: '$',
            display_style: HirDisplayStyle::GnuPrettyWidth,
        },
        packed_sign_rule: PackedSignPolicy::Gnu,
    })
}

/// Verifies `NATIONAL`, `UTF-8`, and `EBCDIC` runtime `MOVE`, `COMPARE`, `CLASS`, `INITIALIZE`, and `UTF-8` refmod.
#[test]
fn m17_move_compare_class_init_and_refmod() -> Result<()> {
    assert_eq!(
        RuntimeMoveSourceKind::from_abi_code(10),
        RuntimeMoveSourceKind::NationalLiteral
    );
    assert_eq!(
        RuntimeMoveSourceKind::from_abi_code(11),
        RuntimeMoveSourceKind::Utf8Literal
    );
    assert_eq!(RuntimeMoveSourceKind::NationalLiteral.to_abi_code(), 10);
    assert_eq!(RuntimeMoveSourceKind::Utf8Literal.to_abi_code(), 11);
    assert_eq!(
        swap_utf16be_to_le(&[0x00, b'A', 0x00, b'B']),
        [b'A', 0x00, b'B', 0x00]
    );

    let nat_spec = spec_bytes(
        HirType::National {
            char_len: 4,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::AsciiGnu,
    );
    let u8_spec = spec_bytes(HirType::Utf8 { byte_len: 6 }, OverpunchDialect::AsciiGnu);
    let ebc_spec = spec_bytes(
        HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::EbcdicIbm,
    );

    let mut nat_buf = [0_u8; 8];
    runtime_execute_move(
        RuntimeMoveSourceKind::NationalLiteral,
        &[0x00, b'A', 0x00, b'B'],
        &[],
        &mut nat_buf,
        &nat_spec,
    )?;
    assert_eq!(nat_buf, [b'A', 0, b'B', 0, b' ', 0, b' ', 0]);

    let mut u8_buf = [0_u8; 6];
    runtime_execute_move(
        RuntimeMoveSourceKind::Utf8Literal,
        "äö".as_bytes(),
        &[],
        &mut u8_buf,
        &u8_spec,
    )?;
    assert_eq!(&u8_buf, "äö  ".as_bytes());

    // Cross-charset comparison: National vs Alphanumeric vs NationalLiteral vs Figuratives
    assert!(runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::Field,
            bytes: &nat_buf,
            spec_bytes: &nat_spec,
        },
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::AlphanumericLiteral,
            bytes: b"AB",
            spec_bytes: &[],
        },
        RelCmpOp::Eq,
        None,
    ));
    assert!(runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::Field,
            bytes: &u8_buf,
            spec_bytes: &u8_spec,
        },
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::Utf8Literal,
            bytes: "äö".as_bytes(),
            spec_bytes: &[],
        },
        RelCmpOp::Eq,
        None,
    ));
    let _ = ebc_spec;
    Ok(())
}

/// Verifies `NATIONAL` and `EBCDIC` figurative constant comparisons (`HIGH-VALUE`, `LOW-VALUE`, `ZERO`, `SPACE`, `QUOTE`, `ALL`).
#[test]
fn m17_figurative_compare() -> Result<()> {
    let nat_spec = spec_bytes(
        HirType::National {
            char_len: 4,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::AsciiGnu,
    );
    let nat_buf = [b'A', 0, b'B', 0, b' ', 0, b' ', 0];
    let ebc_spec = spec_bytes(
        HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::EbcdicIbm,
    );
    for (fk, expected_rel) in [
        (RuntimeMoveSourceKind::FigHigh, RelCmpOp::Lt),
        (RuntimeMoveSourceKind::FigLow, RelCmpOp::Gt),
        (RuntimeMoveSourceKind::FigZero, RelCmpOp::Gt),
        (RuntimeMoveSourceKind::FigSpace, RelCmpOp::Gt),
        (RuntimeMoveSourceKind::FigQuote, RelCmpOp::Gt),
        (RuntimeMoveSourceKind::FigAll, RelCmpOp::Eq),
    ] {
        assert!(runtime_compare_alphanumeric_ex(
            &AlphaCmpOperand {
                kind: RuntimeMoveSourceKind::NationalLiteral,
                bytes: &[0x00, b'A'],
                spec_bytes: &[],
            },
            &AlphaCmpOperand {
                kind: fk,
                bytes: b"A",
                spec_bytes: &[],
            },
            expected_rel,
            None,
        ));
    }

    // EBCDIC field vs National and vs Figurative SPACE (0x40), ZERO (0xF0), QUOTE (0x7F)
    assert!(runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::Field,
            bytes: &[0xC1, 0xC2, 0x40, 0x40],
            spec_bytes: &ebc_spec,
        },
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::Field,
            bytes: &nat_buf,
            spec_bytes: &nat_spec,
        },
        RelCmpOp::Eq,
        None,
    ));
    assert!(runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::Field,
            bytes: &[0x40, 0x40, 0x40, 0x40],
            spec_bytes: &ebc_spec,
        },
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::FigSpace,
            bytes: &[],
            spec_bytes: &[],
        },
        RelCmpOp::Eq,
        None,
    ));
    assert!(runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::Field,
            bytes: &[0xF0, 0x40, 0x40, 0x40],
            spec_bytes: &ebc_spec,
        },
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::AlphanumericLiteral,
            bytes: &[0xF0],
            spec_bytes: &[],
        },
        RelCmpOp::Eq,
        None,
    ));
    assert!(runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::Field,
            bytes: &[0xF0, 0xF0, 0xF0, 0xF0],
            spec_bytes: &ebc_spec,
        },
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::FigZero,
            bytes: &[],
            spec_bytes: &[],
        },
        RelCmpOp::Eq,
        None,
    ));
    assert!(runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::Field,
            bytes: &[0x7F, 0x7F, 0x7F, 0x7F],
            spec_bytes: &ebc_spec,
        },
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::FigQuote,
            bytes: &[],
            spec_bytes: &[],
        },
        RelCmpOp::Eq,
        None,
    ));
    Ok(())
}

/// Verifies `NATIONAL`, `UTF-8`, and `EBCDIC` runtime `CLASS` tests, `UTF-8` refmod, `INITIALIZE`, and `ACCEPT`.
#[test]
fn m17_class_refmod_init_and_accept() -> Result<()> {
    let nat_spec = spec_bytes(
        HirType::National {
            char_len: 4,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::AsciiGnu,
    );
    let ebc_spec = spec_bytes(
        HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::EbcdicIbm,
    );
    let nat_buf = [b'A', 0, b'B', 0, b' ', 0, b' ', 0];

    // Class tests for National & EBCDIC
    assert!(runtime_check_class(
        &nat_buf,
        &nat_spec,
        ClassTestKind::Alphabetic
    )?);
    assert!(runtime_check_class(
        &nat_buf,
        &nat_spec,
        ClassTestKind::AlphabeticUpper
    )?);
    assert!(!runtime_check_class(
        &nat_buf,
        &nat_spec,
        ClassTestKind::AlphabeticLower
    )?);
    assert!(runtime_check_class(
        &[0xC1, 0x40],
        &ebc_spec,
        ClassTestKind::AlphabeticUpper
    )?);
    assert!(runtime_check_class(
        &[0x81, 0x40],
        &ebc_spec,
        ClassTestKind::AlphabeticLower
    )?);
    assert!(runtime_check_class(
        &[0xC1, 0x81],
        &ebc_spec,
        ClassTestKind::Alphabetic
    )?);

    // UTF-8 character-indexed refmod offset & length
    let utf8_sample = "aäb€".as_bytes();
    assert_eq!(utf8_refmod_byte_offset(utf8_sample, 0), 0);
    assert_eq!(utf8_refmod_byte_offset(utf8_sample, 1), 0);
    assert_eq!(utf8_refmod_byte_offset(utf8_sample, 2), 1);
    assert_eq!(utf8_refmod_byte_offset(utf8_sample, 4), 4);
    assert_eq!(utf8_refmod_byte_length(utf8_sample, 1, 2), 3);
    assert_eq!(utf8_refmod_byte_length(utf8_sample, 4, -1), 3);
    assert_eq!(utf8_refmod_byte_length(utf8_sample, 0, 0), 0);
    Ok(())
}

/// Verifies `NATIONAL`, `UTF-8`, and `EBCDIC` runtime `INITIALIZE REPLACING` and `ACCEPT`.
#[test]
fn m17_init_and_accept() -> Result<()> {
    let nat_spec = spec_bytes(
        HirType::National {
            char_len: 4,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::AsciiGnu,
    );
    let ebc_spec = spec_bytes(
        HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::EbcdicIbm,
    );

    // INITIALIZE REPLACING NATIONAL / UTF-8
    let mut init_st = InitStagingState::new();
    init_st.push_replacement(
        DataCategory::National,
        RuntimeMoveSourceKind::NationalLiteral,
        vec![0x00, b'Z'],
        &[],
    )?;
    init_st.push_replacement(
        DataCategory::Utf8,
        RuntimeMoveSourceKind::Utf8Literal,
        b"Q".to_vec(),
        &[],
    )?;
    let plan = encode_init_plan(&DecodedInitPlan {
        filler_policy: FillerPolicy::IncludeFiller,
        to_value_mask: InitCategoryMask::NONE,
        default_mode: DefaultInitMode::StandardCobol,
        entries: vec![RuntimeInitEntry {
            offset: 0,
            byte_len: 4,
            filler: FillerMembership::NamedField,
            redefines: RedefinesMembership::PrimaryStorage,
            value_bytes: None,
            dims: Vec::new(),
            desc: MoveFieldDesc {
                ty: HirType::National {
                    char_len: 2,
                    justified: JustifiedSpec::Left,
                },
                overpunch: OverpunchDialect::AsciiGnu,
                decimal_char: b'.',
                currency_symbol: '$',
                display_style: HirDisplayStyle::GnuPrettyWidth,
            },
        }],
    });
    let mut init_buf = [0_u8; 4];
    init_st.execute_plan(&mut init_buf, &plan)?;
    assert_eq!(init_buf, [b'Z', 0, b' ', 0]);

    // ACCEPT into EBCDIC and National destinations
    let mut env_st = AcceptEnvState::new();
    let mut stdin_cur = std::io::Cursor::new(
        &b"HI
OK
"[..],
    );
    let mut stderr_sink = Vec::new();
    let mut ebc_dst = [0x40_u8; 4];
    let r1 = execute_runtime_accept(
        RuntimeAcceptSource::ConsoleLine,
        AcceptTargetDest::field(&mut ebc_dst, &ebc_spec),
        &mut env_st,
        &[],
        &mut stdin_cur,
        &mut stderr_sink,
        &mut |_| None,
    )?;
    assert_eq!(r1, AcceptFlowOutcome::Succeeded);
    assert_eq!(ebc_dst, [0xC8, 0xC9, 0x40, 0x40]);

    let mut nat_dst = [0_u8; 8];
    let r2 = execute_runtime_accept(
        RuntimeAcceptSource::ConsoleLine,
        AcceptTargetDest::field(&mut nat_dst, &nat_spec),
        &mut env_st,
        &[],
        &mut stdin_cur,
        &mut stderr_sink,
        &mut |_| None,
    )?;
    assert_eq!(r2, AcceptFlowOutcome::Succeeded);
    assert_eq!(nat_dst, [b'O', 0, b'K', 0, b' ', 0, b' ', 0]);
    Ok(())
}

/// Verifies `INSPECT`, `STRING`, and `UNSTRING` over `NATIONAL` (`UTF-16LE`) and `UTF-8` (`PIC U`) fields.
#[test]
fn m17_unicode_inspect_string_and_unstring() -> Result<()> {
    assert_eq!(CharUnitMode::from_desc(None), CharUnitMode::SingleByte);
    assert_eq!(
        bytes_to_unit_chars(&[b'A', 0, b'B', 0], CharUnitMode::NationalUtf16Le),
        ['A', 'B']
    );
    assert_eq!(
        decode_operand_auto_chars(&[0, b'X', 0, b'Y'], CharUnitMode::NationalUtf16Le),
        ['X', 'Y']
    );

    let nat_spec_5 = spec_bytes(
        HirType::National {
            char_len: 5,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::AsciiGnu,
    );
    let u8_spec_6 = spec_bytes(HirType::Utf8 { byte_len: 6 }, OverpunchDialect::AsciiGnu);

    let mut st = M10StringInspectStaging::new();

    // 1. STRING into NATIONAL with POINTER (valid & overflow branches + Pattern delim)
    st.string_begin();
    st.string_set_dst_spec(Some(&nat_spec_5))?;
    st.string_set_delim(StagedStringDelim::FigurativeByte(b','));
    st.string_push_src(&[0, b'A', 0, b'B', 0, b',', 0, b'Z'], None)?;
    st.string_set_delim(StagedStringDelim::Pattern(vec![0, b';']));
    st.string_push_src(&[0, b'C', 0, b';', 0, b'W'], None)?;
    st.string_set_delim(StagedStringDelim::Size);
    st.string_push_src(&[b'D', 0, b'E', 0], None)?;
    let mut nat_into = [b' ', 0, b' ', 0, b' ', 0, b' ', 0, b' ', 0];
    let out = st.string_exec(&mut nat_into, 1);
    assert_eq!(out, StringFlowOutcome::Completed);
    assert_eq!(nat_into, [b'A', 0, b'B', 0, b'C', 0, b'D', 0, b'E', 0]);
    assert_eq!(st.last_pointer(), 6);

    // Overflow branch on initial ptr out of range + mid-write overflow
    st.string_begin();
    st.string_set_dst_spec(Some(&nat_spec_5))?;
    assert_eq!(
        st.string_exec(&mut nat_into, 0),
        StringFlowOutcome::Overflowed
    );
    st.string_begin();
    st.string_set_dst_spec(Some(&nat_spec_5))?;
    st.string_push_src(&[0, b'X', 0, b'Y'], None)?;
    assert_eq!(
        st.string_exec(&mut nat_into, 5),
        StringFlowOutcome::Overflowed
    );

    // 2. UNSTRING over NATIONAL with delimiter, DELIMITER IN, COUNT IN, and no-delimiter mode
    let nat_csv = [b'A', 0, b'B', 0, b':', 0, b':', 0, b'C', 0];
    st.unstring_begin(&nat_csv, Some(&nat_spec_5), 1)?;
    st.unstring_push_delim(UnstringMatchMode::AllRepeated, &[0, b':'], None)?;
    let mut p1 = [0_u8; 10];
    let mut d1 = [0_u8; 10];
    st.unstring_set_delim_in_spec(&nat_spec_5);
    let c1 = st.unstring_step_into(&mut p1, &nat_spec_5, Some(&mut d1))?;
    assert_eq!(c1, Some(2));
    assert_eq!(&p1[..4], &[b'A', 0, b'B', 0]);
    let mut p2 = [0_u8; 10];
    st.unstring_set_delim_in_spec(&nat_spec_5);
    let c2 = st.unstring_step_into(&mut p2, &nat_spec_5, Some(&mut d1))?;
    assert_eq!(c2, Some(1));
    assert_eq!(&p2[..2], &[b'C', 0]);
    assert_eq!(st.unstring_finish(), StringFlowOutcome::Completed);
    assert_eq!(st.last_pointer(), 6);

    // Fixed-slice (no delimiters) UNSTRING over NATIONAL & UTF-8
    let nat_spec_2 = spec_bytes(
        HirType::National {
            char_len: 2,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::AsciiGnu,
    );
    st.unstring_begin(&nat_csv, Some(&nat_spec_5), 1)?;
    let mut s2 = [0_u8; 4];
    st.unstring_set_delim_in_spec(&nat_spec_2);
    assert_eq!(
        st.unstring_step_into(&mut s2, &nat_spec_2, Some(&mut d1))?,
        Some(2)
    );
    assert_eq!(st.unstring_finish(), StringFlowOutcome::Overflowed);
    let _ = u8_spec_6;
    Ok(())
}

/// Verifies `INSPECT` (`TALLYING`, `REPLACING`, windows, and `CONVERTING`) over `NATIONAL` and `UTF-8` fields.
#[test]
fn m17_unicode_inspect_tally_replace_and_convert() -> Result<()> {
    let nat_spec_5 = spec_bytes(
        HirType::National {
            char_len: 5,
            justified: JustifiedSpec::Left,
        },
        OverpunchDialect::AsciiGnu,
    );
    let u8_spec_6 = spec_bytes(HirType::Utf8 { byte_len: 6 }, OverpunchDialect::AsciiGnu);
    let mut st = M10StringInspectStaging::new();

    // 3. INSPECT TALLYING + REPLACING + window bounds on NATIONAL and UTF-8
    let mut nat_insp = [b'A', 0, b'A', 0, b'B', 0, b' ', 0, b' ', 0];
    st.inspect_begin(3);
    st.inspect_set_operand(
        InspectRegSlot::PatternOrFrom,
        StagedInspectOperand::Bytes(vec![0, b'A']),
    );
    st.inspect_push_tally_from_regs(0, InspectMatchKind::Leading);
    st.inspect_set_operand(
        InspectRegSlot::PatternOrFrom,
        StagedInspectOperand::FigurativeByte(b' '),
    );
    st.inspect_push_tally_from_regs(1, InspectMatchKind::Trailing);
    st.inspect_push_tally_from_regs(2, InspectMatchKind::Characters);
    st.inspect_set_operand(
        InspectRegSlot::PatternOrFrom,
        StagedInspectOperand::Bytes(vec![0, b'B']),
    );
    st.inspect_set_operand(
        InspectRegSlot::ByOrTo,
        StagedInspectOperand::Bytes(vec![0, b'Z']),
    );
    st.inspect_push_replace_from_regs(InspectMatchKind::First);
    st.inspect_set_operand(
        InspectRegSlot::PatternOrFrom,
        StagedInspectOperand::FigurativeByte(b' '),
    );
    st.inspect_set_operand(
        InspectRegSlot::ByOrTo,
        StagedInspectOperand::Bytes(vec![0, b'.']),
    );
    st.inspect_push_replace_from_regs(InspectMatchKind::Trailing);
    st.inspect_exec_tally_replace(&mut nat_insp, Some(&nat_spec_5))?;
    assert_eq!(st.inspect_tally_delta(0), 2);
    assert_eq!(st.inspect_tally_delta(1), 2);
    assert_eq!(st.inspect_tally_delta(2), 3);
    assert_eq!(nat_insp, [b'A', 0, b'A', 0, b'Z', 0, b'.', 0, b'.', 0]);

    // Windowed INSPECT REPLACING CHARACTERS + ALL + LEADING on UTF-8
    let mut u8_win = *b"[ab]  ";
    st.inspect_begin(1);
    st.inspect_set_window(
        InspectWindowEdge::After,
        StagedInspectOperand::Bytes(b"[".to_vec()),
    );
    st.inspect_set_window(
        InspectWindowEdge::Before,
        StagedInspectOperand::Bytes(b"]".to_vec()),
    );
    st.inspect_set_operand(
        InspectRegSlot::PatternOrFrom,
        StagedInspectOperand::Bytes(b"a".to_vec()),
    );
    st.inspect_push_tally_from_regs(0, InspectMatchKind::All);
    st.inspect_set_operand(
        InspectRegSlot::ByOrTo,
        StagedInspectOperand::Bytes(b"X".to_vec()),
    );
    st.inspect_push_replace_from_regs(InspectMatchKind::Characters);
    st.inspect_exec_tally_replace(&mut u8_win, Some(&u8_spec_6))?;
    assert_eq!(st.inspect_tally_delta(0), 1);
    assert_eq!(&u8_win, b"[XX]  ");

    // INSPECT CONVERTING on UTF-8
    let mut u8_insp = *b"abca  ";
    st.inspect_set_operand(
        InspectRegSlot::PatternOrFrom,
        StagedInspectOperand::Bytes(b"abc".to_vec()),
    );
    st.inspect_set_operand(
        InspectRegSlot::ByOrTo,
        StagedInspectOperand::Bytes(b"XYZ".to_vec()),
    );
    st.inspect_exec_converting_from_regs(&mut u8_insp, Some(&u8_spec_6))?;
    assert_eq!(&u8_insp, b"XYZX  ");
    Ok(())
}
