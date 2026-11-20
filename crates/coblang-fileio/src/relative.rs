//! `ORGANIZATION IS RELATIVE` file engine keyed by 1-based `RELATIVE KEY` with slot-occupancy
//! flags, `SEQUENTIAL` / `RANDOM` / `DYNAMIC` access modes, `START` (`=`, `>`, `>=`, `<`, `<=`),
//! `READ NEXT` / `READ PREVIOUS` / keyed `READ`, `WRITE`, `REWRITE`, `DELETE`, and record locking.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};

use coblang_common::{CoblangError, DigitCount, FileConnectorId, FileStatusCode, FileStatusKind};

use crate::status::{
    CloseTransitionOutcome, ConnectorLifecycle, ConnectorStateMachine, IoErrorContext,
    MutationPrecheckOutcome, PrecheckOutcome, PriorReadRecord, classify_storage_error,
};
use crate::storage::{
    FileLockRegistry, FilePresence, LockOutcome, OsFileSystem, RandomAccessStream, StorageBackend,
    StorageOpenRequest, read_up_to_full,
};
use crate::types::{
    AccessMode, CloseDisposition, FileOptionality, FileOrganization, LockMode, OpenMode,
    ReadDirection, ReadLockPolicy, RecordLayout, ReelMediumKind, RelativeSlotKey, StartRelation,
};

/// Slot-header byte marking an unoccupied or deleted slot (`0x00`).
const SLOT_EMPTY: u8 = 0x00;

/// Slot-header byte marking a live occupied slot (`0x01`).
const SLOT_OCCUPIED: u8 = 0x01;

/// Fixed 5-byte per-slot prefix (`1` occupancy flag byte + `4`-byte big-endian payload length).
const SLOT_HEADER_LEN: usize = 5;

/// Configuration of a [`RelativeFileEngine`].
#[derive(Debug, Clone)]
pub struct RelativeFileConfig {
    /// Logical COBOL file connector name.
    pub file_name: String,
    /// Unique file connector identifier for multi-connector lock coordination.
    pub connector_id: FileConnectorId,
    /// `SELECT [OPTIONAL]` presence policy.
    pub optionality: FileOptionality,
    /// Declared `ACCESS MODE IS {SEQUENTIAL | RANDOM | DYNAMIC}`.
    pub access_mode: AccessMode,
    /// Fixed or variable (`RECORD IS VARYING ... DEPENDING ON`) record layout.
    pub record_layout: RecordLayout,
    /// Optional decimal digit capacity of `RELATIVE KEY` (governs `"14"` on `READ` & `"24"` on `WRITE`).
    pub key_digits: Option<DigitCount>,
    /// Optional hard maximum 1-based slot capacity (`"24"` `BoundaryViolation24` when exceeded).
    pub max_slots: Option<u64>,
    /// Physical medium kind (`NonReelDisk` vs `ReelTape`).
    pub medium: ReelMediumKind,
    /// File and record locking mode (`Automatic`, `Manual`, `Exclusive`).
    pub lock_mode: LockMode,
}

impl RelativeFileConfig {
    /// Creates a standard disk-backed [`RelativeFileConfig`].
    ///
    /// # Arguments
    ///
    /// * `file_name` - Logical COBOL file connector name.
    /// * `connector_id` - Unique [`FileConnectorId`].
    /// * `access_mode` - Declared [`AccessMode`].
    /// * `record_layout` - Fixed or varying [`RecordLayout`].
    #[must_use]
    pub fn new(
        file_name: impl Into<String>,
        connector_id: FileConnectorId,
        access_mode: AccessMode,
        record_layout: RecordLayout,
    ) -> Self {
        Self {
            file_name: file_name.into(),
            connector_id,
            optionality: FileOptionality::Required,
            access_mode,
            record_layout,
            key_digits: None,
            max_slots: None,
            medium: ReelMediumKind::NonReelDisk,
            lock_mode: LockMode::Automatic,
        }
    }

    /// Returns the fixed byte stride of a single slot (`5 + record_layout.max_bytes()`).
    #[must_use]
    pub const fn slot_stride(&self) -> usize {
        SLOT_HEADER_LEN.saturating_add(self.record_layout.max_bytes())
    }
}

/// Outcome returned by sequential or random `READ` on [`RelativeFileEngine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeReadOutcome {
    /// Resulting two-character `FILE STATUS` (`"00"`, `"04"`, `"10"`, `"14"`, `"23"`, `"30"`, `"46"`, `"47"`, `"51"`).
    pub status: FileStatusCode,
    /// One-based `RELATIVE KEY` of the read record (`Some` on `"00"` / `"04"`).
    pub slot_key: Option<RelativeSlotKey>,
    /// Record payload bytes (`Some` on `"00"` / `"04"`).
    pub record: Option<Vec<u8>>,
    /// Active payload byte length (`DEPENDING ON` target value).
    pub actual_len: Option<usize>,
}

/// Outcome returned by sequential or keyed `WRITE` on [`RelativeFileEngine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelativeWriteOutcome {
    /// Resulting two-character `FILE STATUS` (`"00"`, `"22"`, `"24"`, `"30"`, `"44"`, `"48"`, `"51"`).
    pub status: FileStatusCode,
    /// Assigned or confirmed 1-based `RELATIVE KEY` (`Some` on `"00"`).
    pub slot_key: Option<RelativeSlotKey>,
}

/// ISO COBOL File Position Indicator (`FPI`) state for relative sequential traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelativeFpi {
    /// Initial state after `OPEN INPUT` / `OPEN I-O`: `READ NEXT` scans `>= 1`; `READ PREVIOUS` hits `"10"`.
    BeforeFirst,
    /// Positioned directly at `slot` by `START`: immediate `READ NEXT` OR `READ PREVIOUS` reads `slot`.
    AtStartTarget(RelativeSlotKey),
    /// Positioned after reading or deleting `slot`: `READ NEXT` scans `> slot`; `READ PREVIOUS` scans `< slot`.
    AfterSlot(u64),
}

/// Active stream or virtual-absent handle inside [`RelativeSession`].
#[derive(Debug)]
enum ActiveRelativeHandle {
    /// Open random-access slot stream.
    Stream(Box<dyn RandomAccessStream>),
    /// Opened `INPUT` on a non-existent `SELECT OPTIONAL` file (`"05"`).
    OptionalAbsentInput,
}

/// Unified open-session state coupling `path` and `handle`.
#[derive(Debug)]
struct RelativeSession {
    /// Resolved physical path registered in [`FileLockRegistry`].
    path: PathBuf,
    /// Active stream or virtual-absent handle.
    handle: ActiveRelativeHandle,
}

/// Decoded contents of a single physical slot on disk.
#[derive(Debug)]
enum DecodedSlot {
    /// Slot byte 0 is `0x00` (unoccupied or deleted).
    Empty,
    /// Slot byte 0 is `0x01` with stored payload length and raw payload buffer.
    Occupied {
        /// Stored `u32` payload length from bytes `1..5`.
        stored_len: usize,
        /// Full `max_bytes` payload slice from bytes `5..stride`.
        raw_payload: Vec<u8>,
    },
}

/// Internal low-level slot error distinguishing raw [`CoblangError`] from protocol [`FileStatusKind`].
#[derive(Debug)]
enum SlotOpError {
    /// Underlying storage stream error.
    Storage(CoblangError),
    /// Direct status code (`"23"`, `"24"`, `"30"`).
    Status(FileStatusKind),
}

impl From<CoblangError> for SlotOpError {
    /// Wraps [`CoblangError`] into [`SlotOpError::Storage`].
    ///
    /// # Arguments
    ///
    /// * `err` - Source error.
    fn from(err: CoblangError) -> Self {
        Self::Storage(err)
    }
}

impl SlotOpError {
    /// Converts `self` into the canonical [`FileStatusKind`] under `context`.
    ///
    /// # Arguments
    ///
    /// * `context` - Statement I/O context.
    #[must_use]
    fn into_status(self, context: IoErrorContext) -> FileStatusKind {
        match self {
            Self::Storage(err) => classify_storage_error(&err, context),
            Self::Status(kind) => kind,
        }
    }
}

/// Pure-Rust `ORGANIZATION IS RELATIVE` slot-addressed file engine (`Milestone M14`).
#[derive(Debug)]
pub struct RelativeFileEngine {
    /// Connector configuration.
    config: RelativeFileConfig,
    /// Underlying storage backend.
    backend: Box<dyn StorageBackend>,
    /// Shared multi-connector lock coordinator.
    locks: FileLockRegistry,
    /// Connector protocol state machine.
    state: ConnectorStateMachine,
    /// Active open session (`None` iff closed).
    session: Option<RelativeSession>,
    /// Relative File Position Indicator (`FPI`).
    fpi: RelativeFpi,
    /// Next 1-based slot number assigned by sequential `WRITE` (`OPEN OUTPUT` / `OPEN EXTEND`).
    next_sequential_write_slot: u64,
    /// Single-record automatic lock held by this connector (`LockMode::Automatic`).
    auto_locked_slot: Option<u64>,
}

impl RelativeFileEngine {
    /// Constructs a [`RelativeFileEngine`] backed by the host [`OsFileSystem`].
    ///
    /// # Arguments
    ///
    /// * `config` - Connector configuration.
    #[must_use]
    pub fn new(config: RelativeFileConfig) -> Self {
        Self::with_backend(config, Box::new(OsFileSystem), FileLockRegistry::new())
    }

    /// Constructs a [`RelativeFileEngine`] with an explicit boxed [`StorageBackend`] and [`FileLockRegistry`].
    ///
    /// # Arguments
    ///
    /// * `config` - Connector configuration.
    /// * `backend` - Pluggable storage backend.
    /// * `locks` - Shared multi-connector lock registry.
    #[must_use]
    pub fn with_backend(
        config: RelativeFileConfig,
        backend: Box<dyn StorageBackend>,
        locks: FileLockRegistry,
    ) -> Self {
        let state = ConnectorStateMachine::new(
            FileOrganization::Relative,
            config.access_mode,
            config.medium,
        );
        Self {
            config,
            backend,
            locks,
            state,
            session: None,
            fpi: RelativeFpi::BeforeFirst,
            next_sequential_write_slot: 1,
            auto_locked_slot: None,
        }
    }

    /// Returns the most recently recorded [`FileStatusCode`] on this connector.
    #[must_use]
    pub const fn last_status(&self) -> FileStatusCode {
        self.state.last_status_code()
    }

    /// Returns the count of record locks currently held by this connector.
    #[must_use]
    pub fn held_lock_count(&self) -> usize {
        self.session.as_ref().map_or(0, |sess| {
            self.locks
                .held_lock_count(&sess.path, self.config.connector_id)
        })
    }

    /// Releases all record locks currently held by this connector (`UNLOCK file`).
    pub fn unlock_records(&mut self) -> FileStatusCode {
        if let Some(sess) = self.session.as_ref() {
            self.locks
                .release_all_record_locks(&sess.path, self.config.connector_id);
        }
        self.auto_locked_slot = None;
        self.state.record_status(FileStatusKind::Success00)
    }

    /// Executes `OPEN {INPUT | OUTPUT | I-O | EXTEND}` on `path`.
    ///
    /// # Arguments
    ///
    /// * `path` - Resolved filesystem path.
    /// * `mode` - Requested [`OpenMode`].
    pub fn open(&mut self, path: &Path, mode: OpenMode) -> FileStatusCode {
        if let PrecheckOutcome::Rejected(kind) = self.state.precheck_open(mode) {
            return kind.code();
        }
        if let LockOutcome::Denied(kind) =
            self.locks
                .register_open(path, self.config.connector_id, self.config.lock_mode)
        {
            return self.state.record_status(kind);
        }

        let exists = self.backend.path_exists(path);
        if mode == OpenMode::Input
            && exists == FilePresence::Absent
            && self.config.optionality == FileOptionality::Optional
        {
            self.session = Some(RelativeSession {
                path: PathBuf::from(path),
                handle: ActiveRelativeHandle::OptionalAbsentInput,
            });
            self.fpi = RelativeFpi::BeforeFirst;
            self.next_sequential_write_slot = 1;
            self.auto_locked_slot = None;
            return self
                .state
                .commit_open(mode, FileStatusKind::SuccessOptionalFile05);
        }
        if exists == FilePresence::Absent
            && self.config.optionality == FileOptionality::Required
            && mode != OpenMode::Output
        {
            self.locks.unregister_close(path, self.config.connector_id);
            return self.state.record_status(FileStatusKind::FileNotFound35);
        }

        let (req, success_kind) = select_relative_open_request(mode, exists);
        let mut stream = match self.backend.open_stream(path, req) {
            Ok(s) => s,
            Err(err) => {
                self.locks.unregister_close(path, self.config.connector_id);
                let kind = classify_storage_error(&err, IoErrorContext::Open);
                return self.state.record_status(kind);
            }
        };

        let stride = self.config.slot_stride();
        let highest = if mode == OpenMode::Output {
            0_u64
        } else {
            match highest_occupied_slot(stream.as_mut(), stride) {
                Ok(h) => h,
                Err(err) => {
                    self.locks.unregister_close(path, self.config.connector_id);
                    return self
                        .state
                        .record_status(err.into_status(IoErrorContext::Open));
                }
            }
        };

        self.session = Some(RelativeSession {
            path: PathBuf::from(path),
            handle: ActiveRelativeHandle::Stream(stream),
        });
        self.fpi = RelativeFpi::BeforeFirst;
        self.next_sequential_write_slot = highest.saturating_add(1);
        self.auto_locked_slot = None;
        self.state.commit_open(mode, success_kind)
    }

    /// Executes `START file KEY IS relation raw_key` (`=`, `>`, `>=`, `<`, `<=`).
    ///
    /// # Arguments
    ///
    /// * `relation` - Relational operator (`Equal`, `Greater`, `GreaterOrEqual`, `Less`, `LessOrEqual`).
    /// * `raw_key` - Requested 1-based target key (`0` permitted from COBOL program).
    pub fn start(&mut self, relation: StartRelation, raw_key: u64) -> FileStatusCode {
        let precheck = self.state.precheck_start();
        let Some(session) = self.session.as_mut() else {
            return self.state.last_status_code();
        };
        if let PrecheckOutcome::Rejected(kind) = precheck {
            return kind.code();
        }
        let stream = match &mut session.handle {
            ActiveRelativeHandle::OptionalAbsentInput => {
                return self
                    .state
                    .mark_unpositioned(FileStatusKind::RecordNotFound23);
            }
            ActiveRelativeHandle::Stream(s) => s.as_mut(),
        };

        let stride = self.config.slot_stride();
        match find_start_slot(stream, stride, relation, raw_key) {
            Ok(Some(matched_key)) => {
                self.fpi = RelativeFpi::AtStartTarget(matched_key);
                self.state.commit_start_positioned()
            }
            Ok(None) => self
                .state
                .mark_unpositioned(FileStatusKind::RecordNotFound23),
            Err(err) => self
                .state
                .record_status(err.into_status(IoErrorContext::GeneralIo)),
        }
    }

    /// Executes sequential `READ [NEXT | PREVIOUS]` with `lock_policy`.
    ///
    /// # Arguments
    ///
    /// * `direction` - [`ReadDirection::Next`] or [`ReadDirection::Previous`].
    /// * `lock_policy` - Per-statement [`ReadLockPolicy`].
    pub fn read_sequential(
        &mut self,
        direction: ReadDirection,
        lock_policy: ReadLockPolicy,
    ) -> RelativeReadOutcome {
        let precheck = self.state.precheck_sequential_read();
        let lifecycle = self.state.lifecycle();
        let stride = self.config.slot_stride();
        let layout = self.config.record_layout;
        let key_digits = self.config.key_digits;
        let lock_mode = self.config.lock_mode;
        let connector_id = self.config.connector_id;

        let Some(session) = self.session.as_mut() else {
            return empty_read_outcome(self.state.last_status_code());
        };
        if let PrecheckOutcome::Rejected(kind) = precheck {
            return empty_read_outcome(kind.code());
        }
        let stream = match &mut session.handle {
            ActiveRelativeHandle::OptionalAbsentInput => {
                return empty_read_outcome(self.state.latch_at_end(FileStatusKind::AtEnd10));
            }
            ActiveRelativeHandle::Stream(s) => s.as_mut(),
        };

        let candidate = match scan_next_occupied(stream, stride, self.fpi, direction) {
            Ok(Some(found)) => found,
            Ok(None) => {
                return empty_read_outcome(self.state.latch_at_end(FileStatusKind::AtEnd10));
            }
            Err(err) => {
                let kind = err.into_status(IoErrorContext::GeneralIo);
                return empty_read_outcome(self.state.record_status(kind));
            }
        };

        let (slot_key, stored_len, raw_payload) = candidate;
        if let Some(digits) = key_digits
            && !slot_key.fits_digit_count(digits)
        {
            return empty_read_outcome(
                self.state
                    .latch_at_end(FileStatusKind::RelativeKeyOverflow14),
            );
        }

        let slot_num = slot_key.get();
        let lock_req = ReadLockRequest {
            path: &session.path,
            connector: connector_id,
            lock_mode,
            lifecycle,
            policy: lock_policy,
            slot_num,
        };
        if let LockOutcome::Denied(kind) =
            apply_read_locks(&self.locks, &lock_req, &mut self.auto_locked_slot)
        {
            return empty_read_outcome(self.state.record_status(kind));
        }

        self.fpi = RelativeFpi::AfterSlot(slot_num);
        let (delivered, actual_len, status_kind) =
            materialize_slot_payload(layout, stored_len, raw_payload);
        let status = self.state.commit_read_success(
            PriorReadRecord {
                position_key: slot_num,
                payload_len: actual_len,
                framed_len: stride,
            },
            status_kind,
        );
        RelativeReadOutcome {
            status,
            slot_key: Some(slot_key),
            record: Some(delivered),
            actual_len: Some(actual_len),
        }
    }

    /// Executes keyed random `READ` at 1-based `raw_key` (`ACCESS MODE IS RANDOM` or `DYNAMIC`).
    ///
    /// # Arguments
    ///
    /// * `raw_key` - One-based `RELATIVE KEY` value (`0` or unoccupied returns `"23"`).
    /// * `lock_policy` - Per-statement [`ReadLockPolicy`].
    pub fn read_random(
        &mut self,
        raw_key: u64,
        lock_policy: ReadLockPolicy,
    ) -> RelativeReadOutcome {
        let precheck = self.state.precheck_random_read();
        let lifecycle = self.state.lifecycle();
        let stride = self.config.slot_stride();
        let layout = self.config.record_layout;
        let lock_mode = self.config.lock_mode;
        let connector_id = self.config.connector_id;

        let Some(session) = self.session.as_mut() else {
            return empty_read_outcome(self.state.last_status_code());
        };
        if let PrecheckOutcome::Rejected(kind) = precheck {
            return empty_read_outcome(kind.code());
        }
        let Ok(slot_key) = RelativeSlotKey::try_new(raw_key) else {
            return empty_read_outcome(
                self.state
                    .mark_unpositioned(FileStatusKind::RecordNotFound23),
            );
        };
        let stream = match &mut session.handle {
            ActiveRelativeHandle::OptionalAbsentInput => {
                return empty_read_outcome(
                    self.state
                        .mark_unpositioned(FileStatusKind::RecordNotFound23),
                );
            }
            ActiveRelativeHandle::Stream(s) => s.as_mut(),
        };

        let (stored_len, raw_payload) =
            match read_occupied_slot_or_not_found(stream, stride, raw_key) {
                Ok(Some(pair)) => pair,
                Ok(None) => {
                    return empty_read_outcome(
                        self.state
                            .mark_unpositioned(FileStatusKind::RecordNotFound23),
                    );
                }
                Err(err) => {
                    let kind = err.into_status(IoErrorContext::GeneralIo);
                    return empty_read_outcome(self.state.record_status(kind));
                }
            };

        let lock_req = ReadLockRequest {
            path: &session.path,
            connector: connector_id,
            lock_mode,
            lifecycle,
            policy: lock_policy,
            slot_num: raw_key,
        };
        if let LockOutcome::Denied(kind) =
            apply_read_locks(&self.locks, &lock_req, &mut self.auto_locked_slot)
        {
            return empty_read_outcome(self.state.record_status(kind));
        }

        self.fpi = RelativeFpi::AfterSlot(raw_key);
        let (delivered, actual_len, status_kind) =
            materialize_slot_payload(layout, stored_len, raw_payload);
        let status = self.state.commit_read_success(
            PriorReadRecord {
                position_key: raw_key,
                payload_len: actual_len,
                framed_len: stride,
            },
            status_kind,
        );
        RelativeReadOutcome {
            status,
            slot_key: Some(slot_key),
            record: Some(delivered),
            actual_len: Some(actual_len),
        }
    }

    /// Executes sequential `WRITE` (`ACCESS MODE IS SEQUENTIAL`, `OPEN OUTPUT` / `EXTEND`),
    /// auto-assigning the next 1-based `RELATIVE KEY`.
    ///
    /// # Arguments
    ///
    /// * `record` - Record payload bytes.
    pub fn write_sequential(&mut self, record: &[u8]) -> RelativeWriteOutcome {
        if self.config.access_mode != AccessMode::Sequential {
            return RelativeWriteOutcome {
                status: self
                    .state
                    .record_status(FileStatusKind::WriteWrongOpenMode48),
                slot_key: None,
            };
        }
        let precheck = self.state.precheck_write();
        let stride = self.config.slot_stride();
        let max_bytes = self.config.record_layout.max_bytes();
        let accepts_len = self.config.record_layout.accepts_write_len(record.len());
        let target_raw = self.next_sequential_write_slot;
        let valid_slot = self.validate_slot_within_bounds(target_raw);

        let Some(RelativeSession {
            handle: ActiveRelativeHandle::Stream(stream),
            ..
        }) = self.session.as_mut()
        else {
            return RelativeWriteOutcome {
                status: self.state.last_status_code(),
                slot_key: None,
            };
        };
        if let PrecheckOutcome::Rejected(kind) = precheck {
            return RelativeWriteOutcome {
                status: kind.code(),
                slot_key: None,
            };
        }
        if !accepts_len {
            return RelativeWriteOutcome {
                status: self
                    .state
                    .record_status(FileStatusKind::RewriteLengthMismatch44),
                slot_key: None,
            };
        }
        let Some(slot_key) = valid_slot else {
            return RelativeWriteOutcome {
                status: self
                    .state
                    .record_status(FileStatusKind::BoundaryViolation24),
                slot_key: None,
            };
        };

        match write_occupied_slot(stream.as_mut(), stride, max_bytes, target_raw, record) {
            Ok(()) => {
                self.next_sequential_write_slot = target_raw.saturating_add(1);
                RelativeWriteOutcome {
                    status: self.state.record_status(FileStatusKind::Success00),
                    slot_key: Some(slot_key),
                }
            }
            Err(err) => RelativeWriteOutcome {
                status: self
                    .state
                    .record_status(err.into_status(IoErrorContext::KeyedWrite)),
                slot_key: None,
            },
        }
    }

    /// Executes keyed `WRITE` at 1-based `raw_key` (`ACCESS MODE IS RANDOM` or `DYNAMIC`).
    ///
    /// # Arguments
    ///
    /// * `raw_key` - Target 1-based `RELATIVE KEY`.
    /// * `record` - Record payload bytes.
    pub fn write_keyed(&mut self, raw_key: u64, record: &[u8]) -> RelativeWriteOutcome {
        if self.config.access_mode == AccessMode::Sequential {
            return RelativeWriteOutcome {
                status: self
                    .state
                    .record_status(FileStatusKind::WriteWrongOpenMode48),
                slot_key: None,
            };
        }
        let precheck = self.state.precheck_write();
        let stride = self.config.slot_stride();
        let max_bytes = self.config.record_layout.max_bytes();
        let accepts_len = self.config.record_layout.accepts_write_len(record.len());
        let valid_slot = self.validate_slot_within_bounds(raw_key);
        let connector_id = self.config.connector_id;

        let Some(RelativeSession {
            path,
            handle: ActiveRelativeHandle::Stream(stream),
        }) = self.session.as_mut()
        else {
            return RelativeWriteOutcome {
                status: self.state.last_status_code(),
                slot_key: None,
            };
        };
        if let PrecheckOutcome::Rejected(kind) = precheck {
            return RelativeWriteOutcome {
                status: kind.code(),
                slot_key: None,
            };
        }
        if !accepts_len {
            return RelativeWriteOutcome {
                status: self
                    .state
                    .record_status(FileStatusKind::RewriteLengthMismatch44),
                slot_key: None,
            };
        }
        let Some(slot_key) = valid_slot else {
            return RelativeWriteOutcome {
                status: self
                    .state
                    .record_status(FileStatusKind::BoundaryViolation24),
                slot_key: None,
            };
        };
        if let LockOutcome::Denied(kind) =
            self.locks
                .check_not_locked_by_other(path, connector_id, raw_key)
        {
            return RelativeWriteOutcome {
                status: self.state.record_status(kind),
                slot_key: None,
            };
        }

        match write_new_keyed_slot(stream.as_mut(), stride, max_bytes, raw_key, record) {
            Ok(()) => RelativeWriteOutcome {
                status: self.state.record_status(FileStatusKind::Success00),
                slot_key: Some(slot_key),
            },
            Err(err) => RelativeWriteOutcome {
                status: self
                    .state
                    .record_status(err.into_status(IoErrorContext::KeyedWrite)),
                slot_key: None,
            },
        }
    }

    /// Executes `REWRITE` in `SEQUENTIAL`, `RANDOM`, or `DYNAMIC` access mode.
    ///
    /// # Arguments
    ///
    /// * `raw_key` - Target 1-based `RELATIVE KEY` (used when `access_mode` is `Random` or `Dynamic`).
    /// * `record` - Replacement record payload bytes.
    pub fn rewrite_record(&mut self, raw_key: Option<u64>, record: &[u8]) -> FileStatusCode {
        let precheck = self.state.precheck_rewrite_or_delete();
        let stride = self.config.slot_stride();
        let max_bytes = self.config.record_layout.max_bytes();
        let accepts_len = self.config.record_layout.accepts_write_len(record.len());
        let access_mode = self.config.access_mode;
        let connector_id = self.config.connector_id;

        let Some(RelativeSession {
            path,
            handle: ActiveRelativeHandle::Stream(stream),
        }) = self.session.as_mut()
        else {
            return self.state.last_status_code();
        };
        let prior = match precheck {
            MutationPrecheckOutcome::Allowed(p) => p,
            MutationPrecheckOutcome::Rejected(kind) => return kind.code(),
        };
        if !accepts_len {
            return self
                .state
                .record_status(FileStatusKind::RewriteLengthMismatch44);
        }
        let Some(target_slot) = resolve_mutation_slot(access_mode, prior, raw_key) else {
            return self.state.record_status(FileStatusKind::RecordNotFound23);
        };
        if let LockOutcome::Denied(kind) =
            self.locks
                .check_not_locked_by_other(path, connector_id, target_slot)
        {
            return self.state.record_status(kind);
        }

        match overwrite_existing_slot(stream.as_mut(), stride, max_bytes, target_slot, record) {
            Ok(()) => {
                if self.auto_locked_slot == Some(target_slot) {
                    self.locks
                        .release_single_lock(path, connector_id, target_slot);
                    self.auto_locked_slot = None;
                }
                self.state.commit_mutation(FileStatusKind::Success00)
            }
            Err(err) => self
                .state
                .record_status(err.into_status(IoErrorContext::GeneralIo)),
        }
    }

    /// Executes `DELETE` in `SEQUENTIAL`, `RANDOM`, or `DYNAMIC` access mode.
    ///
    /// # Arguments
    ///
    /// * `raw_key` - Target 1-based `RELATIVE KEY` (used when `access_mode` is `Random` or `Dynamic`).
    pub fn delete_record(&mut self, raw_key: Option<u64>) -> FileStatusCode {
        let precheck = self.state.precheck_rewrite_or_delete();
        let stride = self.config.slot_stride();
        let access_mode = self.config.access_mode;
        let connector_id = self.config.connector_id;

        let Some(RelativeSession {
            path,
            handle: ActiveRelativeHandle::Stream(stream),
        }) = self.session.as_mut()
        else {
            return self.state.last_status_code();
        };
        let prior = match precheck {
            MutationPrecheckOutcome::Allowed(p) => p,
            MutationPrecheckOutcome::Rejected(kind) => return kind.code(),
        };
        let Some(target_slot) = resolve_mutation_slot(access_mode, prior, raw_key) else {
            return self.state.record_status(FileStatusKind::RecordNotFound23);
        };
        if let LockOutcome::Denied(kind) =
            self.locks
                .check_not_locked_by_other(path, connector_id, target_slot)
        {
            return self.state.record_status(kind);
        }

        match delete_existing_slot(stream.as_mut(), stride, target_slot) {
            Ok(()) => {
                self.locks
                    .release_single_lock(path, connector_id, target_slot);
                if self.auto_locked_slot == Some(target_slot) {
                    self.auto_locked_slot = None;
                }
                self.state.commit_mutation(FileStatusKind::Success00)
            }
            Err(err) => self
                .state
                .record_status(err.into_status(IoErrorContext::GeneralIo)),
        }
    }

    /// Executes `CLOSE` with the given [`CloseDisposition`].
    ///
    /// # Arguments
    ///
    /// * `disposition` - Requested [`CloseDisposition`].
    pub fn close(&mut self, disposition: CloseDisposition) -> FileStatusCode {
        if disposition == CloseDisposition::ReelUnit {
            return match self.state.transition_close(disposition) {
                CloseTransitionOutcome::NotOpen(code)
                | CloseTransitionOutcome::RetainedReelOpen(code)
                | CloseTransitionOutcome::ClosedHandle(code) => code,
            };
        }

        let transition = self.state.transition_close(disposition);
        let Some(session) = self.session.take() else {
            return self.state.last_status_code();
        };
        let status = match transition {
            CloseTransitionOutcome::ClosedHandle(code)
            | CloseTransitionOutcome::NotOpen(code)
            | CloseTransitionOutcome::RetainedReelOpen(code) => code,
        };

        let flush_err = if let ActiveRelativeHandle::Stream(mut stream) = session.handle {
            stream.flush_bytes().err()
        } else {
            None
        };

        self.auto_locked_slot = None;
        self.locks
            .unregister_close(&session.path, self.config.connector_id);

        if let Some(err) = flush_err {
            let kind = classify_storage_error(&err, IoErrorContext::GeneralIo);
            return self.state.record_status(kind);
        }
        status
    }

    /// Checks `raw_key >= 1`, `key_digits`, and `max_slots` capacity constraints.
    ///
    /// # Arguments
    ///
    /// * `raw_key` - Candidate 1-based slot number.
    #[must_use]
    fn validate_slot_within_bounds(&self, raw_key: u64) -> Option<RelativeSlotKey> {
        let key = RelativeSlotKey::try_new(raw_key).ok()?;
        if let Some(digits) = self.config.key_digits
            && !key.fits_digit_count(digits)
        {
            return None;
        }
        if let Some(limit) = self.config.max_slots
            && raw_key > limit
        {
            return None;
        }
        Some(key)
    }
}

/// Resolves target 1-based slot for `REWRITE` or `DELETE` (`None` when random/dynamic key is `None` or `0`).
///
/// # Arguments
///
/// * `access_mode` - Connector [`AccessMode`].
/// * `prior` - Validated prior-read record (guaranteed `Some` when `access_mode == Sequential`).
/// * `raw_key` - Caller-supplied key for `Random`/`Dynamic`.
#[must_use]
fn resolve_mutation_slot(
    access_mode: AccessMode,
    prior: Option<PriorReadRecord>,
    raw_key: Option<u64>,
) -> Option<u64> {
    match access_mode {
        AccessMode::Sequential => prior.map(|p| p.position_key),
        AccessMode::Random | AccessMode::Dynamic => raw_key.filter(|&k| k >= 1),
    }
}

/// Maps validated relative `(mode, exists)` to `(StorageOpenRequest, FileStatusKind)`.
///
/// # Arguments
///
/// * `mode` - Validated [`OpenMode`].
/// * `exists` - Pre-checked [`FilePresence`].
#[must_use]
const fn select_relative_open_request(
    mode: OpenMode,
    exists: FilePresence,
) -> (StorageOpenRequest, FileStatusKind) {
    match mode {
        OpenMode::Input => (StorageOpenRequest::ReadOnly, FileStatusKind::Success00),
        OpenMode::Output => (
            StorageOpenRequest::CreateTruncateWrite,
            FileStatusKind::Success00,
        ),
        OpenMode::Io | OpenMode::Extend => match exists {
            FilePresence::Present => (
                StorageOpenRequest::ReadWriteExisting,
                FileStatusKind::Success00,
            ),
            FilePresence::Absent => (
                StorageOpenRequest::ReadWriteCreate,
                FileStatusKind::SuccessOptionalFile05,
            ),
        },
    }
}

/// Builds a failed/empty [`RelativeReadOutcome`] with `status`.
///
/// # Arguments
///
/// * `status` - Two-byte [`FileStatusCode`].
#[must_use]
const fn empty_read_outcome(status: FileStatusCode) -> RelativeReadOutcome {
    RelativeReadOutcome {
        status,
        slot_key: None,
        record: None,
        actual_len: None,
    }
}

/// Computes the total physical slot count (`file_len / stride`), failing with `"30"` if misaligned.
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
fn total_slot_count(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
) -> Result<u64, SlotOpError> {
    let end = stream.seek_pos(SeekFrom::End(0))?;
    let stride_u64 = stride as u64;
    if end % stride_u64 != 0 {
        return Err(SlotOpError::Status(FileStatusKind::PermanentError30));
    }
    Ok(end / stride_u64)
}

/// Reads and validates slot `slot_num` (`1..=total_slots`) from `stream`.
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
/// * `slot_num` - One-based slot index.
fn read_slot(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    slot_num: u64,
) -> Result<DecodedSlot, SlotOpError> {
    let offset = (slot_num - 1)
        .checked_mul(stride as u64)
        .ok_or(SlotOpError::Status(FileStatusKind::PermanentError30))?;
    stream.seek_pos(SeekFrom::Start(offset))?;
    let mut raw = vec![0_u8; stride];
    let _ = read_up_to_full(stream, &mut raw)?;
    let flag = raw.first().copied().unwrap_or(SLOT_EMPTY);
    match flag {
        SLOT_EMPTY => Ok(DecodedSlot::Empty),
        SLOT_OCCUPIED => {
            let len_bytes: [u8; 4] = [
                raw.get(1).copied().unwrap_or(0),
                raw.get(2).copied().unwrap_or(0),
                raw.get(3).copied().unwrap_or(0),
                raw.get(4).copied().unwrap_or(0),
            ];
            let stored_len = u32::from_be_bytes(len_bytes) as usize;
            let payload = raw.get(SLOT_HEADER_LEN..).unwrap_or(&[]).to_vec();
            Ok(DecodedSlot::Occupied {
                stored_len,
                raw_payload: payload,
            })
        }
        _ => Err(SlotOpError::Status(FileStatusKind::PermanentError30)),
    }
}

/// Reads an occupied slot `raw_key` or returns `Ok(None)` when `raw_key > total` or `Empty`.
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride.
/// * `raw_key` - One-based target slot (`>= 1`).
fn read_occupied_slot_or_not_found(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    raw_key: u64,
) -> Result<Option<(usize, Vec<u8>)>, SlotOpError> {
    let total = total_slot_count(stream, stride)?;
    if raw_key > total {
        return Ok(None);
    }
    match read_slot(stream, stride, raw_key)? {
        DecodedSlot::Empty => Ok(None),
        DecodedSlot::Occupied {
            stored_len,
            raw_payload,
        } => Ok(Some((stored_len, raw_payload))),
    }
}

/// Scans `stream` for the highest 1-based occupied slot (`0` if empty).
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
fn highest_occupied_slot(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
) -> Result<u64, SlotOpError> {
    let total = total_slot_count(stream, stride)?;
    let mut cur = total;
    while cur >= 1 {
        if let DecodedSlot::Occupied { .. } = read_slot(stream, stride, cur)? {
            return Ok(cur);
        }
        cur -= 1;
    }
    Ok(0)
}

/// Finds the ISO COBOL `START` target slot for `relation` and `raw_key`.
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
/// * `relation` - Relational operator.
/// * `raw_key` - Requested key.
fn find_start_slot(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    relation: StartRelation,
    raw_key: u64,
) -> Result<Option<RelativeSlotKey>, SlotOpError> {
    let total = total_slot_count(stream, stride)?;
    match relation {
        StartRelation::Equal => {
            if raw_key == 0 || raw_key > total {
                return Ok(None);
            }
            if let DecodedSlot::Occupied { .. } = read_slot(stream, stride, raw_key)? {
                Ok(RelativeSlotKey::try_new(raw_key).ok())
            } else {
                Ok(None)
            }
        }
        StartRelation::Greater | StartRelation::GreaterOrEqual => {
            let mut s = if relation == StartRelation::Greater {
                raw_key.saturating_add(1)
            } else {
                raw_key.max(1)
            };
            while s <= total {
                if let DecodedSlot::Occupied { .. } = read_slot(stream, stride, s)? {
                    return Ok(RelativeSlotKey::try_new(s).ok());
                }
                s += 1;
            }
            Ok(None)
        }
        StartRelation::Less | StartRelation::LessOrEqual => {
            let upper = if relation == StartRelation::Less {
                raw_key.saturating_sub(1)
            } else {
                raw_key
            };
            let mut s = upper.min(total);
            while s >= 1 {
                if let DecodedSlot::Occupied { .. } = read_slot(stream, stride, s)? {
                    return Ok(RelativeSlotKey::try_new(s).ok());
                }
                s -= 1;
            }
            Ok(None)
        }
    }
}

/// Scans forward (`Next`) or backward (`Previous`) from `fpi` for the next occupied slot.
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
/// * `fpi` - Current [`RelativeFpi`].
/// * `direction` - Traversal [`ReadDirection`].
fn scan_next_occupied(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    fpi: RelativeFpi,
    direction: ReadDirection,
) -> Result<Option<(RelativeSlotKey, usize, Vec<u8>)>, SlotOpError> {
    let total = total_slot_count(stream, stride)?;
    match (fpi, direction) {
        (RelativeFpi::BeforeFirst, ReadDirection::Previous) => Ok(None),
        (RelativeFpi::AtStartTarget(key), _) => {
            if let DecodedSlot::Occupied {
                stored_len,
                raw_payload,
            } = read_slot(stream, stride, key.get())?
            {
                return Ok(Some((key, stored_len, raw_payload)));
            }
            scan_after_slot(stream, stride, total, key.get(), direction)
        }
        (RelativeFpi::BeforeFirst, ReadDirection::Next) => {
            scan_after_slot(stream, stride, total, 0, direction)
        }
        (RelativeFpi::AfterSlot(cur), _) => scan_after_slot(stream, stride, total, cur, direction),
    }
}

/// Scans strictly after `cur` (`Next`) or strictly before `cur` (`Previous`).
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
/// * `total` - Total physical slot count.
/// * `cur` - Current 1-based anchor slot (`0` when scanning from before slot 1).
/// * `direction` - Traversal [`ReadDirection`].
fn scan_after_slot(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    total: u64,
    cur: u64,
    direction: ReadDirection,
) -> Result<Option<(RelativeSlotKey, usize, Vec<u8>)>, SlotOpError> {
    match direction {
        ReadDirection::Next => {
            let mut s = cur.saturating_add(1);
            while s <= total {
                if let DecodedSlot::Occupied {
                    stored_len,
                    raw_payload,
                } = read_slot(stream, stride, s)?
                {
                    return Ok(RelativeSlotKey::try_new(s)
                        .ok()
                        .map(|k| (k, stored_len, raw_payload)));
                }
                s += 1;
            }
            Ok(None)
        }
        ReadDirection::Previous => {
            let mut s = cur.saturating_sub(1).min(total);
            while s >= 1 {
                if let DecodedSlot::Occupied {
                    stored_len,
                    raw_payload,
                } = read_slot(stream, stride, s)?
                {
                    return Ok(RelativeSlotKey::try_new(s)
                        .ok()
                        .map(|k| (k, stored_len, raw_payload)));
                }
                s -= 1;
            }
            Ok(None)
        }
    }
}

/// Bundles parameters for [`apply_read_locks`].
struct ReadLockRequest<'a> {
    /// Physical path of open file.
    path: &'a Path,
    /// Connector identifier.
    connector: FileConnectorId,
    /// Connector [`LockMode`].
    lock_mode: LockMode,
    /// Connector [`ConnectorLifecycle`].
    lifecycle: ConnectorLifecycle,
    /// Statement [`ReadLockPolicy`].
    policy: ReadLockPolicy,
    /// One-based slot being read.
    slot_num: u64,
}

/// Applies record-lock checks (`"51"`) and updates automatic/manual/kept locks on `READ`.
///
/// # Arguments
///
/// * `locks` - Shared [`FileLockRegistry`].
/// * `req` - [`ReadLockRequest`] context.
/// * `auto_locked_slot` - Mutable tracker for automatic single-record lock release.
fn apply_read_locks(
    locks: &FileLockRegistry,
    req: &ReadLockRequest<'_>,
    auto_locked_slot: &mut Option<u64>,
) -> LockOutcome {
    if req.policy == ReadLockPolicy::WithNoLock {
        if let Some(prev) = auto_locked_slot.take() {
            locks.release_single_lock(req.path, req.connector, prev);
        }
        return LockOutcome::Granted;
    }

    if let LockOutcome::Denied(kind) =
        locks.check_not_locked_by_other(req.path, req.connector, req.slot_num)
    {
        return LockOutcome::Denied(kind);
    }

    if req.policy != ReadLockPolicy::WithKeptLock
        && let Some(prev) = auto_locked_slot.take()
        && prev != req.slot_num
    {
        locks.release_single_lock(req.path, req.connector, prev);
    }

    let in_io_mode = req.lifecycle == ConnectorLifecycle::Open(OpenMode::Io);
    let explicit_lock = matches!(
        req.policy,
        ReadLockPolicy::WithLock | ReadLockPolicy::WithKeptLock
    );
    let should_acquire = in_io_mode && (explicit_lock || req.lock_mode == LockMode::Automatic);

    if should_acquire {
        let _ = locks.acquire_record_lock(req.path, req.connector, req.slot_num);
        if req.policy != ReadLockPolicy::WithKeptLock {
            *auto_locked_slot = Some(req.slot_num);
        }
    }
    LockOutcome::Granted
}

/// Validates and slices/pads the payload read from an occupied slot (`"00"` vs `"04"`).
///
/// # Arguments
///
/// * `layout` - Connector [`RecordLayout`].
/// * `stored_len` - Stored `u32` payload length in slot header.
/// * `raw_payload` - Full `max_bytes` payload buffer from slot.
#[must_use]
fn materialize_slot_payload(
    layout: RecordLayout,
    stored_len: usize,
    mut raw_payload: Vec<u8>,
) -> (Vec<u8>, usize, FileStatusKind) {
    let min_b = layout.min_bytes();
    let max_b = layout.max_bytes();
    let in_bounds = stored_len >= min_b && stored_len <= max_b;
    let status = if in_bounds {
        FileStatusKind::Success00
    } else {
        FileStatusKind::SuccessRecordLengthMismatch04
    };
    let clamped = stored_len.min(max_b);
    raw_payload.truncate(clamped);
    (raw_payload, clamped, status)
}

/// Verifies `raw_key` is unoccupied (else `"22"`) and writes `record` at `raw_key`.
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
/// * `max_bytes` - Maximum record payload capacity.
/// * `raw_key` - Target 1-based slot.
/// * `record` - Record payload bytes.
fn write_new_keyed_slot(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    max_bytes: usize,
    raw_key: u64,
    record: &[u8],
) -> Result<(), SlotOpError> {
    let total = total_slot_count(stream, stride)?;
    if raw_key <= total
        && let DecodedSlot::Occupied { .. } = read_slot(stream, stride, raw_key)?
    {
        return Err(SlotOpError::Status(FileStatusKind::DuplicateKey22));
    }
    write_occupied_slot(stream, stride, max_bytes, raw_key, record)
}

/// Verifies `target_slot` is occupied (else `"23"`) and overwrites it with `record`.
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
/// * `max_bytes` - Maximum record payload capacity.
/// * `target_slot` - Target 1-based slot.
/// * `record` - Replacement record payload bytes.
fn overwrite_existing_slot(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    max_bytes: usize,
    target_slot: u64,
    record: &[u8],
) -> Result<(), SlotOpError> {
    if read_occupied_slot_or_not_found(stream, stride, target_slot)?.is_none() {
        return Err(SlotOpError::Status(FileStatusKind::RecordNotFound23));
    }
    write_occupied_slot(stream, stride, max_bytes, target_slot, record)
}

/// Verifies `target_slot` is occupied (else `"23"`) and clears it to `SLOT_EMPTY`.
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
/// * `target_slot` - Target 1-based slot.
fn delete_existing_slot(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    target_slot: u64,
) -> Result<(), SlotOpError> {
    if read_occupied_slot_or_not_found(stream, stride, target_slot)?.is_none() {
        return Err(SlotOpError::Status(FileStatusKind::RecordNotFound23));
    }
    clear_slot(stream, stride, target_slot)
}

/// Writes an occupied slot at 1-based `slot_num`, zero-filling any sparse intermediate empty slots.
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
/// * `max_bytes` - Maximum record payload capacity.
/// * `slot_num` - One-based slot index (`>= 1`).
/// * `record` - Validated record payload bytes (`<= max_bytes`).
fn write_occupied_slot(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    max_bytes: usize,
    slot_num: u64,
    record: &[u8],
) -> Result<(), SlotOpError> {
    let total = total_slot_count(stream, stride)?;
    if slot_num > total + 1 {
        let end_off = total
            .checked_mul(stride as u64)
            .ok_or(SlotOpError::Status(FileStatusKind::BoundaryViolation24))?;
        stream.seek_pos(SeekFrom::Start(end_off))?;
        let empty_slot = vec![SLOT_EMPTY; stride];
        let gap_count = slot_num - total - 1;
        for _ in 0..gap_count {
            stream.write_bytes(&empty_slot)?;
        }
    }

    let offset = (slot_num - 1)
        .checked_mul(stride as u64)
        .ok_or(SlotOpError::Status(FileStatusKind::BoundaryViolation24))?;
    stream.seek_pos(SeekFrom::Start(offset))?;

    let copy_len = record.len().min(max_bytes);
    let len_u32 = u32::try_from(copy_len).unwrap_or(u32::MAX);
    let mut slot_buf = Vec::with_capacity(stride);
    slot_buf.push(SLOT_OCCUPIED);
    slot_buf.extend_from_slice(&len_u32.to_be_bytes());
    slot_buf.extend_from_slice(record.get(..copy_len).unwrap_or(record));
    slot_buf.resize(stride, 0_u8);

    stream.write_bytes(&slot_buf)?;
    Ok(())
}

/// Marks 1-based `slot_num` as `SLOT_EMPTY` (`0x00` + zeroed bytes).
///
/// # Arguments
///
/// * `stream` - Open slot stream.
/// * `stride` - Fixed slot stride in bytes.
/// * `slot_num` - One-based slot index (`>= 1`).
fn clear_slot(
    stream: &mut dyn RandomAccessStream,
    stride: usize,
    slot_num: u64,
) -> Result<(), SlotOpError> {
    let offset = (slot_num - 1)
        .checked_mul(stride as u64)
        .ok_or(SlotOpError::Status(FileStatusKind::PermanentError30))?;
    stream.seek_pos(SeekFrom::Start(offset))?;
    let empty_buf = vec![SLOT_EMPTY; stride];
    stream.write_bytes(&empty_buf)?;
    Ok(())
}
