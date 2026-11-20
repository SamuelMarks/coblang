//! Exhaustive unit, conformance, and 100%-branch-coverage suite for `coblang-sort` (`Milestone M16`).

use core::cmp::Ordering;
use std::fs;
use std::path::PathBuf;

use coblang_common::{DigitCount, Result, ScaleFactor};
use coblang_decimal::{
    BinaryByteOrder, BinaryLayout, BinarySignMode, BinaryTruncMode, BlankZeroMode, Decimal64Bid,
    Decimal128Bid, FixedDecimal, FloatBinary128, HighOrderPolicy, OverpunchDialect, PackedKind,
    PackedLayout, RoundingMode, SignPlacement, ZonedLayout, encode_binary, encode_comp1_ne,
    encode_comp2_ne, encode_packed, encode_zoned,
};
use coblang_sort::{
    AlphabetSpecEntry, AlphabetTable, DuplicatesOrder, KeyDirection, RecordLengthBounds,
    SortComparator, SortKeySpec, SortKeyType, SortMemoryLimit, SortReturnStatus, SortSession,
    SortSessionConfig, SortSessionState,
};

/// Creates a unique scratch directory inside the system temp directory for hermetic spill-file tests.
fn make_scratch_dir(tag: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("coblang-sort-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Verifies `AlphabetTable` native, EBCDIC CP037, slice decoding, and custom `THRU` / `ALSO` builders.
#[test]
fn alphabet_table_native_ebcdic_and_custom_specs() -> Result<()> {
    let native = AlphabetTable::default();
    assert_eq!(native, AlphabetTable::native_ascii());
    assert!(native.is_native_identity());
    assert_eq!(native.rank_of(b'A'), b'A');
    assert_eq!(native.as_ranks().len(), 256);

    let ebcdic = AlphabetTable::ebcdic_cp037();
    assert!(!ebcdic.is_native_identity());
    // In EBCDIC CP037: SPACE (0x40) < 'a' (0x81) < 'A' (0xC1) < '0' (0xF0).
    assert!(ebcdic.rank_of(b' ') < ebcdic.rank_of(b'a'));
    assert!(ebcdic.rank_of(b'a') < ebcdic.rank_of(b'A'));
    assert!(ebcdic.rank_of(b'A') < ebcdic.rank_of(b'0'));

    let roundtrip = AlphabetTable::try_from_slice(ebcdic.as_ranks())?;
    assert_eq!(roundtrip, AlphabetTable::from_ranks(*ebcdic.as_ranks()));
    assert!(AlphabetTable::try_from_slice(&[0_u8; 12]).is_err());

    // Custom ALPHABET: high-byte (>=128), reverse THRU ('Z' THRU 'A'), forward THRU ('0' THRU '2'),
    // ALSO equal-rank group, duplicate byte no-op, and already-assigned EqualGroup no-op.
    let custom = AlphabetTable::from_entries(&[
        AlphabetSpecEntry::Byte(200),
        AlphabetSpecEntry::Byte(200),
        AlphabetSpecEntry::Range {
            first: b'Z',
            last: b'A',
        },
        AlphabetSpecEntry::Range {
            first: b'0',
            last: b'2',
        },
        AlphabetSpecEntry::EqualGroup(vec![b'!', b'?', 201]),
        AlphabetSpecEntry::EqualGroup(vec![b'!', b'?']),
    ])?;
    assert_eq!(custom.rank_of(200), 0);
    assert!(custom.rank_of(b'Z') < custom.rank_of(b'A'));
    assert!(custom.rank_of(b'0') < custom.rank_of(b'2'));
    assert_eq!(custom.rank_of(b'!'), custom.rank_of(b'?'));
    assert_eq!(custom.rank_of(b'!'), custom.rank_of(201));

    // Empty EqualGroup is rejected.
    assert!(AlphabetTable::from_entries(&[AlphabetSpecEntry::EqualGroup(Vec::new())]).is_err());
    Ok(())
}

/// Verifies `KeyDirection`, `DuplicatesOrder`, and `SortKeySpec` / `SortComparator` validation errors.
#[test]
fn comparator_key_spec_validation_errors() -> Result<()> {
    assert_eq!(KeyDirection::Ascending.reverse(), KeyDirection::Descending);
    assert_eq!(KeyDirection::Descending.reverse(), KeyDirection::Ascending);
    assert_eq!(
        KeyDirection::Ascending.apply(Ordering::Less),
        Ordering::Less
    );
    assert_eq!(
        KeyDirection::Descending.apply(Ordering::Less),
        Ordering::Greater
    );

    assert_eq!(DuplicatesOrder::default(), DuplicatesOrder::Arbitrary);
    assert!(!DuplicatesOrder::Arbitrary.is_in_order());
    assert!(DuplicatesOrder::InOrder.is_in_order());
    assert_eq!(
        SortKeyType::alphanumeric_ascii(),
        SortKeyType::Alphanumeric { space_byte: b' ' }
    );
    assert_eq!(
        SortKeyType::alphanumeric_ebcdic(),
        SortKeyType::Alphanumeric { space_byte: 0x40 }
    );

    let zoned_layout = ZonedLayout {
        digits: DigitCount::try_new(4)?,
        scale: ScaleFactor::try_new(2)?,
        sign: SignPlacement::EmbeddedTrailing,
        dialect: OverpunchDialect::AsciiGnu,
        blank_zero: BlankZeroMode::RetainZero,
    };
    let packed_layout = PackedLayout {
        digits: DigitCount::try_new(5)?,
        scale: ScaleFactor::try_new(2)?,
        kind: PackedKind::Comp3Signed,
    };
    let bin_layout = BinaryLayout {
        byte_width: 4,
        digits: DigitCount::try_new(9)?,
        scale: ScaleFactor::ZERO,
        sign: BinarySignMode::Signed,
        byte_order: BinaryByteOrder::BigEndian,
        trunc: BinaryTruncMode::FullBinaryRange,
    };
    let bad_bin_layout = BinaryLayout {
        byte_width: 0,
        ..bin_layout
    };

    for invalid in [
        SortKeySpec::try_new(
            0,
            0,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        ),
        SortKeySpec::try_new(
            usize::MAX,
            2,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        ),
        SortKeySpec::try_new(0, 3, KeyDirection::Ascending, SortKeyType::NationalUtf16Be),
        SortKeySpec::try_new(
            0,
            5,
            KeyDirection::Ascending,
            SortKeyType::Zoned(zoned_layout),
        ),
        SortKeySpec::try_new(
            0,
            5,
            KeyDirection::Ascending,
            SortKeyType::Packed(packed_layout),
        ),
        SortKeySpec::try_new(
            0,
            4,
            KeyDirection::Ascending,
            SortKeyType::Binary(bad_bin_layout),
        ),
        SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Ascending,
            SortKeyType::Binary(bin_layout),
        ),
        SortKeySpec::try_new(0, 2, KeyDirection::Ascending, SortKeyType::FloatShort),
        SortKeySpec::try_new(0, 4, KeyDirection::Ascending, SortKeyType::FloatLong),
        SortKeySpec::try_new(0, 8, KeyDirection::Ascending, SortKeyType::FloatExtended),
        SortKeySpec::try_new(0, 4, KeyDirection::Ascending, SortKeyType::FloatDecimal16),
        SortKeySpec::try_new(0, 8, KeyDirection::Ascending, SortKeyType::FloatDecimal34),
    ] {
        assert!(invalid.is_err());
    }
    assert!(
        SortComparator::try_new(
            Vec::new(),
            AlphabetTable::native_ascii(),
            DuplicatesOrder::InOrder,
        )
        .is_err()
    );
    Ok(())
}

/// Verifies `Alphanumeric` (with EBCDIC collation and short-record padding) and `NationalUtf16Be` sort keys.
#[test]
fn comparator_alphanumeric_and_national_keys() -> Result<()> {
    let k_alpha = SortKeySpec::try_new(
        2,
        4,
        KeyDirection::Descending,
        SortKeyType::alphanumeric_ascii(),
    )?;
    assert_eq!(k_alpha.offset(), 2);
    assert_eq!(k_alpha.length(), 4);
    assert_eq!(k_alpha.direction(), KeyDirection::Descending);
    assert_eq!(k_alpha.key_type(), SortKeyType::alphanumeric_ascii());

    let cmp_alpha = SortComparator::try_new(
        vec![k_alpha],
        AlphabetTable::ebcdic_cp037(),
        DuplicatesOrder::InOrder,
    )?;
    assert_eq!(cmp_alpha.keys().len(), 1);
    assert_eq!(cmp_alpha.alphabet(), &AlphabetTable::ebcdic_cp037());
    assert_eq!(cmp_alpha.duplicates_order(), DuplicatesOrder::InOrder);
    assert_eq!(
        cmp_alpha.compare_checked(b"__AB", b"__AB  ")?,
        Ordering::Equal
    );
    assert_eq!(
        cmp_alpha.compare_records(b"__AB", b"__AB  "),
        Ordering::Equal
    );
    assert_eq!(
        cmp_alpha.compare_with_sequence(b"__AB", 7, b"__AB  ", 3),
        Ordering::Greater
    );
    assert_eq!(cmp_alpha.compare_checked(b"__A ", b"__a ")?, Ordering::Less);
    assert_eq!(
        cmp_alpha.compare_with_sequence(b"__A ", 9, b"__a ", 1),
        Ordering::Less
    );

    let k_nat = SortKeySpec::try_new(0, 4, KeyDirection::Ascending, SortKeyType::NationalUtf16Be)?;
    let cmp_nat = SortComparator::try_new(
        vec![k_nat],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::Arbitrary,
    )?;
    assert_eq!(
        cmp_nat.compare_checked(&[0x00, 0x41], &[0x00, 0x41, 0x00, 0x20])?,
        Ordering::Equal
    );
    assert_eq!(
        cmp_nat.compare_checked(&[0x00, 0x41, 0x00, 0x42], &[0x00, 0x41, 0x00, 0x43])?,
        Ordering::Less
    );
    assert_eq!(
        cmp_nat.compare_records(&[0x00, 0x41, 0x00, 0x42], &[0x00, 0x41, 0x00, 0x43]),
        Ordering::Less
    );
    assert_eq!(
        cmp_nat.compare_records(&[0x00, 0x41], &[0x00, 0x41, 0x00, 0x20]),
        Ordering::Equal
    );
    assert_eq!(
        cmp_nat.compare_with_sequence(&[0x00, 0x41], 1, &[0x00, 0x41, 0x00, 0x20], 9),
        Ordering::Equal
    );
    Ok(())
}

/// Verifies algebraic `Zoned` sort keys and malformed-slice total-order fallback branches.
#[test]
fn comparator_zoned_keys_and_malformed_fallback() -> Result<()> {
    let zoned_layout = ZonedLayout {
        digits: DigitCount::try_new(4)?,
        scale: ScaleFactor::try_new(2)?,
        sign: SignPlacement::EmbeddedTrailing,
        dialect: OverpunchDialect::AsciiGnu,
        blank_zero: BlankZeroMode::RetainZero,
    };
    let neg_v = FixedDecimal::parse_str("-12.34")?;
    let pos_v = FixedDecimal::parse_str("5.60")?;
    let z_neg = encode_zoned(
        &neg_v,
        zoned_layout,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?
    .0;
    let z_pos = encode_zoned(
        &pos_v,
        zoned_layout,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?
    .0;
    let cmp_zoned = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            4,
            KeyDirection::Ascending,
            SortKeyType::Zoned(zoned_layout),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    assert_eq!(cmp_zoned.compare_checked(&z_neg, &z_pos)?, Ordering::Less);
    assert!(cmp_zoned.compare_checked(&z_neg, b"@@").is_err());
    assert!(cmp_zoned.compare_checked(b"@@", &z_pos).is_err());
    assert_eq!(cmp_zoned.compare_records(&z_neg, &z_pos), Ordering::Less);
    assert_eq!(cmp_zoned.compare_records(&z_neg, b"@@@@"), Ordering::Less);
    assert_eq!(
        cmp_zoned.compare_records(b"@@@@", &z_pos),
        Ordering::Greater
    );
    assert_eq!(
        cmp_zoned.compare_records(b"@@@@", b"????"),
        Ordering::Greater
    );
    Ok(())
}

/// Verifies algebraic `Packed` (`COMP-3`) and `Binary` (`COMP`) sort keys.
#[test]
fn comparator_packed_and_binary_keys() -> Result<()> {
    let packed_layout = PackedLayout {
        digits: DigitCount::try_new(5)?,
        scale: ScaleFactor::try_new(2)?,
        kind: PackedKind::Comp3Signed,
    };
    let bin_layout = BinaryLayout {
        byte_width: 4,
        digits: DigitCount::try_new(9)?,
        scale: ScaleFactor::ZERO,
        sign: BinarySignMode::Signed,
        byte_order: BinaryByteOrder::BigEndian,
        trunc: BinaryTruncMode::FullBinaryRange,
    };
    let neg_v = FixedDecimal::parse_str("-12.34")?;
    let pos_v = FixedDecimal::parse_str("5.60")?;
    let p_neg = encode_packed(
        &neg_v,
        packed_layout,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?
    .0;
    let p_pos = encode_packed(
        &pos_v,
        packed_layout,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?
    .0;
    let cmp_packed = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            3,
            KeyDirection::Ascending,
            SortKeyType::Packed(packed_layout),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    assert_eq!(cmp_packed.compare_checked(&p_neg, &p_pos)?, Ordering::Less);
    assert!(
        cmp_packed
            .compare_checked(&[0xFF, 0xFF, 0xFF], &p_pos)
            .is_err()
    );

    let b_neg = encode_binary(
        &FixedDecimal::from_i64(-500),
        bin_layout,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?
    .0;
    let b_pos = encode_binary(
        &FixedDecimal::from_i64(42),
        bin_layout,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?
    .0;
    let cmp_bin = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            4,
            KeyDirection::Ascending,
            SortKeyType::Binary(bin_layout),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    assert_eq!(cmp_bin.compare_checked(&b_neg, &b_pos)?, Ordering::Less);
    assert!(cmp_bin.compare_checked(&b_neg, &[1, 2]).is_err());
    Ok(())
}

/// Verifies algebraic `FloatShort`, `FloatLong`, `FloatExtended`, `FloatDecimal16`, and `FloatDecimal34` keys.
#[test]
fn comparator_binary_and_decimal_float_keys() -> Result<()> {
    let neg_v = FixedDecimal::parse_str("-12.34")?;
    let pos_v = FixedDecimal::parse_str("5.60")?;

    let f32_neg = encode_comp1_ne(&neg_v);
    let f32_pos = encode_comp1_ne(&pos_v);
    let cmp_f32 = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            4,
            KeyDirection::Ascending,
            SortKeyType::FloatShort,
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    assert_eq!(cmp_f32.compare_checked(&f32_neg, &f32_pos)?, Ordering::Less);
    assert_eq!(
        cmp_f32.compare_checked(&(-0.0_f32).to_ne_bytes(), &(0.0_f32).to_ne_bytes())?,
        Ordering::Equal
    );
    assert!(cmp_f32.compare_checked(&[0, 1], &f32_pos).is_err());
    assert!(
        cmp_f32
            .compare_checked(&f32::NAN.to_ne_bytes(), &f32_pos)
            .is_err()
    );

    let f64_neg = encode_comp2_ne(&neg_v);
    let f64_pos = encode_comp2_ne(&pos_v);
    let cmp_f64 = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            8,
            KeyDirection::Ascending,
            SortKeyType::FloatLong,
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    assert_eq!(cmp_f64.compare_checked(&f64_neg, &f64_pos)?, Ordering::Less);
    assert!(cmp_f64.compare_checked(&[0; 3], &f64_pos).is_err());
    assert!(
        cmp_f64
            .compare_checked(&f64::NAN.to_ne_bytes(), &f64_pos)
            .is_err()
    );

    let f128_neg = FloatBinary128::from_f64(-12.5)?.to_ne_bytes();
    let f128_pos = FloatBinary128::from_f64(3.25)?.to_ne_bytes();
    let f128_nan = (0x7FFF_8000_0000_0000_0000_0000_0000_0001_u128).to_ne_bytes();
    let cmp_f128 = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            16,
            KeyDirection::Ascending,
            SortKeyType::FloatExtended,
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    assert_eq!(
        cmp_f128.compare_checked(&f128_neg, &f128_pos)?,
        Ordering::Less
    );
    assert!(cmp_f128.compare_checked(&[0; 5], &f128_pos).is_err());
    assert!(cmp_f128.compare_checked(&f128_nan, &f128_pos).is_err());
    Ok(())
}

/// Verifies algebraic `FloatDecimal16` (`BID64`) and `FloatDecimal34` (`BID128`) sort keys.
#[test]
fn comparator_decimal_float_keys() -> Result<()> {
    let neg_v = FixedDecimal::parse_str("-12.34")?;
    let pos_v = FixedDecimal::parse_str("5.60")?;

    let bid64_neg =
        Decimal64Bid::from_fixed_decimal(&neg_v, RoundingMode::NearestEven)?.to_ne_bytes();
    let bid64_pos =
        Decimal64Bid::from_fixed_decimal(&pos_v, RoundingMode::NearestEven)?.to_ne_bytes();
    let bid64_nan = (0x7C00_0000_0000_0000_u64).to_ne_bytes();
    let cmp_bid64 = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            8,
            KeyDirection::Ascending,
            SortKeyType::FloatDecimal16,
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    assert_eq!(
        cmp_bid64.compare_checked(&bid64_neg, &bid64_pos)?,
        Ordering::Less
    );
    assert!(cmp_bid64.compare_checked(&[0; 2], &bid64_pos).is_err());
    assert!(cmp_bid64.compare_checked(&bid64_nan, &bid64_pos).is_err());

    let bid128_neg =
        Decimal128Bid::from_fixed_decimal(&neg_v, RoundingMode::NearestEven)?.to_ne_bytes();
    let bid128_pos =
        Decimal128Bid::from_fixed_decimal(&pos_v, RoundingMode::NearestEven)?.to_ne_bytes();
    let bid128_nan = (0x7C00_0000_0000_0000_0000_0000_0000_0000_u128).to_ne_bytes();
    let cmp_bid128 = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            16,
            KeyDirection::Ascending,
            SortKeyType::FloatDecimal34,
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    assert_eq!(
        cmp_bid128.compare_checked(&bid128_neg, &bid128_pos)?,
        Ordering::Less
    );
    assert!(cmp_bid128.compare_checked(&[0; 6], &bid128_pos).is_err());
    assert!(
        cmp_bid128
            .compare_checked(&bid128_nan, &bid128_pos)
            .is_err()
    );
    Ok(())
}

/// Verifies multi-key cascading tie-breaking and `sort_fixed_stride_table` (both stable and unstable).
#[test]
fn multi_key_cascading_and_fixed_stride_table_sort() -> Result<()> {
    // Major key: bytes 0..2 ASCENDING; Minor key: bytes 2..4 DESCENDING; Payload: byte 4.
    let cmp_stable = SortComparator::try_new(
        vec![
            SortKeySpec::try_new(
                0,
                2,
                KeyDirection::Ascending,
                SortKeyType::alphanumeric_ascii(),
            )?,
            SortKeySpec::try_new(
                2,
                2,
                KeyDirection::Descending,
                SortKeyType::alphanumeric_ascii(),
            )?,
        ],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;

    assert!(cmp_stable.sort_fixed_stride_table(&mut [], 0, 0).is_err());
    assert!(
        cmp_stable
            .sort_fixed_stride_table(&mut [], usize::MAX, 2)
            .is_err()
    );
    let mut tiny = [0_u8; 4];
    assert!(cmp_stable.sort_fixed_stride_table(&mut tiny, 2, 5).is_err());
    cmp_stable.sort_fixed_stride_table(&mut tiny, 0, 4)?;
    cmp_stable.sort_fixed_stride_table(&mut tiny, 1, 4)?;

    let mut table = Vec::new();
    for rec in [b"BB10a", b"AA10x", b"AA20y", b"AA10z"] {
        table.extend_from_slice(rec);
    }
    cmp_stable.sort_fixed_stride_table(&mut table, 4, 5)?;
    assert_eq!(&table, b"AA20yAA10xAA10zBB10a");

    let cmp_unstable = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::Arbitrary,
    )?;
    let mut table2 = b"CCDDAA".to_vec();
    cmp_unstable.sort_fixed_stride_table(&mut table2, 3, 2)?;
    assert_eq!(&table2, b"AACCDD");
    Ok(())
}

/// Verifies `SortMemoryLimit`, `RecordLengthBounds`, `SortReturnStatus`, and `SortSessionConfig` parsing/validation.
#[test]
fn memory_limit_record_bounds_and_config_validation() -> Result<()> {
    assert_eq!(SortMemoryLimit::default(), SortMemoryLimit::DEFAULT);
    assert_eq!(SortMemoryLimit::DEFAULT.as_bytes(), 128 * 1024 * 1024);
    assert!(SortMemoryLimit::try_new(0).is_err());
    assert_eq!(SortMemoryLimit::try_new(256)?.as_bytes(), 256);

    assert_eq!(SortMemoryLimit::parse_str(" 512 ")?.as_bytes(), 512);
    assert_eq!(SortMemoryLimit::parse_str("512B")?.as_bytes(), 512);
    assert_eq!(SortMemoryLimit::parse_str("4k")?.as_bytes(), 4096);
    assert_eq!(SortMemoryLimit::parse_str("4KB")?.as_bytes(), 4096);
    assert_eq!(SortMemoryLimit::parse_str("4KiB")?.as_bytes(), 4096);
    assert_eq!(
        SortMemoryLimit::parse_str("2M")?.as_bytes(),
        2 * 1024 * 1024
    );
    assert_eq!(
        SortMemoryLimit::parse_str("2MiB")?.as_bytes(),
        2 * 1024 * 1024
    );
    assert_eq!(
        SortMemoryLimit::parse_str("1G")?.as_bytes(),
        1024 * 1024 * 1024
    );
    assert!(SortMemoryLimit::parse_str("").is_err());
    assert!(SortMemoryLimit::parse_str("0M").is_err());
    assert!(SortMemoryLimit::parse_str("16T").is_err());
    assert!(SortMemoryLimit::parse_str(&format!("{}G", usize::MAX)).is_err());

    assert_eq!(
        SortMemoryLimit::from_env_value(None)?,
        SortMemoryLimit::DEFAULT
    );
    assert_eq!(
        SortMemoryLimit::from_env_value(Some("8K"))?.as_bytes(),
        8192
    );
    assert!(SortMemoryLimit::from_env_value(Some("bad")).is_err());
    let _ = SortMemoryLimit::from_env()?;

    assert!(RecordLengthBounds::try_new(0, 10).is_err());
    assert!(RecordLengthBounds::try_new(12, 10).is_err());
    assert!(RecordLengthBounds::try_new(1, RecordLengthBounds::MAX_RECORD_BYTES + 1).is_err());
    let bounds = RecordLengthBounds::fixed(8)?;
    assert_eq!(bounds.min_bytes(), 8);
    assert_eq!(bounds.max_bytes(), 8);
    bounds.validate_record(b"12345678")?;
    assert!(bounds.validate_record(b"short").is_err());
    assert!(bounds.validate_record(b"way-too-long").is_err());

    assert_eq!(SortReturnStatus::default(), SortReturnStatus::Success0);
    assert_eq!(SortReturnStatus::from_code(0), SortReturnStatus::Success0);
    assert_eq!(SortReturnStatus::from_code(16), SortReturnStatus::Failed16);
    assert_eq!(SortReturnStatus::Success0.as_code(), 0);
    assert_eq!(SortReturnStatus::Failed16.as_code(), 16);
    assert!(!SortReturnStatus::Success0.is_failed());
    assert!(SortReturnStatus::Failed16.is_failed());

    let dir = make_scratch_dir("cfg")?;
    let cfg = SortSessionConfig::new(bounds)
        .with_memory_limit(SortMemoryLimit::try_new(1024)?)
        .with_spill_dir(dir.clone())
        .with_max_merge_fan_in(4)?;
    assert_eq!(cfg.record_bounds(), bounds);
    assert_eq!(cfg.memory_limit().as_bytes(), 1024);
    assert_eq!(cfg.spill_dir(), dir.as_path());
    assert_eq!(cfg.max_merge_fan_in(), 4);
    assert!(
        SortSessionConfig::new(bounds)
            .with_max_merge_fan_in(1)
            .is_err()
    );

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies pure in-memory `SortSession` (both `InOrder` and `Arbitrary`), `SORT-RETURN`, and state guards.
#[test]
fn sort_session_in_memory_and_lifecycle_guards() -> Result<()> {
    let dir = make_scratch_dir("inmem")?;
    let cmp_stable = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    let cfg = SortSessionConfig::new(RecordLengthBounds::try_new(3, 6)?)
        .with_memory_limit(SortMemoryLimit::try_new(4096)?)
        .with_spill_dir(dir.clone());

    let mut session = SortSession::new_sort(cmp_stable.clone(), cfg.clone());
    assert_eq!(session.state(), SortSessionState::AcceptingInput);
    assert_eq!(session.sort_return(), SortReturnStatus::Success0);
    assert_eq!(session.comparator(), &cmp_stable);
    assert_eq!(session.config(), &cfg);
    assert!(session.return_next().is_err());
    assert!(session.collect_remaining().is_err());
    assert!(session.begin_merge_run().is_err());
    assert!(session.push_merge_record(b"AA1").is_err());
    assert!(session.release(b"X").is_err());

    session.set_sort_return(SortReturnStatus::Success0);
    for rec in [&b"BB1"[..], b"AA1", b"AA2", b"BB2", b"AA3"] {
        session.release(rec)?;
    }
    assert_eq!(session.spilled_run_count(), 0);
    assert!(!session.session_spill_dir().exists());

    session.finish_input()?;
    assert_eq!(session.state(), SortSessionState::ReturningOutput);
    assert!(session.finish_input().is_err());
    assert!(session.release(b"CC1").is_err());

    let sorted = session.collect_remaining()?;
    assert_eq!(
        sorted,
        vec![
            b"AA1".to_vec(),
            b"AA2".to_vec(),
            b"AA3".to_vec(),
            b"BB1".to_vec(),
            b"BB2".to_vec(),
        ]
    );
    assert_eq!(session.state(), SortSessionState::Completed);
    assert_eq!(session.return_next()?, None);
    session.close();
    assert_eq!(session.state(), SortSessionState::Completed);

    // Unstable Arbitrary in-memory path + early close + SORT-RETURN = 16 abort.
    let cmp_arb = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Descending,
            SortKeyType::alphanumeric_ascii(),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::Arbitrary,
    )?;
    let mut s_arb = SortSession::new_sort(cmp_arb, cfg);
    s_arb.release(b"AA1")?;
    s_arb.release(b"CC1")?;
    s_arb.release(b"BB1")?;
    s_arb.finish_input()?;
    assert_eq!(s_arb.return_next()?, Some(b"CC1".to_vec()));
    s_arb.set_sort_return(SortReturnStatus::Failed16);
    assert_eq!(s_arb.state(), SortSessionState::Aborted);
    assert_eq!(s_arb.sort_return(), SortReturnStatus::Failed16);
    assert!(s_arb.return_next().is_err());
    assert!(s_arb.finish_input().is_err());
    s_arb.close();
    assert_eq!(s_arb.state(), SortSessionState::Aborted);

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies external spill-file sorting, multi-pass compaction (`fan_in = 2`), and global `WITH DUPLICATES IN ORDER`.
#[test]
fn sort_session_external_spill_multi_pass_compaction_and_stability() -> Result<()> {
    let dir = make_scratch_dir("spill")?;
    // Key on first 2 bytes; bytes 2..6 carry a 4-digit arrival sequence tag ("0000".."0029").
    let cmp = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    // Each record is 6 bytes; limit = 15 bytes -> max 2 records per in-memory run!
    // 11 records -> 6 spilled runs; fan_in = 2 forces multi-pass compaction (6 -> 3 -> 2 -> final)!
    let cfg = SortSessionConfig::new(RecordLengthBounds::fixed(6)?)
        .with_memory_limit(SortMemoryLimit::try_new(15)?)
        .with_spill_dir(dir.clone())
        .with_max_merge_fan_in(2)?;

    let mut session = SortSession::new_sort(cmp, cfg);
    let keys = [
        "CC", "AA", "BB", "AA", "CC", "BB", "AA", "BB", "CC", "AA", "BB",
    ];
    for (seq, k) in keys.iter().enumerate() {
        let rec = format!("{k}{seq:04}");
        session.release(rec.as_bytes())?;
    }
    // 5 spills happened during `release`, plus 1 trailing spill during `finish_input` = 6 total runs.
    assert_eq!(session.spilled_run_count(), 5);
    let spill_dir_path = session.session_spill_dir().to_path_buf();
    assert!(spill_dir_path.is_dir());

    session.finish_input()?;
    assert_eq!(session.spilled_run_count(), 6);

    let out = session.collect_remaining()?;
    let expected = vec![
        b"AA0001".to_vec(),
        b"AA0003".to_vec(),
        b"AA0006".to_vec(),
        b"AA0009".to_vec(),
        b"BB0002".to_vec(),
        b"BB0005".to_vec(),
        b"BB0007".to_vec(),
        b"BB0010".to_vec(),
        b"CC0000".to_vec(),
        b"CC0004".to_vec(),
        b"CC0008".to_vec(),
    ];
    assert_eq!(out, expected);
    // Spill directory is automatically removed when ReturningOutput reaches EOF!
    assert!(!spill_dir_path.exists());

    // Also verify 5 runs with fan_in = 2 (exercises the odd-remainder `[single_path]` chunk arm!).
    let cmp2 = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    let cfg2 = SortSessionConfig::new(RecordLengthBounds::fixed(4)?)
        .with_memory_limit(SortMemoryLimit::try_new(4)?)
        .with_spill_dir(dir.clone())
        .with_max_merge_fan_in(2)?;
    let mut s5 = SortSession::new_sort(cmp2, cfg2);
    for rec in [b"EE01", b"DD02", b"CC03", b"BB04", b"AA05"] {
        s5.release(rec)?;
    }
    s5.finish_input()?;
    assert_eq!(s5.spilled_run_count(), 5);
    assert_eq!(
        s5.collect_remaining()?,
        vec![
            b"AA05".to_vec(),
            b"BB04".to_vec(),
            b"CC03".to_vec(),
            b"DD02".to_vec(),
            b"EE01".to_vec(),
        ]
    );

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies `MERGE` sessions across multiple pre-sorted streams (including empty streams and ISO `USING` tie precedence).
#[test]
fn merge_session_multi_stream_tie_precedence_and_empty_runs() -> Result<()> {
    let dir = make_scratch_dir("merge")?;
    let cmp = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    let cfg = SortSessionConfig::new(RecordLengthBounds::fixed(4)?).with_spill_dir(dir.clone());

    // Empty MERGE session (zero runs) transitions cleanly and yields None.
    let mut empty_merge = SortSession::new_merge(cmp.clone(), cfg.clone());
    assert_eq!(empty_merge.state(), SortSessionState::AcceptingMergeRuns);
    assert!(empty_merge.release(b"AA01").is_err());
    assert!(empty_merge.push_merge_record(b"AA01").is_err());
    empty_merge.finish_input()?;
    assert_eq!(empty_merge.return_next()?, None);

    // 5-stream MERGE (4 non-empty + 1 empty, exercising 3+ node heap sift-down left vs right child + USING file precedence).
    let mut merge = SortSession::new_merge(cmp, cfg);
    // Stream 0 via streaming begin_merge_run + push_merge_record (left open for next begin_merge_run to flush!).
    merge.begin_merge_run()?;
    assert!(merge.push_merge_record(b"X").is_err());
    merge.push_merge_record(b"BBf0")?;
    merge.push_merge_record(b"FFf0")?;
    // Stream 1: empty run!
    merge.begin_merge_run()?;
    // Stream 2: batch add_merge_run.
    merge.add_merge_run(&[b"AAf2".to_vec(), b"DDf2".to_vec(), b"GGf2".to_vec()])?;
    // Stream 3: batch add_merge_run (smaller head than stream 0/4 so right child < left child is exercised!).
    merge.add_merge_run(&[b"BBf3".to_vec(), b"CCf3".to_vec()])?;
    // Stream 4: open trailing streaming run flushed by finish_input.
    merge.begin_merge_run()?;
    merge.push_merge_record(b"AAf4")?;
    merge.push_merge_record(b"BBf4")?;
    merge.push_merge_record(b"EEf4")?;

    merge.finish_input()?;
    assert!(merge.begin_merge_run().is_err());
    let merged = merge.collect_remaining()?;
    assert_eq!(
        merged,
        vec![
            b"AAf2".to_vec(),
            b"AAf4".to_vec(),
            b"BBf0".to_vec(),
            b"BBf3".to_vec(),
            b"BBf4".to_vec(),
            b"CCf3".to_vec(),
            b"DDf2".to_vec(),
            b"EEf4".to_vec(),
            b"FFf0".to_vec(),
            b"GGf2".to_vec(),
        ]
    );

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises 4-way and 6-way non-empty merges so `MergeHeap::pop_min` covers both `right_smaller` and interior `break` branches.
#[test]
fn merge_heap_sift_down_right_child_and_interior_stop() -> Result<()> {
    let dir = make_scratch_dir("heap-sift")?;
    let cmp = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    let cfg = SortSessionConfig::new(RecordLengthBounds::fixed(4)?).with_spill_dir(dir.clone());

    let mut merge_right = SortSession::new_merge(cmp.clone(), cfg.clone());
    for rec in [b"DDf0", b"AAf1", b"BBf2", b"EEf3"] {
        merge_right.add_merge_run(&[rec.to_vec()])?;
    }
    merge_right.finish_input()?;
    assert_eq!(
        merge_right.collect_remaining()?,
        vec![
            b"AAf1".to_vec(),
            b"BBf2".to_vec(),
            b"DDf0".to_vec(),
            b"EEf3".to_vec(),
        ]
    );

    // 6-stream merge ["AAf0", "BBf1", "CCf2", "FFf3", "GGf4", "EEf5"]:
    // Popping "AAf0" moves "EEf5" (from subtree 2) to root, swaps with "BBf1" (idx 1),
    // and stops above idx 1's children ("FFf3", "GGf4") because "EEf5" < "FFf3".
    let mut merge_six = SortSession::new_merge(cmp, cfg);
    for rec in [b"AAf0", b"BBf1", b"CCf2", b"FFf3", b"GGf4", b"EEf5"] {
        merge_six.add_merge_run(&[rec.to_vec()])?;
    }
    merge_six.finish_input()?;
    assert_eq!(
        merge_six.collect_remaining()?,
        vec![
            b"AAf0".to_vec(),
            b"BBf1".to_vec(),
            b"CCf2".to_vec(),
            b"EEf5".to_vec(),
            b"FFf3".to_vec(),
            b"GGf4".to_vec(),
        ]
    );

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies deterministic directory-creation and spill-file write error handling in `SortSession`.
#[test]
fn spill_directory_and_write_fault_detection() -> Result<()> {
    let dir = make_scratch_dir("write-faults")?;
    let cmp = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    let bounds = RecordLengthBounds::try_new(4, 8)?;

    let blocker_file = dir.join("not_a_directory");
    fs::write(&blocker_file, b"file")?;
    let bad_dir_cfg = SortSessionConfig::new(bounds)
        .with_memory_limit(SortMemoryLimit::try_new(4)?)
        .with_spill_dir(blocker_file);

    let mut s_dir_err = SortSession::new_sort(cmp.clone(), bad_dir_cfg.clone());
    s_dir_err.release(b"AAAA")?;
    assert!(s_dir_err.release(b"BBBB").is_err());

    let mut m_dir_err = SortSession::new_merge(cmp.clone(), bad_dir_cfg);
    assert!(m_dir_err.add_merge_run(&[b"AAAA".to_vec()]).is_err());

    let ok_cfg = SortSessionConfig::new(bounds)
        .with_memory_limit(SortMemoryLimit::try_new(4)?)
        .with_spill_dir(dir.clone());

    let mut m_write_err = SortSession::new_merge(cmp.clone(), ok_cfg.clone());
    fs::create_dir_all(m_write_err.next_spill_file_path())?;
    m_write_err.begin_merge_run()?;
    m_write_err.push_merge_record(b"AAAA")?;
    assert!(m_write_err.begin_merge_run().is_err());

    let mut m_fin_err = SortSession::new_merge(cmp.clone(), ok_cfg.clone());
    fs::create_dir_all(m_fin_err.next_spill_file_path())?;
    m_fin_err.begin_merge_run()?;
    m_fin_err.push_merge_record(b"AAAA")?;
    assert!(m_fin_err.finish_input().is_err());

    let mut s_fin_err = SortSession::new_sort(cmp, ok_cfg);
    s_fin_err.release(b"BBBB")?;
    s_fin_err.release(b"AAAA")?;
    fs::create_dir_all(s_fin_err.next_spill_file_path())?;
    assert!(s_fin_err.finish_input().is_err());

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies missing-file, bad-magic, out-of-bounds length, truncated-frame, and compaction read fault detection.
#[test]
fn spill_corrupt_frame_and_compaction_fault_detection() -> Result<()> {
    let dir = make_scratch_dir("read-faults")?;
    let cmp = SortComparator::try_new(
        vec![SortKeySpec::try_new(
            0,
            2,
            KeyDirection::Ascending,
            SortKeyType::alphanumeric_ascii(),
        )?],
        AlphabetTable::native_ascii(),
        DuplicatesOrder::InOrder,
    )?;
    let bounds = RecordLengthBounds::try_new(4, 8)?;
    let ok_cfg = SortSessionConfig::new(bounds)
        .with_memory_limit(SortMemoryLimit::try_new(4)?)
        .with_spill_dir(dir.clone());

    let mut s_missing = SortSession::new_sort(cmp.clone(), ok_cfg.clone());
    let run0_missing = s_missing.next_spill_file_path();
    s_missing.release(b"BBBB")?;
    s_missing.release(b"AAAA")?;
    fs::remove_file(&run0_missing)?;
    assert!(s_missing.finish_input().is_err());

    let mut m_missing = SortSession::new_merge(cmp.clone(), ok_cfg.clone());
    let m_run0 = m_missing.next_spill_file_path();
    m_missing.add_merge_run(&[b"AAAA".to_vec()])?;
    fs::remove_file(&m_run0)?;
    assert!(m_missing.finish_input().is_err());

    for corrupt_payload in [
        b"COB".to_vec(),
        b"BADMAGIC".to_vec(),
        b"COBSRUN1\x01".to_vec(),
        [&b"COBSRUN1"[..], &2_u32.to_le_bytes()].concat(),
        [&b"COBSRUN1"[..], &20_u32.to_le_bytes()].concat(),
        [&b"COBSRUN1"[..], &4_u32.to_le_bytes(), b"AA"].concat(),
    ] {
        let mut m_corrupt = SortSession::new_merge(cmp.clone(), ok_cfg.clone());
        let p_corrupt = m_corrupt.next_spill_file_path();
        m_corrupt.add_merge_run(&[b"AAAA".to_vec()])?;
        fs::write(&p_corrupt, corrupt_payload)?;
        assert!(m_corrupt.finish_input().is_err());
    }

    let mut bad_second_frame = Vec::from(&b"COBSRUN1"[..]);
    bad_second_frame.extend_from_slice(&4_u32.to_le_bytes());
    bad_second_frame.extend_from_slice(b"AAAA");
    bad_second_frame.extend_from_slice(&4_u32.to_le_bytes());
    bad_second_frame.extend_from_slice(b"B");

    let mut m_trunc2 = SortSession::new_merge(cmp.clone(), ok_cfg.clone());
    let p_trunc2 = m_trunc2.next_spill_file_path();
    m_trunc2.add_merge_run(&[b"AAAA".to_vec()])?;
    fs::write(&p_trunc2, &bad_second_frame)?;
    m_trunc2.finish_input()?;
    assert!(m_trunc2.collect_remaining().is_err());

    let compact_cfg = ok_cfg.with_max_merge_fan_in(2)?;
    let mut m_comp_open_err = SortSession::new_merge(cmp.clone(), compact_cfg.clone());
    let missing_first_run = m_comp_open_err.next_spill_file_path();
    m_comp_open_err.add_merge_run(&[b"AAAA".to_vec()])?;
    m_comp_open_err.add_merge_run(&[b"BBBB".to_vec()])?;
    m_comp_open_err.add_merge_run(&[b"CCCC".to_vec()])?;
    fs::remove_file(&missing_first_run)?;
    assert!(m_comp_open_err.finish_input().is_err());

    let mut m_comp_next_err = SortSession::new_merge(cmp, compact_cfg);
    let corrupt_second_rec_run = m_comp_next_err.next_spill_file_path();
    m_comp_next_err.add_merge_run(&[b"AAAA".to_vec()])?;
    m_comp_next_err.add_merge_run(&[b"BBBB".to_vec()])?;
    m_comp_next_err.add_merge_run(&[b"CCCC".to_vec()])?;
    fs::write(&corrupt_second_rec_run, &bad_second_frame)?;
    assert!(m_comp_next_err.finish_input().is_err());

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}
