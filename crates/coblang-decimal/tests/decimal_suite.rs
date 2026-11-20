//! Exhaustive unit and branch-coverage suite for `coblang-decimal`.

use core::cmp::Ordering;

use coblang_common::{DigitCount, Result, ScaleFactor};
use coblang_decimal::{
    BinaryByteOrder, BinaryLayout, BinarySignMode, BinaryTruncMode, BlankZeroMode, Decimal64Bid,
    Decimal128Bid, DecimalSign, FixedDecimal, FloatBinary128, HighOrderPolicy, OverpunchDialect,
    PackedKind, PackedLayout, PackedSignPolicy, RoundingMode, SignPlacement, SignedInt256,
    SignednessPolicy, SizeErrorStatus, Uint256, ZonedLayout, decode_binary, decode_comp1_ne,
    decode_comp2_ne, decode_packed, decode_zoned, encode_binary, encode_comp1_ne, encode_comp2_ne,
    encode_packed, encode_zoned, is_valid_packed_bcd, is_valid_zoned_numeric, pack_dpd_declet,
    round_quotient_with_remainder, unpack_dpd_declet,
};

/// Verifies `DecimalSign` helpers, `Uint256` limb arithmetic, borrow/carry chains, division, and decimal formatting.
#[test]
fn wide_int_uint256_and_sign_operations() -> Result<()> {
    assert!(DecimalSign::Positive.is_positive());
    assert!(!DecimalSign::Positive.is_negative());
    assert!(!DecimalSign::Positive.is_zero());
    assert!(DecimalSign::Negative.is_negative());
    assert!(DecimalSign::UnsignedZero.is_zero());
    assert_eq!(DecimalSign::Positive.negate(), DecimalSign::Negative);
    assert_eq!(DecimalSign::Negative.negate(), DecimalSign::Positive);
    assert_eq!(
        DecimalSign::UnsignedZero.negate(),
        DecimalSign::UnsignedZero
    );
    assert_eq!(
        DecimalSign::Positive.mul_sign(DecimalSign::Negative),
        DecimalSign::Negative
    );
    assert_eq!(
        DecimalSign::Negative.mul_sign(DecimalSign::Negative),
        DecimalSign::Positive
    );
    assert_eq!(
        DecimalSign::UnsignedZero.mul_sign(DecimalSign::Negative),
        DecimalSign::UnsignedZero
    );
    assert_eq!(
        DecimalSign::Positive.mul_sign(DecimalSign::UnsignedZero),
        DecimalSign::UnsignedZero
    );

    let z = Uint256::ZERO;
    assert!(z.is_zero());
    assert!(!z.is_odd());
    assert!(Uint256::ONE.is_odd());
    assert_eq!(z.to_string(), "0");
    assert_eq!(z.limbs(), [0, 0, 0, 0]);

    let u128_max = Uint256::from_u128(u128::MAX);
    assert_eq!(u128_max.to_u128(), Some(u128::MAX));
    assert_eq!(Uint256::from_limbs([0, 0, 1, 0]).to_u128(), None);
    assert_eq!(Uint256::from_limbs([0, 0, 0, 1]).to_u128(), None);

    // Addition & subtraction across all 4 limb carry/borrow propagation edges.
    assert_eq!(Uint256::MAX.checked_add(Uint256::ONE), None);
    let carry_chain = Uint256::from_limbs([u64::MAX, u64::MAX, u64::MAX, 0]);
    let added = carry_chain.checked_add(Uint256::ONE);
    assert_eq!(added, Some(Uint256::from_limbs([0, 0, 0, 1])));
    assert_eq!(
        added.and_then(|v| v.checked_sub(Uint256::ONE)),
        Some(carry_chain)
    );
    assert_eq!(Uint256::ZERO.checked_sub(Uint256::ONE), None);
    assert_eq!(
        Uint256::from_limbs([5, 0, 0, 1]).checked_sub(Uint256::from_limbs([0, 1, 1, 0])),
        Some(Uint256::from_limbs([5, u64::MAX, u64::MAX - 1, 0]))
    );
    assert_eq!(
        Uint256::from_limbs([5, 0, 0, 0]).checked_sub(Uint256::from_limbs([0, 0, 0, 1])),
        None
    );

    // Ordering across limbs 3, 2, 1, 0.
    assert!(Uint256::from_limbs([0, 0, 0, 2]) > Uint256::from_limbs([9, 9, 9, 1]));
    assert!(Uint256::from_limbs([0, 0, 2, 1]) > Uint256::from_limbs([9, 9, 1, 1]));
    assert!(Uint256::from_limbs([0, 2, 1, 1]) > Uint256::from_limbs([9, 1, 1, 1]));
    assert_eq!(
        Uint256::ONE.partial_cmp(&Uint256::ZERO),
        Some(Ordering::Greater)
    );

    wide_int_mul_div_and_pow10()
}

/// Verifies `Uint256` multiplication overflow paths, binary long division across all 4 limbs, `pow10`, and `SignedInt256`.
fn wide_int_mul_div_and_pow10() -> Result<()> {
    assert_eq!(Uint256::ZERO.checked_mul(Uint256::ONE), Some(Uint256::ZERO));
    assert_eq!(Uint256::ONE.checked_mul(Uint256::ZERO), Some(Uint256::ZERO));
    assert_eq!(Uint256::MAX.checked_mul_u64(2), None);
    // Shift overflow at limb 1, 2, 3 and accumulation overflow.
    assert_eq!(
        Uint256::from_limbs([1, 0, 0, 1]).checked_mul(Uint256::from_limbs([0, 1, 0, 0])),
        None
    );
    assert_eq!(
        Uint256::from_limbs([1, 0, 1, 0]).checked_mul(Uint256::from_limbs([0, 0, 1, 0])),
        None
    );
    assert_eq!(
        Uint256::from_limbs([1, 0, 0, 1]).checked_mul(Uint256::from_limbs([0, 0, 1, 0])),
        None
    );
    assert_eq!(
        Uint256::from_limbs([1, 1, 0, 0]).checked_mul(Uint256::from_limbs([0, 0, 0, 1])),
        None
    );
    assert_eq!(
        Uint256::from_limbs([1, 0, 1, 0]).checked_mul(Uint256::from_limbs([0, 0, 0, 1])),
        None
    );
    assert_eq!(
        Uint256::from_limbs([1, 0, 0, 1]).checked_mul(Uint256::from_limbs([0, 0, 0, 1])),
        None
    );
    assert_eq!(
        Uint256::from_limbs([2, 0, 0, 0]).checked_mul(Uint256::from_limbs([1, 1, 1, 1])),
        Some(Uint256::from_limbs([2, 2, 2, 2]))
    );
    assert_eq!(
        Uint256::from_limbs([u64::MAX, u64::MAX, u64::MAX, u64::MAX / 2 + 1])
            .checked_mul(Uint256::from_limbs([1, 1, 0, 0])),
        None
    );

    // Division & cmp_double_to.
    assert_eq!(Uint256::ONE.div_rem_u64(0), None);
    assert!(Uint256::ONE.checked_div_rem(Uint256::ZERO).is_err());
    assert_eq!(
        Uint256::from_u64(3).checked_div_rem(Uint256::from_u64(7))?,
        (Uint256::ZERO, Uint256::from_u64(3))
    );
    for limb_pos in [
        Uint256::from_limbs([25, 0, 0, 0]),
        Uint256::from_limbs([0, 25, 0, 0]),
        Uint256::from_limbs([0, 0, 25, 0]),
        Uint256::from_limbs([0, 0, 0, 25]),
    ] {
        let (q, r) = limb_pos.checked_div_rem(Uint256::from_u64(7))?;
        let recomposed = q
            .checked_mul(Uint256::from_u64(7))
            .and_then(|p| p.checked_add(r));
        assert_eq!(recomposed, Some(limb_pos));
    }
    assert_eq!(Uint256::MAX.cmp_double_to(Uint256::MAX), Ordering::Greater);
    assert_eq!(
        Uint256::from_u64(5).cmp_double_to(Uint256::from_u64(10)),
        Ordering::Equal
    );

    // pow10 & decimal digits.
    assert!(Uint256::pow10(77).is_err());
    let p76 = Uint256::pow10(76)?;
    let s76 = p76.to_string();
    assert_eq!(s76.len(), 77);
    assert!(Uint256::from_decimal_digits(b"").is_err());
    assert!(Uint256::from_decimal_digits(b"12a").is_err());
    assert!(Uint256::from_decimal_digits(&[b'9'; 77]).is_err());
    let parsed_76 = Uint256::from_decimal_digits(&[b'9'; 76])?;
    assert_eq!(parsed_76.to_decimal_digits(), vec![b'9'; 76]);
    assert_eq!(
        Uint256::from_decimal_digits(b"00042")?,
        Uint256::from_u64(42)
    );

    signed_int256_edge_cases()
}

/// Verifies all `SignedInt256` constructors, conversions, arithmetic signs, and ordering branches.
fn signed_int256_edge_cases() -> Result<()> {
    assert_eq!(
        SignedInt256::from_parts(Uint256::ZERO, DecimalSign::Negative),
        SignedInt256::ZERO
    );
    assert_eq!(
        SignedInt256::from_parts(Uint256::ONE, DecimalSign::UnsignedZero),
        SignedInt256::ZERO
    );
    assert_eq!(SignedInt256::from_i64(0), SignedInt256::ZERO);
    assert_eq!(SignedInt256::from_i64(-9).to_string(), "-9");
    assert_eq!(SignedInt256::from_i64(9).to_string(), "9");
    assert_eq!(SignedInt256::from_i128(0).to_i128(), Some(0));
    assert_eq!(
        SignedInt256::from_i128(i128::MIN).to_i128(),
        Some(i128::MIN)
    );
    assert_eq!(
        SignedInt256::from_i128(i128::MAX).to_i128(),
        Some(i128::MAX)
    );
    assert_eq!(SignedInt256::from_i128(-42).to_i128(), Some(-42));
    assert_eq!(SignedInt256::from_u128(0), SignedInt256::ZERO);
    assert_eq!(SignedInt256::from_u128(u128::MAX).to_i128(), None);
    assert_eq!(
        SignedInt256::from_parts(Uint256::from_u128(u128::MAX), DecimalSign::Negative).to_i128(),
        None
    );
    assert_eq!(
        SignedInt256::from_parts(Uint256::from_limbs([0, 0, 1, 0]), DecimalSign::Positive)
            .to_i128(),
        None
    );
    assert_eq!(SignedInt256::MINUS_ONE.to_u128(), None);
    assert_eq!(SignedInt256::ONE.to_u128(), Some(1));
    assert_eq!(SignedInt256::ZERO.abs(), SignedInt256::ZERO);
    assert_eq!(SignedInt256::MINUS_ONE.abs(), SignedInt256::ONE);

    // Signed add/sub/mul/div_rem/ord.
    assert_eq!(
        SignedInt256::ZERO.checked_add(SignedInt256::ONE),
        Some(SignedInt256::ONE)
    );
    assert_eq!(
        SignedInt256::ONE.checked_add(SignedInt256::ZERO),
        Some(SignedInt256::ONE)
    );
    assert_eq!(
        SignedInt256::ONE.checked_add(SignedInt256::MINUS_ONE),
        Some(SignedInt256::ZERO)
    );
    assert_eq!(
        SignedInt256::from_i64(10).checked_add(SignedInt256::from_i64(-3)),
        Some(SignedInt256::from_i64(7))
    );
    assert_eq!(
        SignedInt256::from_i64(3).checked_sub(SignedInt256::from_i64(10)),
        Some(SignedInt256::from_i64(-7))
    );
    let max_pos = SignedInt256::from_parts(Uint256::MAX, DecimalSign::Positive);
    assert_eq!(max_pos.checked_add(SignedInt256::ONE), None);
    assert_eq!(max_pos.checked_mul(SignedInt256::from_i64(2)), None);
    assert_eq!(
        SignedInt256::ZERO.checked_mul(SignedInt256::ONE),
        Some(SignedInt256::ZERO)
    );
    assert_eq!(
        SignedInt256::ZERO.checked_mul_pow10(5),
        Some(SignedInt256::ZERO)
    );
    assert_eq!(
        SignedInt256::ONE.checked_mul_pow10(0),
        Some(SignedInt256::ONE)
    );
    assert_eq!(SignedInt256::ONE.checked_mul_pow10(80), None);
    assert_eq!(max_pos.checked_mul_pow10(1), None);

    let (q, r) = SignedInt256::from_i64(-17).checked_div_rem(SignedInt256::from_i64(5))?;
    assert_eq!(
        (q, r),
        (SignedInt256::from_i64(-3), SignedInt256::from_i64(-2))
    );

    let items = [
        SignedInt256::from_i64(-5),
        SignedInt256::MINUS_ONE,
        SignedInt256::ZERO,
        SignedInt256::ONE,
        SignedInt256::from_i64(5),
    ];
    for (i, a) in items.iter().enumerate() {
        for (j, b) in items.iter().enumerate() {
            assert_eq!(a.partial_cmp(b), Some(i.cmp(&j)));
        }
    }
    Ok(())
}

/// Verifies all 8 ISO/IEC 1989:2023 `RoundingMode`s across positive, negative, tie, even/odd, exact, and MAX overflow cases.
#[test]
fn all_eight_iso2023_rounding_modes() {
    let d10 = Uint256::from_u64(10);
    let u2 = Uint256::from_u64(2);
    let u4 = Uint256::from_u64(4);
    let u6 = Uint256::from_u64(6);

    assert_eq!(
        round_quotient_with_remainder(
            u2,
            Uint256::ZERO,
            d10,
            DecimalSign::Positive,
            RoundingMode::Prohibited
        ),
        (SignedInt256::from_i64(2), SizeErrorStatus::Exact)
    );
    assert_eq!(
        round_quotient_with_remainder(u2, u4, d10, DecimalSign::Positive, RoundingMode::Prohibited)
            .1,
        SizeErrorStatus::Overflowed
    );
    assert_eq!(
        round_quotient_with_remainder(
            Uint256::MAX,
            u6,
            d10,
            DecimalSign::Positive,
            RoundingMode::AwayFromZero
        )
        .1,
        SizeErrorStatus::Overflowed
    );

    let cases = [
        (2_u64, 6_u64, false, RoundingMode::Truncation, 2_i64),
        (2, 4, true, RoundingMode::AwayFromZero, -3),
        (2, 4, false, RoundingMode::TowardGreater, 3),
        (2, 4, true, RoundingMode::TowardGreater, -2),
        (2, 4, false, RoundingMode::TowardLesser, 2),
        (2, 4, true, RoundingMode::TowardLesser, -3),
        (2, 5, false, RoundingMode::NearestAwayFromZero, 3),
        (2, 4, false, RoundingMode::NearestAwayFromZero, 2),
        (2, 5, false, RoundingMode::NearestTowardZero, 2),
        (2, 6, false, RoundingMode::NearestTowardZero, 3),
        (2, 5, false, RoundingMode::NearestEven, 2),
        (3, 5, false, RoundingMode::NearestEven, 4),
        (2, 6, false, RoundingMode::NearestEven, 3),
        (2, 4, false, RoundingMode::NearestEven, 2),
    ];
    for (q, r, neg, mode, exp) in cases {
        let sign = if neg {
            DecimalSign::Negative
        } else {
            DecimalSign::Positive
        };
        let (res, st) = round_quotient_with_remainder(
            Uint256::from_u64(q),
            Uint256::from_u64(r),
            d10,
            sign,
            mode,
        );
        assert_eq!(
            (res, st),
            (SignedInt256::from_i64(exp), SizeErrorStatus::Exact)
        );
    }
}

/// Verifies `FixedDecimal` parsing, rescaling, 38/76-digit arithmetic, `DIVIDE ... REMAINDER`, `**`, `f64`, and `cmp_numeric`.
#[test]
fn fixed_decimal_arithmetic_and_edge_cases() -> Result<()> {
    assert!(FixedDecimal::try_new(SignedInt256::ONE, 77).is_err());
    assert!(FixedDecimal::parse_str("1.2.3").is_err());
    assert!(FixedDecimal::parse_str("12x").is_err());
    assert!(FixedDecimal::parse_str("").is_err());

    let pos = FixedDecimal::parse_str(" +123.45 ")?;
    let neg = FixedDecimal::parse_str("-0.005")?;
    assert_eq!(pos.to_string(), "123.45");
    assert_eq!(neg.to_string(), "-0.005");
    assert_eq!(FixedDecimal::ZERO.to_string(), "0");
    assert_eq!(
        FixedDecimal::try_new(SignedInt256::ZERO, 2)?.to_string(),
        "0.00"
    );
    assert_eq!(
        FixedDecimal::try_new(SignedInt256::from_i64(91), -3)?.to_string(),
        "91000"
    );
    assert_eq!(
        FixedDecimal::from_i128_scaled(42, ScaleFactor::ZERO).to_exact_i32(),
        Some(42)
    );
    assert_eq!(pos.to_exact_i32(), None);
    assert_eq!(pos.abs(), pos);

    // Fast-path i128 addition & 256-bit escalation addition.
    let (sum_fast, st) = pos.checked_add(&FixedDecimal::parse_str("0.55")?)?;
    assert_eq!(
        (sum_fast.to_string().as_str(), st),
        ("124.00", SizeErrorStatus::Exact)
    );
    let d37 = FixedDecimal::parse_str("9999999999999999999999999999999999999")?;
    assert_eq!(
        FixedDecimal::from_i64(1).checked_add(&d37)?.1,
        SizeErrorStatus::Exact
    );
    assert_eq!(
        d37.checked_add(&FixedDecimal::from_i64(1))?.1,
        SizeErrorStatus::Exact
    );
    let d45 = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::pow10(45)?, DecimalSign::Positive),
        0,
    )?;
    assert_eq!(
        FixedDecimal::from_i64(1).checked_add(&d45)?.1,
        SizeErrorStatus::Exact
    );
    assert_eq!(
        FixedDecimal::try_new(SignedInt256::ONE, 50)?
            .checked_add(&FixedDecimal::try_new(SignedInt256::ONE, -50)?)?
            .1,
        SizeErrorStatus::Overflowed
    );
    let (diff, _) = pos.checked_sub(&neg)?;
    assert_eq!(diff.to_string(), "123.455");

    let big38 = FixedDecimal::parse_str("99999999999999999999999999999999999999")?;
    let (prod76, st_mul) = big38.checked_mul(&big38)?;
    assert_eq!(st_mul, SizeErrorStatus::Exact);
    assert_eq!(prod76.to_string().len(), 76);

    fixed_decimal_rescale_div_pow_and_cmp()
}

/// Verifies `FixedDecimal` extreme scale bounds, `fit_to_picture`, `checked_div`, `checked_pow`, and `cmp_numeric`.
fn fixed_decimal_rescale_div_pow_and_cmp() -> Result<()> {
    assert!(
        FixedDecimal::ZERO
            .rescale(77, RoundingMode::Truncation)
            .is_err()
    );
    let hi_s = FixedDecimal::try_new(SignedInt256::ONE, 50)?;
    let (r_down, _) = hi_s.rescale(-40, RoundingMode::AwayFromZero)?;
    assert_eq!(r_down.mantissa(), SignedInt256::ONE);
    let (z_down, _) =
        FixedDecimal::try_new(SignedInt256::ZERO, 50)?.rescale(-40, RoundingMode::AwayFromZero)?;
    assert!(z_down.is_zero());
    let (_, st_up_ovf) =
        FixedDecimal::try_new(SignedInt256::ONE, -50)?.rescale(50, RoundingMode::Truncation)?;
    assert_eq!(st_up_ovf, SizeErrorStatus::Overflowed);

    // Add overflow & rescale overflow inside checked_add.
    let max_d = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::MAX, DecimalSign::Positive),
        0,
    )?;
    assert_eq!(
        max_d.checked_add(&FixedDecimal::from_i64(1))?.1,
        SizeErrorStatus::Overflowed
    );
    assert_eq!(
        FixedDecimal::try_new(SignedInt256::ONE, -50)?
            .checked_add(&FixedDecimal::try_new(SignedInt256::ONE, 50)?)?
            .1,
        SizeErrorStatus::Overflowed
    );

    // Mul scale clamping (> 76 and < -76) and mantissa overflow.
    assert_eq!(
        max_d.checked_mul(&FixedDecimal::from_i64(2))?.1,
        SizeErrorStatus::Overflowed
    );
    let pos_s50 = FixedDecimal::try_new(SignedInt256::from_i64(25), 50)?;
    let (m_hi, status_hi) = pos_s50.checked_mul(&pos_s50)?;
    assert_eq!((m_hi.scale(), status_hi), (76, SizeErrorStatus::Exact));
    let neg_s50 = FixedDecimal::try_new(SignedInt256::from_i64(2), -50)?;
    let (m_lo, status_lo) = neg_s50.checked_mul(&neg_s50)?;
    assert_eq!((m_lo.scale(), status_lo), (-76, SizeErrorStatus::Exact));
    let sn50_huge = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::pow10(60)?, DecimalSign::Positive),
        -50,
    )?;
    assert_eq!(
        sn50_huge.checked_mul(&neg_s50)?.1,
        SizeErrorStatus::Overflowed
    );

    // fit_to_picture: Prohibited, FlagOverflow, ModuloTruncate, Unsigned/Signed.
    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    assert_eq!(
        FixedDecimal::parse_str("1.5")?
            .fit_to_picture(
                d2,
                sc0,
                SignednessPolicy::Signed,
                RoundingMode::Prohibited,
                HighOrderPolicy::FlagOverflow
            )?
            .1,
        SizeErrorStatus::Overflowed
    );
    assert_eq!(
        FixedDecimal::from_i64(123)
            .fit_to_picture(
                d2,
                sc0,
                SignednessPolicy::Signed,
                RoundingMode::Truncation,
                HighOrderPolicy::FlagOverflow
            )?
            .1,
        SizeErrorStatus::Overflowed
    );
    let (mod_u, _) = FixedDecimal::from_i64(-123).fit_to_picture(
        d2,
        sc0,
        SignednessPolicy::Unsigned,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(mod_u.to_string(), "23");
    let (mod_z, _) = FixedDecimal::from_i64(100).fit_to_picture(
        d2,
        sc0,
        SignednessPolicy::Unsigned,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert!(mod_z.is_zero());

    fixed_decimal_div_pow_f64_and_cmp(max_d, d2, sc0)
}

/// Verifies `checked_div`, `checked_div_rem_cobol`, `checked_pow`, `from_f64`, and `cmp_numeric`.
fn fixed_decimal_div_pow_f64_and_cmp(
    max_d: FixedDecimal,
    d2: DigitCount,
    sc0: ScaleFactor,
) -> Result<()> {
    assert!(
        FixedDecimal::ZERO
            .checked_div(&FixedDecimal::from_i64(1), 77, RoundingMode::Truncation)
            .is_err()
    );
    assert_eq!(
        FixedDecimal::from_i64(1)
            .checked_div(&FixedDecimal::ZERO, 2, RoundingMode::Truncation)?
            .1,
        SizeErrorStatus::DivisionByZero
    );
    assert_eq!(
        FixedDecimal::ZERO
            .checked_div(&FixedDecimal::from_i64(5), 2, RoundingMode::Truncation)?
            .0
            .to_string(),
        "0.00"
    );
    assert_eq!(
        max_d
            .checked_div(&FixedDecimal::from_i64(1), 10, RoundingMode::Truncation)?
            .1,
        SizeErrorStatus::Overflowed
    );
    let (tiny_q, _) = FixedDecimal::try_new(SignedInt256::ONE, 50)?.checked_div(
        &FixedDecimal::try_new(SignedInt256::ONE, -40)?,
        0,
        RoundingMode::AwayFromZero,
    )?;
    assert_eq!(tiny_q.to_string(), "1");
    let (neg_sh_q, _) = FixedDecimal::try_new(SignedInt256::from_i64(50), 1)?.checked_div(
        &FixedDecimal::from_i64(2),
        0,
        RoundingMode::NearestAwayFromZero,
    )?;
    assert_eq!(neg_sh_q.to_string(), "3");

    // DIVIDE ... GIVING ... REMAINDER
    let spec = (d2, sc0, SignednessPolicy::Signed);
    let (q_cb, r_cb, st_cb) = FixedDecimal::from_i64(17).checked_div_rem_cobol(
        &FixedDecimal::from_i64(5),
        spec,
        RoundingMode::NearestAwayFromZero,
    )?;
    assert_eq!(
        (q_cb.to_string().as_str(), r_cb.to_string().as_str(), st_cb),
        ("3", "2", SizeErrorStatus::Exact)
    );
    assert_eq!(
        FixedDecimal::from_i64(17)
            .checked_div_rem_cobol(&FixedDecimal::ZERO, spec, RoundingMode::Truncation)?
            .2,
        SizeErrorStatus::DivisionByZero
    );
    assert_eq!(
        FixedDecimal::from_i64(500)
            .checked_div_rem_cobol(&FixedDecimal::from_i64(2), spec, RoundingMode::Truncation)?
            .2,
        SizeErrorStatus::Overflowed
    );
    let huge_div = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::MAX, DecimalSign::Positive),
        0,
    )?;
    let spec38 = (DigitCount::try_new(38)?, sc0, SignednessPolicy::Signed);
    assert_eq!(
        FixedDecimal::try_new(
            SignedInt256::from_parts(Uint256::pow10(40)?, DecimalSign::Positive),
            -38
        )?
        .checked_div_rem_cobol(&huge_div, spec38, RoundingMode::AwayFromZero)?
        .2,
        SizeErrorStatus::Overflowed
    );

    let huge_s1 = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::MAX, DecimalSign::Positive),
        1,
    )?;
    let (half_max, _) = Uint256::MAX.checked_div_rem(Uint256::from_u64(2))?;
    let half_s1 =
        FixedDecimal::try_new(SignedInt256::from_parts(half_max, DecimalSign::Positive), 1)?;
    assert_eq!(
        huge_s1
            .checked_div_rem_cobol(&half_s1, spec38, RoundingMode::AwayFromZero)?
            .2,
        SizeErrorStatus::Overflowed
    );

    fixed_decimal_pow_f64_and_cmp(max_d)
}

/// Verifies `checked_pow`, `from_f64`, and `cmp_numeric` branches.
///
/// # Arguments
///
/// * `max_d` - Maximum unscaled `FixedDecimal`.
fn fixed_decimal_pow_f64_and_cmp(max_d: FixedDecimal) -> Result<()> {
    assert_eq!(
        FixedDecimal::from_i64(42).negated(),
        FixedDecimal::from_i64(-42)
    );
    assert_eq!(FixedDecimal::ZERO.negated(), FixedDecimal::ZERO);
    // Exponentiation (`**`).
    assert_eq!(
        FixedDecimal::ZERO
            .checked_pow(&FixedDecimal::ZERO, 0, RoundingMode::Truncation)?
            .1,
        SizeErrorStatus::InvalidExponent
    );
    assert_eq!(
        FixedDecimal::ZERO
            .checked_pow(&FixedDecimal::from_i64(-2), 0, RoundingMode::Truncation)?
            .1,
        SizeErrorStatus::DivisionByZero
    );
    assert!(
        FixedDecimal::ZERO
            .checked_pow(&FixedDecimal::from_i64(3), 2, RoundingMode::Truncation)?
            .0
            .is_zero()
    );
    assert_eq!(
        FixedDecimal::from_i64(5)
            .checked_pow(&FixedDecimal::ZERO, 0, RoundingMode::Truncation)?
            .0
            .to_string(),
        "1"
    );
    assert_eq!(
        FixedDecimal::from_i64(2)
            .checked_pow(&FixedDecimal::from_i64(10), 0, RoundingMode::Truncation)?
            .0
            .to_string(),
        "1024"
    );
    assert_eq!(
        FixedDecimal::from_i64(2)
            .checked_pow(&FixedDecimal::from_i64(-2), 2, RoundingMode::Truncation)?
            .0
            .to_string(),
        "0.25"
    );
    assert_eq!(
        max_d
            .checked_pow(&FixedDecimal::from_i64(2), 0, RoundingMode::Truncation)?
            .1,
        SizeErrorStatus::Overflowed
    );
    assert_eq!(
        FixedDecimal::try_new(
            SignedInt256::from_parts(Uint256::pow10(30)?, DecimalSign::Positive),
            0
        )?
        .checked_pow(&FixedDecimal::from_i64(3), 0, RoundingMode::Truncation)?
        .1,
        SizeErrorStatus::Overflowed
    );
    assert_eq!(
        FixedDecimal::from_i64(-4)
            .checked_pow(
                &FixedDecimal::parse_str("0.5")?,
                2,
                RoundingMode::Truncation
            )?
            .1,
        SizeErrorStatus::InvalidExponent
    );
    let (sqrt9, _) = FixedDecimal::from_i64(9).checked_pow(
        &FixedDecimal::parse_str("0.5")?,
        1,
        RoundingMode::NearestAwayFromZero,
    )?;
    assert_eq!(sqrt9.to_string(), "3.0");
    assert_eq!(
        FixedDecimal::from_i64(10)
            .checked_pow(&FixedDecimal::from_i64(1000), 0, RoundingMode::Truncation)?
            .1,
        SizeErrorStatus::Overflowed
    );
    assert!(FixedDecimal::from_f64(f64::NAN, 0, RoundingMode::Truncation).is_err());
    assert_eq!(FixedDecimal::ZERO.to_f64(), 0.0);

    fixed_decimal_cmp_edges()
}

/// Verifies `cmp_numeric` across signs, equal scales, aligned scales, and >76 scale spans.
fn fixed_decimal_cmp_edges() -> Result<()> {
    let z0 = FixedDecimal::ZERO;
    let z2 = FixedDecimal::try_new(SignedInt256::ZERO, 2)?;
    let n1 = FixedDecimal::from_i64(-1);
    let p_half_a = FixedDecimal::parse_str("0.5")?;
    let p_half_b = FixedDecimal::parse_str("0.500")?;
    assert_eq!(z0.cmp_numeric(&z2), Ordering::Equal);
    assert_eq!(n1.cmp_numeric(&z0), Ordering::Less);
    assert_eq!(z0.cmp_numeric(&p_half_a), Ordering::Less);
    assert_eq!(p_half_a.cmp_numeric(&n1), Ordering::Greater);
    assert_eq!(p_half_a.cmp_numeric(&z0), Ordering::Greater);
    assert_eq!(
        n1.cmp_numeric(&FixedDecimal::from_i64(-2)),
        Ordering::Greater
    );
    assert_eq!(p_half_a.cmp_numeric(&p_half_b), Ordering::Equal);
    assert_eq!(p_half_b.cmp_numeric(&p_half_a), Ordering::Equal);
    let s_hi = FixedDecimal::try_new(SignedInt256::ONE, 50)?;
    let s_lo = FixedDecimal::try_new(SignedInt256::ZERO, -40)?;
    let s_lo_big = FixedDecimal::try_new(SignedInt256::ONE, -40)?;
    assert_eq!(s_hi.cmp_numeric(&s_lo), Ordering::Greater);
    assert_eq!(s_hi.cmp_numeric(&s_lo_big), Ordering::Less);
    assert_eq!(
        FixedDecimal::parse_str("-0.5")?.cmp_numeric(&FixedDecimal::parse_str("-0.500")?),
        Ordering::Equal
    );
    let neg_hi = FixedDecimal::try_new(SignedInt256::MINUS_ONE, 50)?;
    let neg_lo = FixedDecimal::try_new(SignedInt256::MINUS_ONE, -40)?;
    assert_eq!(neg_hi.cmp_numeric(&neg_lo), Ordering::Greater);
    Ok(())
}

/// Verifies zoned decimal (`USAGE DISPLAY`) encoding, decoding, and `IS NUMERIC` across all 4 dialects & 5 sign placements.
#[test]
fn zoned_decimal_all_dialects_and_placements() -> Result<()> {
    let d3 = DigitCount::try_new(3)?;
    let sc0 = ScaleFactor::ZERO;

    for dialect in [
        OverpunchDialect::AsciiGnu,
        OverpunchDialect::Cobol85Standard,
        OverpunchDialect::EbcdicIbm,
        OverpunchDialect::MicroFocus,
    ] {
        for sign in [
            SignPlacement::Unsigned,
            SignPlacement::EmbeddedTrailing,
            SignPlacement::EmbeddedLeading,
            SignPlacement::SeparateTrailing,
            SignPlacement::SeparateLeading,
        ] {
            let layout = ZonedLayout {
                digits: d3,
                scale: sc0,
                sign,
                dialect,
                blank_zero: BlankZeroMode::RetainZero,
            };
            for sample in [0_i64, 1, 5, 123, -1, -10, -129] {
                let v = FixedDecimal::from_i64(sample);
                let (enc, st) = encode_zoned(
                    &v,
                    layout,
                    RoundingMode::Truncation,
                    HighOrderPolicy::ModuloTruncate,
                )?;
                assert_eq!(st, SizeErrorStatus::Exact);
                assert!(is_valid_zoned_numeric(&enc, layout));
                let dec = decode_zoned(&enc, layout)?;
                let expected = if sign.is_signed() {
                    sample
                } else {
                    sample.abs()
                };
                assert_eq!(dec, FixedDecimal::from_i64(expected));
            }
            assert!(!is_valid_zoned_numeric(&[0xFF], layout));
            assert!(!is_valid_zoned_numeric(
                &vec![0xFF; layout.byte_len()],
                layout
            ));
        }
    }

    // BlankWhenZero + space leading padding + invalid byte errors.
    let bwz_layout = ZonedLayout {
        digits: d3,
        scale: sc0,
        sign: SignPlacement::EmbeddedTrailing,
        dialect: OverpunchDialect::AsciiGnu,
        blank_zero: BlankZeroMode::BlankWhenZero,
    };
    let (bwz_bytes, _) = encode_zoned(
        &FixedDecimal::ZERO,
        bwz_layout,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(bwz_bytes, b"   ");
    assert!(decode_zoned(b"   ", bwz_layout)?.is_zero());
    assert_eq!(
        decode_zoned(b" 4 ", bwz_layout)?,
        FixedDecimal::from_i64(40)
    );
    assert!(decode_zoned(b"12", bwz_layout).is_err());
    assert!(decode_zoned(b"12~", bwz_layout).is_err());
    assert!(decode_zoned(b"1~2", bwz_layout).is_err());

    let sep_layout = ZonedLayout {
        sign: SignPlacement::SeparateLeading,
        ..bwz_layout
    };
    assert_eq!(
        decode_zoned(b"  12", sep_layout)?,
        FixedDecimal::from_i64(12)
    );
    assert!(decode_zoned(b"?012", sep_layout).is_err());
    let ebc_signed = ZonedLayout {
        digits: d3,
        scale: sc0,
        sign: SignPlacement::EmbeddedTrailing,
        dialect: OverpunchDialect::EbcdicIbm,
        blank_zero: BlankZeroMode::RetainZero,
    };
    assert_eq!(
        decode_zoned(&[0xF0, 0xF1, 0xF5], ebc_signed)?,
        FixedDecimal::from_i64(15)
    );
    Ok(())
}

/// Verifies `COMP-3` (signed/unsigned) and `COMP-6` encoding, decoding, and `GnuCOBOL` vs IBM `IS NUMERIC` validation.
#[test]
fn packed_bcd_comp3_and_comp6_codecs() -> Result<()> {
    let sc0 = ScaleFactor::ZERO;
    for digits_raw in [2_u8, 3] {
        let digits = DigitCount::try_new(digits_raw)?;
        for kind in [
            PackedKind::Comp3Signed,
            PackedKind::Comp3Unsigned,
            PackedKind::Comp6Unsigned,
        ] {
            let layout = PackedLayout {
                digits,
                scale: sc0,
                kind,
            };
            for sample in [0_i64, 7, 99, -45] {
                let (enc, _) = encode_packed(
                    &FixedDecimal::from_i64(sample),
                    layout,
                    RoundingMode::Truncation,
                    HighOrderPolicy::ModuloTruncate,
                )?;
                assert!(is_valid_packed_bcd(&enc, layout, PackedSignPolicy::Gnu));
                assert!(is_valid_packed_bcd(&enc, layout, PackedSignPolicy::Ibm));
                let dec = decode_packed(&enc, layout)?;
                let exp = if kind.is_signed() {
                    sample % 10_i64.pow(u32::from(digits_raw))
                } else {
                    sample.abs() % 10_i64.pow(u32::from(digits_raw))
                };
                assert_eq!(dec, FixedDecimal::from_i64(exp));
            }
        }
    }
    packed_bcd_invalid_and_sign_policy_edges(sc0)
}

/// Verifies `IS NUMERIC` sign-nibble dialect rules and invalid BCD rejection on `COMP-3` / `COMP-6`.
///
/// # Arguments
///
/// * `sc0` - Zero scale factor.
fn packed_bcd_invalid_and_sign_policy_edges(sc0: ScaleFactor) -> Result<()> {
    let s3 = PackedLayout {
        digits: DigitCount::try_new(3)?,
        scale: sc0,
        kind: PackedKind::Comp3Signed,
    };
    let u3 = PackedLayout {
        kind: PackedKind::Comp3Unsigned,
        ..s3
    };
    assert!(!is_valid_packed_bcd(&[0x00], s3, PackedSignPolicy::Gnu));
    assert!(!is_valid_packed_bcd(
        &[0x00, 0x0F],
        s3,
        PackedSignPolicy::Gnu
    ));
    assert!(is_valid_packed_bcd(
        &[0x00, 0x0F],
        s3,
        PackedSignPolicy::Ibm
    ));
    assert!(is_valid_packed_bcd(
        &[0x00, 0x0F],
        u3,
        PackedSignPolicy::Gnu
    ));
    assert!(!is_valid_packed_bcd(
        &[0x00, 0x0C],
        u3,
        PackedSignPolicy::Gnu
    ));
    assert!(!is_valid_packed_bcd(
        &[0xA0, 0x0C],
        s3,
        PackedSignPolicy::Gnu
    ));
    assert!(!is_valid_packed_bcd(
        &[0x00, 0xAC],
        s3,
        PackedSignPolicy::Gnu
    ));

    let s2 = PackedLayout {
        digits: DigitCount::try_new(2)?,
        scale: sc0,
        kind: PackedKind::Comp3Signed,
    };
    assert!(!is_valid_packed_bcd(
        &[0x10, 0x0C],
        s2,
        PackedSignPolicy::Gnu
    ));

    let c6_odd = PackedLayout {
        digits: DigitCount::try_new(3)?,
        scale: sc0,
        kind: PackedKind::Comp6Unsigned,
    };
    assert!(!is_valid_packed_bcd(
        &[0x12, 0x34],
        c6_odd,
        PackedSignPolicy::Gnu
    ));
    assert!(!is_valid_packed_bcd(
        &[0x0A, 0x34],
        c6_odd,
        PackedSignPolicy::Gnu
    ));
    let c6_even = PackedLayout {
        digits: DigitCount::try_new(2)?,
        scale: sc0,
        kind: PackedKind::Comp6Unsigned,
    };
    assert!(is_valid_packed_bcd(&[0x12], c6_even, PackedSignPolicy::Gnu));
    assert!(!is_valid_packed_bcd(
        &[0xA0],
        c6_even,
        PackedSignPolicy::Gnu
    ));
    assert!(!is_valid_packed_bcd(
        &[0x0A, 0x0C],
        s3,
        PackedSignPolicy::Gnu
    ));
    assert!(decode_packed(&[0x00, 0x0D], s3)?.is_zero());

    assert!(decode_packed(&[0x00], s3).is_err());
    assert!(decode_packed(&[0xA0, 0x0C], s3).is_err());
    assert!(decode_packed(&[0x0A, 0x0C], s3).is_err());
    assert!(decode_packed(&[0x00, 0x09], s3).is_err());
    assert!(decode_packed(&[0x00, 0xAC], s3).is_err());
    assert!(decode_packed(&[0x0A, 0x12], c6_odd).is_err());
    assert!(decode_packed(&[0xA0, 0x12], c6_odd).is_err());
    Ok(())
}

/// Verifies `1..=16` byte `BINARY` / `COMP-5` big-endian and native-endian codecs under both truncation policies.
#[test]
fn binary_1_to_16_byte_codecs() -> Result<()> {
    let bad = BinaryLayout {
        byte_width: 0,
        digits: DigitCount::try_new(4)?,
        scale: ScaleFactor::ZERO,
        sign: BinarySignMode::Signed,
        byte_order: BinaryByteOrder::BigEndian,
        trunc: BinaryTruncMode::FullBinaryRange,
    };
    assert!(
        encode_binary(
            &FixedDecimal::ZERO,
            bad,
            RoundingMode::Truncation,
            HighOrderPolicy::FlagOverflow
        )
        .is_err()
    );
    assert!(decode_binary(&[], bad).is_err());

    for width in [1_u8, 2, 3, 4, 5, 6, 7, 8, 16] {
        for byte_order in [BinaryByteOrder::BigEndian, BinaryByteOrder::NativeEndian] {
            for sign in [BinarySignMode::Signed, BinarySignMode::Unsigned] {
                let layout = BinaryLayout {
                    byte_width: width,
                    digits: DigitCount::try_new(2)?,
                    scale: ScaleFactor::ZERO,
                    sign,
                    byte_order,
                    trunc: BinaryTruncMode::FullBinaryRange,
                };
                for sample in [0_i64, 42, -42] {
                    let (enc, st) = encode_binary(
                        &FixedDecimal::from_i64(sample),
                        layout,
                        RoundingMode::Truncation,
                        HighOrderPolicy::ModuloTruncate,
                    )?;
                    assert_eq!(st, SizeErrorStatus::Exact);
                    let dec = decode_binary(&enc, layout)?;
                    let exp = if sign == BinarySignMode::Signed {
                        sample
                    } else {
                        sample.abs()
                    };
                    assert_eq!(dec, FixedDecimal::from_i64(exp));
                }
            }
        }
    }
    binary_overflow_and_trunc_edges()
}

/// Verifies `TruncateToPic`, `Prohibited` rounding, and hardware overflow/wrap edges on `encode_binary`.
fn binary_overflow_and_trunc_edges() -> Result<()> {
    // TruncateToPic, Prohibited rounding error, and hardware overflow (1-byte signed/unsigned & >128-bit).
    let pic2 = BinaryLayout {
        byte_width: 1,
        digits: DigitCount::try_new(2)?,
        scale: ScaleFactor::ZERO,
        sign: BinarySignMode::Signed,
        byte_order: BinaryByteOrder::BigEndian,
        trunc: BinaryTruncMode::TruncateToPic,
    };
    assert!(decode_binary(&[0, 0], pic2).is_err());
    assert_eq!(
        encode_binary(
            &FixedDecimal::from_i64(150),
            pic2,
            RoundingMode::Truncation,
            HighOrderPolicy::FlagOverflow
        )?
        .1,
        SizeErrorStatus::Overflowed
    );
    let (wrapped_pic, _) = encode_binary(
        &FixedDecimal::from_i64(123),
        pic2,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(
        decode_binary(&wrapped_pic, pic2)?,
        FixedDecimal::from_i64(23)
    );

    let full1_s = BinaryLayout {
        trunc: BinaryTruncMode::FullBinaryRange,
        ..pic2
    };
    assert_eq!(
        encode_binary(
            &FixedDecimal::parse_str("1.5")?,
            full1_s,
            RoundingMode::Prohibited,
            HighOrderPolicy::FlagOverflow
        )?
        .1,
        SizeErrorStatus::Overflowed
    );
    assert_eq!(
        encode_binary(
            &FixedDecimal::from_i64(200),
            full1_s,
            RoundingMode::Truncation,
            HighOrderPolicy::FlagOverflow
        )?
        .1,
        SizeErrorStatus::Overflowed
    );
    let (_, _) = encode_binary(
        &FixedDecimal::from_i64(-200),
        full1_s,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    let (_, _) = encode_binary(
        &FixedDecimal::from_i64(200),
        full1_s,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;

    let (_, st_pic_proh) = encode_binary(
        &FixedDecimal::parse_str("1.5")?,
        pic2,
        RoundingMode::Prohibited,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(st_pic_proh, SizeErrorStatus::Overflowed);

    let full1_u = BinaryLayout {
        sign: BinarySignMode::Unsigned,
        ..full1_s
    };
    let (_, _) = encode_binary(
        &FixedDecimal::from_i64(300),
        full1_u,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    let gt128 = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::from_limbs([1, 0, 1, 0]), DecimalSign::Positive),
        0,
    )?;
    let _ = encode_binary(
        &gt128,
        full1_s,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    let _ = encode_binary(
        &gt128,
        full1_u,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    Ok(())
}

/// Verifies `COMP-1`, `COMP-2`, `FloatBinary128`, `Decimal64Bid`, `Decimal128Bid`, and all 1,000 `DPD` declets.
#[test]
fn float_binary_and_ieee754_decimal_bid_dpd_codecs() -> Result<()> {
    let sample = FixedDecimal::parse_str("-12.5")?;
    let c1 = encode_comp1_ne(&sample);
    assert_eq!(
        decode_comp1_ne(c1, 1, RoundingMode::NearestAwayFromZero)?.0,
        sample
    );
    let c2 = encode_comp2_ne(&sample);
    assert_eq!(
        decode_comp2_ne(c2, 1, RoundingMode::NearestAwayFromZero)?.0,
        sample
    );

    // FloatBinary128: normal, +/-0.0, subnormal f64, subnormal binary128, NaN/Inf rejection.
    assert!(FloatBinary128::from_f64(f64::INFINITY).is_err());
    for f in [0.0_f64, -0.0, 1.5, -42.25, f64::from_bits(1)] {
        let b128 = FloatBinary128::from_f64(f)?;
        let round = FloatBinary128::from_ne_bytes(b128.to_ne_bytes()).to_f64()?;
        assert!((round - f).abs() <= 1e-300);
    }
    let sub128 = FloatBinary128::from_ne_bytes(1_u128.to_ne_bytes());
    assert_eq!(sub128.to_f64()?, 0.0);
    let inf128 = FloatBinary128::from_ne_bytes((0x7FFF_u128 << 112).to_ne_bytes());
    assert!(inf128.to_f64().is_err());

    // Decimal64Bid & Decimal128Bid: zero, small/large coefficients (both top2==0b11 and normal), >16/>34 digits, NaN, out-of-range exp.
    let z64 = Decimal64Bid::from_fixed_decimal(&FixedDecimal::ZERO, RoundingMode::Truncation)?;
    assert!(
        Decimal64Bid::from_ne_bytes(z64.to_ne_bytes())
            .to_fixed_decimal()?
            .is_zero()
    );
    let z128 = Decimal128Bid::from_fixed_decimal(&FixedDecimal::ZERO, RoundingMode::Truncation)?;
    assert!(
        Decimal128Bid::from_ne_bytes(z128.to_ne_bytes())
            .to_fixed_decimal()?
            .is_zero()
    );

    for txt in ["123.45", "-9999999999999999", "12345678901234567890"] {
        let d = FixedDecimal::parse_str(txt)?;
        let b64 = Decimal64Bid::from_fixed_decimal(&d, RoundingMode::NearestAwayFromZero)?;
        let _ = b64.to_fixed_decimal()?;
    }
    // Non-canonical (> MAX_COEFF -> 0), NaN, and extreme exponent decode errors on BID64.
    let noncanon64 = Decimal64Bid::from_ne_bytes(
        ((0b11_u64 << 61) | (398_u64 << 51) | ((1_u64 << 51) - 1)).to_ne_bytes(),
    );
    assert!(noncanon64.to_fixed_decimal()?.is_zero());
    let nan64 = Decimal64Bid::from_ne_bytes((0x1F_u64 << 59).to_ne_bytes());
    assert!(nan64.to_fixed_decimal().is_err());
    let huge_exp64 = Decimal64Bid::from_ne_bytes(1_u64.to_ne_bytes());
    assert!(huge_exp64.to_fixed_decimal().is_err());

    for txt in [
        "-123.45",
        "9999999999999999999999999999999999",
        "-123456789012345678901234567890123456",
    ] {
        let d = FixedDecimal::parse_str(txt)?;
        let b128 = Decimal128Bid::from_fixed_decimal(&d, RoundingMode::NearestAwayFromZero)?;
        let _ = b128.to_fixed_decimal()?;
    }
    let noncanon128 = Decimal128Bid::from_ne_bytes(
        ((0b11_u128 << 125) | (6176_u128 << 111) | ((1_u128 << 111) - 1)).to_ne_bytes(),
    );
    assert!(noncanon128.to_fixed_decimal()?.is_zero());
    let nan128 = Decimal128Bid::from_ne_bytes((0x1F_u128 << 123).to_ne_bytes());
    assert!(nan128.to_fixed_decimal().is_err());
    let huge_exp128 = Decimal128Bid::from_ne_bytes(1_u128.to_ne_bytes());
    assert!(huge_exp128.to_fixed_decimal().is_err());

    // Exhaustive 000..=999 DPD declet pack/unpack bijection + invalid inputs.
    assert!(pack_dpd_declet([10, 0, 0]).is_err());
    assert!(pack_dpd_declet([0, 10, 0]).is_err());
    assert!(pack_dpd_declet([0, 0, 10]).is_err());
    assert!(unpack_dpd_declet(0x0400).is_err());
    for d2 in 0..=9_u8 {
        for d1 in 0..=9_u8 {
            for d0 in 0..=9_u8 {
                let declet = pack_dpd_declet([d2, d1, d0])?;
                assert_eq!(unpack_dpd_declet(declet)?, [d2, d1, d0]);
            }
        }
    }
    Ok(())
}

/// Verifies `Codepage` parsing, `TextEncoding` conversion, and 256-byte bijective round-trips.
#[test]
fn m17_codepages_and_bijective_ebcdic_round_trips_coverage() -> Result<()> {
    use coblang_common::TextEncoding;
    use coblang_decimal::charset::{Codepage, UnmappedCharPolicy, transcode_bytes};

    for &cp in Codepage::all() {
        let cname = cp.canonical_name();
        assert_eq!(Codepage::from_name(cname)?, cp);
        let tenc = cp.to_text_encoding();
        assert_eq!(Codepage::from_text_encoding(&tenc)?, cp);
        if cp.is_ebcdic() {
            assert_eq!(cp.single_byte_space(), Some(0x40));
        }
    }
    assert_eq!(Codepage::Utf8.single_byte_space(), Some(0x20));
    assert_eq!(Codepage::Utf16Be.single_byte_space(), None);
    for (alias, expected) in [
        ("ebcdic", Codepage::Cp1140),
        ("ibm_037", Codepage::Cp037),
        ("latin-1", Codepage::Iso8859_1),
        ("latin-9", Codepage::Iso8859_15),
        ("windows-1252", Codepage::Cp1252),
        ("national", Codepage::Utf16Be),
    ] {
        assert_eq!(Codepage::from_name(alias)?, expected);
    }
    assert!(Codepage::from_name("BOGUS_CP").is_err());
    assert!(Codepage::from_text_encoding(&TextEncoding::Codepage(String::from("BAD"))).is_err());

    let all_256: Vec<u8> = (0_u8..=255_u8).collect();
    for bijective_cp in [
        Codepage::Cp1140,
        Codepage::Cp037,
        Codepage::Cp500,
        Codepage::Cp1047,
        Codepage::Iso8859_1,
        Codepage::Iso8859_15,
    ] {
        let utf8 = transcode_bytes(
            &all_256,
            bijective_cp,
            Codepage::Utf8,
            UnmappedCharPolicy::StrictError,
        )?;
        let back = transcode_bytes(
            &utf8,
            Codepage::Utf8,
            bijective_cp,
            UnmappedCharPolicy::StrictError,
        )?;
        assert_eq!(back, all_256);
    }

    let ebcdic_a0 = transcode_bytes(
        b"A0 \xE2\x82\xAC",
        Codepage::Utf8,
        Codepage::Cp1140,
        UnmappedCharPolicy::StrictError,
    )?;
    assert_eq!(ebcdic_a0, vec![0xC1, 0xF0, 0x40, 0x9F]);
    Ok(())
}

/// Verifies `CP1252`, `UTF-8`, and `UTF-16BE`/`UTF-16LE` strict vs substitution error handling.
#[test]
fn m17_charset_utf8_utf16_and_sbcs_error_recovery_coverage() -> Result<()> {
    use coblang_decimal::charset::{
        Codepage, UnmappedCharPolicy, decode_to_chars, encode_from_chars,
    };

    assert!(decode_to_chars(&[0x81], Codepage::Cp1252, UnmappedCharPolicy::StrictError).is_err());
    assert_eq!(
        decode_to_chars(
            &[0x41, 0x81],
            Codepage::Cp1252,
            UnmappedCharPolicy::Substitute('?')
        )?,
        vec!['A', '?']
    );
    assert!(
        decode_to_chars(
            &[0x41, 0xFF, 0x42],
            Codepage::Utf8,
            UnmappedCharPolicy::StrictError
        )
        .is_err()
    );
    assert_eq!(
        decode_to_chars(
            &[0x41, 0xFF, 0x42],
            Codepage::Utf8,
            UnmappedCharPolicy::Substitute('?')
        )?,
        vec!['A', '?', 'B']
    );

    let rocket_chars = ['A', '🚀', '\u{FFFE}'];
    let big_endian_u16 = encode_from_chars(
        &rocket_chars,
        Codepage::Utf16Be,
        UnmappedCharPolicy::StrictError,
    )?;
    let little_endian_u16 = encode_from_chars(
        &rocket_chars,
        Codepage::Utf16Le,
        UnmappedCharPolicy::StrictError,
    )?;
    assert_eq!(
        decode_to_chars(
            &big_endian_u16,
            Codepage::Utf16Be,
            UnmappedCharPolicy::StrictError
        )?,
        rocket_chars
    );
    assert_eq!(
        decode_to_chars(
            &little_endian_u16,
            Codepage::Utf16Le,
            UnmappedCharPolicy::StrictError
        )?,
        rocket_chars
    );
    Ok(())
}

/// Verifies `UnmappedCharPolicy`, UTF-16 surrogate/odd-byte recovery, `national_of`, `display_of`, and `PIC N`/`PIC U` moves.
#[test]
fn m17_charset_substitution_national_and_utf8_moves_coverage() -> Result<()> {
    use coblang_decimal::charset::{
        Codepage, NationalJustify, UnmappedCharPolicy, decode_to_chars, display_of,
        encode_from_chars, move_national_utf16be, move_utf8_characters, national_of,
    };

    for unmapped_ch in ['🚀', '\u{FFFE}'] {
        assert!(
            encode_from_chars(
                &[unmapped_ch],
                Codepage::Iso8859_1,
                UnmappedCharPolicy::StrictError
            )
            .is_err()
        );
    }
    assert_eq!(
        encode_from_chars(
            &['🚀'],
            Codepage::Iso8859_1,
            UnmappedCharPolicy::Substitute('!')
        )?,
        b"!"
    );
    assert_eq!(
        encode_from_chars(
            &['🚀'],
            Codepage::Iso8859_1,
            UnmappedCharPolicy::Substitute('🚀')
        )?,
        b"?"
    );

    for (bad_u16, expected_sub) in [
        (&[0xD8_u8, 0x00][..], vec!['?']),
        (&[0x00_u8, 0x41, 0x99][..], vec!['A', '?']),
    ] {
        assert!(
            decode_to_chars(bad_u16, Codepage::Utf16Be, UnmappedCharPolicy::StrictError).is_err()
        );
        assert_eq!(
            decode_to_chars(
                bad_u16,
                Codepage::Utf16Be,
                UnmappedCharPolicy::Substitute('?')
            )?,
            expected_sub
        );
    }

    let nat = national_of(b"HI", Codepage::Iso8859_1, UnmappedCharPolicy::StrictError)?;
    assert_eq!(nat, vec![0x00, 0x48, 0x00, 0x49]);
    assert_eq!(
        display_of(&nat, Codepage::Cp037, UnmappedCharPolicy::StrictError)?,
        vec![0xC8, 0xC9]
    );
    assert!(move_national_utf16be(&[0x00], 2, NationalJustify::Left).is_err());
    assert_eq!(
        move_national_utf16be(&nat, 4, NationalJustify::Left)?,
        vec![0x00, 0x48, 0x00, 0x49, 0x00, 0x20, 0x00, 0x20]
    );
    assert_eq!(
        move_national_utf16be(&nat, 1, NationalJustify::Right)?,
        vec![0x00, 0x49]
    );
    assert_eq!(
        move_national_utf16be(&nat, 3, NationalJustify::Right)?,
        vec![0x00, 0x20, 0x00, 0x48, 0x00, 0x49]
    );

    assert_eq!(move_utf8_characters("A€B", 3, 4, 6), b"A\xE2\x82\xAC  ");
    assert_eq!(move_utf8_characters("ABC", 2, 10, 0), b"AB");
    Ok(())
}

/// Verifies `EbcdicCodepage` bijective tables, `ORD`/`CHAR`, EBCDIC/`NATIONAL` comparisons, and figurative units.
#[test]
fn m17_ebcdic_bijective_collation_ordinals_and_figuratives_coverage() -> Result<()> {
    use coblang_decimal::charset::{
        ALL_FIGURATIVE_CHARS, Codepage, EbcdicCodepage, FigurativeConstantChar, FigurativeUnit,
        compare_alphanumeric_ebcdic, compare_national_utf16be, fill_figurative_bytes,
    };

    for (alias, expected_cp) in [
        ("CP01140//TRANSLIT", Codepage::Cp1140),
        ("CSIBM037", Codepage::Cp037),
        ("ebcdic500_latin1", Codepage::Cp500),
        ("IBM01047", Codepage::Cp1047),
        ("ANSI_X3.4-1968", Codepage::Iso8859_1),
    ] {
        if alias == "ANSI_X3.4-1968" {
            assert!(Codepage::from_name(alias).is_err());
        } else {
            assert_eq!(Codepage::from_name(alias)?, expected_cp);
        }
    }
    assert_eq!(Codepage::from_name("ANSI-X34-1968")?, Codepage::Iso8859_1);
    assert_eq!(Codepage::from_name("ISO-IR-203")?, Codepage::Iso8859_15);
    assert_eq!(Codepage::from_name("MS-1252")?, Codepage::Cp1252);
    assert_eq!(Codepage::from_name("UTF-8//IGNORE")?, Codepage::Utf8);
    assert_eq!(Codepage::from_name("UCS-2BE")?, Codepage::Utf16Be);
    assert_eq!(Codepage::from_name("UCS-2LE")?, Codepage::Utf16Le);

    for (tbl_name, expected_eb) in [
        ("default", EbcdicCodepage::Cp1140),
        ("ibm", EbcdicCodepage::Cp037),
        ("restricted-gc", EbcdicCodepage::Cp500),
        ("cp1047", EbcdicCodepage::Cp1047),
    ] {
        assert_eq!(
            EbcdicCodepage::from_ebcdic_table_name(tbl_name)?,
            expected_eb
        );
    }
    assert!(EbcdicCodepage::from_ebcdic_table_name("UTF-8").is_err());

    for &cp in Codepage::all() {
        assert_eq!(
            EbcdicCodepage::try_from_codepage(cp).is_some(),
            cp.is_ebcdic()
        );
        for fig in ALL_FIGURATIVE_CHARS {
            let unit = cp.figurative_unit(fig);
            let slice = unit.as_bytes();
            let mut buf = vec![0_u8; slice.len() * 2];
            fill_figurative_bytes(&mut buf, cp, fig)?;
            assert_eq!(buf.get(..slice.len()), Some(slice));
            if matches!(unit, FigurativeUnit::DoubleByte(_)) {
                let mut odd = [0_u8; 3];
                assert!(fill_figurative_bytes(&mut odd, cp, fig).is_err());
            }
        }
    }
    assert_eq!(
        Codepage::Cp1140.figurative_unit(FigurativeConstantChar::Zero),
        FigurativeUnit::SingleByte(0xF0)
    );
    assert_eq!(
        Codepage::Utf16Be.figurative_unit(FigurativeConstantChar::HighValue),
        FigurativeUnit::DoubleByte([0xFF, 0xFF])
    );

    let all_256: Vec<u8> = (0_u8..=255_u8).collect();
    for &eb in EbcdicCodepage::all() {
        assert_eq!(
            EbcdicCodepage::try_from_codepage(eb.to_codepage()),
            Some(eb)
        );
        let enc = eb.ascii_to_ebcdic_bytes(&all_256);
        let dec = eb.ebcdic_to_ascii_bytes(&enc);
        assert_eq!(dec, all_256);
        for b in 0_u8..=255_u8 {
            let ord = eb.ord_1based_of_ascii(b);
            assert!((1..=256).contains(&ord));
            assert_eq!(eb.ascii_char_from_ord_1based(ord)?, b);
        }
        assert!(eb.ascii_char_from_ord_1based(0).is_err());
        assert!(eb.ascii_char_from_ord_1based(257).is_err());
    }

    assert_eq!(
        compare_alphanumeric_ebcdic(b"a", b"A", EbcdicCodepage::Cp1140),
        Ordering::Less
    );
    assert_eq!(
        compare_alphanumeric_ebcdic(b"0", b"Z", EbcdicCodepage::Cp1140),
        Ordering::Greater
    );
    assert_eq!(
        compare_alphanumeric_ebcdic(b"AB", b"AB  ", EbcdicCodepage::Cp1140),
        Ordering::Equal
    );

    assert!(compare_national_utf16be(&[0x00], &[0x00, 0x20]).is_err());
    assert!(compare_national_utf16be(&[0x00, 0x20], &[0x00]).is_err());
    assert_eq!(
        compare_national_utf16be(&[0x00, 0x41], &[0x00, 0x41, 0x00, 0x20])?,
        Ordering::Equal
    );
    assert_eq!(
        compare_national_utf16be(&[0x00, 0x41], &[0x00, 0x42])?,
        Ordering::Less
    );
    Ok(())
}
