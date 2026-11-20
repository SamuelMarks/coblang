//! Single-byte 256-entry collating sequence (`ALPHABET`) rank tables for COBOL `SORT` and `MERGE`.

use coblang_common::{CoblangError, Result};

/// Identity 256-byte native ASCII / ISO-8859-1 collation rank table (`0x00..=0xFF`).
const NATIVE_ASCII_RANKS: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
    0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F,
    0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
    0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F,
    0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F,
    0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
    0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
    0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF,
    0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF,
    0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF,
    0xD0, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF,
    0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF,
    0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF,
];

/// ASCII-to-EBCDIC (`IBM CP037`) collation weight table (`SPACE` < `'a'..'z'` < `'A'..'Z'` < `'0'..'9'`).
const ASCII_TO_CP037_RANKS: [u8; 256] = [
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

/// Declarative clause inside a custom COBOL `SPECIAL-NAMES` `ALPHABET name IS ...` definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlphabetSpecEntry {
    /// Assigns the next sequential collation weight to a single byte value.
    Byte(u8),
    /// Assigns sequential collation weights across `first..=last` (ascending or descending `THRU`).
    Range {
        /// Starting byte of the `THRU` range.
        first: u8,
        /// Ending byte of the `THRU` range (inclusive).
        last: u8,
    },
    /// Assigns the same collation weight to all unassigned bytes in the `ALSO` tie group.
    EqualGroup(Vec<u8>),
}

/// 256-entry byte-to-ordinal-rank lookup table governing COBOL `COLLATING SEQUENCE` comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlphabetTable {
    /// Ordinal collation weight (`0..=255`) assigned to each raw byte value `0..=255`.
    ranks: [u8; 256],
}

impl Default for AlphabetTable {
    /// Returns [`Self::native_ascii`] as the default collating sequence.
    fn default() -> Self {
        Self::native_ascii()
    }
}

impl AlphabetTable {
    /// Returns the host native ASCII / ISO-8859-1 identity collation table (`rank(b) == b`).
    #[must_use]
    pub const fn native_ascii() -> Self {
        Self {
            ranks: NATIVE_ASCII_RANKS,
        }
    }

    /// Returns the IBM `CP037` `EBCDIC` collation table mapping ASCII bytes to EBCDIC weights.
    #[must_use]
    pub const fn ebcdic_cp037() -> Self {
        Self {
            ranks: ASCII_TO_CP037_RANKS,
        }
    }

    /// Wraps a precomputed 256-byte rank array.
    ///
    /// # Arguments
    ///
    /// * `ranks` - Ordinal rank assigned to each byte `0..=255`.
    #[must_use]
    pub const fn from_ranks(ranks: [u8; 256]) -> Self {
        Self { ranks }
    }

    /// Constructs an [`AlphabetTable`] from a 256-byte slice (such as `HirAlphabetTable::ranks`).
    ///
    /// # Arguments
    ///
    /// * `ranks` - Candidate rank slice, which must contain exactly 256 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `ranks.len() != 256`.
    pub fn try_from_slice(ranks: &[u8]) -> Result<Self> {
        let arr: [u8; 256] = ranks.try_into().map_err(|_| CoblangError::SortMerge {
            message: format!(
                "expected 256-byte ALPHABET rank table, found {} bytes",
                ranks.len()
            ),
        })?;
        Ok(Self { ranks: arr })
    }

    /// Compiles a custom COBOL `SPECIAL-NAMES. ALPHABET` specification (`THRU`, `ALSO`, literals)
    /// into a 256-entry [`AlphabetTable`], assigning remaining unmentioned bytes sequentially.
    ///
    /// # Arguments
    ///
    /// * `entries` - Ordered [`AlphabetSpecEntry`] clauses from `ALPHABET name IS ...`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if any [`AlphabetSpecEntry::EqualGroup`] is empty.
    pub fn from_entries(entries: &[AlphabetSpecEntry]) -> Result<Self> {
        let mut assigned_bits = AssignedByteBitmap::new();
        let mut ranks = [0_u8; 256];
        let mut next_rank: u8 = 0;

        for entry in entries {
            match entry {
                AlphabetSpecEntry::Byte(b) => {
                    assign_sequential_byte(*b, &mut assigned_bits, &mut ranks, &mut next_rank);
                }
                AlphabetSpecEntry::Range { first, last } => {
                    if first <= last {
                        for b in *first..=*last {
                            assign_sequential_byte(
                                b,
                                &mut assigned_bits,
                                &mut ranks,
                                &mut next_rank,
                            );
                        }
                    } else {
                        for b in (*last..=*first).rev() {
                            assign_sequential_byte(
                                b,
                                &mut assigned_bits,
                                &mut ranks,
                                &mut next_rank,
                            );
                        }
                    }
                }
                AlphabetSpecEntry::EqualGroup(group) => {
                    if group.is_empty() {
                        return Err(CoblangError::SortMerge {
                            message: String::from(
                                "ALPHABET EqualGroup (ALSO clause) must contain at least one byte",
                            ),
                        });
                    }
                    let shared_rank = next_rank;
                    let mut any_assigned = false;
                    for &b in group {
                        if assigned_bits.mark(b) {
                            set_rank_slot(&mut ranks, b, shared_rank);
                            any_assigned = true;
                        }
                    }
                    if any_assigned {
                        next_rank = next_rank.saturating_add(1);
                    }
                }
            }
        }

        for b in 0_u8..=255_u8 {
            assign_sequential_byte(b, &mut assigned_bits, &mut ranks, &mut next_rank);
        }

        Ok(Self { ranks })
    }

    /// Looks up the ordinal collation rank (`0..=255`) for `byte`.
    ///
    /// # Arguments
    ///
    /// * `byte` - Raw input byte value.
    #[must_use]
    pub fn rank_of(&self, byte: u8) -> u8 {
        self.ranks.get(usize::from(byte)).copied().unwrap_or(byte)
    }

    /// Returns a reference to the underlying 256-byte rank table.
    #[must_use]
    pub const fn as_ranks(&self) -> &[u8; 256] {
        &self.ranks
    }

    /// Returns `true` when every byte `0..=255` maps to its own native ordinal value.
    #[must_use]
    pub fn is_native_identity(&self) -> bool {
        self.ranks == NATIVE_ASCII_RANKS
    }
}

/// 256-bit bitmap tracking which byte values (`0..=255`) have already received an `ALPHABET` rank.
#[derive(Default)]
struct AssignedByteBitmap {
    /// Membership bits for bytes `0..=127`.
    lo: u128,
    /// Membership bits for bytes `128..=255`.
    hi: u128,
}

impl AssignedByteBitmap {
    /// Creates an empty 256-bit bitmap with all bytes unassigned.
    const fn new() -> Self {
        Self { lo: 0, hi: 0 }
    }

    /// Marks `byte` as assigned, returning `true` if `byte` was previously unassigned.
    ///
    /// # Arguments
    ///
    /// * `byte` - Target byte value in `0..=255`.
    fn mark(&mut self, byte: u8) -> bool {
        if byte < 128 {
            let bit = 1_u128 << byte;
            let was_unset = (self.lo & bit) == 0;
            self.lo |= bit;
            was_unset
        } else {
            let bit = 1_u128 << (byte - 128);
            let was_unset = (self.hi & bit) == 0;
            self.hi |= bit;
            was_unset
        }
    }
}

/// Stores `rank` into `ranks` at index `byte` without unchecked slice indexing.
///
/// # Arguments
///
/// * `ranks` - Mutable 256-byte rank array.
/// * `byte` - Target byte slot (`0..=255`).
/// * `rank` - Collation weight to store.
fn set_rank_slot(ranks: &mut [u8; 256], byte: u8, rank: u8) {
    let _ = ranks
        .get_mut(usize::from(byte))
        .map(|slot| core::mem::replace(slot, rank));
}

/// Assigns `next_rank` to `byte` if unassigned and increments `next_rank`.
///
/// # Arguments
///
/// * `byte` - Candidate byte value in `0..=255`.
/// * `assigned` - 256-bit membership tracker.
/// * `ranks` - Mutable 256-byte rank table.
/// * `next_rank` - Monotonic rank counter (`0..=255`).
fn assign_sequential_byte(
    byte: u8,
    assigned: &mut AssignedByteBitmap,
    ranks: &mut [u8; 256],
    next_rank: &mut u8,
) {
    if assigned.mark(byte) {
        set_rank_slot(ranks, byte, *next_rank);
        *next_rank = next_rank.saturating_add(1);
    }
}
