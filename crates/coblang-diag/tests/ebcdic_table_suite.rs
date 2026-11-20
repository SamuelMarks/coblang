//! Exhaustive unit tests for `coblang_diag::ebcdic_table` and `-finternal-ebcdic` / `ebcdic-table` `.conf` keys.

use std::fmt::Write as _;

use coblang_common::{Dialect, Result};
use coblang_diag::{
    DEFAULT_EBCDIC_TO_ASCII, DialectConfig, DisplaySignMode, EbcdicTranslationTables,
    InternalCharsetMode,
};

/// Verifies all built-in `-febcdic-table=` presets and bidirectional transcoding.
#[test]
fn builtin_ebcdic_presets_and_transcoding() {
    let def = EbcdicTranslationTables::default();
    assert_eq!(
        def.transcode_ascii_to_ebcdic(b"A0 |!"),
        [0xC1, 0xF0, 0x40, 0x6A, 0x5A]
    );
    assert_eq!(
        def.transcode_ebcdic_to_ascii(&[0xC1, 0xF0, 0x40, 0x6A, 0x5A]),
        b"A0 |!"
    );

    let from_a2e = EbcdicTranslationTables::from_ascii_to_ebcdic(def.ascii_to_ebcdic);
    assert_eq!(from_a2e.ebcdic_to_ascii[0x40], b' ');

    for name in ["default", "cp037.ttbl", "IBM1140"] {
        assert_eq!(EbcdicTranslationTables::from_builtin_name(name), Some(def));
    }
    let alt = EbcdicTranslationTables::from_builtin_name("alternate");
    assert!(alt.is_some_and(|t| t.ebcdic_to_ascii[0x4F] == b'|'));

    let cp500 = EbcdicTranslationTables::from_builtin_name("ebcdic500_ascii7bit");
    assert!(
        cp500.is_some_and(|t| t.ebcdic_to_ascii[0x5A] == b']' && t.ebcdic_to_ascii[0x6A] == b'|')
    );

    let cp1047 = EbcdicTranslationTables::from_builtin_name("cp1047");
    assert!(
        cp1047.is_some_and(|t| t.ebcdic_to_ascii[0xAD] == b'[' && t.ebcdic_to_ascii[0xBD] == b']')
    );

    let rgc = EbcdicTranslationTables::from_builtin_name("restricted-gc");
    assert!(
        rgc.is_some_and(|t| t.ebcdic_to_ascii[0x40] == b' ' && t.ebcdic_to_ascii[0x4A] == 0x4A)
    );

    assert!(EbcdicTranslationTables::from_builtin_name("no_such_table").is_none());
}

/// Verifies `.ttbl` parsing across 256-entry, 512-entry, comment, invalid-hex, and wrong-length inputs.
#[test]
fn parse_ttbl_all_branches() {
    // 256-entry ebcdic->ascii (0x40 -> 0x20)
    let mut valid_256 = String::from(
        "# comment header
",
    );
    for (i, b) in DEFAULT_EBCDIC_TO_ASCII.iter().enumerate() {
        let _ = write!(valid_256, "{b:02X} ");
        if i % 16 == 15 {
            valid_256.push(char::from(0x0A));
        }
    }
    let parsed_e2a = EbcdicTranslationTables::parse_ttbl_text(&valid_256, "valid.ttbl");
    assert!(parsed_e2a.is_ok());

    // 256-entry ascii->ebcdic (0x20 -> 0x40)
    let def = EbcdicTranslationTables::default();
    let mut valid_a2e = String::new();
    for b in def.ascii_to_ebcdic {
        let _ = write!(valid_a2e, "{b:02x} ");
    }
    let parsed_a2e = EbcdicTranslationTables::parse_ttbl_text(&valid_a2e, "a2e.ttbl");
    assert!(parsed_a2e.is_ok_and(|t| t.ebcdic_to_ascii[0x40] == b' '));

    // 512-entry both orders
    let both_1 = format!(
        "{valid_256}
{valid_a2e}"
    );
    assert!(EbcdicTranslationTables::parse_ttbl_text(&both_1, "512a.ttbl").is_ok());
    let both_2 = format!(
        "{valid_a2e}
{valid_256}"
    );
    assert!(EbcdicTranslationTables::parse_ttbl_text(&both_2, "512b.ttbl").is_ok());

    // Shorter (< 256)
    let err_short = EbcdicTranslationTables::parse_ttbl_text("00 01 02", "short.ttbl");
    assert!(err_short.is_err_and(|e| e.iter().any(|l| l.contains("not enough data"))));

    // Between 256 and 512
    let mid = format!("{valid_256} FF");
    let err_mid = EbcdicTranslationTables::parse_ttbl_text(&mid, "mid.ttbl");
    assert!(err_mid.is_err_and(|e| {
        e.iter()
            .any(|l| l.contains("either not enough or too much"))
    }));

    // Longer (> 512)
    let long = format!("{both_1} FF");
    let err_long = EbcdicTranslationTables::parse_ttbl_text(&long, "long.ttbl");
    assert!(err_long.is_err_and(|e| e.iter().any(|l| l.contains("too much data"))));

    // Invalid hex tokens (`2X`, `ABC`)
    let err_bad = EbcdicTranslationTables::parse_ttbl_text(
        "00 2X
ABC",
        "bad.ttbl",
    );
    assert!(err_bad.is_err_and(|e| {
        e.len() >= 4
            && e.first()
                .is_some_and(|s| s.contains("errors in translation table"))
    }));
}

/// Verifies `internal-ebcdic` and `ebcdic-table` `.conf` keys on [`DialectConfig`].
#[test]
fn dialect_conf_ebcdic_keys() -> Result<()> {
    let mut cfg = DialectConfig::for_dialect(Dialect::Gnu);
    assert_eq!(cfg.internal_charset, InternalCharsetMode::Ascii);

    cfg.apply_conf_key_value("internal-ebcdic", "yes", "test.conf", 1)?;
    assert_eq!(cfg.internal_charset, InternalCharsetMode::Ebcdic);
    assert_eq!(cfg.display_sign, DisplaySignMode::Ebcdic);

    cfg.apply_conf_key_value("internal-ebcdic", "no", "test.conf", 2)?;
    assert_eq!(cfg.internal_charset, InternalCharsetMode::Ascii);
    assert!(
        cfg.apply_conf_key_value("internal-ebcdic", "maybe", "test.conf", 3)
            .is_err()
    );

    cfg.apply_conf_key_value("ebcdic-table", "ebcdic500_ascii8bit", "test.conf", 4)?;
    assert_eq!(cfg.ebcdic_tables.ebcdic_to_ascii[0x5A], b']');
    assert!(
        cfg.apply_conf_key_value("ebcdic-table", "missing", "test.conf", 5)
            .is_err()
    );
    Ok(())
}
