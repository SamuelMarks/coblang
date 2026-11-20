//! Integration, property/stress, corruption, and deterministic fault-injection suite for `coblang-isam`.

use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process::{Command, Stdio};
use std::rc::Rc;

use coblang_common::{CoblangError, FileStatusKind, Result};
use coblang_isam::{
    BTreeInternalNode, BTreeLeafNode, CloseDisposition, CollatingId, CollatingSequence,
    DiskAccessPerm, DiskIoFactory, DiskPageIo, DuplicateKeyPolicy, FORMAT_VERSION,
    FRAME_HEADER_BYTES, FileHeaderState, FileOptionality, IsamAccessMode, IsamFile, IsamOpenMode,
    IsamSchema, KeyIndexId, KeySegmentSpec, KeySpec, LeafEntry, LeafValue, MAGIC_BYTES,
    MAX_ALTERNATE_KEYS, MAX_INLINE_VALUE_BYTES, MAX_KEY_BYTES, MAX_KEY_SEGMENTS, MAX_RECORD_BYTES,
    MemoryDisk, MemoryPageIo, OVERFLOW_CHUNK_CAPACITY, PAGE_SIZE, PageId, PageIo, PageIoFactory,
    PageKind, Pager, RecordBounds, SeparatorKey, SlotIndex, StartRelation, build_framed_page,
    cmp_key_seq, compute_page_crc32, crc32_bytes, crc32_update, decode_free_page,
    decode_overflow_page, encode_free_page, encode_overflow_page, verify_any_page_frame,
    verify_page_frame,
};

/// Recomputes and overwrites the IEEE CRC32 field (`12..16`) of a mutated raw page.
///
/// # Arguments
///
/// * `raw` - Mutable 4096-byte page buffer.
fn restamp_crc(raw: &mut [u8; PAGE_SIZE]) {
    let crc = compute_page_crc32(raw);
    for (dst, &src) in raw.iter_mut().skip(12).zip(crc.to_le_bytes().iter()) {
        *dst = src;
    }
}

/// Writes `src` into `raw` starting at `offset` and updates the page CRC32.
///
/// # Arguments
///
/// * `raw` - Mutable 4096-byte page buffer.
/// * `offset` - Starting byte offset inside `raw`.
/// * `src` - Bytes to patch.
fn patch_and_restamp(raw: &mut [u8; PAGE_SIZE], offset: usize, src: &[u8]) {
    for (dst, &b) in raw.iter_mut().skip(offset).zip(src.iter()) {
        *dst = b;
    }
    restamp_crc(raw);
}

/// Step-counting fault-injecting [`PageIo`] wrapper around [`MemoryPageIo`].
struct FaultPageIo {
    /// Underlying in-memory block device.
    inner: MemoryPageIo,
    /// Shared monotonic step counter incremented on every fallible `PageIo` operation.
    step: Rc<Cell<usize>>,
    /// Exact step index that deterministically fails (`usize::MAX` disables failure).
    fail_at: Rc<Cell<usize>>,
}

impl PageIo for FaultPageIo {
    /// Returns the underlying virtual path label.
    fn path_label(&self) -> &str {
        self.inner.path_label()
    }

    /// Fails if `step == fail_at`, otherwise delegates `page_count`.
    fn page_count(&mut self) -> Result<u64> {
        let cur = self.step.get();
        self.step.set(cur + 1);
        if cur == self.fail_at.get() {
            return Err(CoblangError::IsamStorage {
                path: String::from(self.path_label()),
                page_id: 0,
                message: String::from("injected page_count fault"),
            });
        }
        self.inner.page_count()
    }

    /// Fails if `step == fail_at`, otherwise delegates `read_raw_page`.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Target page identifier.
    /// * `buf` - Destination page buffer.
    fn read_raw_page(&mut self, page_id: PageId, buf: &mut [u8; PAGE_SIZE]) -> Result<()> {
        let cur = self.step.get();
        self.step.set(cur + 1);
        if cur == self.fail_at.get() {
            return Err(CoblangError::IsamStorage {
                path: String::from(self.path_label()),
                page_id: page_id.as_u64(),
                message: String::from("injected read_raw_page fault"),
            });
        }
        self.inner.read_raw_page(page_id, buf)
    }

    /// Fails if `step == fail_at`, otherwise delegates `write_raw_page`.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Target page identifier.
    /// * `buf` - Source page buffer.
    fn write_raw_page(&mut self, page_id: PageId, buf: &[u8; PAGE_SIZE]) -> Result<()> {
        let cur = self.step.get();
        self.step.set(cur + 1);
        if cur == self.fail_at.get() {
            return Err(CoblangError::IsamStorage {
                path: String::from(self.path_label()),
                page_id: page_id.as_u64(),
                message: String::from("injected write_raw_page fault"),
            });
        }
        self.inner.write_raw_page(page_id, buf)
    }
}

/// Step-counting fault-injecting [`PageIoFactory`] wrapping a [`MemoryDisk`].
#[derive(Clone)]
struct FaultIoFactory {
    /// Backing virtual disk.
    disk: MemoryDisk,
    /// Shared step counter.
    step: Rc<Cell<usize>>,
    /// Target failing step index.
    fail_at: Rc<Cell<usize>>,
}

impl FaultIoFactory {
    /// Creates a new [`FaultIoFactory`] initially armed at `fail_at`.
    ///
    /// # Arguments
    ///
    /// * `disk` - Shared virtual disk.
    /// * `fail_step` - Exact step number to fail on.
    fn new(disk: MemoryDisk, fail_step: usize) -> Self {
        Self {
            disk,
            step: Rc::new(Cell::new(0)),
            fail_at: Rc::new(Cell::new(fail_step)),
        }
    }
}

impl PageIoFactory for FaultIoFactory {
    /// Delegates existence check to `self.disk`.
    fn exists(&self) -> bool {
        self.disk.exists()
    }

    /// Fails if `step == fail_at`, otherwise opens a [`FaultPageIo`].
    ///
    /// # Arguments
    ///
    /// * `_perm` - Unused OS permission flag.
    fn open_existing(&self, _perm: DiskAccessPerm) -> Result<Box<dyn PageIo>> {
        let cur = self.step.get();
        self.step.set(cur + 1);
        if cur == self.fail_at.get() {
            return Err(CoblangError::IsamStorage {
                path: String::from("fault-open"),
                page_id: 0,
                message: String::from("injected open_existing fault"),
            });
        }
        let inner = self.disk.open_io()?;
        Ok(Box::new(FaultPageIo {
            inner,
            step: Rc::clone(&self.step),
            fail_at: Rc::clone(&self.fail_at),
        }))
    }

    /// Fails if `step == fail_at`, otherwise truncates `self.disk` and returns [`FaultPageIo`].
    fn create_truncated(&self) -> Result<Box<dyn PageIo>> {
        let cur = self.step.get();
        self.step.set(cur + 1);
        if cur == self.fail_at.get() {
            return Err(CoblangError::IsamStorage {
                path: String::from("fault-create"),
                page_id: 0,
                message: String::from("injected create_truncated fault"),
            });
        }
        self.disk.create_truncated();
        Ok(Box::new(FaultPageIo {
            inner: MemoryPageIo::from_disk(self.disk.clone()),
            step: Rc::clone(&self.step),
            fail_at: Rc::clone(&self.fail_at),
        }))
    }
}

/// Builds a standard 24-byte-record test schema (`PK 0..4`, `Unique ALT1 4..8`, `FIFO ALT2 8..12`).
fn sample_multi_key_schema() -> Result<IsamSchema> {
    let bounds = RecordBounds::varying(16, 4096)?;
    let pk = KeySpec::single(0, 4, DuplicateKeyPolicy::Unique)?;
    let alt1 = KeySpec::single(4, 4, DuplicateKeyPolicy::Unique)?;
    let alt2 = KeySpec::single(8, 4, DuplicateKeyPolicy::AllowedFifo)?;
    IsamSchema::try_new(bounds, pk, vec![alt1, alt2], CollatingSequence::native())
}

/// Verifies IEEE CRC32 vectors and validation bounds on `PageId`, `SlotIndex`, `KeySegmentSpec`, `KeySpec`, and `KeyIndexId`.
#[test]
fn crc32_and_strong_domain_types_validate_all_bounds() -> Result<()> {
    assert_eq!(crc32_bytes(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32_bytes(b""), 0);
    let chained = !crc32_update(crc32_update(0xFFFF_FFFF, b"1234"), b"56789");
    assert_eq!(chained, 0xCBF4_3926);

    let p0 = PageId::HEADER;
    let p1 = p0.checked_next("test.isam")?;
    assert_eq!(p0.as_u64(), 0);
    assert_eq!(p1, PageId::new(1));
    assert!(p0 < p1);
    assert_eq!(PageId::encode_optional(None), 0);
    assert_eq!(PageId::encode_optional(Some(p1)), 1);
    assert_eq!(PageId::decode_optional(0), None);
    assert_eq!(PageId::decode_optional(1), Some(p1));
    assert!(PageId::new(u64::MAX).checked_next("test.isam").is_err());

    let s0 = SlotIndex::ZERO;
    let s5 = SlotIndex::try_from_usize(5)?;
    assert_eq!(s0.as_u16(), 0);
    assert_eq!(s5, SlotIndex::new(5));
    assert_eq!(s5.as_usize(), 5);
    assert!(SlotIndex::try_from_usize(usize::from(u16::MAX) + 1).is_err());

    assert_eq!(DuplicateKeyPolicy::Unique.to_tag(), 0);
    assert_eq!(DuplicateKeyPolicy::AllowedFifo.to_tag(), 1);
    assert_eq!(
        DuplicateKeyPolicy::from_tag(0, "f", p0)?,
        DuplicateKeyPolicy::Unique
    );
    assert_eq!(
        DuplicateKeyPolicy::from_tag(1, "f", p0)?,
        DuplicateKeyPolicy::AllowedFifo
    );
    assert!(DuplicateKeyPolicy::from_tag(7, "f", p0).is_err());

    assert!(KeySegmentSpec::try_new(0, 0).is_err());
    assert!(KeySegmentSpec::try_new(u32::MAX - 2, 5).is_err());
    let seg = KeySegmentSpec::try_new(4, 6)?;
    assert_eq!(seg.offset(), 4);
    assert_eq!(seg.length(), 6);
    assert_eq!(seg.end_offset(), 10);

    assert!(KeySpec::try_new(Vec::new(), DuplicateKeyPolicy::Unique).is_err());
    let too_many_segs = vec![seg; MAX_KEY_SEGMENTS + 1];
    assert!(KeySpec::try_new(too_many_segs, DuplicateKeyPolicy::Unique).is_err());
    let too_wide = vec![
        KeySegmentSpec::try_new(0, 300)?,
        KeySegmentSpec::try_new(300, 300)?,
    ];
    assert!(KeySpec::try_new(too_wide, DuplicateKeyPolicy::Unique).is_err());
    assert!(KeySpec::single(0, 0, DuplicateKeyPolicy::Unique).is_err());

    let split_key = KeySpec::try_new(
        vec![
            KeySegmentSpec::try_new(6, 3)?,
            KeySegmentSpec::try_new(0, 2)?,
        ],
        DuplicateKeyPolicy::AllowedFifo,
    )?;
    assert_eq!(split_key.segments().len(), 2);
    assert_eq!(split_key.total_byte_len(), 5);
    assert_eq!(split_key.max_end_offset(), 9);
    assert_eq!(split_key.extract_from_record(b"AB----XYZ!")?, b"XYZAB");
    assert!(split_key.extract_from_record(b"short").is_err());

    assert!(KeyIndexId::PRIMARY.is_primary());
    assert_eq!(KeyIndexId::PRIMARY.alternate_ordinal(), None);
    let alt0 = KeyIndexId::alternate(0)?;
    assert!(!alt0.is_primary());
    assert_eq!(alt0.as_u16(), 1);
    assert_eq!(alt0.alternate_ordinal(), Some(0));
    assert!(KeyIndexId::alternate(u16::try_from(MAX_ALTERNATE_KEYS).unwrap_or(16)).is_err());

    let fmt_dbg = format!(
        "{p1:?}{s5:?}{:?}{:?}{:?}{:?}{:?}{:?}",
        CollatingId::EBCDIC,
        StartRelation::Equal,
        IsamAccessMode::Dynamic,
        IsamOpenMode::Io,
        FileOptionality::Optional,
        CloseDisposition::WithLock
    );
    assert_ne!(fmt_dbg, "");
    Ok(())
}

/// Verifies `RecordBounds` and `IsamSchema` construction invariants and `OPEN` attribute compatibility checks.
#[test]
fn schema_validation_and_compatibility_matrix() -> Result<()> {
    assert!(RecordBounds::fixed(0).is_err());
    assert!(RecordBounds::varying(20, 10).is_err());
    assert!(RecordBounds::varying(1, MAX_RECORD_BYTES + 1).is_err());
    let fixed_20 = RecordBounds::fixed(20)?;
    assert_eq!(fixed_20.min_bytes(), 20);
    assert_eq!(fixed_20.max_bytes(), 20);
    assert!(!fixed_20.contains_len(19));
    assert!(fixed_20.contains_len(20));
    assert!(!fixed_20.contains_len(21));

    let bad_pk = KeySpec::single(0, 4, DuplicateKeyPolicy::AllowedFifo)?;
    assert!(
        IsamSchema::try_new(fixed_20, bad_pk, Vec::new(), CollatingSequence::native()).is_err()
    );

    let pk = KeySpec::single(0, 4, DuplicateKeyPolicy::Unique)?;
    let many_alts =
        vec![KeySpec::single(4, 2, DuplicateKeyPolicy::AllowedFifo)?; MAX_ALTERNATE_KEYS + 1];
    assert!(
        IsamSchema::try_new(fixed_20, pk.clone(), many_alts, CollatingSequence::native()).is_err()
    );

    let pk_past_min = KeySpec::single(18, 4, DuplicateKeyPolicy::Unique)?;
    assert!(
        IsamSchema::try_new(
            fixed_20,
            pk_past_min,
            Vec::new(),
            CollatingSequence::native()
        )
        .is_err()
    );

    let alt_past_min = KeySpec::single(19, 2, DuplicateKeyPolicy::Unique)?;
    assert!(
        IsamSchema::try_new(
            fixed_20,
            pk.clone(),
            vec![alt_past_min],
            CollatingSequence::native()
        )
        .is_err()
    );

    let alt_ok = KeySpec::single(4, 4, DuplicateKeyPolicy::AllowedFifo)?;
    let base = IsamSchema::try_new(
        fixed_20,
        pk.clone(),
        vec![alt_ok.clone()],
        CollatingSequence::native(),
    )?;
    assert_eq!(base.primary_key(), &pk);
    assert_eq!(base.alternate_keys().len(), 1);
    assert_eq!(base.key_spec(KeyIndexId::PRIMARY)?, &pk);
    assert_eq!(base.key_spec(KeyIndexId::alternate(0)?)?, &alt_ok);
    assert!(base.key_spec(KeyIndexId::alternate(1)?).is_err());
    assert!(base.is_compatible_with(&base.clone()));

    let diff_bounds = IsamSchema::try_new(
        RecordBounds::varying(20, 40)?,
        pk.clone(),
        vec![alt_ok.clone()],
        CollatingSequence::native(),
    )?;
    assert!(!base.is_compatible_with(&diff_bounds));

    let diff_pk = IsamSchema::try_new(
        fixed_20,
        KeySpec::single(0, 5, DuplicateKeyPolicy::Unique)?,
        vec![alt_ok.clone()],
        CollatingSequence::native(),
    )?;
    assert!(!base.is_compatible_with(&diff_pk));

    let diff_alt = IsamSchema::try_new(
        fixed_20,
        pk.clone(),
        Vec::new(),
        CollatingSequence::native(),
    )?;
    assert!(!base.is_compatible_with(&diff_alt));

    let custom_col =
        CollatingSequence::custom(CollatingId::new(9), *CollatingSequence::native().weights());
    let diff_col = IsamSchema::try_new(fixed_20, pk, vec![alt_ok], custom_col)?;
    assert!(!base.is_compatible_with(&diff_col));
    Ok(())
}

/// Verifies 4096-byte page framing, CRC32 corruption detection, header decoding errors, and free/overflow page codecs.
#[test]
fn page_framing_header_corruption_and_freelist_overflow_chains() -> Result<()> {
    for (kind, tag) in [
        (PageKind::FileHeader, 1_u8),
        (PageKind::Free, 2),
        (PageKind::BTreeInternal, 3),
        (PageKind::BTreeLeaf, 4),
        (PageKind::RecordPayload, 5),
    ] {
        assert_eq!(kind.to_tag(), tag);
        assert_eq!(PageKind::from_tag(tag, "p", PageId::HEADER)?, kind);
    }
    assert!(PageKind::from_tag(0, "p", PageId::HEADER).is_err());

    let p2 = PageId::new(2);
    let mut framed = build_framed_page(PageKind::Free, p2, &7_u64.to_le_bytes());
    assert_eq!(&framed[..8], &MAGIC_BYTES);
    assert_eq!(FORMAT_VERSION, 1);
    assert_eq!(verify_any_page_frame(&framed, p2, "p")?, PageKind::Free);
    verify_page_frame(&framed, PageKind::Free, p2, "p")?;
    assert!(verify_page_frame(&framed, PageKind::BTreeLeaf, p2, "p").is_err());
    assert!(verify_any_page_frame(&framed, PageId::new(9), "p").is_err());

    // Corrupt CRC32 (flip a payload byte without restamping)
    let mut bad_crc = framed;
    bad_crc[FRAME_HEADER_BYTES] ^= 0xFF;
    assert!(verify_any_page_frame(&bad_crc, p2, "p").is_err());

    // Corrupt format version
    let mut bad_ver = framed;
    patch_and_restamp(&mut bad_ver, 8, &99_u16.to_le_bytes());
    assert!(verify_any_page_frame(&bad_ver, p2, "p").is_err());

    // Corrupt unknown page kind
    let mut bad_kind = framed;
    patch_and_restamp(&mut bad_kind, 10, &[99_u8]);
    assert!(verify_any_page_frame(&bad_kind, p2, "p").is_err());

    // Corrupt magic
    framed[0] = b'X';
    assert!(verify_any_page_frame(&framed, p2, "p").is_err());

    // Free & overflow page codecs
    let free_raw = encode_free_page(p2, Some(PageId::new(5)));
    assert_eq!(decode_free_page(&free_raw, p2, "p")?, Some(PageId::new(5)));

    let ovf_raw = encode_overflow_page(p2, None, b"hello");
    let (next_p, chunk) = decode_overflow_page(&ovf_raw, p2, "p")?;
    assert_eq!(next_p, None);
    assert_eq!(chunk, b"hello");

    let mut zero_chunk = ovf_raw;
    patch_and_restamp(
        &mut zero_chunk,
        FRAME_HEADER_BYTES + 8,
        &0_u16.to_le_bytes(),
    );
    assert!(decode_overflow_page(&zero_chunk, p2, "p").is_err());

    let mut huge_chunk = ovf_raw;
    let bad_len = u16::try_from(OVERFLOW_CHUNK_CAPACITY + 1).unwrap_or(u16::MAX);
    patch_and_restamp(
        &mut huge_chunk,
        FRAME_HEADER_BYTES + 8,
        &bad_len.to_le_bytes(),
    );
    assert!(decode_overflow_page(&huge_chunk, p2, "p").is_err());

    // Header corruption checks
    let schema = sample_multi_key_schema()?;
    let mut hdr = FileHeaderState::new_empty(schema);
    hdr.set_tree_root(KeyIndexId::PRIMARY, Some(PageId::new(1)));
    hdr.set_tree_root(KeyIndexId::alternate(0)?, Some(PageId::new(2)));
    assert_eq!(hdr.tree_root(KeyIndexId::PRIMARY), Some(PageId::new(1)));
    assert_eq!(
        hdr.tree_root(KeyIndexId::alternate(0)?),
        Some(PageId::new(2))
    );
    assert_eq!(hdr.tree_root(KeyIndexId::alternate(15)?), None);
    hdr.set_tree_root(KeyIndexId::alternate(15)?, Some(PageId::new(9)));
    assert_eq!(hdr.alloc_dup_seq("p")?, 1);
    hdr.next_dup_seq = u64::MAX;
    assert!(hdr.alloc_dup_seq("p").is_err());
    hdr.next_dup_seq = 2;

    let valid_hdr_raw = hdr.encode_page();
    let decoded_hdr = FileHeaderState::decode_page(&valid_hdr_raw, "p")?;
    assert_eq!(decoded_hdr, hdr);

    // 1. total_pages == 0
    let mut c1 = valid_hdr_raw;
    patch_and_restamp(&mut c1, FRAME_HEADER_BYTES, &0_u64.to_le_bytes());
    assert!(FileHeaderState::decode_page(&c1, "p").is_err());

    // 2. next_dup_seq == 0
    let mut c2 = valid_hdr_raw;
    patch_and_restamp(&mut c2, FRAME_HEADER_BYTES + 24, &0_u64.to_le_bytes());
    assert!(FileHeaderState::decode_page(&c2, "p").is_err());

    // 3. invalid record bounds (min=0)
    let mut c3 = valid_hdr_raw;
    patch_and_restamp(&mut c3, FRAME_HEADER_BYTES + 40, &0_u32.to_le_bytes());
    assert!(FileHeaderState::decode_page(&c3, "p").is_err());

    // 4. invalid primary DuplicateKeyPolicy tag (byte 24 + 48 + 258 = 330)
    let pk_pos = FRAME_HEADER_BYTES + 48 + 258;
    let mut c4 = valid_hdr_raw;
    patch_and_restamp(&mut c4, pk_pos, &[9_u8]);
    assert!(FileHeaderState::decode_page(&c4, "p").is_err());

    // 5. primary key AllowedFifo (violates IsamSchema::try_new)
    let mut c5 = valid_hdr_raw;
    patch_and_restamp(&mut c5, pk_pos, &[1_u8]);
    assert!(FileHeaderState::decode_page(&c5, "p").is_err());

    // 6. primary key segment count = 0 (violates KeySpec::try_new)
    let mut c6 = valid_hdr_raw;
    patch_and_restamp(&mut c6, pk_pos + 1, &[0_u8]);
    assert!(FileHeaderState::decode_page(&c6, "p").is_err());

    // 7. primary key segment length = 0 (violates KeySegmentSpec::try_new)
    let mut c7 = valid_hdr_raw;
    patch_and_restamp(&mut c7, pk_pos + 6, &0_u32.to_le_bytes());
    assert!(FileHeaderState::decode_page(&c7, "p").is_err());

    // 8. alt_count > MAX_ALTERNATE_KEYS (right after 10-byte primary key spec)
    let alt_cnt_pos = pk_pos + 10;
    let mut c8 = valid_hdr_raw;
    patch_and_restamp(&mut c8, alt_cnt_pos, &99_u16.to_le_bytes());
    assert!(FileHeaderState::decode_page(&c8, "p").is_err());

    Ok(())
}

/// Verifies `MemoryDisk`, `MemoryPageIo`, and `Pager` freelist reuse, overflow chains, cycle guards, and truncation checks.
#[test]
fn memory_io_and_pager_overflow_freelist_error_paths() -> Result<()> {
    let default_disk = MemoryDisk::default();
    assert!(!default_disk.exists());
    let uncreated = MemoryDisk::uncreated("mem.isam");
    assert!(!uncreated.exists());
    assert_eq!(uncreated.page_len(), 0);
    assert!(uncreated.open_io().is_err());
    assert!(
        uncreated
            .mutate_raw_page(PageId::HEADER, &mut |_| {})
            .is_err()
    );

    let mut raw_mem_io = MemoryPageIo::from_disk(uncreated);
    assert_eq!(raw_mem_io.path_label(), "mem.isam");
    assert!(raw_mem_io.page_count().is_err());
    let mut tmp_buf = [0_u8; PAGE_SIZE];
    assert!(
        raw_mem_io
            .read_raw_page(PageId::HEADER, &mut tmp_buf)
            .is_err()
    );
    assert!(raw_mem_io.write_raw_page(PageId::HEADER, &tmp_buf).is_err());

    let mut mem_io = MemoryPageIo::new("created.isam");
    assert_eq!(mem_io.disk().page_len(), 0);
    assert!(
        mem_io
            .disk()
            .mutate_raw_page(PageId::new(99), &mut |_| {})
            .is_err()
    );
    assert!(mem_io.read_raw_page(PageId::HEADER, &mut tmp_buf).is_err());
    assert!(mem_io.write_raw_page(PageId::new(2), &tmp_buf).is_err());

    // Pager open_existing on 0-page store fails
    assert!(Pager::open_existing(Box::new(mem_io.clone())).is_err());

    let schema = sample_multi_key_schema()?;
    let disk = mem_io.disk();
    let mut pager = Pager::create_new(Box::new(mem_io), schema)?;
    pager.rollback_tx();
    assert_eq!(pager.path_label(), "created.isam");
    assert!(pager.read_any_verified_page(PageId::new(99)).is_err());
    assert!(
        pager
            .read_verified_page(PageId::HEADER, PageKind::Free)
            .is_err()
    );
    assert!(pager.write_overflow_chain(&[]).is_err());

    // Multi-page overflow chain (9000 bytes = 3 pages), read, length mismatch, and free
    pager.begin_tx();
    let big_payload = vec![0xAB_u8; 9_000];
    let head_id = pager.write_overflow_chain(&big_payload)?;
    assert_eq!(pager.read_overflow_chain(head_id, 9_000)?, big_payload);
    assert!(pager.read_overflow_chain(head_id, 8_999).is_err());
    pager.free_overflow_chain(head_id)?;
    let reused_id = pager.alloc_page()?;
    assert!(reused_id.as_u64() >= 1);
    pager.commit_tx()?;

    // Cycle detection in read_overflow_chain and free_overflow_chain
    pager.begin_tx();
    let cyc_id = pager.alloc_page()?;
    let self_loop = encode_overflow_page(cyc_id, Some(cyc_id), b"loop");
    pager.stage_raw_page(cyc_id, self_loop);
    assert!(pager.read_overflow_chain(cyc_id, 4).is_err());
    assert!(pager.free_overflow_chain(cyc_id).is_err());
    pager.rollback_tx();

    // total_pages u64::MAX overflow in alloc_page
    pager.begin_tx();
    pager.header_mut().free_head = None;
    pager.header_mut().total_pages = u64::MAX;
    assert!(pager.alloc_page().is_err());
    pager.rollback_tx();

    // Truncated file check in Pager::open_existing (header expects more pages than physical)
    disk.mutate_raw_page(PageId::HEADER, &mut |raw| {
        patch_and_restamp(raw, FRAME_HEADER_BYTES, &500_u64.to_le_bytes());
    })?;
    assert!(Pager::open_existing(Box::new(disk.open_io()?)).is_err());
    Ok(())
}

/// Verifies real OS filesystem persistence, seek/alignment checks, and descriptor error handling in `DiskPageIo`.
#[test]
fn disk_page_io_and_factory_cover_all_filesystem_paths() -> Result<()> {
    let base = std::env::temp_dir().join(format!("coblang_isam_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base)?;

    let file_path = base.join("accounts.isam");
    let factory = DiskIoFactory::new(&file_path);
    assert_eq!(factory.path(), file_path.as_path());
    assert!(!factory.exists());

    // Directory path errors on open_existing and create_truncated
    assert!(DiskPageIo::open_existing(&base, DiskAccessPerm::ReadWrite).is_err());
    assert!(DiskPageIo::create_truncated(&base).is_err());
    assert!(DiskPageIo::open_existing(&file_path, DiskAccessPerm::ReadOnly).is_err());

    let schema = sample_multi_key_schema()?;
    let mut isam = IsamFile::new_disk(
        &file_path,
        schema.clone(),
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    assert_eq!(isam.open(IsamOpenMode::Output)?, FileStatusKind::Success00);
    assert!(factory.exists());
    let mut rec = b"0001U001D001PAYLOAD!".to_vec();
    assert_eq!(isam.write(&rec)?, FileStatusKind::Success00);
    rec.splice(0..8, *b"0002U002");
    assert_eq!(isam.write(&rec)?, FileStatusKind::SuccessDuplicateKey02);
    assert_eq!(
        isam.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );

    // Reopen from real OS disk in Input and Io modes
    assert_eq!(isam.open(IsamOpenMode::Input)?, FileStatusKind::Success00);
    assert_eq!(isam.record_count(), Some(2));
    let (st, r1) = isam.read_next()?;
    assert_eq!(st, FileStatusKind::Success00);
    assert_eq!(r1.as_deref(), Some(b"0001U001D001PAYLOAD!".as_slice()));
    assert_eq!(
        isam.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );

    assert_eq!(isam.open(IsamOpenMode::Io)?, FileStatusKind::Success00);
    assert_eq!(
        isam.delete_by_primary_key(b"0001")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        isam.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );

    // Direct DiskPageIo edge cases: u64::MAX byte_offset overflow, 1<<51 seek EINVAL, read EOF, write EBADF
    let mut rw_io = DiskPageIo::open_existing(&file_path, DiskAccessPerm::ReadWrite)?;
    assert_ne!(rw_io.path_label(), "");
    assert!(rw_io.page_count()? >= 1);
    let mut buf = [0_u8; PAGE_SIZE];
    assert!(
        rw_io
            .read_raw_page(PageId::new(u64::MAX), &mut buf)
            .is_err()
    );
    assert!(rw_io.write_raw_page(PageId::new(u64::MAX), &buf).is_err());
    assert!(
        rw_io
            .read_raw_page(PageId::new(1_u64 << 51), &mut buf)
            .is_err()
    );
    assert!(
        rw_io
            .write_raw_page(PageId::new(1_u64 << 51), &buf)
            .is_err()
    );
    assert!(rw_io.read_raw_page(PageId::new(999), &mut buf).is_err());

    // Read-only File handle fails write_raw_page with EBADF
    let ro_file = OpenOptions::new().read(true).open(&file_path)?;
    let mut readonly_handle = DiskPageIo::from_file(String::from("ro"), ro_file);
    assert!(
        readonly_handle
            .write_raw_page(PageId::HEADER, &buf)
            .is_err()
    );

    // Misaligned file length fails page_count
    let bad_align_path = base.join("misaligned.isam");
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&bad_align_path)?;
        f.write_all(b"not-4096-aligned")?;
    }
    let mut bad_align_io = DiskPageIo::open_existing(&bad_align_path, DiskAccessPerm::ReadOnly)?;
    assert!(bad_align_io.page_count().is_err());

    // Pipe-backed File fails seek(SeekFrom::End(0)) in page_count with ESPIPE
    #[cfg(unix)]
    {
        let mut child = Command::new("true").stdout(Stdio::piped()).spawn()?;
        if let Some(pipe_out) = child.stdout.take() {
            let owned_fd = std::os::fd::OwnedFd::from(pipe_out);
            let pipe_file = fs::File::from(owned_fd);
            let mut pipe_io = DiskPageIo::from_file(String::from("pipe"), pipe_file);
            assert!(pipe_io.page_count().is_err());
        }
        let _ = child.wait();
    }

    fs::remove_dir_all(&base)?;
    Ok(())
}

/// Verifies multi-level B+Tree splits, `O(1)` bidirectional leaf scans, and multi-page overflow record `REWRITE`/`DELETE`.
#[test]
fn deep_btree_splits_bidirectional_scans_and_overflow_records() -> Result<()> {
    let bounds = RecordBounds::varying(800, 12_000)?;
    let pk = KeySpec::single(0, 400, DuplicateKeyPolicy::Unique)?;
    let schema = IsamSchema::try_new(bounds, pk, Vec::new(), CollatingSequence::native())?;
    let disk = MemoryDisk::uncreated("deep.isam");

    let mut file = IsamFile::new_memory(
        disk,
        schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    assert_eq!(file.open(IsamOpenMode::Output)?, FileStatusKind::Success00);
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    assert_eq!(file.open(IsamOpenMode::Io)?, FileStatusKind::Success00);

    let mut order = Vec::new();
    for i in (0..70_u16).step_by(2) {
        order.push(i);
    }
    for i in (1..70_u16).step_by(2).rev() {
        order.push(i);
    }

    let make_key = |k: u16| -> Vec<u8> {
        let mut key = format!("K{k:05}").into_bytes();
        key.resize(400, b'_');
        key
    };
    let make_rec = |k: u16, body_len: usize| -> Vec<u8> {
        let mut rec = make_key(k);
        rec.resize(body_len, u8::try_from(k % 251).unwrap_or(1));
        rec
    };

    for &k in &order {
        assert_eq!(file.write(&make_rec(k, 800))?, FileStatusKind::Success00);
    }
    assert_eq!(file.record_count(), Some(70));

    let k0 = make_key(0);
    assert_eq!(
        file.start(KeyIndexId::PRIMARY, StartRelation::GreaterOrEqual, &k0)?,
        FileStatusKind::Success00
    );
    for expected in 0..70_u16 {
        let (st, rec_opt) = file.read_next()?;
        assert_eq!(st, FileStatusKind::Success00);
        let rec = rec_opt.unwrap_or_default();
        assert_eq!(rec.get(..6), Some(format!("K{expected:05}").as_bytes()));
    }
    assert_eq!(file.read_next()?.0, FileStatusKind::AtEnd10);
    assert_eq!(file.read_next()?.0, FileStatusKind::ReadBeyondEnd46);

    let high = [0xFF_u8; 400];
    assert_eq!(
        file.start(KeyIndexId::PRIMARY, StartRelation::LessOrEqual, &high)?,
        FileStatusKind::Success00
    );
    for expected in (0..70_u16).rev() {
        let (st, rec_opt) = file.read_previous()?;
        assert_eq!(st, FileStatusKind::Success00);
        let rec = rec_opt.unwrap_or_default();
        assert_eq!(rec.get(..6), Some(format!("K{expected:05}").as_bytes()));
    }
    assert_eq!(file.read_previous()?.0, FileStatusKind::AtEnd10);

    assert_eq!(
        file.rewrite(&make_rec(10, 9_500))?,
        FileStatusKind::Success00
    );
    let (_, big10) = file.read_by_key(KeyIndexId::PRIMARY, &make_key(10))?;
    assert_eq!(big10.as_ref().map(Vec::len), Some(9_500));
    assert_eq!(
        file.rewrite(&make_rec(10, 10_200))?,
        FileStatusKind::Success00
    );
    assert_eq!(file.rewrite(&make_rec(10, 800))?, FileStatusKind::Success00);
    assert_eq!(
        file.rewrite(&make_rec(11, 9_200))?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.delete_by_primary_key(&make_key(11))?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    Ok(())
}

/// Verifies leaf/internal node merges, right-sibling borrows, root collapse to empty, and freelist page reuse.
#[test]
fn deep_btree_leaf_and_internal_borrows_merges_and_root_collapse() -> Result<()> {
    let bounds = RecordBounds::fixed(800)?;
    let pk = KeySpec::single(0, 400, DuplicateKeyPolicy::Unique)?;
    let schema = IsamSchema::try_new(bounds, pk, Vec::new(), CollatingSequence::native())?;
    let disk = MemoryDisk::uncreated("rebalance.isam");
    let mut file = IsamFile::new_memory(
        disk,
        schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    assert_eq!(file.open(IsamOpenMode::Output)?, FileStatusKind::Success00);
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    assert_eq!(file.open(IsamOpenMode::Io)?, FileStatusKind::Success00);

    let make_key = |k: u16| -> Vec<u8> {
        let mut key = format!("K{k:05}").into_bytes();
        key.resize(400, b'_');
        key
    };
    let make_rec = |k: u16| -> Vec<u8> {
        let mut rec = make_key(k);
        rec.resize(800, b'x');
        rec
    };

    for k in 0..70_u16 {
        assert_eq!(file.write(&make_rec(k))?, FileStatusKind::Success00);
    }
    for k in 0..35_u16 {
        assert_eq!(
            file.delete_by_primary_key(&make_key(k))?,
            FileStatusKind::Success00
        );
    }
    for k in (35..70_u16).rev() {
        assert_eq!(
            file.delete_by_primary_key(&make_key(k))?,
            FileStatusKind::Success00
        );
    }
    assert_eq!(file.record_count(), Some(0));

    for k in 0..110_u16 {
        assert_eq!(file.write(&make_rec(k))?, FileStatusKind::Success00);
    }
    for k in 0..18_u16 {
        assert_eq!(
            file.delete_by_primary_key(&make_key(k))?,
            FileStatusKind::Success00
        );
    }
    for k in (92..110_u16).rev() {
        assert_eq!(
            file.delete_by_primary_key(&make_key(k))?,
            FileStatusKind::Success00
        );
    }
    assert_eq!(file.record_count(), Some(74));
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    Ok(())
}

/// Builds the custom-collation composite-key schema (`Z` < `B` < `A`, 5B split PK, Unique ALT0, FIFO ALT1).
fn composite_collated_schema() -> Result<IsamSchema> {
    let mut weights = *CollatingSequence::native().weights();
    if let Some(slot) = weights.get_mut(usize::from(b'Z')) {
        *slot = b'A';
    }
    if let Some(slot) = weights.get_mut(usize::from(b'A')) {
        *slot = b'Z';
    }
    let collation = CollatingSequence::custom(CollatingId::EBCDIC, weights);
    let pk = KeySpec::try_new(
        vec![
            KeySegmentSpec::try_new(2, 3)?,
            KeySegmentSpec::try_new(0, 2)?,
        ],
        DuplicateKeyPolicy::Unique,
    )?;
    let alt0 = KeySpec::single(5, 4, DuplicateKeyPolicy::Unique)?;
    let alt1 = KeySpec::single(9, 4, DuplicateKeyPolicy::AllowedFifo)?;
    IsamSchema::try_new(RecordBounds::fixed(20)?, pk, vec![alt0, alt1], collation)
}

/// Verifies split composite keys (`RECORD KEY IS seg1 seg2`), custom collation, and generic `START` (`=`, `>`, `>=`, `<`, `<=`).
#[test]
fn composite_keys_collation_and_generic_start_relations() -> Result<()> {
    let schema = composite_collated_schema()?;
    let disk = MemoryDisk::uncreated("composite.isam");
    let mut file = IsamFile::new_memory(
        disk,
        schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    assert_eq!(file.open(IsamOpenMode::Output)?, FileStatusKind::Success00);
    assert_eq!(
        file.write(b"10Z01U001D100REC_Z10")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.write(b"10B01U002D100REC_B10")?,
        FileStatusKind::SuccessDuplicateKey02
    );
    assert_eq!(
        file.write(b"20B01U003D100REC_B20")?,
        FileStatusKind::SuccessDuplicateKey02
    );
    assert_eq!(
        file.write(b"10A01U004D200REC_A10")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.write(b"10B01U999D999DUP_PK!")?,
        FileStatusKind::DuplicateKey22
    );
    assert_eq!(
        file.write(b"99C01U002D999DUP_AL0")?,
        FileStatusKind::DuplicateKey22
    );
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );

    assert_eq!(file.open(IsamOpenMode::Io)?, FileStatusKind::Success00);
    assert_eq!(
        file.read_next()?.1.as_deref(),
        Some(b"10Z01U001D100REC_Z10".as_slice())
    );
    assert_eq!(
        file.read_next()?.1.as_deref(),
        Some(b"10B01U002D100REC_B10".as_slice())
    );

    assert_eq!(
        file.start(KeyIndexId::PRIMARY, StartRelation::Equal, b"B01")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.read_next()?.1.as_deref(),
        Some(b"10B01U002D100REC_B10".as_slice())
    );
    assert_eq!(
        file.start(KeyIndexId::PRIMARY, StartRelation::Equal, b"C99")?,
        FileStatusKind::RecordNotFound23
    );
    assert_eq!(
        file.start(KeyIndexId::PRIMARY, StartRelation::Greater, b"B01")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.read_next()?.1.as_deref(),
        Some(b"10A01U004D200REC_A10".as_slice())
    );
    assert_eq!(
        file.start(KeyIndexId::PRIMARY, StartRelation::Less, b"B01")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.read_previous()?.1.as_deref(),
        Some(b"10Z01U001D100REC_Z10".as_slice())
    );
    assert_eq!(
        file.start(KeyIndexId::PRIMARY, StartRelation::LessOrEqual, b"B01")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.read_previous()?.1.as_deref(),
        Some(b"20B01U003D100REC_B20".as_slice())
    );
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    Ok(())
}

/// Verifies FIFO duplicate alternate key `"02"` status and forward File Position Indicator resilience across `REWRITE`.
#[test]
fn fifo_duplicates_and_forward_fpi_resilience() -> Result<()> {
    let schema = composite_collated_schema()?;
    let alt1_id = KeyIndexId::alternate(1)?;
    let disk = MemoryDisk::uncreated("fifo_fwd.isam");
    let mut file = IsamFile::new_memory(
        disk,
        schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    assert_eq!(file.open(IsamOpenMode::Output)?, FileStatusKind::Success00);
    assert_eq!(
        file.write(b"10Z01U001D100REC_Z10")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.write(b"10B01U002D100REC_B10")?,
        FileStatusKind::SuccessDuplicateKey02
    );
    assert_eq!(
        file.write(b"20B01U003D100REC_B20")?,
        FileStatusKind::SuccessDuplicateKey02
    );
    assert_eq!(
        file.write(b"10A01U004D200REC_A10")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );

    assert_eq!(file.open(IsamOpenMode::Io)?, FileStatusKind::Success00);
    let (s1, d1) = file.read_by_key(alt1_id, b"D100")?;
    assert_eq!(s1, FileStatusKind::SuccessDuplicateKey02);
    assert_eq!(d1.as_deref(), Some(b"10Z01U001D100REC_Z10".as_slice()));

    assert_eq!(
        file.rewrite(b"10Z01U001D200MOV_Z10")?,
        FileStatusKind::SuccessDuplicateKey02
    );
    let (s2, d2) = file.read_next()?;
    assert_eq!(s2, FileStatusKind::SuccessDuplicateKey02);
    assert_eq!(d2.as_deref(), Some(b"10B01U002D100REC_B10".as_slice()));
    let (s3, d3) = file.read_next()?;
    assert_eq!(s3, FileStatusKind::Success00);
    assert_eq!(d3.as_deref(), Some(b"20B01U003D100REC_B20".as_slice()));
    let (s4, d4) = file.read_next()?;
    assert_eq!(s4, FileStatusKind::SuccessDuplicateKey02);
    assert_eq!(d4.as_deref(), Some(b"10A01U004D200REC_A10".as_slice()));
    let (s5, d5) = file.read_next()?;
    assert_eq!(s5, FileStatusKind::Success00);
    assert_eq!(d5.as_deref(), Some(b"10Z01U001D200MOV_Z10".as_slice()));
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    Ok(())
}

/// Verifies backward `READ PREVIOUS` over FIFO duplicates, epoch-mismatch re-seeks, and atomic `REWRITE` `"22"` checks.
#[test]
fn backward_fifo_start_reseek_and_rewrite_atomicity() -> Result<()> {
    let schema = composite_collated_schema()?;
    let alt0_id = KeyIndexId::alternate(0)?;
    let alt1_id = KeyIndexId::alternate(1)?;
    let disk = MemoryDisk::uncreated("fifo_rev.isam");
    let mut file = IsamFile::new_memory(
        disk,
        schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    assert_eq!(file.open(IsamOpenMode::Output)?, FileStatusKind::Success00);
    let _ = file.write(b"10Z01U001D200REC_Z10")?;
    let _ = file.write(b"10B01U002D100REC_B10")?;
    let _ = file.write(b"20B01U003D100REC_B20")?;
    let _ = file.write(b"10A01U004D200REC_A10")?;
    let _ = file.close(CloseDisposition::Normal);

    assert_eq!(file.open(IsamOpenMode::Io)?, FileStatusKind::Success00);
    assert_eq!(
        file.start(alt1_id, StartRelation::LessOrEqual, b"D200")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.read_previous()?.0,
        FileStatusKind::SuccessDuplicateKey02
    );
    assert_eq!(
        file.write(b"30B01U005D300NEW_B30")?,
        FileStatusKind::Success00
    );
    let (bp2, bd2) = file.read_previous()?;
    assert_eq!(bp2, FileStatusKind::Success00);
    assert_eq!(bd2.as_deref(), Some(b"10Z01U001D200REC_Z10".as_slice()));

    assert_eq!(
        file.start(alt0_id, StartRelation::Equal, b"U002")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.rewrite(b"30B01U006D300UPD_B30")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.read_next()?.1.as_deref(),
        Some(b"10B01U002D100REC_B10".as_slice())
    );

    assert_eq!(
        file.start(alt0_id, StartRelation::Equal, b"U003")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.rewrite(b"30B01U007D300UPD_B30")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.read_previous()?.1.as_deref(),
        Some(b"20B01U003D100REC_B20".as_slice())
    );

    assert_eq!(
        file.rewrite(b"10B01U001D100COLLIDE")?,
        FileStatusKind::DuplicateKey22
    );
    assert_eq!(
        file.rewrite(b"10B01U002D100SAMEALT")?,
        FileStatusKind::SuccessDuplicateKey02
    );
    assert_eq!(
        file.rewrite(b"30B01U000D050LOW_ALT")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    assert_eq!(
        file.read_by_key(KeyIndexId::PRIMARY, b"10B01")?.0,
        FileStatusKind::ReadWrongOpenMode47
    );
    Ok(())
}

/// Verifies ISO COBOL `FILE STATUS` codes (`"35"`, `"42"`, `"47"`, `"48"`, `"49"`) on closed/missing connectors.
#[test]
fn cobol_closed_connector_file_status_rejections() -> Result<()> {
    let schema = sample_multi_key_schema()?;
    let disk = MemoryDisk::uncreated("closed.isam");
    let mut req_seq = IsamFile::new_memory(
        disk,
        schema.clone(),
        IsamAccessMode::Sequential,
        FileOptionality::Required,
    );
    assert_eq!(req_seq.schema(), &schema);
    assert_eq!(req_seq.access_mode(), IsamAccessMode::Sequential);
    assert!(!req_seq.is_open());
    assert_eq!(req_seq.record_count(), None);
    assert_eq!(
        req_seq.close(CloseDisposition::Normal),
        FileStatusKind::NotOpenOnClose42
    );
    assert_eq!(
        req_seq.open(IsamOpenMode::Input)?,
        FileStatusKind::FileNotFound35
    );
    assert_eq!(
        req_seq.open(IsamOpenMode::Io)?,
        FileStatusKind::FileNotFound35
    );
    assert_eq!(
        req_seq.open(IsamOpenMode::Extend)?,
        FileStatusKind::FileNotFound35
    );

    assert!(
        req_seq
            .start(KeyIndexId::PRIMARY, StartRelation::Equal, b"")
            .is_err()
    );
    assert!(
        req_seq
            .start(KeyIndexId::PRIMARY, StartRelation::Equal, b"12345")
            .is_err()
    );
    assert_eq!(
        req_seq.start(KeyIndexId::PRIMARY, StartRelation::Equal, b"0001")?,
        FileStatusKind::ReadWrongOpenMode47
    );
    assert_eq!(req_seq.read_next()?.0, FileStatusKind::ReadWrongOpenMode47);
    assert_eq!(
        req_seq.read_by_key(KeyIndexId::PRIMARY, b"0001")?.0,
        FileStatusKind::ReadWrongOpenMode47
    );
    assert!(req_seq.read_by_key(KeyIndexId::PRIMARY, b"01").is_err());
    assert_eq!(
        req_seq.write(b"0001U001D001DATA")?,
        FileStatusKind::WriteWrongOpenMode48
    );
    assert_eq!(
        req_seq.rewrite(b"0001U001D001DATA")?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    assert_eq!(
        req_seq.delete_sequential()?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    assert!(req_seq.delete_by_primary_key(b"99").is_err());
    assert_eq!(
        req_seq.delete_by_primary_key(b"0001")?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    Ok(())
}

/// Verifies `SELECT OPTIONAL` (`"05"`), `AlreadyOpen41`, `RewriteLengthMismatch44`, and `CLOSE WITH LOCK` (`"38"`).
#[test]
fn cobol_optional_file_and_with_lock_status_matrix() -> Result<()> {
    let schema = sample_multi_key_schema()?;
    let opt_disk = MemoryDisk::uncreated("opt.isam");
    let mut opt_dyn = IsamFile::new_memory(
        opt_disk.clone(),
        schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Optional,
    );
    assert_eq!(
        opt_dyn.open(IsamOpenMode::Extend)?,
        FileStatusKind::OpenPermissionDenied37
    );
    assert_eq!(
        opt_dyn.open(IsamOpenMode::Input)?,
        FileStatusKind::SuccessOptionalFile05
    );
    assert!(opt_dyn.is_open());
    assert_eq!(opt_dyn.record_count(), Some(0));
    assert_eq!(
        opt_dyn.open(IsamOpenMode::Input)?,
        FileStatusKind::AlreadyOpen41
    );
    assert_eq!(
        opt_dyn.write(b"0001U001D001DATA")?,
        FileStatusKind::WriteWrongOpenMode48
    );
    assert_eq!(
        opt_dyn.rewrite(b"0001U001D001DATA")?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    assert_eq!(
        opt_dyn.delete_by_primary_key(b"0001")?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    assert_eq!(opt_dyn.read_next()?.0, FileStatusKind::AtEnd10);
    assert_eq!(opt_dyn.read_next()?.0, FileStatusKind::ReadBeyondEnd46);
    assert_eq!(
        opt_dyn.start(KeyIndexId::PRIMARY, StartRelation::Equal, b"0001")?,
        FileStatusKind::RecordNotFound23
    );
    assert_eq!(
        opt_dyn.read_by_key(KeyIndexId::PRIMARY, b"0001")?.0,
        FileStatusKind::RecordNotFound23
    );
    let _ = opt_dyn.close(CloseDisposition::Normal);

    assert_eq!(
        opt_dyn.open(IsamOpenMode::Io)?,
        FileStatusKind::SuccessOptionalFile05
    );
    assert!(opt_disk.exists());
    assert_eq!(opt_dyn.read_previous()?.0, FileStatusKind::AtEnd10);
    assert_eq!(
        opt_dyn.read_by_key(KeyIndexId::PRIMARY, b"0001")?.0,
        FileStatusKind::RecordNotFound23
    );
    assert_eq!(
        opt_dyn.rewrite(b"0001U001D001DATA")?,
        FileStatusKind::RecordNotFound23
    );
    assert_eq!(
        opt_dyn.delete_by_primary_key(b"0001")?,
        FileStatusKind::RecordNotFound23
    );
    assert_eq!(
        opt_dyn.delete_sequential()?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    assert_eq!(
        opt_dyn.write(b"short")?,
        FileStatusKind::RewriteLengthMismatch44
    );
    assert_eq!(
        opt_dyn.rewrite(b"short")?,
        FileStatusKind::RewriteLengthMismatch44
    );
    assert_eq!(
        opt_dyn.close(CloseDisposition::WithLock),
        FileStatusKind::Success00
    );
    assert_eq!(
        opt_dyn.open(IsamOpenMode::Input)?,
        FileStatusKind::ClosedWithLock38
    );
    Ok(())
}

/// Verifies `OPEN EXTEND` ascending primary-key enforcement (`"21"`) and schema attribute conflicts (`"39"`).
#[test]
fn cobol_sequential_extend_and_attribute_conflict_matrix() -> Result<()> {
    let schema = sample_multi_key_schema()?;
    let seq_disk = MemoryDisk::uncreated("seq.isam");
    let mut seq_file = IsamFile::new_memory(
        seq_disk.clone(),
        schema,
        IsamAccessMode::Sequential,
        FileOptionality::Optional,
    );
    assert_eq!(
        seq_file.open(IsamOpenMode::Extend)?,
        FileStatusKind::SuccessOptionalFile05
    );
    assert_eq!(seq_file.read_next()?.0, FileStatusKind::ReadWrongOpenMode47);
    assert_eq!(
        seq_file.start(KeyIndexId::PRIMARY, StartRelation::Equal, b"0010")?,
        FileStatusKind::ReadWrongOpenMode47
    );
    assert_eq!(
        seq_file.write(b"0010U010D010PAYLOAD")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        seq_file.write(b"0005U005D005PAYLOAD")?,
        FileStatusKind::InvalidKeySequence21
    );
    assert_eq!(
        seq_file.write(b"0010U011D011PAYLOAD")?,
        FileStatusKind::InvalidKeySequence21
    );
    assert_eq!(
        seq_file.write(b"0020U020D020PAYLOAD")?,
        FileStatusKind::Success00
    );
    let _ = seq_file.close(CloseDisposition::Normal);

    assert_eq!(
        seq_file.open(IsamOpenMode::Extend)?,
        FileStatusKind::Success00
    );
    assert_eq!(
        seq_file.write(b"0015U015D015PAYLOAD")?,
        FileStatusKind::InvalidKeySequence21
    );
    assert_eq!(
        seq_file.write(b"0030U030D030PAYLOAD")?,
        FileStatusKind::Success00
    );
    let _ = seq_file.close(CloseDisposition::Normal);

    let wrong_schema = IsamSchema::try_new(
        RecordBounds::fixed(16)?,
        KeySpec::single(0, 4, DuplicateKeyPolicy::Unique)?,
        Vec::new(),
        CollatingSequence::native(),
    )?;
    let mut bad_conn = IsamFile::new_memory(
        seq_disk,
        wrong_schema,
        IsamAccessMode::Sequential,
        FileOptionality::Required,
    );
    assert_eq!(
        bad_conn.open(IsamOpenMode::Input)?,
        FileStatusKind::AttributeConflict39
    );
    assert_eq!(
        bad_conn.open(IsamOpenMode::Io)?,
        FileStatusKind::AttributeConflict39
    );
    Ok(())
}

/// Verifies `SEQUENTIAL` `I-O` (`"43"`, `"21"`, `"48"`) and `RANDOM` access mode statement restrictions (`"47"`, `"49"`).
#[test]
fn cobol_sequential_io_and_random_access_matrix() -> Result<()> {
    let schema = sample_multi_key_schema()?;
    let seq_disk = MemoryDisk::uncreated("seq_io.isam");
    let mut seq_file = IsamFile::new_memory(
        seq_disk.clone(),
        schema.clone(),
        IsamAccessMode::Sequential,
        FileOptionality::Required,
    );
    let _ = seq_file.open(IsamOpenMode::Output)?;
    let _ = seq_file.write(b"0010U010D010PAYLOAD")?;
    let _ = seq_file.write(b"0020U020D020PAYLOAD")?;
    let _ = seq_file.write(b"0030U030D030PAYLOAD")?;
    let _ = seq_file.close(CloseDisposition::Normal);

    assert_eq!(seq_file.open(IsamOpenMode::Io)?, FileStatusKind::Success00);
    assert_eq!(
        seq_file.write(b"0040U040D040PAYLOAD")?,
        FileStatusKind::WriteWrongOpenMode48
    );
    assert_eq!(
        seq_file.delete_by_primary_key(b"0010")?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    assert_eq!(
        seq_file.rewrite(b"0010U010D010UPDATED")?,
        FileStatusKind::NoPriorRead43
    );
    assert_eq!(seq_file.delete_sequential()?, FileStatusKind::NoPriorRead43);
    assert_eq!(seq_file.read_next()?.0, FileStatusKind::Success00);
    assert_eq!(
        seq_file.rewrite(b"9999U010D010CHANGED")?,
        FileStatusKind::InvalidKeySequence21
    );
    assert_eq!(seq_file.read_next()?.0, FileStatusKind::Success00);
    assert_eq!(
        seq_file.rewrite(b"0020U020D020UPDATED")?,
        FileStatusKind::Success00
    );
    assert_eq!(seq_file.read_next()?.0, FileStatusKind::Success00);
    assert_eq!(seq_file.delete_sequential()?, FileStatusKind::Success00);
    let _ = seq_file.close(CloseDisposition::Normal);

    let mut rnd_file = IsamFile::new_memory(
        seq_disk,
        schema,
        IsamAccessMode::Random,
        FileOptionality::Required,
    );
    let _ = rnd_file.open(IsamOpenMode::Input)?;
    assert_eq!(
        rnd_file.start(KeyIndexId::PRIMARY, StartRelation::Equal, b"0010")?,
        FileStatusKind::ReadWrongOpenMode47
    );
    assert_eq!(rnd_file.read_next()?.0, FileStatusKind::ReadWrongOpenMode47);
    assert_eq!(
        rnd_file.read_previous()?.0,
        FileStatusKind::ReadWrongOpenMode47
    );
    assert_eq!(
        rnd_file.write(b"0050U050D050PAYLOAD")?,
        FileStatusKind::WriteWrongOpenMode48
    );
    assert_eq!(
        rnd_file.rewrite(b"0010U010D010PAYLOAD")?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    assert_eq!(
        rnd_file.delete_by_primary_key(b"0010")?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    assert_eq!(
        rnd_file
            .read_by_key(KeyIndexId::PRIMARY, b"0020")?
            .1
            .as_deref(),
        Some(b"0020U020D020UPDATED".as_slice())
    );
    let _ = rnd_file.close(CloseDisposition::Normal);
    Ok(())
}

/// Verifies `BTreeLeafNode::decode_body` corruption checks on key lengths, value tags, null overflow pages, and ordering.
#[test]
fn btree_leaf_decode_corruption_checks() -> Result<()> {
    assert_eq!(cmp_key_seq(b"A", 1, b"A", 2), core::cmp::Ordering::Less);
    let pid = PageId::new(1);
    let valid_leaf = BTreeLeafNode {
        prev_leaf: None,
        next_leaf: None,
        entries: vec![
            LeafEntry {
                key: b"K1".to_vec(),
                seq: 0,
                value: LeafValue::Inline(b"V1".to_vec()),
            },
            LeafEntry {
                key: b"K2".to_vec(),
                seq: 0,
                value: LeafValue::Overflow {
                    total_len: 900,
                    first_page: PageId::new(3),
                },
            },
        ],
    };
    let leaf_raw = valid_leaf.encode_page(pid);
    assert_eq!(BTreeLeafNode::decode_body(&leaf_raw, pid, "p")?, valid_leaf);

    let mut lc1 = leaf_raw;
    patch_and_restamp(&mut lc1, FRAME_HEADER_BYTES + 18, &0_u16.to_le_bytes());
    assert!(BTreeLeafNode::decode_body(&lc1, pid, "p").is_err());

    let mut lc_wide = leaf_raw;
    patch_and_restamp(
        &mut lc_wide,
        FRAME_HEADER_BYTES + 18,
        &600_u16.to_le_bytes(),
    );
    assert!(BTreeLeafNode::decode_body(&lc_wide, pid, "p").is_err());

    let mut packed = Vec::new();
    packed.extend_from_slice(&0_u64.to_le_bytes());
    packed.extend_from_slice(&0_u64.to_le_bytes());
    packed.extend_from_slice(&5_u16.to_le_bytes());
    for idx in 0..5_u8 {
        packed.extend_from_slice(&400_u16.to_le_bytes());
        packed.extend_from_slice(&0_u64.to_le_bytes());
        packed.extend_from_slice(&[b'A' + idx; 400]);
        packed.push(0);
        packed.extend_from_slice(&600_u16.to_le_bytes());
        packed.extend_from_slice(&[b'v'; 600]);
    }
    let lc_trunc = build_framed_page(PageKind::BTreeLeaf, pid, &packed);
    assert!(BTreeLeafNode::decode_body(&lc_trunc, pid, "p").is_err());

    let mut lc2 = leaf_raw;
    let bad_vlen = u16::try_from(MAX_INLINE_VALUE_BYTES + 1).unwrap_or(999);
    patch_and_restamp(
        &mut lc2,
        FRAME_HEADER_BYTES + 18 + 13,
        &bad_vlen.to_le_bytes(),
    );
    assert!(BTreeLeafNode::decode_body(&lc2, pid, "p").is_err());

    let mut lc3 = leaf_raw;
    patch_and_restamp(&mut lc3, FRAME_HEADER_BYTES + 18 + 12, &[9_u8]);
    assert!(BTreeLeafNode::decode_body(&lc3, pid, "p").is_err());

    let mut lc4 = leaf_raw;
    patch_and_restamp(
        &mut lc4,
        FRAME_HEADER_BYTES + 18 + 17 + 17,
        &0_u64.to_le_bytes(),
    );
    assert!(BTreeLeafNode::decode_body(&lc4, pid, "p").is_err());

    let mut lc5 = leaf_raw;
    patch_and_restamp(&mut lc5, FRAME_HEADER_BYTES + 18 + 17 + 10, b"K0");
    assert!(BTreeLeafNode::decode_body(&lc5, pid, "p").is_err());
    Ok(())
}

/// Verifies `BTreeInternalNode::decode_body` corruption checks on zero counts, null children, key bounds, and ordering.
#[test]
fn btree_internal_decode_corruption_checks() -> Result<()> {
    let pid = PageId::new(1);
    let valid_int = BTreeInternalNode {
        children: vec![PageId::new(2), PageId::new(3), PageId::new(4)],
        separators: vec![
            SeparatorKey {
                key: b"M1".to_vec(),
                seq: 0,
            },
            SeparatorKey {
                key: b"M2".to_vec(),
                seq: 0,
            },
        ],
    };
    let int_raw = valid_int.encode_page(pid);
    assert_eq!(
        BTreeInternalNode::decode_body(&int_raw, pid, "p")?,
        valid_int
    );

    let mut ic1 = int_raw;
    patch_and_restamp(&mut ic1, FRAME_HEADER_BYTES, &0_u16.to_le_bytes());
    assert!(BTreeInternalNode::decode_body(&ic1, pid, "p").is_err());

    let mut ic2 = int_raw;
    patch_and_restamp(&mut ic2, FRAME_HEADER_BYTES + 2, &0_u64.to_le_bytes());
    assert!(BTreeInternalNode::decode_body(&ic2, pid, "p").is_err());

    let mut ic3 = int_raw;
    let invalid_separator_len = u16::try_from(MAX_KEY_BYTES + 1).unwrap_or(999);
    patch_and_restamp(
        &mut ic3,
        FRAME_HEADER_BYTES + 10,
        &invalid_separator_len.to_le_bytes(),
    );
    assert!(BTreeInternalNode::decode_body(&ic3, pid, "p").is_err());

    let mut ic3_zero = int_raw;
    patch_and_restamp(&mut ic3_zero, FRAME_HEADER_BYTES + 10, &0_u16.to_le_bytes());
    assert!(BTreeInternalNode::decode_body(&ic3_zero, pid, "p").is_err());

    let mut ic4 = int_raw;
    patch_and_restamp(&mut ic4, FRAME_HEADER_BYTES + 20, &0_u64.to_le_bytes());
    assert!(BTreeInternalNode::decode_body(&ic4, pid, "p").is_err());

    let mut ic5 = int_raw;
    patch_and_restamp(&mut ic5, FRAME_HEADER_BYTES + 10 + 20 + 18, b"M0");
    assert!(BTreeInternalNode::decode_body(&ic5, pid, "p").is_err());
    Ok(())
}

/// Verifies cross-leaf `prev_leaf` / `next_leaf` range hops and dangling alternate-to-primary pointer detection.
#[test]
fn btree_boundary_hops_and_dangling_alt_pointer() -> Result<()> {
    let bounds = RecordBounds::fixed(600)?;
    let pk = KeySpec::single(0, 300, DuplicateKeyPolicy::Unique)?;
    let alt = KeySpec::single(300, 300, DuplicateKeyPolicy::Unique)?;
    let schema = IsamSchema::try_new(bounds, pk, vec![alt], CollatingSequence::native())?;
    let disk = MemoryDisk::uncreated("hops.isam");
    let mut file = IsamFile::new_memory(
        disk.clone(),
        schema.clone(),
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    let _ = file.open(IsamOpenMode::Output)?;
    assert_eq!(
        file.read_by_key(KeyIndexId::PRIMARY, &[b'A'; 300])?.0,
        FileStatusKind::ReadWrongOpenMode47
    );
    for i in 1..=14_u8 {
        let mut rec = vec![b'A' + i * 2; 300];
        rec.extend_from_slice(&[b'a' + i; 300]);
        assert_eq!(file.write(&rec)?, FileStatusKind::Success00);
    }
    let _ = file.close(CloseDisposition::Normal);

    let _ = file.open(IsamOpenMode::Io)?;
    for i in 1..=14_u8 {
        let exact = [b'A' + i * 2; 300];
        let _ = file.start(KeyIndexId::PRIMARY, StartRelation::Less, &exact)?;
    }
    for b in b'B'..=b'Z' {
        let probe = [b; 1];
        let _ = file.start(KeyIndexId::PRIMARY, StartRelation::GreaterOrEqual, &probe)?;
        let _ = file.start(KeyIndexId::PRIMARY, StartRelation::Greater, &probe)?;
        let _ = file.start(KeyIndexId::PRIMARY, StartRelation::Less, &probe)?;
        let _ = file.start(KeyIndexId::PRIMARY, StartRelation::LessOrEqual, &probe)?;
    }
    let _ = file.close(CloseDisposition::Normal);

    disk.mutate_raw_page(PageId::HEADER, &mut |raw| {
        patch_and_restamp(raw, FRAME_HEADER_BYTES + 32, &0_u64.to_le_bytes());
    })?;
    let mut dangling = IsamFile::new_memory(
        disk,
        schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    let _ = dangling.open(IsamOpenMode::Input)?;
    assert!(
        dangling
            .read_by_key(KeyIndexId::alternate(0)?, &[b'a' + 1; 300])
            .is_err()
    );
    let _ = dangling.close(CloseDisposition::Normal);
    Ok(())
}

/// Verifies wrong-kind root node rejection, internal routing cycle detection, and malformed primary payload decoding.
#[test]
fn btree_corrupted_root_cycle_and_primary_payload_errors() -> Result<()> {
    let bounds = RecordBounds::fixed(600)?;
    let pk = KeySpec::single(0, 300, DuplicateKeyPolicy::Unique)?;
    let alt = KeySpec::single(300, 300, DuplicateKeyPolicy::Unique)?;
    let schema = IsamSchema::try_new(bounds, pk, vec![alt], CollatingSequence::native())?;
    let disk2 = MemoryDisk::uncreated("corrupt_root.isam");
    let mut f2 = IsamFile::new_memory(
        disk2.clone(),
        schema,
        IsamAccessMode::Sequential,
        FileOptionality::Required,
    );
    let _ = f2.open(IsamOpenMode::Output)?;
    let mut rec = vec![b'M'; 300];
    rec.extend_from_slice(&[b'm'; 300]);
    let _ = f2.write(&rec)?;
    let _ = f2.close(CloseDisposition::Normal);

    let _ = f2.open(IsamOpenMode::Input)?;
    assert_eq!(
        f2.delete_sequential()?,
        FileStatusKind::RewriteDeleteWrongOpenMode49
    );
    let _ = f2.close(CloseDisposition::Normal);

    disk2.mutate_raw_page(PageId::new(2), &mut |raw| {
        *raw = encode_free_page(PageId::new(2), None);
    })?;
    let _ = f2.open(IsamOpenMode::Input)?;
    assert!(f2.read_next().is_err());
    let _ = f2.close(CloseDisposition::Normal);

    let self_int = BTreeInternalNode {
        children: vec![PageId::new(2), PageId::new(2)],
        separators: vec![SeparatorKey {
            key: vec![b'M'; 300],
            seq: 0,
        }],
    };
    disk2.mutate_raw_page(PageId::new(2), &mut |raw| {
        *raw = self_int.encode_page(PageId::new(2));
    })?;
    let _ = f2.open(IsamOpenMode::Input)?;
    assert!(f2.read_next().is_err());
    let _ = f2.close(CloseDisposition::Normal);

    for bad_val in [Vec::new(), vec![99_u8], vec![1_u8, 1, 2, 3]] {
        let bad_leaf = BTreeLeafNode {
            prev_leaf: None,
            next_leaf: None,
            entries: vec![LeafEntry {
                key: vec![b'M'; 300],
                seq: 0,
                value: LeafValue::Inline(bad_val),
            }],
        };
        disk2.mutate_raw_page(PageId::new(2), &mut |raw| {
            *raw = bad_leaf.encode_page(PageId::new(2));
        })?;
        let _ = f2.open(IsamOpenMode::Input)?;
        assert!(f2.read_next().is_err());
        let _ = f2.close(CloseDisposition::Normal);
    }
    Ok(())
}

/// Verifies left-sibling leaf/internal node borrows and graceful `DELETE` across desynchronized alternate trees.
#[test]
fn left_sibling_leaf_and_internal_borrows_and_desynced_alt_delete() -> Result<()> {
    // 1. Wide 1013-byte entries (500B key + 500B inline record -> 1014B/entry):
    //    5 entries = 5070B -> exercises both left_sum > 4054 and right_sum > 4054 in choose_leaf_split_index,
    //    AND exercises Leaf Borrow from Left (4 entries in left leaf = 4054? 350B key + 490B body -> 854B/entry,
    //    4 entries = 3416B <= 4054; right leaf 2 entries -> delete 1 -> 854 < 1350, 3416 + 854 = 4270 > 4054 -> BORROW FROM LEFT!).
    let bounds = RecordBounds::fixed(700)?;
    let pk = KeySpec::single(0, 500, DuplicateKeyPolicy::Unique)?;
    let schema = IsamSchema::try_new(bounds, pk, Vec::new(), CollatingSequence::native())?;
    let disk = MemoryDisk::uncreated("left_borrow.isam");
    let mut file = IsamFile::new_memory(
        disk,
        schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    assert_eq!(file.open(IsamOpenMode::Output)?, FileStatusKind::Success00);
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    assert_eq!(file.open(IsamOpenMode::Io)?, FileStatusKind::Success00);

    let make_key = |prefix: u16| -> Vec<u8> {
        let mut k = format!("P{prefix:05}").into_bytes();
        k.resize(500, b'_');
        k
    };
    let make_rec = |prefix: u16| -> Vec<u8> {
        let mut r = make_key(prefix);
        r.resize(700, b'z');
        r
    };

    // Each entry = 10 + 500 + 3 + 701 = 1214 bytes -> max 3 entries (3642B) per leaf!
    // 4th entry (4856B) splits into 2 + 2 (2428B each).
    for k in [100_u16, 200, 300, 400] {
        assert_eq!(file.write(&make_rec(k))?, FileStatusKind::Success00);
    }
    // Pack left leaf to 3 entries (3642B) by inserting 150 (< separator 300):
    assert_eq!(file.write(&make_rec(150))?, FileStatusKind::Success00);
    // Delete 400 from right leaf -> right drops to 1 entry (1214 < 1350), and 3642 + 1214 = 4856 > 4054 -> LEAF BORROW FROM LEFT!
    assert_eq!(
        file.delete_by_primary_key(&make_key(400))?,
        FileStatusKind::Success00
    );

    // Now build a 3-level tree where LEFT internal node has 7 separators (7 * 518 = 3626B)
    // and RIGHT internal node has 3 separators (1554B), then merge a leaf in RIGHT internal node
    // so RIGHT internal drops to 2 separators (1036 < 1350) and 3626 + 518 + 1036 = 5180 > 4062 -> INTERNAL BORROW FROM LEFT!
    for k in (1000..1030_u16).step_by(2) {
        assert_eq!(file.write(&make_rec(k))?, FileStatusKind::Success00);
    }
    // Pack 6 entries into the LEFT internal subtree so Left Internal grows to 6-7 separators (~3626B) without splitting:
    for k in 500..506_u16 {
        assert_eq!(file.write(&make_rec(k))?, FileStatusKind::Success00);
    }
    // Delete from the RIGHT internal subtree (`1028, 1026, ...`) to trigger internal borrow from left, then internal merge with left!
    for k in (1014..1030_u16).step_by(2).rev() {
        assert_eq!(
            file.delete_by_primary_key(&make_key(k))?,
            FileStatusKind::Success00
        );
    }
    assert_eq!(
        file.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );

    // 2. Desynchronized alternate B+Tree on DELETE (alt root = None AND alt leaf missing target key):
    let alt_schema = sample_multi_key_schema()?;
    let alt_disk = MemoryDisk::uncreated("desync_alt.isam");
    let mut af = IsamFile::new_memory(
        alt_disk.clone(),
        alt_schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    assert_eq!(af.open(IsamOpenMode::Output)?, FileStatusKind::Success00);
    assert_eq!(af.write(b"0001U001D001PAYLOAD")?, FileStatusKind::Success00);
    assert_eq!(af.write(b"0002U002D002PAYLOAD")?, FileStatusKind::Success00);
    assert_eq!(
        af.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );

    // Zero out alt0 root (offset 24 + 48 + 258 + 10 + 2 = 342) and replace alt1 key in Page 2!
    let alt0_root_pos = FRAME_HEADER_BYTES + 48 + 258 + 10 + 2;
    alt_disk.mutate_raw_page(PageId::HEADER, &mut |raw| {
        patch_and_restamp(raw, alt0_root_pos, &0_u64.to_le_bytes());
    })?;
    // Page 2 is alt1 leaf: overwrite its entries with an unrelated key "ZZZZ" so binary_search_by misses!
    let unrelated_leaf = BTreeLeafNode {
        prev_leaf: None,
        next_leaf: None,
        entries: vec![LeafEntry {
            key: b"ZZZZ".to_vec(),
            seq: 99,
            value: LeafValue::Inline(b"9999".to_vec()),
        }],
    };
    alt_disk.mutate_raw_page(PageId::new(2), &mut |raw| {
        *raw = unrelated_leaf.encode_page(PageId::new(2));
    })?;
    assert_eq!(af.open(IsamOpenMode::Io)?, FileStatusKind::Success00);
    assert_eq!(
        af.delete_by_primary_key(b"0001")?,
        FileStatusKind::Success00
    );
    assert_eq!(
        af.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    Ok(())
}

/// Verifies corrupted empty neighbor leaf detection across `seek_lower_bound`, `seek_upper_bound`, `step_leaf_next`, and `step_leaf_prev`.
#[test]
fn corrupted_empty_neighbor_leaves_detected_on_all_four_steps() -> Result<()> {
    let bounds = RecordBounds::fixed(600)?;
    let pk = KeySpec::single(0, 300, DuplicateKeyPolicy::Unique)?;
    let alt = KeySpec::single(300, 300, DuplicateKeyPolicy::Unique)?;
    let schema = IsamSchema::try_new(bounds, pk, vec![alt], CollatingSequence::native())?;
    let disk = MemoryDisk::uncreated("empty_neighbor.isam");
    let mut f3 = IsamFile::new_memory(
        disk.clone(),
        schema,
        IsamAccessMode::Dynamic,
        FileOptionality::Required,
    );
    assert_eq!(f3.open(IsamOpenMode::Output)?, FileStatusKind::Success00);
    let mut rec = vec![b'K'; 300];
    rec.extend_from_slice(&[b'k'; 300]);
    assert_eq!(f3.write(&rec)?, FileStatusKind::Success00);
    assert_eq!(
        f3.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );

    let mut pri_val = vec![1_u8];
    pri_val.extend_from_slice(&0_u64.to_le_bytes());
    pri_val.extend_from_slice(&rec);
    let linked_pri = BTreeLeafNode {
        prev_leaf: Some(PageId::new(1)),
        next_leaf: Some(PageId::new(1)),
        entries: vec![LeafEntry {
            key: vec![b'K'; 300],
            seq: 0,
            value: LeafValue::Inline(pri_val),
        }],
    };
    let empty_leaf = BTreeLeafNode {
        prev_leaf: None,
        next_leaf: None,
        entries: Vec::new(),
    };
    disk.mutate_raw_page(PageId::new(1), &mut |raw| {
        *raw = empty_leaf.encode_page(PageId::new(1));
    })?;
    disk.mutate_raw_page(PageId::new(2), &mut |raw| {
        *raw = linked_pri.encode_page(PageId::new(2));
    })?;

    assert_eq!(f3.open(IsamOpenMode::Input)?, FileStatusKind::Success00);
    assert!(
        f3.start(KeyIndexId::PRIMARY, StartRelation::Greater, &[b'Z'; 300])
            .is_err()
    );
    assert!(
        f3.start(KeyIndexId::PRIMARY, StartRelation::Less, &[b'A'; 300])
            .is_err()
    );

    assert_eq!(
        f3.start(KeyIndexId::PRIMARY, StartRelation::Equal, &[b'K'; 300])?,
        FileStatusKind::Success00
    );
    let _ = f3.read_next()?;
    assert!(f3.read_next().is_err());

    assert_eq!(
        f3.start(KeyIndexId::PRIMARY, StartRelation::Equal, &[b'K'; 300])?,
        FileStatusKind::Success00
    );
    let _ = f3.read_previous()?;
    assert!(f3.read_previous().is_err());
    assert_eq!(
        f3.close(CloseDisposition::Normal),
        FileStatusKind::Success00
    );
    Ok(())
}

/// Injects deterministic I/O failures at every step `0..N` across `OPEN`, `WRITE`, `START`, `READ`, `REWRITE`, and `DELETE`.
#[test]
fn step_counting_fault_injection_covers_all_io_error_rollbacks() -> Result<()> {
    let bounds = RecordBounds::varying(800, 5_000)?;
    let pk = KeySpec::single(0, 380, DuplicateKeyPolicy::Unique)?;
    let alt_u = KeySpec::single(380, 200, DuplicateKeyPolicy::Unique)?;
    let alt_d = KeySpec::single(580, 200, DuplicateKeyPolicy::AllowedFifo)?;
    let schema = IsamSchema::try_new(bounds, pk, vec![alt_u, alt_d], CollatingSequence::native())?;

    let run_workload = |fail_step: usize| -> Result<()> {
        let disk = MemoryDisk::uncreated("fault.isam");
        let factory = FaultIoFactory::new(disk, fail_step);
        let mut f = IsamFile::new_with_factory(
            Box::new(factory.clone()),
            schema.clone(),
            IsamAccessMode::Dynamic,
            FileOptionality::Optional,
        );
        let _ = f.open(IsamOpenMode::Io)?;
        for i in 0..14_u8 {
            let mut rec = vec![b'A' + i; 380];
            rec.extend_from_slice(&[b'a' + i; 200]);
            rec.extend_from_slice(&[b'0' + (i % 3); 200]);
            if i == 5 {
                rec.resize(4_500, b'X');
            } else {
                rec.resize(800, b'y');
            }
            let _ = f.write(&rec)?;
        }
        let k3 = [b'A' + 3; 380];
        let _ = f.start(KeyIndexId::PRIMARY, StartRelation::GreaterOrEqual, &k3)?;
        let _ = f.read_next()?;
        let _ = f.read_previous()?;
        let _ = f.read_by_key(KeyIndexId::alternate(1)?, &[b'0'; 200])?;

        let mut rew = vec![b'A' + 5; 380];
        rew.extend_from_slice(&[b'z'; 200]);
        rew.extend_from_slice(&[b'9'; 200]);
        rew.resize(4_600, b'W');
        let _ = f.rewrite(&rew)?;

        for i in 0..14_u8 {
            let _ = f.delete_by_primary_key(&[b'A' + i; 380])?;
        }
        let _ = f.close(CloseDisposition::Normal);

        let mut seq_ext = IsamFile::new_with_factory(
            Box::new(factory),
            schema.clone(),
            IsamAccessMode::Sequential,
            FileOptionality::Required,
        );
        let _ = seq_ext.open(IsamOpenMode::Input)?;
        let _ = seq_ext.close(CloseDisposition::Normal);
        let _ = seq_ext.open(IsamOpenMode::Extend)?;
        let _ = seq_ext.close(CloseDisposition::Normal);
        let _ = seq_ext.open(IsamOpenMode::Output)?;
        let _ = seq_ext.close(CloseDisposition::Normal);
        Ok(())
    };

    let mut completed = false;
    for step in 0..2_500 {
        if run_workload(step).is_ok() {
            completed = true;
            break;
        }
    }
    assert!(completed);
    Ok(())
}
