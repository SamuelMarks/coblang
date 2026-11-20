//! Strongly-typed domain model for COBOL file organization, access modes, open/close dispositions,
//! record layouts, advancing clauses, `LINE SEQUENTIAL` framing, and 1-based relative slot keys.

use coblang_common::{CoblangError, DigitCount, Result};

/// ASCII NUL control byte (`0x00`).
pub const ASCII_NUL: u8 = 0x00;

/// ASCII line-feed (`LF`, `0x0A`).
pub const ASCII_LF: u8 = 0x0A;

/// ASCII form-feed (`FF`, `0x0C`).
pub const ASCII_FF: u8 = 0x0C;

/// ASCII carriage-return (`CR`, `0x0D`).
pub const ASCII_CR: u8 = 0x0D;

/// Single-byte POSIX line-feed slice (`[0x0A]`).
const LF_BYTES: [u8; 1] = [ASCII_LF];

/// Two-byte CRLF line-terminator slice (`[0x0D, 0x0A]`).
const CRLF_BYTES: [u8; 2] = [ASCII_CR, ASCII_LF];

/// File organization declared in `ORGANIZATION IS ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileOrganization {
    /// Binary or fixed/varying record-sequential (`ORGANIZATION IS SEQUENTIAL`).
    Sequential,
    /// Line-delimited text sequential (`ORGANIZATION IS LINE SEQUENTIAL`).
    LineSequential,
    /// One-based slot-addressed relative records (`ORGANIZATION IS RELATIVE`).
    Relative,
    /// Keyed ISAM B+Tree records (`ORGANIZATION IS INDEXED`).
    Indexed,
}

/// File access mode declared in `ACCESS MODE IS ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessMode {
    /// Sequential record-by-record traversal (`ACCESS MODE IS SEQUENTIAL`).
    Sequential,
    /// Keyed random slot/record access (`ACCESS MODE IS RANDOM`).
    Random,
    /// Combined random and sequential access (`ACCESS MODE IS DYNAMIC`).
    Dynamic,
}

/// Open mode requested by `OPEN {INPUT | OUTPUT | I-O | EXTEND}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpenMode {
    /// Read-only access (`OPEN INPUT`).
    Input,
    /// Create or truncate for writing (`OPEN OUTPUT`).
    Output,
    /// Bidirectional read/update in-place (`OPEN I-O`).
    Io,
    /// Append to end of existing or optional file (`OPEN EXTEND`).
    Extend,
}

/// Whether `SELECT [OPTIONAL]` allows opening a non-existent input/I-O/extend file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileOptionality {
    /// File must already exist (`"35"` when missing on `INPUT` / `I-O` / `EXTEND`).
    Required,
    /// `SELECT OPTIONAL` (`"05"` when missing on `INPUT` / `I-O` / `EXTEND`).
    Optional,
}

/// Disposition phrase on a `CLOSE` verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseDisposition {
    /// Standard `CLOSE file` allowing subsequent re-open.
    Normal,
    /// `CLOSE file WITH LOCK` permanently preventing re-open in the current run unit (`"38"`).
    WithLock,
    /// `CLOSE file WITH NO REWIND` (`"07"` on non-reel disk media, `"00"` on reel media).
    NoRewind,
    /// `CLOSE file REEL` / `UNIT` (`"07"` on non-reel disk media, `"00"` on reel media).
    ReelUnit,
}

/// Physical medium classification governing `"07"` (`SuccessNonReel07`) on `NO REWIND` / `REEL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReelMediumKind {
    /// Standard random-access disk or POSIX stream (`NO REWIND`/`REEL` yields `"07"`).
    NonReelDisk,
    /// Multi-reel tape medium (`NO REWIND`/`REEL` yields `"00"`).
    ReelTape,
}

/// Relational operator specified in `START file KEY IS {= | > | >= | < | <=} key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartRelation {
    /// `KEY IS EQUAL TO` (`=`).
    Equal,
    /// `KEY IS GREATER THAN` (`>`).
    Greater,
    /// `KEY IS NOT LESS THAN` / `GREATER THAN OR EQUAL TO` (`>=`).
    GreaterOrEqual,
    /// `KEY IS LESS THAN` (`<`).
    Less,
    /// `KEY IS NOT GREATER THAN` / `LESS THAN OR EQUAL TO` (`<=`).
    LessOrEqual,
}

impl StartRelation {
    /// Evaluates whether `candidate` satisfies `candidate <op> target`.
    ///
    /// # Arguments
    ///
    /// * `candidate` - Occupied 1-based slot key being tested.
    /// * `target` - Requested `START` target key (`0` permitted when program passes zero).
    #[must_use]
    pub const fn matches_u64(self, candidate: u64, target: u64) -> bool {
        match self {
            Self::Equal => candidate == target,
            Self::Greater => candidate > target,
            Self::GreaterOrEqual => candidate >= target,
            Self::Less => candidate < target,
            Self::LessOrEqual => candidate <= target,
        }
    }
}

/// Sequential traversal direction in `READ file [NEXT | PREVIOUS]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadDirection {
    /// Forward traversal (`READ file` or `READ file NEXT`).
    Next,
    /// Backward traversal (`READ file PREVIOUS`).
    Previous,
}

/// File-level and record-level locking mode (`LOCK MODE IS ...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockMode {
    /// Automatically locks record read in `I-O` mode and releases prior single-record lock (`AUTOMATIC`).
    Automatic,
    /// Locks records only when `WITH LOCK` or `WITH KEPT LOCK` is explicitly specified (`MANUAL`).
    Manual,
    /// Locks the entire file exclusively against all other file connectors (`EXCLUSIVE` -> `"61"`).
    Exclusive,
}

/// Per-statement lock modifier on `READ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadLockPolicy {
    /// Governed by the connector's [`LockMode`].
    DefaultLock,
    /// Explicit `READ ... WITH LOCK`.
    WithLock,
    /// Explicit `READ ... WITH KEPT LOCK` (retains previously held locks on this connector).
    WithKeptLock,
    /// Explicit `READ ... WITH NO LOCK` (bypasses record lock acquisition and contention check).
    WithNoLock,
}

/// Validated non-zero record byte length (`>= 1`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordByteLen(
    /// Non-zero byte count (`>= 1`).
    usize,
);

impl RecordByteLen {
    /// Maximum supported 2-byte big-endian variable record payload length (`65535`).
    pub const MAX_TWO_BYTE_HEADER: usize = u16::MAX as usize;

    /// Constructs a validated non-zero [`RecordByteLen`].
    ///
    /// # Arguments
    ///
    /// * `raw` - Candidate byte length (`0` is rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw == 0`.
    pub fn try_new(raw: usize) -> Result<Self> {
        if raw >= 1 {
            Ok(Self(raw))
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "RecordByteLen",
                value: raw.to_string(),
                reason: String::from("COBOL record byte length must be >= 1"),
            })
        }
    }

    /// Returns the underlying `usize` byte count (`>= 1`).
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Length-prefix header encoding for variable-length `SEQUENTIAL` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VaryingHeaderFormat {
    /// Two-byte big-endian payload length (`u16`, max `65535` bytes).
    TwoByteBigEndian,
    /// Four-byte big-endian payload length (`u32`, up to `u32::MAX` bytes).
    FourByteBigEndian,
}

impl VaryingHeaderFormat {
    /// Returns the byte width (`2` or `4`) of the record length prefix.
    #[must_use]
    pub const fn header_bytes(self) -> usize {
        match self {
            Self::TwoByteBigEndian => 2,
            Self::FourByteBigEndian => 4,
        }
    }
}

/// Fixed or variable (`RECORD IS VARYING ... DEPENDING ON`) FD record size specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordLayout {
    /// Fixed-length records (`RECORD CONTAINS record_len CHARACTERS`).
    Fixed {
        /// Exact fixed record length in bytes.
        record_len: RecordByteLen,
        /// Padding byte used when right-filling short final blocks or `LINE SEQUENTIAL` records.
        padding_byte: u8,
    },
    /// Variable-length records (`RECORD IS VARYING IN SIZE FROM min_len TO max_len DEPENDING ON ...`).
    Varying {
        /// Minimum allowed active record byte length.
        min_len: RecordByteLen,
        /// Maximum allowed active record byte length (`>= min_len`).
        max_len: RecordByteLen,
        /// Physical length-prefix encoding used on binary `SEQUENTIAL` streams.
        header_format: VaryingHeaderFormat,
    },
}

impl RecordLayout {
    /// Creates a space-padded fixed-length [`RecordLayout`].
    ///
    /// # Arguments
    ///
    /// * `raw_len` - Fixed record size in bytes (`>= 1`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw_len == 0`.
    pub fn fixed_spaces(raw_len: usize) -> Result<Self> {
        Ok(Self::Fixed {
            record_len: RecordByteLen::try_new(raw_len)?,
            padding_byte: b' ',
        })
    }

    /// Creates a validated variable-length [`RecordLayout`].
    ///
    /// # Arguments
    ///
    /// * `raw_min` - Minimum allowed record size in bytes (`>= 1`).
    /// * `raw_max` - Maximum allowed record size in bytes (`>= raw_min`).
    /// * `header_format` - Length-prefix encoding (`TwoByteBigEndian` requires `raw_max <= 65535`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw_min == 0`, `raw_min > raw_max`, or
    /// `raw_max` exceeds the representable range of `header_format`.
    pub fn varying(
        raw_min: usize,
        raw_max: usize,
        header_format: VaryingHeaderFormat,
    ) -> Result<Self> {
        let min_len = RecordByteLen::try_new(raw_min)?;
        let max_len = RecordByteLen::try_new(raw_max)?;
        if min_len > max_len {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "RecordLayout",
                value: format!("{raw_min}..={raw_max}"),
                reason: String::from("minimum record length exceeds maximum record length"),
            });
        }
        let fits_header = match header_format {
            VaryingHeaderFormat::TwoByteBigEndian => {
                max_len.get() <= RecordByteLen::MAX_TWO_BYTE_HEADER
            }
            VaryingHeaderFormat::FourByteBigEndian => u32::try_from(max_len.get()).is_ok(),
        };
        if !fits_header {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "RecordLayout",
                value: raw_max.to_string(),
                reason: String::from("maximum record length exceeds header format capacity"),
            });
        }
        Ok(Self::Varying {
            min_len,
            max_len,
            header_format,
        })
    }

    /// Returns the minimum allowed record byte length.
    #[must_use]
    pub const fn min_bytes(self) -> usize {
        match self {
            Self::Fixed { record_len, .. } => record_len.get(),
            Self::Varying { min_len, .. } => min_len.get(),
        }
    }

    /// Returns the maximum allowed record byte length.
    #[must_use]
    pub const fn max_bytes(self) -> usize {
        match self {
            Self::Fixed { record_len, .. } => record_len.get(),
            Self::Varying { max_len, .. } => max_len.get(),
        }
    }

    /// Returns `true` when `candidate_len` lies within `min_bytes()..=max_bytes()`.
    ///
    /// # Arguments
    ///
    /// * `candidate_len` - Record slice length supplied to `WRITE` or `REWRITE`.
    #[must_use]
    pub const fn accepts_write_len(self, candidate_len: usize) -> bool {
        candidate_len >= self.min_bytes() && candidate_len <= self.max_bytes()
    }
}

/// Timing of carriage control relative to record emission (`BEFORE` vs `AFTER ADVANCING`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdvancingPhase {
    /// Emit record first, then advance lines or page (`WRITE ... BEFORE ADVANCING`).
    Before,
    /// Advance lines or page first, then emit record (`WRITE ... AFTER ADVANCING`).
    After,
}

/// Carriage-positioning specification on a `WRITE` statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdvancingSpec {
    /// Raw record write with no carriage-control bytes (default for binary `SEQUENTIAL` without `LINAGE`).
    NoneRaw,
    /// Implicit single-line advance (default for `LINE SEQUENTIAL` and `LINAGE` `WRITE` without `ADVANCING`).
    DefaultSingleLine,
    /// Explicit `BEFORE` or `AFTER ADVANCING count LINES` (`0` = carriage return `0x0D` without `0x0A`).
    Lines {
        /// Whether spacing occurs before or after writing the record.
        phase: AdvancingPhase,
        /// Number of lines to advance (`0` emits `0x0D`).
        count: usize,
    },
    /// Explicit `BEFORE` or `AFTER ADVANCING PAGE`.
    Page {
        /// Whether page ejection occurs before or after writing the record.
        phase: AdvancingPhase,
    },
}

/// Result of checking `AT END-OF-PAGE` (`EOP`) during a `LINAGE` `WRITE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageOverflowOutcome {
    /// Positioning stayed strictly above the footing area and within the current page body.
    WithinPage,
    /// Positioning entered the footing area or triggered logical page overflow (`AT END-OF-PAGE`).
    EndOfPageReached,
}

/// Physical line delimiter emitted on `WRITE` by `LineSequentialFileEngine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineTerminator {
    /// POSIX single line-feed (`0x0A`).
    Lf,
    /// Carriage-return + line-feed (`0x0D 0x0A`).
    CrLf,
}

impl LineTerminator {
    /// Returns the raw byte slice (`[0x0A]` or `[0x0D, 0x0A]`) for this terminator.
    #[must_use]
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => &LF_BYTES,
            Self::CrLf => &CRLF_BYTES,
        }
    }
}

/// Trailing-space handling on `LINE SEQUENTIAL` `WRITE` (`COB_LS_FIXED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrailingSpacePolicy {
    /// Strip trailing ASCII space (`0x20`) bytes before writing the line delimiter (standard default).
    StripTrailingSpaces,
    /// Preserve full fixed/active record length including trailing spaces (`COB_LS_FIXED=1`).
    PreserveFixedSpaces,
}

/// Control-byte NUL-prefix escaping policy on `LINE SEQUENTIAL` (`COB_LS_NULLS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NullEscapePolicy {
    /// Emit and read bytes verbatim without `0x00` control-character escaping.
    RawBytes,
    /// Prefix every byte `< 0x20` with `0x00` on `WRITE` and unescape `[0x00, b]` on `READ` (`COB_LS_NULLS=1`).
    NullPrefixControlBytes,
}

/// Validated 1-based COBOL `RELATIVE KEY` slot number (`>= 1`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativeSlotKey(
    /// Validated 1-based slot number (`>= 1`).
    u64,
);

impl RelativeSlotKey {
    /// Constructs a validated 1-based [`RelativeSlotKey`].
    ///
    /// # Arguments
    ///
    /// * `raw` - One-based relative record number (`0` is rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw == 0`.
    pub fn try_new(raw: u64) -> Result<Self> {
        if raw >= 1 {
            Ok(Self(raw))
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "RelativeSlotKey",
                value: raw.to_string(),
                reason: String::from("COBOL RELATIVE KEY must be >= 1"),
            })
        }
    }

    /// Returns the 1-based `u64` slot number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the 0-based slot index (`self.get() - 1`).
    #[must_use]
    pub const fn zero_based_index(self) -> u64 {
        self.0 - 1
    }

    /// Checks whether `self` fits within the decimal digit capacity `digits` (`<= 10^digits - 1`).
    ///
    /// # Arguments
    ///
    /// * `digits` - Declared decimal `DigitCount` (`1..=38`) of the `RELATIVE KEY` data item.
    #[must_use]
    pub const fn fits_digit_count(self, digits: DigitCount) -> bool {
        let d = digits.get();
        if d >= 20 {
            true
        } else {
            let max_plus_one = 10_u128.pow(d as u32);
            (self.0 as u128) < max_plus_one
        }
    }
}
