//! Pure-Rust COBOL `SEQUENTIAL`, `LINE SEQUENTIAL`, and `RELATIVE` file I/O engines,
//! `ASSIGN TO` path resolver, `LINAGE` pagination engine, and exhaustive 28-code `FILE STATUS`
//! transition state machine (`Milestone M14`).

/// `ASSIGN TO` path and environment resolver (`DD_<NAME>`, `dd_<name>`, `<NAME>`, `COB_FILE_PATH`).
pub mod assign;
/// `LINAGE` logical page dimensions, `LINAGE-COUNTER`, and `AT END-OF-PAGE` pagination state.
pub mod linage;
/// `ORGANIZATION IS LINE SEQUENTIAL` text-line file engine (`LF`/`CRLF`, `COB_LS_FIXED`, `COB_LS_NULLS`).
pub mod line_sequential;
/// `ORGANIZATION IS RELATIVE` 1-based slot-addressed file engine (`SEQUENTIAL`/`RANDOM`/`DYNAMIC`, `START`).
pub mod relative;
/// `ORGANIZATION IS SEQUENTIAL` binary fixed/varying file engine (`ADVANCING`, `REWRITE`, `LINAGE`).
pub mod sequential;
/// Exhaustive 28-code `FileStatusCode` transition table, connector state machine, and post-I/O dispatcher.
pub mod status;
/// Pluggable random-access storage traits (`OsFileSystem`, `InMemoryFileSystem`) and `FileLockRegistry`.
pub mod storage;
/// Strongly-typed file organizations, access/open/close modes, record layouts, and 1-based slot keys.
pub mod types;

pub use assign::{
    AssignPrefixPolicy, AssignResolutionConfig, AssignResolutionOrigin, AssignSource, EnvLookup,
    LineSequentialEnvOptions, LiteralEnvPolicy, MapEnv, ProcessEnv, ResolvedAssign,
    ResolvedAssignTarget, StandardDevice, resolve_assign_target,
};
pub use coblang_common::{FileConnectorId, FileStatusCode, FileStatusKind};
pub use linage::{LinagePaginationState, LinageParams, LinageWriteResult};
pub use line_sequential::{LineReadOutcome, LineSequentialConfig, LineSequentialFileEngine};
pub use relative::{
    RelativeFileConfig, RelativeFileEngine, RelativeReadOutcome, RelativeWriteOutcome,
};
pub use sequential::{
    FixedAlignmentPolicy, SequentialFileConfig, SequentialFileEngine, SequentialReadOutcome,
    SequentialWriteOutcome,
};
pub use status::{
    CloseTransitionOutcome, ConnectorLifecycle, ConnectorStateMachine, DeclarativePresence,
    InlineClausePresence, IoErrorContext, IoExceptionClass, MutationPrecheckOutcome,
    PostIoDispatch, PrecheckOutcome, PriorReadRecord, SequentialCursorState, StatusCategory,
    StatusClausePresence, StatusDescriptor, VerbHandlerConfig, classify_io_error,
    classify_storage_error, resolve_post_io_action, status_descriptor, write_status_bytes,
};
pub use storage::{
    FileLockRegistry, FilePresence, InMemoryFileSystem, InMemoryStream, LockOutcome, OsFileSystem,
    RandomAccessStream, StorageBackend, StorageOpenRequest,
};
pub use types::{
    ASCII_CR, ASCII_FF, ASCII_LF, ASCII_NUL, AccessMode, AdvancingPhase, AdvancingSpec,
    CloseDisposition, FileOptionality, FileOrganization, LineTerminator, LockMode,
    NullEscapePolicy, OpenMode, PageOverflowOutcome, ReadDirection, ReadLockPolicy, RecordByteLen,
    RecordLayout, ReelMediumKind, RelativeSlotKey, StartRelation, TrailingSpacePolicy,
    VaryingHeaderFormat,
};
