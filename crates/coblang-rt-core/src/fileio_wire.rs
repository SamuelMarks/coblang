//! Binary wire codec and ABI dispatch constants shared between `coblang-mlir::lower_m14` and `coblang-rt-abi::fileio_abi`.

use coblang_common::{CoblangError, Result};
use coblang_hir::{
    HirAltKeySuppression, HirDuplicateKeyPolicy, HirFileAccessMode, HirFileLockMode,
    HirFileOptionality, HirFileOrganization, HirFileSharingMode, HirKeySegment,
    HirPrintAdvancingMode, HirRecordFormat,
};

/// 4-byte magic prefix identifying serialized [`FileConnectorWireSpec`] payloads.
const FILE_WIRE_MAGIC: &[u8; 4] = b"CBFC";

/// Post-I/O ABI return code: normal `0x` completion with no conditional branch.
pub const POST_IO_CONTINUE: i32 = 0;
/// Post-I/O ABI return code: execute inline `AT END` statements.
pub const POST_IO_FIRE_AT_END: i32 = 1;
/// Post-I/O ABI return code: execute inline `NOT AT END` statements.
pub const POST_IO_FIRE_NOT_AT_END: i32 = 2;
/// Post-I/O ABI return code: execute inline `INVALID KEY` / `ON EXCEPTION` statements.
pub const POST_IO_FIRE_INVALID_KEY: i32 = 3;
/// Post-I/O ABI return code: execute inline `NOT INVALID KEY` / `NOT ON EXCEPTION` statements.
pub const POST_IO_FIRE_NOT_INVALID_KEY: i32 = 4;
/// Post-I/O ABI return code: execute inline `AT END-OF-PAGE` statements.
pub const POST_IO_FIRE_AT_EOP: i32 = 5;
/// Post-I/O ABI return code: execute inline `NOT AT END-OF-PAGE` statements.
pub const POST_IO_FIRE_NOT_AT_EOP: i32 = 6;
/// Post-I/O ABI return code: invoke specific `USE AFTER ERROR PROCEDURE ON file-name` declarative.
pub const POST_IO_INVOKE_DECL_FILE: i32 = 7;
/// Post-I/O ABI return code: invoke `USE AFTER ERROR PROCEDURE ON INPUT` declarative.
pub const POST_IO_INVOKE_DECL_INPUT: i32 = 8;
/// Post-I/O ABI return code: invoke `USE AFTER ERROR PROCEDURE ON OUTPUT` declarative.
pub const POST_IO_INVOKE_DECL_OUTPUT: i32 = 9;
/// Post-I/O ABI return code: invoke `USE AFTER ERROR PROCEDURE ON I-O` declarative.
pub const POST_IO_INVOKE_DECL_IO: i32 = 10;
/// Post-I/O ABI return code: invoke `USE AFTER ERROR PROCEDURE ON EXTEND` declarative.
pub const POST_IO_INVOKE_DECL_EXTEND: i32 = 11;
/// Post-I/O ABI return code: fatal unhandled file I/O error (abort run unit).
pub const POST_IO_FATAL_ABORT: i32 = 12;

/// Declarative presence bitmask bit `0`: `ON file-name` declarative present.
pub const DECL_MASK_FILE: i32 = 1 << 0;
/// Declarative presence bitmask bit `1`: `ON INPUT` declarative present.
pub const DECL_MASK_INPUT: i32 = 1 << 1;
/// Declarative presence bitmask bit `2`: `ON OUTPUT` declarative present.
pub const DECL_MASK_OUTPUT: i32 = 1 << 2;
/// Declarative presence bitmask bit `3`: `ON I-O` declarative present.
pub const DECL_MASK_IO: i32 = 1 << 3;
/// Declarative presence bitmask bit `4`: `ON EXTEND` declarative present.
pub const DECL_MASK_EXTEND: i32 = 1 << 4;

/// Run-unit connector identity inside [`FileConnectorWireSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WireConnectorIdentity {
    /// Unit-scoped (or `IS GLOBAL`) file connector `(owner_unit, owner_fc)`.
    UnitOwned {
        /// Defining unit index.
        owner_unit: u32,
        /// Defining connector index within `owner_unit`.
        owner_fc: u32,
    },
    /// Run-unit-wide `FD ... IS EXTERNAL` connector keyed by canonical name.
    External(String),
}

/// Static `ASSIGN TO` specification inside [`FileConnectorWireSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireAssignKind {
    /// Static string literal (`ASSIGN TO "path"`).
    Literal(String),
    /// Explicit environment variable (`ASSIGN TO EXTERNAL name`).
    ExternalEnv(String),
    /// Dynamic runtime buffer passed at `OPEN` (`ASSIGN TO [DYNAMIC] var`).
    DynamicBuffer,
    /// Unqualified identifier (`DD_name` / `name` / literal fallback).
    EnvOrLiteral(String),
    /// Standard output console (`DISPLAY`).
    DisplayDevice,
    /// Standard input console (`KEYBOARD`).
    KeyboardDevice,
}

/// Serialized alternate key specification inside [`FileConnectorWireSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireAltKey {
    /// Contiguous or split key byte segments.
    pub segments: Vec<HirKeySegment>,
    /// Duplicate-key admission policy.
    pub duplicates: HirDuplicateKeyPolicy,
    /// Optional sparse-index suppression rule.
    pub suppression: Option<HirAltKeySuppression>,
}

/// Complete static schema of a COBOL file connector passed across the C-ABI boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileConnectorWireSpec {
    /// Run-unit connector identity.
    pub identity: WireConnectorIdentity,
    /// Diagnostic COBOL connector name.
    pub file_name: String,
    /// File organization.
    pub organization: HirFileOrganization,
    /// File access mode.
    pub access_mode: HirFileAccessMode,
    /// `SELECT OPTIONAL` flag.
    pub optionality: HirFileOptionality,
    /// Static `ASSIGN TO` kind.
    pub assign: WireAssignKind,
    /// Fixed vs variable record format.
    pub record_format: HirRecordFormat,
    /// Minimum record byte size (`>= 1`).
    pub min_record_bytes: u32,
    /// Maximum record byte size (`>= min_record_bytes`).
    pub max_record_bytes: u32,
    /// Non-zero decimal digit capacity of `RELATIVE KEY` (`0` when unbounded / non-relative).
    pub relative_key_digits: u8,
    /// Connector-level `LOCK MODE`.
    pub lock_mode: HirFileLockMode,
    /// Optional connector-level `SHARING WITH` mode.
    pub sharing_mode: Option<HirFileSharingMode>,
    /// Record padding byte (`b' '` by default).
    pub padding_byte: u8,
    /// Raw binary vs line-advancing sequential framing.
    pub print_mode: HirPrintAdvancingMode,
    /// Optional 256-byte custom `COLLATING SEQUENCE` rank table.
    pub collating_ranks: Option<[u8; 256]>,
    /// Primary `RECORD KEY` segments (for `Indexed`).
    pub primary_key_segments: Vec<HirKeySegment>,
    /// Secondary `ALTERNATE RECORD KEY` specifications (for `Indexed`).
    pub alternate_keys: Vec<WireAltKey>,
}

/// Encodes a [`FileConnectorWireSpec`] into a compact deterministic byte vector.
///
/// # Arguments
///
/// * `spec` - Connector wire descriptor to serialize.
#[must_use]
pub fn encode_file_connector_spec(spec: &FileConnectorWireSpec) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    out.extend_from_slice(FILE_WIRE_MAGIC);

    match &spec.identity {
        WireConnectorIdentity::UnitOwned {
            owner_unit,
            owner_fc,
        } => {
            out.push(0);
            out.extend_from_slice(&owner_unit.to_le_bytes());
            out.extend_from_slice(&owner_fc.to_le_bytes());
        }
        WireConnectorIdentity::External(name) => {
            out.push(1);
            push_str(&mut out, name);
        }
    }
    push_str(&mut out, &spec.file_name);

    out.push(match spec.organization {
        HirFileOrganization::Sequential => 0,
        HirFileOrganization::LineSequential => 1,
        HirFileOrganization::Relative => 2,
        HirFileOrganization::Indexed => 3,
    });
    out.push(match spec.access_mode {
        HirFileAccessMode::Sequential => 0,
        HirFileAccessMode::Random => 1,
        HirFileAccessMode::Dynamic => 2,
    });
    out.push(match spec.optionality {
        HirFileOptionality::Required => 0,
        HirFileOptionality::Optional => 1,
    });

    match &spec.assign {
        WireAssignKind::Literal(s) => {
            out.push(0);
            push_str(&mut out, s);
        }
        WireAssignKind::ExternalEnv(s) => {
            out.push(1);
            push_str(&mut out, s);
        }
        WireAssignKind::DynamicBuffer => out.push(2),
        WireAssignKind::EnvOrLiteral(s) => {
            out.push(3);
            push_str(&mut out, s);
        }
        WireAssignKind::DisplayDevice => out.push(4),
        WireAssignKind::KeyboardDevice => out.push(5),
    }

    out.push(match spec.record_format {
        HirRecordFormat::Fixed => 0,
        HirRecordFormat::Varying => 1,
    });
    out.extend_from_slice(&spec.min_record_bytes.to_le_bytes());
    out.extend_from_slice(&spec.max_record_bytes.to_le_bytes());
    out.push(spec.relative_key_digits);
    out.push(match spec.lock_mode {
        HirFileLockMode::Default => 0,
        HirFileLockMode::Manual => 1,
        HirFileLockMode::Automatic => 2,
        HirFileLockMode::Exclusive => 3,
    });
    out.push(match spec.sharing_mode {
        None => 0,
        Some(HirFileSharingMode::AllOther) => 1,
        Some(HirFileSharingMode::NoOther) => 2,
        Some(HirFileSharingMode::ReadOnly) => 3,
    });
    out.push(spec.padding_byte);
    out.push(match spec.print_mode {
        HirPrintAdvancingMode::RawBinary => 0,
        HirPrintAdvancingMode::LineAdvancing => 1,
    });

    if let Some(ranks) = &spec.collating_ranks {
        out.push(1);
        out.extend_from_slice(ranks);
    } else {
        out.push(0);
    }

    push_segments(&mut out, &spec.primary_key_segments);
    push_alt_keys(&mut out, &spec.alternate_keys);
    out
}

/// Appends serialized [`WireAltKey`] entries to `out`.
///
/// # Arguments
///
/// * `out` - Destination byte buffer.
/// * `alt_keys` - Alternate key descriptors.
fn push_alt_keys(out: &mut Vec<u8>, alt_keys: &[WireAltKey]) {
    let alt_cnt = u16::try_from(alt_keys.len()).unwrap_or(0);
    out.extend_from_slice(&alt_cnt.to_le_bytes());
    for ak in alt_keys.iter().take(usize::from(alt_cnt)) {
        push_segments(out, &ak.segments);
        out.push(match ak.duplicates {
            HirDuplicateKeyPolicy::Unique => 0,
            HirDuplicateKeyPolicy::WithDuplicates => 1,
        });
        match &ak.suppression {
            None => out.push(0),
            Some(HirAltKeySuppression::WhenAllByte(b)) => {
                out.push(1);
                out.push(*b);
            }
            Some(HirAltKeySuppression::WhenExactBytes(bs)) => {
                out.push(2);
                let n = u16::try_from(bs.len()).unwrap_or(0);
                out.extend_from_slice(&n.to_le_bytes());
                out.extend_from_slice(bs.get(..usize::from(n)).unwrap_or(&[]));
            }
        }
    }
}

/// Decodes a [`FileConnectorWireSpec`] from bytes produced by [`encode_file_connector_spec`].
///
/// # Arguments
///
/// * `bytes` - Serialized wire payload.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidDomainValue`] if `bytes` is truncated or has an invalid magic/tag.
pub fn decode_file_connector_spec(bytes: &[u8]) -> Result<FileConnectorWireSpec> {
    let mut cur = WireCursor { bytes, pos: 0 };
    let magic = cur.take(4)?;
    if magic != FILE_WIRE_MAGIC {
        return Err(wire_err("invalid CBFC magic header"));
    }

    let identity = match cur.u8()? {
        0 => WireConnectorIdentity::UnitOwned {
            owner_unit: cur.u32()?,
            owner_fc: cur.u32()?,
        },
        1 => WireConnectorIdentity::External(cur.str()?),
        _ => return Err(wire_err("invalid identity tag")),
    };
    let file_name = cur.str()?;

    let organization = match cur.u8()? {
        0 => HirFileOrganization::Sequential,
        1 => HirFileOrganization::LineSequential,
        2 => HirFileOrganization::Relative,
        3 => HirFileOrganization::Indexed,
        _ => return Err(wire_err("invalid organization tag")),
    };
    let access_mode = match cur.u8()? {
        0 => HirFileAccessMode::Sequential,
        1 => HirFileAccessMode::Random,
        2 => HirFileAccessMode::Dynamic,
        _ => return Err(wire_err("invalid access_mode tag")),
    };
    let optionality = match cur.u8()? {
        0 => HirFileOptionality::Required,
        1 => HirFileOptionality::Optional,
        _ => return Err(wire_err("invalid optionality tag")),
    };
    let assign = match cur.u8()? {
        0 => WireAssignKind::Literal(cur.str()?),
        1 => WireAssignKind::ExternalEnv(cur.str()?),
        2 => WireAssignKind::DynamicBuffer,
        3 => WireAssignKind::EnvOrLiteral(cur.str()?),
        4 => WireAssignKind::DisplayDevice,
        5 => WireAssignKind::KeyboardDevice,
        _ => return Err(wire_err("invalid assign tag")),
    };
    let record_format = match cur.u8()? {
        0 => HirRecordFormat::Fixed,
        1 => HirRecordFormat::Varying,
        _ => return Err(wire_err("invalid record_format tag")),
    };
    let min_record_bytes = cur.u32()?;
    let max_record_bytes = cur.u32()?;
    let relative_key_digits = cur.u8()?;
    let lock_mode = match cur.u8()? {
        0 => HirFileLockMode::Default,
        1 => HirFileLockMode::Manual,
        2 => HirFileLockMode::Automatic,
        3 => HirFileLockMode::Exclusive,
        _ => return Err(wire_err("invalid lock_mode tag")),
    };
    let sharing_mode = match cur.u8()? {
        0 => None,
        1 => Some(HirFileSharingMode::AllOther),
        2 => Some(HirFileSharingMode::NoOther),
        3 => Some(HirFileSharingMode::ReadOnly),
        _ => return Err(wire_err("invalid sharing_mode tag")),
    };
    let padding_byte = cur.u8()?;
    let print_mode = match cur.u8()? {
        0 => HirPrintAdvancingMode::RawBinary,
        1 => HirPrintAdvancingMode::LineAdvancing,
        _ => return Err(wire_err("invalid print_mode tag")),
    };

    let collating_ranks = match cur.u8()? {
        0 => None,
        1 => {
            let s = cur.take(256)?;
            let mut arr = [0_u8; 256];
            arr.copy_from_slice(s);
            Some(arr)
        }
        _ => return Err(wire_err("invalid collation tag")),
    };

    let primary_key_segments = cur.segments()?;
    let alternate_keys = decode_alt_keys(&mut cur)?;

    Ok(FileConnectorWireSpec {
        identity,
        file_name,
        organization,
        access_mode,
        optionality,
        assign,
        record_format,
        min_record_bytes,
        max_record_bytes,
        relative_key_digits,
        lock_mode,
        sharing_mode,
        padding_byte,
        print_mode,
        collating_ranks,
        primary_key_segments,
        alternate_keys,
    })
}

/// Decodes [`WireAltKey`] entries from `cur`.
///
/// # Arguments
///
/// * `cur` - Active wire cursor.
fn decode_alt_keys(cur: &mut WireCursor<'_>) -> Result<Vec<WireAltKey>> {
    let alt_cnt = usize::from(cur.u16()?);
    let mut alternate_keys = Vec::with_capacity(alt_cnt);
    for _ in 0..alt_cnt {
        let segments = cur.segments()?;
        let duplicates = match cur.u8()? {
            0 => HirDuplicateKeyPolicy::Unique,
            1 => HirDuplicateKeyPolicy::WithDuplicates,
            _ => return Err(wire_err("invalid duplicate policy tag")),
        };
        let suppression = match cur.u8()? {
            0 => None,
            1 => Some(HirAltKeySuppression::WhenAllByte(cur.u8()?)),
            2 => {
                let n = usize::from(cur.u16()?);
                Some(HirAltKeySuppression::WhenExactBytes(cur.take(n)?.to_vec()))
            }
            _ => return Err(wire_err("invalid suppression tag")),
        };
        alternate_keys.push(WireAltKey {
            segments,
            duplicates,
            suppression,
        });
    }
    Ok(alternate_keys)
}

/// Appends length-prefixed UTF-8 string bytes to `out`.
///
/// # Arguments
///
/// * `out` - Output buffer.
/// * `s` - String slice.
fn push_str(out: &mut Vec<u8>, s: &str) {
    let raw = s.as_bytes();
    let len = u16::try_from(raw.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(raw.get(..usize::from(len)).unwrap_or(&[]));
}

/// Appends length-prefixed [`HirKeySegment`]s to `out`.
///
/// # Arguments
///
/// * `out` - Output buffer.
/// * `segs` - Key segments.
fn push_segments(out: &mut Vec<u8>, segs: &[HirKeySegment]) {
    let cnt = u16::try_from(segs.len()).unwrap_or(0);
    out.extend_from_slice(&cnt.to_le_bytes());
    for seg in segs.iter().take(usize::from(cnt)) {
        out.extend_from_slice(&seg.offset.to_le_bytes());
        out.extend_from_slice(&seg.length.to_le_bytes());
    }
}

/// Builds a domain validation [`CoblangError`] for malformed [`FileConnectorWireSpec`] bytes.
///
/// # Arguments
///
/// * `reason` - Error description.
fn wire_err(reason: &'static str) -> CoblangError {
    CoblangError::InvalidDomainValue {
        domain_type: "FileConnectorWireSpec",
        value: String::from("<binary>"),
        reason: String::from(reason),
    }
}

/// Bounds-checked byte slice reader for [`decode_file_connector_spec`].
struct WireCursor<'a> {
    /// Source payload bytes.
    bytes: &'a [u8],
    /// Current read offset.
    pos: usize,
}

impl<'a> WireCursor<'a> {
    /// Reads `n` bytes or returns an EOF error.
    ///
    /// # Arguments
    ///
    /// * `n` - Requested byte count.
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.saturating_add(n);
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| wire_err("eof"))?;
        self.pos = end;
        Ok(slice)
    }

    /// Reads one `u8`.
    fn u8(&mut self) -> Result<u8> {
        self.take(1).map(|s| s.first().copied().unwrap_or(0))
    }

    /// Reads a little-endian `u16`.
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes([self.u8()?, self.u8()?]))
    }

    /// Reads a little-endian `u32`.
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes([
            self.u8()?,
            self.u8()?,
            self.u8()?,
            self.u8()?,
        ]))
    }

    /// Reads a `u16`-prefixed UTF-8 string.
    fn str(&mut self) -> Result<String> {
        let len = usize::from(self.u16()?);
        let raw = self.take(len)?;
        core::str::from_utf8(raw)
            .map(String::from)
            .map_err(|_| wire_err("invalid utf8"))
    }

    /// Reads a `u16`-prefixed [`HirKeySegment`] vector.
    fn segments(&mut self) -> Result<Vec<HirKeySegment>> {
        let cnt = usize::from(self.u16()?);
        let mut out = Vec::with_capacity(cnt);
        for _ in 0..cnt {
            out.push(HirKeySegment {
                offset: self.u32()?,
                length: self.u32()?,
            });
        }
        Ok(out)
    }
}
