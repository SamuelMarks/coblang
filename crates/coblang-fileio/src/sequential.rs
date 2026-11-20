//! `ORGANIZATION IS SEQUENTIAL` file engine supporting fixed and variable (`RECORD IS VARYING ...
//! DEPENDING ON`) records, `OPEN INPUT/OUTPUT/I-O/EXTEND`, `OPTIONAL` `"05"` status, `READ NEXT`,
//! `WRITE BEFORE/AFTER ADVANCING`, `LinagePaginationState`, `REWRITE`, and `CLOSE WITH LOCK`.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};

use coblang_common::{FileConnectorId, FileStatusCode, FileStatusKind};

use crate::linage::{LinagePaginationState, LinageParams};
use crate::status::{
    CloseTransitionOutcome, ConnectorStateMachine, IoErrorContext, MutationPrecheckOutcome,
    PrecheckOutcome, PriorReadRecord, classify_storage_error,
};
use crate::storage::{
    FileLockRegistry, FilePresence, LockOutcome, OsFileSystem, RandomAccessStream, StorageBackend,
    StorageOpenRequest, read_up_to_full,
};
use crate::types::{
    ASCII_CR, ASCII_FF, ASCII_LF, AccessMode, AdvancingPhase, AdvancingSpec, CloseDisposition,
    FileOptionality, FileOrganization, LockMode, OpenMode, PageOverflowOutcome, RecordByteLen,
    RecordLayout, ReelMediumKind, VaryingHeaderFormat,
};

/// Policy governing how fixed-length `SEQUENTIAL` files handle non-multiple existing file sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixedAlignmentPolicy {
    /// Allow opening and pad any trailing short record at EOF with status `"04"`.
    AllowShortFinalRecord04,
    /// Reject at `OPEN INPUT` / `OPEN I-O` with `"39"` (`AttributeConflict39`) if file size is not a multiple of `record_len`.
    RequireExactMultiple39,
}

/// Static and dynamic configuration of a [`SequentialFileEngine`].
#[derive(Debug, Clone)]
pub struct SequentialFileConfig {
    /// Logical COBOL file connector name (e.g., `"IN-FILE"`).
    pub file_name: String,
    /// Unique file connector identifier for multi-connector lock coordination.
    pub connector_id: FileConnectorId,
    /// `SELECT [OPTIONAL]` presence policy.
    pub optionality: FileOptionality,
    /// Fixed or variable FD record size specification.
    pub record_layout: RecordLayout,
    /// Fixed-size alignment check policy (`"04"` vs `"39"`).
    pub alignment_policy: FixedAlignmentPolicy,
    /// Physical medium kind (`NonReelDisk` vs `ReelTape`).
    pub medium: ReelMediumKind,
    /// Optional `LINAGE` logical page configuration.
    pub linage: Option<LinageParams>,
    /// File-sharing lock mode (`Automatic`, `Manual`, `Exclusive`).
    pub lock_mode: LockMode,
}

impl SequentialFileConfig {
    /// Creates a standard disk-backed [`SequentialFileConfig`] with sensible defaults.
    ///
    /// # Arguments
    ///
    /// * `file_name` - Logical COBOL file connector name.
    /// * `connector_id` - Unique [`FileConnectorId`].
    /// * `record_layout` - Fixed or varying [`RecordLayout`].
    #[must_use]
    pub fn new(
        file_name: impl Into<String>,
        connector_id: FileConnectorId,
        record_layout: RecordLayout,
    ) -> Self {
        Self {
            file_name: file_name.into(),
            connector_id,
            optionality: FileOptionality::Required,
            record_layout,
            alignment_policy: FixedAlignmentPolicy::AllowShortFinalRecord04,
            medium: ReelMediumKind::NonReelDisk,
            linage: None,
            lock_mode: LockMode::Automatic,
        }
    }
}

/// Outcome returned by [`SequentialFileEngine::read_next`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequentialReadOutcome {
    /// Resulting two-character `FILE STATUS` (`"00"`, `"04"`, `"10"`, `"30"`, `"46"`, `"47"`).
    pub status: FileStatusCode,
    /// Record payload delivered into the FD record area (`None` on `AT END` or error).
    pub record: Option<Vec<u8>>,
    /// Actual logical byte length read (`DEPENDING ON` target value when `Some`).
    pub actual_len: Option<usize>,
}

/// Outcome returned by [`SequentialFileEngine::write_record`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequentialWriteOutcome {
    /// Resulting two-character `FILE STATUS` (`"00"`, `"34"`, `"44"`, `"48"`).
    pub status: FileStatusCode,
    /// `AT END-OF-PAGE` (`EOP`) outcome when `LINAGE` is active (`WithinPage` otherwise).
    pub page_overflow: PageOverflowOutcome,
    /// Updated 1-based `LINAGE-COUNTER` when `LINAGE` is active.
    pub linage_counter: Option<usize>,
}

/// Active physical or virtual-empty open handle inside [`SequentialSession`].
#[derive(Debug)]
enum ActiveSequentialHandle {
    /// Backed by an open [`RandomAccessStream`].
    Stream(Box<dyn RandomAccessStream>),
    /// Opened `INPUT` on a non-existent `SELECT OPTIONAL` file (`"05"` -> immediate `"10"` on `READ`).
    OptionalAbsentInput,
}

/// Unified open-session state coupling `path` and `handle` so illegal half-open states cannot exist.
#[derive(Debug)]
struct SequentialSession {
    /// Resolved physical path registered in [`FileLockRegistry`].
    path: PathBuf,
    /// Active stream or virtual-absent handle.
    handle: ActiveSequentialHandle,
}

/// Pure-Rust `ORGANIZATION IS SEQUENTIAL` file engine (`Milestone M14`).
#[derive(Debug)]
pub struct SequentialFileEngine {
    /// Connector configuration.
    config: SequentialFileConfig,
    /// Underlying storage backend.
    backend: Box<dyn StorageBackend>,
    /// Shared file-lock registry.
    locks: FileLockRegistry,
    /// Connector protocol state machine.
    state: ConnectorStateMachine,
    /// Active open session (`None` iff closed).
    session: Option<SequentialSession>,
    /// Active `LINAGE` pagination state when opened `OUTPUT` or `EXTEND` with `LINAGE`.
    linage_state: Option<LinagePaginationState>,
}

impl SequentialFileEngine {
    /// Constructs a [`SequentialFileEngine`] backed by the host [`OsFileSystem`].
    ///
    /// # Arguments
    ///
    /// * `config` - Connector configuration.
    #[must_use]
    pub fn new(config: SequentialFileConfig) -> Self {
        Self::with_backend(config, Box::new(OsFileSystem), FileLockRegistry::new())
    }

    /// Constructs a [`SequentialFileEngine`] with an explicit boxed [`StorageBackend`] and [`FileLockRegistry`].
    ///
    /// # Arguments
    ///
    /// * `config` - Connector configuration.
    /// * `backend` - Pluggable storage backend.
    /// * `locks` - Shared multi-connector lock registry.
    #[must_use]
    pub fn with_backend(
        config: SequentialFileConfig,
        backend: Box<dyn StorageBackend>,
        locks: FileLockRegistry,
    ) -> Self {
        let state = ConnectorStateMachine::new(
            FileOrganization::Sequential,
            AccessMode::Sequential,
            config.medium,
        );
        Self {
            config,
            backend,
            locks,
            state,
            session: None,
            linage_state: None,
        }
    }

    /// Returns the most recently recorded [`FileStatusCode`] on this connector.
    #[must_use]
    pub const fn last_status(&self) -> FileStatusCode {
        self.state.last_status_code()
    }

    /// Returns the most recently recorded [`FileStatusKind`] on this connector.
    #[must_use]
    pub const fn last_status_kind(&self) -> FileStatusKind {
        self.state.last_status_kind()
    }

    /// Returns the current 1-based `LINAGE-COUNTER` if `LINAGE` pagination is active.
    #[must_use]
    pub fn linage_counter(&self) -> Option<usize> {
        self.linage_state
            .as_ref()
            .map(LinagePaginationState::linage_counter)
    }

    /// Updates dynamic `LINAGE` parameters to latch on the next page transition (`SQ208M`).
    ///
    /// # Arguments
    ///
    /// * `next_params` - Newly sampled [`LinageParams`] for subsequent pages.
    pub fn set_next_linage_params(&mut self, next_params: LinageParams) {
        self.config.linage = Some(next_params);
        if let Some(linage) = self.linage_state.as_mut() {
            linage.set_next_page_params(next_params);
        }
    }

    /// Executes `OPEN {INPUT | OUTPUT | I-O | EXTEND}` on `path`.
    ///
    /// # Arguments
    ///
    /// * `path` - Resolved filesystem path for this file connector.
    /// * `mode` - Requested [`OpenMode`].
    pub fn open(&mut self, path: &Path, mode: OpenMode) -> FileStatusCode {
        if let PrecheckOutcome::Rejected(kind) = self.state.precheck_open(mode) {
            return kind.code();
        }
        if self.config.linage.is_some() && mode == OpenMode::Io {
            return self
                .state
                .record_status(FileStatusKind::OpenPermissionDenied37);
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
            self.session = Some(SequentialSession {
                path: PathBuf::from(path),
                handle: ActiveSequentialHandle::OptionalAbsentInput,
            });
            self.linage_state = None;
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

        let (req, success_kind) = select_sequential_open_request(mode, exists);
        let mut stream = match self.backend.open_stream(path, req) {
            Ok(s) => s,
            Err(err) => {
                self.locks.unregister_close(path, self.config.connector_id);
                let kind = classify_storage_error(&err, IoErrorContext::Open);
                return self.state.record_status(kind);
            }
        };

        if let Some(fail_kind) = self.validate_and_init_stream(stream.as_mut(), mode) {
            self.locks.unregister_close(path, self.config.connector_id);
            return self.state.record_status(fail_kind);
        }

        self.session = Some(SequentialSession {
            path: PathBuf::from(path),
            handle: ActiveSequentialHandle::Stream(stream),
        });
        self.state.commit_open(mode, success_kind)
    }

    /// Validates fixed-size alignment (`"39"`) and initializes `LINAGE` top margin if active.
    ///
    /// # Arguments
    ///
    /// * `stream` - Newly opened physical stream.
    /// * `mode` - Requested [`OpenMode`].
    fn validate_and_init_stream(
        &mut self,
        stream: &mut dyn RandomAccessStream,
        mode: OpenMode,
    ) -> Option<FileStatusKind> {
        if matches!(mode, OpenMode::Input | OpenMode::Io)
            && self.config.alignment_policy == FixedAlignmentPolicy::RequireExactMultiple39
            && let RecordLayout::Fixed { record_len, .. } = self.config.record_layout
        {
            let size_res = stream
                .seek_pos(SeekFrom::End(0))
                .and_then(|end| stream.seek_pos(SeekFrom::Start(0)).map(|_| end));
            match size_res {
                Ok(total) => {
                    if total % (record_len.get() as u64) != 0 {
                        return Some(FileStatusKind::AttributeConflict39);
                    }
                }
                Err(err) => return Some(classify_storage_error(&err, IoErrorContext::Open)),
            }
        }

        if matches!(mode, OpenMode::Output | OpenMode::Extend)
            && let Some(linage_params) = self.config.linage
        {
            let (lstate, top_margin) = LinagePaginationState::open_page(linage_params);
            if !top_margin.is_empty()
                && let Err(err) = stream.write_bytes(&top_margin)
            {
                return Some(classify_storage_error(
                    &err,
                    IoErrorContext::SequentialWrite,
                ));
            }
            self.linage_state = Some(lstate);
        } else {
            self.linage_state = None;
        }
        None
    }

    /// Executes sequential `READ NEXT`.
    pub fn read_next(&mut self) -> SequentialReadOutcome {
        let precheck = self.state.precheck_sequential_read();
        let Some(session) = self.session.as_mut() else {
            return empty_seq_read(self.state.last_status_code());
        };
        if let PrecheckOutcome::Rejected(kind) = precheck {
            return empty_seq_read(kind.code());
        }

        let stream = match &mut session.handle {
            ActiveSequentialHandle::OptionalAbsentInput => {
                return empty_seq_read(self.state.latch_at_end(FileStatusKind::AtEnd10));
            }
            ActiveSequentialHandle::Stream(s) => s.as_mut(),
        };

        let start_offset = match stream.seek_pos(SeekFrom::Current(0)) {
            Ok(off) => off,
            Err(err) => {
                let kind = classify_storage_error(&err, IoErrorContext::GeneralIo);
                return empty_seq_read(self.state.record_status(kind));
            }
        };

        match self.config.record_layout {
            RecordLayout::Fixed {
                record_len,
                padding_byte,
            } => read_fixed_sequential(
                stream,
                &mut self.state,
                start_offset,
                record_len,
                padding_byte,
            ),
            RecordLayout::Varying {
                min_len,
                max_len,
                header_format,
            } => read_varying_sequential(
                stream,
                &mut self.state,
                start_offset,
                min_len,
                max_len,
                header_format,
            ),
        }
    }

    /// Executes `WRITE record` with optional `BEFORE`/`AFTER ADVANCING` and `LINAGE` pagination.
    ///
    /// # Arguments
    ///
    /// * `record` - Active record slice to write (must satisfy `RecordLayout` bounds else `"44"`).
    /// * `advancing` - Carriage-advancing specification (`NoneRaw`, `DefaultSingleLine`, `Lines`, `Page`).
    pub fn write_record(
        &mut self,
        record: &[u8],
        advancing: AdvancingSpec,
    ) -> SequentialWriteOutcome {
        let precheck = self.state.precheck_write();
        let current_lc = self.linage_counter();
        let Some(SequentialSession {
            handle: ActiveSequentialHandle::Stream(stream),
            ..
        }) = self.session.as_mut()
        else {
            return SequentialWriteOutcome {
                status: self.state.last_status_code(),
                page_overflow: PageOverflowOutcome::WithinPage,
                linage_counter: current_lc,
            };
        };
        if let PrecheckOutcome::Rejected(kind) = precheck {
            return SequentialWriteOutcome {
                status: kind.code(),
                page_overflow: PageOverflowOutcome::WithinPage,
                linage_counter: current_lc,
            };
        }
        if !self.config.record_layout.accepts_write_len(record.len()) {
            let status = self
                .state
                .record_status(FileStatusKind::RewriteLengthMismatch44);
            return SequentialWriteOutcome {
                status,
                page_overflow: PageOverflowOutcome::WithinPage,
                linage_counter: current_lc,
            };
        }

        if let Some(linage) = self.linage_state.as_mut() {
            let res = linage.write_record(record, advancing);
            if let Err(err) = stream.write_bytes(&res.framed_bytes) {
                let kind = classify_storage_error(&err, IoErrorContext::SequentialWrite);
                return SequentialWriteOutcome {
                    status: self.state.record_status(kind),
                    page_overflow: PageOverflowOutcome::WithinPage,
                    linage_counter: Some(res.linage_counter),
                };
            }
            return SequentialWriteOutcome {
                status: self.state.record_status(FileStatusKind::Success00),
                page_overflow: res.page_overflow,
                linage_counter: Some(res.linage_counter),
            };
        }

        let encoded_payload = encode_sequential_record(self.config.record_layout, record);
        let framed = frame_non_linage_advancing(&encoded_payload, advancing);
        if let Err(err) = stream.write_bytes(&framed) {
            let kind = classify_storage_error(&err, IoErrorContext::SequentialWrite);
            return SequentialWriteOutcome {
                status: self.state.record_status(kind),
                page_overflow: PageOverflowOutcome::WithinPage,
                linage_counter: None,
            };
        }
        SequentialWriteOutcome {
            status: self.state.record_status(FileStatusKind::Success00),
            page_overflow: PageOverflowOutcome::WithinPage,
            linage_counter: None,
        }
    }

    /// Executes in-place `REWRITE` of the immediately preceding `READ` record (`OPEN I-O` required).
    ///
    /// Per ISO COBOL, `record.len()` must match the exact byte length of the replaced record (`"44"`).
    ///
    /// # Arguments
    ///
    /// * `record` - Replacement record payload bytes.
    pub fn rewrite_record(&mut self, record: &[u8]) -> FileStatusCode {
        let precheck = self.state.precheck_rewrite_or_delete();
        let Some(SequentialSession {
            handle: ActiveSequentialHandle::Stream(stream),
            ..
        }) = self.session.as_mut()
        else {
            return self.state.last_status_code();
        };
        let MutationPrecheckOutcome::Allowed(Some(prior)) = precheck else {
            return self.state.last_status_code();
        };
        if record.len() != prior.payload_len
            || !self.config.record_layout.accepts_write_len(record.len())
        {
            return self
                .state
                .record_status(FileStatusKind::RewriteLengthMismatch44);
        }

        let encoded = encode_sequential_record(self.config.record_layout, record);
        let io_res = stream
            .seek_pos(SeekFrom::Start(prior.position_key))
            .and_then(|_| stream.write_bytes(&encoded))
            .and_then(|()| {
                stream.seek_pos(SeekFrom::Start(
                    prior.position_key.saturating_add(prior.framed_len as u64),
                ))
            });
        match io_res {
            Ok(_) => self.state.commit_mutation(FileStatusKind::Success00),
            Err(err) => {
                let kind = classify_storage_error(&err, IoErrorContext::GeneralIo);
                self.state.record_status(kind)
            }
        }
    }

    /// Executes `CLOSE` with the given [`CloseDisposition`] (`Normal`, `WithLock`, `NoRewind`, `ReelUnit`).
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

        let mut close_err = None;
        if let ActiveSequentialHandle::Stream(mut stream) = session.handle {
            if let Some(mut linage) = self.linage_state.take() {
                let trailing = linage.finish_on_close();
                if !trailing.is_empty()
                    && let Err(err) = stream.write_bytes(&trailing)
                {
                    close_err = Some(err);
                }
            }
            if close_err.is_none()
                && let Err(err) = stream.flush_bytes()
            {
                close_err = Some(err);
            }
        } else {
            self.linage_state = None;
        }

        self.locks
            .unregister_close(&session.path, self.config.connector_id);

        if let Some(err) = close_err {
            let kind = classify_storage_error(&err, IoErrorContext::GeneralIo);
            return self.state.record_status(kind);
        }
        status
    }

    /// Removes `path` when closed (`DELETE FILE`), returning `"41"` if currently open or `"38"` if locked.
    ///
    /// # Arguments
    ///
    /// * `path` - Target filesystem path to delete.
    pub fn delete_file(&mut self, path: &Path) -> FileStatusCode {
        if let PrecheckOutcome::Rejected(kind) = self.state.precheck_open(OpenMode::Output) {
            return kind.code();
        }
        match self.backend.remove_path(path) {
            Ok(()) => self.state.record_status(FileStatusKind::Success00),
            Err(err) => {
                let kind = classify_storage_error(&err, IoErrorContext::Open);
                self.state.record_status(kind)
            }
        }
    }
}

/// Constructs an empty [`SequentialReadOutcome`] with `status`.
///
/// # Arguments
///
/// * `status` - Two-byte [`FileStatusCode`].
#[must_use]
const fn empty_seq_read(status: FileStatusCode) -> SequentialReadOutcome {
    SequentialReadOutcome {
        status,
        record: None,
        actual_len: None,
    }
}

/// Maps `(mode, exists)` (after missing-file optionality checks) to `(StorageOpenRequest, FileStatusKind)`.
///
/// # Arguments
///
/// * `mode` - Validated [`OpenMode`].
/// * `exists` - Pre-checked [`FilePresence`].
#[must_use]
const fn select_sequential_open_request(
    mode: OpenMode,
    exists: FilePresence,
) -> (StorageOpenRequest, FileStatusKind) {
    match mode {
        OpenMode::Input => (StorageOpenRequest::ReadOnly, FileStatusKind::Success00),
        OpenMode::Output => (
            StorageOpenRequest::CreateTruncateWrite,
            FileStatusKind::Success00,
        ),
        OpenMode::Io => match exists {
            FilePresence::Present => (
                StorageOpenRequest::ReadWriteExisting,
                FileStatusKind::Success00,
            ),
            FilePresence::Absent => (
                StorageOpenRequest::ReadWriteCreate,
                FileStatusKind::SuccessOptionalFile05,
            ),
        },
        OpenMode::Extend => match exists {
            FilePresence::Present => (
                StorageOpenRequest::AppendExisting,
                FileStatusKind::Success00,
            ),
            FilePresence::Absent => (
                StorageOpenRequest::AppendCreate,
                FileStatusKind::SuccessOptionalFile05,
            ),
        },
    }
}

/// Reads a single fixed-length sequential record from `stream`.
///
/// # Arguments
///
/// * `stream` - Source stream.
/// * `state` - Connector state machine.
/// * `start_offset` - Byte offset at start of record.
/// * `record_len` - Fixed record size.
/// * `padding_byte` - Padding fill byte for short final blocks (`"04"`).
fn read_fixed_sequential(
    stream: &mut dyn RandomAccessStream,
    state: &mut ConnectorStateMachine,
    start_offset: u64,
    record_len: RecordByteLen,
    padding_byte: u8,
) -> SequentialReadOutcome {
    let target_len = record_len.get();
    let mut buf = vec![padding_byte; target_len];
    match read_up_to_full(stream, &mut buf) {
        Ok(0) => empty_seq_read(state.latch_at_end(FileStatusKind::AtEnd10)),
        Ok(n) => {
            let status_kind = if n == target_len {
                FileStatusKind::Success00
            } else {
                FileStatusKind::SuccessRecordLengthMismatch04
            };
            let status = state.commit_read_success(
                PriorReadRecord {
                    position_key: start_offset,
                    payload_len: n,
                    framed_len: n,
                },
                status_kind,
            );
            SequentialReadOutcome {
                status,
                record: Some(buf),
                actual_len: Some(n),
            }
        }
        Err(err) => {
            let kind = classify_storage_error(&err, IoErrorContext::GeneralIo);
            empty_seq_read(state.record_status(kind))
        }
    }
}

/// Reads a single variable-length (`RECORD IS VARYING`) sequential record from `stream`.
///
/// # Arguments
///
/// * `stream` - Source stream.
/// * `state` - Connector state machine.
/// * `start_offset` - Byte offset at start of record header.
/// * `min_len` - Minimum FD record size.
/// * `max_len` - Maximum FD record size.
/// * `header_format` - Length-prefix encoding (`TwoByteBigEndian` or `FourByteBigEndian`).
fn read_varying_sequential(
    stream: &mut dyn RandomAccessStream,
    state: &mut ConnectorStateMachine,
    start_offset: u64,
    min_len: RecordByteLen,
    max_len: RecordByteLen,
    header_format: VaryingHeaderFormat,
) -> SequentialReadOutcome {
    let hdr_len = header_format.header_bytes();
    let mut hdr = [0_u8; 4];
    let hdr_slice = hdr.get_mut(..hdr_len).unwrap_or(&mut []);
    let hdr_read = match read_up_to_full(stream, hdr_slice) {
        Ok(n) => n,
        Err(err) => {
            let kind = classify_storage_error(&err, IoErrorContext::GeneralIo);
            return empty_seq_read(state.record_status(kind));
        }
    };
    if hdr_read == 0 {
        return empty_seq_read(state.latch_at_end(FileStatusKind::AtEnd10));
    }
    if hdr_read < hdr_len {
        return empty_seq_read(state.record_status(FileStatusKind::PermanentError30));
    }
    let payload_len = match header_format {
        VaryingHeaderFormat::TwoByteBigEndian => u16::from_be_bytes([hdr[0], hdr[1]]) as usize,
        VaryingHeaderFormat::FourByteBigEndian => u32::from_be_bytes(hdr) as usize,
    };
    let mut raw_payload = vec![0_u8; payload_len];
    match read_up_to_full(stream, &mut raw_payload) {
        Ok(n) if n == payload_len => {
            let in_bounds = payload_len >= min_len.get() && payload_len <= max_len.get();
            let status_kind = if in_bounds {
                FileStatusKind::Success00
            } else {
                FileStatusKind::SuccessRecordLengthMismatch04
            };
            let delivered_len = payload_len.min(max_len.get());
            raw_payload.truncate(delivered_len);
            let status = state.commit_read_success(
                PriorReadRecord {
                    position_key: start_offset,
                    payload_len,
                    framed_len: hdr_len.saturating_add(payload_len),
                },
                status_kind,
            );
            SequentialReadOutcome {
                status,
                record: Some(raw_payload),
                actual_len: Some(delivered_len),
            }
        }
        Ok(_) => empty_seq_read(state.record_status(FileStatusKind::PermanentError30)),
        Err(err) => {
            let kind = classify_storage_error(&err, IoErrorContext::GeneralIo);
            empty_seq_read(state.record_status(kind))
        }
    }
}

/// Prepends any variable-length record header required by `layout` in front of `payload`.
///
/// # Arguments
///
/// * `layout` - Active [`RecordLayout`].
/// * `payload` - Validated record payload bytes.
#[must_use]
fn encode_sequential_record(layout: RecordLayout, payload: &[u8]) -> Vec<u8> {
    match layout {
        RecordLayout::Fixed { .. } => payload.to_vec(),
        RecordLayout::Varying {
            header_format: VaryingHeaderFormat::TwoByteBigEndian,
            ..
        } => {
            let len_u16 = u16::try_from(payload.len()).unwrap_or(u16::MAX);
            let mut out = Vec::with_capacity(2 + payload.len());
            out.extend_from_slice(&len_u16.to_be_bytes());
            out.extend_from_slice(payload);
            out
        }
        RecordLayout::Varying {
            header_format: VaryingHeaderFormat::FourByteBigEndian,
            ..
        } => {
            let len_u32 = u32::try_from(payload.len()).unwrap_or(u32::MAX);
            let mut out = Vec::with_capacity(4 + payload.len());
            out.extend_from_slice(&len_u32.to_be_bytes());
            out.extend_from_slice(payload);
            out
        }
    }
}

/// Wraps `payload` with `BEFORE` or `AFTER ADVANCING` control bytes when `LINAGE` is not active.
///
/// # Arguments
///
/// * `payload` - Encoded record bytes.
/// * `advancing` - Requested [`AdvancingSpec`].
#[must_use]
fn frame_non_linage_advancing(payload: &[u8], advancing: AdvancingSpec) -> Vec<u8> {
    match advancing {
        AdvancingSpec::NoneRaw => payload.to_vec(),
        AdvancingSpec::DefaultSingleLine => {
            let mut out = Vec::with_capacity(payload.len() + 1);
            out.extend_from_slice(payload);
            out.push(ASCII_LF);
            out
        }
        AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count,
        } => {
            let mut out = Vec::with_capacity(payload.len() + count.max(1));
            out.extend_from_slice(payload);
            append_line_control(&mut out, count);
            out
        }
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count,
        } => {
            let mut out = Vec::with_capacity(payload.len() + count.max(1));
            append_line_control(&mut out, count);
            out.extend_from_slice(payload);
            out
        }
        AdvancingSpec::Page {
            phase: AdvancingPhase::Before,
        } => {
            let mut out = Vec::with_capacity(payload.len() + 1);
            out.extend_from_slice(payload);
            out.push(ASCII_FF);
            out
        }
        AdvancingSpec::Page {
            phase: AdvancingPhase::After,
        } => {
            let mut out = Vec::with_capacity(payload.len() + 1);
            out.push(ASCII_FF);
            out.extend_from_slice(payload);
            out
        }
    }
}

/// Appends `count` line-feeds (`0x0A`), or a single carriage-return (`0x0D`) when `count == 0`.
///
/// # Arguments
///
/// * `out` - Target byte vector.
/// * `count` - Line advance count.
fn append_line_control(out: &mut Vec<u8>, count: usize) {
    if count == 0 {
        out.push(ASCII_CR);
    } else {
        out.resize(out.len().saturating_add(count), ASCII_LF);
    }
}
