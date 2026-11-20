//! Pure-Rust multi-key COBOL `SORT` and `MERGE` engine with custom `ALPHABET` collation
//! and bounded-memory k-way external spill-file merge (`Milestone M16`).

/// Single-byte 256-entry `ALPHABET` collation rank tables (`NATIVE`, `EBCDIC`, and custom `THRU`/`ALSO`).
pub mod alphabet;
/// Multi-key COBOL record comparator across alphanumeric, national, zoned, packed, binary, and float keys.
pub mod comparator;
/// Bounded-memory `SortSession` supporting in-memory sort, k-way external spill-merge, `MERGE`, and `SORT-RETURN`.
pub mod session;

pub use alphabet::{AlphabetSpecEntry, AlphabetTable};
pub use comparator::{DuplicatesOrder, KeyDirection, SortComparator, SortKeySpec, SortKeyType};
pub use session::{
    RecordLengthBounds, SortMemoryLimit, SortReturnStatus, SortSession, SortSessionConfig,
    SortSessionState,
};
