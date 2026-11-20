//! Validated COBOL numeric, level, and exit-status domain newtypes.

#![deny(clippy::indexing_slicing)]

use crate::error::{CoblangError, Result};

/// Strongly-typed, invariant-validated COBOL Data Division level number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LevelNumber {
    /// Record/group/elementary hierarchy level (`01`..=`49`).
    Hierarchy(
        /// Validated hierarchy level in `1..=49`.
        u8,
    ),
    /// Level `66` (`RENAMES` aliasing entry).
    Renames66,
    /// Level `77` (independent elementary item).
    Independent77,
    /// Level `78` (compile-time named constant extension).
    Constant78,
    /// Level `88` (condition-name entry).
    Condition88,
}

impl LevelNumber {
    /// Validates and classifies a raw COBOL level integer (`1..=49`, `66`, `77`, `78`, or `88`).
    ///
    /// # Arguments
    ///
    /// * `raw` - Parsed unsigned integer level indicator.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw` is not a valid COBOL level number.
    pub fn parse(raw: u8) -> Result<Self> {
        match raw {
            1..=49 => Ok(Self::Hierarchy(raw)),
            66 => Ok(Self::Renames66),
            77 => Ok(Self::Independent77),
            78 => Ok(Self::Constant78),
            88 => Ok(Self::Condition88),
            invalid => Err(CoblangError::InvalidDomainValue {
                domain_type: "LevelNumber",
                value: invalid.to_string(),
                reason: String::from("expected 01..=49, 66, 77, 78, or 88"),
            }),
        }
    }

    /// Returns the numeric representation (`1..=88`) of this [`LevelNumber`].
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Hierarchy(n) => n,
            Self::Renames66 => 66,
            Self::Independent77 => 77,
            Self::Constant78 => 78,
            Self::Condition88 => 88,
        }
    }
}

/// Validated decimal digit count (`1..=38`) for a COBOL numeric `PICTURE` or intermediate item.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DigitCount(
    /// Validated decimal digit count in `1..=38`.
    u8,
);

impl DigitCount {
    /// Minimum valid single decimal digit count (`1`).
    pub const ONE: Self = Self(1);

    /// Maximum supported decimal digits across ISO COBOL 2023, `GnuCOBOL`, and `gcobol` (`38`).
    pub const MAX_ISO_DIGITS: u8 = 38;

    /// Classic ANSI COBOL-85 numeric `PICTURE` digit limit (`18`).
    pub const MAX_COBOL85_DIGITS: u8 = 18;

    /// Constructs a validated [`DigitCount`] in `1..=38`.
    ///
    /// # Arguments
    ///
    /// * `raw` - Total decimal digit positions (`0` and `> 38` are rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw == 0` or `raw > 38`.
    pub fn try_new(raw: u8) -> Result<Self> {
        if (1..=Self::MAX_ISO_DIGITS).contains(&raw) {
            Ok(Self(raw))
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "DigitCount",
                value: raw.to_string(),
                reason: String::from("COBOL numeric digit count must be in 1..=38"),
            })
        }
    }

    /// Validates this [`DigitCount`] against a dialect-specific maximum digit limit.
    ///
    /// # Arguments
    ///
    /// * `max_digits` - Dialect maximum allowed `PICTURE` digits (e.g., `18` or `38`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `self.get() > max_digits`.
    pub fn validate_for_dialect(self, max_digits: u8) -> Result<Self> {
        if self.0 <= max_digits {
            Ok(self)
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "DigitCount",
                value: self.0.to_string(),
                reason: format!("exceeds dialect maximum of {max_digits} digits"),
            })
        }
    }

    /// Returns the underlying `1..=38` digit count.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Validated signed decimal scale factor (`-38..=38`, where positive counts fractional digits).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScaleFactor(
    /// Signed decimal scale factor in `-38..=38`.
    i16,
);

impl ScaleFactor {
    /// Zero fractional scaling (`0`).
    pub const ZERO: Self = Self(0);

    /// Minimum allowed `P` scaling factor (`-38`).
    pub const MIN_SCALE: i16 = -38;

    /// Maximum allowed fractional/`P` scaling factor (`38`).
    pub const MAX_SCALE: i16 = 38;

    /// Constructs a validated [`ScaleFactor`] within `-38..=38`.
    ///
    /// # Arguments
    ///
    /// * `raw` - Signed decimal scale (`PIC 9V99` -> `2`, `PIC 99P` -> `-1`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw` lies outside `-38..=38`.
    pub fn try_new(raw: i16) -> Result<Self> {
        if (Self::MIN_SCALE..=Self::MAX_SCALE).contains(&raw) {
            Ok(Self(raw))
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "ScaleFactor",
                value: raw.to_string(),
                reason: String::from("COBOL decimal scale factor must be in -38..=38"),
            })
        }
    }

    /// Returns the signed `i16` scale factor.
    #[must_use]
    pub const fn get(self) -> i16 {
        self.0
    }
}

/// Strongly-typed process exit code returned by `coblang` compiler and runtime entry points.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CobolExitCode(
    /// Raw process exit status byte.
    u8,
);

impl CobolExitCode {
    /// Successful compilation or program execution (`0`).
    pub const SUCCESS: Self = Self(0);

    /// Compilation, runtime, or CLI failure (`1`).
    pub const ERROR: Self = Self(1);

    /// Deterministic scaffold/incremental unimplemented feature status (`78` / `EX_CONFIG`).
    pub const UNIMPLEMENTED: Self = Self(78);

    /// Wraps a raw `u8` exit status code.
    ///
    /// # Arguments
    ///
    /// * `code` - Raw process exit status byte.
    #[must_use]
    pub const fn new(code: u8) -> Self {
        Self(code)
    }

    /// Returns the underlying `u8` status byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

/// Canonical ISO COBOL two-character `FILE STATUS` classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileStatusKind {
    /// `"00"` — Successful completion.
    Success00,
    /// `"02"` — Successful completion with duplicate alternate key detected/created.
    SuccessDuplicateKey02,
    /// `"04"` — Record read length differed from fixed FD bounds.
    SuccessRecordLengthMismatch04,
    /// `"05"` — `OPEN` / `CLOSE` on `OPTIONAL` non-existent file.
    SuccessOptionalFile05,
    /// `"07"` — `CLOSE` `NO REWIND` / `REEL` on non-reel medium.
    SuccessNonReel07,
    /// `"10"` — End-of-file (`AT END`) reached on sequential `READ`.
    AtEnd10,
    /// `"14"` — Sequential `READ` relative key exceeded `RELATIVE KEY` digit capacity.
    RelativeKeyOverflow14,
    /// `"21"` — Primary key sequence error or illegal primary key mutation on `REWRITE`.
    InvalidKeySequence21,
    /// `"22"` — Attempt to write duplicate primary or unique alternate/relative key.
    DuplicateKey22,
    /// `"23"` — Record not found for specified key on `READ` / `START` / `DELETE` / `REWRITE`.
    RecordNotFound23,
    /// `"24"` — Disk/boundary overflow on `RELATIVE` or `INDEXED` write.
    BoundaryViolation24,
    /// `"30"` — Permanent I/O error.
    PermanentError30,
    /// `"34"` — Sequential boundary violation.
    SequentialBoundary34,
    /// `"35"` — `OPEN INPUT` / `I-O` / `EXTEND` on missing non-`OPTIONAL` file.
    FileNotFound35,
    /// `"37"` — `OPEN` mode unsupported by device/permissions.
    OpenPermissionDenied37,
    /// `"38"` — `OPEN` attempted on file previously closed `WITH LOCK`.
    ClosedWithLock38,
    /// `"39"` — File attribute conflict between FD declaration and physical file header.
    AttributeConflict39,
    /// `"41"` — `OPEN` attempted on an already open file connector.
    AlreadyOpen41,
    /// `"42"` — `CLOSE` attempted on a file connector that is not open.
    NotOpenOnClose42,
    /// `"43"` — `REWRITE` or `DELETE` in sequential mode without prior successful `READ`.
    NoPriorRead43,
    /// `"44"` — Record length boundary violation on `WRITE` / `REWRITE`.
    RewriteLengthMismatch44,
    /// `"46"` — Sequential `READ` attempted after prior `AT END` (`"10"`) or failed `READ`/`START`.
    ReadBeyondEnd46,
    /// `"47"` — `READ` or `START` attempted on file not opened `INPUT` or `I-O`.
    ReadWrongOpenMode47,
    /// `"48"` — `WRITE` attempted on file not opened `OUTPUT`, `I-O`, or `EXTEND`.
    WriteWrongOpenMode48,
    /// `"49"` — `REWRITE` or `DELETE` attempted on file not opened `I-O`.
    RewriteDeleteWrongOpenMode49,
    /// `"51"` — Record locked by another file connector.
    RecordLocked51,
    /// `"61"` — File sharing conflict (`LOCK MODE EXCLUSIVE`).
    FileSharingConflict61,
    /// `"91"` — Runtime/OS implementation-defined file error.
    ImplementationDefined91,
}

impl FileStatusKind {
    /// All canonical [`FileStatusKind`] variants in ascending status-code order.
    pub const ALL: &'static [Self] = &[
        Self::Success00,
        Self::SuccessDuplicateKey02,
        Self::SuccessRecordLengthMismatch04,
        Self::SuccessOptionalFile05,
        Self::SuccessNonReel07,
        Self::AtEnd10,
        Self::RelativeKeyOverflow14,
        Self::InvalidKeySequence21,
        Self::DuplicateKey22,
        Self::RecordNotFound23,
        Self::BoundaryViolation24,
        Self::PermanentError30,
        Self::SequentialBoundary34,
        Self::FileNotFound35,
        Self::OpenPermissionDenied37,
        Self::ClosedWithLock38,
        Self::AttributeConflict39,
        Self::AlreadyOpen41,
        Self::NotOpenOnClose42,
        Self::NoPriorRead43,
        Self::RewriteLengthMismatch44,
        Self::ReadBeyondEnd46,
        Self::ReadWrongOpenMode47,
        Self::WriteWrongOpenMode48,
        Self::RewriteDeleteWrongOpenMode49,
        Self::RecordLocked51,
        Self::FileSharingConflict61,
        Self::ImplementationDefined91,
    ];

    /// Converts this [`FileStatusKind`] into its two-byte ASCII [`FileStatusCode`].
    #[must_use]
    pub const fn code(self) -> FileStatusCode {
        let bytes = match self {
            Self::Success00 => *b"00",
            Self::SuccessDuplicateKey02 => *b"02",
            Self::SuccessRecordLengthMismatch04 => *b"04",
            Self::SuccessOptionalFile05 => *b"05",
            Self::SuccessNonReel07 => *b"07",
            Self::AtEnd10 => *b"10",
            Self::RelativeKeyOverflow14 => *b"14",
            Self::InvalidKeySequence21 => *b"21",
            Self::DuplicateKey22 => *b"22",
            Self::RecordNotFound23 => *b"23",
            Self::BoundaryViolation24 => *b"24",
            Self::PermanentError30 => *b"30",
            Self::SequentialBoundary34 => *b"34",
            Self::FileNotFound35 => *b"35",
            Self::OpenPermissionDenied37 => *b"37",
            Self::ClosedWithLock38 => *b"38",
            Self::AttributeConflict39 => *b"39",
            Self::AlreadyOpen41 => *b"41",
            Self::NotOpenOnClose42 => *b"42",
            Self::NoPriorRead43 => *b"43",
            Self::RewriteLengthMismatch44 => *b"44",
            Self::ReadBeyondEnd46 => *b"46",
            Self::ReadWrongOpenMode47 => *b"47",
            Self::WriteWrongOpenMode48 => *b"48",
            Self::RewriteDeleteWrongOpenMode49 => *b"49",
            Self::RecordLocked51 => *b"51",
            Self::FileSharingConflict61 => *b"61",
            Self::ImplementationDefined91 => *b"91",
        };
        FileStatusCode(bytes)
    }
}

/// Two-character ASCII `FILE STATUS` value (`[u8; 2]`, e.g. `b"00"`, `b"35"`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileStatusCode(
    /// Two printable non-space ASCII status bytes.
    [u8; 2],
);

impl FileStatusCode {
    /// Constructs a [`FileStatusCode`] from its strongly-typed [`FileStatusKind`] classification.
    ///
    /// # Arguments
    ///
    /// * `kind` - Canonical ISO COBOL file status classification.
    #[must_use]
    pub const fn from_kind(kind: FileStatusKind) -> Self {
        kind.code()
    }

    /// Constructs a validated two-character ASCII [`FileStatusCode`].
    ///
    /// # Arguments
    ///
    /// * `bytes` - Two printable non-space ASCII status characters (`0x21..=0x7E`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if either byte is not printable non-space ASCII.
    pub fn try_from_ascii(bytes: [u8; 2]) -> Result<Self> {
        let [first, second] = bytes;
        if first.is_ascii_graphic() && second.is_ascii_graphic() {
            Ok(Self(bytes))
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "FileStatusCode",
                value: format!("{bytes:?}"),
                reason: String::from("FILE STATUS bytes must be printable non-space ASCII"),
            })
        }
    }

    /// Classifies this two-character code into a canonical [`FileStatusKind`], if recognized.
    #[must_use]
    pub const fn kind(self) -> Option<FileStatusKind> {
        match u16::from_be_bytes(self.0) {
            0x3030 => Some(FileStatusKind::Success00),
            0x3032 => Some(FileStatusKind::SuccessDuplicateKey02),
            0x3034 => Some(FileStatusKind::SuccessRecordLengthMismatch04),
            0x3035 => Some(FileStatusKind::SuccessOptionalFile05),
            0x3037 => Some(FileStatusKind::SuccessNonReel07),
            0x3130 => Some(FileStatusKind::AtEnd10),
            0x3134 => Some(FileStatusKind::RelativeKeyOverflow14),
            0x3231 => Some(FileStatusKind::InvalidKeySequence21),
            0x3232 => Some(FileStatusKind::DuplicateKey22),
            0x3233 => Some(FileStatusKind::RecordNotFound23),
            0x3234 => Some(FileStatusKind::BoundaryViolation24),
            0x3330 => Some(FileStatusKind::PermanentError30),
            0x3334 => Some(FileStatusKind::SequentialBoundary34),
            0x3335 => Some(FileStatusKind::FileNotFound35),
            0x3337 => Some(FileStatusKind::OpenPermissionDenied37),
            0x3338 => Some(FileStatusKind::ClosedWithLock38),
            0x3339 => Some(FileStatusKind::AttributeConflict39),
            0x3431 => Some(FileStatusKind::AlreadyOpen41),
            0x3432 => Some(FileStatusKind::NotOpenOnClose42),
            0x3433 => Some(FileStatusKind::NoPriorRead43),
            0x3434 => Some(FileStatusKind::RewriteLengthMismatch44),
            0x3436 => Some(FileStatusKind::ReadBeyondEnd46),
            0x3437 => Some(FileStatusKind::ReadWrongOpenMode47),
            0x3438 => Some(FileStatusKind::WriteWrongOpenMode48),
            0x3439 => Some(FileStatusKind::RewriteDeleteWrongOpenMode49),
            0x3531 => Some(FileStatusKind::RecordLocked51),
            0x3631 => Some(FileStatusKind::FileSharingConflict61),
            0x3931 => Some(FileStatusKind::ImplementationDefined91),
            _ => None,
        }
    }

    /// Returns the raw two-byte ASCII status array.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 2] {
        &self.0
    }

    /// Returns the status as a two-character `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.0).unwrap_or("91")
    }

    /// Returns `true` when this status indicates successful completion (`'0'` first byte).
    #[must_use]
    pub const fn is_success(self) -> bool {
        let [hi, _] = self.0;
        hi == b'0'
    }
}

/// Strongly-typed compiler or test-runner resource budget classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceLimitKind {
    /// Maximum nested `COPY` inclusion depth.
    CopyDepth,
    /// Maximum preprocessed or `REPLACE`-expanded token stream length.
    ExpandedTokens,
    /// Maximum parser cursor operations per compilation group.
    ParseSteps,
    /// Maximum recursive-descent expression, condition, or statement nesting depth.
    ParseRecursionDepth,
    /// Maximum expanded `PICTURE` character positions.
    PictureLength,
    /// Maximum captured subprocess output bytes per stream.
    SubprocessOutputBytes,
}

impl ResourceLimitKind {
    /// All [`ResourceLimitKind`] variants in canonical order.
    pub const ALL: &'static [Self] = &[
        Self::CopyDepth,
        Self::ExpandedTokens,
        Self::ParseSteps,
        Self::ParseRecursionDepth,
        Self::PictureLength,
        Self::SubprocessOutputBytes,
    ];

    /// Returns the canonical diagnostic identifier for this [`ResourceLimitKind`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CopyDepth => "copy-depth",
            Self::ExpandedTokens => "expanded-tokens",
            Self::ParseSteps => "parse-steps",
            Self::ParseRecursionDepth => "parse-recursion-depth",
            Self::PictureLength => "picture-length",
            Self::SubprocessOutputBytes => "subprocess-output-bytes",
        }
    }
}
