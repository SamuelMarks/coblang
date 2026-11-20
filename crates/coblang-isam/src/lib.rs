//! Pure-Rust 4096-byte CRC32 paged Order-$B$ B+Tree `INDEXED` (`ISAM`) storage engine (`Milestone M15`).

/// Order-$B$ B+Tree internal/leaf nodes, doubly-linked cursors, composite/FIFO keys, and COBOL `INDEXED` verbs.
pub mod btree;
/// Fixed 4096-byte `COBLISAM` page framing, IEEE CRC32 checksums, freelist reclamation, and schema metadata.
pub mod page;

pub use btree::{
    BTreeInternalNode, BTreeLeafNode, CloseDisposition, DiskIoFactory, FileOptionality,
    INTERNAL_PAYLOAD_CAPACITY, INTERNAL_UNDERFLOW_BYTES, IsamAccessMode, IsamFile, IsamOpenMode,
    LEAF_PAYLOAD_CAPACITY, LEAF_UNDERFLOW_BYTES, LeafEntry, LeafValue, MAX_INLINE_VALUE_BYTES,
    MemoryIoFactory, PageIoFactory, SeparatorKey, StartRelation, cmp_key_seq,
};
pub use page::{
    CollatingId, CollatingSequence, DiskAccessPerm, DiskPageIo, DuplicateKeyPolicy, FORMAT_VERSION,
    FRAME_HEADER_BYTES, FileHeaderState, IsamSchema, KeyIndexId, KeySegmentSpec, KeySpec,
    MAGIC_BYTES, MAX_ALTERNATE_KEYS, MAX_KEY_BYTES, MAX_KEY_SEGMENTS, MAX_RECORD_BYTES, MemoryDisk,
    MemoryPageIo, OVERFLOW_CHUNK_CAPACITY, PAGE_PAYLOAD_BYTES, PAGE_SIZE, PageId, PageIo, PageKind,
    Pager, RecordBounds, SlotIndex, build_framed_page, compute_page_crc32, crc32_bytes,
    crc32_update, decode_free_page, decode_overflow_page, encode_free_page, encode_overflow_page,
    verify_any_page_frame, verify_page_frame,
};
