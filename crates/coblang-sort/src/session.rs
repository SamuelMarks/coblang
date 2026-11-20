//! Bounded-memory `SortSession` supporting in-memory sort, k-way external spill-file merge
//! when exceeding `COB_SORT_MEMORY`, pre-sorted `MERGE` streams, and `SORT-RETURN` early abort.

use core::cmp::Ordering;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use coblang_common::{CoblangError, Result};

use crate::comparator::SortComparator;

/// 8-byte magic header prefixing every temporary `coblang-sort` spill run file.
const SPILL_MAGIC: &[u8; 8] = b"COBSRUN1";

/// Default maximum number of spill files merged in a single k-way merge pass (`64`).
const DEFAULT_MAX_MERGE_FAN_IN: usize = 64;

/// Process-wide monotonic counter disambiguating temporary spill directories across sessions.
static NEXT_SESSION_NONCE: AtomicU64 = AtomicU64::new(1);

/// Validated non-zero memory budget in bytes (`COB_SORT_MEMORY`) before spilling sorted runs to disk.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SortMemoryLimit(
    /// Non-zero byte ceiling for buffered in-memory sort records.
    usize,
);

impl Default for SortMemoryLimit {
    /// Returns [`Self::DEFAULT`] (`128 MiB`).
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl SortMemoryLimit {
    /// Default `COB_SORT_MEMORY` ceiling (`128 MiB` = `134_217_728` bytes).
    pub const DEFAULT: Self = Self(128 * 1024 * 1024);

    /// Validates and constructs a [`SortMemoryLimit`] (`>= 1` byte).
    ///
    /// # Arguments
    ///
    /// * `bytes` - Maximum buffered record bytes before spilling (`0` is rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `bytes == 0`.
    pub fn try_new(bytes: usize) -> Result<Self> {
        if bytes == 0 {
            return Err(CoblangError::SortMerge {
                message: String::from("COB_SORT_MEMORY byte limit must be >= 1"),
            });
        }
        Ok(Self(bytes))
    }

    /// Returns the underlying byte budget.
    #[must_use]
    pub const fn as_bytes(self) -> usize {
        self.0
    }

    /// Parses a `COB_SORT_MEMORY` specification (`"65536"`, `"64K"`, `"128M"`, `"1G"`, optional `B`/`iB`).
    ///
    /// # Arguments
    ///
    /// * `raw` - Raw human-readable byte or unit-suffixed limit string.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `raw` is malformed, zero, or overflows `usize`.
    pub fn parse_str(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        let split_idx = trimmed
            .bytes()
            .position(|b| !b.is_ascii_digit())
            .unwrap_or(trimmed.len());
        let digits_part = trimmed.get(..split_idx).unwrap_or("");
        let unit_part = trimmed.get(split_idx..).unwrap_or("").trim();

        let base = digits_part
            .parse::<usize>()
            .map_err(|_| CoblangError::SortMerge {
                message: format!("invalid COB_SORT_MEMORY numeric value `{trimmed}`"),
            })?;

        let multiplier: usize = match unit_part.to_ascii_uppercase().as_str() {
            "" | "B" => 1,
            "K" | "KB" | "KIB" => 1024,
            "M" | "MB" | "MIB" => 1024 * 1024,
            "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
            unknown => {
                return Err(CoblangError::SortMerge {
                    message: format!("unsupported COB_SORT_MEMORY unit suffix `{unknown}`"),
                });
            }
        };

        let total = base
            .checked_mul(multiplier)
            .ok_or_else(|| CoblangError::SortMerge {
                message: format!("COB_SORT_MEMORY value `{trimmed}` overflows usize"),
            })?;
        Self::try_new(total)
    }

    /// Resolves an optional `COB_SORT_MEMORY` string value, returning [`Self::DEFAULT`] when `None`.
    ///
    /// # Arguments
    ///
    /// * `raw` - Optional raw `COB_SORT_MEMORY` value.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `raw` is `Some(...)` and fails [`Self::parse_str`].
    pub fn from_env_value(raw: Option<&str>) -> Result<Self> {
        match raw {
            Some(text) => Self::parse_str(text),
            None => Ok(Self::DEFAULT),
        }
    }

    /// Reads `COB_SORT_MEMORY` from the host process environment, defaulting to [`Self::DEFAULT`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `COB_SORT_MEMORY` is set to an invalid value.
    pub fn from_env() -> Result<Self> {
        let val = std::env::var("COB_SORT_MEMORY").ok();
        Self::from_env_value(val.as_deref())
    }
}

/// Validated `SD` record length range (`1 <= min_bytes <= max_bytes <= MAX_RECORD_BYTES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordLengthBounds {
    /// Minimum legal record byte length (`>= 1`).
    min_bytes: usize,
    /// Maximum legal record byte length (`>= min_bytes`).
    max_bytes: usize,
}

impl RecordLengthBounds {
    /// Hard upper bound on a single `SD` sort/merge record (`64 MiB`).
    pub const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

    /// Validates and constructs variable-length `SD` record bounds `min_bytes..=max_bytes`.
    ///
    /// # Arguments
    ///
    /// * `min_bytes` - Minimum record byte length (`>= 1`).
    /// * `max_bytes` - Maximum record byte length (`min_bytes..=MAX_RECORD_BYTES`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `min_bytes == 0`, `min_bytes > max_bytes`,
    /// or `max_bytes > Self::MAX_RECORD_BYTES`.
    pub fn try_new(min_bytes: usize, max_bytes: usize) -> Result<Self> {
        if min_bytes == 0 || min_bytes > max_bytes || max_bytes > Self::MAX_RECORD_BYTES {
            return Err(CoblangError::SortMerge {
                message: format!(
                    "invalid SD record bounds {min_bytes}..={max_bytes} (expected 1..={})",
                    Self::MAX_RECORD_BYTES
                ),
            });
        }
        Ok(Self {
            min_bytes,
            max_bytes,
        })
    }

    /// Constructs fixed-length `SD` record bounds (`exact_bytes..=exact_bytes`).
    ///
    /// # Arguments
    ///
    /// * `exact_bytes` - Exact fixed record length in bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `exact_bytes == 0` or `exact_bytes > Self::MAX_RECORD_BYTES`.
    pub fn fixed(exact_bytes: usize) -> Result<Self> {
        Self::try_new(exact_bytes, exact_bytes)
    }

    /// Returns the minimum allowed record length in bytes.
    #[must_use]
    pub const fn min_bytes(self) -> usize {
        self.min_bytes
    }

    /// Returns the maximum allowed record length in bytes.
    #[must_use]
    pub const fn max_bytes(self) -> usize {
        self.max_bytes
    }

    /// Verifies that `record.len()` lies within `min_bytes..=max_bytes`.
    ///
    /// # Arguments
    ///
    /// * `record` - Candidate record payload.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `record.len()` lies outside `self`.
    pub fn validate_record(self, record: &[u8]) -> Result<()> {
        let len = record.len();
        if len < self.min_bytes || len > self.max_bytes {
            return Err(CoblangError::SortMerge {
                message: format!(
                    "record length {len} outside SD bounds {}..={}",
                    self.min_bytes, self.max_bytes
                ),
            });
        }
        Ok(())
    }
}

/// Strongly-typed COBOL `SORT-RETURN` special register status (`0` success vs `16` early abort).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortReturnStatus {
    /// Normal execution (`SORT-RETURN = 0`).
    #[default]
    Success0,
    /// Early-abort status (`MOVE 16 TO SORT-RETURN`).
    Failed16,
}

impl SortReturnStatus {
    /// Maps a COBOL `SORT-RETURN` integer (`0` -> `Success0`, non-zero -> `Failed16`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw `SORT-RETURN` register value.
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        if code == 0 {
            Self::Success0
        } else {
            Self::Failed16
        }
    }

    /// Returns the canonical `SORT-RETURN` integer (`0` or `16`).
    #[must_use]
    pub const fn as_code(self) -> i32 {
        match self {
            Self::Success0 => 0,
            Self::Failed16 => 16,
        }
    }

    /// Returns `true` when `self` is [`Self::Failed16`].
    #[must_use]
    pub const fn is_failed(self) -> bool {
        matches!(self, Self::Failed16)
    }
}

/// Lifecycle phase of a [`SortSession`] (`SORT` or `MERGE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortSessionState {
    /// Accepting unsorted records via [`SortSession::release`] (`INPUT PROCEDURE` / `USING`).
    AcceptingInput,
    /// Accepting pre-sorted input streams for `MERGE` (`USING f1 f2 ...`).
    AcceptingMergeRuns,
    /// Yielding globally ordered records via [`SortSession::return_next`] (`OUTPUT PROCEDURE` / `GIVING`).
    ReturningOutput,
    /// All records have been returned (`AT END`) or session was closed normally.
    Completed,
    /// Session was terminated early via `SORT-RETURN = 16` ([`SortSession::abort`]).
    Aborted,
}

/// Runtime tuning and storage parameters for a [`SortSession`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortSessionConfig {
    /// Validated `SD` record length bounds.
    record_bounds: RecordLengthBounds,
    /// In-memory byte ceiling (`COB_SORT_MEMORY`) before spilling sorted runs.
    memory_limit: SortMemoryLimit,
    /// Parent directory under which temporary spill subdirectories are created when needed.
    spill_dir: PathBuf,
    /// Maximum number of spill files merged per k-way compaction pass (`>= 2`).
    max_merge_fan_in: usize,
}

impl SortSessionConfig {
    /// Creates a [`SortSessionConfig`] with [`SortMemoryLimit::DEFAULT`], `std::env::temp_dir()`, and fan-in `64`.
    ///
    /// # Arguments
    ///
    /// * `record_bounds` - Validated `SD` record length bounds.
    #[must_use]
    pub fn new(record_bounds: RecordLengthBounds) -> Self {
        Self {
            record_bounds,
            memory_limit: SortMemoryLimit::DEFAULT,
            spill_dir: std::env::temp_dir(),
            max_merge_fan_in: DEFAULT_MAX_MERGE_FAN_IN,
        }
    }

    /// Overrides the `COB_SORT_MEMORY` limit.
    ///
    /// # Arguments
    ///
    /// * `limit` - Validated [`SortMemoryLimit`].
    #[must_use]
    pub const fn with_memory_limit(mut self, limit: SortMemoryLimit) -> Self {
        self.memory_limit = limit;
        self
    }

    /// Overrides the base directory used for temporary spill runs.
    ///
    /// # Arguments
    ///
    /// * `dir` - Parent directory path for external spill subdirectories.
    #[must_use]
    pub fn with_spill_dir(mut self, dir: PathBuf) -> Self {
        self.spill_dir = dir;
        self
    }

    /// Overrides the maximum k-way merge fan-in per compaction pass (`>= 2`).
    ///
    /// # Arguments
    ///
    /// * `fan_in` - Maximum simultaneous open spill run readers (`>= 2`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if `fan_in < 2`.
    pub fn with_max_merge_fan_in(mut self, fan_in: usize) -> Result<Self> {
        if fan_in < 2 {
            return Err(CoblangError::SortMerge {
                message: format!("max_merge_fan_in must be >= 2, found {fan_in}"),
            });
        }
        self.max_merge_fan_in = fan_in;
        Ok(self)
    }

    /// Returns the configured [`RecordLengthBounds`].
    #[must_use]
    pub const fn record_bounds(&self) -> RecordLengthBounds {
        self.record_bounds
    }

    /// Returns the configured [`SortMemoryLimit`].
    #[must_use]
    pub const fn memory_limit(&self) -> SortMemoryLimit {
        self.memory_limit
    }

    /// Returns the configured base spill directory path.
    #[must_use]
    pub fn spill_dir(&self) -> &Path {
        &self.spill_dir
    }

    /// Returns the maximum k-way merge fan-in per pass.
    #[must_use]
    pub const fn max_merge_fan_in(&self) -> usize {
        self.max_merge_fan_in
    }
}

/// Stateful COBOL `SORT` and `MERGE` execution session with external k-way spill-merge.
#[derive(Debug)]
pub struct SortSession {
    /// Multi-key comparator governing run sorting and k-way merge tie-breaking.
    comparator: SortComparator,
    /// Session memory, bounds, and temporary directory configuration.
    config: SortSessionConfig,
    /// Current lifecycle phase.
    state: SortSessionState,
    /// Current `SORT-RETURN` register value (`0` or `16`).
    sort_return: SortReturnStatus,
    /// Buffered records during `AcceptingInput` or pure in-memory `ReturningOutput`.
    in_memory_records: Vec<Vec<u8>>,
    /// Total payload bytes currently held in `in_memory_records`.
    in_memory_bytes: usize,
    /// Read index into `in_memory_records` during pure in-memory `ReturningOutput`.
    in_memory_cursor: usize,
    /// Buffered records of the currently open `MERGE` input stream (`AcceptingMergeRuns`).
    active_merge_records: Option<Vec<Vec<u8>>>,
    /// Unique per-session directory created lazily on the first external spill.
    session_dir: PathBuf,
    /// Whether `session_dir` has been created on disk.
    dir_created: bool,
    /// Next monotonic spill file sequence number (`run-XXXXXX.spill`).
    next_run_id: u64,
    /// Total count of initial spill / merge run files written during input ingestion.
    total_spilled_runs: usize,
    /// Pending spill run file paths awaiting compaction / k-way merge.
    spill_files: Vec<PathBuf>,
    /// Active k-way external merge cursor during spilled `ReturningOutput`.
    merge_cursor: Option<KWayMergeCursor>,
}

impl Drop for SortSession {
    /// Removes any remaining temporary spill run files and session directory on drop.
    fn drop(&mut self) {
        self.cleanup_spill_artifacts();
    }
}

impl SortSession {
    /// Creates a new `SORT` session in [`SortSessionState::AcceptingInput`].
    ///
    /// # Arguments
    ///
    /// * `comparator` - Multi-key [`SortComparator`].
    /// * `config` - [`SortSessionConfig`] governing `COB_SORT_MEMORY` and `SD` record bounds.
    #[must_use]
    pub fn new_sort(comparator: SortComparator, config: SortSessionConfig) -> Self {
        Self::new_with_state(comparator, config, SortSessionState::AcceptingInput)
    }

    /// Creates a new `MERGE` session in [`SortSessionState::AcceptingMergeRuns`].
    ///
    /// # Arguments
    ///
    /// * `comparator` - Multi-key [`SortComparator`].
    /// * `config` - [`SortSessionConfig`] governing `SD` record bounds and spill storage.
    #[must_use]
    pub fn new_merge(comparator: SortComparator, config: SortSessionConfig) -> Self {
        Self::new_with_state(comparator, config, SortSessionState::AcceptingMergeRuns)
    }

    /// Shared constructor initializing per-session unique temporary spill directory path.
    ///
    /// # Arguments
    ///
    /// * `comparator` - Multi-key [`SortComparator`].
    /// * `config` - [`SortSessionConfig`].
    /// * `initial_state` - Initial [`SortSessionState`].
    fn new_with_state(
        comparator: SortComparator,
        config: SortSessionConfig,
        initial_state: SortSessionState,
    ) -> Self {
        let nonce = NEXT_SESSION_NONCE.fetch_add(1, AtomicOrdering::Relaxed);
        let session_dir = config
            .spill_dir
            .join(format!("coblang-sort-{}-{nonce}", std::process::id()));
        Self {
            comparator,
            config,
            state: initial_state,
            sort_return: SortReturnStatus::Success0,
            in_memory_records: Vec::new(),
            in_memory_bytes: 0,
            in_memory_cursor: 0,
            active_merge_records: None,
            session_dir,
            dir_created: false,
            next_run_id: 0,
            total_spilled_runs: 0,
            spill_files: Vec::new(),
            merge_cursor: None,
        }
    }

    /// Returns the current [`SortSessionState`].
    #[must_use]
    pub const fn state(&self) -> SortSessionState {
        self.state
    }

    /// Returns the current [`SortReturnStatus`] (`0` or `16`).
    #[must_use]
    pub const fn sort_return(&self) -> SortReturnStatus {
        self.sort_return
    }

    /// Updates `SORT-RETURN`, aborting the session immediately if `status` is [`SortReturnStatus::Failed16`].
    ///
    /// # Arguments
    ///
    /// * `status` - Desired [`SortReturnStatus`].
    pub fn set_sort_return(&mut self, status: SortReturnStatus) {
        if status.is_failed() {
            self.abort();
        } else {
            self.sort_return = SortReturnStatus::Success0;
        }
    }

    /// Aborts the session (`SORT-RETURN = 16`), discarding buffered records and deleting all spill files.
    pub fn abort(&mut self) {
        self.sort_return = SortReturnStatus::Failed16;
        self.state = SortSessionState::Aborted;
        self.in_memory_records.clear();
        self.in_memory_bytes = 0;
        self.active_merge_records = None;
        self.cleanup_spill_artifacts();
    }

    /// Closes the session cleanly, removing any remaining spill files and transitioning non-aborted sessions to [`SortSessionState::Completed`].
    pub fn close(&mut self) {
        self.in_memory_records.clear();
        self.in_memory_bytes = 0;
        self.active_merge_records = None;
        self.cleanup_spill_artifacts();
        if self.state != SortSessionState::Aborted {
            self.state = SortSessionState::Completed;
        }
    }

    /// Returns a reference to the session's [`SortComparator`].
    #[must_use]
    pub const fn comparator(&self) -> &SortComparator {
        &self.comparator
    }

    /// Returns a reference to the session's [`SortSessionConfig`].
    #[must_use]
    pub const fn config(&self) -> &SortSessionConfig {
        &self.config
    }

    /// Returns the number of external spill / merge run files written during input ingestion.
    #[must_use]
    pub const fn spilled_run_count(&self) -> usize {
        self.total_spilled_runs
    }

    /// Returns the unique temporary directory path assigned to this session for spill files.
    #[must_use]
    pub fn session_spill_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Returns the file path that will be used for the next allocated spill run (`run-XXXXXX.spill`).
    #[must_use]
    pub fn next_spill_file_path(&self) -> PathBuf {
        self.session_dir
            .join(format!("run-{:06}.spill", self.next_run_id))
    }

    /// Releases a single unsorted `SD` record into a `SORT` session (`RELEASE` verb).
    ///
    /// Automatically sorts and spills the current in-memory buffer to a temporary run file
    /// when adding `record` would exceed `COB_SORT_MEMORY`.
    ///
    /// # Arguments
    ///
    /// * `record` - Raw `SD` record bytes within configured [`RecordLengthBounds`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if the session is not in [`SortSessionState::AcceptingInput`]
    /// or `record.len()` violates `SD` bounds, or [`CoblangError::Io`] on spill file I/O failure.
    pub fn release(&mut self, record: &[u8]) -> Result<()> {
        if self.state != SortSessionState::AcceptingInput {
            return Err(CoblangError::SortMerge {
                message: format!("cannot RELEASE record in session state {:?}", self.state),
            });
        }
        self.config.record_bounds.validate_record(record)?;

        if !self.in_memory_records.is_empty()
            && self.in_memory_bytes.saturating_add(record.len())
                > self.config.memory_limit.as_bytes()
        {
            self.spill_in_memory_run()?;
        }

        self.in_memory_bytes = self.in_memory_bytes.saturating_add(record.len());
        self.in_memory_records.push(record.to_vec());
        Ok(())
    }

    /// Starts a new pre-sorted input stream in a `MERGE` session (`USING file-i`).
    ///
    /// Flushes any previously open `MERGE` stream before starting the new stream.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if the session is not in [`SortSessionState::AcceptingMergeRuns`],
    /// or [`CoblangError::Io`] if flushing the previous run fails.
    pub fn begin_merge_run(&mut self) -> Result<()> {
        if self.state != SortSessionState::AcceptingMergeRuns {
            return Err(CoblangError::SortMerge {
                message: format!("cannot begin MERGE run in session state {:?}", self.state),
            });
        }
        self.finish_active_merge_run()?;
        self.active_merge_records = Some(Vec::new());
        Ok(())
    }

    /// Appends one record from the currently active `USING` file into a `MERGE` session.
    ///
    /// # Arguments
    ///
    /// * `record` - Next record from the current pre-sorted `MERGE` input file.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if not in [`SortSessionState::AcceptingMergeRuns`],
    /// [`Self::begin_merge_run`] was not called, or `record.len()` violates `SD` bounds.
    pub fn push_merge_record(&mut self, record: &[u8]) -> Result<()> {
        if self.state != SortSessionState::AcceptingMergeRuns {
            return Err(CoblangError::SortMerge {
                message: format!("cannot push MERGE record in session state {:?}", self.state),
            });
        }
        self.config.record_bounds.validate_record(record)?;
        let Some(buf) = &mut self.active_merge_records else {
            return Err(CoblangError::SortMerge {
                message: String::from("push_merge_record called before begin_merge_run"),
            });
        };
        buf.push(record.to_vec());
        Ok(())
    }

    /// Convenience helper registering a complete pre-sorted `MERGE` input run (`records`).
    ///
    /// # Arguments
    ///
    /// * `records` - Ordered slice of records belonging to one `USING` input file.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] on state/length violation or [`CoblangError::Io`] on spill failure.
    pub fn add_merge_run(&mut self, records: &[Vec<u8>]) -> Result<()> {
        self.begin_merge_run()?;
        for rec in records {
            self.push_merge_record(rec)?;
        }
        self.finish_active_merge_run()
    }

    /// Completes input ingestion, sorting remaining in-memory data or opening the k-way external
    /// merge cursor, and transitions to [`SortSessionState::ReturningOutput`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if not in an input state or if spill-file headers are corrupt,
    /// or [`CoblangError::Io`] if flushing/opening spill runs fails.
    pub fn finish_input(&mut self) -> Result<()> {
        match self.state {
            SortSessionState::AcceptingInput => {
                if self.spill_files.is_empty() {
                    self.sort_in_memory_buffer();
                    self.in_memory_cursor = 0;
                    self.state = SortSessionState::ReturningOutput;
                    Ok(())
                } else {
                    self.spill_in_memory_run()?;
                    self.open_final_merge_cursor()?;
                    self.state = SortSessionState::ReturningOutput;
                    Ok(())
                }
            }
            SortSessionState::AcceptingMergeRuns => {
                self.finish_active_merge_run()?;
                if !self.spill_files.is_empty() {
                    self.open_final_merge_cursor()?;
                }
                self.state = SortSessionState::ReturningOutput;
                Ok(())
            }
            SortSessionState::ReturningOutput
            | SortSessionState::Completed
            | SortSessionState::Aborted => Err(CoblangError::SortMerge {
                message: format!("cannot finish_input in session state {:?}", self.state),
            }),
        }
    }

    /// Returns the next globally sorted/merged record (`RETURN` verb), or `Ok(None)` at `AT END`.
    ///
    /// Automatically deletes temporary spill files and transitions to [`SortSessionState::Completed`]
    /// once the final record has been consumed.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::SortMerge`] if called before [`Self::finish_input`], after [`Self::abort`],
    /// or on corrupted spill frames, or [`CoblangError::Io`] on spill read failure.
    pub fn return_next(&mut self) -> Result<Option<Vec<u8>>> {
        match self.state {
            SortSessionState::ReturningOutput => {
                if let Some(cursor) = &mut self.merge_cursor {
                    if let Some(rec) = cursor.next_record(&self.comparator)? {
                        return Ok(Some(rec));
                    }
                    self.cleanup_spill_artifacts();
                    self.state = SortSessionState::Completed;
                    Ok(None)
                } else if let Some(rec) = self.in_memory_records.get(self.in_memory_cursor).cloned()
                {
                    self.in_memory_cursor += 1;
                    Ok(Some(rec))
                } else {
                    self.in_memory_records.clear();
                    self.in_memory_bytes = 0;
                    self.state = SortSessionState::Completed;
                    Ok(None)
                }
            }
            SortSessionState::Completed => Ok(None),
            SortSessionState::AcceptingInput
            | SortSessionState::AcceptingMergeRuns
            | SortSessionState::Aborted => Err(CoblangError::SortMerge {
                message: format!("cannot RETURN record in session state {:?}", self.state),
            }),
        }
    }

    /// Drains all remaining sorted/merged records from [`SortSessionState::ReturningOutput`] into a vector.
    ///
    /// # Errors
    ///
    /// Propagates any state, framing, or I/O error from [`Self::return_next`].
    pub fn collect_remaining(&mut self) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        while let Some(rec) = self.return_next()? {
            out.push(rec);
        }
        Ok(out)
    }

    /// Sorts `self.in_memory_records` using stable sort for `InOrder` and unstable sort for `Arbitrary`.
    fn sort_in_memory_buffer(&mut self) {
        let cmp = &self.comparator;
        if cmp.duplicates_order().is_in_order() {
            self.in_memory_records
                .sort_by(|lhs, rhs| cmp.compare_records(lhs, rhs));
        } else {
            self.in_memory_records
                .sort_unstable_by(|lhs, rhs| cmp.compare_records(lhs, rhs));
        }
    }

    /// Sorts `self.in_memory_records` and writes them out to a new numbered spill file.
    fn spill_in_memory_run(&mut self) -> Result<()> {
        self.sort_in_memory_buffer();
        self.ensure_session_dir()?;
        let path = self.allocate_spill_path();
        let records = core::mem::take(&mut self.in_memory_records);
        self.in_memory_bytes = 0;
        write_run_file(&path, &records)?;
        self.spill_files.push(path);
        self.total_spilled_runs += 1;
        Ok(())
    }

    /// Flushes `self.active_merge_records` (if open) to a numbered spill file.
    fn finish_active_merge_run(&mut self) -> Result<()> {
        if let Some(records) = self.active_merge_records.take() {
            self.ensure_session_dir()?;
            let path = self.allocate_spill_path();
            write_run_file(&path, &records)?;
            self.spill_files.push(path);
            self.total_spilled_runs += 1;
        }
        Ok(())
    }

    /// Lazily creates `self.session_dir` on disk before the first spill file write.
    fn ensure_session_dir(&mut self) -> Result<()> {
        if !self.dir_created {
            fs::create_dir_all(&self.session_dir)?;
            self.dir_created = true;
        }
        Ok(())
    }

    /// Allocates the next sequential spill file path inside `self.session_dir`.
    fn allocate_spill_path(&mut self) -> PathBuf {
        let path = self.next_spill_file_path();
        self.next_run_id += 1;
        path
    }

    /// Compacts `self.spill_files` down to `<= max_merge_fan_in` runs and opens `self.merge_cursor`.
    fn open_final_merge_cursor(&mut self) -> Result<()> {
        while self.spill_files.len() > self.config.max_merge_fan_in {
            let fan_in = self.config.max_merge_fan_in;
            let old_files = core::mem::take(&mut self.spill_files);
            let mut compacted = Vec::new();
            for chunk in old_files.chunks(fan_in) {
                if let [single_path] = chunk {
                    compacted.push(single_path.clone());
                } else {
                    let merged_path = self.allocate_spill_path();
                    merge_chunk_to_file(
                        chunk.to_vec(),
                        &merged_path,
                        self.config.record_bounds,
                        &self.comparator,
                    )?;
                    compacted.push(merged_path);
                }
            }
            self.spill_files = compacted;
        }

        let final_paths = core::mem::take(&mut self.spill_files);
        let cursor =
            KWayMergeCursor::open(final_paths, self.config.record_bounds, &self.comparator)?;
        self.merge_cursor = Some(cursor);
        Ok(())
    }

    /// Closes open readers, deletes all spill files, and removes `self.session_dir` if created.
    fn cleanup_spill_artifacts(&mut self) {
        if let Some(mut cursor) = self.merge_cursor.take() {
            cursor.remove_backing_files();
        }
        for path in self.spill_files.drain(..) {
            let _ = fs::remove_file(path);
        }
        if self.dir_created {
            let _ = fs::remove_dir_all(&self.session_dir);
            self.dir_created = false;
        }
    }
}

/// Entry inside [`MergeHeap`] pairing a record with its source run index (`0..fan_in`).
#[derive(Debug)]
struct HeapNode {
    /// Current head record bytes of `run_index`.
    record: Vec<u8>,
    /// Zero-based run ordinal used for deterministic FIFO / `MERGE USING` tie-breaking.
    run_index: usize,
}

/// Returns `true` when `lhs` should emerge before `rhs` during k-way merge.
///
/// # Arguments
///
/// * `lhs` - Candidate smaller node.
/// * `rhs` - Candidate larger node.
/// * `comparator` - Active [`SortComparator`].
fn heap_node_less(lhs: &HeapNode, rhs: &HeapNode, comparator: &SortComparator) -> bool {
    match comparator.compare_records(&lhs.record, &rhs.record) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => lhs.run_index < rhs.run_index,
    }
}

/// Zero-`Arc` binary min-heap ordering [`HeapNode`]s by `(comparator, run_index)`.
#[derive(Debug, Default)]
struct MergeHeap {
    /// Contiguous binary heap array (`nodes[0]` is minimum).
    nodes: Vec<HeapNode>,
}

impl MergeHeap {
    /// Inserts `node` into the min-heap and restores heap order.
    ///
    /// # Arguments
    ///
    /// * `node` - [`HeapNode`] to insert.
    /// * `comparator` - Active [`SortComparator`].
    fn push(&mut self, node: HeapNode, comparator: &SortComparator) {
        let mut idx = self.nodes.len();
        self.nodes.push(node);
        while idx > 0 {
            let parent = (idx - 1) / 2;
            let should_swap = self
                .nodes
                .get(idx)
                .zip(self.nodes.get(parent))
                .is_some_and(|(cur, par)| heap_node_less(cur, par, comparator));
            if should_swap {
                self.nodes.swap(idx, parent);
                idx = parent;
            } else {
                break;
            }
        }
    }

    /// Removes and returns the minimum [`HeapNode`], or `None` when empty.
    ///
    /// # Arguments
    ///
    /// * `comparator` - Active [`SortComparator`].
    fn pop_min(&mut self, comparator: &SortComparator) -> Option<HeapNode> {
        if self.nodes.is_empty() {
            return None;
        }
        let min_node = self.nodes.swap_remove(0);
        let len = self.nodes.len();
        let mut idx = 0;
        loop {
            let left = idx * 2 + 1;
            if left >= len {
                break;
            }
            let right = left + 1;
            let right_smaller = right < len
                && self
                    .nodes
                    .get(right)
                    .zip(self.nodes.get(left))
                    .is_some_and(|(r_node, l_node)| heap_node_less(r_node, l_node, comparator));
            let smallest_child = if right_smaller { right } else { left };
            let child_smaller = self
                .nodes
                .get(smallest_child)
                .zip(self.nodes.get(idx))
                .is_some_and(|(c_node, p_node)| heap_node_less(c_node, p_node, comparator));
            if child_smaller {
                self.nodes.swap(idx, smallest_child);
                idx = smallest_child;
            } else {
                break;
            }
        }
        Some(min_node)
    }
}

/// Streaming k-way merge cursor over open temporary spill run files.
#[derive(Debug)]
struct KWayMergeCursor {
    /// Buffered readers for each run in `0..paths.len()`.
    readers: Vec<BufReader<File>>,
    /// Backing spill file paths to delete once exhausted or aborted.
    paths: Vec<PathBuf>,
    /// Validated `SD` record bounds enforced on every read frame.
    bounds: RecordLengthBounds,
    /// Min-heap of current head records across active runs.
    heap: MergeHeap,
}

impl KWayMergeCursor {
    /// Opens `paths`, validates headers, and primes the min-heap with each non-empty run's first record.
    ///
    /// # Arguments
    ///
    /// * `paths` - Ordered spill run file paths.
    /// * `bounds` - Validated `SD` record length bounds.
    /// * `comparator` - Active [`SortComparator`].
    fn open(
        paths: Vec<PathBuf>,
        bounds: RecordLengthBounds,
        comparator: &SortComparator,
    ) -> Result<Self> {
        let mut readers = Vec::with_capacity(paths.len());
        let mut heap = MergeHeap::default();

        for (run_index, path) in paths.iter().enumerate() {
            let mut reader = open_spill_run(path)?;
            if let Some(first_record) = read_run_record(&mut reader, bounds)? {
                heap.push(
                    HeapNode {
                        record: first_record,
                        run_index,
                    },
                    comparator,
                );
            }
            readers.push(reader);
        }

        Ok(Self {
            readers,
            paths,
            bounds,
            heap,
        })
    }

    /// Pops the next globally smallest record and replenishes the heap from its source run.
    ///
    /// # Arguments
    ///
    /// * `comparator` - Active [`SortComparator`].
    fn next_record(&mut self, comparator: &SortComparator) -> Result<Option<Vec<u8>>> {
        let Some(min_node) = self.heap.pop_min(comparator) else {
            return Ok(None);
        };
        if let Some(next_rec) =
            read_next_from_slot(&mut self.readers, min_node.run_index, self.bounds)?
        {
            self.heap.push(
                HeapNode {
                    record: next_rec,
                    run_index: min_node.run_index,
                },
                comparator,
            );
        }
        Ok(Some(min_node.record))
    }

    /// Closes all open file handles and removes backing spill files from disk.
    fn remove_backing_files(&mut self) {
        self.readers.clear();
        for path in self.paths.drain(..) {
            let _ = fs::remove_file(path);
        }
    }
}

/// Reads the next framed record from `readers[run_index]`.
///
/// # Arguments
///
/// * `readers` - Slice of active run readers.
/// * `run_index` - Target run slot index.
/// * `bounds` - Validated `SD` record length bounds.
fn read_next_from_slot(
    readers: &mut [BufReader<File>],
    run_index: usize,
    bounds: RecordLengthBounds,
) -> Result<Option<Vec<u8>>> {
    readers
        .get_mut(run_index)
        .map_or(Ok(None), |reader| read_run_record(reader, bounds))
}

/// Encodes `records` with `SPILL_MAGIC` header, length-prefixed frames, and `0_u32` EOF sentinel,
/// writing the buffer to `path`.
///
/// # Arguments
///
/// * `path` - Destination spill file path.
/// * `records` - Ordered records to persist.
fn write_run_file(path: &Path, records: &[Vec<u8>]) -> Result<()> {
    let total_payload: usize = records.iter().map(|r| 4 + r.len()).sum();
    let mut buf = Vec::with_capacity(SPILL_MAGIC.len() + total_payload + 4);
    buf.extend_from_slice(SPILL_MAGIC);
    for rec in records {
        append_framed_record(&mut buf, rec);
    }
    buf.extend_from_slice(&0_u32.to_le_bytes());
    fs::write(path, buf).map_err(CoblangError::from)
}

/// Appends one `u32` little-endian length prefix + record bytes onto `buf`.
///
/// # Arguments
///
/// * `buf` - Destination byte buffer.
/// * `record` - Validated record bytes (`1..=64 MiB`).
fn append_framed_record(buf: &mut Vec<u8>, record: &[u8]) {
    let len_u32 = u32::try_from(record.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&len_u32.to_le_bytes());
    buf.extend_from_slice(record);
}

/// Merges `chunk_paths` via [`KWayMergeCursor`] into a single compacted spill file at `dest_path`,
/// removing the consumed `chunk_paths`.
///
/// # Arguments
///
/// * `chunk_paths` - Input spill run paths for this compaction batch.
/// * `dest_path` - Output compacted spill run path.
/// * `bounds` - Validated `SD` record bounds.
/// * `comparator` - Active [`SortComparator`].
fn merge_chunk_to_file(
    chunk_paths: Vec<PathBuf>,
    dest_path: &Path,
    bounds: RecordLengthBounds,
    comparator: &SortComparator,
) -> Result<()> {
    let mut cursor = KWayMergeCursor::open(chunk_paths, bounds, comparator)?;
    let mut out_bytes = Vec::from(SPILL_MAGIC);
    while let Some(rec) = cursor.next_record(comparator)? {
        append_framed_record(&mut out_bytes, &rec);
    }
    out_bytes.extend_from_slice(&0_u32.to_le_bytes());
    cursor.remove_backing_files();
    fs::write(dest_path, out_bytes).map_err(CoblangError::from)
}

/// Opens a spill run file and verifies its 8-byte `SPILL_MAGIC` header.
///
/// # Arguments
///
/// * `path` - Spill run file path.
fn open_spill_run(path: &Path) -> Result<BufReader<File>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut magic = [0_u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != SPILL_MAGIC {
        return Err(CoblangError::SortMerge {
            message: format!("invalid spill run header magic in `{}`", path.display()),
        });
    }
    Ok(reader)
}

/// Reads one length-prefixed record frame from `reader`, returning `Ok(None)` at the `0_u32` EOF sentinel.
///
/// # Arguments
///
/// * `reader` - Open spill run stream positioned at a frame boundary.
/// * `bounds` - Validated `SD` record length bounds.
fn read_run_record(reader: &mut dyn Read, bounds: RecordLengthBounds) -> Result<Option<Vec<u8>>> {
    let mut len_bytes = [0_u8; 4];
    reader.read_exact(&mut len_bytes)?;
    let raw_len = u32::from_le_bytes(len_bytes);
    if raw_len == 0 {
        return Ok(None);
    }
    let rec_len = usize::try_from(raw_len).unwrap_or(usize::MAX);
    if rec_len < bounds.min_bytes() || rec_len > bounds.max_bytes() {
        return Err(CoblangError::SortMerge {
            message: format!(
                "corrupt spill record length {rec_len} outside {}..={}",
                bounds.min_bytes(),
                bounds.max_bytes()
            ),
        });
    }
    let mut payload = vec![0_u8; rec_len];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}
