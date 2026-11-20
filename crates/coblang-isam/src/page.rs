//! Fixed 4096-byte `COBLISAM` paged storage, IEEE CRC32 integrity verification,
//! freelist reclamation, overflow record chains, and schema metadata (`Milestone M15`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::rc::Rc;

use coblang_common::{CoblangError, Result};

/// Fixed byte size of every physical and in-memory `COBLISAM` page (`4096` bytes).
pub const PAGE_SIZE: usize = 4096;

/// Eight-byte ASCII magic signature stamped at byte `0..8` of every `COBLISAM` page.
pub const MAGIC_BYTES: [u8; 8] = *b"COBLISAM";

/// Serialized binary page format version (`1`).
pub const FORMAT_VERSION: u16 = 1;

/// Byte length of the universal page frame header (`magic`, `version`, `kind`, `crc32`, `page_id`).
pub const FRAME_HEADER_BYTES: usize = 24;

/// Usable payload byte capacity inside each 4096-byte page (`4072` bytes).
pub const PAGE_PAYLOAD_BYTES: usize = PAGE_SIZE - FRAME_HEADER_BYTES;

/// Maximum number of split-key segments (`RECORD KEY IS seg1 seg2 ...`) per key descriptor.
pub const MAX_KEY_SEGMENTS: usize = 16;

/// Maximum total assembled byte width of a primary or alternate key (`512` bytes).
pub const MAX_KEY_BYTES: usize = 512;

/// Maximum number of `ALTERNATE RECORD KEY` indices supported per indexed file (`16`).
pub const MAX_ALTERNATE_KEYS: usize = 16;

/// Maximum logical record byte size supported by an ISAM file (`1 MiB`).
pub const MAX_RECORD_BYTES: u32 = 1_048_576;

/// Payload chunk capacity per [`PageKind::RecordPayload`] page (`4072 - 10 = 4062` bytes).
pub const OVERFLOW_CHUNK_CAPACITY: usize = PAGE_PAYLOAD_BYTES - 10;

/// Precomputed IEEE 802.3 (`0xEDB8_8320`) CRC32 lookup table.
const CRC32_TABLE: [u32; 256] = [
    0x0000_0000,
    0x7707_3096,
    0xEE0E_612C,
    0x9909_51BA,
    0x076D_C419,
    0x706A_F48F,
    0xE963_A535,
    0x9E64_95A3,
    0x0EDB_8832,
    0x79DC_B8A4,
    0xE0D5_E91E,
    0x97D2_D988,
    0x09B6_4C2B,
    0x7EB1_7CBD,
    0xE7B8_2D07,
    0x90BF_1D91,
    0x1DB7_1064,
    0x6AB0_20F2,
    0xF3B9_7148,
    0x84BE_41DE,
    0x1ADA_D47D,
    0x6DDD_E4EB,
    0xF4D4_B551,
    0x83D3_85C7,
    0x136C_9856,
    0x646B_A8C0,
    0xFD62_F97A,
    0x8A65_C9EC,
    0x1401_5C4F,
    0x6306_6CD9,
    0xFA0F_3D63,
    0x8D08_0DF5,
    0x3B6E_20C8,
    0x4C69_105E,
    0xD560_41E4,
    0xA267_7172,
    0x3C03_E4D1,
    0x4B04_D447,
    0xD20D_85FD,
    0xA50A_B56B,
    0x35B5_A8FA,
    0x42B2_986C,
    0xDBBB_C9D6,
    0xACBC_F940,
    0x32D8_6CE3,
    0x45DF_5C75,
    0xDCD6_0DCF,
    0xABD1_3D59,
    0x26D9_30AC,
    0x51DE_003A,
    0xC8D7_5180,
    0xBFD0_6116,
    0x21B4_F4B5,
    0x56B3_C423,
    0xCFBA_9599,
    0xB8BD_A50F,
    0x2802_B89E,
    0x5F05_8808,
    0xC60C_D9B2,
    0xB10B_E924,
    0x2F6F_7C87,
    0x5868_4C11,
    0xC161_1DAB,
    0xB666_2D3D,
    0x76DC_4190,
    0x01DB_7106,
    0x98D2_20BC,
    0xEFD5_102A,
    0x71B1_8589,
    0x06B6_B51F,
    0x9FBF_E4A5,
    0xE8B8_D433,
    0x7807_C9A2,
    0x0F00_F934,
    0x9609_A88E,
    0xE10E_9818,
    0x7F6A_0DBB,
    0x086D_3D2D,
    0x9164_6C97,
    0xE663_5C01,
    0x6B6B_51F4,
    0x1C6C_6162,
    0x8565_30D8,
    0xF262_004E,
    0x6C06_95ED,
    0x1B01_A57B,
    0x8208_F4C1,
    0xF50F_C457,
    0x65B0_D9C6,
    0x12B7_E950,
    0x8BBE_B8EA,
    0xFCB9_887C,
    0x62DD_1DDF,
    0x15DA_2D49,
    0x8CD3_7CF3,
    0xFBD4_4C65,
    0x4DB2_6158,
    0x3AB5_51CE,
    0xA3BC_0074,
    0xD4BB_30E2,
    0x4ADF_A541,
    0x3DD8_95D7,
    0xA4D1_C46D,
    0xD3D6_F4FB,
    0x4369_E96A,
    0x346E_D9FC,
    0xAD67_8846,
    0xDA60_B8D0,
    0x4404_2D73,
    0x3303_1DE5,
    0xAA0A_4C5F,
    0xDD0D_7CC9,
    0x5005_713C,
    0x2702_41AA,
    0xBE0B_1010,
    0xC90C_2086,
    0x5768_B525,
    0x206F_85B3,
    0xB966_D409,
    0xCE61_E49F,
    0x5EDE_F90E,
    0x29D9_C998,
    0xB0D0_9822,
    0xC7D7_A8B4,
    0x59B3_3D17,
    0x2EB4_0D81,
    0xB7BD_5C3B,
    0xC0BA_6CAD,
    0xEDB8_8320,
    0x9ABF_B3B6,
    0x03B6_E20C,
    0x74B1_D29A,
    0xEAD5_4739,
    0x9DD2_77AF,
    0x04DB_2615,
    0x73DC_1683,
    0xE363_0B12,
    0x9464_3B84,
    0x0D6D_6A3E,
    0x7A6A_5AA8,
    0xE40E_CF0B,
    0x9309_FF9D,
    0x0A00_AE27,
    0x7D07_9EB1,
    0xF00F_9344,
    0x8708_A3D2,
    0x1E01_F268,
    0x6906_C2FE,
    0xF762_575D,
    0x8065_67CB,
    0x196C_3671,
    0x6E6B_06E7,
    0xFED4_1B76,
    0x89D3_2BE0,
    0x10DA_7A5A,
    0x67DD_4ACC,
    0xF9B9_DF6F,
    0x8EBE_EFF9,
    0x17B7_BE43,
    0x60B0_8ED5,
    0xD6D6_A3E8,
    0xA1D1_937E,
    0x38D8_C2C4,
    0x4FDF_F252,
    0xD1BB_67F1,
    0xA6BC_5767,
    0x3FB5_06DD,
    0x48B2_364B,
    0xD80D_2BDA,
    0xAF0A_1B4C,
    0x3603_4AF6,
    0x4104_7A60,
    0xDF60_EFC3,
    0xA867_DF55,
    0x316E_8EEF,
    0x4669_BE79,
    0xCB61_B38C,
    0xBC66_831A,
    0x256F_D2A0,
    0x5268_E236,
    0xCC0C_7795,
    0xBB0B_4703,
    0x2202_16B9,
    0x5505_262F,
    0xC5BA_3BBE,
    0xB2BD_0B28,
    0x2BB4_5A92,
    0x5CB3_6A04,
    0xC2D7_FFA7,
    0xB5D0_CF31,
    0x2CD9_9E8B,
    0x5BDE_AE1D,
    0x9B64_C2B0,
    0xEC63_F226,
    0x756A_A39C,
    0x026D_930A,
    0x9C09_06A9,
    0xEB0E_363F,
    0x7207_6785,
    0x0500_5713,
    0x95BF_4A82,
    0xE2B8_7A14,
    0x7BB1_2BAE,
    0x0CB6_1B38,
    0x92D2_8E9B,
    0xE5D5_BE0D,
    0x7CDC_EFB7,
    0x0BDB_DF21,
    0x86D3_D2D4,
    0xF1D4_E242,
    0x68DD_B3F8,
    0x1FDA_836E,
    0x81BE_16CD,
    0xF6B9_265B,
    0x6FB0_77E1,
    0x18B7_4777,
    0x8808_5AE6,
    0xFF0F_6A70,
    0x6606_3BCA,
    0x1101_0B5C,
    0x8F65_9EFF,
    0xF862_AE69,
    0x616B_FFD3,
    0x166C_CF45,
    0xA00A_E278,
    0xD70D_D2EE,
    0x4E04_8354,
    0x3903_B3C2,
    0xA767_2661,
    0xD060_16F7,
    0x4969_474D,
    0x3E6E_77DB,
    0xAED1_6A4A,
    0xD9D6_5ADC,
    0x40DF_0B66,
    0x37D8_3BF0,
    0xA9BC_AE53,
    0xDEBB_9EC5,
    0x47B2_CF7F,
    0x30B5_FFE9,
    0xBDBD_F21C,
    0xCABA_C28A,
    0x53B3_9330,
    0x24B4_A3A6,
    0xBAD0_3605,
    0xCDD7_0693,
    0x54DE_5729,
    0x23D9_67BF,
    0xB366_7A2E,
    0xC461_4AB8,
    0x5D68_1B02,
    0x2A6F_2B94,
    0xB40B_BE37,
    0xC30C_8EA1,
    0x5A05_DF1B,
    0x2D02_EF8D,
];

/// Identity 256-byte ordinal weight table (`weights[b] == b`).
const NATIVE_WEIGHTS: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
    0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F,
    0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
    0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F,
    0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F,
    0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
    0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
    0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF,
    0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF,
    0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF,
    0xD0, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF,
    0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF,
    0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF,
];

/// Updates a running inverted IEEE CRC32 state over `bytes`.
///
/// # Arguments
///
/// * `state` - Running inverted CRC32 accumulator (initialize with `0xFFFF_FFFF`).
/// * `bytes` - Input byte slice to fold into `state`.
#[must_use]
pub fn crc32_update(mut state: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        let [low, _, _, _] = state.to_le_bytes();
        let idx = usize::from(low ^ byte);
        let entry = CRC32_TABLE.get(idx).copied().unwrap_or(0);
        state = (state >> 8) ^ entry;
    }
    state
}

/// Computes the standard IEEE 802.3 CRC32 checksum of `bytes`.
///
/// # Arguments
///
/// * `bytes` - Input byte slice to checksum.
#[must_use]
pub fn crc32_bytes(bytes: &[u8]) -> u32 {
    !crc32_update(0xFFFF_FFFF, bytes)
}

/// Computes the page-frame IEEE CRC32 checksum covering bytes `0..12` and `16..4096`.
///
/// Bytes `12..16` store the little-endian `crc32` field itself and are excluded.
///
/// # Arguments
///
/// * `raw_page` - Full 4096-byte page buffer.
#[must_use]
pub fn compute_page_crc32(raw_page: &[u8; PAGE_SIZE]) -> u32 {
    let head = raw_page.get(..12).unwrap_or(&[]);
    let tail = raw_page.get(16..).unwrap_or(&[]);
    let state = crc32_update(0xFFFF_FFFF, head);
    !crc32_update(state, tail)
}

/// Strongly-typed zero-based 4096-byte page identifier inside a `COBLISAM` file.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageId(
    /// Underlying zero-based page number.
    pub u64,
);

impl PageId {
    /// Reserved page `0` storing the `COBLISAM` file header, freelist head, and schema metadata.
    pub const HEADER: Self = Self(0);

    /// Wraps a raw zero-based `u64` page number.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based 64-bit page number.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the underlying zero-based `u64` page number.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Returns `self + 1`, failing on `u64::MAX` page-counter exhaustion.
    ///
    /// # Arguments
    ///
    /// * `path` - File path label used in error diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if incrementing `self` overflows `u64`.
    pub fn checked_next(self, path: &str) -> Result<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| CoblangError::IsamStorage {
                path: String::from(path),
                page_id: self.0,
                message: String::from("page identifier u64 counter overflow"),
            })
    }

    /// Encodes an optional non-header [`PageId`] (`None` -> `0`, `Some(id)` -> `id.as_u64()`).
    ///
    /// # Arguments
    ///
    /// * `page` - Optional non-header page reference.
    #[must_use]
    pub const fn encode_optional(page: Option<Self>) -> u64 {
        match page {
            None => 0,
            Some(id) => id.0,
        }
    }

    /// Decodes a serialized optional non-header page link (`0` -> `None`, `n >= 1` -> `Some(PageId(n))`).
    ///
    /// # Arguments
    ///
    /// * `raw` - Serialized `u64` page link (`0` represents null).
    #[must_use]
    pub const fn decode_optional(raw: u64) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }
}

/// Strongly-typed zero-based entry/cell slot index within a B+Tree page.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotIndex(
    /// Zero-based 16-bit slot index.
    pub u16,
);

impl SlotIndex {
    /// First slot (`0`) on a B+Tree page.
    pub const ZERO: Self = Self(0);

    /// Wraps a raw `u16` slot index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based 16-bit slot index.
    #[must_use]
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// Converts a `usize` index into a validated [`SlotIndex`].
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based `usize` slot index.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw > usize::from(u16::MAX)`.
    pub fn try_from_usize(raw: usize) -> Result<Self> {
        u16::try_from(raw)
            .map(Self)
            .map_err(|_| CoblangError::InvalidDomainValue {
                domain_type: "SlotIndex",
                value: raw.to_string(),
                reason: String::from("slot index exceeds u16::MAX"),
            })
    }

    /// Returns the underlying `u16` slot index.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// Widens this slot index to `usize`.
    #[must_use]
    pub fn as_usize(self) -> usize {
        usize::from(self.0)
    }
}

/// Duplicate-key admission policy for primary and alternate B+Tree indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DuplicateKeyPolicy {
    /// Keys must be strictly unique (`"22"` on duplicate `WRITE` / `REWRITE`).
    Unique,
    /// `ALTERNATE RECORD KEY WITH DUPLICATES` preserving monotonic FIFO order (`"02"` status).
    AllowedFifo,
}

impl DuplicateKeyPolicy {
    /// Encodes `self` into its 1-byte header tag (`0 = Unique`, `1 = AllowedFifo`).
    #[must_use]
    pub const fn to_tag(self) -> u8 {
        match self {
            Self::Unique => 0,
            Self::AllowedFifo => 1,
        }
    }

    /// Decodes a 1-byte header tag into [`DuplicateKeyPolicy`].
    ///
    /// # Arguments
    ///
    /// * `tag` - Raw serialized policy byte.
    /// * `path` - File path label for corruption diagnostics.
    /// * `page_id` - Page identifier where `tag` was read.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if `tag` is neither `0` nor `1`.
    pub fn from_tag(tag: u8, path: &str, page_id: PageId) -> Result<Self> {
        match tag {
            0 => Ok(Self::Unique),
            1 => Ok(Self::AllowedFifo),
            other => Err(CoblangError::IsamStorage {
                path: String::from(path),
                page_id: page_id.as_u64(),
                message: format!("invalid DuplicateKeyPolicy tag {other}"),
            }),
        }
    }
}

/// Validated zero-based `(offset, length)` byte slice segment within an ISAM record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySegmentSpec {
    /// Zero-based starting byte offset inside the record.
    offset: u32,
    /// Non-zero byte length of this key segment.
    length: u32,
}

impl KeySegmentSpec {
    /// Validates and constructs a [`KeySegmentSpec`].
    ///
    /// # Arguments
    ///
    /// * `offset` - Zero-based byte offset inside the record.
    /// * `length` - Segment byte length (`>= 1`, without `u32` overflow in `offset + length`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `length == 0` or `offset + length` overflows `u32`.
    pub fn try_new(offset: u32, length: u32) -> Result<Self> {
        if length == 0 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "KeySegmentSpec",
                value: format!("offset={offset}, length={length}"),
                reason: String::from("key segment length must be at least 1 byte"),
            });
        }
        if offset.checked_add(length).is_none() {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "KeySegmentSpec",
                value: format!("offset={offset}, length={length}"),
                reason: String::from("key segment offset + length overflows u32"),
            });
        }
        Ok(Self { offset, length })
    }

    /// Returns the zero-based starting byte offset within the record.
    #[must_use]
    pub const fn offset(self) -> u32 {
        self.offset
    }

    /// Returns the non-zero segment byte length.
    #[must_use]
    pub const fn length(self) -> u32 {
        self.length
    }

    /// Returns the exclusive ending byte offset (`offset + length`).
    #[must_use]
    pub const fn end_offset(self) -> u32 {
        self.offset + self.length
    }
}

/// Complete single-segment or composite/split (`RECORD KEY IS seg1 seg2`) key descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpec {
    /// Ordered list of `1..=MAX_KEY_SEGMENTS` byte segments concatenated to form the key.
    segments: Vec<KeySegmentSpec>,
    /// Uniqueness vs FIFO-duplicate policy.
    duplicates: DuplicateKeyPolicy,
    /// Cached sum of all segment byte lengths (`1..=MAX_KEY_BYTES`).
    total_len: usize,
    /// Cached maximum `end_offset` across all segments.
    max_end: u32,
}

impl KeySpec {
    /// Validates and constructs a [`KeySpec`] from one or more [`KeySegmentSpec`]s.
    ///
    /// # Arguments
    ///
    /// * `segments` - Non-empty ordered list of key byte segments (`1..=16`).
    /// * `duplicates` - Duplicate-key policy (`Unique` for primary keys).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `segments` is empty, exceeds
    /// [`MAX_KEY_SEGMENTS`], or has assembled byte length exceeding [`MAX_KEY_BYTES`].
    pub fn try_new(segments: Vec<KeySegmentSpec>, duplicates: DuplicateKeyPolicy) -> Result<Self> {
        if segments.is_empty() || segments.len() > MAX_KEY_SEGMENTS {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "KeySpec",
                value: format!("segments={}", segments.len()),
                reason: format!("segment count must be in 1..={MAX_KEY_SEGMENTS}"),
            });
        }
        let mut total_len = 0_usize;
        let mut max_end = 0_u32;
        for seg in &segments {
            let seg_len = usize::try_from(seg.length()).unwrap_or(MAX_KEY_BYTES + 1);
            total_len = total_len.saturating_add(seg_len);
            if seg.end_offset() > max_end {
                max_end = seg.end_offset();
            }
        }
        if total_len > MAX_KEY_BYTES {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "KeySpec",
                value: total_len.to_string(),
                reason: format!("assembled key length exceeds maximum of {MAX_KEY_BYTES} bytes"),
            });
        }
        Ok(Self {
            segments,
            duplicates,
            total_len,
            max_end,
        })
    }

    /// Convenience constructor for a contiguous single-segment key.
    ///
    /// # Arguments
    ///
    /// * `offset` - Zero-based byte offset inside the record.
    /// * `length` - Non-zero key byte length (`1..=512`).
    /// * `duplicates` - Duplicate-key policy.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `(offset, length)` or key width is invalid.
    pub fn single(offset: u32, length: u32, duplicates: DuplicateKeyPolicy) -> Result<Self> {
        let seg = KeySegmentSpec::try_new(offset, length)?;
        Self::try_new(vec![seg], duplicates)
    }

    /// Returns the ordered slice of [`KeySegmentSpec`]s.
    #[must_use]
    pub fn segments(&self) -> &[KeySegmentSpec] {
        &self.segments
    }

    /// Returns the [`DuplicateKeyPolicy`] of this key.
    #[must_use]
    pub const fn duplicates(&self) -> DuplicateKeyPolicy {
        self.duplicates
    }

    /// Returns the assembled key width in bytes (`1..=MAX_KEY_BYTES`).
    #[must_use]
    pub const fn total_byte_len(&self) -> usize {
        self.total_len
    }

    /// Returns the minimum record byte length required to cover all segments of this key.
    #[must_use]
    pub const fn max_end_offset(&self) -> u32 {
        self.max_end
    }

    /// Assembles raw key bytes from `record`, validating that `record` covers every segment.
    ///
    /// # Arguments
    ///
    /// * `record` - Logical record byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `record.len() < self.max_end_offset()`.
    pub fn extract_from_record(&self, record: &[u8]) -> Result<Vec<u8>> {
        let required = usize::try_from(self.max_end).unwrap_or(usize::MAX);
        if record.len() < required {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "KeySpec",
                value: format!("record_len={}", record.len()),
                reason: format!("record shorter than key end offset {required}"),
            });
        }
        Ok(self.extract_within_validated_record(record))
    }

    /// Assembles raw key bytes from a pre-validated `record` (`record.len() >= max_end_offset()`).
    ///
    /// # Arguments
    ///
    /// * `record` - Pre-validated record byte slice.
    #[must_use]
    pub(crate) fn extract_within_validated_record(&self, record: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total_len);
        for seg in &self.segments {
            let start = usize::try_from(seg.offset()).unwrap_or(0);
            let end = usize::try_from(seg.end_offset()).unwrap_or(0);
            out.extend_from_slice(record.get(start..end).unwrap_or(&[]));
        }
        out
    }
}

/// Strongly-typed identifier selecting the primary key (`0`) or an alternate key (`1..=16`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyIndexId(
    /// `0` for primary `RECORD KEY`, `1 + i` for the `i`-th `ALTERNATE RECORD KEY`.
    u16,
);

impl KeyIndexId {
    /// Primary `RECORD KEY` index (`0`).
    pub const PRIMARY: Self = Self(0);

    /// Constructs the [`KeyIndexId`] for the `zero_based_ordinal`-th `ALTERNATE RECORD KEY`.
    ///
    /// # Arguments
    ///
    /// * `zero_based_ordinal` - Zero-based index into `IsamSchema::alternate_keys` (`0..16`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `zero_based_ordinal >= MAX_ALTERNATE_KEYS`.
    pub fn alternate(zero_based_ordinal: u16) -> Result<Self> {
        if usize::from(zero_based_ordinal) >= MAX_ALTERNATE_KEYS {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "KeyIndexId",
                value: zero_based_ordinal.to_string(),
                reason: format!("alternate key ordinal must be < {MAX_ALTERNATE_KEYS}"),
            });
        }
        Ok(Self(zero_based_ordinal + 1))
    }

    /// Constructs the [`KeyIndexId`] for a pre-validated ordinal in `0..MAX_ALTERNATE_KEYS`.
    ///
    /// # Arguments
    ///
    /// * `zero_based_ordinal` - Validated ordinal (`< MAX_ALTERNATE_KEYS`).
    #[must_use]
    pub(crate) const fn from_validated_ordinal(zero_based_ordinal: u16) -> Self {
        Self(zero_based_ordinal + 1)
    }

    /// Returns `true` when `self` selects the primary `RECORD KEY`.
    #[must_use]
    pub const fn is_primary(self) -> bool {
        self.0 == 0
    }

    /// Returns the raw `u16` index (`0` = primary, `1..=16` = alternate).
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// Returns `Some(zero_based_ordinal)` for an alternate key, or `None` for [`Self::PRIMARY`].
    #[must_use]
    pub fn alternate_ordinal(self) -> Option<usize> {
        if self.0 == 0 {
            None
        } else {
            Some(usize::from(self.0 - 1))
        }
    }
}

/// Strongly-typed collating sequence identifier persisted in `COBLISAM` schema metadata.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CollatingId(
    /// Raw 16-bit collating sequence tag (`0 = NATIVE`, `1 = EBCDIC`, `>= 2` = custom `ALPHABET`).
    pub u16,
);

impl CollatingId {
    /// Identity unsigned byte ordering (`0x00..=0xFF`).
    pub const NATIVE: Self = Self(0);

    /// Canonical EBCDIC collating sequence identifier (`1`).
    pub const EBCDIC: Self = Self(1);

    /// Wraps a raw 16-bit collating sequence identifier.
    ///
    /// # Arguments
    ///
    /// * `raw` - Numeric collating sequence tag.
    #[must_use]
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// Returns the underlying `u16` identifier.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self.0
    }
}

/// Complete 256-byte ordinal collating sequence persisted in the `COBLISAM` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollatingSequence {
    /// Collating sequence tag checked on `OPEN` for attribute compatibility.
    id: CollatingId,
    /// Ordinal sort weight (`0..=255`) assigned to each possible input byte `0x00..=0xFF`.
    weights: [u8; 256],
}

impl CollatingSequence {
    /// Constructs the identity unsigned-byte `NATIVE` collating sequence.
    #[must_use]
    pub const fn native() -> Self {
        Self {
            id: CollatingId::NATIVE,
            weights: NATIVE_WEIGHTS,
        }
    }

    /// Constructs a custom or EBCDIC [`CollatingSequence`] from explicit ordinal `weights`.
    ///
    /// # Arguments
    ///
    /// * `id` - Collating sequence identifier.
    /// * `weights` - 256-entry byte-to-rank mapping table.
    #[must_use]
    pub const fn custom(id: CollatingId, weights: [u8; 256]) -> Self {
        Self { id, weights }
    }

    /// Returns the [`CollatingId`] of this sequence.
    #[must_use]
    pub const fn id(&self) -> CollatingId {
        self.id
    }

    /// Returns the 256-byte rank table.
    #[must_use]
    pub const fn weights(&self) -> &[u8; 256] {
        &self.weights
    }

    /// Projects `raw_key` through `self.weights` into an unsigned-lexicographical sort key.
    ///
    /// # Arguments
    ///
    /// * `raw_key` - Raw key or partial-key prefix bytes extracted from a COBOL field/record.
    #[must_use]
    pub fn encode_key(&self, raw_key: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(raw_key.len());
        for &b in raw_key {
            let weight = self.weights.get(usize::from(b)).copied().unwrap_or(b);
            out.push(weight);
        }
        out
    }
}

/// Validated minimum and maximum logical record byte bounds for an `FD` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordBounds {
    /// Minimum permitted record length in bytes (`>= 1`).
    min_bytes: u32,
    /// Maximum permitted record length in bytes (`min_bytes..=MAX_RECORD_BYTES`).
    max_bytes: u32,
}

impl RecordBounds {
    /// Constructs fixed-length `RECORD CONTAINS byte_len CHARACTERS` bounds.
    ///
    /// # Arguments
    ///
    /// * `byte_len` - Exact fixed record size in bytes (`1..=MAX_RECORD_BYTES`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `byte_len == 0` or `byte_len > MAX_RECORD_BYTES`.
    pub fn fixed(byte_len: u32) -> Result<Self> {
        Self::varying(byte_len, byte_len)
    }

    /// Constructs variable-length `RECORD IS VARYING FROM min_bytes TO max_bytes` bounds.
    ///
    /// # Arguments
    ///
    /// * `min_bytes` - Minimum valid record byte length (`>= 1`).
    /// * `max_bytes` - Maximum valid record byte length (`min_bytes..=MAX_RECORD_BYTES`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `min_bytes == 0`, `min_bytes > max_bytes`,
    /// or `max_bytes > MAX_RECORD_BYTES`.
    pub fn varying(min_bytes: u32, max_bytes: u32) -> Result<Self> {
        if min_bytes == 0 || min_bytes > max_bytes || max_bytes > MAX_RECORD_BYTES {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "RecordBounds",
                value: format!("{min_bytes}..={max_bytes}"),
                reason: format!(
                    "record bounds must satisfy 1 <= min_bytes <= max_bytes <= {MAX_RECORD_BYTES}"
                ),
            });
        }
        Ok(Self {
            min_bytes,
            max_bytes,
        })
    }

    /// Returns the minimum valid record length in bytes.
    #[must_use]
    pub const fn min_bytes(self) -> u32 {
        self.min_bytes
    }

    /// Returns the maximum valid record length in bytes.
    #[must_use]
    pub const fn max_bytes(self) -> u32 {
        self.max_bytes
    }

    /// Returns `true` when `byte_len` falls within `min_bytes..=max_bytes`.
    ///
    /// # Arguments
    ///
    /// * `byte_len` - Candidate record slice length in bytes.
    #[must_use]
    pub fn contains_len(self, byte_len: usize) -> bool {
        let min = usize::try_from(self.min_bytes).unwrap_or(usize::MAX);
        let max = usize::try_from(self.max_bytes).unwrap_or(0);
        byte_len >= min && byte_len <= max
    }
}

/// Validated `ORGANIZATION IS INDEXED` schema persisted in `PageId::HEADER`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsamSchema {
    /// Fixed or varying record length bounds.
    record_bounds: RecordBounds,
    /// Primary `RECORD KEY` descriptor (always [`DuplicateKeyPolicy::Unique`]).
    primary_key: KeySpec,
    /// Ordered `ALTERNATE RECORD KEY` descriptors (`0..=MAX_ALTERNATE_KEYS`).
    alternate_keys: Vec<KeySpec>,
    /// Active key collating sequence.
    collating: CollatingSequence,
}

impl IsamSchema {
    /// Validates and constructs an [`IsamSchema`].
    ///
    /// # Arguments
    ///
    /// * `record_bounds` - Minimum/maximum record size bounds.
    /// * `primary_key` - Primary `RECORD KEY` (must specify [`DuplicateKeyPolicy::Unique`]).
    /// * `alternate_keys` - Secondary `ALTERNATE RECORD KEY` specifications (`<= 16`).
    /// * `collating` - Active [`CollatingSequence`] for key comparisons.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `primary_key` allows duplicates,
    /// `alternate_keys.len() > MAX_ALTERNATE_KEYS`, or any key segment extends past
    /// `record_bounds.min_bytes()`.
    pub fn try_new(
        record_bounds: RecordBounds,
        primary_key: KeySpec,
        alternate_keys: Vec<KeySpec>,
        collating: CollatingSequence,
    ) -> Result<Self> {
        if primary_key.duplicates() != DuplicateKeyPolicy::Unique {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "IsamSchema",
                value: String::from("primary_key.duplicates=AllowedFifo"),
                reason: String::from("primary RECORD KEY must use DuplicateKeyPolicy::Unique"),
            });
        }
        if alternate_keys.len() > MAX_ALTERNATE_KEYS {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "IsamSchema",
                value: alternate_keys.len().to_string(),
                reason: format!("alternate key count exceeds maximum of {MAX_ALTERNATE_KEYS}"),
            });
        }
        let min_rec = record_bounds.min_bytes();
        if primary_key.max_end_offset() > min_rec {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "IsamSchema",
                value: primary_key.max_end_offset().to_string(),
                reason: format!("primary key end offset exceeds minimum record length {min_rec}"),
            });
        }
        for (idx, alt) in alternate_keys.iter().enumerate() {
            if alt.max_end_offset() > min_rec {
                return Err(CoblangError::InvalidDomainValue {
                    domain_type: "IsamSchema",
                    value: format!("alt[{idx}].end={}", alt.max_end_offset()),
                    reason: format!(
                        "alternate key end offset exceeds minimum record length {min_rec}"
                    ),
                });
            }
        }
        Ok(Self {
            record_bounds,
            primary_key,
            alternate_keys,
            collating,
        })
    }

    /// Returns the [`RecordBounds`] of this file schema.
    #[must_use]
    pub const fn record_bounds(&self) -> RecordBounds {
        self.record_bounds
    }

    /// Returns the primary [`KeySpec`].
    #[must_use]
    pub const fn primary_key(&self) -> &KeySpec {
        &self.primary_key
    }

    /// Returns the slice of alternate [`KeySpec`]s.
    #[must_use]
    pub fn alternate_keys(&self) -> &[KeySpec] {
        &self.alternate_keys
    }

    /// Returns the [`CollatingSequence`] of this schema.
    #[must_use]
    pub const fn collating(&self) -> &CollatingSequence {
        &self.collating
    }

    /// Resolves a [`KeyIndexId`] (`PRIMARY` or `alternate(i)`) to its [`KeySpec`].
    ///
    /// # Arguments
    ///
    /// * `key_id` - Target key index handle.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `key_id` names a non-existent alternate key.
    pub fn key_spec(&self, key_id: KeyIndexId) -> Result<&KeySpec> {
        match key_id.alternate_ordinal() {
            None => Ok(&self.primary_key),
            Some(ord) => {
                self.alternate_keys
                    .get(ord)
                    .ok_or_else(|| CoblangError::InvalidDomainValue {
                        domain_type: "KeyIndexId",
                        value: key_id.as_u16().to_string(),
                        reason: format!(
                            "alternate key ordinal {ord} out of range (count {})",
                            self.alternate_keys.len()
                        ),
                    })
            }
        }
    }

    /// Checks ISO COBOL `OPEN` attribute compatibility (`"39"` check) against `other`.
    ///
    /// # Arguments
    ///
    /// * `other` - Persisted [`IsamSchema`] decoded from `PageId::HEADER`.
    #[must_use]
    pub fn is_compatible_with(&self, other: &Self) -> bool {
        self.record_bounds == other.record_bounds
            && self.primary_key == other.primary_key
            && self.alternate_keys == other.alternate_keys
            && self.collating.id() == other.collating.id()
    }
}

/// Discriminator tag stamped at byte `10` of every 4096-byte `COBLISAM` page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageKind {
    /// Page `0` file header (`schema`, `total_pages`, `free_head`, `record_count`, B+Tree roots).
    FileHeader,
    /// Reclaimed freelist page storing `next_free: Option<PageId>`.
    Free,
    /// Order-$B$ B+Tree internal separator/child routing node.
    BTreeInternal,
    /// Doubly-linked B+Tree leaf node (`prev_leaf`, `next_leaf`, sorted entries).
    BTreeLeaf,
    /// Chained overflow payload page for large variable/fixed records.
    RecordPayload,
}

impl PageKind {
    /// Encodes `self` into its 1-byte header tag (`1..=5`).
    #[must_use]
    pub const fn to_tag(self) -> u8 {
        match self {
            Self::FileHeader => 1,
            Self::Free => 2,
            Self::BTreeInternal => 3,
            Self::BTreeLeaf => 4,
            Self::RecordPayload => 5,
        }
    }

    /// Decodes a 1-byte header tag into [`PageKind`].
    ///
    /// # Arguments
    ///
    /// * `tag` - Raw page kind tag byte at offset `10`.
    /// * `path` - File path label for error reporting.
    /// * `page_id` - Page identifier being decoded.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if `tag` is outside `1..=5`.
    pub fn from_tag(tag: u8, path: &str, page_id: PageId) -> Result<Self> {
        match tag {
            1 => Ok(Self::FileHeader),
            2 => Ok(Self::Free),
            3 => Ok(Self::BTreeInternal),
            4 => Ok(Self::BTreeLeaf),
            5 => Ok(Self::RecordPayload),
            other => Err(CoblangError::IsamStorage {
                path: String::from(path),
                page_id: page_id.as_u64(),
                message: format!("unknown page kind tag {other}"),
            }),
        }
    }
}

/// Builds a complete 4096-byte page from `payload` (`<= 4072` bytes) and stamps its CRC32 frame.
///
/// # Arguments
///
/// * `kind` - [`PageKind`] tag for byte `10`.
/// * `page_id` - Expected [`PageId`] stamped at bytes `16..24`.
/// * `payload` - Serialized body bytes copied starting at byte `24`.
#[must_use]
pub fn build_framed_page(kind: PageKind, page_id: PageId, payload: &[u8]) -> [u8; PAGE_SIZE] {
    let mut raw = [0_u8; PAGE_SIZE];
    for (dst, &src) in raw.iter_mut().zip(MAGIC_BYTES.iter()) {
        *dst = src;
    }
    for (dst, &src) in raw
        .iter_mut()
        .skip(8)
        .zip(FORMAT_VERSION.to_le_bytes().iter())
    {
        *dst = src;
    }
    raw[10] = kind.to_tag();
    for (dst, &src) in raw
        .iter_mut()
        .skip(16)
        .zip(page_id.as_u64().to_le_bytes().iter())
    {
        *dst = src;
    }
    for (dst, &src) in raw.iter_mut().skip(FRAME_HEADER_BYTES).zip(payload.iter()) {
        *dst = src;
    }
    let crc = compute_page_crc32(&raw);
    for (dst, &src) in raw.iter_mut().skip(12).zip(crc.to_le_bytes().iter()) {
        *dst = src;
    }
    raw
}

/// Verifies the 24-byte `COBLISAM` frame (`magic`, `version`, `page_id`, and `CRC32`) and returns its [`PageKind`].
///
/// # Arguments
///
/// * `raw` - Raw 4096-byte page buffer read from storage.
/// * `expected_id` - Expected [`PageId`].
/// * `path` - File path label for corruption diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::IsamStorage`] if magic, version, kind tag, `page_id`, or CRC32 is invalid.
pub fn verify_any_page_frame(
    raw: &[u8; PAGE_SIZE],
    expected_id: PageId,
    path: &str,
) -> Result<PageKind> {
    if raw.get(..8).unwrap_or(&[]) != MAGIC_BYTES {
        return Err(CoblangError::IsamStorage {
            path: String::from(path),
            page_id: expected_id.as_u64(),
            message: String::from("invalid COBLISAM page magic header"),
        });
    }
    let version = u16::from_le_bytes([
        raw.get(8).copied().unwrap_or(0),
        raw.get(9).copied().unwrap_or(0),
    ]);
    if version != FORMAT_VERSION {
        return Err(CoblangError::IsamStorage {
            path: String::from(path),
            page_id: expected_id.as_u64(),
            message: format!("unsupported COBLISAM format version {version}"),
        });
    }
    let kind = PageKind::from_tag(raw.get(10).copied().unwrap_or(0), path, expected_id)?;
    let stored_id = u64::from_le_bytes([
        raw.get(16).copied().unwrap_or(0),
        raw.get(17).copied().unwrap_or(0),
        raw.get(18).copied().unwrap_or(0),
        raw.get(19).copied().unwrap_or(0),
        raw.get(20).copied().unwrap_or(0),
        raw.get(21).copied().unwrap_or(0),
        raw.get(22).copied().unwrap_or(0),
        raw.get(23).copied().unwrap_or(0),
    ]);
    if stored_id != expected_id.as_u64() {
        return Err(CoblangError::IsamStorage {
            path: String::from(path),
            page_id: expected_id.as_u64(),
            message: format!("page_id mismatch: header stores {stored_id}"),
        });
    }
    let stored_crc = u32::from_le_bytes([
        raw.get(12).copied().unwrap_or(0),
        raw.get(13).copied().unwrap_or(0),
        raw.get(14).copied().unwrap_or(0),
        raw.get(15).copied().unwrap_or(0),
    ]);
    let computed_crc = compute_page_crc32(raw);
    if stored_crc != computed_crc {
        return Err(CoblangError::IsamStorage {
            path: String::from(path),
            page_id: expected_id.as_u64(),
            message: format!(
                "CRC32 checksum mismatch: stored 0x{stored_crc:08X}, computed 0x{computed_crc:08X}"
            ),
        });
    }
    Ok(kind)
}

/// Verifies the 24-byte `COBLISAM` frame (`magic`, `version`, `kind`, `page_id`, and `CRC32`).
///
/// # Arguments
///
/// * `raw` - Raw 4096-byte page buffer read from storage.
/// * `expected_kind` - Required [`PageKind`].
/// * `expected_id` - Expected [`PageId`].
/// * `path` - File path label for corruption diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::IsamStorage`] if magic, version, kind, `page_id`, or CRC32 mismatches.
pub fn verify_page_frame(
    raw: &[u8; PAGE_SIZE],
    expected_kind: PageKind,
    expected_id: PageId,
    path: &str,
) -> Result<()> {
    let kind = verify_any_page_frame(raw, expected_id, path)?;
    if kind != expected_kind {
        return Err(CoblangError::IsamStorage {
            path: String::from(path),
            page_id: expected_id.as_u64(),
            message: format!("expected page kind {expected_kind:?}, found {kind:?}"),
        });
    }
    Ok(())
}

/// Bounds-checked sequential cursor over a page payload byte slice.
pub(crate) struct PayloadReader<'buf, 'path> {
    /// Backing payload slice (`raw[24..4096]`).
    buf: &'buf [u8],
    /// Current zero-based read cursor inside `buf`.
    pos: usize,
    /// File path label for corruption diagnostics.
    path: &'path str,
    /// Owning page identifier for corruption diagnostics.
    page_id: PageId,
}

impl<'buf, 'path> PayloadReader<'buf, 'path> {
    /// Creates a new [`PayloadReader`] over the payload body of `raw_page`.
    ///
    /// # Arguments
    ///
    /// * `raw_page` - Verified 4096-byte page buffer.
    /// * `path` - File path label for error diagnostics.
    /// * `page_id` - Identifier of `raw_page`.
    #[must_use]
    pub(crate) fn new(raw_page: &'buf [u8; PAGE_SIZE], path: &'path str, page_id: PageId) -> Self {
        Self {
            buf: raw_page.get(FRAME_HEADER_BYTES..).unwrap_or(&[]),
            pos: 0,
            path,
            page_id,
        }
    }

    /// Reads an exact sub-slice of `len` bytes, advancing `self.pos`.
    ///
    /// # Arguments
    ///
    /// * `len` - Number of bytes to consume.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if fewer than `len` bytes remain in `self.buf`.
    pub(crate) fn read_slice(&mut self, len: usize) -> Result<&'buf [u8]> {
        let end = self.pos.saturating_add(len);
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| CoblangError::IsamStorage {
                path: String::from(self.path),
                page_id: self.page_id.as_u64(),
                message: format!(
                    "truncated page payload at offset {}: needed {len} bytes",
                    self.pos
                ),
            })?;
        self.pos = end;
        Ok(slice)
    }

    /// Reads a single `u8`.
    pub(crate) fn read_u8(&mut self) -> Result<u8> {
        let s = self.read_slice(1)?;
        Ok(s.first().copied().unwrap_or(0))
    }

    /// Reads a little-endian `u16`.
    pub(crate) fn read_u16(&mut self) -> Result<u16> {
        let s = self.read_slice(2)?;
        Ok(u16::from_le_bytes([
            s.first().copied().unwrap_or(0),
            s.get(1).copied().unwrap_or(0),
        ]))
    }

    /// Reads a little-endian `u32`.
    pub(crate) fn read_u32(&mut self) -> Result<u32> {
        let s = self.read_slice(4)?;
        Ok(u32::from_le_bytes([
            s.first().copied().unwrap_or(0),
            s.get(1).copied().unwrap_or(0),
            s.get(2).copied().unwrap_or(0),
            s.get(3).copied().unwrap_or(0),
        ]))
    }

    /// Reads a little-endian `u64`.
    pub(crate) fn read_u64(&mut self) -> Result<u64> {
        let s = self.read_slice(8)?;
        Ok(u64::from_le_bytes([
            s.first().copied().unwrap_or(0),
            s.get(1).copied().unwrap_or(0),
            s.get(2).copied().unwrap_or(0),
            s.get(3).copied().unwrap_or(0),
            s.get(4).copied().unwrap_or(0),
            s.get(5).copied().unwrap_or(0),
            s.get(6).copied().unwrap_or(0),
            s.get(7).copied().unwrap_or(0),
        ]))
    }
}

/// Decoded contents of `PageId::HEADER` (`PageKind::FileHeader`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHeaderState {
    /// Total allocated 4096-byte pages in the file (`>= 1`).
    pub total_pages: u64,
    /// Head of the singly-linked reclaimed [`PageKind::Free`] page list.
    pub free_head: Option<PageId>,
    /// Number of live records currently stored in the file.
    pub record_count: u64,
    /// Monotonically increasing FIFO duplicate sequence counter (`>= 1`).
    pub next_dup_seq: u64,
    /// Root [`PageId`] of the primary `RECORD KEY` B+Tree (`None` when empty).
    pub primary_root: Option<PageId>,
    /// Root [`PageId`]s for each `ALTERNATE RECORD KEY` B+Tree (parallel to `schema.alternate_keys()`).
    pub alternate_roots: Vec<Option<PageId>>,
    /// Persisted [`IsamSchema`] metadata.
    pub schema: IsamSchema,
}

impl FileHeaderState {
    /// Creates an initial empty [`FileHeaderState`] (`total_pages = 1`, `next_dup_seq = 1`).
    ///
    /// # Arguments
    ///
    /// * `schema` - Validated [`IsamSchema`] for the newly created file.
    #[must_use]
    pub fn new_empty(schema: IsamSchema) -> Self {
        let alt_count = schema.alternate_keys().len();
        Self {
            total_pages: 1,
            free_head: None,
            record_count: 0,
            next_dup_seq: 1,
            primary_root: None,
            alternate_roots: vec![None; alt_count],
            schema,
        }
    }

    /// Serializes `self` into a framed 4096-byte `PageId::HEADER` buffer.
    #[must_use]
    pub fn encode_page(&self) -> [u8; PAGE_SIZE] {
        let mut out = Vec::with_capacity(PAGE_PAYLOAD_BYTES);
        out.extend_from_slice(&self.total_pages.to_le_bytes());
        out.extend_from_slice(&PageId::encode_optional(self.free_head).to_le_bytes());
        out.extend_from_slice(&self.record_count.to_le_bytes());
        out.extend_from_slice(&self.next_dup_seq.to_le_bytes());
        out.extend_from_slice(&PageId::encode_optional(self.primary_root).to_le_bytes());
        out.extend_from_slice(&self.schema.record_bounds().min_bytes().to_le_bytes());
        out.extend_from_slice(&self.schema.record_bounds().max_bytes().to_le_bytes());
        out.extend_from_slice(&self.schema.collating().id().as_u16().to_le_bytes());
        out.extend_from_slice(self.schema.collating().weights());

        encode_key_spec(&mut out, self.schema.primary_key());
        let alt_len = u16::try_from(self.schema.alternate_keys().len()).unwrap_or(0);
        out.extend_from_slice(&alt_len.to_le_bytes());
        for (alt_spec, &root_opt) in self
            .schema
            .alternate_keys()
            .iter()
            .zip(self.alternate_roots.iter())
        {
            out.extend_from_slice(&PageId::encode_optional(root_opt).to_le_bytes());
            encode_key_spec(&mut out, alt_spec);
        }
        build_framed_page(PageKind::FileHeader, PageId::HEADER, &out)
    }

    /// Verifies and decodes a `PageId::HEADER` 4096-byte buffer into [`FileHeaderState`].
    ///
    /// # Arguments
    ///
    /// * `raw_page` - Raw 4096-byte page `0` buffer.
    /// * `path` - File path label for error diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if the header frame, CRC32, page counts, or schema
    /// descriptors are corrupted.
    pub fn decode_page(raw_page: &[u8; PAGE_SIZE], path: &str) -> Result<Self> {
        verify_page_frame(raw_page, PageKind::FileHeader, PageId::HEADER, path)?;
        let mut rdr = PayloadReader::new(raw_page, path, PageId::HEADER);
        let total_pages = rdr.read_u64()?;
        if total_pages == 0 {
            return Err(CoblangError::IsamStorage {
                path: String::from(path),
                page_id: 0,
                message: String::from("corrupted header: total_pages cannot be 0"),
            });
        }
        let free_head = PageId::decode_optional(rdr.read_u64()?);
        let record_count = rdr.read_u64()?;
        let next_dup_seq = rdr.read_u64()?;
        if next_dup_seq == 0 {
            return Err(CoblangError::IsamStorage {
                path: String::from(path),
                page_id: 0,
                message: String::from("corrupted header: next_dup_seq cannot be 0"),
            });
        }
        let primary_root = PageId::decode_optional(rdr.read_u64()?);
        let min_rec = rdr.read_u32()?;
        let max_rec = rdr.read_u32()?;
        let record_bounds =
            RecordBounds::varying(min_rec, max_rec).map_err(|err| CoblangError::IsamStorage {
                path: String::from(path),
                page_id: 0,
                message: format!("corrupted header record bounds: {err}"),
            })?;

        let collating_id = CollatingId::new(rdr.read_u16()?);
        let weight_slice = rdr.read_slice(256)?;
        let mut weights = [0_u8; 256];
        for (dst, &src) in weights.iter_mut().zip(weight_slice.iter()) {
            *dst = src;
        }
        let collating = CollatingSequence::custom(collating_id, weights);

        let primary_key = decode_key_spec(&mut rdr, path)?;
        let alt_count = usize::from(rdr.read_u16()?);
        if alt_count > MAX_ALTERNATE_KEYS {
            return Err(CoblangError::IsamStorage {
                path: String::from(path),
                page_id: 0,
                message: format!("corrupted header alternate key count {alt_count}"),
            });
        }
        let mut alternate_roots = Vec::with_capacity(alt_count);
        let mut alternate_keys = Vec::with_capacity(alt_count);
        for _ in 0..alt_count {
            let root = PageId::decode_optional(rdr.read_u64()?);
            let spec = decode_key_spec(&mut rdr, path)?;
            alternate_roots.push(root);
            alternate_keys.push(spec);
        }
        let schema = IsamSchema::try_new(record_bounds, primary_key, alternate_keys, collating)
            .map_err(|err| CoblangError::IsamStorage {
                path: String::from(path),
                page_id: 0,
                message: format!("corrupted header schema: {err}"),
            })?;
        Ok(Self {
            total_pages,
            free_head,
            record_count,
            next_dup_seq,
            primary_root,
            alternate_roots,
            schema,
        })
    }

    /// Allocates the next monotonic FIFO duplicate sequence number (`>= 1`).
    ///
    /// # Arguments
    ///
    /// * `path` - File path label for overflow diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if `next_dup_seq` reaches `u64::MAX`.
    pub fn alloc_dup_seq(&mut self, path: &str) -> Result<u64> {
        let current = self.next_dup_seq;
        self.next_dup_seq = current
            .checked_add(1)
            .ok_or_else(|| CoblangError::IsamStorage {
                path: String::from(path),
                page_id: 0,
                message: String::from("duplicate sequence u64 counter overflow"),
            })?;
        Ok(current)
    }

    /// Reads the root [`PageId`] of `key_id`.
    ///
    /// # Arguments
    ///
    /// * `key_id` - Primary or alternate key selector.
    #[must_use]
    pub fn tree_root(&self, key_id: KeyIndexId) -> Option<PageId> {
        match key_id.alternate_ordinal() {
            None => self.primary_root,
            Some(ord) => self.alternate_roots.get(ord).copied().flatten(),
        }
    }

    /// Updates the root [`PageId`] of `key_id`.
    ///
    /// # Arguments
    ///
    /// * `key_id` - Primary or alternate key selector.
    /// * `new_root` - Updated root page (`None` when the tree becomes empty).
    pub fn set_tree_root(&mut self, key_id: KeyIndexId, new_root: Option<PageId>) {
        match key_id.alternate_ordinal() {
            None => self.primary_root = new_root,
            Some(ord) => {
                if let Some(slot) = self.alternate_roots.get_mut(ord) {
                    *slot = new_root;
                }
            }
        }
    }
}

/// Appends a serialized [`KeySpec`] to `out`.
///
/// # Arguments
///
/// * `out` - Destination header payload byte buffer.
/// * `spec` - Key specification to encode.
fn encode_key_spec(out: &mut Vec<u8>, spec: &KeySpec) {
    out.push(spec.duplicates().to_tag());
    let seg_count = u8::try_from(spec.segments().len()).unwrap_or(0);
    out.push(seg_count);
    for seg in spec.segments() {
        out.extend_from_slice(&seg.offset().to_le_bytes());
        out.extend_from_slice(&seg.length().to_le_bytes());
    }
}

/// Decodes a single [`KeySpec`] from `rdr`.
///
/// # Arguments
///
/// * `rdr` - Header payload reader.
/// * `path` - File path label for error diagnostics.
fn decode_key_spec(rdr: &mut PayloadReader<'_, '_>, path: &str) -> Result<KeySpec> {
    let duplicates = DuplicateKeyPolicy::from_tag(rdr.read_u8()?, path, PageId::HEADER)?;
    let seg_count = usize::from(rdr.read_u8()?);
    let mut segments = Vec::with_capacity(seg_count.min(MAX_KEY_SEGMENTS + 1));
    for _ in 0..seg_count {
        let offset = rdr.read_u32()?;
        let length = rdr.read_u32()?;
        let seg =
            KeySegmentSpec::try_new(offset, length).map_err(|err| CoblangError::IsamStorage {
                path: String::from(path),
                page_id: 0,
                message: format!("corrupted key segment: {err}"),
            })?;
        segments.push(seg);
    }
    KeySpec::try_new(segments, duplicates).map_err(|err| CoblangError::IsamStorage {
        path: String::from(path),
        page_id: 0,
        message: format!("corrupted key spec: {err}"),
    })
}

/// Encodes a reclaimed [`PageKind::Free`] page pointing to `next_free`.
///
/// # Arguments
///
/// * `page_id` - Identifier of the freed page.
/// * `next_free` - Previous freelist head (`None` at tail).
#[must_use]
pub fn encode_free_page(page_id: PageId, next_free: Option<PageId>) -> [u8; PAGE_SIZE] {
    let payload = PageId::encode_optional(next_free).to_le_bytes();
    build_framed_page(PageKind::Free, page_id, &payload)
}

/// Verifies a [`PageKind::Free`] page and returns its `next_free` link.
///
/// # Arguments
///
/// * `raw_page` - Raw 4096-byte page buffer.
/// * `page_id` - Expected identifier of the free page.
/// * `path` - File path label for error diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::IsamStorage`] if the page frame or CRC32 is invalid.
pub fn decode_free_page(
    raw_page: &[u8; PAGE_SIZE],
    page_id: PageId,
    path: &str,
) -> Result<Option<PageId>> {
    verify_page_frame(raw_page, PageKind::Free, page_id, path)?;
    let mut rdr = PayloadReader::new(raw_page, path, page_id);
    Ok(PageId::decode_optional(rdr.read_u64()?))
}

/// Encodes a [`PageKind::RecordPayload`] overflow chunk page.
///
/// # Arguments
///
/// * `page_id` - Identifier of the payload page.
/// * `next_page` - Optional next overflow page in the chain.
/// * `chunk` - Slice of `1..=OVERFLOW_CHUNK_CAPACITY` payload bytes.
#[must_use]
pub fn encode_overflow_page(
    page_id: PageId,
    next_page: Option<PageId>,
    chunk: &[u8],
) -> [u8; PAGE_SIZE] {
    let mut payload = Vec::with_capacity(10 + chunk.len());
    payload.extend_from_slice(&PageId::encode_optional(next_page).to_le_bytes());
    let chunk_len = u16::try_from(chunk.len()).unwrap_or(0);
    payload.extend_from_slice(&chunk_len.to_le_bytes());
    payload.extend_from_slice(chunk);
    build_framed_page(PageKind::RecordPayload, page_id, &payload)
}

/// Decodes a verified [`PageKind::RecordPayload`] page into `(next_page, chunk_bytes)`.
///
/// # Arguments
///
/// * `raw_page` - Raw 4096-byte page buffer.
/// * `page_id` - Expected identifier of the overflow page.
/// * `path` - File path label for error diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::IsamStorage`] if the frame, CRC32, or `chunk_len` is invalid.
pub fn decode_overflow_page<'a>(
    raw_page: &'a [u8; PAGE_SIZE],
    page_id: PageId,
    path: &str,
) -> Result<(Option<PageId>, &'a [u8])> {
    verify_page_frame(raw_page, PageKind::RecordPayload, page_id, path)?;
    let mut rdr = PayloadReader::new(raw_page, path, page_id);
    let next_page = PageId::decode_optional(rdr.read_u64()?);
    let chunk_len = usize::from(rdr.read_u16()?);
    if chunk_len == 0 || chunk_len > OVERFLOW_CHUNK_CAPACITY {
        return Err(CoblangError::IsamStorage {
            path: String::from(path),
            page_id: page_id.as_u64(),
            message: format!("invalid overflow chunk length {chunk_len}"),
        });
    }
    let chunk = rdr.read_slice(chunk_len)?;
    Ok((next_page, chunk))
}

/// Abstract 4096-byte random-access block device backing a [`Pager`].
pub trait PageIo {
    /// Human-readable file path or identifier used in [`CoblangError::IsamStorage`] diagnostics.
    fn path_label(&self) -> &str;

    /// Returns the current number of physical 4096-byte pages stored in the device.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if querying device size fails or byte size is not a multiple of 4096.
    fn page_count(&mut self) -> Result<u64>;

    /// Reads an exact 4096-byte page at `page_id` into `buf`.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Zero-based target page identifier.
    /// * `buf` - Destination 4096-byte buffer.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `page_id` is out of bounds or reading fails.
    fn read_raw_page(&mut self, page_id: PageId, buf: &mut [u8; PAGE_SIZE]) -> Result<()>;

    /// Writes an exact 4096-byte page `buf` at `page_id`.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Zero-based target page identifier.
    /// * `buf` - Source 4096-byte page buffer.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if seeking or writing fails.
    fn write_raw_page(&mut self, page_id: PageId, buf: &[u8; PAGE_SIZE]) -> Result<()>;
}

/// OS filesystem [`PageIo`] implementation backed by [`std::fs::File`].
#[derive(Debug)]
pub struct DiskPageIo {
    /// Display path for diagnostics.
    path_label: String,
    /// Open random-access file handle.
    file: File,
}

impl DiskPageIo {
    /// Opens an existing `COBLISAM` file on disk for reading (and writing when `writable` is set).
    ///
    /// # Arguments
    ///
    /// * `path` - Filesystem path to open.
    /// * `writable` - [`DiskAccessPerm`] controlling read-only vs read-write file descriptor flags.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] if opening `path` fails.
    pub fn open_existing(path: &Path, writable: DiskAccessPerm) -> Result<Self> {
        let mut opts = OpenOptions::new();
        opts.read(true);
        if writable == DiskAccessPerm::ReadWrite {
            opts.write(true);
        }
        let file = opts.open(path)?;
        Ok(Self {
            path_label: path.display().to_string(),
            file,
        })
    }

    /// Creates or truncates a `COBLISAM` file on disk with read-write permissions.
    ///
    /// # Arguments
    ///
    /// * `path` - Filesystem path to create or truncate.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] if creating or truncating `path` fails.
    pub fn create_truncated(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            path_label: path.display().to_string(),
            file,
        })
    }

    /// Wraps an already-opened [`File`] with an explicit diagnostic `path_label`.
    ///
    /// # Arguments
    ///
    /// * `path_label` - Diagnostic path string.
    /// * `file` - Open OS file handle.
    #[must_use]
    pub const fn from_file(path_label: String, file: File) -> Self {
        Self { path_label, file }
    }

    /// Computes the byte offset `page_id * 4096` with overflow protection.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Target page identifier.
    fn byte_offset(&self, page_id: PageId) -> Result<u64> {
        page_id
            .as_u64()
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| CoblangError::IsamStorage {
                path: self.path_label.clone(),
                page_id: page_id.as_u64(),
                message: String::from("page byte offset overflows u64"),
            })
    }
}

/// Read-only vs read-write OS descriptor permission selector for [`DiskPageIo::open_existing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiskAccessPerm {
    /// Open file descriptor with `O_RDONLY` (`OPEN INPUT`).
    ReadOnly,
    /// Open file descriptor with `O_RDWR` (`OPEN I-O` / `OPEN EXTEND`).
    ReadWrite,
}

impl PageIo for DiskPageIo {
    /// Returns the filesystem path label of this open file.
    fn path_label(&self) -> &str {
        &self.path_label
    }

    /// Computes the number of 4096-byte pages from the OS file end offset.
    fn page_count(&mut self) -> Result<u64> {
        let byte_len = self.file.seek(SeekFrom::End(0))?;
        if byte_len % (PAGE_SIZE as u64) != 0 {
            return Err(CoblangError::IsamStorage {
                path: self.path_label.clone(),
                page_id: byte_len / (PAGE_SIZE as u64),
                message: format!("file byte length {byte_len} is not a multiple of {PAGE_SIZE}"),
            });
        }
        Ok(byte_len / (PAGE_SIZE as u64))
    }

    /// Seeks to `page_id * 4096` and reads an exact 4096-byte page into `buf`.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Zero-based target page identifier.
    /// * `buf` - Destination 4096-byte buffer.
    fn read_raw_page(&mut self, page_id: PageId, buf: &mut [u8; PAGE_SIZE]) -> Result<()> {
        let offset = self.byte_offset(page_id)?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)?;
        Ok(())
    }

    /// Seeks to `page_id * 4096` and writes `buf` to disk.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Zero-based target page identifier.
    /// * `buf` - Source 4096-byte buffer.
    fn write_raw_page(&mut self, page_id: PageId, buf: &[u8; PAGE_SIZE]) -> Result<()> {
        let offset = self.byte_offset(page_id)?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(buf)?;
        Ok(())
    }
}

/// Cloneable handle to an in-memory virtual file supporting `OPEN`/`CLOSE` persistence.
#[derive(Debug, Clone, Default)]
pub struct MemoryDisk {
    /// Diagnostic label for the virtual file.
    label: String,
    /// `None` when the virtual file does not yet exist; `Some(pages)` once created.
    image: Rc<RefCell<Option<Vec<[u8; PAGE_SIZE]>>>>,
}

impl MemoryDisk {
    /// Creates an uncreated (`!exists()`) virtual file with `label`.
    ///
    /// # Arguments
    ///
    /// * `label` - Virtual path label used in error diagnostics.
    #[must_use]
    pub fn uncreated(label: &str) -> Self {
        Self {
            label: String::from(label),
            image: Rc::new(RefCell::new(None)),
        }
    }

    /// Creates an existing empty (`0`-page) virtual file with `label`.
    ///
    /// # Arguments
    ///
    /// * `label` - Virtual path label used in error diagnostics.
    #[must_use]
    pub fn new(label: &str) -> Self {
        Self {
            label: String::from(label),
            image: Rc::new(RefCell::new(Some(Vec::new()))),
        }
    }

    /// Returns `true` if the virtual file has been created.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.image.borrow().is_some()
    }

    /// Truncates or creates the virtual file image to zero pages.
    pub fn create_truncated(&self) {
        *self.image.borrow_mut() = Some(Vec::new());
    }

    /// Returns the number of pages currently stored (`0` when uncreated).
    #[must_use]
    pub fn page_len(&self) -> usize {
        self.image.borrow().as_ref().map_or(0, Vec::len)
    }

    /// Mutates a raw 4096-byte page in place (used by corruption tests).
    ///
    /// # Arguments
    ///
    /// * `page_id` - Target zero-based page identifier.
    /// * `mutate` - Callback receiving `&mut [u8; PAGE_SIZE]`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if the virtual file is uncreated or `page_id` is out of range.
    pub fn mutate_raw_page(
        &self,
        page_id: PageId,
        mutate: &mut dyn FnMut(&mut [u8; PAGE_SIZE]),
    ) -> Result<()> {
        let mut guard = self.image.borrow_mut();
        let idx = usize::try_from(page_id.as_u64()).unwrap_or(usize::MAX);
        let page = guard
            .as_mut()
            .and_then(|pages| pages.get_mut(idx))
            .ok_or_else(|| CoblangError::IsamStorage {
                path: self.label.clone(),
                page_id: page_id.as_u64(),
                message: String::from("cannot mutate missing in-memory page"),
            })?;
        mutate(page);
        Ok(())
    }

    /// Opens a [`MemoryPageIo`] view over this existing virtual file.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if the virtual file has not been created yet.
    pub fn open_io(&self) -> Result<MemoryPageIo> {
        if !self.exists() {
            return Err(CoblangError::IsamStorage {
                path: self.label.clone(),
                page_id: 0,
                message: String::from("in-memory ISAM file does not exist"),
            });
        }
        Ok(MemoryPageIo { disk: self.clone() })
    }
}

/// [`PageIo`] implementation backed by a shared [`MemoryDisk`].
#[derive(Debug, Clone)]
pub struct MemoryPageIo {
    /// Underlying shared virtual file.
    disk: MemoryDisk,
}

impl MemoryPageIo {
    /// Creates a fresh created [`MemoryPageIo`] with `label`.
    ///
    /// # Arguments
    ///
    /// * `label` - Diagnostic label for the in-memory store.
    #[must_use]
    pub fn new(label: &str) -> Self {
        let disk = MemoryDisk::new(label);
        Self { disk }
    }

    /// Wraps an existing [`MemoryDisk`] handle directly.
    ///
    /// # Arguments
    ///
    /// * `disk` - Shared virtual file handle.
    #[must_use]
    pub const fn from_disk(disk: MemoryDisk) -> Self {
        Self { disk }
    }

    /// Returns a cloneable handle to the underlying [`MemoryDisk`].
    #[must_use]
    pub fn disk(&self) -> MemoryDisk {
        self.disk.clone()
    }
}

impl PageIo for MemoryPageIo {
    /// Returns the virtual path label of the underlying [`MemoryDisk`].
    fn path_label(&self) -> &str {
        &self.disk.label
    }

    /// Returns the number of 4096-byte pages currently stored in memory.
    fn page_count(&mut self) -> Result<u64> {
        let guard = self.disk.image.borrow();
        let pages = guard.as_ref().ok_or_else(|| CoblangError::IsamStorage {
            path: self.disk.label.clone(),
            page_id: 0,
            message: String::from("in-memory ISAM file is uncreated"),
        })?;
        Ok(pages.len() as u64)
    }

    /// Copies the 4096-byte page at `page_id` into `buf`.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Zero-based target page identifier.
    /// * `buf` - Destination 4096-byte buffer.
    fn read_raw_page(&mut self, page_id: PageId, buf: &mut [u8; PAGE_SIZE]) -> Result<()> {
        let guard = self.disk.image.borrow();
        let idx = usize::try_from(page_id.as_u64()).unwrap_or(usize::MAX);
        let src = guard
            .as_ref()
            .and_then(|pages| pages.get(idx))
            .ok_or_else(|| CoblangError::IsamStorage {
                path: self.disk.label.clone(),
                page_id: page_id.as_u64(),
                message: String::from("read past end of in-memory ISAM file"),
            })?;
        *buf = *src;
        Ok(())
    }

    /// Writes `buf` into `page_id` (overwriting an existing page or appending at `pages.len()`).
    ///
    /// # Arguments
    ///
    /// * `page_id` - Zero-based target page identifier.
    /// * `buf` - Source 4096-byte buffer.
    fn write_raw_page(&mut self, page_id: PageId, buf: &[u8; PAGE_SIZE]) -> Result<()> {
        let mut guard = self.disk.image.borrow_mut();
        let pages = guard.as_mut().ok_or_else(|| CoblangError::IsamStorage {
            path: self.disk.label.clone(),
            page_id: page_id.as_u64(),
            message: String::from("write to uncreated in-memory ISAM file"),
        })?;
        let idx = usize::try_from(page_id.as_u64())
            .ok()
            .filter(|&i| i <= pages.len())
            .ok_or_else(|| CoblangError::IsamStorage {
                path: self.disk.label.clone(),
                page_id: page_id.as_u64(),
                message: String::from("non-contiguous page write in MemoryPageIo"),
            })?;
        if let Some(slot) = pages.get_mut(idx) {
            *slot = *buf;
        } else {
            pages.push(*buf);
        }
        Ok(())
    }
}

/// Transactional 4096-byte page manager handling CRC32 verification, freelist reuse, and overflow chains.
pub struct Pager {
    /// Dynamic block device (`DiskPageIo`, `MemoryPageIo`, or test fault-injector).
    io: Box<dyn PageIo>,
    /// Live (or in-flight transaction) file header state.
    header: FileHeaderState,
    /// Snapshot of `header` taken at `begin_tx` for atomic rollback on error.
    saved_header: Option<FileHeaderState>,
    /// Dirty non-header pages staged during the active operation and flushed atomically in `commit_tx`.
    staged_pages: BTreeMap<PageId, [u8; PAGE_SIZE]>,
}

impl Pager {
    /// Initializes a brand-new `COBLISAM` file on `io` and flushes `PageId::HEADER`.
    ///
    /// # Arguments
    ///
    /// * `io` - Empty/truncated block storage backend.
    /// * `schema` - Validated [`IsamSchema`] written to page `0`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if writing `PageId::HEADER` to `io` fails.
    pub fn create_new(mut io: Box<dyn PageIo>, schema: IsamSchema) -> Result<Self> {
        let header = FileHeaderState::new_empty(schema);
        let raw_hdr = header.encode_page();
        io.write_raw_page(PageId::HEADER, &raw_hdr)?;
        Ok(Self {
            io,
            header,
            saved_header: None,
            staged_pages: BTreeMap::new(),
        })
    }

    /// Opens an existing `COBLISAM` file on `io`, verifying page alignment and `PageId::HEADER` CRC32.
    ///
    /// # Arguments
    ///
    /// * `io` - Existing block storage backend.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if the file is empty, misaligned, truncated, or fails header CRC32/schema checks.
    pub fn open_existing(mut io: Box<dyn PageIo>) -> Result<Self> {
        let physical_pages = io.page_count()?;
        if physical_pages == 0 {
            return Err(CoblangError::IsamStorage {
                path: String::from(io.path_label()),
                page_id: 0,
                message: String::from("ISAM file contains 0 pages"),
            });
        }
        let mut raw_hdr = [0_u8; PAGE_SIZE];
        io.read_raw_page(PageId::HEADER, &mut raw_hdr)?;
        let header = FileHeaderState::decode_page(&raw_hdr, io.path_label())?;
        if header.total_pages > physical_pages {
            return Err(CoblangError::IsamStorage {
                path: String::from(io.path_label()),
                page_id: 0,
                message: format!(
                    "truncated ISAM file: header expects {} pages, found {physical_pages}",
                    header.total_pages
                ),
            });
        }
        Ok(Self {
            io,
            header,
            saved_header: None,
            staged_pages: BTreeMap::new(),
        })
    }

    /// Returns the storage path label for error reporting.
    #[must_use]
    pub fn path_label(&self) -> &str {
        self.io.path_label()
    }

    /// Returns an immutable reference to the active [`FileHeaderState`].
    #[must_use]
    pub const fn header(&self) -> &FileHeaderState {
        &self.header
    }

    /// Returns a mutable reference to the active [`FileHeaderState`].
    pub const fn header_mut(&mut self) -> &mut FileHeaderState {
        &mut self.header
    }

    /// Captures a rollback checkpoint prior to a mutating verb (`WRITE`, `REWRITE`, `DELETE`).
    pub fn begin_tx(&mut self) {
        self.saved_header = Some(self.header.clone());
        self.staged_pages.clear();
    }

    /// Discards all staged page mutations and restores `self.header` to the pre-verb checkpoint.
    pub fn rollback_tx(&mut self) {
        if let Some(prev) = self.saved_header.take() {
            self.header = prev;
        }
        self.staged_pages.clear();
    }

    /// Flushes all staged pages in ascending `PageId` order followed by `PageId::HEADER`.
    ///
    /// If an I/O write error occurs, `self.header` is rolled back to `saved_header`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if writing any staged page or `PageId::HEADER` fails.
    pub fn commit_tx(&mut self) -> Result<()> {
        let pending: Vec<(PageId, [u8; PAGE_SIZE])> = self
            .staged_pages
            .iter()
            .map(|(&pid, &raw)| (pid, raw))
            .collect();
        for (pid, raw) in &pending {
            if let Err(err) = self.io.write_raw_page(*pid, raw) {
                self.rollback_tx();
                return Err(err);
            }
        }
        let raw_hdr = self.header.encode_page();
        if let Err(err) = self.io.write_raw_page(PageId::HEADER, &raw_hdr) {
            self.rollback_tx();
            return Err(err);
        }
        self.saved_header = None;
        self.staged_pages.clear();
        Ok(())
    }

    /// Reads and frame/CRC32-verifies `page_id`, returning its [`PageKind`] and raw 4096-byte buffer.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Target page identifier.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `page_id >= total_pages`, I/O fails, or frame/CRC32 verification fails.
    pub fn read_any_verified_page(
        &mut self,
        page_id: PageId,
    ) -> Result<(PageKind, [u8; PAGE_SIZE])> {
        if page_id.as_u64() >= self.header.total_pages {
            return Err(CoblangError::IsamStorage {
                path: String::from(self.io.path_label()),
                page_id: page_id.as_u64(),
                message: format!(
                    "page_id {} out of allocated range (total_pages={})",
                    page_id.as_u64(),
                    self.header.total_pages
                ),
            });
        }
        let raw = if let Some(&staged) = self.staged_pages.get(&page_id) {
            staged
        } else {
            let mut buf = [0_u8; PAGE_SIZE];
            self.io.read_raw_page(page_id, &mut buf)?;
            buf
        };
        let kind = verify_any_page_frame(&raw, page_id, self.io.path_label())?;
        Ok((kind, raw))
    }

    /// Reads and frame/CRC32-verifies `page_id` against `expected_kind` (consulting `staged_pages` first).
    ///
    /// # Arguments
    ///
    /// * `page_id` - Target non-header or header page identifier.
    /// * `expected_kind` - Required [`PageKind`] of `page_id`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `page_id >= total_pages`, I/O fails, or frame/CRC32/kind verification fails.
    pub fn read_verified_page(
        &mut self,
        page_id: PageId,
        expected_kind: PageKind,
    ) -> Result<[u8; PAGE_SIZE]> {
        let (kind, raw) = self.read_any_verified_page(page_id)?;
        if kind != expected_kind {
            return Err(CoblangError::IsamStorage {
                path: String::from(self.io.path_label()),
                page_id: page_id.as_u64(),
                message: format!("expected page kind {expected_kind:?}, found {kind:?}"),
            });
        }
        Ok(raw)
    }

    /// Stages a framed 4096-byte page in memory for atomic write at `commit_tx`.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Destination non-header page identifier.
    /// * `raw_page` - Complete CRC32-stamped 4096-byte page buffer.
    pub fn stage_raw_page(&mut self, page_id: PageId, raw_page: [u8; PAGE_SIZE]) {
        self.staged_pages.insert(page_id, raw_page);
    }

    /// Allocates a [`PageId`], reusing `header.free_head` if available or extending `total_pages`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if reading a reclaimed freelist page fails or `total_pages` overflows `u64`.
    pub fn alloc_page(&mut self) -> Result<PageId> {
        if let Some(free_id) = self.header.free_head {
            let raw = self.read_verified_page(free_id, PageKind::Free)?;
            let next_free = decode_free_page(&raw, free_id, self.io.path_label())?;
            self.header.free_head = next_free;
            return Ok(free_id);
        }
        let fresh = PageId::new(self.header.total_pages);
        let next_total = fresh.checked_next(self.io.path_label())?;
        self.header.total_pages = next_total.as_u64();
        Ok(fresh)
    }

    /// Pushes `page_id` onto the LIFO freelist (`PageKind::Free`) inside the active transaction.
    ///
    /// # Arguments
    ///
    /// * `page_id` - Non-header [`PageId`] being reclaimed.
    pub fn free_page(&mut self, page_id: PageId) {
        let prev_head = self.header.free_head;
        let raw = encode_free_page(page_id, prev_head);
        self.staged_pages.insert(page_id, raw);
        self.header.free_head = Some(page_id);
    }

    /// Stores a large byte slice across one or more chained [`PageKind::RecordPayload`] pages.
    ///
    /// # Arguments
    ///
    /// * `payload` - Non-empty byte slice to store across overflow pages.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `payload` is empty or allocating any page fails.
    pub fn write_overflow_chain(&mut self, payload: &[u8]) -> Result<PageId> {
        if payload.is_empty() {
            return Err(CoblangError::IsamStorage {
                path: String::from(self.io.path_label()),
                page_id: 0,
                message: String::from("overflow payload must not be empty"),
            });
        }
        let chunks: Vec<&[u8]> = payload.chunks(OVERFLOW_CHUNK_CAPACITY).collect();
        let mut page_ids = Vec::with_capacity(chunks.len());
        for _ in 0..chunks.len() {
            page_ids.push(self.alloc_page()?);
        }
        for (idx, (&pid, &chunk)) in page_ids.iter().zip(chunks.iter()).enumerate() {
            let next_id = page_ids.get(idx + 1).copied();
            let raw = encode_overflow_page(pid, next_id, chunk);
            self.stage_raw_page(pid, raw);
        }
        Ok(page_ids.first().copied().unwrap_or(PageId::HEADER))
    }

    /// Reassembles `expected_len` bytes from a [`PageKind::RecordPayload`] chain starting at `first_page`.
    ///
    /// # Arguments
    ///
    /// * `first_page` - Head [`PageId`] of the overflow chain.
    /// * `expected_len` - Exact expected reassembled byte count.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] on I/O failure, checksum corruption, chain cycle, or length mismatch.
    pub fn read_overflow_chain(
        &mut self,
        first_page: PageId,
        expected_len: u32,
    ) -> Result<Vec<u8>> {
        let target_len = usize::try_from(expected_len).unwrap_or(usize::MAX);
        let max_hops = self.header.total_pages;
        let mut out = Vec::with_capacity(target_len.min(65_536));
        let mut cur = Some(first_page);
        let mut hops = 0_u64;
        while let Some(pid) = cur {
            hops += 1;
            if hops > max_hops {
                return Err(CoblangError::IsamStorage {
                    path: String::from(self.io.path_label()),
                    page_id: pid.as_u64(),
                    message: String::from("cycle detected in RecordPayload overflow chain"),
                });
            }
            let raw = self.read_verified_page(pid, PageKind::RecordPayload)?;
            let (next_page, chunk) = decode_overflow_page(&raw, pid, self.io.path_label())?;
            out.extend_from_slice(chunk);
            cur = next_page;
        }
        if out.len() != target_len {
            return Err(CoblangError::IsamStorage {
                path: String::from(self.io.path_label()),
                page_id: first_page.as_u64(),
                message: format!(
                    "overflow chain length mismatch: expected {target_len} bytes, got {}",
                    out.len()
                ),
            });
        }
        Ok(out)
    }

    /// Reclaims every [`PageKind::RecordPayload`] page in the chain starting at `first_page`.
    ///
    /// # Arguments
    ///
    /// * `first_page` - Head [`PageId`] of the overflow chain to free.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::IsamStorage`] if any overflow page is unreadable/corrupted or forms a cycle.
    pub fn free_overflow_chain(&mut self, first_page: PageId) -> Result<()> {
        let max_hops = self.header.total_pages;
        let mut to_free = Vec::new();
        let mut cur = Some(first_page);
        let mut hops = 0_u64;
        while let Some(pid) = cur {
            hops += 1;
            if hops > max_hops {
                return Err(CoblangError::IsamStorage {
                    path: String::from(self.io.path_label()),
                    page_id: pid.as_u64(),
                    message: String::from("cycle detected while freeing overflow chain"),
                });
            }
            let raw = self.read_verified_page(pid, PageKind::RecordPayload)?;
            let (next_page, _) = decode_overflow_page(&raw, pid, self.io.path_label())?;
            to_free.push(pid);
            cur = next_page;
        }
        for pid in to_free {
            self.free_page(pid);
        }
        Ok(())
    }
}
