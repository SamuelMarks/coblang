//! Multi-key COBOL `SORT` and `MERGE` record comparator supporting custom `ALPHABET`
//! collation, algebraic decimal/binary/float keys, and `WITH DUPLICATES IN ORDER`.

use core::cmp::Ordering;

use coblang_common::{CoblangError, Result};
use coblang_decimal::{
    BinaryLayout, Decimal64Bid, Decimal128Bid, FixedDecimal, FloatBinary128, PackedLayout,
    RoundingMode, ZonedLayout, decode_binary, decode_comp1_ne, decode_comp2_ne, decode_packed,
    decode_zoned,
};

use crate::alphabet::AlphabetTable;

/// Key ordering direction in `ON {ASCENDING | DESCENDING} KEY` clauses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyDirection {
    /// Smaller key values precede larger key values (`ASCENDING KEY`).
    Ascending,
    /// Larger key values precede smaller key values (`DESCENDING KEY`).
    Descending,
}

impl KeyDirection {
    /// Returns the opposite [`KeyDirection`].
    #[must_use]
    pub const fn reverse(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }

    /// Applies `self` to a raw ascending [`Ordering`], reversing it for [`Self::Descending`].
    ///
    /// # Arguments
    ///
    /// * `ord` - Raw ascending comparison result.
    #[must_use]
    pub const fn apply(self, ord: Ordering) -> Ordering {
        match self {
            Self::Ascending => ord,
            Self::Descending => ord.reverse(),
        }
    }
}

/// Duplicate-key stability policy (`WITH DUPLICATES IN ORDER` vs implementation-defined).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DuplicatesOrder {
    /// Duplicate-key relative ordering is unconstrained (`WITH DUPLICATES` omitted).
    #[default]
    Arbitrary,
    /// Equal-key records strictly preserve chronological input sequence (`WITH DUPLICATES IN ORDER`).
    InOrder,
}

impl DuplicatesOrder {
    /// Returns `true` when `self` is [`Self::InOrder`].
    #[must_use]
    pub const fn is_in_order(self) -> bool {
        matches!(self, Self::InOrder)
    }
}

/// Physical storage category and codec layout of an individual `SORT` / `MERGE` key field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortKeyType {
    /// Single-byte character, group, or edited field collated bytewise via [`AlphabetTable`].
    Alphanumeric {
        /// Space pad byte (`0x20` ASCII or `0x40` EBCDIC) supplied when a variable-length record is shorter than the key end.
        space_byte: u8,
    },
    /// `USAGE NATIONAL` (`UTF-16BE`) field compared by 16-bit big-endian code units (`0x0020` pad).
    NationalUtf16Be,
    /// `USAGE DISPLAY` zoned decimal field (`PIC [S]9...`) compared algebraically.
    Zoned(ZonedLayout),
    /// `PACKED-DECIMAL` (`COMP-3`) or unsigned `COMP-6` BCD field compared algebraically.
    Packed(PackedLayout),
    /// `BINARY` / `COMP` / `COMP-4` / `COMP-5` / `COMP-X` (`1..=16` bytes) compared algebraically.
    Binary(BinaryLayout),
    /// 4-byte native-endian IEEE 754 `COMP-1` (`f32`) compared algebraically.
    FloatShort,
    /// 8-byte native-endian IEEE 754 `COMP-2` (`f64`) compared algebraically.
    FloatLong,
    /// 16-byte native-endian IEEE 754 `FLOAT-EXTENDED` (`binary128`) compared algebraically.
    FloatExtended,
    /// 8-byte native-endian IEEE 754-2008 `FLOAT-DECIMAL-16` (`BID64`) compared algebraically.
    FloatDecimal16,
    /// 16-byte native-endian IEEE 754-2008 `FLOAT-DECIMAL-34` (`BID128`) compared algebraically.
    FloatDecimal34,
}

impl SortKeyType {
    /// Convenience constructor for ASCII-padded (`0x20`) alphanumeric/group keys.
    #[must_use]
    pub const fn alphanumeric_ascii() -> Self {
        Self::Alphanumeric { space_byte: b' ' }
    }

    /// Convenience constructor for EBCDIC-padded (`0x40`) alphanumeric/group keys.
    #[must_use]
    pub const fn alphanumeric_ebcdic() -> Self {
        Self::Alphanumeric { space_byte: 0x40 }
    }
}

/// Validated byte-range and codec descriptor for a single key in a `SORT` or `MERGE` statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SortKeySpec {
    /// Zero-based byte offset of the key within the `SD` / table record.
    offset: usize,
    /// Non-zero byte length of the key field.
    length: usize,
    /// `ASCENDING` vs `DESCENDING` direction for this key.
    direction: KeyDirection,
    /// Physical storage codec / collation category for this key.
    key_type: SortKeyType,
}

impl SortKeySpec {
    /// Validates and constructs a [`SortKeySpec`].
    ///
    /// # Arguments
    ///
    /// * `offset` - Zero-based byte offset within each record.
    /// * `length` - Key byte width (`>= 1`, matching the codec's exact width for numeric/national types).
    /// * `direction` - [`KeyDirection::Ascending`] or [`KeyDirection::Descending`].
    /// * `key_type` - [`SortKeyType`] governing comparison semantics.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `length == 0`, `offset + length` overflows `usize`,
    /// or `length` violates the storage width invariant of `key_type`.
    pub fn try_new(
        offset: usize,
        length: usize,
        direction: KeyDirection,
        key_type: SortKeyType,
    ) -> Result<Self> {
        if length == 0 {
            return Err(CoblangError::SortMerge {
                message: String::from("SORT/MERGE key byte length must be >= 1"),
            });
        }
        if offset.checked_add(length).is_none() {
            return Err(CoblangError::SortMerge {
                message: format!("SORT/MERGE key range {offset}+{length} overflows usize"),
            });
        }
        validate_key_type_length(length, key_type)?;
        Ok(Self {
            offset,
            length,
            direction,
            key_type,
        })
    }

    /// Returns the zero-based byte offset of this key within a record.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the validated non-zero byte length of this key.
    #[must_use]
    pub const fn length(&self) -> usize {
        self.length
    }

    /// Returns the [`KeyDirection`] (`Ascending` or `Descending`) of this key.
    #[must_use]
    pub const fn direction(&self) -> KeyDirection {
        self.direction
    }

    /// Returns the [`SortKeyType`] of this key.
    #[must_use]
    pub const fn key_type(&self) -> SortKeyType {
        self.key_type
    }
}

/// Multi-key record comparator for COBOL file `SORT`, `MERGE`, and table `SORT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortComparator {
    /// Ordered major-to-minor key specifications (`>= 1`).
    keys: Vec<SortKeySpec>,
    /// Active single-byte `COLLATING SEQUENCE` rank table.
    alphabet: AlphabetTable,
    /// Duplicate-key stability policy (`WITH DUPLICATES IN ORDER`).
    duplicates: DuplicatesOrder,
}

impl SortComparator {
    /// Constructs a validated multi-key [`SortComparator`].
    ///
    /// # Arguments
    ///
    /// * `keys` - Non-empty major-to-minor [`SortKeySpec`] vector.
    /// * `alphabet` - Active [`AlphabetTable`] for alphanumeric keys.
    /// * `duplicates` - [`DuplicatesOrder`] governing equal-key stability.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `keys` is empty.
    pub fn try_new(
        keys: Vec<SortKeySpec>,
        alphabet: AlphabetTable,
        duplicates: DuplicatesOrder,
    ) -> Result<Self> {
        if keys.is_empty() {
            return Err(CoblangError::SortMerge {
                message: String::from("SortComparator requires at least one SortKeySpec"),
            });
        }
        Ok(Self {
            keys,
            alphabet,
            duplicates,
        })
    }

    /// Returns the ordered major-to-minor [`SortKeySpec`] slice.
    #[must_use]
    pub fn keys(&self) -> &[SortKeySpec] {
        &self.keys
    }

    /// Returns the active [`AlphabetTable`].
    #[must_use]
    pub const fn alphabet(&self) -> &AlphabetTable {
        &self.alphabet
    }

    /// Returns the configured [`DuplicatesOrder`] policy.
    #[must_use]
    pub const fn duplicates_order(&self) -> DuplicatesOrder {
        self.duplicates
    }

    /// Compares `lhs` and `rhs` across all keys strictly, failing on truncated or malformed numeric bytes.
    ///
    /// # Arguments
    ///
    /// * `lhs` - Left record byte slice.
    /// * `rhs` - Right record byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if any numeric key slice is shorter than its declared
    /// length or contains invalid BCD/zoned/floating-point bytes.
    pub fn compare_checked(&self, lhs: &[u8], rhs: &[u8]) -> Result<Ordering> {
        for key in &self.keys {
            let raw_ord = compare_single_key_checked(lhs, rhs, key, &self.alphabet)?;
            let directed = key.direction.apply(raw_ord);
            if directed != Ordering::Equal {
                return Ok(directed);
            }
        }
        Ok(Ordering::Equal)
    }

    /// Compares `lhs` and `rhs` under a total preorder suitable for `slice::sort_by` and merge heaps.
    ///
    /// Valid numeric keys are compared algebraically; if a numeric slice is truncated or malformed,
    /// valid numeric keys sort before malformed slices and malformed slices are compared bytewise.
    ///
    /// # Arguments
    ///
    /// * `lhs` - Left record byte slice.
    /// * `rhs` - Right record byte slice.
    #[must_use]
    pub fn compare_records(&self, lhs: &[u8], rhs: &[u8]) -> Ordering {
        for key in &self.keys {
            let raw_ord = compare_single_key_total(lhs, rhs, key, &self.alphabet);
            let directed = key.direction.apply(raw_ord);
            if directed != Ordering::Equal {
                return directed;
            }
        }
        Ordering::Equal
    }

    /// Compares `lhs` and `rhs`, breaking key ties with `(lhs_seq, rhs_seq)` when [`DuplicatesOrder::InOrder`] is active.
    ///
    /// # Arguments
    ///
    /// * `lhs` - Left record byte slice.
    /// * `lhs_seq` - Zero-based arrival sequence number of `lhs`.
    /// * `rhs` - Right record byte slice.
    /// * `rhs_seq` - Zero-based arrival sequence number of `rhs`.
    #[must_use]
    pub fn compare_with_sequence(
        &self,
        lhs: &[u8],
        lhs_seq: u64,
        rhs: &[u8],
        rhs_seq: u64,
    ) -> Ordering {
        let key_ord = self.compare_records(lhs, rhs);
        if key_ord != Ordering::Equal {
            key_ord
        } else if self.duplicates.is_in_order() {
            lhs_seq.cmp(&rhs_seq)
        } else {
            Ordering::Equal
        }
    }

    /// Sorts `count` fixed-stride elements of length `stride` in place within `buf`.
    ///
    /// Uses stable drift-sort when [`DuplicatesOrder::InOrder`] is configured and pattern-defeating
    /// unstable sort when [`DuplicatesOrder::Arbitrary`] is configured.
    ///
    /// # Arguments
    ///
    /// * `buf` - Mutable contiguous table buffer starting at occurrence `1`.
    /// * `count` - Active occurrence count (`0..=max`).
    /// * `stride` - Byte stride per table element (`>= 1`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `stride == 0`, `count * stride` overflows, or `count * stride > buf.len()`.
    pub fn sort_fixed_stride_table(
        &self,
        buf: &mut [u8],
        count: usize,
        stride: usize,
    ) -> Result<()> {
        if stride == 0 {
            return Err(CoblangError::SortMerge {
                message: String::from("table SORT element stride must be >= 1"),
            });
        }
        let Some(required) = count.checked_mul(stride) else {
            return Err(CoblangError::SortMerge {
                message: format!("table SORT byte range {count} * {stride} overflows usize"),
            });
        };
        if required > buf.len() {
            return Err(CoblangError::SortMerge {
                message: format!(
                    "table SORT range 0..{required} exceeds buffer length {}",
                    buf.len()
                ),
            });
        }
        if count <= 1 {
            return Ok(());
        }

        let mut order: Vec<usize> = (0..count).collect();
        if self.duplicates.is_in_order() {
            order.sort_by(|&a_idx, &b_idx| {
                let a_elem = buf.get(a_idx * stride..(a_idx + 1) * stride).unwrap_or(&[]);
                let b_elem = buf.get(b_idx * stride..(b_idx + 1) * stride).unwrap_or(&[]);
                self.compare_records(a_elem, b_elem)
            });
        } else {
            order.sort_unstable_by(|&a_idx, &b_idx| {
                let a_elem = buf.get(a_idx * stride..(a_idx + 1) * stride).unwrap_or(&[]);
                let b_elem = buf.get(b_idx * stride..(b_idx + 1) * stride).unwrap_or(&[]);
                self.compare_records(a_elem, b_elem)
            });
        }

        let mut sorted = Vec::with_capacity(required);
        for idx in order {
            sorted.extend_from_slice(buf.get(idx * stride..(idx + 1) * stride).unwrap_or(&[]));
        }
        for (dst_byte, src_byte) in buf.iter_mut().zip(sorted) {
            *dst_byte = src_byte;
        }
        Ok(())
    }
}

/// Validates that `length` satisfies the storage invariants of `key_type`.
///
/// # Arguments
///
/// * `length` - Non-zero declared key byte width.
/// * `key_type` - Key codec variant.
fn validate_key_type_length(length: usize, key_type: SortKeyType) -> Result<()> {
    match key_type {
        SortKeyType::Alphanumeric { .. } => Ok(()),
        SortKeyType::NationalUtf16Be => {
            if !length.is_multiple_of(2) {
                return Err(CoblangError::SortMerge {
                    message: format!(
                        "NATIONAL UTF-16BE sort key length {length} must be a multiple of 2"
                    ),
                });
            }
            Ok(())
        }
        SortKeyType::Zoned(layout) => {
            let expected = layout.byte_len();
            if length != expected {
                return Err(CoblangError::SortMerge {
                    message: format!(
                        "zoned decimal sort key length {length} does not match layout width {expected}"
                    ),
                });
            }
            Ok(())
        }
        SortKeyType::Packed(layout) => {
            let expected = layout.byte_len();
            if length != expected {
                return Err(CoblangError::SortMerge {
                    message: format!(
                        "packed BCD sort key length {length} does not match layout width {expected}"
                    ),
                });
            }
            Ok(())
        }
        SortKeyType::Binary(layout) => {
            layout.validate().map_err(|err| CoblangError::SortMerge {
                message: err.to_string(),
            })?;
            let expected = usize::from(layout.byte_width);
            if length != expected {
                return Err(CoblangError::SortMerge {
                    message: format!(
                        "binary sort key length {length} does not match byte_width {expected}"
                    ),
                });
            }
            Ok(())
        }
        SortKeyType::FloatShort => require_exact_key_len(length, 4, "COMP-1 (FloatShort)"),
        SortKeyType::FloatLong => require_exact_key_len(length, 8, "COMP-2 (FloatLong)"),
        SortKeyType::FloatExtended => {
            require_exact_key_len(length, 16, "FLOAT-EXTENDED (FloatExtended)")
        }
        SortKeyType::FloatDecimal16 => {
            require_exact_key_len(length, 8, "FLOAT-DECIMAL-16 (FloatDecimal16)")
        }
        SortKeyType::FloatDecimal34 => {
            require_exact_key_len(length, 16, "FLOAT-DECIMAL-34 (FloatDecimal34)")
        }
    }
}

/// Checks `actual == expected` for fixed-width floating-point key variants.
///
/// # Arguments
///
/// * `actual` - Provided key length in bytes.
/// * `expected` - Required IEEE floating-point storage width (`4`, `8`, or `16`).
/// * `label` - Diagnostic type name.
fn require_exact_key_len(actual: usize, expected: usize, label: &str) -> Result<()> {
    if actual != expected {
        return Err(CoblangError::SortMerge {
            message: format!("{label} sort key requires {expected} bytes, found {actual}"),
        });
    }
    Ok(())
}

/// Internal classification separating character/national keys from exact-decimal numeric keys.
#[derive(Debug, Clone, Copy)]
enum KeyDispatchKind {
    /// Bytewise single-byte collated character key.
    Alphanumeric {
        /// Space pad byte for short variable-length records.
        space_byte: u8,
    },
    /// Big-endian 16-bit `NATIONAL` key.
    NationalUtf16Be,
    /// Numeric key compared algebraically via [`FixedDecimal`].
    Numeric(NumericKeyCodec),
}

/// Internal codec tag exclusively covering the 8 numeric [`SortKeyType`] variants.
#[derive(Debug, Clone, Copy)]
enum NumericKeyCodec {
    /// Zoned decimal (`USAGE DISPLAY`).
    Zoned(ZonedLayout),
    /// Packed BCD (`COMP-3` / `COMP-6`).
    Packed(PackedLayout),
    /// Binary integer (`1..=16` bytes).
    Binary(BinaryLayout),
    /// 4-byte `COMP-1`.
    FloatShort,
    /// 8-byte `COMP-2`.
    FloatLong,
    /// 16-byte `FLOAT-EXTENDED`.
    FloatExtended,
    /// 8-byte `FLOAT-DECIMAL-16`.
    FloatDecimal16,
    /// 16-byte `FLOAT-DECIMAL-34`.
    FloatDecimal34,
}

/// Classifies a public [`SortKeyType`] into [`KeyDispatchKind`].
///
/// # Arguments
///
/// * `key_type` - Public key type descriptor.
const fn classify_key_type(key_type: SortKeyType) -> KeyDispatchKind {
    match key_type {
        SortKeyType::Alphanumeric { space_byte } => KeyDispatchKind::Alphanumeric { space_byte },
        SortKeyType::NationalUtf16Be => KeyDispatchKind::NationalUtf16Be,
        SortKeyType::Zoned(layout) => KeyDispatchKind::Numeric(NumericKeyCodec::Zoned(layout)),
        SortKeyType::Packed(layout) => KeyDispatchKind::Numeric(NumericKeyCodec::Packed(layout)),
        SortKeyType::Binary(layout) => KeyDispatchKind::Numeric(NumericKeyCodec::Binary(layout)),
        SortKeyType::FloatShort => KeyDispatchKind::Numeric(NumericKeyCodec::FloatShort),
        SortKeyType::FloatLong => KeyDispatchKind::Numeric(NumericKeyCodec::FloatLong),
        SortKeyType::FloatExtended => KeyDispatchKind::Numeric(NumericKeyCodec::FloatExtended),
        SortKeyType::FloatDecimal16 => KeyDispatchKind::Numeric(NumericKeyCodec::FloatDecimal16),
        SortKeyType::FloatDecimal34 => KeyDispatchKind::Numeric(NumericKeyCodec::FloatDecimal34),
    }
}

/// Compares `lhs` and `rhs` on a single `key`, returning an error on truncated/invalid numeric fields.
///
/// # Arguments
///
/// * `lhs` - Left record slice.
/// * `rhs` - Right record slice.
/// * `key` - Validated key specification.
/// * `alphabet` - Collating sequence table.
fn compare_single_key_checked(
    lhs: &[u8],
    rhs: &[u8],
    key: &SortKeySpec,
    alphabet: &AlphabetTable,
) -> Result<Ordering> {
    match classify_key_type(key.key_type) {
        KeyDispatchKind::Alphanumeric { space_byte } => Ok(compare_alphanumeric_padded(
            lhs, rhs, key.offset, key.length, space_byte, alphabet,
        )),
        KeyDispatchKind::NationalUtf16Be => {
            Ok(compare_national_padded(lhs, rhs, key.offset, key.length))
        }
        KeyDispatchKind::Numeric(codec) => {
            let lhs_slice = slice_bounded(lhs, key.offset, key.length);
            let rhs_slice = slice_bounded(rhs, key.offset, key.length);
            let lv = decode_numeric_key_exact(lhs_slice, codec)?;
            let rv = decode_numeric_key_exact(rhs_slice, codec)?;
            Ok(lv.cmp_numeric(&rv))
        }
    }
}

/// Compares `lhs` and `rhs` on a single `key` under a total fallback order.
///
/// # Arguments
///
/// * `lhs` - Left record slice.
/// * `rhs` - Right record slice.
/// * `key` - Validated key specification.
/// * `alphabet` - Collating sequence table.
fn compare_single_key_total(
    lhs: &[u8],
    rhs: &[u8],
    key: &SortKeySpec,
    alphabet: &AlphabetTable,
) -> Ordering {
    match classify_key_type(key.key_type) {
        KeyDispatchKind::Alphanumeric { space_byte } => {
            compare_alphanumeric_padded(lhs, rhs, key.offset, key.length, space_byte, alphabet)
        }
        KeyDispatchKind::NationalUtf16Be => {
            compare_national_padded(lhs, rhs, key.offset, key.length)
        }
        KeyDispatchKind::Numeric(codec) => {
            let lhs_slice = slice_bounded(lhs, key.offset, key.length);
            let rhs_slice = slice_bounded(rhs, key.offset, key.length);
            match (
                decode_numeric_key_exact(lhs_slice, codec),
                decode_numeric_key_exact(rhs_slice, codec),
            ) {
                (Ok(lv), Ok(rv)) => lv.cmp_numeric(&rv),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => lhs_slice.cmp(rhs_slice),
            }
        }
    }
}

/// Compares two alphanumeric key ranges byte-by-byte through `alphabet`, padding short records with `space_byte`.
///
/// # Arguments
///
/// * `lhs` - Left record bytes.
/// * `rhs` - Right record bytes.
/// * `offset` - Key start offset.
/// * `length` - Key byte width.
/// * `space_byte` - Pad byte for variable-length records shorter than `offset + length`.
/// * `alphabet` - Active 256-entry [`AlphabetTable`].
fn compare_alphanumeric_padded(
    lhs: &[u8],
    rhs: &[u8],
    offset: usize,
    length: usize,
    space_byte: u8,
    alphabet: &AlphabetTable,
) -> Ordering {
    for pos in 0..length {
        let idx = offset + pos;
        let lb = lhs.get(idx).copied().unwrap_or(space_byte);
        let rb = rhs.get(idx).copied().unwrap_or(space_byte);
        let lw = alphabet.rank_of(lb);
        let rw = alphabet.rank_of(rb);
        if lw != rw {
            return lw.cmp(&rw);
        }
    }
    Ordering::Equal
}

/// Compares two `UTF-16BE` `NATIONAL` key ranges unit-by-unit, padding short records with `0x0020`.
///
/// # Arguments
///
/// * `lhs` - Left record bytes.
/// * `rhs` - Right record bytes.
/// * `offset` - Key start offset.
/// * `length` - Even key byte width (`2 * units`).
fn compare_national_padded(lhs: &[u8], rhs: &[u8], offset: usize, length: usize) -> Ordering {
    let units = length / 2;
    for unit_idx in 0..units {
        let base = offset + unit_idx * 2;
        let lu = read_utf16be_unit_or_space(lhs, base);
        let ru = read_utf16be_unit_or_space(rhs, base);
        if lu != ru {
            return lu.cmp(&ru);
        }
    }
    Ordering::Equal
}

/// Reads a big-endian 16-bit `NATIONAL` code unit at `base`, or `0x0020` when `record` ends earlier.
///
/// # Arguments
///
/// * `record` - Record byte slice.
/// * `base` - Byte offset of the 16-bit unit.
fn read_utf16be_unit_or_space(record: &[u8], base: usize) -> u16 {
    match (record.get(base), record.get(base + 1)) {
        (Some(&hi), Some(&lo)) => u16::from_be_bytes([hi, lo]),
        _ => 0x0020,
    }
}

/// Extracts `record[offset..offset + length]` clamped to `record.len()` without panicking.
///
/// # Arguments
///
/// * `record` - Source record byte slice.
/// * `offset` - Start byte offset.
/// * `length` - Requested byte width.
fn slice_bounded(record: &[u8], offset: usize, length: usize) -> &[u8] {
    let start = offset.min(record.len());
    let end = offset.saturating_add(length).min(record.len());
    record.get(start..end).unwrap_or(&[])
}

/// Decodes a numeric key slice into an exact [`FixedDecimal`] for algebraic comparison.
///
/// # Arguments
///
/// * `bytes` - Raw key slice extracted from a record.
/// * `codec` - [`NumericKeyCodec`] variant.
fn decode_numeric_key_exact(bytes: &[u8], codec: NumericKeyCodec) -> Result<FixedDecimal> {
    let decoded = match codec {
        NumericKeyCodec::Zoned(layout) => decode_zoned(bytes, layout),
        NumericKeyCodec::Packed(layout) => decode_packed(bytes, layout),
        NumericKeyCodec::Binary(layout) => decode_binary(bytes, layout),
        NumericKeyCodec::FloatShort => {
            let arr: [u8; 4] = bytes.try_into().map_err(|_| CoblangError::SortMerge {
                message: format!("expected 4 COMP-1 bytes, found {}", bytes.len()),
            })?;
            decode_comp1_ne(arr, 18, RoundingMode::NearestEven).map(|(dec, _)| dec)
        }
        NumericKeyCodec::FloatLong => {
            let arr: [u8; 8] = bytes.try_into().map_err(|_| CoblangError::SortMerge {
                message: format!("expected 8 COMP-2 bytes, found {}", bytes.len()),
            })?;
            decode_comp2_ne(arr, 18, RoundingMode::NearestEven).map(|(dec, _)| dec)
        }
        NumericKeyCodec::FloatExtended => {
            let arr: [u8; 16] = bytes.try_into().map_err(|_| CoblangError::SortMerge {
                message: format!("expected 16 FLOAT-EXTENDED bytes, found {}", bytes.len()),
            })?;
            let val = FloatBinary128::from_ne_bytes(arr).to_f64().map_err(|err| {
                CoblangError::SortMerge {
                    message: err.to_string(),
                }
            })?;
            FixedDecimal::from_f64(val, 18, RoundingMode::NearestEven).map(|(dec, _)| dec)
        }
        NumericKeyCodec::FloatDecimal16 => {
            let arr: [u8; 8] = bytes.try_into().map_err(|_| CoblangError::SortMerge {
                message: format!("expected 8 FLOAT-DECIMAL-16 bytes, found {}", bytes.len()),
            })?;
            Decimal64Bid::from_ne_bytes(arr).to_fixed_decimal()
        }
        NumericKeyCodec::FloatDecimal34 => {
            let arr: [u8; 16] = bytes.try_into().map_err(|_| CoblangError::SortMerge {
                message: format!("expected 16 FLOAT-DECIMAL-34 bytes, found {}", bytes.len()),
            })?;
            Decimal128Bid::from_ne_bytes(arr).to_fixed_decimal()
        }
    };
    decoded.map_err(|err| CoblangError::SortMerge {
        message: err.to_string(),
    })
}
