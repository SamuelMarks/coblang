//! `ORGANIZATION IS LINE SEQUENTIAL` file engine supporting `LF` and `CRLF` line framing,
//! trailing-space stripping, `COB_LS_FIXED` fixed-space preservation, and `COB_LS_NULLS`
//! NUL-prefix control-byte escaping (`Milestone M14`).

use std::path::{Path, PathBuf};

use coblang_common::{CoblangError, FileConnectorId, FileStatusCode, FileStatusKind};

use crate::status::{
    CloseTransitionOutcome, ConnectorStateMachine, IoErrorContext, PrecheckOutcome,
    PriorReadRecord, classify_storage_error,
};
use crate::storage::{
    FileLockRegistry, FilePresence, LockOutcome, OsFileSystem, RandomAccessStream, StorageBackend,
    StorageOpenRequest,
};
use crate::types::{
    ASCII_CR, ASCII_FF, ASCII_LF, ASCII_NUL, AccessMode, AdvancingPhase, AdvancingSpec,
    CloseDisposition, FileOptionality, FileOrganization, LineTerminator, LockMode,
    NullEscapePolicy, OpenMode, RecordLayout, ReelMediumKind, TrailingSpacePolicy,
};

/// Configuration of a [`LineSequentialFileEngine`].
#[derive(Debug, Clone)]
pub struct LineSequentialConfig {
    /// Logical COBOL file connector name.
    pub file_name: String,
    /// Unique file connector identifier.
    pub connector_id: FileConnectorId,
    /// `SELECT [OPTIONAL]` presence policy.
    pub optionality: FileOptionality,
    /// Fixed or varying FD record bounds.
    pub record_layout: RecordLayout,
    /// Line delimiter emitted on `WRITE` (`Lf` or `CrLf`; `READ` accepts both).
    pub write_terminator: LineTerminator,
    /// Trailing-space handling (`StripTrailingSpaces` vs `PreserveFixedSpaces` for `COB_LS_FIXED`).
    pub trailing_spaces: TrailingSpacePolicy,
    /// Control-byte NUL escaping (`RawBytes` vs `NullPrefixControlBytes` for `COB_LS_NULLS`).
    pub null_escapes: NullEscapePolicy,
    /// Physical medium kind (`NonReelDisk` vs `ReelTape`).
    pub medium: ReelMediumKind,
    /// File-sharing lock mode (`Automatic`, `Manual`, `Exclusive`).
    pub lock_mode: LockMode,
}

impl LineSequentialConfig {
    /// Creates a standard `LF`-delimited [`LineSequentialConfig`] with trailing-space stripping.
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
            write_terminator: LineTerminator::Lf,
            trailing_spaces: TrailingSpacePolicy::StripTrailingSpaces,
            null_escapes: NullEscapePolicy::RawBytes,
            medium: ReelMediumKind::NonReelDisk,
            lock_mode: LockMode::Automatic,
        }
    }
}

/// Outcome returned by [`LineSequentialFileEngine::read_next`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineReadOutcome {
    /// Resulting two-character `FILE STATUS` (`"00"`, `"04"`, `"10"`, `"30"`, `"46"`, `"47"`).
    pub status: FileStatusCode,
    /// Space-padded (for `Fixed`) or active (for `Varying`) record bytes.
    pub record: Option<Vec<u8>>,
    /// Decoded logical character length clamped to `max_bytes` (`DEPENDING ON` target value).
    pub actual_len: Option<usize>,
}

/// Active physical or virtual-empty stream state inside [`LineSession`].
#[derive(Debug)]
enum ActiveLineHandle {
    /// Open buffered byte reader/writer over [`RandomAccessStream`].
    Stream {
        /// Underlying random-access stream.
        stream: Box<dyn RandomAccessStream>,
        /// Single-byte pushback slot used when peeking `LF` after `CR` (`CRLF` framing).
        peeked_byte: Option<u8>,
        /// Monotonically incremented 0-based logical line counter.
        lines_read: u64,
    },
    /// Opened `INPUT` on a non-existent `SELECT OPTIONAL` file (`"05"` -> `"10"` on first `READ`).
    OptionalAbsentInput,
}

/// Unified open-session state coupling `path` and `handle`.
#[derive(Debug)]
struct LineSession {
    /// Resolved physical path registered in [`FileLockRegistry`].
    path: PathBuf,
    /// Active stream or virtual-absent handle.
    handle: ActiveLineHandle,
}

/// Pure-Rust `ORGANIZATION IS LINE SEQUENTIAL` file engine (`Milestone M14`).
#[derive(Debug)]
pub struct LineSequentialFileEngine {
    /// Connector configuration.
    config: LineSequentialConfig,
    /// Underlying storage backend.
    backend: Box<dyn StorageBackend>,
    /// Shared file-lock registry.
    locks: FileLockRegistry,
    /// Connector protocol state machine.
    state: ConnectorStateMachine,
    /// Active open session (`None` iff closed).
    session: Option<LineSession>,
}

impl LineSequentialFileEngine {
    /// Constructs a [`LineSequentialFileEngine`] backed by the host [`OsFileSystem`].
    ///
    /// # Arguments
    ///
    /// * `config` - Connector configuration.
    #[must_use]
    pub fn new(config: LineSequentialConfig) -> Self {
        Self::with_backend(config, Box::new(OsFileSystem), FileLockRegistry::new())
    }

    /// Constructs a [`LineSequentialFileEngine`] with an explicit boxed [`StorageBackend`] and [`FileLockRegistry`].
    ///
    /// # Arguments
    ///
    /// * `config` - Connector configuration.
    /// * `backend` - Pluggable storage backend.
    /// * `locks` - Shared multi-connector lock registry.
    #[must_use]
    pub fn with_backend(
        config: LineSequentialConfig,
        backend: Box<dyn StorageBackend>,
        locks: FileLockRegistry,
    ) -> Self {
        let state = ConnectorStateMachine::new(
            FileOrganization::LineSequential,
            AccessMode::Sequential,
            config.medium,
        );
        Self {
            config,
            backend,
            locks,
            state,
            session: None,
        }
    }

    /// Returns the most recently recorded [`FileStatusCode`] on this connector.
    #[must_use]
    pub const fn last_status(&self) -> FileStatusCode {
        self.state.last_status_code()
    }

    /// Executes `OPEN {INPUT | OUTPUT | EXTEND}` (`OPEN I-O` returns `"37"` on `LINE SEQUENTIAL`).
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
            self.session = Some(LineSession {
                path: PathBuf::from(path),
                handle: ActiveLineHandle::OptionalAbsentInput,
            });
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

        let (req, success_kind) = select_line_open_request(mode, exists);
        match self.backend.open_stream(path, req) {
            Ok(stream) => {
                self.session = Some(LineSession {
                    path: PathBuf::from(path),
                    handle: ActiveLineHandle::Stream {
                        stream,
                        peeked_byte: None,
                        lines_read: 0,
                    },
                });
                self.state.commit_open(mode, success_kind)
            }
            Err(err) => {
                self.locks.unregister_close(path, self.config.connector_id);
                let kind = classify_storage_error(&err, IoErrorContext::Open);
                self.state.record_status(kind)
            }
        }
    }

    /// Reads the next `LF`- or `CRLF`-delimited logical record, unescaping `COB_LS_NULLS` if enabled.
    pub fn read_next(&mut self) -> LineReadOutcome {
        let precheck = self.state.precheck_sequential_read();
        let Some(session) = self.session.as_mut() else {
            return empty_line_read(self.state.last_status_code());
        };
        if let PrecheckOutcome::Rejected(kind) = precheck {
            return empty_line_read(kind.code());
        }

        let (stream, peeked_byte, lines_read) = match &mut session.handle {
            ActiveLineHandle::OptionalAbsentInput => {
                return empty_line_read(self.state.latch_at_end(FileStatusKind::AtEnd10));
            }
            ActiveLineHandle::Stream {
                stream,
                peeked_byte,
                lines_read,
            } => (stream, peeked_byte, lines_read),
        };

        let decoded_line =
            match read_logical_line(stream.as_mut(), peeked_byte, self.config.null_escapes) {
                Ok(None) => {
                    return empty_line_read(self.state.latch_at_end(FileStatusKind::AtEnd10));
                }
                Ok(Some(bytes)) => bytes,
                Err(LineDecodeError::TruncatedNullEscape) => {
                    return empty_line_read(
                        self.state.record_status(FileStatusKind::PermanentError30),
                    );
                }
                Err(LineDecodeError::Storage(err)) => {
                    let kind = classify_storage_error(&err, IoErrorContext::GeneralIo);
                    return empty_line_read(self.state.record_status(kind));
                }
            };

        let line_idx = *lines_read;
        *lines_read = lines_read.saturating_add(1);
        format_decoded_line(
            self.config.record_layout,
            &mut self.state,
            line_idx,
            decoded_line,
        )
    }

    /// Writes a logical record formatted with trailing-space policy, `COB_LS_NULLS`, and `advancing`.
    ///
    /// # Arguments
    ///
    /// * `record` - Record payload slice (validated against `RecordLayout` bounds else `"44"`).
    /// * `advancing` - Line/page carriage control (`DefaultSingleLine` emits `write_terminator`).
    pub fn write_record(&mut self, record: &[u8], advancing: AdvancingSpec) -> FileStatusCode {
        let precheck = self.state.precheck_write();
        let Some(LineSession {
            handle: ActiveLineHandle::Stream { stream, .. },
            ..
        }) = self.session.as_mut()
        else {
            return self.state.last_status_code();
        };
        if let PrecheckOutcome::Rejected(kind) = precheck {
            return kind.code();
        }
        if !self.config.record_layout.accepts_write_len(record.len()) {
            return self
                .state
                .record_status(FileStatusKind::RewriteLengthMismatch44);
        }

        let trimmed = match self.config.trailing_spaces {
            TrailingSpacePolicy::PreserveFixedSpaces => record,
            TrailingSpacePolicy::StripTrailingSpaces => strip_trailing_ascii_spaces(record),
        };
        let escaped = encode_null_escapes(trimmed, self.config.null_escapes);
        let framed = frame_line_write(&escaped, advancing, self.config.write_terminator);

        match stream.write_bytes(&framed) {
            Ok(()) => self.state.record_status(FileStatusKind::Success00),
            Err(err) => {
                let kind = classify_storage_error(&err, IoErrorContext::SequentialWrite);
                self.state.record_status(kind)
            }
        }
    }

    /// Rejects `REWRITE` on `LINE SEQUENTIAL` with `"49"` (since `LINE SEQUENTIAL` cannot open `I-O`).
    pub fn rewrite_unsupported(&mut self) -> FileStatusCode {
        self.state
            .record_status(FileStatusKind::RewriteDeleteWrongOpenMode49)
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

        let flush_err = if let ActiveLineHandle::Stream { mut stream, .. } = session.handle {
            stream.flush_bytes().err()
        } else {
            None
        };

        self.locks
            .unregister_close(&session.path, self.config.connector_id);

        if let Some(err) = flush_err {
            let kind = classify_storage_error(&err, IoErrorContext::GeneralIo);
            return self.state.record_status(kind);
        }
        status
    }
}

/// Constructs an empty [`LineReadOutcome`] with `status`.
///
/// # Arguments
///
/// * `status` - Two-byte [`FileStatusCode`].
#[must_use]
const fn empty_line_read(status: FileStatusCode) -> LineReadOutcome {
    LineReadOutcome {
        status,
        record: None,
        actual_len: None,
    }
}

/// Maps validated `LINE SEQUENTIAL` `(mode, exists)` to `(StorageOpenRequest, FileStatusKind)`.
///
/// # Arguments
///
/// * `mode` - Validated [`OpenMode`] (`Input`, `Output`, or `Extend`).
/// * `exists` - Pre-checked [`FilePresence`].
#[must_use]
const fn select_line_open_request(
    mode: OpenMode,
    exists: FilePresence,
) -> (StorageOpenRequest, FileStatusKind) {
    if matches!(mode, OpenMode::Input) {
        (StorageOpenRequest::ReadOnly, FileStatusKind::Success00)
    } else if matches!(mode, OpenMode::Extend) {
        match exists {
            FilePresence::Present => (
                StorageOpenRequest::AppendExisting,
                FileStatusKind::Success00,
            ),
            FilePresence::Absent => (
                StorageOpenRequest::AppendCreate,
                FileStatusKind::SuccessOptionalFile05,
            ),
        }
    } else {
        (
            StorageOpenRequest::CreateTruncateWrite,
            FileStatusKind::Success00,
        )
    }
}

/// Materializes a decoded `LINE SEQUENTIAL` record according to `layout` (`"00"` vs `"04"`).
///
/// # Arguments
///
/// * `layout` - Connector [`RecordLayout`].
/// * `state` - Connector state machine.
/// * `line_idx` - Zero-based logical line index.
/// * `decoded_line` - Decoded unescaped line bytes.
fn format_decoded_line(
    layout: RecordLayout,
    state: &mut ConnectorStateMachine,
    line_idx: u64,
    mut decoded_line: Vec<u8>,
) -> LineReadOutcome {
    match layout {
        RecordLayout::Fixed {
            record_len,
            padding_byte,
        } => {
            let fixed_len = record_len.get();
            let raw_len = decoded_line.len();
            let status_kind = if raw_len > fixed_len {
                FileStatusKind::SuccessRecordLengthMismatch04
            } else {
                FileStatusKind::Success00
            };
            let mut rec = vec![padding_byte; fixed_len];
            let copy_len = raw_len.min(fixed_len);
            for (dst, &src) in rec.iter_mut().zip(decoded_line.iter().take(copy_len)) {
                *dst = src;
            }
            let status = state.commit_read_success(
                PriorReadRecord {
                    position_key: line_idx,
                    payload_len: fixed_len,
                    framed_len: fixed_len,
                },
                status_kind,
            );
            LineReadOutcome {
                status,
                record: Some(rec),
                actual_len: Some(copy_len),
            }
        }
        RecordLayout::Varying {
            min_len, max_len, ..
        } => {
            let raw_len = decoded_line.len();
            let status_kind = if raw_len > max_len.get() {
                FileStatusKind::SuccessRecordLengthMismatch04
            } else {
                FileStatusKind::Success00
            };
            if decoded_line.len() > max_len.get() {
                decoded_line.truncate(max_len.get());
            } else if decoded_line.len() < min_len.get() {
                decoded_line.resize(min_len.get(), b' ');
            }
            let actual = decoded_line.len();
            let status = state.commit_read_success(
                PriorReadRecord {
                    position_key: line_idx,
                    payload_len: actual,
                    framed_len: actual,
                },
                status_kind,
            );
            LineReadOutcome {
                status,
                record: Some(decoded_line),
                actual_len: Some(actual),
            }
        }
    }
}

/// Internal error produced while scanning a `LINE SEQUENTIAL` record.
#[derive(Debug)]
enum LineDecodeError {
    /// Underlying storage error.
    Storage(CoblangError),
    /// Lone `0x00` escape prefix encountered immediately before physical EOF (`COB_LS_NULLS`).
    TruncatedNullEscape,
}

/// Reads a single byte from `stream`, honoring `peeked_byte` if buffered.
///
/// # Arguments
///
/// * `stream` - Source stream.
/// * `peeked_byte` - Single-byte lookahead slot.
fn read_next_byte(
    stream: &mut dyn RandomAccessStream,
    peeked_byte: &mut Option<u8>,
) -> coblang_common::Result<Option<u8>> {
    if let Some(b) = peeked_byte.take() {
        return Ok(Some(b));
    }
    let mut one = [0_u8; 1];
    let n = stream.read_bytes(&mut one)?;
    if n == 0 { Ok(None) } else { Ok(Some(one[0])) }
}

/// Decodes one `LF`- or `CRLF`-delimited line (or `None` at clean EOF).
///
/// # Arguments
///
/// * `stream` - Source stream.
/// * `peeked_byte` - Single-byte lookahead slot.
/// * `null_policy` - Active `COB_LS_NULLS` unescaping policy.
fn read_logical_line(
    stream: &mut dyn RandomAccessStream,
    peeked_byte: &mut Option<u8>,
    null_policy: NullEscapePolicy,
) -> Result<Option<Vec<u8>>, LineDecodeError> {
    let mut out = Vec::new();
    let mut saw_any_byte = false;

    loop {
        let next = read_next_byte(stream, peeked_byte).map_err(LineDecodeError::Storage)?;
        let Some(b) = next else {
            return if saw_any_byte {
                Ok(Some(out))
            } else {
                Ok(None)
            };
        };
        saw_any_byte = true;

        if null_policy == NullEscapePolicy::NullPrefixControlBytes && b == ASCII_NUL {
            let escaped = read_next_byte(stream, peeked_byte).map_err(LineDecodeError::Storage)?;
            let Some(ctrl) = escaped else {
                return Err(LineDecodeError::TruncatedNullEscape);
            };
            out.push(ctrl);
            continue;
        }

        if b == ASCII_LF {
            return Ok(Some(out));
        }

        if b == ASCII_CR {
            let after_cr = read_next_byte(stream, peeked_byte).map_err(LineDecodeError::Storage)?;
            if let Some(following) = after_cr
                && following != ASCII_LF
            {
                *peeked_byte = Some(following);
            }
            return Ok(Some(out));
        }

        out.push(b);
    }
}

/// Strips trailing `0x20` space bytes from `record`.
///
/// # Arguments
///
/// * `record` - Raw record byte slice.
#[must_use]
fn strip_trailing_ascii_spaces(record: &[u8]) -> &[u8] {
    let keep_len = record
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |idx| idx + 1);
    record.get(..keep_len).unwrap_or(&[])
}

/// Applies `COB_LS_NULLS` control-byte prefixing (`b < 0x20` -> `[0x00, b]`) when enabled.
///
/// # Arguments
///
/// * `record` - Space-processed record slice.
/// * `policy` - Control-byte escaping policy.
#[must_use]
fn encode_null_escapes(record: &[u8], policy: NullEscapePolicy) -> Vec<u8> {
    match policy {
        NullEscapePolicy::RawBytes => record.to_vec(),
        NullEscapePolicy::NullPrefixControlBytes => {
            let mut out = Vec::with_capacity(record.len());
            for &b in record {
                if b < b' ' {
                    out.push(ASCII_NUL);
                }
                out.push(b);
            }
            out
        }
    }
}

/// Frames `escaped_payload` with `advancing` and `terminator`.
///
/// # Arguments
///
/// * `escaped_payload` - Escaped line payload bytes.
/// * `advancing` - Requested [`AdvancingSpec`].
/// * `terminator` - Configured [`LineTerminator`] (`Lf` or `CrLf`).
#[must_use]
fn frame_line_write(
    escaped_payload: &[u8],
    advancing: AdvancingSpec,
    terminator: LineTerminator,
) -> Vec<u8> {
    let term_bytes = terminator.as_bytes();
    match advancing {
        AdvancingSpec::NoneRaw => escaped_payload.to_vec(),
        AdvancingSpec::DefaultSingleLine => {
            let mut out = Vec::with_capacity(escaped_payload.len() + term_bytes.len());
            out.extend_from_slice(escaped_payload);
            out.extend_from_slice(term_bytes);
            out
        }
        AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count,
        } => {
            let mut out = Vec::new();
            out.extend_from_slice(escaped_payload);
            append_line_terminators(&mut out, count, term_bytes);
            out
        }
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count,
        } => {
            let mut out = Vec::new();
            append_line_terminators(&mut out, count, term_bytes);
            out.extend_from_slice(escaped_payload);
            out
        }
        AdvancingSpec::Page {
            phase: AdvancingPhase::Before,
        } => {
            let mut out = Vec::with_capacity(escaped_payload.len() + 1);
            out.extend_from_slice(escaped_payload);
            out.push(ASCII_FF);
            out
        }
        AdvancingSpec::Page {
            phase: AdvancingPhase::After,
        } => {
            let mut out = Vec::with_capacity(escaped_payload.len() + 1);
            out.push(ASCII_FF);
            out.extend_from_slice(escaped_payload);
            out
        }
    }
}

/// Appends `count` repetitions of `term_bytes`, or a single `CR` (`0x0D`) when `count == 0`.
///
/// # Arguments
///
/// * `out` - Destination buffer.
/// * `count` - Line count.
/// * `term_bytes` - Configured line terminator bytes.
fn append_line_terminators(out: &mut Vec<u8>, count: usize, term_bytes: &[u8]) {
    if count == 0 {
        out.push(ASCII_CR);
    } else {
        for _ in 0..count {
            out.extend_from_slice(term_bytes);
        }
    }
}
