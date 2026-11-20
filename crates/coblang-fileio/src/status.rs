//! Exhaustive 28-code ISO COBOL `FILE STATUS` transition table, connector lifecycle state machine,
//! OS error classifier, and post-statement control-flow dispatcher.

use std::io::ErrorKind;

use coblang_common::{CoblangError, FileStatusCode, FileStatusKind, Result};

use crate::types::{AccessMode, CloseDisposition, FileOrganization, OpenMode, ReelMediumKind};

/// High-level ISO COBOL status class corresponding to the two-character `FILE STATUS` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusCategory {
    /// `"00"` — Exact unconditional success.
    SuccessExact,
    /// `"02"`, `"04"`, `"05"`, `"07"` — Successful completion with non-fatal warning condition.
    SuccessWarning,
    /// `"10"`, `"14"` — Sequential `AT END` condition (`1x`).
    AtEnd,
    /// `"21"`, `"22"`, `"23"`, `"24"` — Keyed `INVALID KEY` condition (`2x`).
    InvalidKey,
    /// `"30"`, `"34"`, `"35"`, `"37"`, `"38"`, `"39"` — Permanent physical/attribute error (`3x`).
    PermanentError,
    /// `"41"`, `"42"`, `"43"`, `"44"`, `"46"`, `"47"`, `"48"`, `"49"` — Protocol logic error (`4x`).
    LogicError,
    /// `"51"` — Record currently locked by another file connector (`5x`).
    RecordLockContention,
    /// `"61"` — Exclusive file-sharing conflict (`6x`).
    FileSharingConflict,
    /// `"91"` — Runtime/OS implementation-defined error (`9x`).
    ImplementorError,
}

/// ISO/IEC 1989:2023 `EC-I-O-*` exception condition associated with a non-`"00"` [`FileStatusKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoExceptionClass {
    /// `EC-I-O-WARNING` (`02`, `04`, `05`, `07`).
    Warning,
    /// `EC-I-O-AT-END` (`10`, `14`).
    AtEnd,
    /// `EC-I-O-INVALID-KEY` (`21`, `22`, `23`, `24`).
    InvalidKey,
    /// `EC-I-O-PERMANENT-ERROR` (`30`, `34`, `35`, `37`, `38`, `39`).
    PermanentError,
    /// `EC-I-O-LOGIC-ERROR` (`41`, `42`, `43`, `44`, `46`, `47`, `48`, `49`).
    LogicError,
    /// `EC-I-O-FILE-SHARING` (`51`, `61`).
    FileSharing,
    /// `EC-I-O-IMP` (`91`).
    Implementor,
}

impl IoExceptionClass {
    /// Returns the canonical ISO 2023 `EC-I-O-*` exception condition name.
    #[must_use]
    pub const fn ec_name(self) -> &'static str {
        match self {
            Self::Warning => "EC-I-O-WARNING",
            Self::AtEnd => "EC-I-O-AT-END",
            Self::InvalidKey => "EC-I-O-INVALID-KEY",
            Self::PermanentError => "EC-I-O-PERMANENT-ERROR",
            Self::LogicError => "EC-I-O-LOGIC-ERROR",
            Self::FileSharing => "EC-I-O-FILE-SHARING",
            Self::Implementor => "EC-I-O-IMP",
        }
    }
}

/// Static metadata entry in the exhaustive 28-code [`FileStatusKind`] transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusDescriptor {
    /// Canonical [`FileStatusKind`] variant.
    pub kind: FileStatusKind,
    /// Two-byte ASCII [`FileStatusCode`] (`"00"`..`"91"`).
    pub code: FileStatusCode,
    /// High-level [`StatusCategory`].
    pub category: StatusCategory,
    /// Associated ISO 2023 `EC-I-O-*` condition (`None` only for `"00"`).
    pub exception: Option<IoExceptionClass>,
    /// Concise human-readable ISO COBOL diagnostic summary.
    pub description: &'static str,
}

impl StatusDescriptor {
    /// Constructs a [`StatusDescriptor`] for `kind`.
    ///
    /// # Arguments
    ///
    /// * `kind` - Status kind.
    /// * `category` - High-level category.
    /// * `exception` - Optional `EC-I-O-*` class.
    /// * `description` - Concise diagnostic text.
    #[must_use]
    const fn entry(
        kind: FileStatusKind,
        category: StatusCategory,
        exception: Option<IoExceptionClass>,
        description: &'static str,
    ) -> Self {
        Self {
            kind,
            code: kind.code(),
            category,
            exception,
            description,
        }
    }
}

/// Returns the static [`StatusDescriptor`] for any of the 28 canonical [`FileStatusKind`] codes.
///
/// Covers `"00"`, `"02"`, `"04"`, `"05"`, `"07"`, `"10"`, `"14"`, `"21"`, `"22"`, `"23"`, `"24"`,
/// `"30"`, `"34"`, `"35"`, `"37"`, `"38"`, `"39"`, `"41"`, `"42"`, `"43"`, `"44"`, `"46"`, `"47"`,
/// `"48"`, `"49"`, `"51"`, `"61"`, and `"91"`.
///
/// # Arguments
///
/// * `kind` - Canonical ISO COBOL file status classification.
#[must_use]
pub const fn status_descriptor(kind: FileStatusKind) -> StatusDescriptor {
    match kind {
        FileStatusKind::Success00 => StatusDescriptor::entry(
            kind,
            StatusCategory::SuccessExact,
            None,
            "successful completion",
        ),
        FileStatusKind::SuccessDuplicateKey02
        | FileStatusKind::SuccessRecordLengthMismatch04
        | FileStatusKind::SuccessOptionalFile05
        | FileStatusKind::SuccessNonReel07 => describe_warning_status(kind),
        FileStatusKind::AtEnd10 | FileStatusKind::RelativeKeyOverflow14 => {
            describe_at_end_status(kind)
        }
        FileStatusKind::InvalidKeySequence21
        | FileStatusKind::DuplicateKey22
        | FileStatusKind::RecordNotFound23
        | FileStatusKind::BoundaryViolation24 => describe_invalid_key_status(kind),
        FileStatusKind::PermanentError30
        | FileStatusKind::SequentialBoundary34
        | FileStatusKind::FileNotFound35
        | FileStatusKind::OpenPermissionDenied37
        | FileStatusKind::ClosedWithLock38
        | FileStatusKind::AttributeConflict39 => describe_permanent_status(kind),
        FileStatusKind::AlreadyOpen41
        | FileStatusKind::NotOpenOnClose42
        | FileStatusKind::NoPriorRead43
        | FileStatusKind::RewriteLengthMismatch44
        | FileStatusKind::ReadBeyondEnd46
        | FileStatusKind::ReadWrongOpenMode47
        | FileStatusKind::WriteWrongOpenMode48
        | FileStatusKind::RewriteDeleteWrongOpenMode49 => describe_logic_status(kind),
        FileStatusKind::RecordLocked51 => StatusDescriptor::entry(
            kind,
            StatusCategory::RecordLockContention,
            Some(IoExceptionClass::FileSharing),
            "target record is locked by another file connector",
        ),
        FileStatusKind::FileSharingConflict61 => StatusDescriptor::entry(
            kind,
            StatusCategory::FileSharingConflict,
            Some(IoExceptionClass::FileSharing),
            "file sharing conflict due to EXCLUSIVE lock mode",
        ),
        FileStatusKind::ImplementationDefined91 => StatusDescriptor::entry(
            kind,
            StatusCategory::ImplementorError,
            Some(IoExceptionClass::Implementor),
            "implementation-defined runtime or OS file error",
        ),
    }
}

/// Builds a `0x` warning [`StatusDescriptor`] (`02`, `04`, `05`, `07`).
///
/// # Arguments
///
/// * `kind` - Warning [`FileStatusKind`].
#[must_use]
const fn describe_warning_status(kind: FileStatusKind) -> StatusDescriptor {
    let text = if matches!(kind, FileStatusKind::SuccessDuplicateKey02) {
        "duplicate alternate record key detected or created"
    } else if matches!(kind, FileStatusKind::SuccessRecordLengthMismatch04) {
        "read record length differed from FD record bounds"
    } else if matches!(kind, FileStatusKind::SuccessOptionalFile05) {
        "optional file not present at OPEN"
    } else {
        "NO REWIND or REEL/UNIT specified on non-reel medium"
    };
    StatusDescriptor::entry(
        kind,
        StatusCategory::SuccessWarning,
        Some(IoExceptionClass::Warning),
        text,
    )
}

/// Builds a `1x` `AT END` [`StatusDescriptor`] (`10`, `14`).
///
/// # Arguments
///
/// * `kind` - `AT END` [`FileStatusKind`].
#[must_use]
const fn describe_at_end_status(kind: FileStatusKind) -> StatusDescriptor {
    let text = if matches!(kind, FileStatusKind::AtEnd10) {
        "end of file (AT END) reached on sequential READ"
    } else {
        "sequential READ relative record number exceeded RELATIVE KEY digits"
    };
    StatusDescriptor::entry(
        kind,
        StatusCategory::AtEnd,
        Some(IoExceptionClass::AtEnd),
        text,
    )
}

/// Builds a `2x` `INVALID KEY` [`StatusDescriptor`] (`21`, `22`, `23`, `24`).
///
/// # Arguments
///
/// * `kind` - `INVALID KEY` [`FileStatusKind`].
#[must_use]
const fn describe_invalid_key_status(kind: FileStatusKind) -> StatusDescriptor {
    let text = if matches!(kind, FileStatusKind::InvalidKeySequence21) {
        "primary key sequence error or illegal primary key mutation"
    } else if matches!(kind, FileStatusKind::DuplicateKey22) {
        "attempt to write duplicate primary, unique alternate, or relative key"
    } else if matches!(kind, FileStatusKind::RecordNotFound23) {
        "no record found matching specified key"
    } else {
        "relative or indexed file boundary or key-digit capacity exceeded"
    };
    StatusDescriptor::entry(
        kind,
        StatusCategory::InvalidKey,
        Some(IoExceptionClass::InvalidKey),
        text,
    )
}

/// Builds a `3x` permanent-error [`StatusDescriptor`] (`30`, `34`, `35`, `37`, `38`, `39`).
///
/// # Arguments
///
/// * `kind` - Permanent-error [`FileStatusKind`].
#[must_use]
const fn describe_permanent_status(kind: FileStatusKind) -> StatusDescriptor {
    let text = if matches!(kind, FileStatusKind::PermanentError30) {
        "permanent physical I/O error"
    } else if matches!(kind, FileStatusKind::SequentialBoundary34) {
        "sequential file boundary overflow on WRITE"
    } else if matches!(kind, FileStatusKind::FileNotFound35) {
        "non-optional file not found on OPEN INPUT, I-O, or EXTEND"
    } else if matches!(kind, FileStatusKind::OpenPermissionDenied37) {
        "OPEN mode unsupported by file organization, access mode, or permissions"
    } else if matches!(kind, FileStatusKind::ClosedWithLock38) {
        "OPEN attempted on file previously closed WITH LOCK"
    } else {
        "physical file attributes conflict with FD specification"
    };
    StatusDescriptor::entry(
        kind,
        StatusCategory::PermanentError,
        Some(IoExceptionClass::PermanentError),
        text,
    )
}

/// Builds a `4x` logic-error [`StatusDescriptor`] (`41`..`49`).
///
/// # Arguments
///
/// * `kind` - Logic-error [`FileStatusKind`].
#[must_use]
const fn describe_logic_status(kind: FileStatusKind) -> StatusDescriptor {
    let text = if matches!(kind, FileStatusKind::AlreadyOpen41) {
        "OPEN attempted on an already open file connector"
    } else if matches!(kind, FileStatusKind::NotOpenOnClose42) {
        "CLOSE attempted on a file connector that is not open"
    } else if matches!(kind, FileStatusKind::NoPriorRead43) {
        "REWRITE or DELETE in sequential access mode without prior successful READ"
    } else if matches!(kind, FileStatusKind::RewriteLengthMismatch44) {
        "record length out of FD bounds or differs from replaced record on REWRITE"
    } else if matches!(kind, FileStatusKind::ReadBeyondEnd46) {
        "sequential READ attempted with no valid next record position"
    } else if matches!(kind, FileStatusKind::ReadWrongOpenMode47) {
        "READ or START attempted on file not opened INPUT or I-O"
    } else if matches!(kind, FileStatusKind::WriteWrongOpenMode48) {
        "WRITE attempted on file not opened in a valid output mode"
    } else {
        "REWRITE or DELETE attempted on file not opened I-O"
    };
    StatusDescriptor::entry(
        kind,
        StatusCategory::LogicError,
        Some(IoExceptionClass::LogicError),
        text,
    )
}

/// Copies the two ASCII status characters of `status` into a COBOL `PIC XX` `FILE STATUS` buffer.
///
/// # Arguments
///
/// * `status` - Resulting two-character [`FileStatusCode`].
/// * `target` - Two-byte destination buffer representing the COBOL `FILE STATUS` data item.
pub const fn write_status_bytes(status: FileStatusCode, target: &mut [u8; 2]) {
    *target = *status.as_bytes();
}

/// Context verb category used when mapping an OS [`std::io::Error`] to a [`FileStatusKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoErrorContext {
    /// Error occurred during `OPEN`.
    Open,
    /// Error occurred during sequential `WRITE` (`StorageFull` -> `"34"`).
    SequentialWrite,
    /// Error occurred during keyed `RELATIVE` / `INDEXED` `WRITE` (`StorageFull` -> `"24"`).
    KeyedWrite,
    /// Error occurred during `READ`, `REWRITE`, `DELETE`, `START`, or `CLOSE`.
    GeneralIo,
}

/// Classifies a [`CoblangError`] produced by a [`crate::storage::StorageBackend`] into a [`FileStatusKind`].
///
/// # Arguments
///
/// * `err` - Underlying [`CoblangError`] (typically [`CoblangError::Io`]).
/// * `context` - Statement context distinguishing sequential `"34"` from keyed `"24"` overflow.
#[must_use]
pub fn classify_storage_error(err: &CoblangError, context: IoErrorContext) -> FileStatusKind {
    match err {
        CoblangError::Io(io_err) => classify_io_error(io_err, context),
        _ => FileStatusKind::PermanentError30,
    }
}

/// Classifies a [`std::io::Error`] into the deterministic ISO COBOL [`FileStatusKind`].
///
/// # Arguments
///
/// * `err` - Underlying standard I/O error.
/// * `context` - Statement context distinguishing sequential `"34"` from keyed `"24"` overflow.
#[must_use]
pub fn classify_io_error(err: &std::io::Error, context: IoErrorContext) -> FileStatusKind {
    match err.kind() {
        ErrorKind::NotFound => match context {
            IoErrorContext::Open => FileStatusKind::FileNotFound35,
            IoErrorContext::SequentialWrite
            | IoErrorContext::KeyedWrite
            | IoErrorContext::GeneralIo => FileStatusKind::PermanentError30,
        },
        ErrorKind::PermissionDenied => match context {
            IoErrorContext::Open => FileStatusKind::OpenPermissionDenied37,
            IoErrorContext::SequentialWrite
            | IoErrorContext::KeyedWrite
            | IoErrorContext::GeneralIo => FileStatusKind::PermanentError30,
        },
        ErrorKind::StorageFull | ErrorKind::WriteZero | ErrorKind::FileTooLarge => match context {
            IoErrorContext::SequentialWrite => FileStatusKind::SequentialBoundary34,
            IoErrorContext::KeyedWrite => FileStatusKind::BoundaryViolation24,
            IoErrorContext::Open | IoErrorContext::GeneralIo => FileStatusKind::PermanentError30,
        },
        ErrorKind::WouldBlock => FileStatusKind::RecordLocked51,
        ErrorKind::UnexpectedEof | ErrorKind::InvalidData | ErrorKind::BrokenPipe => {
            FileStatusKind::PermanentError30
        }
        _ => FileStatusKind::ImplementationDefined91,
    }
}

/// Presence of an inline `AT END`, `INVALID KEY`, or `AT END-OF-PAGE` clause on an I/O statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InlineClausePresence {
    /// Statement includes the inline handler phrase.
    Present,
    /// Statement omits the inline handler phrase.
    Absent,
}

/// Whether the file connector declares a `FILE STATUS IS data-name` item in `FILE-CONTROL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusClausePresence {
    /// `FILE STATUS IS ...` is declared and receives the two-byte status code.
    Declared,
    /// No `FILE STATUS` item is declared for this file connector.
    Undeclared,
}

/// Whether an applicable `USE AFTER STANDARD EXCEPTION/ERROR PROCEDURE` declarative exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeclarativePresence {
    /// Matching file-specific or open-mode `USE` procedure is active.
    MatchingUseProcedure,
    /// No matching `USE` procedure exists.
    None,
}

/// Exception-handling bindings in effect at a particular COBOL I/O statement site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VerbHandlerConfig {
    /// Whether `AT END` is written on this `READ` statement.
    pub at_end_clause: InlineClausePresence,
    /// Whether `INVALID KEY` is written on this keyed `READ` / `WRITE` / `REWRITE` / `DELETE` / `START`.
    pub invalid_key_clause: InlineClausePresence,
    /// Whether `FILE STATUS IS ...` is declared on the file connector.
    pub file_status_item: StatusClausePresence,
    /// Whether an applicable `DECLARATIVES` `USE AFTER EXCEPTION` procedure exists.
    pub declarative: DeclarativePresence,
}

/// Control-flow action selected after updating `FILE STATUS` on an I/O verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PostIoDispatch {
    /// Status is `0x` (`"00"`, `"02"`, `"04"`, `"05"`, `"07"`): execute `NOT AT END` / `NOT INVALID KEY` if present and continue.
    NormalCompletion,
    /// Status is `1x` (`"10"`, `"14"`) and `AT END` phrase is present: branch to inline `AT END` statements.
    FireAtEnd,
    /// Status is `2x` (`"21"`, `"22"`, `"23"`, `"24"`) and `INVALID KEY` phrase is present: branch to inline `INVALID KEY` statements.
    FireInvalidKey,
    /// Unhandled `1x`/`2x` or `3x`/`4x`/`5x`/`6x`/`9x` status with matching `USE` declarative: invoke declarative section.
    InvokeDeclarative,
    /// Unhandled non-`0x` status without inline handler or declarative, absorbed because `FILE STATUS` is declared.
    AbsorbViaFileStatus,
}

/// Determines the exact ISO COBOL post-statement control-flow action for `status_kind`.
///
/// # Arguments
///
/// * `file_name` - Logical COBOL file connector name (used if an unhandled runtime abort occurs).
/// * `status_kind` - Completed operation [`FileStatusKind`].
/// * `handlers` - Inline clause, `FILE STATUS`, and `DECLARATIVES` bindings for the statement.
///
/// # Errors
///
/// Returns [`CoblangError::RuntimeFileIo`] when `status_kind` is non-`0x` and neither an
/// applicable inline clause (`AT END` / `INVALID KEY`), nor a `USE` declarative, nor a
/// `FILE STATUS` clause is present to handle the failure.
pub fn resolve_post_io_action(
    file_name: &str,
    status_kind: FileStatusKind,
    handlers: VerbHandlerConfig,
) -> Result<PostIoDispatch> {
    let desc = status_descriptor(status_kind);
    match desc.category {
        StatusCategory::SuccessExact | StatusCategory::SuccessWarning => {
            Ok(PostIoDispatch::NormalCompletion)
        }
        StatusCategory::AtEnd => {
            if handlers.at_end_clause == InlineClausePresence::Present {
                Ok(PostIoDispatch::FireAtEnd)
            } else {
                fallback_unhandled_io(file_name, desc, handlers)
            }
        }
        StatusCategory::InvalidKey => {
            if handlers.invalid_key_clause == InlineClausePresence::Present {
                Ok(PostIoDispatch::FireInvalidKey)
            } else {
                fallback_unhandled_io(file_name, desc, handlers)
            }
        }
        StatusCategory::PermanentError
        | StatusCategory::LogicError
        | StatusCategory::RecordLockContention
        | StatusCategory::FileSharingConflict
        | StatusCategory::ImplementorError => fallback_unhandled_io(file_name, desc, handlers),
    }
}

/// Resolves declarative vs `FILE STATUS` absorption vs hard runtime abort for unhandled I/O errors.
///
/// # Arguments
///
/// * `file_name` - Logical COBOL file connector name.
/// * `desc` - Descriptor of the non-`0x` status condition.
/// * `handlers` - Statement handler configuration.
fn fallback_unhandled_io(
    file_name: &str,
    desc: StatusDescriptor,
    handlers: VerbHandlerConfig,
) -> Result<PostIoDispatch> {
    if handlers.declarative == DeclarativePresence::MatchingUseProcedure {
        Ok(PostIoDispatch::InvokeDeclarative)
    } else if handlers.file_status_item == StatusClausePresence::Declared {
        Ok(PostIoDispatch::AbsorbViaFileStatus)
    } else {
        Err(CoblangError::RuntimeFileIo {
            file_name: String::from(file_name),
            status: desc.code,
            message: String::from(desc.description),
        })
    }
}

/// Open/closed lifecycle state of a single COBOL file connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectorLifecycle {
    /// Connector is closed and eligible for `OPEN`.
    Closed,
    /// Connector is currently open in the given [`OpenMode`].
    Open(OpenMode),
    /// Connector was closed `WITH LOCK` and rejects all subsequent `OPEN` attempts with `"38"`.
    ClosedWithLock,
}

/// Sequential File Position Indicator (`FPI`) state for `READ NEXT` / `READ PREVIOUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SequentialCursorState {
    /// Valid sequential position is established (`OPEN` or `START` or prior `READ`).
    Positioned,
    /// Prior sequential `READ` reached `AT END` (`"10"` / `"14"`): next sequential `READ` yields `"46"`.
    AtEndLatched,
    /// Prior `START` or keyed `READ` failed to establish a position: next sequential `READ` yields `"46"`.
    Unpositioned,
}

/// Tracks the most recently read record for `SEQUENTIAL` access-mode `REWRITE` / `DELETE` (`"43"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PriorReadRecord {
    /// Physical byte offset or 1-based slot number of the read record.
    pub position_key: u64,
    /// Payload byte length of the read record (checked for `"44"` on sequential `REWRITE`).
    pub payload_len: usize,
    /// Total framed byte span occupied in a sequential stream (header + payload).
    pub framed_len: usize,
}

/// Domain outcome of checking statement preconditions on [`ConnectorStateMachine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrecheckOutcome {
    /// Statement preconditions succeeded; execution may proceed.
    Allowed,
    /// Statement preconditions failed; `FILE STATUS` has been updated to this [`FileStatusKind`].
    Rejected(FileStatusKind),
}

/// Domain outcome of checking `CLOSE` preconditions and updating lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseTransitionOutcome {
    /// Connector closed physical handle and set status (`"00"` or `"07"`).
    ClosedHandle(FileStatusCode),
    /// `CLOSE REEL/UNIT` kept connector open and set status (`"00"` or `"07"`).
    RetainedReelOpen(FileStatusCode),
    /// Connector was not open; status set to `"42"`.
    NotOpen(FileStatusCode),
}

/// Domain outcome of checking `REWRITE` or `DELETE` preconditions on [`ConnectorStateMachine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MutationPrecheckOutcome {
    /// `OPEN I-O` (and prior `READ` when `SEQUENTIAL`) verified.
    Allowed(Option<PriorReadRecord>),
    /// Rejected with `"43"` or `"49"`.
    Rejected(FileStatusKind),
}

/// Reusable ISO COBOL `FILE STATUS` and connector protocol state machine shared across engines.
#[derive(Debug, Clone)]
pub struct ConnectorStateMachine {
    /// File organization of this connector.
    organization: FileOrganization,
    /// Access mode of this connector.
    access_mode: AccessMode,
    /// Physical medium kind (`NonReelDisk` vs `ReelTape`).
    medium: ReelMediumKind,
    /// Current open/closed/locked lifecycle state.
    lifecycle: ConnectorLifecycle,
    /// Current sequential File Position Indicator validity.
    cursor: SequentialCursorState,
    /// Record established by the immediately preceding successful `READ`, if still valid.
    prior_read: Option<PriorReadRecord>,
    /// Most recent [`FileStatusKind`] recorded on this connector.
    last_status: FileStatusKind,
}

impl ConnectorStateMachine {
    /// Creates a closed [`ConnectorStateMachine`] with initial status `"00"`.
    ///
    /// # Arguments
    ///
    /// * `organization` - File organization (`Sequential`, `LineSequential`, `Relative`, `Indexed`).
    /// * `access_mode` - Access mode (`Sequential`, `Random`, `Dynamic`).
    /// * `medium` - Physical medium kind (`NonReelDisk` vs `ReelTape`).
    #[must_use]
    pub const fn new(
        organization: FileOrganization,
        access_mode: AccessMode,
        medium: ReelMediumKind,
    ) -> Self {
        Self {
            organization,
            access_mode,
            medium,
            lifecycle: ConnectorLifecycle::Closed,
            cursor: SequentialCursorState::Unpositioned,
            prior_read: None,
            last_status: FileStatusKind::Success00,
        }
    }

    /// Returns the current [`ConnectorLifecycle`].
    #[must_use]
    pub const fn lifecycle(&self) -> ConnectorLifecycle {
        self.lifecycle
    }

    /// Returns the current [`SequentialCursorState`].
    #[must_use]
    pub const fn cursor(&self) -> SequentialCursorState {
        self.cursor
    }

    /// Returns the active [`PriorReadRecord`], if a prior `READ` remains valid for `REWRITE`/`DELETE`.
    #[must_use]
    pub const fn prior_read(&self) -> Option<PriorReadRecord> {
        self.prior_read
    }

    /// Returns the most recently recorded [`FileStatusKind`].
    #[must_use]
    pub const fn last_status_kind(&self) -> FileStatusKind {
        self.last_status
    }

    /// Returns the most recently recorded two-character [`FileStatusCode`].
    #[must_use]
    pub const fn last_status_code(&self) -> FileStatusCode {
        self.last_status.code()
    }

    /// Records `kind` as the connector's latest status and returns its [`FileStatusCode`].
    ///
    /// # Arguments
    ///
    /// * `kind` - Newly determined [`FileStatusKind`].
    pub const fn record_status(&mut self, kind: FileStatusKind) -> FileStatusCode {
        self.last_status = kind;
        kind.code()
    }

    /// Validates pre-`OPEN` lifecycle and mode-combination legality before touching storage.
    ///
    /// # Arguments
    ///
    /// * `requested` - Requested [`OpenMode`].
    #[must_use]
    pub fn precheck_open(&mut self, requested: OpenMode) -> PrecheckOutcome {
        match self.lifecycle {
            ConnectorLifecycle::ClosedWithLock => {
                self.last_status = FileStatusKind::ClosedWithLock38;
                return PrecheckOutcome::Rejected(FileStatusKind::ClosedWithLock38);
            }
            ConnectorLifecycle::Open(_) => {
                self.last_status = FileStatusKind::AlreadyOpen41;
                return PrecheckOutcome::Rejected(FileStatusKind::AlreadyOpen41);
            }
            ConnectorLifecycle::Closed => {}
        }

        let mode_supported = match self.organization {
            FileOrganization::Sequential => self.access_mode == AccessMode::Sequential,
            FileOrganization::LineSequential => {
                self.access_mode == AccessMode::Sequential && requested != OpenMode::Io
            }
            FileOrganization::Relative | FileOrganization::Indexed => match requested {
                OpenMode::Extend => self.access_mode == AccessMode::Sequential,
                OpenMode::Input | OpenMode::Output | OpenMode::Io => true,
            },
        };

        if !mode_supported {
            self.last_status = FileStatusKind::OpenPermissionDenied37;
            return PrecheckOutcome::Rejected(FileStatusKind::OpenPermissionDenied37);
        }
        PrecheckOutcome::Allowed
    }

    /// Commits a successful `OPEN` transition with `status` (`Success00` or `SuccessOptionalFile05`).
    ///
    /// # Arguments
    ///
    /// * `mode` - Opened [`OpenMode`].
    /// * `status` - Completion status (`Success00` or `SuccessOptionalFile05`).
    pub const fn commit_open(&mut self, mode: OpenMode, status: FileStatusKind) -> FileStatusCode {
        self.lifecycle = ConnectorLifecycle::Open(mode);
        self.cursor = SequentialCursorState::Positioned;
        self.prior_read = None;
        self.record_status(status)
    }

    /// Validates `CLOSE` legality (`"42"` when not open) and transitions lifecycle state.
    ///
    /// # Arguments
    ///
    /// * `disposition` - Requested [`CloseDisposition`].
    #[must_use]
    pub fn transition_close(&mut self, disposition: CloseDisposition) -> CloseTransitionOutcome {
        let ConnectorLifecycle::Open(active_mode) = self.lifecycle else {
            return CloseTransitionOutcome::NotOpen(
                self.record_status(FileStatusKind::NotOpenOnClose42),
            );
        };
        self.prior_read = None;
        match disposition {
            CloseDisposition::Normal => {
                self.lifecycle = ConnectorLifecycle::Closed;
                self.cursor = SequentialCursorState::Unpositioned;
                CloseTransitionOutcome::ClosedHandle(self.record_status(FileStatusKind::Success00))
            }
            CloseDisposition::WithLock => {
                self.lifecycle = ConnectorLifecycle::ClosedWithLock;
                self.cursor = SequentialCursorState::Unpositioned;
                CloseTransitionOutcome::ClosedHandle(self.record_status(FileStatusKind::Success00))
            }
            CloseDisposition::NoRewind => {
                self.lifecycle = ConnectorLifecycle::Closed;
                self.cursor = SequentialCursorState::Unpositioned;
                let kind = match self.medium {
                    ReelMediumKind::NonReelDisk => FileStatusKind::SuccessNonReel07,
                    ReelMediumKind::ReelTape => FileStatusKind::Success00,
                };
                CloseTransitionOutcome::ClosedHandle(self.record_status(kind))
            }
            CloseDisposition::ReelUnit => {
                self.lifecycle = ConnectorLifecycle::Open(active_mode);
                let kind = match self.medium {
                    ReelMediumKind::NonReelDisk => FileStatusKind::SuccessNonReel07,
                    ReelMediumKind::ReelTape => FileStatusKind::Success00,
                };
                CloseTransitionOutcome::RetainedReelOpen(self.record_status(kind))
            }
        }
    }

    /// Validates open-mode and cursor preconditions for a sequential `READ` (`"47"`, `"46"`).
    #[must_use]
    pub fn precheck_sequential_read(&mut self) -> PrecheckOutcome {
        if self.access_mode == AccessMode::Random {
            self.last_status = FileStatusKind::ReadWrongOpenMode47;
            return PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47);
        }
        match self.lifecycle {
            ConnectorLifecycle::Open(OpenMode::Input | OpenMode::Io) => {}
            ConnectorLifecycle::Closed
            | ConnectorLifecycle::ClosedWithLock
            | ConnectorLifecycle::Open(OpenMode::Output | OpenMode::Extend) => {
                self.last_status = FileStatusKind::ReadWrongOpenMode47;
                return PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47);
            }
        }
        match self.cursor {
            SequentialCursorState::Positioned => PrecheckOutcome::Allowed,
            SequentialCursorState::AtEndLatched | SequentialCursorState::Unpositioned => {
                self.prior_read = None;
                self.last_status = FileStatusKind::ReadBeyondEnd46;
                PrecheckOutcome::Rejected(FileStatusKind::ReadBeyondEnd46)
            }
        }
    }

    /// Validates open-mode and access-mode preconditions for a random keyed `READ` (`"47"`).
    #[must_use]
    pub fn precheck_random_read(&mut self) -> PrecheckOutcome {
        if self.access_mode == AccessMode::Sequential {
            self.last_status = FileStatusKind::ReadWrongOpenMode47;
            return PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47);
        }
        match self.lifecycle {
            ConnectorLifecycle::Open(OpenMode::Input | OpenMode::Io) => PrecheckOutcome::Allowed,
            ConnectorLifecycle::Closed
            | ConnectorLifecycle::ClosedWithLock
            | ConnectorLifecycle::Open(OpenMode::Output | OpenMode::Extend) => {
                self.last_status = FileStatusKind::ReadWrongOpenMode47;
                PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47)
            }
        }
    }

    /// Validates open-mode and access-mode preconditions for `START` (`"47"`).
    #[must_use]
    pub fn precheck_start(&mut self) -> PrecheckOutcome {
        self.prior_read = None;
        if self.access_mode == AccessMode::Random {
            self.last_status = FileStatusKind::ReadWrongOpenMode47;
            return PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47);
        }
        match self.lifecycle {
            ConnectorLifecycle::Open(OpenMode::Input | OpenMode::Io) => PrecheckOutcome::Allowed,
            ConnectorLifecycle::Closed
            | ConnectorLifecycle::ClosedWithLock
            | ConnectorLifecycle::Open(OpenMode::Output | OpenMode::Extend) => {
                self.last_status = FileStatusKind::ReadWrongOpenMode47;
                PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47)
            }
        }
    }

    /// Validates open-mode preconditions for `WRITE` (`"48"`).
    #[must_use]
    pub fn precheck_write(&mut self) -> PrecheckOutcome {
        self.prior_read = None;
        let ConnectorLifecycle::Open(mode) = self.lifecycle else {
            self.last_status = FileStatusKind::WriteWrongOpenMode48;
            return PrecheckOutcome::Rejected(FileStatusKind::WriteWrongOpenMode48);
        };
        let valid = match self.access_mode {
            AccessMode::Sequential => matches!(mode, OpenMode::Output | OpenMode::Extend),
            AccessMode::Random | AccessMode::Dynamic => {
                matches!(mode, OpenMode::Output | OpenMode::Io)
            }
        };
        if valid {
            PrecheckOutcome::Allowed
        } else {
            self.last_status = FileStatusKind::WriteWrongOpenMode48;
            PrecheckOutcome::Rejected(FileStatusKind::WriteWrongOpenMode48)
        }
    }

    /// Validates open-mode (and prior-`READ` in `SEQUENTIAL` access mode) for `REWRITE` / `DELETE`.
    #[must_use]
    pub fn precheck_rewrite_or_delete(&mut self) -> MutationPrecheckOutcome {
        if self.lifecycle != ConnectorLifecycle::Open(OpenMode::Io) {
            self.last_status = FileStatusKind::RewriteDeleteWrongOpenMode49;
            return MutationPrecheckOutcome::Rejected(FileStatusKind::RewriteDeleteWrongOpenMode49);
        }
        if self.access_mode == AccessMode::Sequential {
            let Some(prior) = self.prior_read else {
                self.last_status = FileStatusKind::NoPriorRead43;
                return MutationPrecheckOutcome::Rejected(FileStatusKind::NoPriorRead43);
            };
            MutationPrecheckOutcome::Allowed(Some(prior))
        } else {
            MutationPrecheckOutcome::Allowed(self.prior_read)
        }
    }

    /// Records a successful `READ` and establishes `prior` for subsequent sequential `REWRITE`/`DELETE`.
    ///
    /// # Arguments
    ///
    /// * `prior` - Metadata of the record just read.
    /// * `status` - Completion status (`Success00`, `SuccessDuplicateKey02`, or `SuccessRecordLengthMismatch04`).
    pub const fn commit_read_success(
        &mut self,
        prior: PriorReadRecord,
        status: FileStatusKind,
    ) -> FileStatusCode {
        self.cursor = SequentialCursorState::Positioned;
        self.prior_read = Some(prior);
        self.record_status(status)
    }

    /// Latches `AT END` (`"10"` or `"14"`), clearing prior-read state so another sequential `READ` yields `"46"`.
    ///
    /// # Arguments
    ///
    /// * `at_end_kind` - Either [`FileStatusKind::AtEnd10`] or [`FileStatusKind::RelativeKeyOverflow14`].
    pub const fn latch_at_end(&mut self, at_end_kind: FileStatusKind) -> FileStatusCode {
        self.cursor = SequentialCursorState::AtEndLatched;
        self.prior_read = None;
        self.record_status(at_end_kind)
    }

    /// Marks sequential positioning as `Positioned` (after successful `START`).
    pub const fn commit_start_positioned(&mut self) -> FileStatusCode {
        self.cursor = SequentialCursorState::Positioned;
        self.prior_read = None;
        self.record_status(FileStatusKind::Success00)
    }

    /// Marks sequential positioning as `Unpositioned` after a failed `START` or keyed `READ` (`"23"`).
    ///
    /// # Arguments
    ///
    /// * `failure_kind` - Resulting failure status (typically `RecordNotFound23`).
    pub const fn mark_unpositioned(&mut self, failure_kind: FileStatusKind) -> FileStatusCode {
        self.cursor = SequentialCursorState::Unpositioned;
        self.prior_read = None;
        self.record_status(failure_kind)
    }

    /// Clears `prior_read` after a successful `REWRITE` or `DELETE` and records `status`.
    ///
    /// # Arguments
    ///
    /// * `status` - Completion status (`Success00` or `SuccessDuplicateKey02`).
    pub const fn commit_mutation(&mut self, status: FileStatusKind) -> FileStatusCode {
        self.prior_read = None;
        self.record_status(status)
    }
}
