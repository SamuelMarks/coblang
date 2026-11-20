//! Comprehensive integration and branch-coverage suite for `coblang-intrinsics` (`Milestone M12`).

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};
use coblang_decimal::{
    DecimalSign, FixedDecimal, SignedInt256, SignednessPolicy, Uint256,
    charset::{Codepage, UnmappedCharPolicy},
};
use coblang_intrinsics::{
    AlgebraicTargetSpec, IntrinsicCategory, IntrinsicId, IntrinsicReturnKind,
    datetime::{
        ClockSnapshot, MAX_LILIAN_DAY, MIN_LILIAN_DAY, UtcOffsetSpec, dt_combined_datetime,
        dt_current_date, dt_date_of_integer, dt_date_to_yyyymmdd, dt_day_of_integer,
        dt_day_to_yyyyddd, dt_formatted_current_date, dt_formatted_date, dt_formatted_datetime,
        dt_formatted_time, dt_integer_of_date, dt_integer_of_day, dt_integer_of_formatted_date,
        dt_locale_date, dt_locale_time, dt_locale_time_from_seconds,
        dt_seconds_from_formatted_time, dt_seconds_past_midnight, dt_test_date_yyyymmdd,
        dt_test_day_yyyyddd, dt_test_formatted_datetime, dt_when_compiled, dt_year_to_yyyy,
    },
    ec_argument_function,
    math::{
        MAX_EXACT_FACTORIAL_ARG, math_abs, math_acos, math_asin, math_atan, math_cos, math_e,
        math_exp, math_exp10, math_factorial, math_fraction_part, math_highest_algebraic,
        math_integer, math_integer_part, math_log, math_log10, math_lowest_algebraic, math_mod,
        math_pi, math_rem, math_sign, math_sin, math_smallest_algebraic, math_sqrt, math_tan,
    },
    meta_unicode::{
        EXCEPTION_REGISTER_WIDTH, ExceptionReflectionState, ModuleReflectionInfo,
        NationalByteOrder, UnicodeSlice, Uuid4Generator, meta_char_national, meta_content_of,
        meta_currency_symbol, meta_display_of, meta_exception_file, meta_exception_file_n,
        meta_exception_location, meta_exception_location_n, meta_exception_statement,
        meta_exception_status, meta_module_caller_id, meta_module_date, meta_module_formatted_date,
        meta_module_id, meta_module_name, meta_module_path, meta_module_source, meta_module_time,
        meta_national_of, meta_ulength, meta_upos, meta_usubstr, meta_usupplementary, meta_uuid4,
        meta_uuid4_from_entropy, meta_uvalid, meta_uwidth,
    },
    stats::{
        AlphaSeriesArg, NumericSeriesArg, RandomGenerator, RandomSeed, stats_annuity, stats_max,
        stats_max_alpha, stats_max_series, stats_mean, stats_median, stats_midrange, stats_min,
        stats_min_alpha, stats_min_series, stats_ord_max, stats_ord_max_alpha,
        stats_ord_max_series, stats_ord_min, stats_ord_min_alpha, stats_ord_min_series,
        stats_present_value, stats_random, stats_range, stats_standard_deviation, stats_sum,
        stats_sum_series, stats_variance,
    },
    strings::{
        CaseSensitivity, CollatingTable, CompareIndicator, ConvertFormat, ConvertSpec,
        CurrencyCasePolicy, FindStringMode, FindStringSpec, MeasuredItem, NumericBase,
        TrimDirection, str_baseconvert, str_bit_of, str_bit_to_char, str_boolean_of_integer,
        str_byte_length, str_char, str_concat, str_concatenate, str_convert, str_find_string,
        str_hex_of, str_hex_to_char, str_integer_of_boolean, str_length, str_length_an,
        str_locale_compare, str_lower_case, str_numval, str_numval_c, str_numval_f, str_ord,
        str_reverse, str_standard_compare, str_stored_char_length, str_substitute,
        str_substitute_case, str_test_numval, str_test_numval_c, str_test_numval_f, str_trim,
        str_upper_case,
    },
};
use coblang_pic::DecimalPointKind;

/// Parses a decimal literal in test assertions.
#[must_use]
fn dec(lit: &str) -> FixedDecimal {
    FixedDecimal::parse_str(lit).unwrap_or(FixedDecimal::ZERO)
}

/// Asserts that `err` is an `EC-ARGUMENT-FUNCTION` domain error mentioning `fn_name`.
fn assert_ec_arg_fn(err: &CoblangError, fn_name: &str) {
    assert!(matches!(
        err,
        CoblangError::InvalidDomainValue {
            domain_type: "EC-ARGUMENT-FUNCTION",
            value,
            ..
        } if value.starts_with(fn_name)
    ));
}

/// Verifies `IntrinsicId` catalog lookups, ABI codes, categories, arity bounds, and return kinds.
#[test]
fn intrinsic_id_catalog_and_metadata_round_trips() -> Result<()> {
    assert_eq!(IntrinsicId::ALL.len(), 116);
    for (idx, &id) in IntrinsicId::ALL.iter().enumerate() {
        let expected_code = u16::try_from(idx + 1).unwrap_or(0);
        assert_eq!(id.to_abi_code(), expected_code);
        assert_eq!(IntrinsicId::from_abi_code(expected_code)?, id);
        let name = id.canonical_name();
        assert_eq!(IntrinsicId::from_cobol_name(name)?, id);
        assert_eq!(
            IntrinsicId::from_cobol_name(&name.to_ascii_lowercase())?,
            id
        );
        let (min_a, max_a) = id.arity_bounds();
        assert!(min_a <= max_a);
        let _cat = id.category();
        let _ret = id.return_kind();
    }

    assert_eq!(
        IntrinsicId::from_cobol_name("CONCAT")?,
        IntrinsicId::Concatenate
    );
    assert_eq!(
        IntrinsicId::from_cobol_name("MODULE-NAME")?,
        IntrinsicId::ModuleId
    );
    assert!(IntrinsicId::from_cobol_name("NO-SUCH-FUNCTION").is_err());
    assert!(IntrinsicId::from_abi_code(0).is_err());
    assert!(IntrinsicId::from_abi_code(117).is_err());

    assert_eq!(IntrinsicId::Abs.category(), IntrinsicCategory::Math);
    assert_eq!(IntrinsicId::Annuity.category(), IntrinsicCategory::Stats);
    assert_eq!(
        IntrinsicId::CurrentDate.category(),
        IntrinsicCategory::DateTime
    );
    assert_eq!(IntrinsicId::Trim.category(), IntrinsicCategory::Strings);
    assert_eq!(
        IntrinsicId::Uuid4.category(),
        IntrinsicCategory::MetaUnicode
    );

    assert_eq!(
        IntrinsicId::Factorial.return_kind(),
        IntrinsicReturnKind::Integer
    );
    assert_eq!(
        IntrinsicId::NationalOf.return_kind(),
        IntrinsicReturnKind::National
    );
    assert_eq!(
        IntrinsicId::UpperCase.return_kind(),
        IntrinsicReturnKind::Alphanumeric
    );
    assert_eq!(IntrinsicId::Sin.return_kind(), IntrinsicReturnKind::Numeric);

    let err = ec_argument_function("SQRT", "-4", "negative");
    assert_ec_arg_fn(&err, "SQRT");
    Ok(())
}

/// Verifies trigonometric, exponential, logarithmic, square-root, and factorial math intrinsics.
#[test]
fn math_trig_exp_log_sqrt_and_factorial_coverage() -> Result<()> {
    assert_eq!(math_abs(&dec("-42.75")), dec("42.75"));
    assert_eq!(math_abs(&dec("0")), FixedDecimal::ZERO);
    assert_eq!(math_sign(&dec("-0.001")), FixedDecimal::from_i64(-1));
    assert_eq!(math_sign(&dec("0.00")), FixedDecimal::ZERO);
    assert_eq!(math_sign(&dec("99")), FixedDecimal::from_i64(1));

    assert!((math_e().to_f64() - core::f64::consts::E).abs() < 1e-15);
    assert!((math_pi().to_f64() - core::f64::consts::PI).abs() < 1e-15);

    assert!((math_acos(&dec("1"))?.to_f64() - 0.0).abs() < 1e-14);
    assert!((math_acos(&dec("-1"))?.to_f64() - core::f64::consts::PI).abs() < 1e-14);
    assert_ec_arg_fn(
        &math_acos(&dec("-1.01"))
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "ACOS",
    );
    assert_ec_arg_fn(
        &math_acos(&dec("1.01"))
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "ACOS",
    );

    assert!((math_asin(&dec("0"))?.to_f64() - 0.0).abs() < 1e-14);
    assert!((math_asin(&dec("1"))?.to_f64() - core::f64::consts::FRAC_PI_2).abs() < 1e-14);
    assert_ec_arg_fn(
        &math_asin(&dec("-1.5"))
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "ASIN",
    );
    assert_ec_arg_fn(
        &math_asin(&dec("1.5"))
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "ASIN",
    );

    assert!((math_atan(&dec("1")).to_f64() - core::f64::consts::FRAC_PI_4).abs() < 1e-14);
    assert!((math_cos(&dec("0")).to_f64() - 1.0).abs() < 1e-14);
    assert!((math_sin(&dec("0")).to_f64() - 0.0).abs() < 1e-14);
    assert!((math_tan(&dec("0")).to_f64() - 0.0).abs() < 1e-14);

    assert!((math_exp(&dec("1"))?.to_f64() - core::f64::consts::E).abs() < 1e-14);
    assert!(math_exp(&dec("100"))?.to_f64() > 1.0e43);
    assert_ec_arg_fn(
        &math_exp(&dec("131"))
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "EXP",
    );
    assert_eq!(math_exp10(&dec("0"))?, FixedDecimal::from_i64(1));
    assert_eq!(math_exp10(&dec("3"))?, FixedDecimal::from_i64(1000));
    assert_eq!(math_exp10(&dec("-2"))?, dec("0.01"));
    assert!((math_exp10(&dec("0.5"))?.to_f64() - 10.0_f64.sqrt()).abs() < 1e-13);
    assert!((math_exp10(&dec("-25"))?.to_f64() - 1e-25).abs() < 1e-30);
    assert!((math_exp10(&dec("-25.5"))?.to_f64() - 10.0_f64.powf(-25.5)).abs() < 1e-30);
    assert!(math_exp10(&dec("-80"))?.is_zero());
    assert_ec_arg_fn(
        &math_exp10(&dec("58"))
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "EXP10",
    );

    assert_eq!(math_factorial(&dec("0"))?, FixedDecimal::from_i64(1));
    assert_eq!(math_factorial(&dec("5"))?, FixedDecimal::from_i64(120));
    assert!(
        !math_factorial(&FixedDecimal::from_i64(i64::from(MAX_EXACT_FACTORIAL_ARG)))?.is_zero()
    );
    for bad in ["2.5", "-1", "300", "57"] {
        assert_ec_arg_fn(
            &math_factorial(&dec(bad))
                .err()
                .unwrap_or_else(|| ec_argument_function("X", "", "")),
            "FACTORIAL",
        );
    }

    assert!((math_log(&math_e())?.to_f64() - 1.0).abs() < 1e-14);
    assert!((math_log10(&dec("1000"))?.to_f64() - 3.0).abs() < 1e-14);
    for bad in ["0", "-2"] {
        assert_ec_arg_fn(
            &math_log(&dec(bad))
                .err()
                .unwrap_or_else(|| ec_argument_function("X", "", "")),
            "LOG",
        );
        assert_ec_arg_fn(
            &math_log10(&dec(bad))
                .err()
                .unwrap_or_else(|| ec_argument_function("X", "", "")),
            "LOG10",
        );
    }
    assert_eq!(math_sqrt(&dec("0"))?, FixedDecimal::ZERO);
    assert!((math_sqrt(&dec("9"))?.to_f64() - 3.0).abs() < 1e-14);
    assert_ec_arg_fn(
        &math_sqrt(&dec("-0.01"))
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "SQRT",
    );
    Ok(())
}

/// Verifies integer/fraction truncation, algebraic limits, and `MOD`/`REM` intrinsics.
#[test]
fn math_integer_algebraic_mod_and_rem_coverage() -> Result<()> {
    assert_eq!(math_integer(&dec("1.9")), FixedDecimal::from_i64(1));
    assert_eq!(math_integer(&dec("-1.1")), FixedDecimal::from_i64(-2));
    assert_eq!(math_integer(&dec("-2.0")), FixedDecimal::from_i64(-2));
    assert_eq!(math_integer(&dec("7")), FixedDecimal::from_i64(7));
    assert_eq!(math_integer_part(&dec("1.9")), FixedDecimal::from_i64(1));
    assert_eq!(math_integer_part(&dec("-1.9")), FixedDecimal::from_i64(-1));
    assert_eq!(math_integer_part(&dec("7")), FixedDecimal::from_i64(7));
    assert_eq!(math_fraction_part(&dec("3.14")), dec("0.14"));
    assert_eq!(math_fraction_part(&dec("-3.14")), dec("-0.14"));
    assert_eq!(math_fraction_part(&dec("42")), FixedDecimal::ZERO);

    let p_scaled = FixedDecimal::try_new(SignedInt256::from_i64(5), -2)?;
    assert_eq!(math_integer(&p_scaled), FixedDecimal::from_i64(500));
    assert_eq!(math_integer_part(&p_scaled), FixedDecimal::from_i64(500));
    let huge_neg_scale = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::MAX, DecimalSign::Positive),
        -10,
    )?;
    assert_eq!(math_integer(&huge_neg_scale), huge_neg_scale);
    assert_eq!(math_integer_part(&huge_neg_scale), huge_neg_scale);

    let s99v99 = AlgebraicTargetSpec::new(
        DigitCount::try_new(4)?,
        ScaleFactor::try_new(2)?,
        SignednessPolicy::Signed,
    );
    let u99v99 = AlgebraicTargetSpec::new(
        DigitCount::try_new(4)?,
        ScaleFactor::try_new(2)?,
        SignednessPolicy::Unsigned,
    );
    assert_eq!(math_highest_algebraic(s99v99), dec("99.99"));
    assert_eq!(math_lowest_algebraic(s99v99), dec("-99.99"));
    assert_eq!(math_lowest_algebraic(u99v99), FixedDecimal::ZERO);
    assert_eq!(math_smallest_algebraic(s99v99), dec("0.01"));
    assert_eq!(math_smallest_algebraic(u99v99), dec("0.01"));

    assert_eq!(math_mod(&dec("11"), &dec("5"))?, dec("1"));
    assert_eq!(math_mod(&dec("-11"), &dec("5"))?, dec("4"));
    assert_eq!(math_mod(&dec("11"), &dec("-5"))?, dec("-4"));
    assert_eq!(math_mod(&dec("-11"), &dec("-5"))?, dec("-1"));
    assert_ec_arg_fn(
        &math_mod(&dec("11"), &dec("0"))
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "MOD",
    );

    assert_eq!(math_rem(&dec("11"), &dec("5"))?, dec("1"));
    assert_eq!(math_rem(&dec("-11"), &dec("5"))?, dec("-1"));
    assert_eq!(math_rem(&dec("11"), &dec("-5"))?, dec("1"));
    assert_eq!(math_rem(&dec("-11"), &dec("-5"))?, dec("-1"));
    assert_eq!(math_rem(&dec("11.5"), &dec("2.5"))?, dec("1.5"));
    assert_ec_arg_fn(
        &math_rem(&dec("11"), &dec("0"))
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "REM",
    );

    let tiny = FixedDecimal::try_new(SignedInt256::ONE, 70)?;
    let huge = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::MAX, DecimalSign::Positive),
        -70,
    )?;
    assert_ec_arg_fn(
        &math_rem(&huge, &tiny)
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "REM",
    );
    let a_align = FixedDecimal::try_new(SignedInt256::from_i64(2), -35)?;
    let b_align = FixedDecimal::try_new(SignedInt256::from_i64(3), 42)?;
    assert_ec_arg_fn(
        &math_rem(&a_align, &b_align)
            .err()
            .unwrap_or_else(|| ec_argument_function("X", "", "")),
        "REM",
    );
    Ok(())
}

/// Verifies `ANNUITY`, `PRESENT-VALUE`, and variadic/`ALL`-slice `MAX`/`MIN`/`ORD-*`/`SUM`.
#[test]
fn stats_financial_extrema_and_series_coverage() -> Result<()> {
    let tiny_rate = FixedDecimal::try_new(SignedInt256::ONE, 30)?;
    assert!((stats_annuity(&dec("0"), &dec("4"))?.to_f64() - 0.25).abs() < 1e-14);
    assert!((stats_annuity(&tiny_rate, &dec("2"))?.to_f64() - 0.5).abs() < 1e-14);
    assert!((stats_annuity(&dec("0.05"), &dec("1"))?.to_f64() - 1.05).abs() < 1e-12);
    assert!(stats_annuity(&dec("-0.01"), &dec("5")).is_err());
    assert!(stats_annuity(&dec("0.05"), &dec("2.5")).is_err());
    assert!(stats_annuity(&dec("0.05"), &dec("0")).is_err());

    let flows = [dec("110"), dec("121")];
    assert!((stats_present_value(&dec("0.10"), &flows)?.to_f64() - 200.0).abs() < 1e-11);
    assert_eq!(stats_present_value(&dec("0"), &flows)?, dec("231"));
    let big_flow = [FixedDecimal::try_new(SignedInt256::ONE, -40)?];
    assert!(stats_present_value(&dec("0.25"), &big_flow)?.to_f64() > 1.0e39);
    assert!(stats_present_value(&dec("0.10"), &[]).is_err());
    assert!(stats_present_value(&dec("-1"), &flows).is_err());
    let overflow_flow = [FixedDecimal::try_new(SignedInt256::ONE, -55)?];
    assert!(stats_present_value(&dec("-0.9999"), &overflow_flow).is_err());
    let inf_flow = [
        FixedDecimal::try_new(SignedInt256::ONE, -76)?,
        FixedDecimal::try_new(SignedInt256::ONE, -76)?,
    ];
    let near_neg_one = dec("-0.9999999999999999");
    let many_inf: Vec<FixedDecimal> = inf_flow.into_iter().cycle().take(20).collect();
    assert!(stats_present_value(&near_neg_one, &many_inf).is_err());

    let tbl = [dec("50"), dec("50"), dec("-10")];
    let series = [
        NumericSeriesArg::Scalar(dec("20")),
        NumericSeriesArg::TableAll(&tbl),
        NumericSeriesArg::Scalar(dec("-10")),
    ];
    assert_eq!(stats_max_series(&series)?, dec("50"));
    assert_eq!(stats_min_series(&series)?, dec("-10"));
    assert_eq!(stats_ord_max_series(&series)?, 2);
    assert_eq!(stats_ord_min_series(&series)?, 4);
    assert_eq!(stats_sum_series(&series)?, dec("100"));
    assert!(stats_max(&[]).is_err());
    assert!(stats_min(&[]).is_err());
    assert!(stats_ord_max(&[]).is_err());
    assert!(stats_ord_min(&[]).is_err());
    assert!(stats_sum(&[]).is_err());

    let alpha_tbl: [&[u8]; 2] = [b"CHARLIE", b"CHARLIE   "];
    let alpha_series = [
        AlphaSeriesArg::Scalar(b"BRAVO"),
        AlphaSeriesArg::TableAll(&alpha_tbl),
        AlphaSeriesArg::Scalar(b"ALPHA  "),
        AlphaSeriesArg::Scalar(b"ALPHA"),
    ];
    assert_eq!(stats_max_alpha(&alpha_series)?, b"CHARLIE");
    assert_eq!(stats_min_alpha(&alpha_series)?, b"ALPHA  ");
    assert_eq!(stats_ord_max_alpha(&alpha_series)?, 2);
    assert_eq!(stats_ord_min_alpha(&alpha_series)?, 4);
    assert!(stats_max_alpha(&[]).is_err());
    assert!(stats_min_alpha(&[]).is_err());
    assert!(stats_ord_max_alpha(&[]).is_err());
    assert!(stats_ord_min_alpha(&[]).is_err());
    Ok(())
}

/// Verifies `MEAN`, `MEDIAN`, `MIDRANGE`, `RANGE`, `VARIANCE`, `STANDARD-DEVIATION`, and `RANDOM`.
#[test]
fn stats_moments_overflow_and_random_coverage() -> Result<()> {
    let odd = [dec("30"), dec("10"), dec("20")];
    let even = [dec("40"), dec("10"), dec("20"), dec("30")];
    assert_eq!(stats_mean(&odd)?.to_f64(), 20.0);
    assert_eq!(stats_median(&odd)?, dec("20"));
    assert_eq!(stats_median(&even)?, dec("25.0"));
    assert_eq!(
        stats_midrange(&[dec("10"), dec("20"), dec("100")])?,
        dec("55.0")
    );
    assert_eq!(stats_range(&[dec("-5"), dec("20"), dec("10")])?, dec("25"));
    assert_eq!(stats_variance(&[dec("42")])?, FixedDecimal::ZERO);
    assert!((stats_variance(&[dec("1"), dec("3")])?.to_f64() - 1.0).abs() < 1e-14);
    assert_eq!(
        stats_standard_deviation(&[dec("7"), dec("7")])?,
        FixedDecimal::ZERO
    );
    assert!((stats_standard_deviation(&[dec("1"), dec("3")])?.to_f64() - 1.0).abs() < 1e-14);

    assert!(stats_mean(&[]).is_err());
    assert!(stats_median(&[]).is_err());
    assert!(stats_midrange(&[]).is_err());
    assert!(stats_range(&[]).is_err());
    assert!(stats_variance(&[]).is_err());
    assert!(stats_standard_deviation(&[]).is_err());

    let max_pos = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::MAX, DecimalSign::Positive),
        0,
    )?;
    let max_neg = max_pos.negate();
    assert!(stats_sum(&[max_pos, max_pos]).is_err());
    assert!(stats_mean(&[max_pos, max_pos]).is_err());
    assert!(stats_median(&[max_pos, max_pos]).is_err());
    assert!(stats_midrange(&[max_pos, max_pos]).is_err());
    assert!(stats_range(&[max_neg, max_pos]).is_err());

    let astro = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::MAX, DecimalSign::Positive),
        -75,
    )?;
    assert!(stats_variance(&[astro.negate(), astro]).is_err());
    assert!(stats_standard_deviation(&[astro.negate(), astro]).is_err());

    let mut rng_a = RandomGenerator::default();
    let mut rng_b = RandomGenerator::new();
    let r1 = stats_random(&mut rng_a, RandomSeed::Unseeded)?;
    let r2 = stats_random(&mut rng_b, RandomSeed::Unseeded)?;
    assert_eq!(r1, r2);
    assert!(r1.cmp_numeric(&FixedDecimal::ZERO).is_ge());
    assert!(r1.cmp_numeric(&FixedDecimal::from_i64(1)).is_lt());

    let s1 = stats_random(&mut rng_a, RandomSeed::Seeded(&dec("12345")))?;
    let s1_next = stats_random(&mut rng_a, RandomSeed::Unseeded)?;
    let s2 = stats_random(&mut rng_b, RandomSeed::Seeded(&dec("12345")))?;
    let s2_next = stats_random(&mut rng_b, RandomSeed::Unseeded)?;
    assert_eq!((s1, s1_next), (s2, s2_next));

    assert!(stats_random(&mut rng_a, RandomSeed::Seeded(&dec("-1"))).is_err());
    assert!(stats_random(&mut rng_a, RandomSeed::Seeded(&dec("1.5"))).is_err());
    assert!(stats_random(&mut rng_a, RandomSeed::Seeded(&max_pos)).is_err());
    let over_u64 = FixedDecimal::try_new(SignedInt256::from_i128(i128::from(u64::MAX) + 1), 0)?;
    assert!(stats_random(&mut rng_a, RandomSeed::Seeded(&over_u64)).is_err());
    Ok(())
}

/// Verifies `CURRENT-DATE`, `WHEN-COMPILED`, `SECONDS-PAST-MIDNIGHT`, Lilian bijections, and sliding windows.
#[test]
fn datetime_clock_lilian_and_sliding_window_coverage() -> Result<()> {
    let live = ClockSnapshot::now_system_utc();
    assert!(live.year >= 1970);
    let epoch = ClockSnapshot::from_unix_utc(0, 250_000_000);
    assert_eq!(&dt_current_date(epoch, None)?, b"1970010100000025+0000");

    let mut unzoned = epoch;
    unzoned.offset = UtcOffsetSpec::Unknown;
    assert_eq!(&dt_when_compiled(unzoned)?, b"197001010000002500000");
    assert_eq!(dt_seconds_past_midnight(epoch)?, dec("0.250000000"));

    assert_eq!(
        &dt_current_date(epoch, Some("20261119"))?,
        b"2026111900000025+0000"
    );
    assert_eq!(
        &dt_current_date(epoch, Some("20261119143059"))?,
        b"2026111914305900+0000"
    );
    assert_eq!(
        &dt_current_date(epoch, Some("2026-11-19T14:30:59.75-05:30"))?,
        b"2026111914305975-0530"
    );
    assert_eq!(
        &dt_current_date(epoch, Some("2026-11-19T14:30:59Z"))?,
        b"2026111914305900+0000"
    );
    for bad_env in [
        "2026",
        "2026$119",
        "20261119143059+02X0",
        "20261119143059+02",
        "20261119143059+2400",
        "20261119143059+0060",
    ] {
        assert!(dt_current_date(epoch, Some(bad_env)).is_err());
    }

    let mut bad_clk = epoch;
    bad_clk.month = 13;
    assert!(dt_when_compiled(bad_clk).is_err());
    bad_clk = epoch;
    bad_clk.hour = 24;
    assert!(dt_when_compiled(bad_clk).is_err());
    bad_clk = epoch;
    bad_clk.minute = 60;
    assert!(dt_seconds_past_midnight(bad_clk).is_err());
    bad_clk = epoch;
    bad_clk.second = 61;
    assert!(dt_when_compiled(bad_clk).is_err());
    bad_clk = epoch;
    bad_clk.nanosecond = 1_000_000_000;
    assert!(dt_when_compiled(bad_clk).is_err());
    bad_clk = epoch;
    bad_clk.offset = UtcOffsetSpec::Minutes(1500);
    assert!(dt_when_compiled(bad_clk).is_err());

    assert_eq!(dt_integer_of_date(16_010_101)?, MIN_LILIAN_DAY);
    assert_eq!(dt_integer_of_date(99_991_231)?, MAX_LILIAN_DAY);
    assert_eq!(dt_date_of_integer(MIN_LILIAN_DAY)?, 16_010_101);
    assert_eq!(dt_date_of_integer(MAX_LILIAN_DAY)?, 99_991_231);
    assert_eq!(dt_integer_of_day(1_601_001)?, MIN_LILIAN_DAY);
    assert_eq!(dt_day_of_integer(MAX_LILIAN_DAY)?, 9_999_365);

    for m in 1..=12_i64 {
        let ymd = 20_240_000 + m * 100 + 15;
        let l = dt_integer_of_date(ymd)?;
        assert_eq!(dt_date_of_integer(l)?, ymd);
    }
    for d in 1..=7_i64 {
        let l = dt_integer_of_day(1_601_000 + d)?;
        assert_eq!(dt_day_of_integer(l)?, 1_601_000 + d);
    }
    assert_eq!(dt_test_date_yyyymmdd(15_991_231), 1);
    assert_eq!(dt_test_date_yyyymmdd(20_241_301), 2);
    assert_eq!(dt_test_date_yyyymmdd(20_240_100), 3);
    assert_eq!(dt_test_date_yyyymmdd(17_000_229), 3);
    assert_eq!(dt_test_date_yyyymmdd(20_000_229), 0);
    assert_eq!(dt_test_day_yyyyddd(1_500_001), 1);
    assert_eq!(dt_test_day_yyyyddd(2_024_000), 2);
    assert_eq!(dt_test_day_yyyyddd(2_023_366), 2);
    assert_eq!(dt_test_day_yyyyddd(2_024_366), 0);
    assert!(dt_integer_of_date(17_000_229).is_err());
    assert!(dt_date_of_integer(0).is_err());
    assert!(dt_integer_of_day(2_023_366).is_err());
    assert!(dt_day_of_integer(MAX_LILIAN_DAY + 1).is_err());

    assert_eq!(dt_year_to_yyyy(45, None, 1995)?, 2045);
    assert_eq!(dt_year_to_yyyy(46, Some(50), 1995)?, 1946);
    assert!(dt_year_to_yyyy(100, None, 1995).is_err());
    assert!(dt_year_to_yyyy(10, None, 1500).is_err());
    assert!(dt_year_to_yyyy(10, Some(-100), 1750).is_err());
    assert_eq!(dt_date_to_yyyymmdd(240_229, None, 2020)?, 20_240_229);
    assert!(dt_date_to_yyyymmdd(0, None, 2020).is_err());
    assert!(dt_date_to_yyyymmdd(230_229, None, 2020).is_err());
    assert_eq!(dt_day_to_yyyyddd(24_366, None, 2020)?, 2_024_366);
    assert!(dt_day_to_yyyyddd(0, None, 2020).is_err());
    assert!(dt_day_to_yyyyddd(23_366, None, 2020).is_err());

    assert_eq!(dt_combined_datetime(10, &dec("43200.5"))?, dec("10.432005"));
    assert!(dt_combined_datetime(0, &dec("0")).is_err());
    assert!(dt_combined_datetime(1, &dec("-1")).is_err());
    assert!(dt_combined_datetime(1, &dec("86400")).is_err());
    let ultra_prec = FixedDecimal::try_new(SignedInt256::ONE, 73)?;
    assert!(dt_combined_datetime(1, &ultra_prec).is_err());
    Ok(())
}

/// Verifies ISO 8601 `FORMATTED-*`, `INTEGER-OF-FORMATTED-DATE`, and `SECONDS-FROM-FORMATTED-TIME`.
#[test]
fn datetime_iso8601_formatting_and_parsing_coverage() -> Result<()> {
    let clk = ClockSnapshot {
        year: 2026,
        month: 11,
        day: 19,
        hour: 9,
        minute: 5,
        second: 3,
        nanosecond: 500_000_000,
        offset: UtcOffsetSpec::Unknown,
    };
    assert_eq!(
        dt_formatted_current_date("YYYY-MM-DD", clk, None)?,
        "2026-11-19"
    );
    assert_eq!(
        dt_formatted_current_date("hh:mm:ss.s", clk, None)?,
        "09:05:03.5"
    );
    assert_eq!(
        dt_formatted_current_date("YYYYMMDDThhmmss+hhmm", clk, Some("2026111909050300+0200"))?,
        "20261119T090503+0200"
    );

    let l_2021_jan1 = dt_integer_of_date(20_210_101)?; // 2020-W53-5
    let l_2019_dec30 = dt_integer_of_date(20_191_230)?; // 2020-W01-1
    let l_2026_nov19 = dt_integer_of_date(20_261_119)?; // 2026-W47-4
    assert_eq!(dt_formatted_date("YYYY-Www-D", l_2021_jan1)?, "2020-W53-5");
    assert_eq!(dt_formatted_date("YYYYWwwD", l_2019_dec30)?, "2020W011");
    assert_eq!(dt_formatted_date("YYYY-Www-D", l_2026_nov19)?, "2026-W47-4");
    assert_eq!(
        dt_formatted_date("YYYY-Www-D", MAX_LILIAN_DAY)?,
        "9999-W52-5"
    );
    for wd_lilian in 1..=7_i64 {
        let w_str = dt_formatted_date("YYYY-Www-D", wd_lilian)?;
        assert_eq!(
            dt_integer_of_formatted_date("YYYY-Www-D", &w_str)?,
            wd_lilian
        );
    }
    for fmt in [
        "YYYYMMDD",
        "YYYY-MM-DD",
        "YYYYDDD",
        "YYYY-DDD",
        "YYYYWwwD",
        "YYYY-Www-D",
    ] {
        let s = dt_formatted_date(fmt, l_2021_jan1)?;
        assert_eq!(dt_integer_of_formatted_date(fmt, &s)?, l_2021_jan1);
    }
    assert!(dt_formatted_date("hh:mm:ss", 1).is_err());
    assert!(dt_integer_of_formatted_date("hh:mm:ss", "12:00:00").is_err());
    assert!(dt_integer_of_formatted_date("YYYY-MM-DD", "2024-99-01").is_err());

    assert_eq!(
        dt_formatted_time("hhmmss,ss+hhmm", &dec("3661.25"), Some(-300), None)?,
        "010101,25-0500"
    );
    assert_eq!(
        dt_formatted_time("hh:mm:ssZ", &dec("3600"), Some(60), None)?,
        "00:00:00Z"
    );
    assert_eq!(
        dt_formatted_time("hh:mm:ssZ", &dec("3600"), None, None)?,
        "01:00:00Z"
    );
    assert!(dt_formatted_time("YYYYMMDD", &dec("0"), None, None).is_err());
    assert!(dt_formatted_time("hh:mm:ss+hh:mm", &dec("0"), None, None).is_err());
    assert!(dt_formatted_time("hh:mm:ss", &dec("0"), Some(2000), None).is_err());
    assert!(dt_formatted_time("hh:mm:ss", &dec("0"), None, Some(-2000)).is_err());

    // UTC Z conversion rolling across midnight backward & forward + Lilian overflow
    assert_eq!(
        dt_formatted_datetime("YYYY-MM-DDThh:mm:ssZ", 2, &dec("1800"), None, Some(60))?,
        "1601-01-01T23:30:00Z"
    );
    assert_eq!(
        dt_formatted_datetime("YYYY-MM-DDThh:mm:ssZ", 2, &dec("85000"), None, Some(-60))?,
        "1601-01-03T00:36:40Z"
    );
    assert_eq!(
        dt_formatted_datetime(
            "YYYY-MM-DDThh:mm:ss+hh:mm",
            2,
            &dec("3600"),
            Some(120),
            None
        )?,
        "1601-01-02T01:00:00+02:00"
    );
    assert!(dt_formatted_datetime("YYYY-MM-DDThh:mm:ssZ", 1, &dec("60"), None, Some(60)).is_err());
    assert!(dt_formatted_datetime("YYYY-MM-DD", 1, &dec("0"), None, None).is_err());

    assert_eq!(
        dt_seconds_from_formatted_time("hhmmss", "010203")?,
        dec("3723")
    );
    assert_eq!(
        dt_seconds_from_formatted_time("YYYY-MM-DDThh:mm:ss.ssZ", "2026-11-19T01:02:03.45Z")?,
        dec("3723.45")
    );
    assert!(dt_seconds_from_formatted_time("YYYYMMDD", "20261119").is_err());
    assert!(dt_seconds_from_formatted_time("hh:mm:ss", "99:00:00").is_err());
    Ok(())
}

/// Verifies `TEST-FORMATTED-DATETIME` error pinpointing and `LOCALE-*` intrinsics.
#[test]
fn datetime_test_formatted_and_locale_coverage() -> Result<()> {
    assert!(dt_test_formatted_datetime("NOT-A-FORMAT", "2026").is_err());
    assert!(dt_test_formatted_datetime("hh:mm:ss.", "12:00:00.").is_err());
    assert!(dt_test_formatted_datetime("hh:mm:ss+bad", "12:00:00").is_err());
    assert!(dt_test_formatted_datetime("YYYY-MM-DDThhmmss", "2026-11-19T120000").is_err());

    let cases: &[(&str, &str, usize)] = &[
        ("YYYY-MM-DD", "2026-11-19", 0),
        ("YYYY-MM-DD", "20", 3),
        ("YYYY-MM-DD", "20X6-11-19", 3),
        ("YYYY-MM-DD", "2026-11-19X", 11),
        ("YYYY-MM-DD", "1599-01-01", 2),
        ("YYYY-MM-DD", "2026/11-19", 5),
        ("YYYY-MM-DD", "2026-13-01", 7),
        ("YYYY-MM-DD", "2026-11/19", 8),
        ("YYYYMMDD", "20261301", 6),
        ("YYYY-DDD", "2026/001", 5),
        ("YYYY-Www-D", "2026/W01-1", 5),
        ("YYYY-Www-D", "2026-X01-1", 6),
        ("YYYY-Www-D", "2026-W54-1", 8),
        ("YYYY-Www-D", "2026-W01/1", 9),
        ("YYYY-Www-D", "2026-W01-8", 10),
        ("YYYY-Www-D", "9999-W52-6", 10),
        ("hh:mm:ss.ssZ", "12:34:56.78Z", 0),
        ("hh:mm:ss.ssZ", "12:34:56.78Z!", 13),
        ("hh:mm:ss.ssZ", "25:00:00.00Z", 2),
        ("hh:mm:ss.ssZ", "12-00:00.00Z", 3),
        ("hh:mm:ss.ssZ", "12:60:00.00Z", 4),
        ("hh:mm:ss.ssZ", "12:00-00.00Z", 6),
        ("hh:mm:ss.ssZ", "12:00:60.00Z", 7),
        ("hh:mm:ss.ssZ", "12:00:00,00Z", 9),
        ("hh:mm:ss.ssZ", "12:00:00.0", 11),
        ("hh:mm:ss.ssZ", "12:00:00.0XZ", 11),
        ("hh:mm:ss.ssZ", "12:00:00.00X", 12),
        ("hh:mm:ss+hh:mm", "12:00:00", 9),
        ("hh:mm:ss+hh:mm", "12:00:00Z00:00", 9),
        ("hh:mm:ss+hh:mm", "12:00:00+25:00", 11),
        ("hh:mm:ss+hh:mm", "12:00:00+02-00", 12),
        ("hh:mm:ss+hh:mm", "12:00:00+02:60", 13),
        ("hh:mm:ss+hh:mm", "12:00:00-00:00", 14),
        ("hh:mm:ss+hh:mm", "12:00:00-05:00", 0),
        ("hh:mm:ss+hh:mm", "12:00:00-00:30", 0),
        ("hhmmss+hhmm", "120000+0200", 0),
        ("YYYY-MM-DDThh:mm:ss", "2026-13-01T12:00:00", 7),
        ("YYYY-MM-DDThh:mm:ss", "2026-11-19", 11),
        ("YYYY-MM-DDThh:mm:ss", "2026-11-19 12:00:00", 11),
        ("YYYY-MM-DDThh:mm:ss", "2026-11-19T25:00:00", 13),
        ("YYYY-MM-DDThh:mm:ss", "2026-11-19T12:00:00!", 20),
    ];
    for &(fmt, val, expected_pos) in cases {
        assert_eq!(
            dt_test_formatted_datetime(fmt, val)?,
            expected_pos,
            "{fmt} / {val}"
        );
    }

    assert_eq!(dt_locale_date("20261119", None)?, "2026-11-19");
    assert_eq!(
        dt_locale_date("20261119", Some("en-US.UTF-8"))?,
        "11/19/2026"
    );
    assert_eq!(dt_locale_date("20261119", Some("en_GB"))?, "19/11/2026");
    assert_eq!(dt_locale_date("20261119", Some("de_DE"))?, "19.11.2026");
    assert_eq!(dt_locale_date("20261119", Some("ja_JP"))?, "2026/11/19");
    assert!(dt_locale_date("2026", None).is_err());
    assert!(dt_locale_date("202611XX", None).is_err());
    assert!(dt_locale_date("20261301", None).is_err());
    assert!(dt_locale_date("20261119", Some("   ")).is_err());

    assert_eq!(dt_locale_time("001530", Some("en_US"))?, "12:15:30 AM");
    assert_eq!(dt_locale_time("091530", Some("en_US"))?, "09:15:30 AM");
    assert_eq!(dt_locale_time("121530", Some("en_US"))?, "12:15:30 PM");
    assert_eq!(dt_locale_time("151530", Some("en_US"))?, "03:15:30 PM");
    assert_eq!(dt_locale_time("151530", None)?, "15:15:30");
    assert!(dt_locale_time("123", None).is_err());
    assert!(dt_locale_time("12XX00", None).is_err());
    assert!(dt_locale_time("250000", None).is_err());
    assert!(dt_locale_time("126000", None).is_err());
    assert!(dt_locale_time("120061", None).is_err());

    assert_eq!(
        dt_locale_time_from_seconds(&dec("45000.9"), Some("C"))?,
        "12:30:00"
    );
    assert!(dt_locale_time_from_seconds(&dec("-1"), None).is_err());
    Ok(())
}

/// Verifies length, `CHAR`/`ORD`, case, `REVERSE`, `TRIM`, and `SUBSTITUTE*` intrinsics.
#[test]
fn strings_lengths_case_trim_and_substitute_coverage() -> Result<()> {
    assert_eq!(str_byte_length(b"COBOL"), 5);
    assert_eq!(str_length_an(b"COBOL"), 5);
    assert_eq!(str_length(MeasuredItem::Alphanumeric(b"ABC")), 3);
    assert_eq!(str_length(MeasuredItem::NationalUtf16(&[0, 65, 0, 66])), 2);
    assert_eq!(str_length(MeasuredItem::Utf8("A\u{00E9}\u{1F600}")), 3);
    assert_eq!(str_stored_char_length(b"HELLO   "), 5);
    assert_eq!(str_stored_char_length(b"   "), 0);

    assert_eq!(str_char(1, CollatingTable::NativeAscii)?, 0x00);
    assert_eq!(str_char(66, CollatingTable::NativeAscii)?, b'A');
    assert_eq!(str_char(256, CollatingTable::NativeAscii)?, 0xFF);
    assert!(str_char(0, CollatingTable::NativeAscii).is_err());
    assert!(str_char(257, CollatingTable::NativeAscii).is_err());
    assert_eq!(str_ord(b"A", CollatingTable::NativeAscii)?, 66);
    assert!(str_ord(b"", CollatingTable::NativeAscii).is_err());
    assert!(str_ord(b"AB", CollatingTable::NativeAscii).is_err());

    let mut custom = [b'X'; 256];
    custom[0] = b'Z';
    assert_eq!(str_char(1, CollatingTable::Custom(&custom))?, b'Z');
    assert_eq!(str_ord(b"Z", CollatingTable::Custom(&custom))?, 1);
    assert!(str_ord(b"A", CollatingTable::Custom(&custom)).is_err());

    assert_eq!(str_concat(&[b"CO", b"BOL"]), b"COBOL");
    assert_eq!(str_concatenate(&[b"A", b"", b"B"]), b"AB");
    assert_eq!(
        str_reverse("A\u{00E9}B".as_bytes()),
        "B\u{00E9}A".as_bytes()
    );
    assert_eq!(str_reverse(&[0xFF, 0x80]), vec![0x80, 0xFF]);
    assert_eq!(
        str_upper_case("ab\u{00E9}".as_bytes()),
        "AB\u{00C9}".as_bytes()
    );
    assert_eq!(str_upper_case(&[b'a', 0xFF]), vec![b'A', 0xFF]);
    assert_eq!(
        str_lower_case("AB\u{00C9}".as_bytes()),
        "ab\u{00E9}".as_bytes()
    );
    assert_eq!(str_lower_case(&[b'A', 0xFF]), vec![b'a', 0xFF]);

    assert_eq!(str_trim(b"  hi  ", TrimDirection::Both, None), b"hi");
    assert_eq!(
        str_trim(b"**hi**", TrimDirection::Leading, Some(b'*')),
        b"hi**"
    );
    assert_eq!(
        str_trim(b"**hi**", TrimDirection::Trailing, Some(b'*')),
        b"**hi"
    );

    assert_eq!(
        str_substitute(b"ab_cd", &[(b"a", b"b"), (b"b", b"c")])?,
        b"bc_cd"
    );
    assert_eq!(
        str_substitute_case(b"FooBar", &[(b"foo", b"X"), (b"BAR", b"Y")])?,
        b"XY"
    );
    assert_eq!(str_substitute(b"x", &[(b"longer", b"y")])?, b"x");
    assert!(str_substitute(b"abc", &[]).is_err());
    assert!(str_substitute(b"abc", &[(b"", b"x")]).is_err());
    Ok(())
}

/// Verifies ISO 2023 `FIND-STRING` modes (`FIRST`, `LAST`, `Occurrence`, `ANY`, `ALL`, `INSENSITIVE`).
#[test]
fn strings_find_string_modes_coverage() -> Result<()> {
    let def = FindStringSpec::first_sensitive();
    assert_eq!(str_find_string(b"ababa", b"aba", def)?, 1);
    assert_eq!(str_find_string(b"ababa", b"xyz", def)?, 0);
    let last_ins = FindStringSpec {
        mode: FindStringMode::Last,
        case_mode: CaseSensitivity::Insensitive,
        after_pos: 0,
    };
    assert_eq!(str_find_string(b"ababa", b"AB", last_ins)?, 3);
    assert_eq!(
        str_find_string(
            b"ab",
            b"long",
            FindStringSpec {
                after_pos: 1,
                ..last_ins
            }
        )?,
        0
    );
    assert_eq!(str_find_string(b"xyz", b"ab", last_ins)?, 0);
    assert_eq!(
        str_find_string(
            b"aaaa",
            b"aa",
            FindStringSpec {
                mode: FindStringMode::Occurrence(2),
                ..def
            },
        )?,
        3
    );
    assert_eq!(
        str_find_string(
            b"aaaa",
            b"aa",
            FindStringSpec {
                mode: FindStringMode::AllCount,
                ..def
            },
        )?,
        2
    );
    assert_eq!(
        str_find_string(
            b"hello",
            b"OE",
            FindStringSpec {
                mode: FindStringMode::AnyChar,
                case_mode: CaseSensitivity::Insensitive,
                after_pos: 0,
            },
        )?,
        2
    );
    assert_eq!(
        str_find_string(
            b"hello",
            b"z",
            FindStringSpec {
                mode: FindStringMode::AnyChar,
                ..def
            },
        )?,
        0
    );
    assert!(str_find_string(b"abc", b"", def).is_err());
    assert!(
        str_find_string(
            b"abc",
            b"a",
            FindStringSpec {
                after_pos: 4,
                ..def
            }
        )
        .is_err()
    );
    assert!(
        str_find_string(
            b"abc",
            b"a",
            FindStringSpec {
                mode: FindStringMode::Occurrence(0),
                ..def
            },
        )
        .is_err()
    );
    Ok(())
}

/// Verifies `NUMVAL`, `NUMVAL-C`, `NUMVAL-F`, and `TEST-NUMVAL*` valid and invalid grammars.
#[test]
fn strings_numval_family_valid_and_error_coverage() -> Result<()> {
    assert_eq!(str_test_numval(b"   ", DecimalPointKind::Period), 4);
    assert_eq!(str_test_numval(b" + 12.50 ", DecimalPointKind::Period), 0);
    assert_eq!(
        str_numval(b" - 12,50 ", DecimalPointKind::Comma)?,
        dec("-12.50")
    );
    assert_eq!(
        str_numval(b" 12.50 CR ", DecimalPointKind::Period)?,
        dec("-12.50")
    );
    assert_eq!(
        str_numval(b" 12.50 DB ", DecimalPointKind::Period)?,
        dec("-12.50")
    );
    assert_eq!(
        str_numval(b" 12.50 + ", DecimalPointKind::Period)?,
        dec("12.50")
    );
    assert_eq!(
        str_numval(b" 12.50 - ", DecimalPointKind::Period)?,
        dec("-12.50")
    );
    assert_eq!(
        str_numval(b" 0.00 - ", DecimalPointKind::Period)?,
        dec("0.00")
    );
    for bad in [
        &b"."[..],
        b"12 C",
        b"12 CX",
        b"12 D",
        b"12 DX",
        b"12 Z",
        b"+12-",
        b"12.34.56",
    ] {
        assert!(str_test_numval(bad, DecimalPointKind::Period) > 0);
        assert!(str_numval(bad, DecimalPointKind::Period).is_err());
    }
    let frac_77 = format!("0.{}", "1".repeat(77));
    assert!(str_numval(frac_77.as_bytes(), DecimalPointKind::Period).is_err());
    let digits_40 = "9".repeat(40);
    assert!(str_numval(digits_40.as_bytes(), DecimalPointKind::Period).is_err());
    let digits_80 = "9".repeat(80);
    assert!(str_numval(digits_80.as_bytes(), DecimalPointKind::Period).is_err());
    Ok(())
}

/// Verifies `NUMVAL-C` and `TEST-NUMVAL-C` monetary parsing, `ANYCASE`, and currency validation.
#[test]
fn strings_numval_c_coverage() -> Result<()> {
    assert_eq!(
        str_test_numval_c(
            b"   ",
            None,
            CurrencyCasePolicy::ExactCase,
            DecimalPointKind::Period
        )?,
        4
    );
    assert_eq!(
        str_test_numval_c(
            b" + eur 1.234,56 ",
            Some(b" EUR "),
            CurrencyCasePolicy::AnyCase,
            DecimalPointKind::Comma,
        )?,
        0
    );
    assert_eq!(
        str_numval_c(
            b" $ 1,234.56 cr ",
            None,
            CurrencyCasePolicy::AnyCase,
            DecimalPointKind::Period,
        )?,
        dec("-1234.56")
    );
    assert_eq!(
        str_numval_c(
            b" - $1,234.56 ",
            None,
            CurrencyCasePolicy::ExactCase,
            DecimalPointKind::Period,
        )?,
        dec("-1234.56")
    );
    assert_eq!(
        str_numval_c(
            b"5",
            Some(b"EURO"),
            CurrencyCasePolicy::ExactCase,
            DecimalPointKind::Period,
        )?,
        dec("5")
    );
    for bad_c in [&b",123"[..], b"123,", b"123,.45", b"$123 XX", b"+$123+"] {
        assert!(
            str_test_numval_c(
                bad_c,
                None,
                CurrencyCasePolicy::ExactCase,
                DecimalPointKind::Period
            )? > 0
        );
        assert!(
            str_numval_c(
                bad_c,
                None,
                CurrencyCasePolicy::ExactCase,
                DecimalPointKind::Period
            )
            .is_err()
        );
    }
    assert!(
        str_numval_c(
            b"10",
            Some(b"   "),
            CurrencyCasePolicy::ExactCase,
            DecimalPointKind::Period
        )
        .is_err()
    );
    assert!(
        str_test_numval_c(
            b"10",
            Some(b"$1"),
            CurrencyCasePolicy::ExactCase,
            DecimalPointKind::Period,
        )
        .is_err()
    );
    Ok(())
}

/// Verifies `NUMVAL-F` and `TEST-NUMVAL-F` floating-point parsing and exponent bounds.
#[test]
fn strings_numval_f_coverage() -> Result<()> {
    assert_eq!(str_test_numval_f(b"   ", DecimalPointKind::Period), 4);
    assert_eq!(
        str_numval_f(b" 12.5 ", DecimalPointKind::Period)?,
        dec("12.5")
    );
    assert!(
        str_numval_f(b" 1.5E+2 ", DecimalPointKind::Period)?
            .cmp_numeric(&dec("150"))
            .is_eq()
    );
    assert_eq!(str_test_numval_f(b"1.1 -", DecimalPointKind::Period), 0);
    assert_eq!(str_test_numval_f(b"+1.1 E001", DecimalPointKind::Period), 7);
    assert_eq!(
        str_test_numval_f(b" -1.25 E +2 ", DecimalPointKind::Period),
        0
    );
    assert_eq!(
        str_numval_f(b" -1.25 E +2 ", DecimalPointKind::Period)?,
        dec("-125")
    );
    assert_eq!(
        str_numval_f(b" +250e-2 ", DecimalPointKind::Period)?,
        dec("2.50")
    );
    for bad_f in [
        &b"1.0X"[..],
        b"1.0E",
        b"1.0E",
        b"1.0E+12345",
        b"1.0E+1X",
        b"1.0E+999",
    ] {
        assert!(str_test_numval_f(bad_f, DecimalPointKind::Period) > 0);
        assert!(str_numval_f(bad_f, DecimalPointKind::Period).is_err());
    }
    Ok(())
}

/// Verifies `BASECONVERT`, `BOOLEAN-OF-INTEGER`, `INTEGER-OF-BOOLEAN`, `BIT-*`, and `HEX-*`.
#[test]
fn strings_base_boolean_bit_and_hex_coverage() -> Result<()> {
    assert!(NumericBase::try_new(1).is_err());
    assert!(NumericBase::try_new(37).is_err());
    let b2 = NumericBase::try_new(2)?;
    let b10 = NumericBase::try_new(10)?;
    let b16 = NumericBase::try_new(16)?;
    let b36 = NumericBase::try_new(36)?;
    assert_eq!(str_baseconvert(b"0", b10, b16)?, b"0");
    assert_eq!(str_baseconvert(b"+255", b10, b16)?, b"FF");
    assert_eq!(str_baseconvert(b" -ff ", b16, b10)?, b"-255");
    assert_eq!(str_baseconvert(b"Z", b36, b2)?, b"100011");
    for bad_bc in [&b"   "[..], b"+", b"1$0", b"102"] {
        assert!(str_baseconvert(bad_bc, b2, b10).is_err());
    }
    let over_mul = vec![b'Z'; 60];
    assert!(str_baseconvert(&over_mul, b36, b10).is_err());
    let max_str = Uint256::MAX.to_string();
    let max_plus_1 = format!("{}9", &max_str[..max_str.len() - 1]);
    assert!(str_baseconvert(max_plus_1.as_bytes(), b10, b16).is_err());

    assert_eq!(str_boolean_of_integer(&dec("5"), 4)?, b"0101");
    assert_eq!(str_integer_of_boolean(b"0101")?, dec("5"));
    assert!(str_boolean_of_integer(&dec("5"), 0).is_err());
    assert!(str_boolean_of_integer(&dec("-1"), 4).is_err());
    assert!(str_boolean_of_integer(&dec("1.5"), 4).is_err());
    assert!(str_integer_of_boolean(b"").is_err());
    assert!(str_integer_of_boolean(&vec![b'0'; 257]).is_err());
    assert!(str_integer_of_boolean(b"012").is_err());

    assert_eq!(str_bit_of(b"A"), b"01000001");
    assert_eq!(str_bit_to_char(b"01000001")?, b"A");
    assert!(str_bit_to_char(b"").is_err());
    assert!(str_bit_to_char(b"0101").is_err());
    assert!(str_bit_to_char(b"0100000X").is_err());

    assert_eq!(str_hex_of(b"\x0F\xA5"), b"0FA5");
    assert_eq!(str_hex_to_char(b"0Fa5")?, b"\x0F\xA5");
    assert!(str_hex_to_char(b"").is_err());
    assert!(str_hex_to_char(b"0F0").is_err());
    assert!(str_hex_to_char(b"0G").is_err());
    Ok(())
}

/// Verifies ISO 2023 `CONVERT`, `STANDARD-COMPARE`, and `LOCALE-COMPARE`.
#[test]
fn strings_convert_and_collation_compare_coverage() -> Result<()> {
    assert_eq!(
        str_convert(
            b"ab-c",
            ConvertSpec::Alphabet {
                from_chars: b"abc",
                to_chars: b"ABC",
            },
        )?,
        b"AB-C"
    );
    assert!(
        str_convert(
            b"a",
            ConvertSpec::Alphabet {
                from_chars: b"ab",
                to_chars: b"A",
            },
        )
        .is_err()
    );
    for plain in [&b"M"[..], b"Ma", b"Man", b"\xFB\xFF\x00Hello"] {
        let b64 = str_convert(
            plain,
            ConvertSpec::Format {
                source: ConvertFormat::Alphanumeric,
                target: ConvertFormat::Base64,
            },
        )?;
        let round = str_convert(
            &b64,
            ConvertSpec::Format {
                source: ConvertFormat::Base64,
                target: ConvertFormat::Alphanumeric,
            },
        )?;
        assert_eq!(round, plain);
    }
    let hex_lo = str_convert(
        b"\xAB",
        ConvertSpec::Format {
            source: ConvertFormat::Alphanumeric,
            target: ConvertFormat::HexLower,
        },
    )?;
    let bits = str_convert(
        &hex_lo,
        ConvertSpec::Format {
            source: ConvertFormat::HexLower,
            target: ConvertFormat::BitString,
        },
    )?;
    assert_eq!(
        str_convert(
            &bits,
            ConvertSpec::Format {
                source: ConvertFormat::BitString,
                target: ConvertFormat::HexUpper,
            },
        )?,
        b"AB"
    );
    for bad_b64 in [
        &b"ABC"[..],
        b"!BCD",
        b"AB=D",
        b"AA==AAAA",
        b"AB$=AAAA",
        b"AAA=AAAA",
        b"AAA$",
    ] {
        assert!(
            str_convert(
                bad_b64,
                ConvertSpec::Format {
                    source: ConvertFormat::Base64,
                    target: ConvertFormat::Alphanumeric,
                },
            )
            .is_err()
        );
    }
    Ok(())
}

/// Verifies `STANDARD-COMPARE` and `LOCALE-COMPARE` cultural and custom collation ordering.
#[test]
fn strings_collation_compare_coverage() -> Result<()> {
    assert_eq!(CompareIndicator::Less.as_byte(), b'<');
    assert_eq!(CompareIndicator::Equal.as_byte(), b'=');
    assert_eq!(CompareIndicator::Greater.as_byte(), b'>');
    assert_eq!(CompareIndicator::Less.as_str(), "<");
    assert_eq!(CompareIndicator::Equal.as_str(), "=");
    assert_eq!(CompareIndicator::Greater.as_str(), ">");

    assert_eq!(
        str_standard_compare(b"abc", b"ABC", CollatingTable::NativeAscii, Some(1))?,
        CompareIndicator::Equal
    );
    assert_eq!(
        str_standard_compare(b"abc", b"ABC", CollatingTable::NativeAscii, Some(3))?,
        CompareIndicator::Less
    );
    assert_eq!(
        str_standard_compare(b"z", b"a", CollatingTable::NativeAscii, None)?,
        CompareIndicator::Greater
    );
    let mut rev_ab = [0_u8; 256];
    rev_ab[0] = b'b';
    rev_ab[1] = b'a';
    assert_eq!(
        str_standard_compare(b"b", b"z", CollatingTable::Custom(&rev_ab), Some(4))?,
        CompareIndicator::Less
    );
    assert!(str_standard_compare(b"a", b"b", CollatingTable::NativeAscii, Some(0)).is_err());

    assert_eq!(
        str_locale_compare(b"a", b"A", Some("C"))?,
        CompareIndicator::Greater
    );
    assert_eq!(
        str_locale_compare(b"a", b"A", Some("POSIX"))?,
        CompareIndicator::Greater
    );
    assert_eq!(
        str_locale_compare(b"a", b"A", None)?,
        CompareIndicator::Less
    );
    assert_eq!(
        str_locale_compare(b"a", b"b", Some("en_US"))?,
        CompareIndicator::Less
    );
    assert!(str_locale_compare(b"a", b"b", Some("  ")).is_err());
    Ok(())
}

/// Verifies `CHAR-NATIONAL`, `DISPLAY-OF`, `NATIONAL-OF`, and ISO 2023 `U*` Unicode intrinsics.
#[test]
fn meta_national_and_unicode_intrinsics_coverage() -> Result<()> {
    assert_eq!(
        meta_char_national(66, NationalByteOrder::BigEndian)?,
        vec![0x00, 0x41]
    );
    assert_eq!(
        meta_char_national(66, NationalByteOrder::LittleEndian)?,
        vec![0x41, 0x00]
    );
    assert_eq!(
        meta_char_national(0x1F600 + 1, NationalByteOrder::BigEndian)?,
        vec![0xD8, 0x3D, 0xDE, 0x00]
    );
    for bad_ord in [i64::MIN, 0, 0xD801, 0x11_0001] {
        assert!(meta_char_national(bad_ord, NationalByteOrder::BigEndian).is_err());
    }

    let nat_be = meta_national_of(
        b"HI",
        Codepage::Iso8859_1,
        NationalByteOrder::BigEndian,
        UnmappedCharPolicy::StrictError,
    )?;
    assert_eq!(nat_be, vec![0x00, b'H', 0x00, b'I']);
    assert_eq!(
        meta_display_of(
            &nat_be,
            NationalByteOrder::BigEndian,
            Codepage::Iso8859_1,
            UnmappedCharPolicy::StrictError,
        )?,
        b"HI"
    );
    assert!(
        meta_national_of(
            &[0xFF],
            Codepage::Utf8,
            NationalByteOrder::LittleEndian,
            UnmappedCharPolicy::StrictError,
        )
        .is_err()
    );
    assert!(
        meta_display_of(
            &[0x20, 0xAC],
            NationalByteOrder::BigEndian,
            Codepage::Iso8859_1,
            UnmappedCharPolicy::StrictError,
        )
        .is_err()
    );
    Ok(())
}

/// Verifies ISO 2023 `ULENGTH`, `UPOS`, `USUBSTR`, `USUPPLEMENTARY`, `UVALID`, and `UWIDTH`.
#[test]
fn meta_unicode_u_family_coverage() -> Result<()> {
    let sample = "A\u{00E9}\u{1F600}B";
    let u8_in = UnicodeSlice::Utf8(sample.as_bytes());
    let big_vec = meta_national_of(
        sample.as_bytes(),
        Codepage::Utf8,
        NationalByteOrder::BigEndian,
        UnmappedCharPolicy::StrictError,
    )?;
    let little_vec = meta_national_of(
        sample.as_bytes(),
        Codepage::Utf8,
        NationalByteOrder::LittleEndian,
        UnmappedCharPolicy::StrictError,
    )?;
    let big_in = UnicodeSlice::Utf16Be(&big_vec);
    let little_in = UnicodeSlice::Utf16Le(&little_vec);

    assert_eq!(meta_uvalid(u8_in), 0);
    assert_eq!(meta_uvalid(big_in), 0);
    assert_eq!(meta_uvalid(little_in), 0);
    assert_eq!(meta_ulength(u8_in)?, 4);
    assert_eq!(meta_ulength(big_in)?, 4);
    assert_eq!(meta_ulength(little_in)?, 4);
    assert_eq!(meta_upos(u8_in, 3)?, 4);
    assert_eq!(meta_upos(big_in, 4)?, 5);
    assert_eq!(meta_uwidth(u8_in, 3)?, 4);
    assert_eq!(meta_uwidth(little_in, 3)?, 4);
    assert_eq!(meta_usupplementary(u8_in)?, 4);
    assert_eq!(meta_usupplementary(big_in)?, 3);
    assert_eq!(meta_usupplementary(UnicodeSlice::Utf8(b"AB"))?, 0);

    assert_eq!(meta_usubstr(u8_in, 2, 2)?, "\u{00E9}\u{1F600}".as_bytes());
    assert_eq!(meta_usubstr(big_in, 1, 1)?, vec![0x00, b'A']);
    assert_eq!(meta_usubstr(little_in, 5, 0)?, Vec::<u8>::new());

    assert!(meta_upos(u8_in, 0).is_err());
    assert!(meta_upos(u8_in, 5).is_err());
    assert!(meta_uwidth(u8_in, 0).is_err());
    assert!(meta_uwidth(u8_in, 5).is_err());
    assert!(meta_usubstr(u8_in, 0, 1).is_err());
    assert!(meta_usubstr(u8_in, 6, 0).is_err());
    assert!(meta_usubstr(u8_in, 3, 3).is_err());

    let bad_u8 = UnicodeSlice::Utf8(&[b'A', 0xFF]);
    assert_eq!(meta_uvalid(bad_u8), 2);
    assert!(meta_ulength(bad_u8).is_err());
    assert!(meta_upos(bad_u8, 1).is_err());
    assert!(meta_usubstr(bad_u8, 1, 1).is_err());
    assert!(meta_usupplementary(bad_u8).is_err());
    assert!(meta_uwidth(bad_u8, 1).is_err());

    for bad_16 in [
        &[0x00_u8][..],
        &[0xDC, 0x00],
        &[0xD8, 0x00],
        &[0xD8, 0x00, 0x00, 0x41],
    ] {
        assert_eq!(meta_uvalid(UnicodeSlice::Utf16Be(bad_16)), 1);
    }
    Ok(())
}

/// Verifies `CONTENT-OF`, `CURRENCY-SYMBOL`, `MODULE-*`, `EXCEPTION-*`, and `UUID4`.
#[test]
fn meta_reflection_exceptions_and_uuid4_coverage() -> Result<()> {
    assert_eq!(meta_content_of(Some(b"HELLO\0WORLD"), None)?, b"HELLO");
    assert_eq!(meta_content_of(Some(b"HELLO"), None)?, b"HELLO");
    assert_eq!(meta_content_of(Some(b"HELLO"), Some(3))?, b"HEL");
    assert!(meta_content_of(None, None).is_err());
    assert!(meta_content_of(Some(b"HI"), Some(5)).is_err());

    assert_eq!(meta_currency_symbol("")?, "$");
    assert_eq!(meta_currency_symbol("EUR")?, "EUR");
    assert!(meta_currency_symbol("\x07").is_err());
    assert!(meta_currency_symbol("\x7F").is_err());

    let mod_info = ModuleReflectionInfo {
        module_id: String::from("PAYROLL"),
        caller_id: String::from("MAIN"),
        compile_date_yyyymmdd: *b"20261119",
        compile_time_hhmmss: *b"143000",
        formatted_compile_date: String::from("2026-11-19T14:30:00Z"),
        module_path: String::from("/bin/payroll"),
        module_source: String::from("payroll.cob"),
    };
    assert_eq!(meta_module_id(&mod_info), "PAYROLL");
    assert_eq!(meta_module_name(&mod_info), "PAYROLL");
    assert_eq!(meta_module_caller_id(&mod_info), "MAIN");
    assert_eq!(&meta_module_date(&mod_info), b"20261119");
    assert_eq!(&meta_module_time(&mod_info), b"143000");
    assert_eq!(
        meta_module_formatted_date(&mod_info),
        "2026-11-19T14:30:00Z"
    );
    assert_eq!(meta_module_path(&mod_info), "/bin/payroll");
    assert_eq!(meta_module_source(&mod_info), "payroll.cob");

    let cleared = ExceptionReflectionState::default();
    assert_eq!(meta_exception_file(&cleared), "00");
    assert_eq!(meta_exception_location(&cleared), " ");
    assert_eq!(
        meta_exception_status(&cleared),
        [b' '; EXCEPTION_REGISTER_WIDTH]
    );
    assert_eq!(
        meta_exception_statement(&cleared),
        [b' '; EXCEPTION_REGISTER_WIDTH]
    );

    let active = ExceptionReflectionState {
        status_name: Some(String::from("ec-io-at-end")),
        statement_verb: Some(String::from("read")),
        location: Some(String::from("PAYROLL;P1;88")),
        io_status: *b"10",
        file_name: String::from("IN-FILE"),
    };
    assert_eq!(meta_exception_file(&active), "10IN-FILE");
    assert_eq!(
        meta_exception_file_n(&active, NationalByteOrder::BigEndian).len(),
        18
    );
    assert_eq!(meta_exception_location(&active), "PAYROLL;P1;88");
    assert_eq!(
        meta_exception_location_n(&active, NationalByteOrder::LittleEndian).len(),
        26
    );
    let empty_loc = ExceptionReflectionState {
        location: Some(String::new()),
        ..cleared
    };
    assert_eq!(meta_exception_location(&empty_loc), " ");
    assert!(meta_exception_status(&active).starts_with(b"EC-IO-AT-END "));
    assert!(meta_exception_statement(&active).starts_with(b"READ "));

    let mut g_zero = Uuid4Generator::from_seeds(0, 0);
    let mut g_cancel = Uuid4Generator::from_seeds(0xA076_1D64_78BD_642F, 1);
    let mut g_sys = Uuid4Generator::from_system_clock();
    for u in [
        meta_uuid4(&mut g_zero),
        meta_uuid4(&mut g_cancel),
        meta_uuid4(&mut g_sys),
        meta_uuid4_from_entropy([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ]),
    ] {
        assert_eq!(u.len(), 36);
        assert_eq!((u[8], u[13], u[18], u[23]), (b'-', b'-', b'-', b'-'));
        assert_eq!(u[14], b'4');
        assert!(matches!(u[19], b'8' | b'9' | b'a' | b'b'));
    }
    Ok(())
}

/// Covers high-precision fixed-point transcendental, statistical, ISO-8601, and `NUMVAL` branches.
#[test]
fn high_precision_math_stats_datetime_and_numval_branch_coverage() -> Result<()> {
    let half = FixedDecimal::parse_str("0.5")?;
    let neg_half = FixedDecimal::parse_str("-0.5")?;
    let neg_two = FixedDecimal::from_i64(-2);
    let huge_sq = FixedDecimal::parse_str("100000000000000000000000000000000000000")?;
    let near_pi_2 = FixedDecimal::parse_str("1.57079632679489661923132169163975144209")?;

    assert!(math_sin(&half).to_f64() > 0.47);
    assert!(
        math_cos(&FixedDecimal::parse_str("-0.2345")?)
            .to_string()
            .starts_with("0.97263064125625818471341696241456141")
    );
    assert!(
        math_asin(&FixedDecimal::parse_str("-0.2345")?)?
            .to_string()
            .starts_with("-0.23670419431334681587017874688345882")
    );
    assert!(
        stats_annuity(&FixedDecimal::from_i64(3), &FixedDecimal::from_i64(5))?
            .to_string()
            .starts_with("3.00293255131964809384164222873900293")
    );
    assert!(math_sin(&huge_sq).to_f64().abs() <= 1.0);
    let ten_pow_40 = FixedDecimal::parse_str("10000000000000000000000000000000000000000")?;
    assert!(math_sin(&ten_pow_40).to_f64().abs() <= 1.0);
    assert!(math_cos(&half).to_f64() > 0.87);
    assert!(math_tan(&half).to_f64() > 0.54);
    assert!(math_tan(&near_pi_2).to_f64().abs() > 1.0e35);
    assert!(math_atan(&FixedDecimal::ZERO).is_zero());
    assert!(math_atan(&neg_two).is_negative());
    assert!(math_asin(&half)?.to_f64() > 0.52);
    assert!(math_asin(&FixedDecimal::from_i64(-1))?.is_negative());
    assert!(math_acos(&neg_half)?.to_f64() > 2.09);
    assert!(math_sqrt(&FixedDecimal::from_i64(1000))?.to_f64() > 31.6);
    assert!(math_sqrt(&huge_sq)?.to_f64() > 1.0e18);
    assert!(math_exp(&FixedDecimal::from_i64(-95))?.is_zero());
    assert!(math_exp(&FixedDecimal::parse_str("0.1")?)?.to_f64() > 1.1);
    assert!(math_exp(&FixedDecimal::from_i64(-5))?.to_f64() < 0.01);
    assert!(math_exp(&FixedDecimal::from_i64(120))?.to_f64() > 1.0e50);
    assert!(math_log(&FixedDecimal::parse_str("3.5")?)?.to_f64() > 1.25);

    // ANNUITY large/tiny rates + VARIANCE small/medium/huge magnitudes.
    let ann_big = stats_annuity(&FixedDecimal::from_i64(10), &FixedDecimal::from_i64(100))?;
    assert!(ann_big.to_f64() > 9.9);
    let tiny_rate = FixedDecimal::parse_str("0.0000000000000000000000000000000000000001")?;
    let ann_tiny = stats_annuity(&tiny_rate, &FixedDecimal::from_i64(2))?;
    assert!((ann_tiny.to_f64() - 0.5).abs() < 1.0e-6);
    let small_vals = [half, FixedDecimal::parse_str("1.5")?];
    assert!(stats_variance(&small_vals)?.to_f64() > 0.24);
    let med_vals = [FixedDecimal::from_i64(100), FixedDecimal::from_i64(200)];
    assert!(
        stats_variance(&med_vals)?
            .cmp_numeric(&FixedDecimal::from_i64(2500))
            .is_eq()
    );
    let mid_high_vals = [
        FixedDecimal::from_i64(100_000_000),
        FixedDecimal::from_i64(300_000_000),
    ];
    assert!(stats_variance(&mid_high_vals)?.to_f64() > 1.0e15);
    let big_vals = [
        FixedDecimal::parse_str("1000000000000000000")?,
        FixedDecimal::parse_str("3000000000000000000")?,
    ];
    assert!(stats_variance(&big_vals)?.to_f64() > 1.0e35);

    // ISO-8601 trailing spaces and '0'-prefixed timezone offsets.
    Ok(())
}

/// Covers ISO-8601 trailing-space / `'0'`-offset checks and `NUMVAL` / `NUMVAL-C` / `NUMVAL-F` branches.
#[test]
fn iso8601_and_numval_branch_coverage() -> Result<()> {
    let dt_fmt = "YYYY-MM-DDThh:mm:ss+hh:mm";
    assert_eq!(
        dt_test_formatted_datetime(dt_fmt, "2026-11-19T12:00:00+00:00   ")?,
        0
    );
    assert_eq!(
        dt_test_formatted_datetime(dt_fmt, "2026-11-19T12:00:00000:00")?,
        0
    );
    assert_eq!(
        dt_test_formatted_datetime(dt_fmt, "2026-11-19T12:00:00000:01")?,
        25
    );

    // NUMVAL / NUMVAL-C / NUMVAL-F branches.
    assert_eq!(
        str_numval(b"123", DecimalPointKind::Period)?,
        FixedDecimal::from_i64(123)
    );
    let over38 = b"9".repeat(39);
    assert!(str_numval(&over38, DecimalPointKind::Period).is_err());
    assert!(
        str_numval_c(
            &[b"$", over38.as_slice()].concat(),
            Some(b"$"),
            CurrencyCasePolicy::ExactCase,
            DecimalPointKind::Period,
        )
        .is_err()
    );
    assert_eq!(
        str_numval_c(
            b"% -9876.12",
            Some(b"%"),
            CurrencyCasePolicy::ExactCase,
            DecimalPointKind::Period,
        )?,
        FixedDecimal::parse_str("-9876.12")?
    );
    assert_eq!(
        str_numval_c(
            b"% +9876.12",
            Some(b"%"),
            CurrencyCasePolicy::ExactCase,
            DecimalPointKind::Period,
        )?,
        FixedDecimal::parse_str("9876.12")?
    );
    assert_eq!(str_test_numval_f(b"1.1 +", DecimalPointKind::Period), 0);
    assert_eq!(str_test_numval_f(b"+1.1 E001", DecimalPointKind::Period), 7);
    assert_eq!(str_test_numval(b"123 X", DecimalPointKind::Period), 5);
    assert_eq!(str_test_numval_f(b"1.0E+", DecimalPointKind::Period), 6);
    let over78_float = [b"9".repeat(79).as_slice(), b"E+1"].concat();
    assert!(str_numval_f(&over78_float, DecimalPointKind::Period).is_err());
    Ok(())
}
