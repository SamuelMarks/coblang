//! Order-$B$ B+Tree index structures, doubly-linked leaf cursors, composite/split key routing,
//! FIFO duplicate alternate keys, generic `START`, and transactional COBOL `INDEXED` verbs (`M15`).

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use coblang_common::{CoblangError, FileStatusKind, Result};

use crate::page::{
    DiskAccessPerm, DiskPageIo, DuplicateKeyPolicy, IsamSchema, KeyIndexId, MAX_ALTERNATE_KEYS,
    MAX_KEY_BYTES, MemoryDisk, MemoryPageIo, PAGE_PAYLOAD_BYTES, PAGE_SIZE, PageId, PageIo,
    PageKind, Pager, PayloadReader, SlotIndex, build_framed_page,
};

/// Maximum payload byte length stored inline inside a [`BTreeLeafNode`] entry (`768` bytes).
///
/// Payloads larger than `768` bytes spill into chained [`PageKind::RecordPayload`] pages.
pub const MAX_INLINE_VALUE_BYTES: usize = 768;

/// Maximum serialized separator byte budget inside a [`BTreeInternalNode`] (`4062` bytes).
pub const INTERNAL_PAYLOAD_CAPACITY: usize = PAGE_PAYLOAD_BYTES - 10;

/// Maximum serialized entry byte budget inside a [`BTreeLeafNode`] (`4054` bytes).
pub const LEAF_PAYLOAD_CAPACITY: usize = PAGE_PAYLOAD_BYTES - 18;

/// Byte occupancy threshold below which a non-root leaf triggers borrow or merge (`1350` bytes).
pub const LEAF_UNDERFLOW_BYTES: usize = 1350;

/// Byte occupancy threshold below which a non-root internal node triggers borrow or merge (`1350` bytes).
pub const INTERNAL_UNDERFLOW_BYTES: usize = 1350;

/// Relational operator for COBOL `START file KEY IS <relation> data-name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartRelation {
    /// `KEY IS =` / `EQUAL TO` (exact full-key or generic prefix equality).
    Equal,
    /// `KEY IS >` / `GREATER THAN` (strictly greater full key or generic prefix).
    Greater,
    /// `KEY IS >=` / `NOT LESS THAN` / `GREATER THAN OR EQUAL TO`.
    GreaterOrEqual,
    /// `KEY IS <` / `LESS THAN` (COBOL 2002/2023 backward positioning at greatest key `< prefix`).
    Less,
    /// `KEY IS <=` / `NOT GREATER THAN` / `LESS THAN OR EQUAL TO` (greatest key `<= prefix`).
    LessOrEqual,
}

/// COBOL `ACCESS MODE` clause governing permissible statements on an [`IsamFile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IsamAccessMode {
    /// `ACCESS MODE IS SEQUENTIAL`.
    Sequential,
    /// `ACCESS MODE IS RANDOM`.
    Random,
    /// `ACCESS MODE IS DYNAMIC`.
    Dynamic,
}

/// COBOL `OPEN` mode for an `ORGANIZATION IS INDEXED` file connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IsamOpenMode {
    /// `OPEN INPUT` (read-only).
    Input,
    /// `OPEN OUTPUT` (creates or truncates the indexed file).
    Output,
    /// `OPEN I-O` (bidirectional read/write/rewrite/delete).
    Io,
    /// `OPEN EXTEND` (sequential ascending primary-key append).
    Extend,
}

/// Presence of `SELECT OPTIONAL` on an indexed file connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileOptionality {
    /// Mandatory file (`OPEN INPUT`/`I-O`/`EXTEND` on missing file returns `"35"`).
    Required,
    /// `SELECT OPTIONAL` (`OPEN INPUT` returns `"05"` at EOF; `OPEN I-O`/`EXTEND` creates with `"05"`).
    Optional,
}

/// Disposition supplied to `CLOSE` (`CLOSE file` vs `CLOSE file WITH LOCK`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseDisposition {
    /// Normal `CLOSE` allowing subsequent `OPEN` in the same run unit.
    Normal,
    /// `CLOSE WITH LOCK` permanently locking the connector for the remainder of the run unit (`"38"`).
    WithLock,
}

/// Value representation inside a [`LeafEntry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafValue {
    /// Payload stored inline inside the 4096-byte leaf page (`<= MAX_INLINE_VALUE_BYTES`).
    Inline(Vec<u8>),
    /// Payload stored across chained [`PageKind::RecordPayload`] pages.
    Overflow {
        /// Total unchunked byte length of the overflow payload.
        total_len: u32,
        /// Head [`PageId`] of the overflow chain.
        first_page: PageId,
    },
}

impl LeafValue {
    /// Returns the serialized byte size of `self` inside a leaf page (`3 + len` or `13`).
    #[must_use]
    pub fn encoded_byte_len(&self) -> usize {
        match self {
            Self::Inline(bytes) => 3 + bytes.len(),
            Self::Overflow { .. } => 13,
        }
    }
}

/// Single sorted `(key, seq) -> value` cell stored inside a [`BTreeLeafNode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafEntry {
    /// Collation-encoded index key (`1..=MAX_KEY_BYTES`).
    pub key: Vec<u8>,
    /// `0` for `Unique` indices; monotonic `>= 1` FIFO sequence number for `AllowedFifo` indices.
    pub seq: u64,
    /// Associated inline or overflow payload.
    pub value: LeafValue,
}

impl LeafEntry {
    /// Computes the exact serialized byte footprint of this entry inside a leaf page.
    #[must_use]
    pub fn encoded_byte_len(&self) -> usize {
        10 + self.key.len() + self.value.encoded_byte_len()
    }
}

/// Doubly-linked B+Tree leaf page (`PageKind::BTreeLeaf`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BTreeLeafNode {
    /// Optional predecessor leaf page in ascending key order.
    pub prev_leaf: Option<PageId>,
    /// Optional successor leaf page in ascending key order.
    pub next_leaf: Option<PageId>,
    /// Strictly ascending `(key, seq)` entries.
    pub entries: Vec<LeafEntry>,
}

impl BTreeLeafNode {
    /// Computes the total serialized byte size of `self.entries`.
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        self.entries.iter().map(LeafEntry::encoded_byte_len).sum()
    }

    /// Encodes `self` into a CRC32-framed 4096-byte [`PageKind::BTreeLeaf`] buffer.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Identifier of this leaf page.
    #[must_use]
    pub fn encode_page(&self, page_id: PageId) -> [u8; PAGE_SIZE] {
        let mut out = Vec::with_capacity(18 + self.payload_bytes());
        out.extend_from_slice(&PageId::encode_optional(self.prev_leaf).to_le_bytes());
        out.extend_from_slice(&PageId::encode_optional(self.next_leaf).to_le_bytes());
        let count = u16::try_from(self.entries.len()).unwrap_or(0);
        out.extend_from_slice(&count.to_le_bytes());
        for entry in &self.entries {
            let key_len = u16::try_from(entry.key.len()).unwrap_or(0);
            out.extend_from_slice(&key_len.to_le_bytes());
            out.extend_from_slice(&entry.seq.to_le_bytes());
            out.extend_from_slice(&entry.key);
            match &entry.value {
                LeafValue::Inline(bytes) => {
                    out.push(0);
                    let val_len = u16::try_from(bytes.len()).unwrap_or(0);
                    out.extend_from_slice(&val_len.to_le_bytes());
                    out.extend_from_slice(bytes);
                }
                LeafValue::Overflow {
                    total_len,
                    first_page,
                } => {
                    out.push(1);
                    out.extend_from_slice(&total_len.to_le_bytes());
                    out.extend_from_slice(&first_page.as_u64().to_le_bytes());
                }
            }
        }
        build_framed_page(PageKind::BTreeLeaf, page_id, &out)
    }

    /// Decodes a payload-verified [`PageKind::BTreeLeaf`] buffer into [`BTreeLeafNode`].
    ///
    /// # Arguments
    ///
    /// * `raw_page` - Frame/CRC32-verified 4096-byte leaf page buffer.
    /// * `page_id` - Identifier of this leaf page.
    /// * `path` - File path label for corruption diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if any entry length, value tag, overflow link, or sort
    /// invariant is corrupted.
    pub fn decode_body(raw_page: &[u8; PAGE_SIZE], page_id: PageId, path: &str) -> Result<Self> {
        let mut rdr = PayloadReader::new(raw_page, path, page_id);
        let prev_leaf = PageId::decode_optional(rdr.read_u64()?);
        let next_leaf = PageId::decode_optional(rdr.read_u64()?);
        let count = usize::from(rdr.read_u16()?);
        let mut entries: Vec<LeafEntry> = Vec::with_capacity(count.min(512));
        for _ in 0..count {
            let key_len = usize::from(rdr.read_u16()?);
            if key_len == 0 || key_len > MAX_KEY_BYTES {
                return Err(CoblangError::IsamStorage {
                    path: String::from(path),
                    page_id: page_id.as_u64(),
                    message: format!("corrupted leaf key length {key_len}"),
                });
            }
            let seq = rdr.read_u64()?;
            let key = rdr.read_slice(key_len)?.to_vec();
            let value = match rdr.read_u8()? {
                0 => {
                    let val_len = usize::from(rdr.read_u16()?);
                    if val_len > MAX_INLINE_VALUE_BYTES {
                        return Err(CoblangError::IsamStorage {
                            path: String::from(path),
                            page_id: page_id.as_u64(),
                            message: format!("corrupted inline value length {val_len}"),
                        });
                    }
                    LeafValue::Inline(rdr.read_slice(val_len)?.to_vec())
                }
                1 => {
                    let total_len = rdr.read_u32()?;
                    let first_page = PageId::decode_optional(rdr.read_u64()?).ok_or_else(|| {
                        CoblangError::IsamStorage {
                            path: String::from(path),
                            page_id: page_id.as_u64(),
                            message: String::from("corrupted overflow value with null page 0"),
                        }
                    })?;
                    LeafValue::Overflow {
                        total_len,
                        first_page,
                    }
                }
                other => {
                    return Err(CoblangError::IsamStorage {
                        path: String::from(path),
                        page_id: page_id.as_u64(),
                        message: format!("unknown LeafValue tag {other}"),
                    });
                }
            };
            if let Some(prev) = entries.last()
                && cmp_key_seq(&prev.key, prev.seq, &key, seq).is_ge()
            {
                return Err(CoblangError::IsamStorage {
                    path: String::from(path),
                    page_id: page_id.as_u64(),
                    message: String::from("corrupted leaf entries out of strict ascending order"),
                });
            }
            entries.push(LeafEntry { key, seq, value });
        }
        Ok(Self {
            prev_leaf,
            next_leaf,
            entries,
        })
    }
}

/// Routing separator key `(key, seq)` inside a [`BTreeInternalNode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeparatorKey {
    /// Collation-encoded separator key bytes (`1..=MAX_KEY_BYTES`).
    pub key: Vec<u8>,
    /// FIFO tie-breaker sequence number (`0` for `Unique` trees).
    pub seq: u64,
}

impl SeparatorKey {
    /// Serialized byte footprint inside an internal routing page (`18 + key.len()`).
    #[must_use]
    pub fn encoded_byte_len(&self) -> usize {
        18 + self.key.len()
    }
}

/// Order-$B$ B+Tree internal routing page (`PageKind::BTreeInternal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BTreeInternalNode {
    /// Child page pointers (`separators.len() + 1` elements).
    pub children: Vec<PageId>,
    /// Strictly ascending separator keys (`>= 1`).
    pub separators: Vec<SeparatorKey>,
}

impl BTreeInternalNode {
    /// Computes the serialized separator payload size (`sum(18 + key.len())`).
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        self.separators
            .iter()
            .map(SeparatorKey::encoded_byte_len)
            .sum()
    }

    /// Encodes `self` into a CRC32-framed 4096-byte [`PageKind::BTreeInternal`] buffer.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Identifier of this internal page.
    #[must_use]
    pub fn encode_page(&self, page_id: PageId) -> [u8; PAGE_SIZE] {
        let mut out = Vec::with_capacity(10 + self.payload_bytes());
        let count = u16::try_from(self.separators.len()).unwrap_or(0);
        out.extend_from_slice(&count.to_le_bytes());
        let first_child = self.children.first().copied().unwrap_or(PageId::HEADER);
        out.extend_from_slice(&first_child.as_u64().to_le_bytes());
        for (sep, right_child) in self.separators.iter().zip(self.children.iter().skip(1)) {
            let key_len = u16::try_from(sep.key.len()).unwrap_or(0);
            out.extend_from_slice(&key_len.to_le_bytes());
            out.extend_from_slice(&sep.seq.to_le_bytes());
            out.extend_from_slice(&right_child.as_u64().to_le_bytes());
            out.extend_from_slice(&sep.key);
        }
        build_framed_page(PageKind::BTreeInternal, page_id, &out)
    }

    /// Decodes a payload-verified [`PageKind::BTreeInternal`] buffer into [`BTreeInternalNode`].
    ///
    /// # Arguments
    ///
    /// * `raw_page` - Frame/CRC32-verified 4096-byte internal page buffer.
    /// * `page_id` - Identifier of this internal page.
    /// * `path` - File path label for corruption diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if separator count is zero, child links are zero,
    /// key lengths are invalid, or separators are unsorted.
    pub fn decode_body(raw_page: &[u8; PAGE_SIZE], page_id: PageId, path: &str) -> Result<Self> {
        let mut rdr = PayloadReader::new(raw_page, path, page_id);
        let count = usize::from(rdr.read_u16()?);
        if count == 0 {
            return Err(CoblangError::IsamStorage {
                path: String::from(path),
                page_id: page_id.as_u64(),
                message: String::from("corrupted internal B+Tree page with 0 separators"),
            });
        }
        let first_child =
            PageId::decode_optional(rdr.read_u64()?).ok_or_else(|| CoblangError::IsamStorage {
                path: String::from(path),
                page_id: page_id.as_u64(),
                message: String::from("corrupted internal B+Tree page with null first_child"),
            })?;
        let mut children = Vec::with_capacity(count.min(512) + 1);
        let mut separators: Vec<SeparatorKey> = Vec::with_capacity(count.min(512));
        children.push(first_child);
        for _ in 0..count {
            let key_len = usize::from(rdr.read_u16()?);
            if key_len == 0 || key_len > MAX_KEY_BYTES {
                return Err(CoblangError::IsamStorage {
                    path: String::from(path),
                    page_id: page_id.as_u64(),
                    message: format!("corrupted internal separator key length {key_len}"),
                });
            }
            let seq = rdr.read_u64()?;
            let right_child = PageId::decode_optional(rdr.read_u64()?).ok_or_else(|| {
                CoblangError::IsamStorage {
                    path: String::from(path),
                    page_id: page_id.as_u64(),
                    message: String::from("corrupted internal B+Tree page with null right_child"),
                }
            })?;
            let key = rdr.read_slice(key_len)?.to_vec();
            if let Some(prev) = separators.last()
                && cmp_key_seq(&prev.key, prev.seq, &key, seq).is_ge()
            {
                return Err(CoblangError::IsamStorage {
                    path: String::from(path),
                    page_id: page_id.as_u64(),
                    message: String::from("corrupted internal separators out of ascending order"),
                });
            }
            separators.push(SeparatorKey { key, seq });
            children.push(right_child);
        }
        Ok(Self {
            children,
            separators,
        })
    }
}

/// Lexicographically compares composite B+Tree keys `(a_key, a_seq)` vs `(b_key, b_seq)`.
///
/// # Arguments
///
/// * `a_key` - Left collation-encoded key bytes.
/// * `a_seq` - Left FIFO tie-breaker sequence number.
/// * `b_key` - Right collation-encoded key bytes.
/// * `b_seq` - Right FIFO tie-breaker sequence number.
#[must_use]
pub fn cmp_key_seq(a_key: &[u8], a_seq: u64, b_key: &[u8], b_seq: u64) -> Ordering {
    a_key.cmp(b_key).then_with(|| a_seq.cmp(&b_seq))
}

/// Decoded B+Tree node returned during root-to-leaf descent.
enum AnyBTreeNode {
    /// Internal routing node.
    Internal(BTreeInternalNode),
    /// Doubly-linked leaf node.
    Leaf(BTreeLeafNode),
}

/// Epoch-validated physical `(leaf_id, slot)` hint enabling $O(1)$ bidirectional sequential reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LeafCursorHint {
    /// Tree mutation epoch when `leaf_id` and `slot` were recorded.
    epoch: u64,
    /// Physical leaf [`PageId`].
    leaf_id: PageId,
    /// Zero-based [`SlotIndex`] inside `leaf_id`.
    slot: SlotIndex,
}

/// Located leaf entry bundled with its `O(1)` physical cursor coordinates.
struct LocatedLeafEntry {
    /// Physical leaf [`PageId`].
    leaf_id: PageId,
    /// Zero-based [`SlotIndex`] inside `leaf_id`.
    slot: SlotIndex,
    /// Cloned [`LeafEntry`].
    entry: LeafEntry,
}

/// Inclusive vs exclusive bound selector for forward/backward B+Tree range probes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BoundInclusion {
    /// Include entries equal to `(target_key, target_seq)` (`>=` or `<=`).
    Inclusive,
    /// Strictly exclude `(target_key, target_seq)` (`>` or `<`).
    Exclusive,
}

/// Reads and decodes either a [`BTreeInternalNode`] or [`BTreeLeafNode`] at `page_id`.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `page_id` - Target B+Tree node page identifier.
fn read_btree_node(pager: &mut Pager, page_id: PageId) -> Result<AnyBTreeNode> {
    let (kind, raw) = pager.read_any_verified_page(page_id)?;
    let path = String::from(pager.path_label());
    match kind {
        PageKind::BTreeInternal => Ok(AnyBTreeNode::Internal(BTreeInternalNode::decode_body(
            &raw, page_id, &path,
        )?)),
        PageKind::BTreeLeaf => Ok(AnyBTreeNode::Leaf(BTreeLeafNode::decode_body(
            &raw, page_id, &path,
        )?)),
        PageKind::FileHeader | PageKind::Free | PageKind::RecordPayload => {
            Err(CoblangError::IsamStorage {
                path,
                page_id: page_id.as_u64(),
                message: format!("expected B+Tree node page, found {kind:?}"),
            })
        }
    }
}

/// Reads and decodes a [`BTreeLeafNode`] at `page_id`.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `page_id` - Target leaf [`PageId`].
fn read_leaf_node(pager: &mut Pager, page_id: PageId) -> Result<BTreeLeafNode> {
    let raw = pager.read_verified_page(page_id, PageKind::BTreeLeaf)?;
    let path = String::from(pager.path_label());
    BTreeLeafNode::decode_body(&raw, page_id, &path)
}

/// Reads and decodes a [`BTreeInternalNode`] at `page_id`.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `page_id` - Target internal [`PageId`].
fn read_internal_node(pager: &mut Pager, page_id: PageId) -> Result<BTreeInternalNode> {
    let raw = pager.read_verified_page(page_id, PageKind::BTreeInternal)?;
    let path = String::from(pager.path_label());
    BTreeInternalNode::decode_body(&raw, page_id, &path)
}

/// Result of a root-to-leaf B+Tree search traversal.
struct LeafDescent {
    /// Ancestor `(internal_page_id, child_slot_index)` stack from root down to leaf parent.
    path: Vec<(PageId, usize)>,
    /// Located leaf [`PageId`].
    leaf_id: PageId,
    /// Decoded [`BTreeLeafNode`].
    leaf: BTreeLeafNode,
}

/// Descends from `root_id` to the leaf covering `(target_key, target_seq)` in $O(\log N)$ page reads.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `root_id` - Non-null root [`PageId`].
/// * `target_key` - Search key or prefix bytes.
/// * `target_seq` - Search FIFO sequence number.
fn locate_leaf(
    pager: &mut Pager,
    root_id: PageId,
    target_key: &[u8],
    target_seq: u64,
) -> Result<LeafDescent> {
    let max_depth = pager.header().total_pages;
    let mut path = Vec::new();
    let mut cur_id = root_id;
    let mut depth = 0_u64;
    loop {
        depth += 1;
        if depth > max_depth {
            return Err(CoblangError::IsamStorage {
                path: String::from(pager.path_label()),
                page_id: cur_id.as_u64(),
                message: String::from("cycle detected in B+Tree internal routing pages"),
            });
        }
        match read_btree_node(pager, cur_id)? {
            AnyBTreeNode::Leaf(leaf) => {
                return Ok(LeafDescent {
                    path,
                    leaf_id: cur_id,
                    leaf,
                });
            }
            AnyBTreeNode::Internal(internal) => {
                let child_idx = internal.separators.partition_point(|sep| {
                    cmp_key_seq(&sep.key, sep.seq, target_key, target_seq).is_le()
                });
                let next_id = internal
                    .children
                    .get(child_idx)
                    .copied()
                    .unwrap_or(PageId::HEADER);
                path.push((cur_id, child_idx));
                cur_id = next_id;
            }
        }
    }
}

/// Probes the smallest entry `>=` (`Inclusive`) or `>` (`Exclusive`) `(target_key, target_seq)`.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `key_id` - Target B+Tree selector.
/// * `target_key` - Lower-bound key or prefix bytes.
/// * `target_seq` - Lower-bound sequence number.
/// * `inclusion` - [`BoundInclusion::Inclusive`] (`>=`) or [`BoundInclusion::Exclusive`] (`>`).
fn seek_lower_bound(
    pager: &mut Pager,
    key_id: KeyIndexId,
    target_key: &[u8],
    target_seq: u64,
    inclusion: BoundInclusion,
) -> Result<Option<LocatedLeafEntry>> {
    let Some(root_id) = pager.header().tree_root(key_id) else {
        return Ok(None);
    };
    let LeafDescent { leaf_id, leaf, .. } = locate_leaf(pager, root_id, target_key, target_seq)?;
    let idx = leaf.entries.partition_point(|e| {
        let ord = cmp_key_seq(&e.key, e.seq, target_key, target_seq);
        match inclusion {
            BoundInclusion::Inclusive => ord.is_lt(),
            BoundInclusion::Exclusive => ord.is_le(),
        }
    });
    if let Some(entry) = leaf.entries.get(idx) {
        let slot = SlotIndex::new(u16::try_from(idx).unwrap_or(0));
        return Ok(Some(LocatedLeafEntry {
            leaf_id,
            slot,
            entry: entry.clone(),
        }));
    }
    let Some(next_id) = leaf.next_leaf else {
        return Ok(None);
    };
    let next_leaf = read_leaf_node(pager, next_id)?;
    let first = next_leaf
        .entries
        .first()
        .ok_or_else(|| CoblangError::IsamStorage {
            path: String::from(pager.path_label()),
            page_id: next_id.as_u64(),
            message: String::from("corrupted empty non-root leaf on next_leaf chain"),
        })?;
    Ok(Some(LocatedLeafEntry {
        leaf_id: next_id,
        slot: SlotIndex::ZERO,
        entry: first.clone(),
    }))
}

/// Probes the greatest entry `<=` (`Inclusive`) or `<` (`Exclusive`) `(target_key, target_seq)`.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `key_id` - Target B+Tree selector.
/// * `target_key` - Upper-bound key or prefix bytes.
/// * `target_seq` - Upper-bound sequence number.
/// * `inclusion` - [`BoundInclusion::Inclusive`] (`<=`) or [`BoundInclusion::Exclusive`] (`<`).
fn seek_upper_bound(
    pager: &mut Pager,
    key_id: KeyIndexId,
    target_key: &[u8],
    target_seq: u64,
    inclusion: BoundInclusion,
) -> Result<Option<LocatedLeafEntry>> {
    let Some(root_id) = pager.header().tree_root(key_id) else {
        return Ok(None);
    };
    let LeafDescent { leaf_id, leaf, .. } = locate_leaf(pager, root_id, target_key, target_seq)?;
    let idx = leaf.entries.partition_point(|e| {
        let ord = cmp_key_seq(&e.key, e.seq, target_key, target_seq);
        match inclusion {
            BoundInclusion::Inclusive => ord.is_le(),
            BoundInclusion::Exclusive => ord.is_lt(),
        }
    });
    if let Some(prev_idx) = idx.checked_sub(1) {
        let entry = leaf.entries.get(prev_idx).cloned().unwrap_or(LeafEntry {
            key: Vec::new(),
            seq: 0,
            value: LeafValue::Inline(Vec::new()),
        });
        let slot = SlotIndex::new(u16::try_from(prev_idx).unwrap_or(0));
        return Ok(Some(LocatedLeafEntry {
            leaf_id,
            slot,
            entry,
        }));
    }
    let Some(prev_id) = leaf.prev_leaf else {
        return Ok(None);
    };
    let prev_leaf = read_leaf_node(pager, prev_id)?;
    let last_idx =
        prev_leaf
            .entries
            .len()
            .checked_sub(1)
            .ok_or_else(|| CoblangError::IsamStorage {
                path: String::from(pager.path_label()),
                page_id: prev_id.as_u64(),
                message: String::from("corrupted empty non-root leaf on prev_leaf chain"),
            })?;
    let last = prev_leaf
        .entries
        .get(last_idx)
        .cloned()
        .unwrap_or(LeafEntry {
            key: Vec::new(),
            seq: 0,
            value: LeafValue::Inline(Vec::new()),
        });
    Ok(Some(LocatedLeafEntry {
        leaf_id: prev_id,
        slot: SlotIndex::new(u16::try_from(last_idx).unwrap_or(0)),
        entry: last,
    }))
}

/// Steps one entry forward in $O(1)$ using doubly-linked `next_leaf` pointers.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `leaf_id` - Current leaf [`PageId`].
/// * `slot` - Current [`SlotIndex`] within `leaf_id`.
fn step_leaf_next(
    pager: &mut Pager,
    leaf_id: PageId,
    slot: SlotIndex,
) -> Result<Option<LocatedLeafEntry>> {
    let leaf = read_leaf_node(pager, leaf_id)?;
    let next_idx = slot.as_usize() + 1;
    if let Some(entry) = leaf.entries.get(next_idx) {
        return Ok(Some(LocatedLeafEntry {
            leaf_id,
            slot: SlotIndex::new(u16::try_from(next_idx).unwrap_or(0)),
            entry: entry.clone(),
        }));
    }
    let Some(next_id) = leaf.next_leaf else {
        return Ok(None);
    };
    let next_leaf = read_leaf_node(pager, next_id)?;
    let first = next_leaf
        .entries
        .first()
        .ok_or_else(|| CoblangError::IsamStorage {
            path: String::from(pager.path_label()),
            page_id: next_id.as_u64(),
            message: String::from("corrupted empty leaf encountered during READ NEXT"),
        })?;
    Ok(Some(LocatedLeafEntry {
        leaf_id: next_id,
        slot: SlotIndex::ZERO,
        entry: first.clone(),
    }))
}

/// Steps one entry backward in $O(1)$ using doubly-linked `prev_leaf` pointers.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `leaf_id` - Current leaf [`PageId`].
/// * `slot` - Current [`SlotIndex`] within `leaf_id`.
fn step_leaf_prev(
    pager: &mut Pager,
    leaf_id: PageId,
    slot: SlotIndex,
) -> Result<Option<LocatedLeafEntry>> {
    let leaf = read_leaf_node(pager, leaf_id)?;
    if let Some(prev_idx) = slot.as_usize().checked_sub(1) {
        let entry = leaf.entries.get(prev_idx).cloned().unwrap_or(LeafEntry {
            key: Vec::new(),
            seq: 0,
            value: LeafValue::Inline(Vec::new()),
        });
        return Ok(Some(LocatedLeafEntry {
            leaf_id,
            slot: SlotIndex::new(u16::try_from(prev_idx).unwrap_or(0)),
            entry,
        }));
    }
    let Some(prev_id) = leaf.prev_leaf else {
        return Ok(None);
    };
    let prev_leaf = read_leaf_node(pager, prev_id)?;
    let last_idx =
        prev_leaf
            .entries
            .len()
            .checked_sub(1)
            .ok_or_else(|| CoblangError::IsamStorage {
                path: String::from(pager.path_label()),
                page_id: prev_id.as_u64(),
                message: String::from("corrupted empty leaf encountered during READ PREVIOUS"),
            })?;
    let last = prev_leaf
        .entries
        .get(last_idx)
        .cloned()
        .unwrap_or(LeafEntry {
            key: Vec::new(),
            seq: 0,
            value: LeafValue::Inline(Vec::new()),
        });
    Ok(Some(LocatedLeafEntry {
        leaf_id: prev_id,
        slot: SlotIndex::new(u16::try_from(last_idx).unwrap_or(0)),
        entry: last,
    }))
}

/// Materializes a [`LeafValue`] into owned bytes, resolving any overflow chain.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `value` - Inline or overflow leaf value.
fn resolve_leaf_value(pager: &mut Pager, value: &LeafValue) -> Result<Vec<u8>> {
    match value {
        LeafValue::Inline(bytes) => Ok(bytes.clone()),
        LeafValue::Overflow {
            total_len,
            first_page,
        } => pager.read_overflow_chain(*first_page, *total_len),
    }
}

/// Packs `payload` into [`LeafValue::Inline`] or allocates an overflow [`PageKind::RecordPayload`] chain.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `payload` - Serialized leaf value bytes.
fn build_leaf_value(pager: &mut Pager, payload: Vec<u8>) -> Result<LeafValue> {
    if payload.len() <= MAX_INLINE_VALUE_BYTES {
        return Ok(LeafValue::Inline(payload));
    }
    let total_len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let first_page = pager.write_overflow_chain(&payload)?;
    Ok(LeafValue::Overflow {
        total_len,
        first_page,
    })
}

/// Chooses the byte-balanced split index `k` in `1..entries.len()` minimizing `|left_bytes - right_bytes|`.
///
/// Because `MAX_KEY_BYTES + MAX_INLINE_VALUE_BYTES + 13 == 1293` and `total <= 4054 + 1293`, the
/// minimum-difference split point mathematically satisfies `max(left, right) <= 3320 <= 4054`.
///
/// # Arguments
///
/// * `entries` - Overflowing leaf entry vector (`>= 4` entries).
fn choose_leaf_split_index(entries: &[LeafEntry]) -> usize {
    let total: usize = entries.iter().map(LeafEntry::encoded_byte_len).sum();
    let mut left_sum = 0_usize;
    let mut best_idx = 1_usize;
    let mut best_diff = usize::MAX;
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            let right_sum = total.saturating_sub(left_sum);
            let diff = left_sum.abs_diff(right_sum);
            if diff < best_diff {
                best_diff = diff;
                best_idx = i;
            }
        }
        left_sum = left_sum.saturating_add(entry.encoded_byte_len());
    }
    best_idx
}

/// Inserts or replaces `(key, seq) -> raw_payload` inside `key_id`'s B+Tree, splitting nodes as needed.
///
/// # Arguments
///
/// * `pager` - Active transactional [`Pager`].
/// * `key_id` - Target B+Tree selector.
/// * `key` - Collation-encoded key bytes (`1..=512`).
/// * `seq` - `0` for `Unique` indices or monotonic FIFO sequence number.
/// * `raw_payload` - Value bytes to associate with `(key, seq)`.
/// * `epoch` - Mutable tree generation counter incremented on structural/entry mutation.
fn btree_upsert(
    pager: &mut Pager,
    key_id: KeyIndexId,
    key: Vec<u8>,
    seq: u64,
    raw_payload: Vec<u8>,
    epoch: &mut u64,
) -> Result<()> {
    *epoch = epoch.wrapping_add(1);
    let value = build_leaf_value(pager, raw_payload)?;
    let new_entry = LeafEntry { key, seq, value };

    let Some(root_id) = pager.header().tree_root(key_id) else {
        let leaf_id = pager.alloc_page()?;
        let root_leaf = BTreeLeafNode {
            prev_leaf: None,
            next_leaf: None,
            entries: vec![new_entry],
        };
        pager.stage_raw_page(leaf_id, root_leaf.encode_page(leaf_id));
        pager.header_mut().set_tree_root(key_id, Some(leaf_id));
        return Ok(());
    };

    let LeafDescent {
        path,
        leaf_id,
        mut leaf,
    } = locate_leaf(pager, root_id, &new_entry.key, new_entry.seq)?;
    match leaf
        .entries
        .binary_search_by(|e| cmp_key_seq(&e.key, e.seq, &new_entry.key, new_entry.seq))
    {
        Ok(existing_idx) => {
            if let Some(LeafValue::Overflow { first_page, .. }) =
                leaf.entries.get(existing_idx).map(|e| &e.value)
            {
                pager.free_overflow_chain(*first_page)?;
            }
            let _ = leaf
                .entries
                .get_mut(existing_idx)
                .map(|slot| *slot = new_entry);
        }
        Err(insert_idx) => {
            leaf.entries.insert(insert_idx, new_entry);
        }
    }

    if leaf.payload_bytes() <= LEAF_PAYLOAD_CAPACITY {
        pager.stage_raw_page(leaf_id, leaf.encode_page(leaf_id));
        return Ok(());
    }

    let split_idx = choose_leaf_split_index(&leaf.entries);
    let right_entries = leaf.entries.split_off(split_idx);
    let first_right = right_entries.first().cloned().unwrap_or(LeafEntry {
        key: Vec::new(),
        seq: 0,
        value: LeafValue::Inline(Vec::new()),
    });
    let promoted_sep = SeparatorKey {
        key: first_right.key,
        seq: first_right.seq,
    };

    let right_id = pager.alloc_page()?;
    let old_next = leaf.next_leaf;
    leaf.next_leaf = Some(right_id);
    let right_leaf = BTreeLeafNode {
        prev_leaf: Some(leaf_id),
        next_leaf: old_next,
        entries: right_entries,
    };
    if let Some(after_id) = old_next {
        let mut after_leaf = read_leaf_node(pager, after_id)?;
        after_leaf.prev_leaf = Some(right_id);
        pager.stage_raw_page(after_id, after_leaf.encode_page(after_id));
    }
    pager.stage_raw_page(leaf_id, leaf.encode_page(leaf_id));
    pager.stage_raw_page(right_id, right_leaf.encode_page(right_id));

    propagate_split_upward(pager, key_id, root_id, path, promoted_sep, right_id)
}

/// Inserts `(promoted_sep, right_id)` into internal ancestors along `path`, splitting up to a new root if needed.
///
/// # Arguments
///
/// * `pager` - Active transactional [`Pager`].
/// * `key_id` - Target B+Tree selector.
/// * `root_id` - Previous root [`PageId`].
/// * `path` - Ancestor `(internal_id, child_slot)` stack from root down to split leaf's parent.
/// * `promoted_sep` - Separator key promoted from the child split.
/// * `right_id` - Newly allocated right child [`PageId`].
fn propagate_split_upward(
    pager: &mut Pager,
    key_id: KeyIndexId,
    root_id: PageId,
    path: Vec<(PageId, usize)>,
    mut promoted_sep: SeparatorKey,
    mut right_id: PageId,
) -> Result<()> {
    for (parent_id, child_idx) in path.into_iter().rev() {
        let mut parent = read_internal_node(pager, parent_id)?;
        parent.separators.insert(child_idx, promoted_sep);
        parent.children.insert(child_idx + 1, right_id);
        if parent.payload_bytes() <= INTERNAL_PAYLOAD_CAPACITY {
            pager.stage_raw_page(parent_id, parent.encode_page(parent_id));
            return Ok(());
        }
        let mid = parent.separators.len() / 2;
        let right_seps = parent.separators.split_off(mid + 1);
        let up_sep = parent.separators.pop().unwrap_or(SeparatorKey {
            key: Vec::new(),
            seq: 0,
        });
        let right_children = parent.children.split_off(mid + 1);
        let new_right_internal_id = pager.alloc_page()?;
        let right_internal = BTreeInternalNode {
            children: right_children,
            separators: right_seps,
        };
        pager.stage_raw_page(parent_id, parent.encode_page(parent_id));
        pager.stage_raw_page(
            new_right_internal_id,
            right_internal.encode_page(new_right_internal_id),
        );
        promoted_sep = up_sep;
        right_id = new_right_internal_id;
    }

    let new_root_id = pager.alloc_page()?;
    let new_root = BTreeInternalNode {
        children: vec![root_id, right_id],
        separators: vec![promoted_sep],
    };
    pager.stage_raw_page(new_root_id, new_root.encode_page(new_root_id));
    pager.header_mut().set_tree_root(key_id, Some(new_root_id));
    Ok(())
}

/// Removes `(target_key, target_seq)` from `key_id`'s B+Tree, rebalancing and reclaiming freed pages.
///
/// Returns `Ok(Some(resolved_payload_bytes))` when found and removed, or `Ok(None)` when absent.
///
/// # Arguments
///
/// * `pager` - Active transactional [`Pager`].
/// * `key_id` - Target B+Tree selector.
/// * `target_key` - Exact collation-encoded key bytes.
/// * `target_seq` - Exact FIFO sequence number (`0` for `Unique` indices).
/// * `epoch` - Mutable tree generation counter incremented when removed.
fn btree_remove(
    pager: &mut Pager,
    key_id: KeyIndexId,
    target_key: &[u8],
    target_seq: u64,
    epoch: &mut u64,
) -> Result<Option<Vec<u8>>> {
    let Some(root_id) = pager.header().tree_root(key_id) else {
        return Ok(None);
    };
    let LeafDescent {
        mut path,
        leaf_id,
        mut leaf,
    } = locate_leaf(pager, root_id, target_key, target_seq)?;
    let Ok(found_idx) = leaf
        .entries
        .binary_search_by(|e| cmp_key_seq(&e.key, e.seq, target_key, target_seq))
    else {
        return Ok(None);
    };
    *epoch = epoch.wrapping_add(1);
    let removed = leaf.entries.remove(found_idx);
    let removed_bytes = resolve_leaf_value(pager, &removed.value)?;
    if let LeafValue::Overflow { first_page, .. } = removed.value {
        pager.free_overflow_chain(first_page)?;
    }

    let Some((parent_id, child_idx)) = path.pop() else {
        if leaf.entries.is_empty() {
            pager.free_page(leaf_id);
            pager.header_mut().set_tree_root(key_id, None);
        } else {
            pager.stage_raw_page(leaf_id, leaf.encode_page(leaf_id));
        }
        return Ok(Some(removed_bytes));
    };

    if leaf.payload_bytes() >= LEAF_UNDERFLOW_BYTES {
        pager.stage_raw_page(leaf_id, leaf.encode_page(leaf_id));
        return Ok(Some(removed_bytes));
    }

    let mut parent = read_internal_node(pager, parent_id)?;
    let (sep_idx, left_id, mut left_leaf, right_id, mut right_leaf) = if child_idx > 0 {
        let sib_id = parent
            .children
            .get(child_idx - 1)
            .copied()
            .unwrap_or(PageId::HEADER);
        let sib = read_leaf_node(pager, sib_id)?;
        (child_idx - 1, sib_id, sib, leaf_id, leaf)
    } else {
        let sib_id = parent.children.get(1).copied().unwrap_or(PageId::HEADER);
        let sib = read_leaf_node(pager, sib_id)?;
        (0, leaf_id, leaf, sib_id, sib)
    };

    if left_leaf.payload_bytes() + right_leaf.payload_bytes() <= LEAF_PAYLOAD_CAPACITY {
        left_leaf.entries.append(&mut right_leaf.entries);
        left_leaf.next_leaf = right_leaf.next_leaf;
        if let Some(after_id) = right_leaf.next_leaf {
            let mut after = read_leaf_node(pager, after_id)?;
            after.prev_leaf = Some(left_id);
            pager.stage_raw_page(after_id, after.encode_page(after_id));
        }
        pager.stage_raw_page(left_id, left_leaf.encode_page(left_id));
        pager.free_page(right_id);
        parent.separators.remove(sep_idx);
        parent.children.remove(sep_idx + 1);
        rebalance_internal_upward(pager, key_id, path, parent_id, parent)?;
    } else {
        if child_idx > 0 {
            let borrowed = left_leaf.entries.pop().unwrap_or(LeafEntry {
                key: Vec::new(),
                seq: 0,
                value: LeafValue::Inline(Vec::new()),
            });
            right_leaf.entries.insert(0, borrowed);
        } else {
            let borrowed = right_leaf.entries.remove(0);
            left_leaf.entries.push(borrowed);
        }
        let first_right = right_leaf.entries.first().cloned().unwrap_or(LeafEntry {
            key: Vec::new(),
            seq: 0,
            value: LeafValue::Inline(Vec::new()),
        });
        let new_sep = SeparatorKey {
            key: first_right.key,
            seq: first_right.seq,
        };
        let _ = parent.separators.get_mut(sep_idx).map(|s| *s = new_sep);
        pager.stage_raw_page(left_id, left_leaf.encode_page(left_id));
        pager.stage_raw_page(right_id, right_leaf.encode_page(right_id));
        pager.stage_raw_page(parent_id, parent.encode_page(parent_id));
    }

    Ok(Some(removed_bytes))
}

/// Iteratively rebalances internal ancestors after a child merge removes a separator from `node`.
///
/// # Arguments
///
/// * `pager` - Active transactional [`Pager`].
/// * `key_id` - Target B+Tree selector.
/// * `path` - Remaining `(ancestor_id, child_idx)` stack above `node_id`.
/// * `node_id` - Identifier of `node`.
/// * `node` - Modified [`BTreeInternalNode`].
fn rebalance_internal_upward(
    pager: &mut Pager,
    key_id: KeyIndexId,
    mut path: Vec<(PageId, usize)>,
    mut node_id: PageId,
    mut node: BTreeInternalNode,
) -> Result<()> {
    loop {
        let Some((grand_id, child_idx)) = path.pop() else {
            if node.separators.is_empty() {
                let sole_child = node.children.first().copied().unwrap_or(PageId::HEADER);
                pager.free_page(node_id);
                pager.header_mut().set_tree_root(key_id, Some(sole_child));
            } else {
                pager.stage_raw_page(node_id, node.encode_page(node_id));
            }
            return Ok(());
        };

        if node.payload_bytes() >= INTERNAL_UNDERFLOW_BYTES {
            pager.stage_raw_page(node_id, node.encode_page(node_id));
            return Ok(());
        }

        let mut grand = read_internal_node(pager, grand_id)?;
        let (sep_idx, left_id, mut left_int, right_id, mut right_int) = if child_idx > 0 {
            let sib_id = grand
                .children
                .get(child_idx - 1)
                .copied()
                .unwrap_or(PageId::HEADER);
            let sib = read_internal_node(pager, sib_id)?;
            (child_idx - 1, sib_id, sib, node_id, node)
        } else {
            let sib_id = grand.children.get(1).copied().unwrap_or(PageId::HEADER);
            let sib = read_internal_node(pager, sib_id)?;
            (0, node_id, node, sib_id, sib)
        };

        let pivot = grand
            .separators
            .get(sep_idx)
            .cloned()
            .unwrap_or(SeparatorKey {
                key: Vec::new(),
                seq: 0,
            });
        let merged_bytes =
            left_int.payload_bytes() + pivot.encoded_byte_len() + right_int.payload_bytes();

        if merged_bytes <= INTERNAL_PAYLOAD_CAPACITY {
            left_int.separators.push(pivot);
            left_int.separators.append(&mut right_int.separators);
            left_int.children.append(&mut right_int.children);
            pager.stage_raw_page(left_id, left_int.encode_page(left_id));
            pager.free_page(right_id);
            grand.separators.remove(sep_idx);
            grand.children.remove(sep_idx + 1);
            node_id = grand_id;
            node = grand;
        } else {
            let up_sep = if child_idx > 0 {
                let promoted = left_int.separators.pop().unwrap_or(SeparatorKey {
                    key: Vec::new(),
                    seq: 0,
                });
                let moved_child = left_int.children.pop().unwrap_or(PageId::HEADER);
                right_int.separators.insert(0, pivot);
                right_int.children.insert(0, moved_child);
                promoted
            } else {
                let promoted = right_int.separators.remove(0);
                let moved_child = right_int.children.remove(0);
                left_int.separators.push(pivot);
                left_int.children.push(moved_child);
                promoted
            };
            let _ = grand.separators.get_mut(sep_idx).map(|s| *s = up_sep);
            pager.stage_raw_page(left_id, left_int.encode_page(left_id));
            pager.stage_raw_page(right_id, right_int.encode_page(right_id));
            pager.stage_raw_page(grand_id, grand.encode_page(grand_id));
            return Ok(());
        }
    }
}

/// Encodes `(alt_seqs, record_bytes)` into the Primary B+Tree value format.
///
/// # Arguments
///
/// * `alt_seqs` - Per-alternate-key FIFO sequence numbers (`0..=16` entries).
/// * `record` - Logical COBOL record byte slice.
fn encode_primary_value(alt_seqs: &[u64], record: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + alt_seqs.len() * 8 + record.len());
    let count = u8::try_from(alt_seqs.len()).unwrap_or(0);
    out.push(count);
    for &seq in alt_seqs {
        out.extend_from_slice(&seq.to_le_bytes());
    }
    out.extend_from_slice(record);
    out
}

/// Decodes a Primary B+Tree value buffer back into `(alt_seqs, record_bytes)`.
///
/// # Arguments
///
/// * `raw` - Materialized primary value bytes.
/// * `expected_alt_count` - Expected `schema.alternate_keys().len()`.
/// * `path` - File path label for corruption diagnostics.
fn decode_primary_value(
    raw: &[u8],
    expected_alt_count: usize,
    path: &str,
) -> Result<(Vec<u64>, Vec<u8>)> {
    let count = usize::from(*raw.first().ok_or_else(|| CoblangError::IsamStorage {
        path: String::from(path),
        page_id: 0,
        message: String::from("corrupted empty primary record value"),
    })?);
    if count != expected_alt_count {
        return Err(CoblangError::IsamStorage {
            path: String::from(path),
            page_id: 0,
            message: format!(
                "corrupted primary value alt_seq count {count} (expected {expected_alt_count})"
            ),
        });
    }
    let header_end = 1 + count * 8;
    let seq_slice = raw
        .get(1..header_end)
        .ok_or_else(|| CoblangError::IsamStorage {
            path: String::from(path),
            page_id: 0,
            message: String::from("truncated alt_seq header in primary record value"),
        })?;
    let mut alt_seqs = Vec::with_capacity(count);
    let (chunks, _) = seq_slice.as_chunks::<8>();
    for &chunk in chunks {
        alt_seqs.push(u64::from_le_bytes(chunk));
    }
    let record = raw.get(header_end..).unwrap_or(&[]).to_vec();
    Ok((alt_seqs, record))
}

/// Abstract factory opening or creating [`PageIo`] handles across `OPEN` / `CLOSE` cycles.
pub trait PageIoFactory {
    /// Returns `true` if the backing physical or virtual file currently exists.
    fn exists(&self) -> bool;

    /// Opens an existing backing store with `perm` access flags.
    ///
    /// # Arguments
    ///
    /// * `perm` - Read-only (`OPEN INPUT`) vs read-write (`OPEN I-O` / `EXTEND`) flag.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if opening the existing store fails.
    fn open_existing(&self, perm: DiskAccessPerm) -> Result<Box<dyn PageIo>>;

    /// Creates or truncates the backing store for `OPEN OUTPUT` (or `OPTIONAL` `I-O`/`EXTEND`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if creating or truncating the store fails.
    fn create_truncated(&self) -> Result<Box<dyn PageIo>>;
}

/// Filesystem [`PageIoFactory`] backed by an OS [`PathBuf`].
#[derive(Debug, Clone)]
pub struct DiskIoFactory {
    /// Target filesystem path.
    path: PathBuf,
}

impl DiskIoFactory {
    /// Constructs a [`DiskIoFactory`] for `path`.
    ///
    /// # Arguments
    ///
    /// * `path` - Target filesystem path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the underlying filesystem [`Path`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl PageIoFactory for DiskIoFactory {
    /// Checks whether `self.path` exists on disk.
    fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Opens `self.path` with the requested `perm` permissions.
    ///
    /// # Arguments
    ///
    /// * `perm` - Read-only vs read-write OS file descriptor flag.
    fn open_existing(&self, perm: DiskAccessPerm) -> Result<Box<dyn PageIo>> {
        Ok(Box::new(DiskPageIo::open_existing(&self.path, perm)?))
    }

    /// Creates or truncates `self.path` on disk.
    fn create_truncated(&self) -> Result<Box<dyn PageIo>> {
        Ok(Box::new(DiskPageIo::create_truncated(&self.path)?))
    }
}

/// In-memory [`PageIoFactory`] backed by a cloneable [`MemoryDisk`].
#[derive(Debug, Clone)]
pub struct MemoryIoFactory {
    /// Shared virtual file handle.
    disk: MemoryDisk,
}

impl MemoryIoFactory {
    /// Constructs a [`MemoryIoFactory`] wrapping `disk`.
    ///
    /// # Arguments
    ///
    /// * `disk` - Shared in-memory virtual file.
    #[must_use]
    pub const fn new(disk: MemoryDisk) -> Self {
        Self { disk }
    }
}

impl PageIoFactory for MemoryIoFactory {
    /// Checks whether `self.disk` has been created.
    fn exists(&self) -> bool {
        self.disk.exists()
    }

    /// Opens an existing [`MemoryPageIo`] over `self.disk`.
    ///
    /// # Arguments
    ///
    /// * `_perm` - Unused OS permission flag for in-memory handles.
    fn open_existing(&self, _perm: DiskAccessPerm) -> Result<Box<dyn PageIo>> {
        Ok(Box::new(self.disk.open_io()?))
    }

    /// Truncates or initializes `self.disk` to zero pages and returns a [`MemoryPageIo`].
    fn create_truncated(&self) -> Result<Box<dyn PageIo>> {
        self.disk.create_truncated();
        Ok(Box::new(MemoryPageIo::from_disk(self.disk.clone())))
    }
}

/// Logical state of the ISO COBOL File Position Indicator (FPI).
#[derive(Debug, Clone)]
enum FpiPosition {
    /// Positioned prior to the first logical record (`OPEN INPUT` / `OPEN I-O`).
    BeforeFirst,
    /// Positioned directly AT `(key, seq)` by a `START` statement.
    PositionedAt {
        /// Target collation-encoded key.
        key: Vec<u8>,
        /// Target FIFO sequence number.
        seq: u64,
        /// Physical leaf hint for $O(1)$ read.
        hint: LeafCursorHint,
    },
    /// Positioned immediately after reading `(key, seq)`.
    PositionedAfterRead {
        /// Last-read collation-encoded key.
        key: Vec<u8>,
        /// Last-read FIFO sequence number.
        seq: u64,
        /// Physical leaf hint for $O(1)$ next/previous step.
        hint: LeafCursorHint,
    },
    /// Reached `AT END` (`"10"`) or invalidated by an unsuccessful `START` / keyed `READ` (`"46"` on next seq read).
    ExhaustedOrInvalid,
}

/// Active paged session state inside an [`IsamFile`].
struct ActiveSession {
    /// Open mode (`Input`, `Output`, `Io`, `Extend`).
    mode: IsamOpenMode,
    /// Verified 4096-byte page manager.
    pager: Pager,
    /// Per-tree mutation generation counters (`0` = primary, `1..=16` = alternate).
    epochs: [u64; MAX_ALTERNATE_KEYS + 1],
    /// Current COBOL Key of Reference (`KeyIndexId::PRIMARY` initially on `OPEN`).
    key_of_ref: KeyIndexId,
    /// Current COBOL File Position Indicator state.
    fpi: FpiPosition,
    /// Primary sort key of the most recently `READ` record (for `SEQUENTIAL` `REWRITE`/`DELETE` `"43"`/`"21"`).
    last_read_primary_key: Option<Vec<u8>>,
    /// Highest primary sort key written so far in `SEQUENTIAL` `OUTPUT`/`EXTEND` (`"21"` check).
    last_seq_written_primary_key: Option<Vec<u8>>,
}

/// Open session variant inside [`IsamFile`], making missing-optional `INPUT` states strongly typed.
enum SessionState {
    /// `SELECT OPTIONAL` opened `INPUT` when the backing file did not exist (`"05"`).
    OptionalMissingInput {
        /// `true` once `AT END` (`"10"`) or failed `START`/`READ KEY` (`"23"`) has occurred.
        exhausted: bool,
    },
    /// Backed by an initialized or opened [`Pager`].
    Active(Box<ActiveSession>),
}

/// Planned per-alternate-key mutation computed during `REWRITE` validation.
struct AltRewritePlan {
    /// Target alternate B+Tree selector.
    alt_id: KeyIndexId,
    /// Uniqueness vs FIFO-duplicate policy.
    policy: DuplicateKeyPolicy,
    /// Previous collation-encoded alternate key.
    old_sort_key: Vec<u8>,
    /// Previous FIFO sequence number in `alt_id`.
    old_seq: u64,
    /// Replacement collation-encoded alternate key.
    new_sort_key: Vec<u8>,
    /// `true` when `new_sort_key != old_sort_key`.
    changed: bool,
}

/// Direction of a sequential `READ` operation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SeqReadDir {
    /// Forward `READ` / `READ NEXT`.
    Next,
    /// Backward `READ PREVIOUS`.
    Previous,
}

/// Stateful pure-Rust COBOL `ORGANIZATION IS INDEXED` (`ISAM`) file connector (`Milestone M15`).
pub struct IsamFile {
    /// Storage factory creating/reopening `PageIo` handles.
    factory: Box<dyn PageIoFactory>,
    /// Declared `FD` / `SELECT` schema metadata.
    schema: IsamSchema,
    /// Configured `ACCESS MODE` (`Sequential`, `Random`, or `Dynamic`).
    access_mode: IsamAccessMode,
    /// `SELECT` vs `SELECT OPTIONAL` behavior.
    optionality: FileOptionality,
    /// Set to `true` after `CLOSE WITH LOCK`, causing subsequent `OPEN` to return `"38"`.
    closed_with_lock: bool,
    /// Active open session (`None` when closed).
    session: Option<SessionState>,
}

impl IsamFile {
    /// Creates a closed [`IsamFile`] backed by an OS filesystem path.
    ///
    /// # Arguments
    ///
    /// * `path` - Filesystem path of the `COBLISAM` file.
    /// * `schema` - Validated `FD`/`SELECT` [`IsamSchema`].
    /// * `access_mode` - Declared [`IsamAccessMode`].
    /// * `optionality` - Declared [`FileOptionality`].
    #[must_use]
    pub fn new_disk(
        path: impl Into<PathBuf>,
        schema: IsamSchema,
        access_mode: IsamAccessMode,
        optionality: FileOptionality,
    ) -> Self {
        Self::new_with_factory(
            Box::new(DiskIoFactory::new(path)),
            schema,
            access_mode,
            optionality,
        )
    }

    /// Creates a closed [`IsamFile`] backed by a shared in-memory [`MemoryDisk`].
    ///
    /// # Arguments
    ///
    /// * `disk` - Shared virtual file handle.
    /// * `schema` - Validated `FD`/`SELECT` [`IsamSchema`].
    /// * `access_mode` - Declared [`IsamAccessMode`].
    /// * `optionality` - Declared [`FileOptionality`].
    #[must_use]
    pub fn new_memory(
        disk: MemoryDisk,
        schema: IsamSchema,
        access_mode: IsamAccessMode,
        optionality: FileOptionality,
    ) -> Self {
        Self::new_with_factory(
            Box::new(MemoryIoFactory::new(disk)),
            schema,
            access_mode,
            optionality,
        )
    }

    /// Creates a closed [`IsamFile`] with a custom [`PageIoFactory`].
    ///
    /// # Arguments
    ///
    /// * `factory` - Custom storage factory implementation.
    /// * `schema` - Validated `FD`/`SELECT` [`IsamSchema`].
    /// * `access_mode` - Declared [`IsamAccessMode`].
    /// * `optionality` - Declared [`FileOptionality`].
    #[must_use]
    pub fn new_with_factory(
        factory: Box<dyn PageIoFactory>,
        schema: IsamSchema,
        access_mode: IsamAccessMode,
        optionality: FileOptionality,
    ) -> Self {
        Self {
            factory,
            schema,
            access_mode,
            optionality,
            closed_with_lock: false,
            session: None,
        }
    }

    /// Returns the declared [`IsamSchema`] of this file connector.
    #[must_use]
    pub const fn schema(&self) -> &IsamSchema {
        &self.schema
    }

    /// Returns the declared [`IsamAccessMode`] of this file connector.
    #[must_use]
    pub const fn access_mode(&self) -> IsamAccessMode {
        self.access_mode
    }

    /// Returns `true` if the connector is currently open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// Returns the live record count when open (`Some(0)` for optional-empty `INPUT`, `None` when closed).
    #[must_use]
    pub fn record_count(&self) -> Option<u64> {
        match self.session.as_ref()? {
            SessionState::OptionalMissingInput { .. } => Some(0),
            SessionState::Active(sess) => Some(sess.pager.header().record_count),
        }
    }

    /// Executes COBOL `OPEN <mode>` on this indexed file connector.
    ///
    /// # Arguments
    ///
    /// * `mode` - Requested [`IsamOpenMode`] (`Input`, `Output`, `Io`, or `Extend`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] on underlying storage I/O or page-checksum corruption.
    pub fn open(&mut self, mode: IsamOpenMode) -> Result<FileStatusKind> {
        if self.closed_with_lock {
            return Ok(FileStatusKind::ClosedWithLock38);
        }
        if self.session.is_some() {
            return Ok(FileStatusKind::AlreadyOpen41);
        }
        if mode == IsamOpenMode::Extend && self.access_mode != IsamAccessMode::Sequential {
            return Ok(FileStatusKind::OpenPermissionDenied37);
        }

        match mode {
            IsamOpenMode::Output => {
                let io = self.factory.create_truncated()?;
                let pager = Pager::create_new(io, self.schema.clone())?;
                self.session = Some(SessionState::Active(Box::new(ActiveSession {
                    mode,
                    pager,
                    epochs: [0; MAX_ALTERNATE_KEYS + 1],
                    key_of_ref: KeyIndexId::PRIMARY,
                    fpi: FpiPosition::ExhaustedOrInvalid,
                    last_read_primary_key: None,
                    last_seq_written_primary_key: None,
                })));
                Ok(FileStatusKind::Success00)
            }
            IsamOpenMode::Input => {
                if !self.factory.exists() {
                    if self.optionality == FileOptionality::Optional {
                        self.session =
                            Some(SessionState::OptionalMissingInput { exhausted: false });
                        return Ok(FileStatusKind::SuccessOptionalFile05);
                    }
                    return Ok(FileStatusKind::FileNotFound35);
                }
                let io = self.factory.open_existing(DiskAccessPerm::ReadOnly)?;
                let pager = Pager::open_existing(io)?;
                if !self.schema.is_compatible_with(&pager.header().schema) {
                    return Ok(FileStatusKind::AttributeConflict39);
                }
                self.session = Some(SessionState::Active(Box::new(ActiveSession {
                    mode,
                    pager,
                    epochs: [0; MAX_ALTERNATE_KEYS + 1],
                    key_of_ref: KeyIndexId::PRIMARY,
                    fpi: FpiPosition::BeforeFirst,
                    last_read_primary_key: None,
                    last_seq_written_primary_key: None,
                })));
                Ok(FileStatusKind::Success00)
            }
            IsamOpenMode::Io | IsamOpenMode::Extend => {
                let (mut pager, status) = if self.factory.exists() {
                    let io = self.factory.open_existing(DiskAccessPerm::ReadWrite)?;
                    let opened = Pager::open_existing(io)?;
                    if !self.schema.is_compatible_with(&opened.header().schema) {
                        return Ok(FileStatusKind::AttributeConflict39);
                    }
                    (opened, FileStatusKind::Success00)
                } else if self.optionality == FileOptionality::Optional {
                    let io = self.factory.create_truncated()?;
                    let created = Pager::create_new(io, self.schema.clone())?;
                    (created, FileStatusKind::SuccessOptionalFile05)
                } else {
                    return Ok(FileStatusKind::FileNotFound35);
                };

                let last_seq_written_primary_key = if mode == IsamOpenMode::Extend {
                    let max_bound = vec![0xFF_u8; MAX_KEY_BYTES];
                    seek_upper_bound(
                        &mut pager,
                        KeyIndexId::PRIMARY,
                        &max_bound,
                        u64::MAX,
                        BoundInclusion::Inclusive,
                    )?
                    .map(|loc| loc.entry.key)
                } else {
                    None
                };

                self.session = Some(SessionState::Active(Box::new(ActiveSession {
                    mode,
                    pager,
                    epochs: [0; MAX_ALTERNATE_KEYS + 1],
                    key_of_ref: KeyIndexId::PRIMARY,
                    fpi: if mode == IsamOpenMode::Io {
                        FpiPosition::BeforeFirst
                    } else {
                        FpiPosition::ExhaustedOrInvalid
                    },
                    last_read_primary_key: None,
                    last_seq_written_primary_key,
                })));
                Ok(status)
            }
        }
    }

    /// Executes COBOL `CLOSE` (`Normal` or `WithLock`) on this connector.
    ///
    /// # Arguments
    ///
    /// * `disposition` - [`CloseDisposition::Normal`] or [`CloseDisposition::WithLock`].
    pub fn close(&mut self, disposition: CloseDisposition) -> FileStatusKind {
        if self.session.take().is_none() {
            return FileStatusKind::NotOpenOnClose42;
        }
        if disposition == CloseDisposition::WithLock {
            self.closed_with_lock = true;
        }
        FileStatusKind::Success00
    }

    /// Executes generic/partial-key or full-key COBOL `START` (`=`, `>`, `>=`, `<`, `<=`).
    ///
    /// # Arguments
    ///
    /// * `key_id` - Primary (`KeyIndexId::PRIMARY`) or alternate (`KeyIndexId::alternate(i)`) key.
    /// * `relation` - Relational operator (`Equal`, `Greater`, `GreaterOrEqual`, `Less`, `LessOrEqual`).
    /// * `partial_raw_key` - Raw key or leading prefix bytes (`1..=key_spec.total_byte_len()`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `key_id` or `partial_raw_key` length is invalid,
    /// or [`CoblangError::IsamStorage`] / [`CoblangError::Io`] on storage failure.
    pub fn start(
        &mut self,
        key_id: KeyIndexId,
        relation: StartRelation,
        partial_raw_key: &[u8],
    ) -> Result<FileStatusKind> {
        let key_spec = self.schema.key_spec(key_id)?;
        let full_len = key_spec.total_byte_len();
        if partial_raw_key.is_empty() || partial_raw_key.len() > full_len {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "StartKeyPrefix",
                value: format!("len={}", partial_raw_key.len()),
                reason: format!("START key prefix length must be in 1..={full_len}"),
            });
        }
        let encoded_prefix = self.schema.collating().encode_key(partial_raw_key);

        if self.access_mode == IsamAccessMode::Random {
            return Ok(FileStatusKind::ReadWrongOpenMode47);
        }
        let sess = match self.session.as_mut() {
            None => return Ok(FileStatusKind::ReadWrongOpenMode47),
            Some(SessionState::OptionalMissingInput { exhausted }) => {
                *exhausted = true;
                return Ok(FileStatusKind::RecordNotFound23);
            }
            Some(SessionState::Active(active)) => active.as_mut(),
        };
        if !matches!(sess.mode, IsamOpenMode::Input | IsamOpenMode::Io) {
            return Ok(FileStatusKind::ReadWrongOpenMode47);
        }
        sess.last_read_primary_key = None;

        let mut padded_max = encoded_prefix.clone();
        padded_max.resize(full_len, 0xFF);

        let candidate = match relation {
            StartRelation::Equal => seek_lower_bound(
                &mut sess.pager,
                key_id,
                &encoded_prefix,
                0,
                BoundInclusion::Inclusive,
            )?
            .filter(|loc| loc.entry.key.starts_with(&encoded_prefix)),
            StartRelation::GreaterOrEqual => seek_lower_bound(
                &mut sess.pager,
                key_id,
                &encoded_prefix,
                0,
                BoundInclusion::Inclusive,
            )?,
            StartRelation::Greater => seek_lower_bound(
                &mut sess.pager,
                key_id,
                &padded_max,
                u64::MAX,
                BoundInclusion::Exclusive,
            )?,
            StartRelation::Less => seek_upper_bound(
                &mut sess.pager,
                key_id,
                &encoded_prefix,
                0,
                BoundInclusion::Exclusive,
            )?,
            StartRelation::LessOrEqual => seek_upper_bound(
                &mut sess.pager,
                key_id,
                &padded_max,
                u64::MAX,
                BoundInclusion::Inclusive,
            )?,
        };

        let Some(loc) = candidate else {
            sess.fpi = FpiPosition::ExhaustedOrInvalid;
            return Ok(FileStatusKind::RecordNotFound23);
        };

        let epoch = sess
            .epochs
            .get(usize::from(key_id.as_u16()))
            .copied()
            .unwrap_or(0);
        sess.key_of_ref = key_id;
        sess.fpi = FpiPosition::PositionedAt {
            key: loc.entry.key,
            seq: loc.entry.seq,
            hint: LeafCursorHint {
                epoch,
                leaf_id: loc.leaf_id,
                slot: loc.slot,
            },
        };
        Ok(FileStatusKind::Success00)
    }

    /// Executes sequential forward `READ` / `READ NEXT` along the active Key of Reference.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] on underlying storage I/O or page-checksum corruption.
    pub fn read_next(&mut self) -> Result<(FileStatusKind, Option<Vec<u8>>)> {
        self.read_sequential_impl(SeqReadDir::Next)
    }

    /// Executes sequential backward `READ PREVIOUS` along the active Key of Reference.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] on underlying storage I/O or page-checksum corruption.
    pub fn read_previous(&mut self) -> Result<(FileStatusKind, Option<Vec<u8>>)> {
        self.read_sequential_impl(SeqReadDir::Previous)
    }

    /// Internal shared sequential `READ NEXT` / `READ PREVIOUS` implementation.
    ///
    /// # Arguments
    ///
    /// * `dir` - Forward (`Next`) vs backward (`Previous`) traversal direction.
    fn read_sequential_impl(
        &mut self,
        dir: SeqReadDir,
    ) -> Result<(FileStatusKind, Option<Vec<u8>>)> {
        if self.access_mode == IsamAccessMode::Random {
            return Ok((FileStatusKind::ReadWrongOpenMode47, None));
        }
        let sess = match self.session.as_mut() {
            None => return Ok((FileStatusKind::ReadWrongOpenMode47, None)),
            Some(SessionState::OptionalMissingInput { exhausted }) => {
                if *exhausted {
                    return Ok((FileStatusKind::ReadBeyondEnd46, None));
                }
                *exhausted = true;
                return Ok((FileStatusKind::AtEnd10, None));
            }
            Some(SessionState::Active(active)) => active.as_mut(),
        };
        if !matches!(sess.mode, IsamOpenMode::Input | IsamOpenMode::Io) {
            return Ok((FileStatusKind::ReadWrongOpenMode47, None));
        }
        sess.last_read_primary_key = None;

        let key_id = sess.key_of_ref;
        let cur_epoch = sess
            .epochs
            .get(usize::from(key_id.as_u16()))
            .copied()
            .unwrap_or(0);

        let loc = match locate_sequential_target(sess, key_id, cur_epoch, dir)? {
            SeqLocateOutcome::Exhausted46 => {
                return Ok((FileStatusKind::ReadBeyondEnd46, None));
            }
            SeqLocateOutcome::AtEnd10 => {
                sess.fpi = FpiPosition::ExhaustedOrInvalid;
                return Ok((FileStatusKind::AtEnd10, None));
            }
            SeqLocateOutcome::Found(entry) => entry,
        };

        let (primary_sort_key, record_bytes) =
            resolve_located_record(&mut sess.pager, &self.schema, key_id, &loc.entry)?;
        let key_spec = self.schema.key_spec(key_id)?;
        let has_more_dups = if key_spec.duplicates() == DuplicateKeyPolicy::AllowedFifo {
            let neighbor = match dir {
                SeqReadDir::Next => step_leaf_next(&mut sess.pager, loc.leaf_id, loc.slot)?,
                SeqReadDir::Previous => step_leaf_prev(&mut sess.pager, loc.leaf_id, loc.slot)?,
            };
            neighbor.is_some_and(|n| n.entry.key == loc.entry.key)
        } else {
            false
        };

        sess.fpi = FpiPosition::PositionedAfterRead {
            key: loc.entry.key,
            seq: loc.entry.seq,
            hint: LeafCursorHint {
                epoch: cur_epoch,
                leaf_id: loc.leaf_id,
                slot: loc.slot,
            },
        };
        sess.last_read_primary_key = Some(primary_sort_key);

        Ok((
            if has_more_dups {
                FileStatusKind::SuccessDuplicateKey02
            } else {
                FileStatusKind::Success00
            },
            Some(record_bytes),
        ))
    }

    /// Executes keyed random/dynamic `READ file [KEY IS key_id]` by exact full key.
    ///
    /// # Arguments
    ///
    /// * `key_id` - Primary or alternate key index selector.
    /// * `raw_key` - Exact full-width raw key bytes (`raw_key.len() == key_spec.total_byte_len()`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `key_id` or `raw_key.len()` is invalid, or
    /// [`CoblangError`] on underlying storage failure.
    pub fn read_by_key(
        &mut self,
        key_id: KeyIndexId,
        raw_key: &[u8],
    ) -> Result<(FileStatusKind, Option<Vec<u8>>)> {
        let key_spec = self.schema.key_spec(key_id)?;
        if raw_key.len() != key_spec.total_byte_len() {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "ReadKeyBytes",
                value: format!("len={}", raw_key.len()),
                reason: format!(
                    "keyed READ requires exact key length {}",
                    key_spec.total_byte_len()
                ),
            });
        }
        let sort_key = self.schema.collating().encode_key(raw_key);

        if self.access_mode == IsamAccessMode::Sequential {
            return Ok((FileStatusKind::ReadWrongOpenMode47, None));
        }
        let sess = match self.session.as_mut() {
            None => return Ok((FileStatusKind::ReadWrongOpenMode47, None)),
            Some(SessionState::OptionalMissingInput { exhausted }) => {
                *exhausted = true;
                return Ok((FileStatusKind::RecordNotFound23, None));
            }
            Some(SessionState::Active(active)) => active.as_mut(),
        };
        if !matches!(sess.mode, IsamOpenMode::Input | IsamOpenMode::Io) {
            return Ok((FileStatusKind::ReadWrongOpenMode47, None));
        }
        sess.last_read_primary_key = None;
        sess.key_of_ref = key_id;

        let found = seek_lower_bound(
            &mut sess.pager,
            key_id,
            &sort_key,
            0,
            BoundInclusion::Inclusive,
        )?
        .filter(|loc| loc.entry.key == sort_key);

        let Some(loc) = found else {
            sess.fpi = FpiPosition::ExhaustedOrInvalid;
            return Ok((FileStatusKind::RecordNotFound23, None));
        };

        let (primary_sort_key, record_bytes) =
            resolve_located_record(&mut sess.pager, &self.schema, key_id, &loc.entry)?;
        let has_more_dups = if key_spec.duplicates() == DuplicateKeyPolicy::AllowedFifo {
            step_leaf_next(&mut sess.pager, loc.leaf_id, loc.slot)?
                .is_some_and(|n| n.entry.key == loc.entry.key)
        } else {
            false
        };

        let cur_epoch = sess
            .epochs
            .get(usize::from(key_id.as_u16()))
            .copied()
            .unwrap_or(0);
        sess.fpi = FpiPosition::PositionedAfterRead {
            key: loc.entry.key,
            seq: loc.entry.seq,
            hint: LeafCursorHint {
                epoch: cur_epoch,
                leaf_id: loc.leaf_id,
                slot: loc.slot,
            },
        };
        sess.last_read_primary_key = Some(primary_sort_key);

        Ok((
            if has_more_dups {
                FileStatusKind::SuccessDuplicateKey02
            } else {
                FileStatusKind::Success00
            },
            Some(record_bytes),
        ))
    }

    /// Executes COBOL `WRITE` inserting `record` across the primary and all alternate B+Trees atomically.
    ///
    /// # Arguments
    ///
    /// * `record` - Logical record bytes (`min_bytes..=max_bytes`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] on underlying storage I/O or page-checksum corruption.
    pub fn write(&mut self, record: &[u8]) -> Result<FileStatusKind> {
        let Some(SessionState::Active(sess)) = self.session.as_mut() else {
            return Ok(FileStatusKind::WriteWrongOpenMode48);
        };
        let sess = sess.as_mut();
        let valid_mode = match self.access_mode {
            IsamAccessMode::Sequential => {
                matches!(sess.mode, IsamOpenMode::Output | IsamOpenMode::Extend)
            }
            IsamAccessMode::Random | IsamAccessMode::Dynamic => {
                matches!(sess.mode, IsamOpenMode::Output | IsamOpenMode::Io)
            }
        };
        if !valid_mode {
            return Ok(FileStatusKind::WriteWrongOpenMode48);
        }
        if !self.schema.record_bounds().contains_len(record.len()) {
            return Ok(FileStatusKind::RewriteLengthMismatch44);
        }

        let primary_raw = self
            .schema
            .primary_key()
            .extract_within_validated_record(record);
        let primary_sort_key = self.schema.collating().encode_key(&primary_raw);

        if self.access_mode == IsamAccessMode::Sequential
            && let Some(prev) = &sess.last_seq_written_primary_key
            && primary_sort_key <= *prev
        {
            return Ok(FileStatusKind::InvalidKeySequence21);
        }

        if seek_lower_bound(
            &mut sess.pager,
            KeyIndexId::PRIMARY,
            &primary_sort_key,
            0,
            BoundInclusion::Inclusive,
        )?
        .is_some_and(|loc| loc.entry.key == primary_sort_key)
        {
            return Ok(FileStatusKind::DuplicateKey22);
        }

        let alt_count = self.schema.alternate_keys().len();
        let mut encoded_alt_keys = Vec::with_capacity(alt_count);
        let mut had_allowed_duplicate = false;

        for (ord, alt_spec) in self.schema.alternate_keys().iter().enumerate() {
            let alt_id = KeyIndexId::from_validated_ordinal(u16::try_from(ord).unwrap_or(0));
            let raw_alt = alt_spec.extract_within_validated_record(record);
            let sort_alt = self.schema.collating().encode_key(&raw_alt);
            if seek_lower_bound(
                &mut sess.pager,
                alt_id,
                &sort_alt,
                0,
                BoundInclusion::Inclusive,
            )?
            .is_some_and(|loc| loc.entry.key == sort_alt)
            {
                match alt_spec.duplicates() {
                    DuplicateKeyPolicy::Unique => return Ok(FileStatusKind::DuplicateKey22),
                    DuplicateKeyPolicy::AllowedFifo => had_allowed_duplicate = true,
                }
            }
            encoded_alt_keys.push((alt_id, alt_spec.duplicates(), sort_alt));
        }

        commit_write_trees(sess, &primary_sort_key, &encoded_alt_keys, record)?;
        if self.access_mode == IsamAccessMode::Sequential {
            sess.last_seq_written_primary_key = Some(primary_sort_key);
        }
        Ok(if had_allowed_duplicate {
            FileStatusKind::SuccessDuplicateKey02
        } else {
            FileStatusKind::Success00
        })
    }

    /// Executes COBOL `REWRITE` updating primary payload and all changed alternate B+Trees atomically.
    ///
    /// # Arguments
    ///
    /// * `record` - Replacement record bytes (`min_bytes..=max_bytes`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] on underlying storage I/O or page-checksum corruption.
    pub fn rewrite(&mut self, record: &[u8]) -> Result<FileStatusKind> {
        let Some(SessionState::Active(sess)) = self.session.as_mut() else {
            return Ok(FileStatusKind::RewriteDeleteWrongOpenMode49);
        };
        let sess = sess.as_mut();
        if sess.mode != IsamOpenMode::Io {
            return Ok(FileStatusKind::RewriteDeleteWrongOpenMode49);
        }
        let prior_read_pk = sess.last_read_primary_key.take();
        if self.access_mode == IsamAccessMode::Sequential && prior_read_pk.is_none() {
            return Ok(FileStatusKind::NoPriorRead43);
        }
        if !self.schema.record_bounds().contains_len(record.len()) {
            return Ok(FileStatusKind::RewriteLengthMismatch44);
        }

        let primary_raw = self
            .schema
            .primary_key()
            .extract_within_validated_record(record);
        let primary_sort_key = self.schema.collating().encode_key(&primary_raw);

        if self.access_mode == IsamAccessMode::Sequential
            && prior_read_pk.as_ref() != Some(&primary_sort_key)
        {
            return Ok(FileStatusKind::InvalidKeySequence21);
        }

        let Some(old_loc) = seek_lower_bound(
            &mut sess.pager,
            KeyIndexId::PRIMARY,
            &primary_sort_key,
            0,
            BoundInclusion::Inclusive,
        )?
        .filter(|loc| loc.entry.key == primary_sort_key) else {
            return Ok(FileStatusKind::RecordNotFound23);
        };

        let old_primary_raw = resolve_leaf_value(&mut sess.pager, &old_loc.entry.value)?;
        let alt_count = self.schema.alternate_keys().len();
        let path_label = String::from(sess.pager.path_label());
        let (old_alt_seqs, old_record) =
            decode_primary_value(&old_primary_raw, alt_count, &path_label)?;

        let (plans, had_allowed_duplicate) = match prepare_rewrite_plans(
            &mut sess.pager,
            &self.schema,
            &old_record,
            &old_alt_seqs,
            record,
        )? {
            Ok(pair) => pair,
            Err(dup_status) => return Ok(dup_status),
        };

        commit_rewrite_trees(sess, &primary_sort_key, &plans, record, &path_label)?;
        Ok(if had_allowed_duplicate {
            FileStatusKind::SuccessDuplicateKey02
        } else {
            FileStatusKind::Success00
        })
    }

    /// Executes COBOL `DELETE` in `ACCESS MODE IS SEQUENTIAL` (deleting the last-read record).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] on underlying storage I/O or page-checksum corruption.
    pub fn delete_sequential(&mut self) -> Result<FileStatusKind> {
        let Some(SessionState::Active(sess)) = self.session.as_mut() else {
            return Ok(FileStatusKind::RewriteDeleteWrongOpenMode49);
        };
        if sess.mode != IsamOpenMode::Io || self.access_mode != IsamAccessMode::Sequential {
            return Ok(FileStatusKind::RewriteDeleteWrongOpenMode49);
        }
        let Some(target_pk) = sess.last_read_primary_key.take() else {
            return Ok(FileStatusKind::NoPriorRead43);
        };
        delete_by_encoded_primary_key(sess.as_mut(), &self.schema, &target_pk)
    }

    /// Executes COBOL `DELETE` in `RANDOM` or `DYNAMIC` mode by primary key.
    ///
    /// # Arguments
    ///
    /// * `raw_primary_key` - Exact full-width raw primary key bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw_primary_key.len()` mismatches the
    /// primary key length, or [`CoblangError`] on storage failure.
    pub fn delete_by_primary_key(&mut self, raw_primary_key: &[u8]) -> Result<FileStatusKind> {
        let pk_len = self.schema.primary_key().total_byte_len();
        if raw_primary_key.len() != pk_len {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "DeletePrimaryKey",
                value: format!("len={}", raw_primary_key.len()),
                reason: format!("DELETE requires exact primary key length {pk_len}"),
            });
        }
        let target_pk = self.schema.collating().encode_key(raw_primary_key);

        let Some(SessionState::Active(sess)) = self.session.as_mut() else {
            return Ok(FileStatusKind::RewriteDeleteWrongOpenMode49);
        };
        if sess.mode != IsamOpenMode::Io || self.access_mode == IsamAccessMode::Sequential {
            return Ok(FileStatusKind::RewriteDeleteWrongOpenMode49);
        }
        sess.last_read_primary_key = None;
        delete_by_encoded_primary_key(sess.as_mut(), &self.schema, &target_pk)
    }
}

/// Outcome of resolving `sess.fpi` during a sequential `READ NEXT` / `READ PREVIOUS`.
enum SeqLocateOutcome {
    /// FPI was already at `AT END` or invalidated (`"46"`).
    Exhausted46,
    /// Traversal reached end/beginning of index (`"10"`).
    AtEnd10,
    /// Located target entry (`"00"` or `"02"`).
    Found(LocatedLeafEntry),
}

/// Resolves the next or previous [`LocatedLeafEntry`] from `sess.fpi` (using $O(1)$ hints when valid).
///
/// # Arguments
///
/// * `sess` - Active open session.
/// * `key_id` - Active Key of Reference.
/// * `cur_epoch` - Current mutation epoch of `key_id`.
/// * `dir` - Forward (`Next`) vs backward (`Previous`) read direction.
fn locate_sequential_target(
    sess: &mut ActiveSession,
    key_id: KeyIndexId,
    cur_epoch: u64,
    dir: SeqReadDir,
) -> Result<SeqLocateOutcome> {
    let opt = match sess.fpi.clone() {
        FpiPosition::ExhaustedOrInvalid => return Ok(SeqLocateOutcome::Exhausted46),
        FpiPosition::BeforeFirst => match dir {
            SeqReadDir::Next => {
                seek_lower_bound(&mut sess.pager, key_id, &[], 0, BoundInclusion::Inclusive)?
            }
            SeqReadDir::Previous => None,
        },
        FpiPosition::PositionedAt { key, seq, hint } => {
            if hint.epoch == cur_epoch {
                let leaf = read_leaf_node(&mut sess.pager, hint.leaf_id)?;
                let entry = leaf
                    .entries
                    .get(hint.slot.as_usize())
                    .cloned()
                    .unwrap_or(LeafEntry {
                        key: Vec::new(),
                        seq: 0,
                        value: LeafValue::Inline(Vec::new()),
                    });
                Some(LocatedLeafEntry {
                    leaf_id: hint.leaf_id,
                    slot: hint.slot,
                    entry,
                })
            } else {
                match dir {
                    SeqReadDir::Next => seek_lower_bound(
                        &mut sess.pager,
                        key_id,
                        &key,
                        seq,
                        BoundInclusion::Inclusive,
                    )?,
                    SeqReadDir::Previous => seek_upper_bound(
                        &mut sess.pager,
                        key_id,
                        &key,
                        seq,
                        BoundInclusion::Inclusive,
                    )?,
                }
            }
        }
        FpiPosition::PositionedAfterRead { key, seq, hint } => {
            if hint.epoch == cur_epoch {
                match dir {
                    SeqReadDir::Next => step_leaf_next(&mut sess.pager, hint.leaf_id, hint.slot)?,
                    SeqReadDir::Previous => {
                        step_leaf_prev(&mut sess.pager, hint.leaf_id, hint.slot)?
                    }
                }
            } else {
                match dir {
                    SeqReadDir::Next => seek_lower_bound(
                        &mut sess.pager,
                        key_id,
                        &key,
                        seq,
                        BoundInclusion::Exclusive,
                    )?,
                    SeqReadDir::Previous => seek_upper_bound(
                        &mut sess.pager,
                        key_id,
                        &key,
                        seq,
                        BoundInclusion::Exclusive,
                    )?,
                }
            }
        }
    };
    Ok(match opt {
        Some(loc) => SeqLocateOutcome::Found(loc),
        None => SeqLocateOutcome::AtEnd10,
    })
}

/// Atomically inserts a new record into all alternate B+Trees and the primary B+Tree.
///
/// # Arguments
///
/// * `sess` - Active writable session.
/// * `primary_sort_key` - Collation-encoded primary key bytes.
/// * `encoded_alt_keys` - Pre-validated `(alt_id, policy, sort_alt)` tuples.
/// * `record` - Logical record bytes.
fn commit_write_trees(
    sess: &mut ActiveSession,
    primary_sort_key: &[u8],
    encoded_alt_keys: &[(KeyIndexId, DuplicateKeyPolicy, Vec<u8>)],
    record: &[u8],
) -> Result<()> {
    let saved_epochs = sess.epochs;
    sess.pager.begin_tx();
    let tx_res = (|| -> Result<()> {
        let path_label = String::from(sess.pager.path_label());
        let fifo_seq = sess.pager.header_mut().alloc_dup_seq(&path_label)?;
        let mut alt_seqs = Vec::with_capacity(encoded_alt_keys.len());
        for &(alt_id, policy, ref sort_alt) in encoded_alt_keys {
            let seq = match policy {
                DuplicateKeyPolicy::Unique => 0,
                DuplicateKeyPolicy::AllowedFifo => fifo_seq,
            };
            alt_seqs.push(seq);
            let ep_idx = usize::from(alt_id.as_u16());
            let mut alt_ep = sess.epochs.get(ep_idx).copied().unwrap_or(0);
            btree_upsert(
                &mut sess.pager,
                alt_id,
                sort_alt.clone(),
                seq,
                primary_sort_key.to_vec(),
                &mut alt_ep,
            )?;
            let _ = sess.epochs.get_mut(ep_idx).map(|slot| *slot = alt_ep);
        }
        let primary_val = encode_primary_value(&alt_seqs, record);
        let mut pri_ep = sess.epochs[0];
        btree_upsert(
            &mut sess.pager,
            KeyIndexId::PRIMARY,
            primary_sort_key.to_vec(),
            0,
            primary_val,
            &mut pri_ep,
        )?;
        sess.epochs[0] = pri_ep;
        sess.pager.header_mut().record_count = sess.pager.header().record_count.saturating_add(1);
        Ok(())
    })();

    if let Err(err) = tx_res {
        sess.pager.rollback_tx();
        sess.epochs = saved_epochs;
        return Err(err);
    }
    if let Err(err) = sess.pager.commit_tx() {
        sess.epochs = saved_epochs;
        return Err(err);
    }
    Ok(())
}

/// Counts up to `2` entries matching `sort_key` inside `alt_id` (`0`, `1`, or `2`).
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `alt_id` - Target alternate key selector.
/// * `sort_key` - Collation-encoded alternate key bytes.
fn count_key_up_to_two(pager: &mut Pager, alt_id: KeyIndexId, sort_key: &[u8]) -> Result<u8> {
    let Some(first) = seek_lower_bound(pager, alt_id, sort_key, 0, BoundInclusion::Inclusive)?
    else {
        return Ok(0);
    };
    if first.entry.key != sort_key {
        return Ok(0);
    }
    let has_second = step_leaf_next(pager, first.leaf_id, first.slot)?
        .is_some_and(|second| second.entry.key == sort_key);
    Ok(if has_second { 2 } else { 1 })
}

/// Validates all alternate keys for `REWRITE` prior to mutating any B+Tree.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `schema` - File [`IsamSchema`].
/// * `old_record` - Existing record bytes.
/// * `old_alt_seqs` - Existing per-alternate-key FIFO sequence numbers.
/// * `new_record` - Replacement record bytes.
fn prepare_rewrite_plans(
    pager: &mut Pager,
    schema: &IsamSchema,
    old_record: &[u8],
    old_alt_seqs: &[u64],
    new_record: &[u8],
) -> Result<core::result::Result<(Vec<AltRewritePlan>, bool), FileStatusKind>> {
    let mut plans = Vec::with_capacity(schema.alternate_keys().len());
    let mut had_allowed_duplicate = false;

    for (ord, alt_spec) in schema.alternate_keys().iter().enumerate() {
        let alt_id = KeyIndexId::from_validated_ordinal(u16::try_from(ord).unwrap_or(0));
        let old_raw = alt_spec.extract_within_validated_record(old_record);
        let old_sort_key = schema.collating().encode_key(&old_raw);
        let old_seq = old_alt_seqs.get(ord).copied().unwrap_or(0);

        let new_raw = alt_spec.extract_within_validated_record(new_record);
        let new_sort_key = schema.collating().encode_key(&new_raw);
        let changed = new_sort_key != old_sort_key;
        let existing_count = count_key_up_to_two(pager, alt_id, &new_sort_key)?;

        if changed {
            if existing_count >= 1 {
                match alt_spec.duplicates() {
                    DuplicateKeyPolicy::Unique => return Ok(Err(FileStatusKind::DuplicateKey22)),
                    DuplicateKeyPolicy::AllowedFifo => had_allowed_duplicate = true,
                }
            }
        } else if alt_spec.duplicates() == DuplicateKeyPolicy::AllowedFifo && existing_count >= 2 {
            had_allowed_duplicate = true;
        }

        plans.push(AltRewritePlan {
            alt_id,
            policy: alt_spec.duplicates(),
            old_sort_key,
            old_seq,
            new_sort_key,
            changed,
        });
    }
    Ok(Ok((plans, had_allowed_duplicate)))
}

/// Atomically applies validated `REWRITE` updates across changed alternate B+Trees and the primary B+Tree.
///
/// # Arguments
///
/// * `sess` - Active `I-O` session.
/// * `primary_sort_key` - Unchanged collation-encoded primary key.
/// * `plans` - Validated per-alternate-key mutation plans.
/// * `record` - Replacement record bytes.
/// * `path_label` - Diagnostic file path label.
fn commit_rewrite_trees(
    sess: &mut ActiveSession,
    primary_sort_key: &[u8],
    plans: &[AltRewritePlan],
    record: &[u8],
    path_label: &str,
) -> Result<()> {
    let saved_epochs = sess.epochs;
    sess.pager.begin_tx();
    let tx_res = (|| -> Result<()> {
        let mut new_alt_seqs = Vec::with_capacity(plans.len());
        for plan in plans {
            if !plan.changed {
                new_alt_seqs.push(plan.old_seq);
                continue;
            }
            let ep_idx = usize::from(plan.alt_id.as_u16());
            let mut alt_ep = sess.epochs.get(ep_idx).copied().unwrap_or(0);
            btree_remove(
                &mut sess.pager,
                plan.alt_id,
                &plan.old_sort_key,
                plan.old_seq,
                &mut alt_ep,
            )?;
            let new_seq = match plan.policy {
                DuplicateKeyPolicy::Unique => 0,
                DuplicateKeyPolicy::AllowedFifo => {
                    sess.pager.header_mut().alloc_dup_seq(path_label)?
                }
            };
            btree_upsert(
                &mut sess.pager,
                plan.alt_id,
                plan.new_sort_key.clone(),
                new_seq,
                primary_sort_key.to_vec(),
                &mut alt_ep,
            )?;
            let _ = sess.epochs.get_mut(ep_idx).map(|slot| *slot = alt_ep);
            new_alt_seqs.push(new_seq);
        }

        let primary_val = encode_primary_value(&new_alt_seqs, record);
        let mut pri_ep = sess.epochs[0];
        btree_upsert(
            &mut sess.pager,
            KeyIndexId::PRIMARY,
            primary_sort_key.to_vec(),
            0,
            primary_val,
            &mut pri_ep,
        )?;
        sess.epochs[0] = pri_ep;
        Ok(())
    })();

    if let Err(err) = tx_res {
        sess.pager.rollback_tx();
        sess.epochs = saved_epochs;
        return Err(err);
    }
    if let Err(err) = sess.pager.commit_tx() {
        sess.epochs = saved_epochs;
        return Err(err);
    }
    Ok(())
}

/// Resolves a located B+Tree entry (from Primary or Alternate index) into `(primary_sort_key, record_bytes)`.
///
/// # Arguments
///
/// * `pager` - Active [`Pager`].
/// * `schema` - File [`IsamSchema`].
/// * `key_id` - Index from which `entry` was read.
/// * `entry` - Located [`LeafEntry`].
fn resolve_located_record(
    pager: &mut Pager,
    schema: &IsamSchema,
    key_id: KeyIndexId,
    entry: &LeafEntry,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let path_label = String::from(pager.path_label());
    let alt_count = schema.alternate_keys().len();
    if key_id.is_primary() {
        let raw_val = resolve_leaf_value(pager, &entry.value)?;
        let (_, record) = decode_primary_value(&raw_val, alt_count, &path_label)?;
        return Ok((entry.key.clone(), record));
    }
    let primary_sort_key = resolve_leaf_value(pager, &entry.value)?;
    let pri_loc = seek_lower_bound(
        pager,
        KeyIndexId::PRIMARY,
        &primary_sort_key,
        0,
        BoundInclusion::Inclusive,
    )?
    .filter(|loc| loc.entry.key == primary_sort_key)
    .ok_or_else(|| CoblangError::IsamStorage {
        path: path_label.clone(),
        page_id: 0,
        message: String::from("dangling alternate key pointer: primary record missing"),
    })?;
    let raw_val = resolve_leaf_value(pager, &pri_loc.entry.value)?;
    let (_, record) = decode_primary_value(&raw_val, alt_count, &path_label)?;
    Ok((primary_sort_key, record))
}

/// Removes `primary_sort_key` from the Primary B+Tree and all Alternate B+Trees atomically.
///
/// # Arguments
///
/// * `sess` - Active `I-O` [`ActiveSession`].
/// * `schema` - File [`IsamSchema`].
/// * `primary_sort_key` - Collation-encoded primary key bytes.
fn delete_by_encoded_primary_key(
    sess: &mut ActiveSession,
    schema: &IsamSchema,
    primary_sort_key: &[u8],
) -> Result<FileStatusKind> {
    let exists = seek_lower_bound(
        &mut sess.pager,
        KeyIndexId::PRIMARY,
        primary_sort_key,
        0,
        BoundInclusion::Inclusive,
    )?
    .is_some_and(|loc| loc.entry.key == primary_sort_key);
    if !exists {
        return Ok(FileStatusKind::RecordNotFound23);
    }

    let saved_epochs = sess.epochs;
    sess.pager.begin_tx();
    let tx_res = (|| -> Result<()> {
        let mut pri_ep = sess.epochs[0];
        let removed_raw = btree_remove(
            &mut sess.pager,
            KeyIndexId::PRIMARY,
            primary_sort_key,
            0,
            &mut pri_ep,
        )?
        .unwrap_or_default();
        sess.epochs[0] = pri_ep;
        let path_label = String::from(sess.pager.path_label());
        let alt_count = schema.alternate_keys().len();
        let (alt_seqs, old_record) = decode_primary_value(&removed_raw, alt_count, &path_label)?;

        for (ord, alt_spec) in schema.alternate_keys().iter().enumerate() {
            let alt_id = KeyIndexId::from_validated_ordinal(u16::try_from(ord).unwrap_or(0));
            let raw_alt = alt_spec.extract_within_validated_record(&old_record);
            let sort_alt = schema.collating().encode_key(&raw_alt);
            let seq = alt_seqs.get(ord).copied().unwrap_or(0);
            let ep_idx = usize::from(alt_id.as_u16());
            let mut alt_ep = sess.epochs.get(ep_idx).copied().unwrap_or(0);
            btree_remove(&mut sess.pager, alt_id, &sort_alt, seq, &mut alt_ep)?;
            let _ = sess.epochs.get_mut(ep_idx).map(|slot| *slot = alt_ep);
        }
        sess.pager.header_mut().record_count = sess.pager.header().record_count.saturating_sub(1);
        Ok(())
    })();

    if let Err(err) = tx_res {
        sess.pager.rollback_tx();
        sess.epochs = saved_epochs;
        return Err(err);
    }
    if let Err(err) = sess.pager.commit_tx() {
        sess.epochs = saved_epochs;
        return Err(err);
    }
    Ok(FileStatusKind::Success00)
}
