//! Pluggable random-access storage abstraction (`OsFileSystem` & `InMemoryFileSystem` with
//! deterministic fault injection) and multi-connector file/record lock registry (`"51"` & `"61"`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use coblang_common::{CoblangError, FileConnectorId, FileStatusKind, Result};

use crate::types::LockMode;

/// Physical file existence indicator returned by [`StorageBackend::path_exists`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilePresence {
    /// Physical file exists at the given path.
    Present,
    /// Physical file does not exist at the given path.
    Absent,
}

/// Concrete low-level open operation requested from a [`StorageBackend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageOpenRequest {
    /// Open existing file read-only at offset 0.
    ReadOnly,
    /// Create or truncate file for writing at offset 0.
    CreateTruncateWrite,
    /// Open existing file for reading and writing without truncation at offset 0.
    ReadWriteExisting,
    /// Open or create file for reading and writing without truncation at offset 0.
    ReadWriteCreate,
    /// Open existing file positioned at EOF for appending.
    AppendExisting,
    /// Open or create file positioned at EOF for appending.
    AppendCreate,
}

/// Outcome of a file-sharing (`"61"`) or record-lock (`"51"`) request on [`FileLockRegistry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockOutcome {
    /// Lock or sharing check succeeded.
    Granted,
    /// Contended with another connector; carries `"51"` or `"61"`.
    Denied(FileStatusKind),
}

/// Random-access byte stream supporting read, write, flush, and seek operations.
pub trait RandomAccessStream: std::fmt::Debug {
    /// Reads up to `buf.len()` bytes from the current cursor, returning `0` at EOF.
    ///
    /// # Arguments
    ///
    /// * `buf` - Destination byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] on underlying stream read failure.
    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Writes all bytes of `buf` at the current cursor.
    ///
    /// # Arguments
    ///
    /// * `buf` - Source byte slice to write completely.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] on underlying stream write failure.
    fn write_bytes(&mut self, buf: &[u8]) -> Result<()>;

    /// Flushes buffered writes to the underlying storage medium.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] on underlying stream flush failure.
    fn flush_bytes(&mut self) -> Result<()>;

    /// Repositions the stream cursor according to `pos` and returns the new 0-based byte offset.
    ///
    /// # Arguments
    ///
    /// * `pos` - Target [`SeekFrom`] specification.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] on underlying stream seek failure.
    fn seek_pos(&mut self, pos: SeekFrom) -> Result<u64>;
}

/// Pluggable filesystem backend enabling both native OS files and deterministic fault-injected tests.
pub trait StorageBackend: std::fmt::Debug {
    /// Checks whether `path` currently exists in this storage backend.
    ///
    /// # Arguments
    ///
    /// * `path` - Target filesystem path.
    fn path_exists(&self, path: &Path) -> FilePresence;

    /// Opens `path` using the requested [`StorageOpenRequest`] disposition.
    ///
    /// # Arguments
    ///
    /// * `path` - Target filesystem path.
    /// * `request` - Low-level open disposition.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] on underlying open failure.
    fn open_stream(
        &mut self,
        path: &Path,
        request: StorageOpenRequest,
    ) -> Result<Box<dyn RandomAccessStream>>;

    /// Deletes `path` from the storage backend (`DELETE FILE`).
    ///
    /// # Arguments
    ///
    /// * `path` - Target filesystem path to remove.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] on underlying removal failure.
    fn remove_path(&mut self, path: &Path) -> Result<()>;
}

impl RandomAccessStream for File {
    /// Reads bytes via [`Read::read`].
    ///
    /// # Arguments
    ///
    /// * `buf` - Destination buffer.
    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize> {
        Read::read(self, buf).map_err(CoblangError::Io)
    }

    /// Writes all bytes via [`Write::write_all`].
    ///
    /// # Arguments
    ///
    /// * `buf` - Source buffer.
    fn write_bytes(&mut self, buf: &[u8]) -> Result<()> {
        Write::write_all(self, buf).map_err(CoblangError::Io)
    }

    /// Flushes buffered data via [`Write::flush`].
    fn flush_bytes(&mut self) -> Result<()> {
        Write::flush(self).map_err(CoblangError::Io)
    }

    /// Seeks via [`Seek::seek`].
    ///
    /// # Arguments
    ///
    /// * `pos` - Target seek position.
    fn seek_pos(&mut self, pos: SeekFrom) -> Result<u64> {
        Seek::seek(self, pos).map_err(CoblangError::Io)
    }
}

/// Native POSIX / host OS filesystem backend backed by [`std::fs::File`].
#[derive(Debug, Clone, Copy, Default)]
pub struct OsFileSystem;

impl StorageBackend for OsFileSystem {
    /// Queries [`Path::exists`] on the host OS.
    ///
    /// # Arguments
    ///
    /// * `path` - Target filesystem path.
    fn path_exists(&self, path: &Path) -> FilePresence {
        if path.exists() {
            FilePresence::Present
        } else {
            FilePresence::Absent
        }
    }

    /// Opens `path` using [`OpenOptions`] matching `request`.
    ///
    /// # Arguments
    ///
    /// * `path` - Target filesystem path.
    /// * `request` - Low-level open disposition.
    fn open_stream(
        &mut self,
        path: &Path,
        request: StorageOpenRequest,
    ) -> Result<Box<dyn RandomAccessStream>> {
        let mut opts = OpenOptions::new();
        match request {
            StorageOpenRequest::ReadOnly => {
                opts.read(true);
            }
            StorageOpenRequest::CreateTruncateWrite => {
                opts.write(true).create(true).truncate(true);
            }
            StorageOpenRequest::ReadWriteExisting => {
                opts.read(true).write(true);
            }
            StorageOpenRequest::ReadWriteCreate => {
                opts.read(true).write(true).create(true).truncate(false);
            }
            StorageOpenRequest::AppendExisting => {
                opts.write(true).append(true);
            }
            StorageOpenRequest::AppendCreate => {
                opts.write(true).create(true).append(true);
            }
        }
        let file = opts.open(path).map_err(CoblangError::Io)?;
        Ok(Box::new(file))
    }

    /// Removes `path` via [`std::fs::remove_file`].
    ///
    /// # Arguments
    ///
    /// * `path` - Target filesystem path.
    fn remove_path(&mut self, path: &Path) -> Result<()> {
        std::fs::remove_file(path).map_err(CoblangError::Io)
    }
}

/// Scheduled fault-injection rule inside [`InMemoryFileSystem`].
#[derive(Debug, Clone, Copy)]
struct ScheduledFault {
    /// Remaining successful I/O operations before returning `error_kind`.
    ops_remaining: usize,
    /// [`ErrorKind`] injected once `ops_remaining` reaches `0`.
    error_kind: ErrorKind,
}

/// Shared mutable state inside [`InMemoryFileSystem`].
#[derive(Debug, Default)]
struct InMemoryState {
    /// File byte buffers keyed by path.
    files: BTreeMap<PathBuf, Rc<RefCell<Vec<u8>>>>,
    /// Paths marked read-only (simulating `ErrorKind::PermissionDenied` on write/remove).
    read_only_paths: BTreeSet<PathBuf>,
    /// Optional countdown fault injector triggered on open/read/write/flush/seek/remove.
    scheduled_fault: Option<ScheduledFault>,
}

impl InMemoryState {
    /// Steps the fault countdown, returning an injected [`CoblangError::Io`] when armed at `0`.
    fn check_fault(&mut self) -> Result<()> {
        let Some(mut fault) = self.scheduled_fault else {
            return Ok(());
        };
        if fault.ops_remaining == 0 {
            self.scheduled_fault = None;
            Err(CoblangError::Io(io::Error::new(
                fault.error_kind,
                "injected storage fault",
            )))
        } else {
            fault.ops_remaining -= 1;
            self.scheduled_fault = Some(fault);
            Ok(())
        }
    }
}

/// Deterministic in-memory filesystem backend supporting multi-handle sharing and fault injection.
#[derive(Debug, Clone, Default)]
pub struct InMemoryFileSystem {
    /// Shared interior state across cloned handles (so two engines can share one virtual disk).
    state: Rc<RefCell<InMemoryState>>,
}

impl InMemoryFileSystem {
    /// Creates an empty [`InMemoryFileSystem`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-populates `path` with `bytes`.
    ///
    /// # Arguments
    ///
    /// * `path` - Virtual file path.
    /// * `bytes` - Initial file contents.
    pub fn seed_file(&self, path: &Path, bytes: &[u8]) {
        self.state
            .borrow_mut()
            .files
            .insert(PathBuf::from(path), Rc::new(RefCell::new(bytes.to_vec())));
    }

    /// Reads a snapshot of the current raw bytes stored at `path`, if present.
    ///
    /// # Arguments
    ///
    /// * `path` - Virtual file path.
    #[must_use]
    pub fn snapshot(&self, path: &Path) -> Option<Vec<u8>> {
        self.state
            .borrow()
            .files
            .get(path)
            .map(|buf| buf.borrow().clone())
    }

    /// Marks `path` as read-only so write/create/truncate/append/remove attempts return `PermissionDenied`.
    ///
    /// # Arguments
    ///
    /// * `path` - Virtual file path to mark read-only.
    pub fn mark_read_only(&self, path: &Path) {
        self.state
            .borrow_mut()
            .read_only_paths
            .insert(PathBuf::from(path));
    }

    /// Arms a one-shot fault that will fail with `error_kind` after `ops_before_failure` operations.
    ///
    /// # Arguments
    ///
    /// * `ops_before_failure` - Count of operations allowed to succeed before failing (`0` fails next op).
    /// * `error_kind` - [`ErrorKind`] to inject.
    pub fn inject_fault_after(&self, ops_before_failure: usize, error_kind: ErrorKind) {
        self.state.borrow_mut().scheduled_fault = Some(ScheduledFault {
            ops_remaining: ops_before_failure,
            error_kind,
        });
    }
}

/// Stream modes tracked by [`InMemoryStream`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InMemoryWriteMode {
    /// Stream disallows writes.
    ReadOnly,
    /// Writes overwrite at current `cursor`.
    PositionedWrite,
    /// Writes always append at buffer end.
    AppendAtEnd,
}

/// Open stream handle into an [`InMemoryFileSystem`] buffer.
#[derive(Debug)]
pub struct InMemoryStream {
    /// Shared filesystem state for fault injection checks.
    fs_state: Rc<RefCell<InMemoryState>>,
    /// Shared byte buffer of the opened virtual file.
    buffer: Rc<RefCell<Vec<u8>>>,
    /// Current 0-based stream cursor.
    cursor: u64,
    /// Write mode (`ReadOnly`, `PositionedWrite`, `AppendAtEnd`).
    write_mode: InMemoryWriteMode,
}

impl RandomAccessStream for InMemoryStream {
    /// Reads bytes from the in-memory vector starting at `self.cursor`.
    ///
    /// # Arguments
    ///
    /// * `buf` - Destination slice.
    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.fs_state.borrow_mut().check_fault()?;
        let data = self.buffer.borrow();
        let start = usize::try_from(self.cursor).unwrap_or(data.len());
        let avail = data.get(start..).unwrap_or(&[]);
        let n = avail.len().min(buf.len());
        for (dst, &src) in buf.iter_mut().zip(avail.iter().take(n)) {
            *dst = src;
        }
        self.cursor = self.cursor.saturating_add(n as u64);
        Ok(n)
    }

    /// Writes `buf` into the in-memory vector at `self.cursor` (or EOF in append mode).
    ///
    /// # Arguments
    ///
    /// * `buf` - Source slice.
    fn write_bytes(&mut self, buf: &[u8]) -> Result<()> {
        self.fs_state.borrow_mut().check_fault()?;
        match self.write_mode {
            InMemoryWriteMode::ReadOnly => Err(CoblangError::Io(io::Error::new(
                ErrorKind::PermissionDenied,
                "stream is read-only",
            ))),
            InMemoryWriteMode::AppendAtEnd => {
                let mut data = self.buffer.borrow_mut();
                data.extend_from_slice(buf);
                self.cursor = data.len() as u64;
                Ok(())
            }
            InMemoryWriteMode::PositionedWrite => {
                let mut data = self.buffer.borrow_mut();
                let start = usize::try_from(self.cursor).unwrap_or(usize::MAX);
                let end = start.checked_add(buf.len()).ok_or_else(|| {
                    CoblangError::Io(io::Error::new(ErrorKind::FileTooLarge, "length overflow"))
                })?;
                if data.len() < end {
                    data.resize(end, 0);
                }
                for (dst, &src) in data.iter_mut().skip(start).zip(buf.iter()) {
                    *dst = src;
                }
                self.cursor = end as u64;
                Ok(())
            }
        }
    }

    /// Checks fault injection on flush.
    fn flush_bytes(&mut self) -> Result<()> {
        self.fs_state.borrow_mut().check_fault()
    }

    /// Updates `self.cursor` according to `pos`.
    ///
    /// # Arguments
    ///
    /// * `pos` - Target seek position.
    fn seek_pos(&mut self, pos: SeekFrom) -> Result<u64> {
        self.fs_state.borrow_mut().check_fault()?;
        let len = self.buffer.borrow().len() as u64;
        let next = match pos {
            SeekFrom::Start(off) => Some(off),
            SeekFrom::End(delta) => len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.cursor.checked_add_signed(delta),
        }
        .ok_or_else(|| {
            CoblangError::Io(io::Error::new(
                ErrorKind::InvalidInput,
                "negative or overflowing seek",
            ))
        })?;
        self.cursor = next;
        Ok(next)
    }
}

impl StorageBackend for InMemoryFileSystem {
    /// Checks whether `path` is present in the virtual file map.
    ///
    /// # Arguments
    ///
    /// * `path` - Virtual file path.
    fn path_exists(&self, path: &Path) -> FilePresence {
        if self.state.borrow().files.contains_key(path) {
            FilePresence::Present
        } else {
            FilePresence::Absent
        }
    }

    /// Opens or creates an [`InMemoryStream`] matching `request`.
    ///
    /// # Arguments
    ///
    /// * `path` - Virtual file path.
    /// * `request` - Low-level open disposition.
    fn open_stream(
        &mut self,
        path: &Path,
        request: StorageOpenRequest,
    ) -> Result<Box<dyn RandomAccessStream>> {
        let mut st = self.state.borrow_mut();
        st.check_fault()?;
        let needs_write = request != StorageOpenRequest::ReadOnly;
        if needs_write && st.read_only_paths.contains(path) {
            return Err(CoblangError::Io(io::Error::new(
                ErrorKind::PermissionDenied,
                "virtual path is marked read-only",
            )));
        }

        let (buffer, cursor, write_mode) = match request {
            StorageOpenRequest::ReadOnly => {
                let buf = st.files.get(path).cloned().ok_or_else(|| {
                    CoblangError::Io(io::Error::new(
                        ErrorKind::NotFound,
                        "virtual file not found",
                    ))
                })?;
                (buf, 0, InMemoryWriteMode::ReadOnly)
            }
            StorageOpenRequest::CreateTruncateWrite => {
                let buf = st
                    .files
                    .entry(PathBuf::from(path))
                    .or_insert_with(|| Rc::new(RefCell::new(Vec::new())))
                    .clone();
                buf.borrow_mut().clear();
                (buf, 0, InMemoryWriteMode::PositionedWrite)
            }
            StorageOpenRequest::ReadWriteExisting => {
                let buf = st.files.get(path).cloned().ok_or_else(|| {
                    CoblangError::Io(io::Error::new(
                        ErrorKind::NotFound,
                        "virtual file not found",
                    ))
                })?;
                (buf, 0, InMemoryWriteMode::PositionedWrite)
            }
            StorageOpenRequest::ReadWriteCreate => {
                let buf = st
                    .files
                    .entry(PathBuf::from(path))
                    .or_insert_with(|| Rc::new(RefCell::new(Vec::new())))
                    .clone();
                (buf, 0, InMemoryWriteMode::PositionedWrite)
            }
            StorageOpenRequest::AppendExisting => {
                let buf = st.files.get(path).cloned().ok_or_else(|| {
                    CoblangError::Io(io::Error::new(
                        ErrorKind::NotFound,
                        "virtual file not found",
                    ))
                })?;
                let end = buf.borrow().len() as u64;
                (buf, end, InMemoryWriteMode::AppendAtEnd)
            }
            StorageOpenRequest::AppendCreate => {
                let buf = st
                    .files
                    .entry(PathBuf::from(path))
                    .or_insert_with(|| Rc::new(RefCell::new(Vec::new())))
                    .clone();
                let end = buf.borrow().len() as u64;
                (buf, end, InMemoryWriteMode::AppendAtEnd)
            }
        };

        drop(st);
        Ok(Box::new(InMemoryStream {
            fs_state: Rc::clone(&self.state),
            buffer,
            cursor,
            write_mode,
        }))
    }

    /// Removes `path` from the virtual file map.
    ///
    /// # Arguments
    ///
    /// * `path` - Virtual file path.
    fn remove_path(&mut self, path: &Path) -> Result<()> {
        let mut st = self.state.borrow_mut();
        st.check_fault()?;
        if st.read_only_paths.contains(path) {
            return Err(CoblangError::Io(io::Error::new(
                ErrorKind::PermissionDenied,
                "virtual path is marked read-only",
            )));
        }
        st.files.remove(path).map(|_| ()).ok_or_else(|| {
            CoblangError::Io(io::Error::new(
                ErrorKind::NotFound,
                "virtual file not found",
            ))
        })
    }
}

/// Active open-file and record-lock state for a single physical path inside [`FileLockRegistry`].
#[derive(Debug, Default)]
struct PathLockEntry {
    /// Active `(connector_id, lock_mode)` pairs currently holding this path open.
    open_connectors: BTreeMap<FileConnectorId, LockMode>,
    /// Record locks mapped from 1-based record/slot key to owning `FileConnectorId`.
    record_locks: BTreeMap<u64, FileConnectorId>,
}

/// Shared multi-connector file-sharing (`"61"`) and record-locking (`"51"`) coordinator.
#[derive(Debug, Clone, Default)]
pub struct FileLockRegistry {
    /// Interior lock table keyed by resolved physical [`PathBuf`].
    entries: Rc<RefCell<BTreeMap<PathBuf, PathLockEntry>>>,
}

impl FileLockRegistry {
    /// Creates a new empty [`FileLockRegistry`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquires file-open registration for `(path, connector, mode)` or denies with `"61"`.
    ///
    /// # Arguments
    ///
    /// * `path` - Physical file path being opened.
    /// * `connector` - Unique [`FileConnectorId`] opening the file.
    /// * `mode` - Connector [`LockMode`] (`Automatic`, `Manual`, or `Exclusive`).
    #[must_use]
    pub fn register_open(
        &self,
        path: &Path,
        connector: FileConnectorId,
        mode: LockMode,
    ) -> LockOutcome {
        let mut map = self.entries.borrow_mut();
        let entry = map.entry(PathBuf::from(path)).or_default();
        let has_conflict = entry
            .open_connectors
            .iter()
            .any(|(&other_id, &other_mode)| {
                other_id != connector
                    && (mode == LockMode::Exclusive || other_mode == LockMode::Exclusive)
            });
        if has_conflict {
            return LockOutcome::Denied(FileStatusKind::FileSharingConflict61);
        }
        entry.open_connectors.insert(connector, mode);
        LockOutcome::Granted
    }

    /// Releases all record locks AND the file-open registration held by `connector` on `path`.
    ///
    /// # Arguments
    ///
    /// * `path` - Physical file path being closed.
    /// * `connector` - Closing [`FileConnectorId`].
    pub fn unregister_close(&self, path: &Path, connector: FileConnectorId) {
        let mut map = self.entries.borrow_mut();
        let should_remove = if let Some(entry) = map.get_mut(path) {
            entry.open_connectors.remove(&connector);
            entry
                .record_locks
                .retain(|_, &mut owner| owner != connector);
            entry.open_connectors.is_empty() && entry.record_locks.is_empty()
        } else {
            false
        };
        if should_remove {
            map.remove(path);
        }
    }

    /// Checks whether `record_key` on `path` is currently locked by a DIFFERENT connector (`"51"`).
    ///
    /// # Arguments
    ///
    /// * `path` - Physical file path.
    /// * `connector` - Requesting [`FileConnectorId`].
    /// * `record_key` - One-based slot or record key.
    #[must_use]
    pub fn check_not_locked_by_other(
        &self,
        path: &Path,
        connector: FileConnectorId,
        record_key: u64,
    ) -> LockOutcome {
        let map = self.entries.borrow();
        if let Some(entry) = map.get(path)
            && let Some(&owner) = entry.record_locks.get(&record_key)
            && owner != connector
        {
            return LockOutcome::Denied(FileStatusKind::RecordLocked51);
        }
        LockOutcome::Granted
    }

    /// Acquires a record lock on `record_key` for `connector`, denying with `"51"` on contention.
    ///
    /// # Arguments
    ///
    /// * `path` - Physical file path.
    /// * `connector` - Requesting [`FileConnectorId`].
    /// * `record_key` - One-based slot or record key.
    #[must_use]
    pub fn acquire_record_lock(
        &self,
        path: &Path,
        connector: FileConnectorId,
        record_key: u64,
    ) -> LockOutcome {
        if let LockOutcome::Denied(kind) =
            self.check_not_locked_by_other(path, connector, record_key)
        {
            return LockOutcome::Denied(kind);
        }
        let mut map = self.entries.borrow_mut();
        let entry = map.entry(PathBuf::from(path)).or_default();
        entry.record_locks.insert(record_key, connector);
        LockOutcome::Granted
    }

    /// Releases a single record lock on `record_key` if currently owned by `connector`.
    ///
    /// # Arguments
    ///
    /// * `path` - Physical file path.
    /// * `connector` - Owning [`FileConnectorId`].
    /// * `record_key` - One-based slot or record key to unlock.
    pub fn release_single_lock(&self, path: &Path, connector: FileConnectorId, record_key: u64) {
        let mut map = self.entries.borrow_mut();
        if let Some(entry) = map.get_mut(path)
            && entry.record_locks.get(&record_key) == Some(&connector)
        {
            entry.record_locks.remove(&record_key);
        }
    }

    /// Releases all record locks currently held by `connector` on `path` (`UNLOCK file`).
    ///
    /// # Arguments
    ///
    /// * `path` - Physical file path.
    /// * `connector` - Owning [`FileConnectorId`].
    pub fn release_all_record_locks(&self, path: &Path, connector: FileConnectorId) {
        let mut map = self.entries.borrow_mut();
        if let Some(entry) = map.get_mut(path) {
            entry
                .record_locks
                .retain(|_, &mut owner| owner != connector);
        }
    }

    /// Returns the count of record locks currently held by `connector` on `path`.
    ///
    /// # Arguments
    ///
    /// * `path` - Physical file path.
    /// * `connector` - Queried [`FileConnectorId`].
    #[must_use]
    pub fn held_lock_count(&self, path: &Path, connector: FileConnectorId) -> usize {
        self.entries.borrow().get(path).map_or(0, |entry| {
            entry
                .record_locks
                .values()
                .filter(|&&owner| owner == connector)
                .count()
        })
    }
}

/// Reads repeatedly from `stream` until `buf` is filled or physical EOF (`0` bytes) is reached.
///
/// # Arguments
///
/// * `stream` - Source [`RandomAccessStream`].
/// * `buf` - Destination slice to fill.
///
/// # Errors
///
/// Propagates any [`CoblangError::Io`] from [`RandomAccessStream::read_bytes`].
pub(crate) fn read_up_to_full(
    stream: &mut dyn RandomAccessStream,
    buf: &mut [u8],
) -> Result<usize> {
    let mut total = 0_usize;
    while total < buf.len() {
        let tail = buf.get_mut(total..).unwrap_or(&mut []);
        let n = stream.read_bytes(tail)?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n);
    }
    Ok(total)
}
