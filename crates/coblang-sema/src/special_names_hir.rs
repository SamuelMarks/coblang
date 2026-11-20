//! `SPECIAL-NAMES` `ALPHABET` collation rank tables, `CLASS` 256-bit bitmaps, and `SWITCH-0..36` resolution.

use coblang_common::SymbolId;
use coblang_diag::{DefaultColSeqMode, SymbolInterner};
use coblang_hir::{HirAlphabetTable, HirCustomClass, HirSwitchSpec};
use coblang_syntax::ast::{
    AlphabetDecl, AlphabetKind, ClassDecl, EnvironmentDivision, SpecialNamesAtom, SwitchDecl,
};

/// Returns a fresh 256-byte ASCII-to-CP037 EBCDIC collation rank table.
#[must_use]
pub(crate) fn ebcdic_collation_ranks() -> Vec<u8> {
    ASCII_TO_CP037.to_vec()
}

/// ASCII-to-EBCDIC (IBM CP037 / US) byte mapping used for `ALPHABET ... IS EBCDIC` collation ordering.
const ASCII_TO_CP037: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x37, 0x2D, 0x2E, 0x2F, 0x16, 0x05, 0x25, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x3C, 0x3D, 0x32, 0x26, 0x18, 0x19, 0x3F, 0x27, 0x1C, 0x1D, 0x1E, 0x1F,
    0x40, 0x5A, 0x7F, 0x7B, 0x5B, 0x6C, 0x50, 0x7D, 0x4D, 0x5D, 0x5C, 0x4E, 0x6B, 0x60, 0x4B, 0x61,
    0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0x7A, 0x5E, 0x4C, 0x7E, 0x6E, 0x6F,
    0x7C, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6,
    0xD7, 0xD8, 0xD9, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xBA, 0xE0, 0xBB, 0xB0, 0x6D,
    0x79, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96,
    0x97, 0x98, 0x99, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xC0, 0x4F, 0xD0, 0xA1, 0x07,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x15, 0x06, 0x17, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x09, 0x0A, 0x1B,
    0x30, 0x31, 0x1A, 0x33, 0x34, 0x35, 0x36, 0x08, 0x38, 0x39, 0x3A, 0x3B, 0x04, 0x14, 0x3E, 0xFF,
    0x41, 0xAA, 0x4A, 0xB1, 0x9F, 0xB2, 0x6A, 0xB5, 0xBD, 0xB4, 0x9A, 0x8A, 0x5F, 0xCA, 0xAF, 0xBC,
    0x90, 0x8F, 0xEA, 0xFA, 0xBE, 0xA0, 0xB6, 0xB3, 0x9D, 0xDA, 0x9B, 0x8B, 0xB7, 0xB8, 0xB9, 0xAB,
    0x64, 0x65, 0x62, 0x66, 0x63, 0x67, 0x9E, 0x68, 0x74, 0x71, 0x72, 0x73, 0x78, 0x75, 0x76, 0x77,
    0xAC, 0x69, 0xED, 0xEE, 0xEB, 0xEF, 0xEC, 0xBF, 0x80, 0xFD, 0xFE, 0xFB, 0xFC, 0xAD, 0xAE, 0x59,
    0x44, 0x45, 0x42, 0x46, 0x43, 0x47, 0x9C, 0x48, 0x54, 0x51, 0x52, 0x53, 0x58, 0x55, 0x56, 0x57,
    0x8C, 0x49, 0xCD, 0xCE, 0xCB, 0xCF, 0xCC, 0xE1, 0x70, 0xDD, 0xDE, 0xDB, 0xDC, 0x8D, 0x8E, 0xDF,
];

/// Lowered `ENVIRONMENT DIVISION` `SPECIAL-NAMES` & `OBJECT-COMPUTER` tables for a `HirProgram`.
#[derive(Debug, Default)]
pub struct LoweredSpecialNames {
    /// Compiled 256-byte `ALPHABET` rank tables.
    pub alphabets: Vec<HirAlphabetTable>,
    /// Active `PROGRAM COLLATING SEQUENCE` index into `alphabets` when non-identity.
    pub collating_sequence: Option<usize>,
    /// Compiled 256-bit `CLASS` membership bitmaps.
    pub custom_classes: Vec<HirCustomClass>,
    /// Compiled `SWITCH-0..36` definitions.
    pub switches: Vec<HirSwitchSpec>,
}

/// Compiles all `ALPHABET`, `PROGRAM COLLATING SEQUENCE`, `CLASS`, and `SWITCH` declarations in `env`.
///
/// # Arguments
///
/// * `env` - AST `ENVIRONMENT DIVISION`.
/// * `default_colseq` - Dialect default collating sequence (`-fdefault-colseq=`).
/// * `interner` - Symbol interner.
#[must_use]
pub fn build_special_names_tables(
    env: &EnvironmentDivision,
    default_colseq: DefaultColSeqMode,
    interner: &SymbolInterner,
) -> LoweredSpecialNames {
    let mut cfg = coblang_diag::DialectConfig::for_dialect(coblang_common::Dialect::Gnu);
    cfg.default_colseq = default_colseq;
    build_special_names_tables_with_dialect(env, &cfg, interner)
}

/// Compiles `SPECIAL-NAMES` `ALPHABET`, `CLASS`, and `SWITCH` tables honoring `dialect`
/// (`internal_charset` and `-febcdic-table=` translation tables).
///
/// # Arguments
///
/// * `env` - AST `ENVIRONMENT DIVISION`.
/// * `dialect` - Active [`coblang_diag::DialectConfig`].
/// * `interner` - Symbol interner.
#[must_use]
pub fn build_special_names_tables_with_dialect(
    env: &EnvironmentDivision,
    dialect: &coblang_diag::DialectConfig,
    interner: &SymbolInterner,
) -> LoweredSpecialNames {
    let sn = &env.configuration.special_names;
    let mut alphabets: Vec<HirAlphabetTable> = sn
        .alphabets
        .iter()
        .map(|decl| compile_alphabet_with_dialect(decl, dialect, interner))
        .collect();

    let mut collating_sequence = env
        .configuration
        .program_collating_sequence
        .and_then(|alpha_sym| alphabets.iter().position(|a| a.name == alpha_sym))
        .filter(|&idx| {
            alphabets
                .get(idx)
                .is_some_and(|a| !is_identity_rank_table(&a.ranks))
        });

    if collating_sequence.is_none()
        && dialect.default_colseq == DefaultColSeqMode::Ebcdic
        && dialect.internal_charset == coblang_diag::InternalCharsetMode::Ascii
    {
        let idx = alphabets.len();
        alphabets.push(HirAlphabetTable {
            name: SymbolId::new(u32::MAX),
            ranks: dialect.ebcdic_tables.ascii_to_ebcdic.to_vec(),
        });
        collating_sequence = Some(idx);
    }

    let custom_classes: Vec<HirCustomClass> = sn
        .classes
        .iter()
        .map(|cls| compile_custom_class(cls, &alphabets, interner))
        .collect();

    let switches: Vec<HirSwitchSpec> = sn
        .switches
        .iter()
        .map(|sw| compile_switch_decl(sw, interner))
        .collect();

    LoweredSpecialNames {
        alphabets,
        collating_sequence,
        custom_classes,
        switches,
    }
}

/// Returns `true` when `ranks` is the trivial ASCII identity permutation `0..=255`.
///
/// # Arguments
///
/// * `ranks` - 256-entry collation rank slice.
#[must_use]
fn is_identity_rank_table(ranks: &[u8]) -> bool {
    ranks.iter().enumerate().all(|(i, &r)| usize::from(r) == i)
}

/// Compiles a single `ALPHABET` AST declaration into a 256-entry [`HirAlphabetTable`] honoring `dialect`.
///
/// # Arguments
///
/// * `decl` - AST `ALPHABET` declaration.
/// * `dialect` - Active dialect configuration.
/// * `interner` - Symbol interner.
#[must_use]
fn compile_alphabet_with_dialect(
    decl: &AlphabetDecl,
    dialect: &coblang_diag::DialectConfig,
    interner: &SymbolInterner,
) -> HirAlphabetTable {
    let ranks = if let Some(builtin) =
        crate::m17_charset::compile_builtin_alphabet_ranks(&decl.kind, dialect)
    {
        builtin
    } else if let AlphabetKind::Custom(specs) = &decl.kind {
        compile_custom_alphabet_specs(specs, interner)
    } else {
        ASCII_TO_CP037.to_vec()
    };
    HirAlphabetTable {
        name: decl.name,
        ranks,
    }
}

/// Compiles a custom `ALPHABET name IS ...` token list (`THRU` / `ALSO` / literals / ordinals) into 256 ranks.
///
/// # Arguments
///
/// * `specs` - Ordered specification symbol tokens.
/// * `interner` - Symbol interner.
#[must_use]
fn compile_custom_alphabet_specs(specs: &[SpecialNamesAtom], interner: &SymbolInterner) -> Vec<u8> {
    let mut assigned = [false; 256];
    let mut ranks = [0_u8; 256];
    let mut next_rank: u8 = 0;
    let mut last_rank: u8 = 0;
    let mut idx = 0_usize;

    while let Some(atom) = specs.get(idx) {
        let next_kw = specs.get(idx + 1).and_then(|a| atom_keyword(a, interner));

        if matches!(next_kw, Some("THRU" | "THROUGH"))
            && let Some(hi_atom) = specs.get(idx + 2)
        {
            let lo = resolve_spec_single_byte(atom, interner);
            let hi = resolve_spec_single_byte(hi_atom, interner);
            assign_byte_range(
                lo,
                hi,
                &mut assigned,
                &mut ranks,
                &mut next_rank,
                &mut last_rank,
            );
            idx += 3;
            continue;
        }

        let bytes = resolve_spec_bytes(atom, interner);
        for &b in &bytes {
            assign_one_byte(
                b,
                next_rank,
                &mut assigned,
                &mut ranks,
                &mut next_rank,
                &mut last_rank,
            );
        }
        idx += 1;

        while specs.get(idx).and_then(|a| atom_keyword(a, interner)) == Some("ALSO")
            && let Some(also_atom) = specs.get(idx + 1)
        {
            let tie_bytes = resolve_spec_bytes(also_atom, interner);
            for &tb in &tie_bytes {
                let _ = mark_rank_if_unassigned(tb, last_rank, &mut assigned, &mut ranks);
            }
            idx += 2;
        }
    }

    for byte_val in 0_u8..=255_u8 {
        assign_one_byte(
            byte_val,
            next_rank,
            &mut assigned,
            &mut ranks,
            &mut next_rank,
            &mut last_rank,
        );
    }

    ranks.to_vec()
}

/// Returns the canonical uppercase keyword if `atom` is [`SpecialNamesAtom::Word`].
///
/// # Arguments
///
/// * `atom` - Candidate `SPECIAL-NAMES` atom.
/// * `interner` - Symbol interner.
#[must_use]
fn atom_keyword<'a>(atom: &SpecialNamesAtom, interner: &'a SymbolInterner) -> Option<&'a str> {
    match atom {
        SpecialNamesAtom::Word(sym) => interner.resolve_canonical(*sym).ok(),
        SpecialNamesAtom::StringBytes(_) | SpecialNamesAtom::OrdinalOneBased(_) => None,
    }
}

/// Marks `byte_val` with `rank_val` when unassigned, returning `true` if newly assigned.
///
/// # Arguments
///
/// * `byte_val` - Target byte (`0..=255`).
/// * `rank_val` - Rank to store.
/// * `assigned` - Assigned flags table.
/// * `ranks` - Output rank table.
fn mark_rank_if_unassigned(
    byte_val: u8,
    rank_val: u8,
    assigned: &mut [bool; 256],
    ranks: &mut [u8; 256],
) -> bool {
    let slot = usize::from(byte_val);
    let mut dummy_flag = true;
    let flag = assigned.get_mut(slot).unwrap_or(&mut dummy_flag);
    if *flag {
        return false;
    }
    *flag = true;
    let mut dummy_rk = 0_u8;
    *ranks.get_mut(slot).unwrap_or(&mut dummy_rk) = rank_val;
    true
}

/// Assigns `rank_val` to `byte_val` if not yet assigned, advancing `next_rank`.
///
/// # Arguments
///
/// * `byte_val` - Target byte (`0..=255`).
/// * `rank_val` - Rank to assign.
/// * `assigned` - Assigned flags table.
/// * `ranks` - Output rank table.
/// * `next_rank` - Monotonic rank counter.
/// * `last_rank` - Most recently assigned rank (used by `ALSO`).
fn assign_one_byte(
    byte_val: u8,
    rank_val: u8,
    assigned: &mut [bool; 256],
    ranks: &mut [u8; 256],
    next_rank: &mut u8,
    last_rank: &mut u8,
) {
    if mark_rank_if_unassigned(byte_val, rank_val, assigned, ranks) {
        *last_rank = rank_val;
        *next_rank = next_rank.saturating_add(1);
    }
}

/// Assigns consecutive collation ranks across an inclusive `lo..=hi` or reverse `lo..=hi` byte range.
///
/// # Arguments
///
/// * `lo` - Range starting byte.
/// * `hi` - Range ending byte.
/// * `assigned` - Assigned flags table.
/// * `ranks` - Output rank table.
/// * `next_rank` - Monotonic rank counter.
/// * `last_rank` - Most recently assigned rank.
fn assign_byte_range(
    lo: u8,
    hi: u8,
    assigned: &mut [bool; 256],
    ranks: &mut [u8; 256],
    next_rank: &mut u8,
    last_rank: &mut u8,
) {
    if lo <= hi {
        for b in lo..=hi {
            assign_one_byte(b, *next_rank, assigned, ranks, next_rank, last_rank);
        }
    } else {
        let mut cur = lo;
        loop {
            assign_one_byte(cur, *next_rank, assigned, ranks, next_rank, last_rank);
            if cur == hi {
                break;
            }
            cur -= 1;
        }
    }
}

/// Resolves a single `ALPHABET` or `CLASS` atom into one or more character bytes.
///
/// # Arguments
///
/// * `atom` - Strongly-typed `SPECIAL-NAMES` atom.
/// * `interner` - Symbol interner.
#[must_use]
fn resolve_spec_bytes(atom: &SpecialNamesAtom, interner: &SymbolInterner) -> Vec<u8> {
    match atom {
        SpecialNamesAtom::StringBytes(bytes) => bytes.clone(),
        SpecialNamesAtom::OrdinalOneBased(sym) => {
            let orig = interner.resolve_original(*sym).unwrap_or("");
            let ord = orig.parse::<u16>().unwrap_or(1).clamp(1, 256);
            vec![u8::try_from(ord - 1).unwrap_or(0)]
        }
        SpecialNamesAtom::Word(sym) => {
            let orig = interner.resolve_original(*sym).unwrap_or("");
            match orig.to_ascii_uppercase().as_str() {
                "SPACE" | "SPACES" => vec![b' '],
                "ZERO" | "ZEROS" | "ZEROES" => vec![b'0'],
                "QUOTE" | "QUOTES" => vec![b'"'],
                "LOW-VALUE" | "LOW-VALUES" => vec![0x00],
                "HIGH-VALUE" | "HIGH-VALUES" => vec![0xFF],
                _ => orig.as_bytes().to_vec(),
            }
        }
    }
}

/// Resolves a range endpoint atom to a single byte (`0x00` fallback when empty).
///
/// # Arguments
///
/// * `atom` - Endpoint atom.
/// * `interner` - Symbol interner.
#[must_use]
fn resolve_spec_single_byte(atom: &SpecialNamesAtom, interner: &SymbolInterner) -> u8 {
    resolve_spec_bytes(atom, interner)
        .first()
        .copied()
        .unwrap_or(0)
}

/// Compiles a `SPECIAL-NAMES. CLASS` declaration into a 256-bit [`HirCustomClass`] bitmap.
///
/// # Arguments
///
/// * `cls` - AST `CLASS` declaration.
/// * `alphabets` - Previously compiled `ALPHABET` tables in the same unit.
/// * `interner` - Symbol interner.
#[must_use]
fn compile_custom_class(
    cls: &ClassDecl,
    alphabets: &[HirAlphabetTable],
    interner: &SymbolInterner,
) -> HirCustomClass {
    let mut bitmap = [0_u8; 32];
    let alpha_ranks = cls
        .alphabet
        .and_then(|asym| alphabets.iter().find(|a| a.name == asym))
        .map(|a| a.ranks.as_slice());

    for range in &cls.ranges {
        if let Some(hi_atom) = &range.high {
            let lo = resolve_spec_single_byte(&range.low, interner);
            let hi = resolve_spec_single_byte(hi_atom, interner);
            if let Some(ranks) = alpha_ranks {
                let r_lo = ranks.get(usize::from(lo)).copied().unwrap_or(lo);
                let r_hi = ranks.get(usize::from(hi)).copied().unwrap_or(hi);
                let (min_r, max_r) = if r_lo <= r_hi {
                    (r_lo, r_hi)
                } else {
                    (r_hi, r_lo)
                };
                for (bu8, &rk) in (0_u8..=255_u8).zip(ranks.iter()) {
                    if (min_r..=max_r).contains(&rk) {
                        set_bitmap_bit(&mut bitmap, bu8);
                    }
                }
            } else {
                let (min_b, max_b) = if lo <= hi { (lo, hi) } else { (hi, lo) };
                for b in min_b..=max_b {
                    set_bitmap_bit(&mut bitmap, b);
                }
            }
        } else {
            for b in resolve_spec_bytes(&range.low, interner) {
                set_bitmap_bit(&mut bitmap, b);
            }
        }
    }

    HirCustomClass {
        name: cls.name,
        bitmap,
    }
}

/// Sets bit `byte_val` inside a 32-byte (`256`-bit) class membership bitmap.
///
/// # Arguments
///
/// * `bitmap` - Mutable 32-byte bitmap.
/// * `byte_val` - Character byte to mark present.
fn set_bitmap_bit(bitmap: &mut [u8; 32], byte_val: u8) {
    let idx = usize::from(byte_val >> 3);
    let mask = 1_u8 << (byte_val & 7);
    let mut dummy = 0_u8;
    *bitmap.get_mut(idx).unwrap_or(&mut dummy) |= mask;
}

/// Compiles a `SPECIAL-NAMES. SWITCH-n` / `UPSI-n` declaration into [`HirSwitchSpec`].
///
/// # Arguments
///
/// * `sw` - AST `SwitchDecl`.
/// * `interner` - Symbol interner.
#[must_use]
fn compile_switch_decl(sw: &SwitchDecl, interner: &SymbolInterner) -> HirSwitchSpec {
    let sys = interner.resolve_canonical(sw.system_name).unwrap_or("");
    let index = parse_switch_index(sys);
    HirSwitchSpec {
        index,
        mnemonic: sw.mnemonic,
        on_cond: sw.on_condition,
        off_cond: sw.off_condition,
    }
}

/// Maps a system switch name (`SWITCH-0`..`SWITCH-36`, `SWITCH-A`..`SWITCH-Z`, `UPSI-0`..`UPSI-7`) to `0..=36`.
///
/// # Arguments
///
/// * `sys` - Canonical uppercase system switch identifier.
#[must_use]
pub fn parse_switch_index(sys: &str) -> u8 {
    let tail = sys
        .strip_prefix("SWITCH-")
        .or_else(|| sys.strip_prefix("SWITCH"))
        .or_else(|| sys.strip_prefix("UPSI-"))
        .or_else(|| sys.strip_prefix("UPSI"))
        .unwrap_or(sys)
        .trim();
    if let Ok(num) = tail.parse::<u8>() {
        return num.min(36);
    }
    if let &[ch @ b'A'..=b'Z'] = tail.as_bytes() {
        return (ch - b'A' + 1).min(36);
    }
    0
}
