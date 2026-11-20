//! Canonical, strongly-typed, symbol-resolved High-Level IR for COBOL.

/// Milestone M18 (`REPORT SECTION`) and Milestone M19 (`SCREEN SECTION` & TUI) HIR nodes.
pub mod m18_m19_hir;
/// Milestone M20 HIR nodes for `XML`, `JSON`, and Object-Oriented COBOL (`CLASS-ID` / `INVOKE`).
pub mod m20_hir;

pub use m18_m19_hir::*;
pub use m20_hir::*;

use coblang_common::{
    ByteOffset, DataItemId, DigitCount, FileConnectorId, LevelNumber, ProcedureId, ProgramUnitId,
    ReportId, ScaleFactor, ScopeId, SourceSpan, SymbolId,
};

/// Elementary bytecode instruction for `PICTURE` formatting and de-editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PicEditOp {
    /// Alphabetic character position (`A`).
    AlphaSlot,
    /// Alphanumeric character position (`X`).
    AnyCharSlot,
    /// Numeric decimal digit position (`9`).
    DigitSlot,
    /// Implied decimal point (`V`, occupies 0 storage bytes).
    ImpliedDecimal,
    /// Assumed decimal scaling position (`P`, occupies 0 storage bytes).
    ScalingPosition,
    /// Leading/trailing zero suppression replaced by space (`Z`).
    ZeroSuppressSpace,
    /// Check-protection zero suppression replaced by `*` (`*`).
    ZeroSuppressAsterisk,
    /// Floating `+` sign insertion/suppression (`++`).
    FloatingSignPlus,
    /// Floating `-` sign insertion/suppression (`--`).
    FloatingSignMinus,
    /// Floating currency symbol insertion/suppression (`$$`).
    FloatingCurrency(char),
    /// Fixed `+` sign character (`+`).
    FixedSignPlus,
    /// Fixed `-` sign character (`-`).
    FixedSignMinus,
    /// Fixed 2-byte credit indicator (`CR`).
    FixedCreditCr,
    /// Fixed 2-byte debit indicator (`DB`).
    FixedDebitDb,
    /// Fixed currency character (`$`).
    FixedCurrency(char),
    /// Simple space insertion (`B`).
    InsertBlank,
    /// Simple `'0'` insertion (`0`).
    InsertZero,
    /// Simple `'/'` insertion (`/`).
    InsertSlash,
    /// Grouping separator insertion (`,` or `.` under `DECIMAL-POINT IS COMMA`).
    InsertGroupSep,
    /// Actual decimal point insertion (`.` or `,` under `DECIMAL-POINT IS COMMA`).
    InsertDecimalPoint,
    /// Bit/boolean position (`1`).
    BitSlot,
    /// National UTF-16 character position (`N`).
    NationalSlot,
    /// UTF-8 character position (`U`).
    Utf8Slot,
}

/// Text justification policy on resolved HIR character items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JustifiedSpec {
    /// Standard left-justified storage.
    Left,
    /// `JUSTIFIED RIGHT` storage.
    Right,
}

/// Zero-blanking policy on resolved HIR zoned/edited numeric items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlankZeroSpec {
    /// Display zero digits normally.
    RetainZero,
    /// Fill entire item with spaces when numeric value is zero (`BLANK WHEN ZERO`).
    BlankWhenZero,
}

/// Signedness capability of a numeric or edited HIR type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signedness {
    /// Unsigned (`PIC 9(...)`).
    Unsigned,
    /// Signed (`PIC S9(...)`).
    Signed,
}

/// Exact zoned-decimal sign representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumericSignSpec {
    /// Unsigned item.
    Unsigned,
    /// Trailing embedded overpunch sign (default `PIC S9`).
    EmbeddedTrailing,
    /// Leading embedded overpunch sign (`SIGN IS LEADING`).
    EmbeddedLeading,
    /// Trailing separate `+`/`-` byte (`SIGN IS TRAILING SEPARATE`).
    SeparateTrailing,
    /// Leading separate `+`/`-` byte (`SIGN IS LEADING SEPARATE`).
    SeparateLeading,
}

/// Distinguishes standard `COMP-3` (with sign nibble) from unsigned `COMP-6` BCD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Comp6Mode {
    /// Standard `PACKED-DECIMAL` / `COMP-3` with trailing sign nibble.
    StandardComp3,
    /// Unsigned `COMP-6` without sign nibble.
    UnsignedComp6,
}

/// Byte order of a binary integer HIR field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryEndianness {
    /// Target native endianness (`COMP-5`, `BINARY-*`).
    Native,
    /// Big-endian network/mainframe byte order (`BINARY`/`COMP` under IBM/GnuCOBOL).
    BigEndian,
}

/// Decimal `PICTURE` truncation behavior on binary integer fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TruncationPolicy {
    /// Truncate to `PICTURE` `9(...)` digit count.
    TruncateToPic,
    /// Full native binary capacity (`COMP-5` / `-fnotrunc`).
    FullBinaryRange,
}

/// IEEE 754 binary floating-point width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatBinaryWidth {
    /// 32-bit `COMP-1` / `FLOAT-SHORT`.
    Bits32,
    /// 64-bit `COMP-2` / `FLOAT-LONG`.
    Bits64,
    /// 128-bit `FLOAT-EXTENDED`.
    Bits128,
}

/// IEEE 754-2008 decimal floating-point width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatDecimalWidth {
    /// 64-bit `FLOAT-DECIMAL-16` (`decimal64`).
    Decimal64,
    /// 128-bit `FLOAT-DECIMAL-34` (`decimal128`).
    Decimal128,
}

/// Indicates whether a group item contains variable-length `OCCURS ... DEPENDING ON` children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DynamicOdoPresence {
    /// Fixed compile-time group byte size.
    StaticSize,
    /// Contains at least one `OCCURS DEPENDING ON` subordinate table.
    HasDynamicOdo,
}

/// High-level ISO COBOL data category used by `MOVE`, `INITIALIZE`, and relational rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataCategory {
    /// Alphabetic (`PIC A`).
    Alphabetic,
    /// Alphanumeric (`PIC X`).
    Alphanumeric,
    /// Alphanumeric-edited (`PIC X/B/0`).
    AlphanumericEdited,
    /// Exact or floating numeric (`9`, `COMP*`, `FLOAT*`).
    Numeric,
    /// Numeric-edited (`PIC Z,ZZ9.99`, etc.).
    NumericEdited,
    /// National UTF-16 (`PIC N`).
    National,
    /// National-edited (`PIC N/B/0`).
    NationalEdited,
    /// UTF-8 (`PIC U`).
    Utf8,
    /// Bit/boolean (`PIC 1 USAGE BIT`).
    BooleanBit,
    /// Data/program/function pointer.
    Pointer,
    /// Table `USAGE INDEX`.
    Index,
    /// OO `OBJECT REFERENCE`.
    ObjectReference,
    /// Composite group item (alphanumeric group semantics).
    Group,
}

/// Canonical, resolved COBOL storage and value type in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirType {
    /// `PIC A(n)`.
    Alphabetic {
        /// Storage length in bytes.
        byte_len: u32,
        /// Justification mode.
        justified: JustifiedSpec,
    },
    /// `PIC X(n)`.
    Alphanumeric {
        /// Storage length in bytes.
        byte_len: u32,
        /// Justification mode.
        justified: JustifiedSpec,
    },
    /// Edited alphanumeric display item.
    AlphanumericEdited {
        /// Storage length in bytes.
        byte_len: u32,
        /// Compiled editing bytecode.
        edit_ops: Vec<PicEditOp>,
    },
    /// `USAGE DISPLAY` zoned decimal (`PIC [S]9(...)V9(...)`).
    ZonedDecimal {
        /// Total decimal digits (`1..=38`).
        digits: DigitCount,
        /// Decimal scale (`-38..=38`).
        scale: ScaleFactor,
        /// Sign representation.
        sign: NumericSignSpec,
        /// `BLANK WHEN ZERO` setting.
        blank_zero: BlankZeroSpec,
    },
    /// `PACKED-DECIMAL` (`COMP-3`) or unsigned `COMP-6` BCD.
    PackedDecimal {
        /// Total decimal digits (`1..=38`).
        digits: DigitCount,
        /// Decimal scale.
        scale: ScaleFactor,
        /// Signed vs unsigned BCD.
        sign: Signedness,
        /// `StandardComp3` vs `UnsignedComp6`.
        unsigned_comp6: Comp6Mode,
    },
    /// Binary integer (`BINARY`, `COMP`, `COMP-4`, `COMP-5`, `COMP-X`, `BINARY-*`).
    BinaryInt {
        /// Storage width in bytes (`1..=16`).
        byte_width: u8,
        /// Logical decimal digit count.
        digits: DigitCount,
        /// Decimal scale.
        scale: ScaleFactor,
        /// Signed vs unsigned.
        sign: Signedness,
        /// Byte order.
        endian: BinaryEndianness,
        /// Truncation mode.
        trunc: TruncationPolicy,
    },
    /// Numeric-edited display item (`PIC $$$,$$9.99CR`, etc.).
    NumericEdited {
        /// Formatted storage byte length.
        byte_len: u32,
        /// Source numeric digit capacity.
        digits: DigitCount,
        /// Source decimal scale.
        scale: ScaleFactor,
        /// Whether negative sign indicator is present in `PICTURE`.
        sign_capability: Signedness,
        /// Compiled editing bytecode.
        edit_ops: Vec<PicEditOp>,
        /// `BLANK WHEN ZERO` setting.
        blank_zero: BlankZeroSpec,
    },
    /// Binary IEEE floating-point (`COMP-1`, `COMP-2`, `FLOAT-EXTENDED`).
    FloatBinary {
        /// 32, 64, or 128-bit width.
        width: FloatBinaryWidth,
    },
    /// Decimal IEEE 754-2008 floating-point (`FLOAT-DECIMAL-16`, `FLOAT-DECIMAL-34`).
    FloatDecimal {
        /// 64 or 128-bit width.
        width: FloatDecimalWidth,
    },
    /// `USAGE NATIONAL` UTF-16 character string (`2 * char_len` bytes).
    National {
        /// Number of UTF-16 code units.
        char_len: u32,
        /// Justification mode.
        justified: JustifiedSpec,
    },
    /// National-edited UTF-16 string.
    NationalEdited {
        /// Number of UTF-16 code units.
        char_len: u32,
        /// Compiled editing ops.
        edit_ops: Vec<PicEditOp>,
    },
    /// `USAGE UTF-8` string.
    Utf8 {
        /// Maximum byte length.
        byte_len: u32,
    },
    /// `USAGE BIT` boolean/bit string.
    Bit {
        /// Number of bits.
        bit_len: u32,
    },
    /// `USAGE POINTER` (8-byte machine pointer on 64-bit targets).
    Pointer,
    /// `USAGE PROGRAM-POINTER` / `FUNCTION-POINTER`.
    ProgramPointer,
    /// `USAGE INDEX` (4 or 8-byte table offset/ordinal).
    Index,
    /// OO `USAGE OBJECT REFERENCE [class]`.
    ObjectRef {
        /// Optional static class constraint symbol.
        class_sym: Option<SymbolId>,
    },
    /// Alphanumeric group item (`01`..`49` parent or `66 RENAMES` span).
    Group {
        /// Maximum static byte length.
        static_byte_len: u32,
        /// Dynamic ODO indicator.
        odo_presence: DynamicOdoPresence,
    },
}

impl HirType {
    /// Returns the ISO COBOL [`DataCategory`] of this [`HirType`].
    #[must_use]
    pub const fn category(&self) -> DataCategory {
        match self {
            Self::Alphabetic { .. } => DataCategory::Alphabetic,
            Self::Alphanumeric { .. } => DataCategory::Alphanumeric,
            Self::AlphanumericEdited { .. } => DataCategory::AlphanumericEdited,
            Self::ZonedDecimal { .. }
            | Self::PackedDecimal { .. }
            | Self::BinaryInt { .. }
            | Self::FloatBinary { .. }
            | Self::FloatDecimal { .. } => DataCategory::Numeric,
            Self::NumericEdited { .. } => DataCategory::NumericEdited,
            Self::National { .. } => DataCategory::National,
            Self::NationalEdited { .. } => DataCategory::NationalEdited,
            Self::Utf8 { .. } => DataCategory::Utf8,
            Self::Bit { .. } => DataCategory::BooleanBit,
            Self::Pointer | Self::ProgramPointer => DataCategory::Pointer,
            Self::Index => DataCategory::Index,
            Self::ObjectRef { .. } => DataCategory::ObjectReference,
            Self::Group { .. } => DataCategory::Group,
        }
    }

    /// Computes the single-element storage size in bytes for this [`HirType`].
    #[must_use]
    pub const fn static_byte_size(&self) -> u32 {
        match self {
            Self::Alphabetic { byte_len, .. }
            | Self::Alphanumeric { byte_len, .. }
            | Self::AlphanumericEdited { byte_len, .. }
            | Self::NumericEdited { byte_len, .. }
            | Self::Utf8 { byte_len } => *byte_len,
            Self::ZonedDecimal { digits, sign, .. } => {
                let d = digits.get() as u32;
                match sign {
                    NumericSignSpec::SeparateLeading | NumericSignSpec::SeparateTrailing => d + 1,
                    NumericSignSpec::Unsigned
                    | NumericSignSpec::EmbeddedLeading
                    | NumericSignSpec::EmbeddedTrailing => d,
                }
            }
            Self::PackedDecimal {
                digits,
                unsigned_comp6,
                ..
            } => {
                let d = digits.get() as u32;
                match unsigned_comp6 {
                    Comp6Mode::StandardComp3 => (d / 2) + 1,
                    Comp6Mode::UnsignedComp6 => d.div_ceil(2),
                }
            }
            Self::BinaryInt { byte_width, .. } => *byte_width as u32,
            Self::FloatBinary { width } => match width {
                FloatBinaryWidth::Bits32 => 4,
                FloatBinaryWidth::Bits64 => 8,
                FloatBinaryWidth::Bits128 => 16,
            },
            Self::FloatDecimal { width } => match width {
                FloatDecimalWidth::Decimal64 => 8,
                FloatDecimalWidth::Decimal128 => 16,
            },
            Self::National { char_len, .. } | Self::NationalEdited { char_len, .. } => {
                char_len.saturating_mul(2)
            }
            Self::Bit { bit_len } => bit_len.div_ceil(8),
            Self::Pointer | Self::ProgramPointer | Self::Index | Self::ObjectRef { .. } => 8,
            Self::Group {
                static_byte_len, ..
            } => *static_byte_len,
        }
    }
}

/// Case-sensitivity rule for resolving [`HirStorageSection::ExternalWorkingStorage`] segment names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirExternalCaseMode {
    /// Implicit `IS EXTERNAL` item name folded to canonical ASCII uppercase.
    CanonicalUpper,
    /// Explicit `IS EXTERNAL AS "literal"` preserving original literal spelling.
    ExactAlias,
}

/// Storage section residency of a resolved [`HirDataItem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirStorageSection {
    /// Record storage owned by a `FILE SECTION` connector.
    FileSection(FileConnectorId),
    /// Static/resident `WORKING-STORAGE SECTION`.
    WorkingStorage,
    /// Cross-program `IS EXTERNAL [AS "name"]` `WORKING-STORAGE` segment.
    ExternalWorkingStorage {
        /// Segment name symbol (item name or `AS` literal).
        external_name: SymbolId,
        /// Uppercase folding vs exact `AS` literal spelling.
        case_mode: HirExternalCaseMode,
    },
    /// Inherited `IS GLOBAL` storage root owned by an enclosing ancestor program.
    InheritedGlobal {
        /// Enclosing ancestor [`ProgramUnitId`] owning the backing global symbol.
        owner_unit: ProgramUnitId,
        /// Root [`DataItemId`] inside `owner_unit`.
        owner_root: DataItemId,
    },
    /// Heap/pointer-backed `BASED` item (`ALLOCATE` / `SET ADDRESS OF`).
    BasedStorage,
    /// Per-invocation stack/frame `LOCAL-STORAGE SECTION`.
    LocalStorage,
    /// Caller-supplied `LINKAGE SECTION`.
    Linkage,
    /// Compile-time level-`78` or `01 CONSTANT` entry.
    CompileTimeConstant,
    /// Compiler-synthesized special register (`RETURN-CODE`, `TALLY`, `SORT-RETURN`, `XML-CODE`, etc.).
    SpecialRegister,
}

/// Sort order for `OCCURS ... KEY` and `SORT table` specifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirSortDirection {
    /// Ascending comparison order (`ASCENDING KEY`).
    Ascending,
    /// Descending comparison order (`DESCENDING KEY`).
    Descending,
}

/// Resolved `ASCENDING` / `DESCENDING KEY` attached to an `OCCURS` table dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirTableKey {
    /// Key sort direction.
    pub direction: HirSortDirection,
    /// Resolved subordinate (or self) [`DataItemId`] key field.
    pub key_item: DataItemId,
}

/// Table dimension stride and bounds metadata attached to a [`HirDataItem`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirOccursDim {
    /// Minimum occurrences (`0`..=`max`).
    pub min: u32,
    /// Maximum occurrences (`u32::MAX` for `UNBOUNDED`).
    pub max: u32,
    /// Static single-element stride in bytes.
    pub element_byte_stride: u32,
    /// Optional resolved `DEPENDING ON` object `DataItemId`.
    pub depending_on: Option<DataItemId>,
    /// Resolved `ASCENDING` / `DESCENDING KEY` fields declared on this dimension.
    pub keys: Vec<HirTableKey>,
    /// Implicit `INDEXED BY` [`DataItemId`]s declared on this dimension.
    pub indexed_by: Vec<DataItemId>,
}

/// Resolved Data Division item in [`HirProgram`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirDataItem {
    /// Unique HIR item handle.
    pub id: DataItemId,
    /// Lexical scope owning this item.
    pub scope: ScopeId,
    /// Optional identifier (`None` for `FILLER`).
    pub name: Option<SymbolId>,
    /// COBOL level number.
    pub level: LevelNumber,
    /// Enclosing parent group item, if any.
    pub parent: Option<DataItemId>,
    /// Direct child items (in declaration order) when this item is a group.
    pub children: Vec<DataItemId>,
    /// Storage section residency.
    pub section: HirStorageSection,
    /// Resolved storage & value type.
    pub ty: HirType,
    /// Static byte offset from the root `01`/`77` record start.
    pub byte_offset: ByteOffset,
    /// Total static byte size (including own `OCCURS` multiplier).
    pub total_byte_size: u32,
    /// Inherited + own table dimensions from outermost `OCCURS` to innermost `OCCURS`.
    pub dimensions: Vec<HirOccursDim>,
    /// Optional `REDEFINES` target `DataItemId`.
    pub redefines: Option<DataItemId>,
    /// Optional elementary `VALUE` literal/figurative initializer.
    pub initial_value: Option<HirConstValue>,
    /// Source span of the declaration.
    pub span: SourceSpan,
}

/// Resolved level-`88` condition-name predicate attached to a conditional variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirCondition88 {
    /// Condition-name symbol.
    pub name: SymbolId,
    /// Owning conditional variable `DataItemId`.
    pub parent_item: DataItemId,
    /// Inclusive `(low, optional_high)` literal ranges.
    pub ranges: Vec<(HirConstValue, Option<HirConstValue>)>,
    /// Optional `WHEN SET TO FALSE` value.
    pub false_value: Option<HirConstValue>,
    /// Source span.
    pub span: SourceSpan,
}

/// Distinguishes a standard `FD` file connector from an `SD` sort-merge connector in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirConnectorKind {
    /// Standard `FD` file description.
    Fd,
    /// Sort-merge `SD` description.
    Sd,
}

/// Run-unit sharing identity of a [`HirFileConnector`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HirConnectorSharing {
    /// Unit-scoped (or `IS GLOBAL` parent-unit-owned) file connector.
    UnitOwned {
        /// Defining [`ProgramUnitId`].
        owner_unit: ProgramUnitId,
        /// Defining [`FileConnectorId`] within `owner_unit`.
        owner_fc: FileConnectorId,
    },
    /// Run-unit-wide `FD ... IS EXTERNAL` connector keyed by uppercase external name.
    External {
        /// Canonical uppercase external file connector name.
        external_name: String,
    },
}

/// File organization of a [`HirFileConnector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirFileOrganization {
    /// Record-sequential (`ORGANIZATION IS SEQUENTIAL`).
    Sequential,
    /// Newline-delimited text (`ORGANIZATION IS LINE SEQUENTIAL`).
    LineSequential,
    /// 1-based slot-numbered relative file (`ORGANIZATION IS RELATIVE`).
    Relative,
    /// B+Tree keyed ISAM file (`ORGANIZATION IS INDEXED`).
    Indexed,
}

/// File access mode of a [`HirFileConnector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirFileAccessMode {
    /// Sequential traversal (`ACCESS MODE IS SEQUENTIAL`).
    Sequential,
    /// Direct random lookup (`ACCESS MODE IS RANDOM`).
    Random,
    /// Mixed sequential and random access (`ACCESS MODE IS DYNAMIC`).
    Dynamic,
}

/// `SELECT [OPTIONAL]` presence on a [`HirFileConnector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirFileOptionality {
    /// Missing input file yields `"35"`.
    Required,
    /// `SELECT OPTIONAL` yields `"05"` / `"00"` on missing file.
    Optional,
}

/// Resolved `ASSIGN TO` path/device specification on a [`HirFileConnector`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirAssignSpec {
    /// Static string literal path (`ASSIGN TO "myfile.dat"`).
    Literal(String),
    /// Explicit environment variable (`ASSIGN TO EXTERNAL env-name`).
    ExternalEnv(String),
    /// Dynamic `WORKING-STORAGE` / `LINKAGE` data item (`ASSIGN TO [DYNAMIC] data-item`).
    DynamicItem(DataItemId),
    /// Unqualified name resolved via environment (`DD_name` / `name`) or literal fallback.
    EnvOrLiteral(String),
    /// Standard output console (`ASSIGN TO DISPLAY`).
    DisplayDevice,
    /// Standard input console (`ASSIGN TO KEYBOARD`).
    KeyboardDevice,
}

/// Fixed vs variable record framing on a [`HirFileConnector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirRecordFormat {
    /// Fixed-length records (`min_record_bytes == max_record_bytes`, no `DEPENDING ON`).
    Fixed,
    /// Variable-length records (`RECORD IS VARYING`, `DEPENDING ON`, or differing `01` sizes).
    Varying,
}

/// Byte-offset segment within an `FD` record for primary or alternate `INDEXED` keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirKeySegment {
    /// Zero-based byte offset from the start of the `FD` record area.
    pub offset: u32,
    /// Segment width in bytes (`>= 1`).
    pub length: u32,
}

/// Duplicate-key policy on an `ALTERNATE RECORD KEY` in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirDuplicateKeyPolicy {
    /// Keys must be strictly unique (`"22"` on duplicate).
    Unique,
    /// `WITH DUPLICATES` permitted in FIFO order (`"02"` status).
    WithDuplicates,
}

/// Sparse-index suppression rule on an `ALTERNATE RECORD KEY` in HIR.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HirAltKeySuppression {
    /// Suppress index entry when every byte of the assembled key equals `byte` (`SUPPRESS WHEN ALL`).
    WhenAllByte(u8),
    /// Suppress index entry when the assembled key equals this exact byte sequence (`SUPPRESS WHEN lit`).
    WhenExactBytes(Vec<u8>),
}

/// Resolved `ALTERNATE RECORD KEY` layout on a [`HirFileConnector`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirAltKeySpec {
    /// Optional synthetic split-key alias symbol (`KEY IS split-name = ...` / `SOURCE IS ...`).
    pub split_name: Option<SymbolId>,
    /// Declared key `DataItemId`s.
    pub key_items: Vec<DataItemId>,
    /// Contiguous or split key byte segments within the `FD` record area.
    pub segments: Vec<HirKeySegment>,
    /// Duplicate-key admission policy.
    pub duplicates: HirDuplicateKeyPolicy,
    /// Optional sparse-index suppression rule.
    pub suppression: Option<HirAltKeySuppression>,
}

/// Connector-level `LOCK MODE` in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirFileLockMode {
    /// Default lock behavior.
    Default,
    /// `LOCK MODE IS MANUAL`.
    Manual,
    /// `LOCK MODE IS AUTOMATIC`.
    Automatic,
    /// `LOCK MODE IS EXCLUSIVE`.
    Exclusive,
}

/// Connector-level `SHARING WITH` mode in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirFileSharingMode {
    /// `SHARING WITH ALL OTHER`.
    AllOther,
    /// `SHARING WITH NO OTHER`.
    NoOther,
    /// `SHARING WITH READ ONLY`.
    ReadOnly,
}

/// Printer / line-advancing framing mode on a `SEQUENTIAL` [`HirFileConnector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirPrintAdvancingMode {
    /// Unframed binary fixed/variable sequential records.
    RawBinary,
    /// Line-advancing sequential file (`ASSIGN TO PRINTER`, `LINAGE`, or `WRITE ... ADVANCING`).
    LineAdvancing,
}

/// Static literal or dynamic data-item parameter inside a [`HirLinageClause`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirLinageParam {
    /// Compile-time integer line count.
    Static(u32),
    /// Runtime integer data item evaluated at `OPEN` / page boundary.
    Dynamic(DataItemId),
}

/// Resolved `LINAGE` logical page layout on an `FD` [`HirFileConnector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirLinageClause {
    /// `LINAGE IS n LINES` page body count.
    pub body: HirLinageParam,
    /// Optional `WITH FOOTING AT f` footing start line.
    pub footing: Option<HirLinageParam>,
    /// Optional `LINES AT TOP t` top margin count.
    pub top: Option<HirLinageParam>,
    /// Optional `LINES AT BOTTOM b` bottom margin count.
    pub bottom: Option<HirLinageParam>,
    /// Associated `LINAGE-COUNTER` special-register [`DataItemId`].
    pub counter_item: DataItemId,
}

/// Resolved `SELECT` + `FD`/`SD` file connector in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileConnector {
    /// Unique file connector handle within the owning [`HirProgram`].
    pub id: FileConnectorId,
    /// Internal file connector symbol.
    pub name: SymbolId,
    /// `FD` vs `SD` connector kind.
    pub kind: HirConnectorKind,
    /// Unit-owned vs `IS EXTERNAL` run-unit sharing identity.
    pub sharing: HirConnectorSharing,
    /// Canonical `FileConnectorId` in this unit sharing the same `SAME RECORD AREA` buffer.
    pub record_area_group: FileConnectorId,
    /// File organization (`Sequential`, `LineSequential`, `Relative`, `Indexed`).
    pub organization: HirFileOrganization,
    /// File access mode (`Sequential`, `Random`, `Dynamic`).
    pub access_mode: HirFileAccessMode,
    /// `SELECT OPTIONAL` vs required.
    pub optionality: HirFileOptionality,
    /// Resolved `ASSIGN TO` specification.
    pub assign: HirAssignSpec,
    /// Fixed vs variable record format.
    pub record_format: HirRecordFormat,
    /// Minimum record byte size (`>= 1` when records exist).
    pub min_record_bytes: u32,
    /// Maximum record byte size across associated `01` records.
    pub max_record_bytes: u32,
    /// Optional `RECORD IS VARYING ... DEPENDING ON` length item.
    pub depending_on_item: Option<DataItemId>,
    /// Associated `01` record `DataItemId`s.
    pub records: Vec<DataItemId>,
    /// Optional resolved `FILE STATUS` `DataItemId`.
    pub status_item: Option<DataItemId>,
    /// Optional resolved secondary status `DataItemId`.
    pub secondary_status_item: Option<DataItemId>,
    /// Optional resolved `RELATIVE KEY` `DataItemId`.
    pub relative_key_item: Option<DataItemId>,
    /// Optional synthetic split-key alias symbol on the primary `RECORD KEY`.
    pub primary_split_name: Option<SymbolId>,
    /// Optional resolved primary `RECORD KEY` `DataItemId`s.
    pub primary_key_items: Vec<DataItemId>,
    /// Resolved primary key byte segments within the `FD` record area.
    pub primary_key_segments: Vec<HirKeySegment>,
    /// Resolved `ALTERNATE RECORD KEY` specifications.
    pub alternate_keys: Vec<HirAltKeySpec>,
    /// Connector-level `LOCK MODE`.
    pub lock_mode: HirFileLockMode,
    /// Optional connector-level `SHARING WITH` mode.
    pub sharing_mode: Option<HirFileSharingMode>,
    /// `PADDING CHARACTER` byte (`b' '` by default).
    pub padding_byte: u8,
    /// Raw binary vs line-advancing sequential framing.
    pub print_mode: HirPrintAdvancingMode,
    /// Optional `LINAGE` logical page configuration.
    pub linage: Option<HirLinageClause>,
    /// Optional `COLLATING SEQUENCE` index into `HirProgram::alphabets`.
    pub collating_alphabet: Option<usize>,
    /// Source span.
    pub span: SourceSpan,
}

/// Bitmask of active `EC-*` runtime checks enabled via `>>TURN` or dialect flags at a statement/place.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ExceptionCheckMask(u32);

impl ExceptionCheckMask {
    /// Empty check mask (no extra `EC-*` checks active).
    pub const NONE: Self = Self(0);
    /// Subscript bounds checking (`EC-BOUND-SUBSCRIPT`).
    pub const BOUND_SUBSCRIPT: Self = Self(1 << 0);
    /// `OCCURS DEPENDING ON` range checking (`EC-BOUND-ODO`).
    pub const BOUND_ODO: Self = Self(1 << 1);
    /// Reference modification bounds checking (`EC-BOUND-REF-MOD`).
    pub const BOUND_REF_MOD: Self = Self(1 << 2);
    /// Size overflow/zero-divide checking (`EC-SIZE-*`).
    pub const SIZE_ERROR: Self = Self(1 << 3);
    /// Fatal `libcob` bounds-violation abort (`-debug`, `-fec=bound*`, `$SET BOUND`/`SSRANGE`).
    pub const BOUND_FATAL: Self = Self(1 << 4);

    /// Creates a mask from raw bit flags.
    ///
    /// # Arguments
    ///
    /// * `bits` - Raw bitfield value.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Returns the raw `u32` bitmask.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Returns the bitwise union of `self` and `other`.
    ///
    /// # Arguments
    ///
    /// * `other` - Additional check mask bits to enable.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns `self` with all bits in `other` cleared.
    ///
    /// # Arguments
    ///
    /// * `other` - Mask bits to clear.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Returns `true` if all bits in `flag` are set in `self`.
    ///
    /// # Arguments
    ///
    /// * `flag` - Mask flag(s) to test.
    #[must_use]
    pub const fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }
}

/// Resolved 1-based reference-modification slice on a [`HirPlace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirRefMod {
    /// 1-based start offset expression.
    pub offset: Box<HirExpr>,
    /// Optional slice length expression.
    pub length: Option<Box<HirExpr>>,
}

/// Fully symbol-resolved addressable storage reference (`HirPlace`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirPlace {
    /// Resolved target [`DataItemId`].
    pub item: DataItemId,
    /// Static byte offset of `item`.
    pub static_offset: ByteOffset,
    /// Resolved 1-based subscript expressions paired with static byte strides.
    pub subscripts: Vec<(HirExpr, u32)>,
    /// Optional 1-based `(offset : [length])` reference modification.
    pub refmod: Option<HirRefMod>,
    /// Effective [`HirType`] after subscripting / reference modification.
    pub effective_ty: HirType,
    /// Active runtime exception check mask at this access site.
    pub check_mask: ExceptionCheckMask,
    /// Source span.
    pub span: SourceSpan,
}

/// Explicit sign status of a decimal literal in [`HirConstValue::Decimal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirDecimalSign {
    /// No explicit sign on `DecimalLit`, or sign already embedded in `IntegerLit` text.
    UnsignedOrEmbedded,
    /// Explicit leading `+` on `DecimalLit`.
    Positive,
    /// Explicit leading `-` on `DecimalLit`.
    Negative,
}

/// Compile-time constant / literal value in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirConstValue {
    /// Decoded raw alphanumeric/hex/EBCDIC byte string.
    Bytes(Vec<u8>),
    /// `NATIONAL` (`N"..."` / `NX"..."`) UTF-16BE byte sequence.
    NationalBytes(Vec<u8>),
    /// `UTF-8` (`U"..."` / `UX"..."`) UTF-8 byte sequence.
    Utf8Bytes(Vec<u8>),
    /// Interned integer/decimal literal text + scale (`0` for integer).
    Decimal {
        /// Signed or unsigned digit string symbol.
        text: SymbolId,
        /// Decimal scale.
        scale: ScaleFactor,
        /// Explicit decimal literal sign qualifier.
        sign: HirDecimalSign,
    },
    /// Interned floating-point literal symbol.
    Float(SymbolId),
    /// Figurative fill kind (`Zero`, `Space`, `High`, `Low`, `Quote`, `Null`, `AllBytes`).
    Figurative(HirFigurative),
}

/// Canonical figurative constant in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirFigurative {
    /// `ZERO`.
    Zero,
    /// `SPACE`.
    Space,
    /// `HIGH-VALUE`.
    HighValue,
    /// `LOW-VALUE`.
    LowValue,
    /// `QUOTE`.
    Quote,
    /// `NULL`.
    Null,
    /// `ALL <bytes>`.
    All(Vec<u8>),
}

/// Binary arithmetic/string operator in [`HirExpr::Binary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirBinOp {
    /// Addition (`+`).
    Add,
    /// Subtraction (`-`).
    Sub,
    /// Multiplication (`*`).
    Mul,
    /// Division (`/`).
    Div,
    /// Exponentiation (`**`).
    Pow,
    /// String concatenation (`&`).
    Concat,
}

/// Resolved expression node in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirExpr {
    /// Read from a resolved storage [`HirPlace`].
    Place(HirPlace),
    /// Address of a storage [`HirPlace`] (`ADDRESS OF x`).
    AddressOf(HirPlace),
    /// Byte/character length of a storage [`HirPlace`] (`LENGTH OF x`).
    LengthOf(HirPlace),
    /// Literal or compile-time constant.
    Const {
        /// Constant payload.
        value: HirConstValue,
        /// Source span.
        span: SourceSpan,
    },
    /// Unary negation (`-expr`).
    Neg(Box<HirExpr>),
    /// Binary arithmetic or concatenation (`lhs op rhs`).
    Binary {
        /// Operator.
        op: HirBinOp,
        /// Left operand.
        lhs: Box<HirExpr>,
        /// Right operand.
        rhs: Box<HirExpr>,
        /// Source span.
        span: SourceSpan,
    },
    /// Intrinsic or User-Defined Function call.
    CallFunction {
        /// Function symbol.
        name: SymbolId,
        /// Lowered argument expressions.
        args: Vec<HirExpr>,
        /// Source span.
        span: SourceSpan,
    },
    /// Fully typechecked Milestone M12 intrinsic or `FUNCTION-ID` call (`ALL` slices, keywords, refmod).
    M12Call(Box<HirM12FunctionCall>),
    /// Milestone M20 OO `SELF` reference or inline `receiver::"method"(...)` invocation.
    M20(Box<HirM20Expr>),
}

/// Dialect extension intrinsics beyond `coblang_intrinsics::IntrinsicId` (`GnuCOBOL` locale/pointer helpers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirExtIntrinsic {
    /// `FUNCTION CONTENT-LENGTH(ptr)` (`strlen` of C pointer; `0` on `NULL`).
    ContentLength,
    /// `FUNCTION MONETARY-DECIMAL-POINT()`.
    MonetaryDecimalPoint,
    /// `FUNCTION MONETARY-THOUSANDS-SEPARATOR()`.
    MonetaryThousandsSeparator,
    /// `FUNCTION NUMERIC-DECIMAL-POINT()`.
    NumericDecimalPoint,
    /// `FUNCTION NUMERIC-THOUSANDS-SEPARATOR()`.
    NumericThousandsSeparator,
}

/// Resolved callee of [`HirM12FunctionCall`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirM12Callee {
    /// Standard ISO/`GnuCOBOL` intrinsic identified by `IntrinsicId::to_abi_code()`.
    Intrinsic {
        /// Canonical ABI code (`1..=116`).
        abi_code: u16,
    },
    /// `GnuCOBOL` extension intrinsic.
    ExtIntrinsic(HirExtIntrinsic),
    /// User-Defined Function (`FUNCTION-ID. name [AS "ext"]`).
    UserDefined {
        /// External linkage symbol (`AS "ext"` if present, else canonical uppercase `name`).
        external_name: SymbolId,
    },
}

/// Upper bound of one `ALL`-wildcard table dimension in [`HirTableSlice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirSliceBound {
    /// Fixed compile-time `OCCURS n TIMES` count.
    StaticCount(u32),
    /// Dynamic `OCCURS min TO max DEPENDING ON dep_place` count clamped to `0..=max_occurs`.
    DynamicOdo {
        /// Resolved `DEPENDING ON` counter place.
        dep_place: HirPlace,
        /// Declared maximum `OCCURS` ceiling.
        max_occurs: u32,
    },
}

/// Multi-dimensional `ALL` table-slice expansion descriptor (`TAB(ALL)`, `TAB(i, ALL)`, `TAB(ALL, ALL)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirTableSlice {
    /// Base element [`HirPlace`] with fixed subscript dimensions retained and `ALL` dimensions anchored at `1`.
    pub base_place: HirPlace,
    /// Ordered `(bound, byte_stride)` pairs for each `ALL` wildcard dimension.
    pub varying_dims: Vec<(HirSliceBound, u32)>,
}

/// Compile-time algebraic type probe for `HIGHEST-ALGEBRAIC`, `LOWEST-ALGEBRAIC`, and `SMALLEST-ALGEBRAIC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirAlgebraicSpec {
    /// Total decimal digits (`1..=38`), or `0` when probing an IEEE float target (`COMP-1`/`COMP-2`/`FLOAT-*`).
    pub digits: u16,
    /// Decimal scale factor.
    pub scale: i16,
    /// Signed vs unsigned capability.
    pub signedness: Signedness,
}

/// Resolved argument item inside [`HirM12FunctionCall`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirM12Arg {
    /// Evaluated scalar expression.
    Expr(HirExpr),
    /// Direct storage [`HirPlace`] preserving raw bytes and encoding (`BIT-OF`, `HEX-OF`, `LENGTH`, `CONTENT-OF`, `DISPLAY-OF`, `NATIONAL-OF`, `U*`).
    RawStoragePlace(HirPlace),
    /// `ALL`-subscripted table slice (`MAX`, `MIN`, `ORD-MAX`, `ORD-MIN`, `SUM`, `MEAN`, `MEDIAN`, `MIDRANGE`, `RANGE`, `VARIANCE`, `STANDARD-DEVIATION`).
    TableSlice(HirTableSlice),
    /// Compile-time numeric layout probe (`HIGHEST-ALGEBRAIC`, `LOWEST-ALGEBRAIC`, `SMALLEST-ALGEBRAIC`).
    AlgebraicProbe(HirAlgebraicSpec),
    /// Variable-length `OCCURS DEPENDING ON` group byte/character length probe (`base_bytes + odo * elem_stride`).
    OdoLengthProbe {
        /// Fixed prefix byte count preceding the variable table.
        base_bytes: u32,
        /// `DEPENDING ON` object place.
        odo_place: HirPlace,
        /// Byte or character stride per element.
        elem_stride: u32,
    },
    /// Canonical uppercase modifier keyword (`LEADING`, `TRAILING`, `SYSTEM-OFFSET`, `LAST`, `ANYCASE`, `PHYSICAL`, etc.).
    Keyword(SymbolId),
    /// Explicitly or trailing `OMITTED` argument.
    Omitted,
}

/// Fully resolved Milestone M12 intrinsic or User-Defined Function call expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirM12FunctionCall {
    /// Target intrinsic or UDF callee.
    pub callee: HirM12Callee,
    /// Lowered argument list.
    pub args: Vec<HirM12Arg>,
    /// Optional `(offset : [length])` reference modification applied to alphanumeric/national results.
    pub refmod: Option<HirRefMod>,
    /// Static or upper-bound result [`HirType`].
    pub return_ty: HirType,
    /// Interned ISO exception location `"<unit>; <proc>; <line>"` recorded on `EC-ARGUMENT-FUNCTION`.
    pub location_sym: SymbolId,
    /// Source span.
    pub span: SourceSpan,
}

/// Relational operator in [`HirCondition::Compare`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirRelOp {
    /// `=`.
    Eq,
    /// `<>`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
}

/// Strongly-typed boolean truth constant in [`HirCondition::BoolConst`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirTruthValue {
    /// `TRUE`.
    True,
    /// `FALSE`.
    False,
}

/// Desugared canonical boolean condition in HIR (all abbreviated relations & `88`s expanded!).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirCondition {
    /// Constant boolean (`TRUE` / `FALSE`).
    BoolConst(HirTruthValue),
    /// Binary comparison (`lhs op rhs`).
    Compare {
        /// Relational operator.
        op: HirRelOp,
        /// Left operand.
        lhs: HirExpr,
        /// Right operand.
        rhs: HirExpr,
    },
    /// Character class check (`NUMERIC`, `ALPHABETIC`, etc.) with `SymbolId` class tag.
    ClassCheck {
        /// Subject expression.
        expr: HirExpr,
        /// Class identifier symbol.
        class_name: SymbolId,
    },
    /// Sign check (`> 0`, `< 0`, `= 0` mapped to `Compare` or dedicated sign op).
    Omitted {
        /// Tested `LINKAGE` item.
        item: DataItemId,
    },
    /// External hardware/software switch test (`SWITCH-0..36`).
    Switch {
        /// Zero-based switch slot (`0..=36`).
        switch_index: u8,
        /// Expected switch status (`True` for `ON`, `False` for `OFF`).
        expected: HirTruthValue,
    },
    /// Logical negation (`!cond`).
    Not(Box<HirCondition>),
    /// Logical conjunction (`a && b`).
    And(Box<HirCondition>, Box<HirCondition>),
    /// Logical disjunction (`a || b`).
    Or(Box<HirCondition>, Box<HirCondition>),
}

/// Distinguishes outermost user `PERFORM` loops from nested internal `AFTER` counter tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirLoopKind {
    /// Outermost user `PERFORM` loop (`EXIT PERFORM` breaks out past this loop).
    UserPerform,
    /// Nested inner `AFTER` tier inside `PERFORM VARYING ... AFTER ...`.
    InternalAfterTier,
}

/// Structured control-transfer statement kinds inside `PROCEDURE DIVISION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirFlowExitKind {
    /// `EXIT PARAGRAPH` (jump to current paragraph epilogue).
    ExitParagraph,
    /// `EXIT SECTION` (jump to enclosing section's final paragraph epilogue).
    ExitSection,
    /// `EXIT PERFORM` (break out of enclosing inline `PERFORM`).
    ExitPerformBreak,
    /// `EXIT PERFORM CYCLE` (continue next iteration of enclosing inline `PERFORM`).
    ExitPerformCycle,
    /// `NEXT SENTENCE` (jump to end of enclosing period-terminated sentence).
    NextSentence,
}

/// Canonical lowered HIR statement (`CORRESPONDING`, `88`, and abbreviated conditions desugared!).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirStmt {
    /// Elementary or group `MOVE src TO dst`.
    Move {
        /// Source expression.
        src: HirExpr,
        /// Destination storage place.
        dst: HirPlace,
        /// Source span.
        span: SourceSpan,
    },
    /// `COMPUTE` / desugared `ADD` / `SUBTRACT` / `MULTIPLY` / `DIVIDE` assignment.
    AssignArith {
        /// Target places with rounding flag (`true` via `Signedness`-style enum `HirRounding`).
        targets: Vec<(HirPlace, HirRounding)>,
        /// Computed expression.
        expr: HirExpr,
        /// Optional `ON SIZE ERROR` handler statements.
        on_size_error: Vec<HirStmt>,
        /// Optional `NOT ON SIZE ERROR` handler statements.
        not_on_size_error: Vec<HirStmt>,
        /// Source span.
        span: SourceSpan,
    },
    /// `DISPLAY` output statement.
    Display {
        /// Displayed expressions.
        operands: Vec<HirExpr>,
        /// Output stream (`Stdout` vs `Stderr`).
        device: HirDisplayDevice,
        /// Newline suppression (`HirAdvancing`).
        advancing: HirAdvancing,
        /// Source span.
        span: SourceSpan,
    },
    /// `IF cond THEN ... ELSE ...`.
    If {
        /// Desugared condition.
        cond: HirCondition,
        /// True branch.
        then_body: Vec<HirStmt>,
        /// False branch.
        else_body: Vec<HirStmt>,
        /// Source span.
        span: SourceSpan,
    },
    /// Sentence scope wrapper anchoring `NEXT SENTENCE` branch targets.
    Sentence {
        /// Statements belonging to this period-terminated sentence.
        body: Vec<HirStmt>,
        /// Source span.
        span: SourceSpan,
    },
    /// Out-of-line `PERFORM start_proc ..= end_proc`.
    PerformRange {
        /// Start procedure handle.
        start_proc: ProcedureId,
        /// Inclusive end procedure handle.
        end_proc: ProcedureId,
        /// Source span.
        span: SourceSpan,
    },
    /// Structured loop (`PERFORM UNTIL / TIMES / VARYING`).
    Loop {
        /// Outermost user `PERFORM` vs internal `AFTER` counter tier.
        kind: HirLoopKind,
        /// Test-before vs test-after mode.
        test_before: HirLoopTest,
        /// Termination condition.
        until: HirCondition,
        /// Loop body statements.
        body: Vec<HirStmt>,
        /// End-of-iteration step statements executed on normal loop latch and `EXIT PERFORM CYCLE`.
        cycle_step: Vec<HirStmt>,
        /// Source span.
        span: SourceSpan,
    },
    /// Structured intra-procedure exit (`EXIT PARAGRAPH/SECTION/PERFORM [CYCLE]` or `NEXT SENTENCE`).
    FlowExit {
        /// Specific control-transfer target.
        kind: HirFlowExitKind,
        /// Source span.
        span: SourceSpan,
    },
    /// `ALTER para-1 TO [PROCEED TO] para-2 ...` runtime target update.
    Alter {
        /// `(altered_paragraph, new_target_paragraph)` pairs.
        pairs: Vec<(ProcedureId, ProcedureId)>,
        /// Source span.
        span: SourceSpan,
    },
    /// `SET switch-mnemonic TO {ON | OFF}` runtime switch update.
    SetSwitch {
        /// Zero-based switch index (`0..=36`).
        switch_index: u8,
        /// Desired switch status (`True` for `ON`, `False` for `OFF`).
        state: HirTruthValue,
        /// Source span.
        span: SourceSpan,
    },
    /// Unconditional or `DEPENDING ON` jump to resolved [`ProcedureId`]\(s\).
    Jump {
        /// Target procedure handles.
        targets: Vec<ProcedureId>,
        /// Optional selector expression.
        depending_on: Option<HirExpr>,
        /// Source span.
        span: SourceSpan,
    },
    /// Subprogram `CALL` (`BY REFERENCE` / `BY CONTENT` / `BY VALUE` / `OMITTED`, `RETURNING`, `ON EXCEPTION`).
    Call(Box<HirCallSpec>),
    /// Subprogram `CANCEL` (`CANCEL target ...`).
    Cancel {
        /// Module name expressions to cancel.
        targets: Vec<HirExpr>,
        /// Source span.
        span: SourceSpan,
    },
    /// `DIVIDE dividend BY divisor GIVING quotient [ROUNDED] REMAINDER remainder`.
    DivideRemainder(Box<HirDivideRemainderSpec>),
    /// Category-aware `INITIALIZE target ... [WITH FILLER] [ALL TO VALUE] [REPLACING ...]`.
    Initialize {
        /// Resolved target places to initialize.
        targets: Vec<HirPlace>,
        /// Whether `FILLER` items participate.
        filler: HirInitFiller,
        /// Whether `VALUE` clauses are restored.
        value_mode: HirInitValueMode,
        /// Explicit `(DataCategory, replacement_expr)` pairs.
        replacements: Vec<(DataCategory, HirExpr)>,
        /// Source span.
        span: SourceSpan,
    },
    /// `GOBACK` / `STOP RUN` / `EXIT`.
    Return {
        /// Termination scope (`StopRun`, `Goback`, or `ExitProgram`).
        mode: HirReturnMode,
        /// Optional return code expression.
        code: Option<HirExpr>,
        /// Source span.
        span: SourceSpan,
    },
    /// In-memory table `SORT table-name [ON {ASCENDING|DESCENDING} KEY ...] [COLLATING SEQUENCE ...]`.
    SortTable(Box<HirSortTableSpec>),
    /// `DISPLAY operand ... UPON {ENVIRONMENT-NAME | ENVIRONMENT-VALUE | ARGUMENT-NUMBER | COMMAND-LINE}`.
    DisplayEnv {
        /// Displayed operand expressions.
        operands: Vec<HirExpr>,
        /// Environment or argument target register.
        target: HirEnvTargetKind,
        /// Source span.
        span: SourceSpan,
    },
    /// `ACCEPT dst [FROM ...]` (`Milestone M10`).
    Accept(Box<HirAcceptSpec>),
    /// `STRING ... DELIMITED BY ... INTO dst [WITH POINTER ptr] [ON OVERFLOW ...]` (`Milestone M10`).
    StringVerb(Box<HirStringSpec>),
    /// `UNSTRING src [DELIMITED BY ...] INTO ... [WITH POINTER ptr] [TALLYING IN tally] [ON OVERFLOW ...]` (`Milestone M10`).
    Unstring(Box<HirUnstringSpec>),
    /// `INSPECT target {TALLYING | REPLACING | CONVERTING} ...` and `TRANSFORM` (`Milestone M10`).
    Inspect(Box<HirInspectSpec>),
    /// `SET` pointer / `ADDRESS OF` / `PROGRAM-POINTER` operation (`Milestone M11`).
    SetPointer(Box<HirSetPointerSpec>),
    /// `ALLOCATE {expr CHARACTERS | based-item} [INITIALIZED [TO expr]] [RETURNING ptr]` (`Milestone M11`).
    Allocate(Box<HirAllocateSpec>),
    /// `FREE {based-item | ADDRESS OF based-item | ptr} ...` (`Milestone M11`).
    Free {
        /// Ordered targets to release and reset to `NULL`.
        targets: Vec<HirFreeTarget>,
        /// Source span.
        span: SourceSpan,
    },
    /// `OPEN {INPUT | OUTPUT | I-O | EXTEND} file ...` (`Milestone M14`/`M15`).
    FileOpen(Box<HirFileOpenStmt>),
    /// `CLOSE file [WITH LOCK | WITH NO REWIND | REEL/UNIT] ...` (`Milestone M14`/`M15`).
    FileClose(Box<HirFileCloseStmt>),
    /// `READ file [NEXT | PREVIOUS] [INTO dst] [KEY IS key] [AT END / INVALID KEY ...]` (`Milestone M14`/`M15`).
    FileRead(Box<HirFileReadStmt>),
    /// `WRITE record [FROM src] [ADVANCING ...] [AT EOP / INVALID KEY ...]` (`Milestone M14`/`M15`).
    FileWrite(Box<HirFileWriteStmt>),
    /// `REWRITE record [FROM src] [INVALID KEY ...]` (`Milestone M14`/`M15`).
    FileRewrite(Box<HirFileRewriteStmt>),
    /// `DELETE file [RECORD] [INVALID KEY ...]` (`Milestone M14`/`M15`).
    FileDelete(Box<HirFileDeleteStmt>),
    /// `DELETE FILE file ... [ON EXCEPTION ...]` (`Milestone M14`/`M15`).
    FileDeletePhysical(Box<HirFileDeletePhysicalStmt>),
    /// `START file [FIRST | LAST | KEY IS rel_op key] [INVALID KEY ...]` (`Milestone M14`/`M15`).
    FileStart(Box<HirFileStartStmt>),
    /// `UNLOCK file [RECORD | RECORDS]` (`Milestone M14`/`M15`).
    FileUnlock(Box<HirFileUnlockStmt>),
    /// `SORT sd-file ON {ASCENDING|DESCENDING} KEY ... {USING | INPUT PROCEDURE} {GIVING | OUTPUT PROCEDURE}` (`Milestone M16`).
    SortFile(Box<HirSortFileStmt>),
    /// `MERGE sd-file ON {ASCENDING|DESCENDING} KEY ... USING f1 f2 ... {GIVING | OUTPUT PROCEDURE}` (`Milestone M16`).
    MergeFile(Box<HirMergeFileStmt>),
    /// `RELEASE sd-record [FROM src]` (`Milestone M16`).
    ReleaseRecord(Box<HirReleaseStmt>),
    /// `RETURN sd-file RECORD [INTO dst] AT END ... [NOT AT END ...]` (`Milestone M16`).
    ReturnRecord(Box<HirReturnStmt>),
    /// `>>TURN ec-name ... CHECKING {ON [WITH LOCATION] | OFF}` runtime state update (`Milestone M13`).
    SetTurn(HirSetTurnStmt),
    /// `SET LAST EXCEPTION TO OFF` (`Milestone M13`).
    SetLastExceptionOff {
        /// Source span.
        span: SourceSpan,
    },
    /// `RAISE [EXCEPTION ec-name]` (`Milestone M13`).
    Raise(HirRaiseStmt),
    /// `RESUME [AT NEXT STATEMENT | AT proc-name]` (`Milestone M13`).
    Resume(HirResumeStmt),
    /// Post-statement active exception check and `DECLARATIVES` / `RESUME` router (`Milestone M13`).
    CheckException(HirCheckExceptionStmt),
    /// NIST CCVS-85 `DB` Debug Module `DEBUG-ITEM` population & `USE FOR DEBUGGING` hook (`Milestone M13`).
    DebugTrace(Box<HirDebugTraceStmt>),
    /// Milestone M18 declarative `REPORT SECTION` operations (`INITIATE`, `GENERATE`, `TERMINATE`, `SUPPRESS`).
    M18(Box<HirM18Stmt>),
    /// Milestone M19 `SCREEN SECTION` & TUI `DISPLAY` / `ACCEPT` / `SET ... ATTRIBUTE` operations.
    M19(Box<HirM19Stmt>),
    /// Milestone M20 `XML`, `JSON`, and Object-Oriented `INVOKE` operations.
    M20(Box<HirM20Stmt>),
    /// Generic preserved high-level operation (Report, Screen, XML/JSON, Set, etc.) for M12–M20 lowering.
    HighLevelOp {
        /// Interned verb tag (`"INITIATE"`, `"GENERATE"`, `"XML"`, etc.).
        verb: SymbolId,
        /// Source span.
        span: SourceSpan,
    },
}

/// Runtime checking disposition in [`HirSetTurnStmt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirTurnMode {
    /// `CHECKING OFF`.
    Off,
    /// `CHECKING ON` (without `WITH LOCATION`).
    On,
    /// `CHECKING ON WITH LOCATION`.
    OnWithLocation,
}

/// Target exception set in [`HirSetTurnStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirTurnTarget {
    /// `EC-ALL`.
    All,
    /// Expanded leaf/category `ExceptionCode::to_abi_id()` codes.
    Codes(Vec<u16>),
}

/// Payload of [`HirStmt::SetTurn`] (`Milestone M13`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirSetTurnStmt {
    /// Target `EC-*` codes or `EC-ALL`.
    pub target: HirTurnTarget,
    /// New checking mode.
    pub mode: HirTurnMode,
    /// Source span.
    pub span: SourceSpan,
}

/// Target exception raised by [`HirStmt::Raise`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirRaiseTarget {
    /// Bare `RAISE` re-raising `LAST EXCEPTION`.
    LastException,
    /// `RAISE [EXCEPTION] ec-name` identified by `ExceptionCode::to_abi_id()`.
    EcCode(u16),
}

/// Payload of [`HirStmt::Raise`] (`Milestone M13`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirRaiseStmt {
    /// Exception target to raise.
    pub target: HirRaiseTarget,
    /// Interned ISO exception location `"<unit>; <proc>; <line>"`.
    pub location_sym: SymbolId,
    /// Interned COBOL verb name (`"RAISE"`).
    pub statement_sym: SymbolId,
    /// Source span.
    pub span: SourceSpan,
}

/// Continuation target of [`HirStmt::Resume`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirResumeTarget {
    /// `RESUME [AT] NEXT STATEMENT`.
    NextStatement,
    /// `RESUME AT proc-name`.
    Procedure(ProcedureId),
}

/// Payload of [`HirStmt::Resume`] (`Milestone M13`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirResumeStmt {
    /// Continuation target.
    pub target: HirResumeTarget,
    /// Source span.
    pub span: SourceSpan,
}

/// Payload of [`HirStmt::CheckException`] (`Milestone M13`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirCheckExceptionStmt {
    /// Interned ISO exception location `"<unit>; <proc>; <line>"`.
    pub location_sym: SymbolId,
    /// Interned COBOL verb name (`"MOVE"`, `"COMPUTE"`, `"DISPLAY"`, etc.).
    pub statement_sym: SymbolId,
    /// Source span.
    pub span: SourceSpan,
}

/// Payload of [`HirStmt::DebugTrace`] (`Milestone M13` NIST `DB` Debug Module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirDebugTraceStmt {
    /// Target `USE FOR DEBUGGING` declarative section.
    pub section: HirDeclRef,
    /// 6-character `DEBUG-LINE` text (1-based line number formatted for `PIC X(6)`).
    pub line_text: String,
    /// 30-character `DEBUG-NAME` subject identifier/procedure/file name.
    pub name_text: String,
    /// Up to 3 evaluated subscript expressions (`DEBUG-SUB-1..3`).
    pub subscripts: Vec<HirExpr>,
    /// Optional `DEBUG-CONTENTS` text/expression (`"START PROGRAM"`, `"PERFORM LOOP"`, `"FALL THROUGH"`, procedure target, or referenced variable value).
    pub contents: HirDebugContents,
    /// Source span.
    pub span: SourceSpan,
}

/// Source value written into `DEBUG-CONTENTS` on a [`HirDebugTraceStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirDebugContents {
    /// Fill `DEBUG-CONTENTS` with spaces.
    Spaces,
    /// Static ASCII status label (`"START PROGRAM"`, `"SORT INPUT"`, `"PERFORM LOOP"`, `"FALL THROUGH"`, target procedure name).
    StaticText(String),
    /// Dynamic runtime value of a referenced/modified `HirPlace`.
    PlaceValue(HirPlace),
}

/// Resolved target reference to a `USE [GLOBAL] AFTER ERROR/EXCEPTION` declarative section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirDeclRef {
    /// Compilation unit owning the declarative section.
    pub owner_unit: ProgramUnitId,
    /// Entry [`ProcedureId`] of the declarative section.
    pub proc_start: ProcedureId,
    /// Inclusive final [`ProcedureId`] of the declarative section.
    pub proc_end: ProcedureId,
}

/// Complete 5-slot candidate `USE AFTER ERROR/EXCEPTION` declarative bindings for a file I/O verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HirFileDeclSet {
    /// Specific `USE AFTER ERROR PROCEDURE ON file-name` (highest priority).
    pub on_file: Option<HirDeclRef>,
    /// `USE AFTER ERROR PROCEDURE ON INPUT`.
    pub on_input: Option<HirDeclRef>,
    /// `USE AFTER ERROR PROCEDURE ON OUTPUT`.
    pub on_output: Option<HirDeclRef>,
    /// `USE AFTER ERROR PROCEDURE ON I-O`.
    pub on_io: Option<HirDeclRef>,
    /// `USE AFTER ERROR PROCEDURE ON EXTEND`.
    pub on_extend: Option<HirDeclRef>,
}

/// File open mode in HIR (`INPUT`, `OUTPUT`, `I-O`, `EXTEND`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirOpenMode {
    /// Read-only `OPEN INPUT`.
    Input,
    /// Create/truncate `OPEN OUTPUT`.
    Output,
    /// Read-write `OPEN I-O`.
    Io,
    /// Append `OPEN EXTEND`.
    Extend,
}

/// Per-entry option on [`HirFileOpenEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirOpenOption {
    /// Standard `OPEN`.
    Standard,
    /// `OPEN ... WITH NO REWIND` (`"07"` on disk files).
    WithNoRewind,
    /// `OPEN INPUT ... REVERSED`.
    Reversed,
    /// `OPEN ... WITH LOCK` (exclusive file lock).
    WithLock,
    /// `OPEN ... SHARING WITH ALL OTHER`.
    SharingAllOther,
    /// `OPEN ... SHARING WITH NO OTHER`.
    SharingNoOther,
    /// `OPEN ... SHARING WITH READ ONLY`.
    SharingReadOnly,
}

/// Single file entry inside [`HirFileOpenStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileOpenEntry {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Requested [`HirOpenMode`].
    pub mode: HirOpenMode,
    /// Entry option modifier.
    pub option: HirOpenOption,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
}

/// Payload of [`HirStmt::FileOpen`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileOpenStmt {
    /// Ordered open entries executed left-to-right.
    pub entries: Vec<HirFileOpenEntry>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Per-file disposition on [`HirFileCloseEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirCloseDisposition {
    /// Standard `CLOSE`.
    Standard,
    /// `CLOSE ... WITH LOCK` (`"38"` permanent run-unit lock).
    WithLock,
    /// `CLOSE ... WITH NO REWIND` (`"07"` on disk files).
    WithNoRewind,
    /// `CLOSE ... REEL/UNIT` (`"07"` on disk files).
    ReelUnit,
    /// `CLOSE ... REEL/UNIT FOR REMOVAL` (`"07"` on disk files).
    ReelUnitForRemoval,
}

/// Single file entry inside [`HirFileCloseStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileCloseEntry {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Close disposition.
    pub disposition: HirCloseDisposition,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
}

/// Payload of [`HirStmt::FileClose`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileCloseStmt {
    /// Ordered close entries executed left-to-right.
    pub entries: Vec<HirFileCloseEntry>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Direction of a sequential `READ` in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirSeqReadDir {
    /// `READ NEXT`.
    Next,
    /// `READ PREVIOUS`.
    Previous,
}

/// Sequential vs random keyed `READ` mode in [`HirFileReadStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirReadMode {
    /// Sequential cursor `READ` (`AT END` / `NOT AT END`).
    Sequential {
        /// `Next` vs `Previous`.
        direction: HirSeqReadDir,
    },
    /// Keyed random `READ` (`INVALID KEY` / `NOT INVALID KEY`).
    Random {
        /// Key index (`0` = primary/relative key, `1..` = 1-based alternate key).
        key_index: u16,
        /// Optional explicit key source place when `KEY IS` names an external/subordinate field.
        key_place: Option<HirPlace>,
    },
}

/// Per-statement record lock modifier on [`HirFileReadStmt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirReadLockPolicy {
    /// Governed by connector `LOCK MODE`.
    Default,
    /// `READ ... WITH LOCK`.
    WithLock,
    /// `READ ... WITH KEPT LOCK`.
    WithKeptLock,
    /// `READ ... WITH NO LOCK`.
    WithNoLock,
    /// `READ ... IGNORING LOCK`.
    IgnoreLock,
}

/// Presence of an inline conditional handler clause (`AT END`, `INVALID KEY`, `AT EOP`, `ON EXCEPTION`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirInlineClausePresence {
    /// Inline clause omitted (fall back to `DECLARATIVES` / `FILE STATUS`).
    Absent,
    /// Inline clause explicitly written on the statement.
    Present,
}

/// Payload of [`HirStmt::FileRead`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileReadStmt {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Sequential vs random read mode.
    pub mode: HirReadMode,
    /// Record lock modifier.
    pub lock_policy: HirReadLockPolicy,
    /// Pre-lowered `MOVE fd_rec TO dst` statements executed on `0x` success (`READ ... INTO`).
    pub into_moves: Vec<HirStmt>,
    /// Whether an inline `AT END` or `INVALID KEY` handler clause was written.
    pub inline_presence: HirInlineClausePresence,
    /// `AT END` or `INVALID KEY` handler statements.
    pub on_condition: Vec<HirStmt>,
    /// `NOT AT END` or `NOT INVALID KEY` handler statements.
    pub not_on_condition: Vec<HirStmt>,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
    /// Statement source span.
    pub span: SourceSpan,
}

/// `BEFORE` vs `AFTER` advancing side on [`HirWriteAdvancing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirAdvancingSide {
    /// `BEFORE ADVANCING`.
    Before,
    /// `AFTER ADVANCING`.
    After,
}

/// Line/page advancing specification on [`HirFileWriteStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirWriteAdvancing {
    /// No explicit `ADVANCING` clause (`NoneRaw` or `DefaultSingleLine` depending on connector `print_mode`).
    Default,
    /// `{BEFORE | AFTER} ADVANCING expr LINE(S)`.
    Lines {
        /// `Before` vs `After`.
        side: HirAdvancingSide,
        /// Line count expression (`>= 0`).
        count: HirExpr,
    },
    /// `{BEFORE | AFTER} ADVANCING PAGE` (or top-of-page mnemonic).
    Page {
        /// `Before` vs `After`.
        side: HirAdvancingSide,
    },
}

/// Conditional handler flavor on [`HirFileWriteStmt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirWriteHandlerKind {
    /// No conditional handler clause specified.
    None,
    /// `[NOT] AT END-OF-PAGE` / `EOP`.
    EndOfPage,
    /// `[NOT] INVALID KEY`.
    InvalidKey,
}

/// Payload of [`HirStmt::FileWrite`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileWriteStmt {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Written `01` record place (determines record byte length when `depending_on_item` is absent).
    pub record_place: HirPlace,
    /// Pre-lowered `MOVE src TO record` statements executed before writing (`WRITE ... FROM`).
    pub pre_from_moves: Vec<HirStmt>,
    /// Advancing specification.
    pub advancing: HirWriteAdvancing,
    /// Conditional handler flavor.
    pub handler_kind: HirWriteHandlerKind,
    /// `AT EOP` or `INVALID KEY` handler statements.
    pub on_event: Vec<HirStmt>,
    /// `NOT AT EOP` or `NOT INVALID KEY` handler statements.
    pub not_on_event: Vec<HirStmt>,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Payload of [`HirStmt::FileRewrite`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileRewriteStmt {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Rewritten `01` record place.
    pub record_place: HirPlace,
    /// Pre-lowered `MOVE src TO record` statements executed before rewriting (`REWRITE ... FROM`).
    pub pre_from_moves: Vec<HirStmt>,
    /// Whether an inline `INVALID KEY` handler was written.
    pub inline_presence: HirInlineClausePresence,
    /// `INVALID KEY` handler statements.
    pub on_invalid_key: Vec<HirStmt>,
    /// `NOT INVALID KEY` handler statements.
    pub not_on_invalid_key: Vec<HirStmt>,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Payload of [`HirStmt::FileDelete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileDeleteStmt {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Whether an inline `INVALID KEY` handler was written.
    pub inline_presence: HirInlineClausePresence,
    /// `INVALID KEY` handler statements.
    pub on_invalid_key: Vec<HirStmt>,
    /// `NOT INVALID KEY` handler statements.
    pub not_on_invalid_key: Vec<HirStmt>,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Single target entry in [`HirFileDeletePhysicalStmt`] (`DELETE FILE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirDeletePhysicalEntry {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
}

/// Payload of [`HirStmt::FileDeletePhysical`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileDeletePhysicalStmt {
    /// Target files to physically remove.
    pub entries: Vec<HirDeletePhysicalEntry>,
    /// Whether an inline `ON EXCEPTION` / `INVALID KEY` handler was written.
    pub inline_presence: HirInlineClausePresence,
    /// `ON EXCEPTION` handler statements.
    pub on_exception: Vec<HirStmt>,
    /// `NOT ON EXCEPTION` handler statements.
    pub not_on_exception: Vec<HirStmt>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Relational operator on `START ... KEY IS ...` in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirStartRelation {
    /// `KEY = k`.
    Equal,
    /// `KEY > k`.
    Greater,
    /// `KEY >= k` (`NOT <`).
    GreaterOrEqual,
    /// `KEY < k`.
    Less,
    /// `KEY <= k` (`NOT >`).
    LessOrEqual,
}

/// Positioning mode on [`HirFileStartStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirStartMode {
    /// ISO 2023 `START file FIRST`.
    First,
    /// ISO 2023 `START file LAST`.
    Last,
    /// Key comparison positioning (`START file [KEY IS rel_op key [WITH SIZE n]]`).
    Relation {
        /// Comparison relation.
        relation: HirStartRelation,
        /// Key index (`0` = primary/relative key, `1..` = 1-based alternate key).
        key_index: u16,
        /// Optional explicit key source place (for partial/subordinate key prefixes).
        key_place: Option<Box<HirPlace>>,
        /// Optional explicit `WITH SIZE/LENGTH` expression.
        key_length: Option<Box<HirExpr>>,
    },
}

/// Payload of [`HirStmt::FileStart`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileStartStmt {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Positioning mode.
    pub mode: HirStartMode,
    /// Whether an inline `INVALID KEY` handler was written.
    pub inline_presence: HirInlineClausePresence,
    /// `INVALID KEY` handler statements.
    pub on_invalid_key: Vec<HirStmt>,
    /// `NOT INVALID KEY` handler statements.
    pub not_on_invalid_key: Vec<HirStmt>,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Payload of [`HirStmt::FileUnlock`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFileUnlockStmt {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Single sort/merge key field descriptor inside an `SD` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirSortKeyDesc {
    /// `Ascending` vs `Descending`.
    pub direction: HirSortDirection,
    /// Zero-based byte offset from the start of the `SD` record area.
    pub offset: u32,
    /// Key field byte width (`>= 1`).
    pub length: u32,
    /// Resolved key [`HirType`] (selects alphanumeric vs algebraic decimal/binary/float comparison).
    pub ty: HirType,
}

/// Duplicate-key tie-breaking order on [`HirSortFileStmt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirSortDuplicates {
    /// Unspecified order for equal keys.
    Arbitrary,
    /// `WITH DUPLICATES IN ORDER` (stable FIFO input order).
    InOrder,
}

/// File connector plus declarative bindings used in `SORT`/`MERGE` `USING` and `GIVING` phrases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirSortTransferFile {
    /// Target [`FileConnectorId`].
    pub file: FileConnectorId,
    /// Candidate `DECLARATIVES` bindings.
    pub decls: HirFileDeclSet,
}

/// Input source of [`HirSortFileStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirSortInput {
    /// `USING file-1 ...` automatic open-read-release-close pump.
    Using(Vec<HirSortTransferFile>),
    /// `INPUT PROCEDURE IS start [THRU end]`.
    Procedure {
        /// Starting [`ProcedureId`].
        start: ProcedureId,
        /// Inclusive ending [`ProcedureId`].
        end: ProcedureId,
    },
}

/// Output sink of [`HirSortFileStmt`] and [`HirMergeFileStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirSortOutput {
    /// `GIVING file-1 ...` automatic open-return-write-close pump.
    Giving(Vec<HirSortTransferFile>),
    /// `OUTPUT PROCEDURE IS start [THRU end]`.
    Procedure {
        /// Starting [`ProcedureId`].
        start: ProcedureId,
        /// Inclusive ending [`ProcedureId`].
        end: ProcedureId,
    },
}

/// Payload of [`HirStmt::SortFile`] (`Milestone M16`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirSortFileStmt {
    /// `SD` sort-file connector.
    pub sd_file: FileConnectorId,
    /// Ordered multi-key specifications.
    pub keys: Vec<HirSortKeyDesc>,
    /// `WITH DUPLICATES IN ORDER` policy.
    pub duplicates: HirSortDuplicates,
    /// Optional `COLLATING SEQUENCE` index into `HirProgram::alphabets`.
    pub collating_alphabet: Option<usize>,
    /// `USING` files or `INPUT PROCEDURE`.
    pub input: HirSortInput,
    /// `GIVING` files or `OUTPUT PROCEDURE`.
    pub output: HirSortOutput,
    /// Synthesized `SORT-RETURN` special-register [`DataItemId`].
    pub sort_return_item: Option<DataItemId>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Payload of [`HirStmt::MergeFile`] (`Milestone M16`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirMergeFileStmt {
    /// `SD` merge-file connector.
    pub sd_file: FileConnectorId,
    /// Ordered multi-key specifications.
    pub keys: Vec<HirSortKeyDesc>,
    /// Optional `COLLATING SEQUENCE` index into `HirProgram::alphabets`.
    pub collating_alphabet: Option<usize>,
    /// Pre-sorted `USING` input file streams.
    pub using_files: Vec<HirSortTransferFile>,
    /// `GIVING` files or `OUTPUT PROCEDURE`.
    pub output: HirSortOutput,
    /// Synthesized `SORT-RETURN` special-register [`DataItemId`].
    pub sort_return_item: Option<DataItemId>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Payload of [`HirStmt::ReleaseRecord`] (`Milestone M16`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirReleaseStmt {
    /// Owning `SD` [`FileConnectorId`].
    pub sd_file: FileConnectorId,
    /// Released `01` `SD` record place.
    pub record_place: HirPlace,
    /// Pre-lowered `MOVE src TO sd_record` statements (`RELEASE ... FROM`).
    pub pre_from_moves: Vec<HirStmt>,
    /// Synthesized `SORT-RETURN` special-register [`DataItemId`].
    pub sort_return_item: Option<DataItemId>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Payload of [`HirStmt::ReturnRecord`] (`Milestone M16`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirReturnStmt {
    /// Owning `SD` [`FileConnectorId`].
    pub sd_file: FileConnectorId,
    /// Pre-lowered `MOVE sd_record TO dst` statements (`RETURN ... INTO`).
    pub into_moves: Vec<HirStmt>,
    /// `AT END` handler statements.
    pub at_end: Vec<HirStmt>,
    /// `NOT AT END` handler statements.
    pub not_at_end: Vec<HirStmt>,
    /// Synthesized `SORT-RETURN` special-register [`DataItemId`].
    pub sort_return_item: Option<DataItemId>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Target register of [`HirStmt::DisplayEnv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirEnvTargetKind {
    /// `ENVIRONMENT-NAME` (`0`).
    EnvironmentName,
    /// `ENVIRONMENT-VALUE` (`1`).
    EnvironmentValue,
    /// `ARGUMENT-NUMBER` (`2`).
    ArgumentNumber,
    /// `COMMAND-LINE` (`3`).
    CommandLine,
}

/// Destination of [`HirSetPointerSpec`] (`USAGE POINTER`/`PROGRAM-POINTER` place or `ADDRESS OF` root slot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirPointerTarget {
    /// Stored 8-byte `Pointer` or `ProgramPointer` place (`SET ptr TO ...`).
    PointerPlace(HirPlace),
    /// Dynamic base-pointer slot of a `01`/`77` `BASED` or `LINKAGE` root (`SET ADDRESS OF item TO ...`).
    BaseAddressOf(DataItemId),
}

/// Pointer byte-stepping direction in `SET ptr/ADDRESS OF item {UP | DOWN} BY expr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirStepDir {
    /// Add byte offset (`UP BY`).
    Up,
    /// Subtract byte offset (`DOWN BY`).
    Down,
}

/// Right-hand pointer source or adjustment in [`HirSetPointerSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirPointerValue {
    /// `NULL` / `NULLS` (`0`).
    Null,
    /// Runtime address of an addressable [`HirPlace`] (`ADDRESS OF item`).
    AddressOf(HirPlace),
    /// Stored 8-byte pointer value loaded from another pointer [`HirPlace`].
    PointerPlace(HirPlace),
    /// Resolved subprogram/entry function address (`SET prog-ptr TO ENTRY / ADDRESS OF PROGRAM expr`).
    Entry(HirExpr),
    /// Byte-granular pointer arithmetic (`UP BY expr` / `DOWN BY expr`).
    StepBytes {
        /// Direction (`Up` vs `Down`).
        direction: HirStepDir,
        /// Byte delta expression.
        amount: HirExpr,
    },
}

/// Lowered specification for [`HirStmt::SetPointer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirSetPointerSpec {
    /// Destination pointer place(s) or `ADDRESS OF` root slot(s).
    pub targets: Vec<HirPointerTarget>,
    /// Pointer value or step delta applied to each target.
    pub value: HirPointerValue,
    /// Source span.
    pub span: SourceSpan,
}

/// Allocation size and layout source for [`HirAllocateSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirAllocateSize {
    /// Character/byte count expression (`ALLOCATE expr CHARACTERS ...`).
    CharCount(HirExpr),
    /// `01`/`77` `BASED` root item (`ALLOCATE based-item ...`, updating `ADDRESS OF based-item`).
    BasedItem(DataItemId),
}

/// Initialization mode for [`HirAllocateSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirAllocateInit {
    /// Uninitialized allocation (ISO 2023 Rules 8–9: `default_byte`/`0x00` + `NULL` pointer leaves on `BasedItem`).
    Uninitialized,
    /// `INITIALIZED` allocation (ISO 2023 Rules 6–7: `default_byte`/`0x00` on `CharCount`, `VALUE`/default image on `BasedItem`).
    InitializedDefault,
    /// `INITIALIZED TO expr` (`GnuCOBOL` byte/pattern fill on `CharCount`).
    InitializedTo(HirExpr),
}

/// Lowered specification for [`HirStmt::Allocate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirAllocateSpec {
    /// Byte count or `BASED` root item.
    pub size: HirAllocateSize,
    /// Initialization behavior.
    pub init: HirAllocateInit,
    /// Optional `RETURNING ptr` destination place.
    pub returning: Option<HirPlace>,
    /// Source span.
    pub span: SourceSpan,
}

/// Single release target inside [`HirStmt::Free`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirFreeTarget {
    /// `FREE based-item` or `FREE ADDRESS OF based-item` (frees heap block and resets base slot to `NULL`).
    BasedItem(DataItemId),
    /// `FREE ptr` (frees heap block and resets `ptr` to `NULL`).
    PointerPlace(HirPlace),
}

/// Destination of [`HirAcceptSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirAcceptTarget {
    /// `ACCEPT OMITTED` (consumes input and discards value).
    Omitted,
    /// Addressable COBOL target place.
    Place(HirPlace),
}

/// Source register or stream of [`HirAcceptSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirAcceptSourceKind {
    /// `CONSOLE` / `SYSIN` (`0`).
    ConsoleLine,
    /// `DATE` (`YYMMDD`, `1`).
    DateYymmdd,
    /// `DATE YYYYMMDD` (`YYYYMMDD`, `2`).
    DateYyyymmdd,
    /// `DAY` (`YYDDD`, `3`).
    DayYyddd,
    /// `DAY YYYYDDD` (`YYYYDDD`, `4`).
    DayYyyyddd,
    /// `DAY-OF-WEEK` (`1..=7`, `5`).
    DayOfWeek,
    /// `TIME` (`HHMMSShh`, `6`).
    TimeStandard8,
    /// `TIME` under ACU dialect (`HHMMSSuuuuuu` when target > 8 bytes, `7`).
    TimeAcu12,
    /// `MICROSECOND-TIME` (`HHMMSSuuuuuu`, `8`).
    MicrosecondTime12,
    /// `ARGUMENT-NUMBER` (`9`).
    ArgumentNumber,
    /// `ARGUMENT-VALUE` (`10`).
    ArgumentValue,
    /// `COMMAND-LINE` (`11`).
    CommandLine,
    /// `ENVIRONMENT-NAME` (`12`).
    EnvironmentName,
    /// `ENVIRONMENT-VALUE` (`13`).
    EnvironmentValue,
}

/// Boxed payload of [`HirStmt::Accept`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirAcceptSpec {
    /// Receiving place or `OMITTED`.
    pub target: HirAcceptTarget,
    /// Resolved `ACCEPT` source kind.
    pub source: HirAcceptSourceKind,
    /// `ON EXCEPTION` imperative body.
    pub on_exception: Vec<HirStmt>,
    /// `NOT ON EXCEPTION` imperative body.
    pub not_on_exception: Vec<HirStmt>,
    /// Source span.
    pub span: SourceSpan,
}

/// Delimiter rule on a [`HirStringSpec`] piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirStringDelim {
    /// `DELIMITED BY SIZE`.
    Size,
    /// `DELIMITED BY <expr>`.
    Pattern(HirExpr),
}

/// Boxed payload of [`HirStmt::StringVerb`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirStringSpec {
    /// Ordered `(source_expr, delimiter)` pairs.
    pub pieces: Vec<(HirExpr, HirStringDelim)>,
    /// Destination `INTO` place.
    pub into: HirPlace,
    /// Optional 1-based `WITH POINTER` integer place.
    pub pointer: Option<HirPlace>,
    /// `ON OVERFLOW` imperative body.
    pub on_overflow: Vec<HirStmt>,
    /// `NOT ON OVERFLOW` imperative body.
    pub not_on_overflow: Vec<HirStmt>,
    /// Source span.
    pub span: SourceSpan,
}

/// Delimiter repetition rule in [`HirUnstringSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirUnstringMode {
    /// Single delimiter occurrence.
    Single,
    /// `ALL` contiguous repeated occurrences.
    AllRepeated,
}

/// Single `INTO` target slot in [`HirUnstringSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirUnstringTargetSpec {
    /// Destination `INTO` place.
    pub into: HirPlace,
    /// Optional `DELIMITER IN` place.
    pub delimiter_in: Option<HirPlace>,
    /// Optional `COUNT IN` integer place.
    pub count_in: Option<HirPlace>,
}

/// Boxed payload of [`HirStmt::Unstring`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirUnstringSpec {
    /// Inspected source place.
    pub source: HirPlace,
    /// Ordered `(mode, pattern_expr)` delimiters (empty when `DELIMITED BY` is omitted).
    pub delimiters: Vec<(HirUnstringMode, HirExpr)>,
    /// Ordered `INTO` target triplets.
    pub targets: Vec<HirUnstringTargetSpec>,
    /// Optional 1-based `WITH POINTER` integer place.
    pub pointer: Option<HirPlace>,
    /// Optional `TALLYING IN` integer accumulator place.
    pub tallying: Option<HirPlace>,
    /// `ON OVERFLOW` imperative body.
    pub on_overflow: Vec<HirStmt>,
    /// `NOT ON OVERFLOW` imperative body.
    pub not_on_overflow: Vec<HirStmt>,
    /// Source span.
    pub span: SourceSpan,
}

/// Match qualifier in [`HirInspectSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirInspectQualifier {
    /// `CHARACTERS`.
    Characters,
    /// `ALL`.
    All,
    /// `LEADING`.
    Leading,
    /// `FIRST`.
    First,
    /// `TRAILING`.
    Trailing,
}

/// Optional `AFTER INITIAL` / `BEFORE INITIAL` window on an `INSPECT` clause.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HirInspectWindow {
    /// Optional `AFTER INITIAL` pattern expression.
    pub after: Option<HirExpr>,
    /// Optional `BEFORE INITIAL` pattern expression.
    pub before: Option<HirExpr>,
}

/// Single `TALLYING` sub-clause inside [`HirInspectAction::TallyAndReplace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirInspectTallyClause {
    /// Index into `counters`.
    pub counter_idx: usize,
    /// Match qualifier.
    pub qualifier: HirInspectQualifier,
    /// Pattern expression (`None` when `Characters`).
    pub pattern: Option<HirExpr>,
    /// Window bounds.
    pub window: HirInspectWindow,
}

/// Single `REPLACING` sub-clause inside [`HirInspectAction::TallyAndReplace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirInspectReplaceClause {
    /// Match qualifier.
    pub qualifier: HirInspectQualifier,
    /// Pattern expression (`None` when `Characters`).
    pub pattern: Option<HirExpr>,
    /// Replacement (`BY`) expression.
    pub by: HirExpr,
    /// Window bounds.
    pub window: HirInspectWindow,
}

/// Boxed payload of [`HirInspectAction::Converting`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirInspectConvertSpec {
    /// Source character expression.
    pub from: HirExpr,
    /// Replacement character expression.
    pub to: HirExpr,
    /// Window bounds.
    pub window: HirInspectWindow,
}

/// Action body of [`HirInspectSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirInspectAction {
    /// Unified Format 1/2/3 `TALLYING` and/or `REPLACING` pass.
    TallyAndReplace {
        /// Distinct `TALLYING` counter places.
        counters: Vec<HirPlace>,
        /// Ordered `TALLYING` sub-clauses.
        tallies: Vec<HirInspectTallyClause>,
        /// Ordered `REPLACING` sub-clauses.
        replaces: Vec<HirInspectReplaceClause>,
    },
    /// Format 4 `CONVERTING from TO to` (also used for `TRANSFORM`).
    Converting(Box<HirInspectConvertSpec>),
}

/// Boxed payload of [`HirStmt::Inspect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirInspectSpec {
    /// Inspected target place.
    pub target: HirPlace,
    /// `TallyAndReplace` or `Converting` action.
    pub action: HirInspectAction,
    /// Source span.
    pub span: SourceSpan,
}

/// Single key descriptor inside [`HirSortTableSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirSortTableKey {
    /// `Ascending` vs `Descending` comparison order.
    pub direction: HirSortDirection,
    /// Static byte offset of the key within a single table element (`0..stride`).
    pub rel_offset: u32,
    /// Static byte length of the key field.
    pub byte_len: u32,
    /// Key value [`HirType`] (algebraic numeric vs byte/collated character).
    pub ty: HirType,
}

/// Boxed payload of [`HirStmt::SortTable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirSortTableSpec {
    /// Base element-0 place of the sorted table (carrying any outer subscripts).
    pub table: HirPlace,
    /// Optional `DEPENDING ON` object [`DataItemId`] supplying dynamic element count.
    pub count_odo: Option<DataItemId>,
    /// Static maximum element count of the sorted dimension.
    pub max_count: u32,
    /// Single-element byte stride of the sorted dimension.
    pub stride: u32,
    /// Ordered comparison keys from major to minor.
    pub keys: Vec<HirSortTableKey>,
    /// Optional 256-byte non-numeric collation rank table.
    pub collating_ranks: Option<Vec<u8>>,
    /// Source span.
    pub span: SourceSpan,
}

/// Boxed payload of [`HirStmt::DivideRemainder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirDivideRemainderSpec {
    /// Dividend expression.
    pub dividend: HirExpr,
    /// Divisor expression.
    pub divisor: HirExpr,
    /// Quotient target place and rounding mode.
    pub quotient: (HirPlace, HirRounding),
    /// Remainder target place (`rem = dividend - quotient * divisor`).
    pub remainder: HirPlace,
    /// Optional `ON SIZE ERROR` statements.
    pub on_size_error: Vec<HirStmt>,
    /// Optional `NOT ON SIZE ERROR` statements.
    pub not_on_size_error: Vec<HirStmt>,
    /// Source span.
    pub span: SourceSpan,
}

/// `WITH FILLER` mode on [`HirStmt::Initialize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirInitFiller {
    /// Exclude unnamed/`FILLER` items.
    ExcludeFiller,
    /// Include `FILLER` items (`WITH FILLER`).
    IncludeFiller,
}

/// `TO VALUE` mode on [`HirStmt::Initialize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirInitValueMode {
    /// Default category zero/space fill.
    DefaultCategory,
    /// Restore Data Division `VALUE` clauses (`TO VALUE`).
    RestoreValue,
}

/// All eight ISO/IEC 1989:2023 rounding modes in [`HirStmt::AssignArith`] and [`HirStmt::DivideRemainder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirRounding {
    /// Truncate fractional excess (`TRUNCATION` or omitted `ROUNDED`).
    Truncate,
    /// Round to nearest; ties away from zero (default `ROUNDED` / `NEAREST-AWAY-FROM-ZERO`).
    NearestAwayFromZero,
    /// Round to nearest; ties to even digit (`NEAREST-EVEN`).
    NearestEven,
    /// Round to nearest; ties toward zero (`NEAREST-TOWARD-ZERO`).
    NearestTowardZero,
    /// Flag size error if any non-zero fractional digit is discarded (`PROHIBITED`).
    Prohibited,
    /// Round toward $+\infty$ (`TOWARD-GREATER`).
    TowardGreater,
    /// Round toward $-\infty$ (`TOWARD-LESSER`).
    TowardLesser,
    /// Round away from zero on any non-zero remainder (`AWAY-FROM-ZERO`).
    AwayFromZero,
}

/// Zoned overpunch & packed `IS NUMERIC` dialect convention on [`HirProgram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirSignConvention {
    /// `GnuCOBOL` / `gcobol` ASCII overpunch (`'p'..'y'`) and strict `COMP-3` sign check.
    AsciiGnu,
    /// COBOL-85 / `-fsign=ebcdic` overpunch (`'{'`/`'}'`/`'A'..'R'`) with `GnuCOBOL` `COMP-3` check.
    EbcdicOverpunch,
    /// IBM mainframe EBCDIC-mapped overpunch + permissive `0x0A..=0x0F` signed `COMP-3` check.
    IbmMainframe,
    /// Native `-finternal-ebcdic` storage (`0x40` space, `0xF0..=0xF9` unsigned, `0xC0..=0xC9` `+`, `0xD0..=0xD9` `-`).
    EbcdicNative,
}

/// Runtime internal character encoding on [`HirProgramFormatting`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HirInternalCharset {
    /// Native ASCII / UTF-8-compatible single-byte storage (`0x20` space).
    #[default]
    Ascii,
    /// Native `-finternal-ebcdic` single-byte storage (`0x40` space).
    Ebcdic,
}

/// Numeric `DISPLAY` width style on [`HirProgram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirDisplayStyle {
    /// ISO / `gcobol` `PICTURE`-digit width (`PIC 999 COMP-5` -> 3 digits).
    IsoPicWidth,
    /// `GnuCOBOL` `pretty-display` hardware width on `COMP-5` / `-fnotrunc` (`1->3, 2->5, 4->10, 8->20`).
    GnuPrettyWidth,
}

/// Per-program `SPECIAL-NAMES` and dialect formatting configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirProgramFormatting {
    /// Active decimal point byte (`b'.'` or `b','` under `DECIMAL-POINT IS COMMA`).
    pub decimal_char: u8,
    /// Active currency symbol (`'$'` by default).
    pub currency_symbol: char,
    /// Zoned & packed sign convention.
    pub sign_convention: HirSignConvention,
    /// Numeric `DISPLAY` formatting style.
    pub display_style: HirDisplayStyle,
    /// Optional `-fdefaultbyte` fill byte (`0..=255`) for uninitialized storage and `ALLOCATE`.
    pub default_byte: Option<u8>,
    /// Active internal single-byte charset (`Ascii` vs `Ebcdic`).
    pub internal_charset: HirInternalCharset,
}

impl Default for HirProgramFormatting {
    /// Returns default ASCII `GnuCOBOL` `.`/`$` formatting settings.
    fn default() -> Self {
        Self {
            decimal_char: b'.',
            currency_symbol: '$',
            sign_convention: HirSignConvention::AsciiGnu,
            display_style: HirDisplayStyle::GnuPrettyWidth,
            default_byte: None,
            internal_charset: HirInternalCharset::Ascii,
        }
    }
}

/// Output device selector on [`HirStmt::Display`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirDisplayDevice {
    /// Standard output (`SYSOUT` / `CONSOLE`).
    Stdout,
    /// Standard error (`SYSERR` / `STDERR` / `SYSPUNCH`).
    Stderr,
}

/// Line advancing mode on [`HirStmt::Display`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirAdvancing {
    /// Append trailing newline.
    AdvanceLine,
    /// Suppress trailing newline (`WITH NO ADVANCING`).
    NoAdvancing,
}

/// Loop termination check position in [`HirStmt::Loop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirLoopTest {
    /// `WITH TEST BEFORE`.
    Before,
    /// `WITH TEST AFTER`.
    After,
}

/// Return termination mode in [`HirStmt::Return`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirReturnMode {
    /// Terminate entire run unit (`STOP RUN`).
    StopRun,
    /// Return to caller or OS (`GOBACK` / `EXIT FUNCTION`).
    Goback,
    /// Return to caller when inside a `CALL`ed subprogram; no-op in main (`EXIT PROGRAM`).
    ExitProgram,
}

/// Distinguishes a `SECTION` entry block from a named paragraph block in [`HirParagraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirProcKind {
    /// Named paragraph (`PARA-1.`).
    Paragraph,
    /// Section header (`SEC-1 SECTION.`).
    SectionHeader,
}

/// Variably-located sibling offset mode on [`HirProgram`] (`odoslide`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirOdoSlideMode {
    /// Standard ISO / Micro Focus / `GnuCOBOL` fixed maximum offsets (`odoslide: no`).
    FixedOffsets,
    /// IBM `ODOSLIDE` sliding offsets after preceding `OCCURS DEPENDING ON` tables (`odoslide: yes`).
    SlideOffsets,
}

/// Out-of-line `PERFORM` frame return discipline on [`HirProgram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirPerformStyle {
    /// ISO stack-disciplined `PERFORM` return (`PerformStack`).
    IsoStack,
    /// IBM OS/VS continuation-slot return (`PerformContinuationSlot`).
    OsVsContinuation,
}

/// Program recursion attribute (`PROGRAM-ID. name IS RECURSIVE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirRecursivity {
    /// Standard non-recursive program.
    NonRecursive,
    /// `IS RECURSIVE` program.
    Recursive,
}

/// Program initial-state attribute (`PROGRAM-ID. name IS INITIAL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirInitialState {
    /// Retain `WORKING-STORAGE` and `ALTER` slots across calls.
    RetainStorage,
    /// Reset `WORKING-STORAGE` and `ALTER` slots on every entry (`IS INITIAL`).
    ResetInitial,
}

/// Resolved 256-byte ordinal rank table for a `SPECIAL-NAMES. ALPHABET` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirAlphabetTable {
    /// Alphabet identifier symbol.
    pub name: SymbolId,
    /// 256-entry byte-to-rank weight table (`ranks[byte]` gives collation rank `0..=255`).
    pub ranks: Vec<u8>,
}

/// Resolved 256-bit character membership bitmap for a `SPECIAL-NAMES. CLASS` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirCustomClass {
    /// Custom class identifier symbol.
    pub name: SymbolId,
    /// 256-bit lookup table (`(bitmap[byte >> 3] >> (byte & 7)) & 1 == 1`).
    pub bitmap: [u8; 32],
}

/// Resolved `SPECIAL-NAMES. SWITCH-n` declaration on [`HirProgram`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirSwitchSpec {
    /// Zero-based switch index (`0..=36`).
    pub index: u8,
    /// Optional user mnemonic for `SET mnemonic TO ON / OFF`.
    pub mnemonic: Option<SymbolId>,
    /// Optional `ON STATUS IS cond` symbol.
    pub on_cond: Option<SymbolId>,
    /// Optional `OFF STATUS IS cond` symbol.
    pub off_cond: Option<SymbolId>,
}

/// Resolved executable paragraph or section entry block in [`HirProgram`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirParagraph {
    /// Unique procedure handle.
    pub id: ProcedureId,
    /// Paragraph or section name symbol.
    pub name: SymbolId,
    /// `Paragraph` vs `SectionHeader`.
    pub kind: HirProcKind,
    /// Inclusive final `ProcedureId` of the enclosing `SECTION` (or `id` when unsectioned).
    pub section_end: ProcedureId,
    /// Lowered statements.
    pub statements: Vec<HirStmt>,
    /// Source span.
    pub span: SourceSpan,
}

/// Parameter passing convention (`BY REFERENCE`, `BY CONTENT`, `BY VALUE`) in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirPassingMode {
    /// Pass address of storage (`BY REFERENCE`, default).
    ByReference,
    /// Pass address of temporary copy (`BY CONTENT`).
    ByContent,
    /// Pass scalar value (`BY VALUE`).
    ByValue,
}

/// Formal parameter optionality in `PROCEDURE DIVISION USING [OPTIONAL] param`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirParamOptionality {
    /// Argument must be supplied (`EC-PROGRAM-ARG-OMITTED` otherwise).
    Required,
    /// Caller may pass `OMITTED` (`IS OMITTED` testable).
    Optional,
}

/// Source clause of a formal parameter on `PROCEDURE DIVISION` (`USING` vs `CHAINING`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirParamSource {
    /// `PROCEDURE DIVISION USING ...` (`LINKAGE SECTION` parameter).
    Using,
    /// `PROCEDURE DIVISION CHAINING ...` (command-line argument copied into `WORKING-STORAGE`).
    Chaining,
}

/// Actual argument passing convention in [`HirCallSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirCallArg {
    /// `OMITTED` parameter (passes null pointer and size `0`).
    Omitted,
    /// `BY REFERENCE` addressable data item.
    ByReference(HirPlace),
    /// `BY CONTENT` copied expression or literal.
    ByContent(HirExpr),
    /// `BY VALUE` scalar or pointer value expression.
    ByValue(HirExpr),
}

/// `RETURNING` / `GIVING` disposition in [`HirCallSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirCallReturning {
    /// No `RETURNING` clause (`RETURN-CODE` updated normally).
    ImplicitReturnCode,
    /// `RETURNING OMITTED` / `RETURNING NOTHING` (caller `RETURN-CODE` preserved).
    Omitted,
    /// `RETURNING target` (stores callee `i32` return value into `target`).
    Place(HirPlace),
}

/// Boxed payload of [`HirStmt::Call`] (`Milestone M11`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirCallSpec {
    /// Callee literal, alphanumeric identifier, or `PROGRAM-POINTER` expression.
    pub callee: HirExpr,
    /// Ordered `USING` arguments.
    pub args: Vec<HirCallArg>,
    /// `RETURNING` disposition.
    pub returning: HirCallReturning,
    /// `ON EXCEPTION` / `ON OVERFLOW` handler body.
    pub on_exception: Vec<HirStmt>,
    /// `NOT ON EXCEPTION` / `NOT ON OVERFLOW` handler body.
    pub not_on_exception: Vec<HirStmt>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Length variability of a formal `LINKAGE` parameter (`Fixed` vs `PIC X ANY LENGTH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirParamLengthMode {
    /// Fixed static byte size.
    Fixed,
    /// `ANY LENGTH` — inherits dynamic byte length from caller argument metadata.
    AnyLength,
}

/// Resolved formal parameter in `PROCEDURE DIVISION USING ...`, `CHAINING ...`, or `ENTRY ... USING ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirUsingParam {
    /// `USING` vs `CHAINING` binding source.
    pub source: HirParamSource,
    /// Parameter passing convention.
    pub mode: HirPassingMode,
    /// Required vs `OPTIONAL`.
    pub optionality: HirParamOptionality,
    /// Fixed vs `ANY LENGTH` dynamic sizing.
    pub length_mode: HirParamLengthMode,
    /// Resolved `LINKAGE` or `WORKING-STORAGE` root [`DataItemId`].
    pub item: DataItemId,
    /// Source span.
    pub span: SourceSpan,
}

/// Global vs local visibility of a [`HirDeclarativeRule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirGlobalFlag {
    /// Visible only within the declaring compilation unit.
    Local,
    /// `USE GLOBAL` — inherited by contained nested programs.
    Global,
}

/// File identity key matched by a `USE AFTER ERROR PROCEDURE ON file-name` rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HirDeclarativeFileKey {
    /// Unit-scoped (or `GLOBAL`) file connector `(owner_unit, owner_fc)`.
    UnitOwned(ProgramUnitId, FileConnectorId),
    /// `FD ... EXTERNAL` file connector name.
    External(String),
}

/// Trigger condition of a [`HirDeclarativeRule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirDeclarativeTrigger {
    /// `USE AFTER ERROR PROCEDURE ON file-1 ...`.
    Files(Vec<HirDeclarativeFileKey>),
    /// `USE AFTER ERROR PROCEDURE ON {INPUT | OUTPUT | I-O | EXTEND}`.
    OpenMode(HirOpenMode),
}

/// Resolved `USE [GLOBAL] AFTER STANDARD {ERROR | EXCEPTION} PROCEDURE` rule on [`HirProgram`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirDeclarativeRule {
    /// Reference to the declarative section procedures.
    pub target: HirDeclRef,
    /// Local vs `GLOBAL` inheritance flag.
    pub visibility: HirGlobalFlag,
    /// File or open-mode trigger condition.
    pub trigger: HirDeclarativeTrigger,
}

/// Resolved `PROGRAM-ID` unit in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirProgram {
    /// Unique unit handle.
    pub id: ProgramUnitId,
    /// Lexical scope ID.
    pub scope: ScopeId,
    /// Program name symbol.
    pub name: SymbolId,
    /// Program-level `SPECIAL-NAMES` and dialect formatting rules.
    pub formatting: HirProgramFormatting,
    /// Out-of-line `PERFORM` return discipline (`IsoStack` vs `OsVsContinuation`).
    pub perform_style: HirPerformStyle,
    /// Variably-located ODO offset mode (`FixedOffsets` vs `SlideOffsets`).
    pub odo_slide: HirOdoSlideMode,
    /// Program recursion mode (`NonRecursive` vs `Recursive`).
    pub recursivity: HirRecursivity,
    /// Program initial-state mode (`RetainStorage` vs `ResetInitial`).
    pub initial_state: HirInitialState,
    /// Declared `SPECIAL-NAMES. ALPHABET` collation rank tables.
    pub alphabets: Vec<HirAlphabetTable>,
    /// Optional active `PROGRAM COLLATING SEQUENCE` index into `alphabets`.
    pub collating_sequence: Option<usize>,
    /// Declared `SPECIAL-NAMES. CLASS` 256-bit membership bitmaps.
    pub custom_classes: Vec<HirCustomClass>,
    /// Declared `SPECIAL-NAMES. SWITCH-0..36` bindings.
    pub switches: Vec<HirSwitchSpec>,
    /// Resolved file connectors.
    pub files: Vec<HirFileConnector>,
    /// Resolved Data Division items (indexed by local order).
    pub data_items: Vec<HirDataItem>,
    /// Resolved level-`88` condition definitions.
    pub conditions_88: Vec<HirCondition88>,
    /// Resolved `RD` report handles.
    pub reports: Vec<(ReportId, SymbolId)>,
    /// Lowered Milestone M18 declarative report plans.
    pub report_plans: Vec<HirReportPlan>,
    /// Lowered Milestone M19 addressable screen plans.
    pub screen_plans: Vec<HirScreenPlan>,
    /// Lowered Milestone M19 `CURSOR` and `CRT STATUS` special-register bindings.
    pub screen_special_regs: HirScreenSpecialRegs,
    /// Resolved `PROCEDURE DIVISION USING` / `CHAINING` formal parameters.
    pub using_params: Vec<HirUsingParam>,
    /// Optional resolved `PROCEDURE DIVISION RETURNING` target item.
    pub returning_item: Option<DataItemId>,
    /// Entry statements before the first named paragraph.
    pub entry_statements: Vec<HirStmt>,
    /// Ordered resolved paragraphs and sections (normal procedures followed by declarative procedures).
    pub procedures: Vec<HirParagraph>,
    /// Exclusive upper `ProcedureId` limit for sequential `PROCEDURE DIVISION` fallthrough (`Some(first_decl_pid)` when declaratives exist).
    pub fallthrough_limit_pid: Option<ProcedureId>,
    /// Active (local + inherited `GLOBAL`) file `DECLARATIVES` rules.
    pub declaratives: Vec<HirDeclarativeRule>,
    /// Contained nested programs.
    pub nested: Vec<HirProgram>,
    /// Source span.
    pub span: SourceSpan,
}

/// Resolved `FUNCTION-ID` User-Defined Function in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirFunction {
    /// Internal function name symbol.
    pub name: SymbolId,
    /// External linkage symbol (`AS "ext"` if present, else canonical uppercase `name`).
    pub external_name: SymbolId,
    /// Backing program-like frame & body.
    pub body: HirProgram,
}

/// Compile-time `SOURCE-COMPUTER ... WITH DEBUGGING MODE` status on [`HirM13UnitSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirDebugCompileMode {
    /// `WITH DEBUGGING MODE` omitted (declarative `USE FOR DEBUGGING` sections dormant at runtime).
    Disabled,
    /// `WITH DEBUGGING MODE` enabled (gated at runtime by `COB_SET_DEBUG` / active debug flag).
    WithDebuggingMode,
}

/// Resolved `USE [GLOBAL] AFTER EXCEPTION CONDITION ec-name` rule on [`HirM13UnitSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirEcDeclarativeRule {
    /// Declarative section procedure range.
    pub target: HirDeclRef,
    /// Local vs `GLOBAL` visibility.
    pub visibility: HirGlobalFlag,
    /// Matched `ExceptionCode::to_abi_id()` codes (`0` = `EC-ALL`, category codes match descendants with lower specificity than leaf codes).
    pub ec_codes: Vec<u16>,
}

/// Per-unit Milestone M13 exception-declarative and NIST `DB` debug metadata stored on [`HirModule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirM13UnitSpec {
    /// Owning `ProgramUnitId`.
    pub unit_id: ProgramUnitId,
    /// `WITH DEBUGGING MODE` compile-time status.
    pub debugging_mode: HirDebugCompileMode,
    /// Synthesized `01 DEBUG-ITEM` root `DataItemId` when `USE FOR DEBUGGING` or `DEBUG-ITEM` is used.
    pub debug_item: Option<DataItemId>,
    /// Active (local + inherited `GLOBAL`) `EC-*` exception declarative rules.
    pub ec_declaratives: Vec<HirEcDeclarativeRule>,
    /// Resolved `USE FOR DEBUGGING ON file-name` hooks `(file_id, decl_ref)`.
    pub file_debug_hooks: Vec<(FileConnectorId, HirDeclRef)>,
}

/// Resolved OO `CLASS-ID` or `INTERFACE-ID` metadata in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirClass {
    /// Class or interface name symbol.
    pub name: SymbolId,
    /// Superclasses / superinterfaces.
    pub inherits: Vec<SymbolId>,
    /// Lowered factory & instance method bodies.
    pub methods: Vec<HirProgram>,
    /// Optional Milestone M20 runtime OO class descriptor (`Some` for `CLASS-ID`, `None` for `INTERFACE-ID`).
    pub oo_spec: Option<HirOoClassSpec>,
    /// Source span.
    pub span: SourceSpan,
}

/// Complete lowered High-Level IR module for a compilation group.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HirModule {
    /// Top-level `PROGRAM-ID` units.
    pub programs: Vec<HirProgram>,
    /// Top-level `FUNCTION-ID` units.
    pub functions: Vec<HirFunction>,
    /// Top-level OO `CLASS-ID` and `INTERFACE-ID` units.
    pub classes: Vec<HirClass>,
    /// Per-unit Milestone M13 exception & NIST `DB` debug specifications.
    pub m13_units: Vec<HirM13UnitSpec>,
}

/// Typestate-verified wrapper guaranteeing a [`HirModule`] passed `coblang-sema` without hard errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHirModule(pub HirModule);
