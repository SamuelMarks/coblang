//! Typed AST nodes for the COBOL `DATA DIVISION` (`FILE`, `WORKING-STORAGE`, `LOCAL-STORAGE`, `LINKAGE`, `REPORT`, and `SCREEN` sections).

use coblang_common::{LevelNumber, SourceSpan, SymbolId};

/// Distinguishes `FD` (File Description) from `SD` (Sort-Merge File Description).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FdOrSd {
    /// Standard file description (`FD`).
    Fd,
    /// Sort-merge file description (`SD`).
    Sd,
}

/// Nested visibility of a file connector, data item, or report (`IS GLOBAL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlobalVisibility {
    /// Visible only in the declaring unit.
    Local,
    /// Visible to contained nested programs (`IS GLOBAL`).
    Global,
}

/// Cross-module shared storage linkage (`IS EXTERNAL [AS "name"]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExternalLinkage {
    /// Private to the compilation unit.
    Internal,
    /// Shared external segment (`IS EXTERNAL`), with optional external alias literal.
    External(Option<SymbolId>),
}

/// `RECORD CONTAINS` / `RECORD IS VARYING` size specification on an `FD` or `SD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordSizeClause {
    /// Fixed or static range `RECORD CONTAINS min [TO max] CHARACTERS`.
    Contains {
        /// Minimum (or exact) byte length.
        min_bytes: u32,
        /// Optional maximum byte length.
        max_bytes: Option<u32>,
    },
    /// Variable-length `RECORD IS VARYING [IN SIZE] [[FROM min] [TO max]] [DEPENDING ON data-name]`.
    Varying {
        /// Optional minimum byte length.
        min_bytes: Option<u32>,
        /// Optional maximum byte length.
        max_bytes: Option<u32>,
        /// Optional `DEPENDING ON` qualified identifier.
        depending_on: Option<Vec<SymbolId>>,
    },
}

/// `LINAGE IS ...` logical page layout specification on a sequential `FD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinageSpec {
    /// Body line count literal or data-name path.
    pub body_lines: Vec<SymbolId>,
    /// Optional `WITH FOOTING AT` line number or data-name path.
    pub footing_at: Option<Vec<SymbolId>>,
    /// Optional `LINES AT TOP` count or data-name path.
    pub lines_at_top: Option<Vec<SymbolId>>,
    /// Optional `LINES AT BOTTOM` count or data-name path.
    pub lines_at_bottom: Option<Vec<SymbolId>>,
    /// Source span.
    pub span: SourceSpan,
}

/// `FD` or `SD` file description entry together with its associated `01` record descriptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdSdEntry {
    /// `FD` vs `SD` classification.
    pub kind: FdOrSd,
    /// File connector symbol matching a `SELECT` entry.
    pub file_name: SymbolId,
    /// `IS EXTERNAL` attribute.
    pub external: ExternalLinkage,
    /// `IS GLOBAL` attribute.
    pub global: GlobalVisibility,
    /// Optional `RECORD CONTAINS` / `RECORD IS VARYING` specification.
    pub record_size: Option<RecordSizeClause>,
    /// Optional `LINAGE` page specification.
    pub linage: Option<LinageSpec>,
    /// Associated `REPORT IS / REPORTS ARE` report names.
    pub reports: Vec<SymbolId>,
    /// Associated `REPORT IS / REPORTS ARE` `(report_name, span)` pairs.
    pub report_spans: Vec<(SymbolId, SourceSpan)>,
    /// Optional span of `LABEL RECORDS` clause for dialect diagnostics.
    pub label_records_span: Option<SourceSpan>,
    /// Optional span of `DATA RECORD(S)` clause for dialect diagnostics.
    pub data_records_span: Option<SourceSpan>,
    /// Optional span of `RECORD CONTAINS` clause for `LINE SEQUENTIAL` diagnostics.
    pub record_clause_span: Option<SourceSpan>,
    /// Span immediately following the `FD`/`SD` header period (for missing-record diagnostics).
    pub after_fd_span: SourceSpan,
    /// Associated `CODE-SET IS` alphabet name.
    pub code_set: Option<SymbolId>,
    /// Child record description entries (`01`..`88`).
    pub records: Vec<DataEntry>,
    /// Source span of the `FD`/`SD` header.
    pub span: SourceSpan,
}

/// Explicit identifier vs implicit/explicit `FILLER` on a Data Division entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataEntryName {
    /// Named data item, condition-name, or constant.
    Named(SymbolId),
    /// Unnamed or explicit `FILLER` item.
    Filler,
}

/// Exhaustive COBOL `USAGE` storage representation (all 28 ISO/IBM/MF/`GnuCOBOL` usages).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UsageKind {
    /// `USAGE DISPLAY` (character / zoned decimal).
    Display,
    /// `USAGE DISPLAY-1` (DBCS).
    Display1,
    /// `USAGE NATIONAL` (`UTF-16`).
    National,
    /// `USAGE UTF-8`.
    Utf8,
    /// `USAGE BINARY` / `COMP` / `COMP-4`.
    Binary,
    /// `USAGE COMP-5` (native binary full bit range).
    Comp5,
    /// `USAGE PACKED-DECIMAL` / `COMP-3` (BCD with sign nibble).
    PackedDecimal,
    /// `USAGE COMP-6` (unsigned packed BCD).
    Comp6,
    /// `USAGE COMP-X` (big-endian unsigned raw binary bytes).
    CompX,
    /// `USAGE COMP-N` (native unsigned raw binary bytes).
    CompN,
    /// `USAGE COMP-1` / `FLOAT-SHORT` (IEEE `f32`).
    FloatShort,
    /// `USAGE COMP-2` / `FLOAT-LONG` (IEEE `f64`).
    FloatLong,
    /// `USAGE FLOAT-EXTENDED` (IEEE `f128`).
    FloatExtended,
    /// `USAGE FLOAT-DECIMAL-16` (IEEE 754 `decimal64`).
    FloatDecimal16,
    /// `USAGE FLOAT-DECIMAL-34` (IEEE 754 `decimal128`).
    FloatDecimal34,
    /// `USAGE BINARY-CHAR SIGNED` (`i8`).
    BinaryCharSigned,
    /// `USAGE BINARY-CHAR UNSIGNED` (`u8`).
    BinaryCharUnsigned,
    /// `USAGE BINARY-SHORT SIGNED` (`i16`).
    BinaryShortSigned,
    /// `USAGE BINARY-SHORT UNSIGNED` (`u16`).
    BinaryShortUnsigned,
    /// `USAGE BINARY-LONG SIGNED` (`i32`).
    BinaryLongSigned,
    /// `USAGE BINARY-LONG UNSIGNED` (`u32`).
    BinaryLongUnsigned,
    /// `USAGE BINARY-DOUBLE SIGNED` (`i64`).
    BinaryDoubleSigned,
    /// `USAGE BINARY-DOUBLE UNSIGNED` (`u64`).
    BinaryDoubleUnsigned,
    /// `USAGE BINARY-C-LONG SIGNED` (platform C `long`).
    BinaryCLongSigned,
    /// `USAGE BINARY-C-LONG UNSIGNED` (platform C `unsigned long`).
    BinaryCLongUnsigned,
    /// `USAGE BIT` (`PIC 1` boolean/bit string).
    Bit,
    /// `USAGE INDEX`.
    Index,
    /// `USAGE POINTER` (data pointer).
    Pointer,
    /// `USAGE PROGRAM-POINTER`.
    ProgramPointer,
    /// `USAGE FUNCTION-POINTER`.
    FunctionPointer,
    /// `USAGE OBJECT REFERENCE [class-name]`.
    ObjectReference(Option<SymbolId>),
}

/// Sign position (`LEADING` vs `TRAILING`) in `SIGN IS ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignPlacement {
    /// Sign on most-significant digit (`LEADING`).
    Leading,
    /// Sign on least-significant digit (`TRAILING`).
    Trailing,
}

/// Sign byte separation (`SEPARATE CHARACTER` vs embedded overpunch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignSeparation {
    /// Overpunched into digit byte.
    Embedded,
    /// Dedicated `+` / `-` byte (`SEPARATE CHARACTER`).
    SeparateCharacter,
}

/// `SIGN IS {LEADING | TRAILING} [SEPARATE CHARACTER]` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignClause {
    /// Leading vs trailing position.
    pub placement: SignPlacement,
    /// Embedded vs separate character.
    pub separation: SignSeparation,
}

/// Sort direction for `ASCENDING` / `DESCENDING KEY IS` on `OCCURS` or `SORT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyOrder {
    /// Ascending key order.
    Ascending,
    /// Descending key order.
    Descending,
}

/// Key specification inside an `OCCURS` table declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccursKeySpec {
    /// Ascending or descending order.
    pub order: KeyOrder,
    /// Key field paths.
    pub keys: Vec<Vec<SymbolId>>,
}

/// Upper bound of an `OCCURS` dimension (`fixed/max` vs `UNBOUNDED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OccursUpperBound {
    /// Finite maximum element count.
    Bounded(u32),
    /// ISO/IBM `OCCURS ... TO UNBOUNDED DEPENDING ON ...`.
    Unbounded,
}

/// `OCCURS [min TO] {max | UNBOUNDED} [TIMES] [DEPENDING ON odo] [KEY ...] [INDEXED BY ...]` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccursClause {
    /// Minimum occurrences (equals `max` for fixed-length tables).
    pub min: u32,
    /// Maximum occurrences or `Unbounded`.
    pub max: OccursUpperBound,
    /// Optional `DEPENDING ON` qualified object path.
    pub depending_on: Option<Vec<SymbolId>>,
    /// Declared `ASCENDING`/`DESCENDING KEY` clauses for `SEARCH ALL` / table `SORT`.
    pub keys: Vec<OccursKeySpec>,
    /// Implicitly defined `INDEXED BY` index-names.
    pub indexed_by: Vec<SymbolId>,
    /// Source span of the `OCCURS` clause.
    pub span: SourceSpan,
}

/// `SYNCHRONIZED [LEFT | RIGHT]` alignment specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncAlignment {
    /// Unaligned byte packing (default).
    Unsynchronized,
    /// `SYNCHRONIZED` (unspecified side).
    SyncDefault,
    /// `SYNCHRONIZED LEFT`.
    SyncLeft,
    /// `SYNCHRONIZED RIGHT`.
    SyncRight,
}

/// `JUSTIFIED RIGHT` alignment mode on alphanumeric/alphabetic/national items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JustificationMode {
    /// Standard left-justified character storage.
    StandardLeft,
    /// `JUSTIFIED [RIGHT]` right-aligned character storage.
    JustifiedRight,
}

/// `BLANK WHEN ZERO` editing specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlankWhenZeroMode {
    /// Normal zero formatting.
    Normal,
    /// Fill entire field with spaces when value is zero (`BLANK WHEN ZERO`).
    BlankWhenZero,
}

/// Literal or figurative atom inside a `VALUE` clause or `88` condition range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueAtom {
    /// String/hex/national/bit literal bytes + raw display text symbol.
    String {
        /// Raw decoded bytes.
        bytes: Vec<u8>,
        /// Prefix kind (`Plain`, `HexX`, `NationalN`, etc.).
        prefix: crate::token::StringPrefix,
    },
    /// Signed or unsigned integer literal symbol.
    Integer(SymbolId),
    /// Signed or unsigned decimal literal (`digits`, `scale`, `sign`).
    Decimal {
        /// Unscaled digit string symbol.
        digits: SymbolId,
        /// Fractional digit count.
        scale: coblang_common::ScaleFactor,
        /// Explicit sign.
        sign: crate::token::LiteralSign,
    },
    /// Scientific floating-point literal symbol.
    Float(SymbolId),
    /// Figurative constant (`ZERO`, `SPACE`, `HIGH-VALUE`, `LOW-VALUE`, `QUOTE`, `NULL`, `ALL lit`).
    Figurative(FigurativeValue),
    /// Symbolic character or `78` level constant identifier.
    Identifier(SymbolId),
}

/// Figurative constant classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FigurativeValue {
    /// `ZERO` / `ZEROS` / `ZEROES`.
    Zero,
    /// `SPACE` / `SPACES`.
    Space,
    /// `HIGH-VALUE` / `HIGH-VALUES`.
    HighValue,
    /// `LOW-VALUE` / `LOW-VALUES`.
    LowValue,
    /// `QUOTE` / `QUOTES`.
    Quote,
    /// `NULL` / `NULLS`.
    Null,
    /// `ALL <literal>` repeating pattern.
    All(Vec<u8>),
}

/// Single value or inclusive `low THRU high` range inside `VALUE IS ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueRange {
    /// Single value or lower bound.
    pub low: ValueAtom,
    /// Optional `THRU` / `THROUGH` upper bound (for `88` condition names).
    pub high: Option<ValueAtom>,
    /// Span of this value or range.
    pub span: SourceSpan,
}

/// `VALUE IS ...` and `WHEN SET TO FALSE IS ...` clause on a Data Division entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueClause {
    /// Primary value(s) or `88` condition ranges.
    pub values: Vec<ValueRange>,
    /// Optional ISO 2002+ `WHEN SET TO FALSE IS atom` on level-`88` items.
    pub false_value: Option<ValueAtom>,
    /// Source span of the `VALUE` clause.
    pub span: SourceSpan,
}

/// `66 name RENAMES start-item [THRU end-item]` range specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamesRange {
    /// Qualified path of first renamed item.
    pub from: Vec<SymbolId>,
    /// Optional qualified path of inclusive ending item (`THRU` / `THROUGH`).
    pub thru: Option<Vec<SymbolId>>,
    /// Source span.
    pub span: SourceSpan,
}

/// Dynamic/special storage flags (`BASED`, `ANY LENGTH`, `DYNAMIC LENGTH`, `IS TYPEDEF`, `CONSTANT`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageLinkageFlags {
    /// `IS EXTERNAL` attribute.
    pub external: ExternalLinkage,
    /// `IS GLOBAL` attribute.
    pub global: GlobalVisibility,
    /// Allocation residency (`Static`, `Based`, `Constant`).
    pub residency: ResidencyKind,
    /// Length variability (`Fixed`, `AnyLength`, `DynamicLength`).
    pub length_mode: LengthVariability,
    /// Type definition / instantiation (`Instance`, `Typedef`, `TypeRef(sym)`, `SameAs(path)`).
    pub type_role: TypeRole,
    /// Optional `IS SPECIAL-NAMES {CRT STATUS | CURSOR}` binding.
    pub special_names_role: Option<super::report_screen::SpecialNamesDataRole>,
}

impl Default for StorageLinkageFlags {
    /// Returns standard concrete fixed-length private storage flags.
    fn default() -> Self {
        Self {
            external: ExternalLinkage::Internal,
            global: GlobalVisibility::Local,
            residency: ResidencyKind::AutomaticOrStatic,
            length_mode: LengthVariability::Fixed,
            type_role: TypeRole::DataInstance,
            special_names_role: None,
        }
    }
}

/// Memory allocation residency of a Data Division item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResidencyKind {
    /// Standard section-allocated storage (`WORKING-STORAGE`, `LOCAL-STORAGE`, `LINKAGE`, `FILE`).
    AutomaticOrStatic,
    /// Heap/pointer-backed `BASED` item (`ALLOCATE` / `SET ADDRESS OF`).
    Based,
    /// Compile-time `01 name CONSTANT [IS GLOBAL] AS literal` or level `78`.
    CompileTimeConstant,
}

/// Elementary item length variability (`ANY LENGTH` / `DYNAMIC LENGTH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LengthVariability {
    /// Compile-time fixed byte/character count.
    Fixed,
    /// `ANY LENGTH` `LINKAGE` / `RETURNING` item.
    AnyLength,
    /// ISO 2014+ `DYNAMIC LENGTH` item.
    DynamicLength,
}

/// ISO 2002+ `TYPEDEF` / `USAGE TYPE` / `SAME AS` role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRole {
    /// Regular data item instance.
    DataInstance,
    /// `IS TYPEDEF [STRONG]` type template.
    Typedef,
    /// `TYPE [TO] type-name` instantiation.
    TypeInstance(SymbolId),
    /// `SAME AS data-name` layout clone.
    SameAs(Vec<SymbolId>),
}

/// Clause multiplicity counters captured during arbitrary-order Data Division parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DataClauseCounts {
    /// Number of `PIC`/`PICTURE` clauses on this entry.
    pub pic_count: u8,
    /// Number of `USAGE` clauses on this entry.
    pub usage_count: u8,
    /// Number of `OCCURS` clauses on this entry.
    pub occurs_count: u8,
    /// Number of `VALUE` clauses on this entry.
    pub value_count: u8,
    /// Number of `REDEFINES` clauses on this entry.
    pub redefines_count: u8,
}

/// Single Data Division entry (`01`..`49`, `66`, `77`, `78`, `88`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataEntry {
    /// Validated COBOL [`LevelNumber`].
    pub level: LevelNumber,
    /// Item name or `FILLER`.
    pub name: DataEntryName,
    /// Optional `REDEFINES target-name` (immediately following item name per ISO standard).
    pub redefines: Option<SymbolId>,
    /// Optional `66 ... RENAMES` range.
    pub renames: Option<RenamesRange>,
    /// Optional raw `PICTURE` string symbol + span.
    pub picture: Option<(SymbolId, SourceSpan)>,
    /// Optional explicit `USAGE` clause.
    pub usage: Option<UsageKind>,
    /// Optional `SIGN IS ...` clause.
    pub sign: Option<SignClause>,
    /// Optional `OCCURS` table specification.
    pub occurs: Option<OccursClause>,
    /// `SYNCHRONIZED` alignment mode.
    pub sync: SyncAlignment,
    /// `JUSTIFIED` mode.
    pub justified: JustificationMode,
    /// `BLANK WHEN ZERO` mode.
    pub blank_when_zero: BlankWhenZeroMode,
    /// Optional `VALUE` / `88` condition values / `78` constant value.
    pub value: Option<ValueClause>,
    /// Storage residency, visibility, and typedef flags.
    pub flags: StorageLinkageFlags,
    /// Clause repetition counts for duplicate-clause semantic checks.
    pub clause_counts: DataClauseCounts,
    /// Source span of the entire entry.
    pub span: SourceSpan,
}

pub use super::report_screen::{RdEntry, ReportGroupEntry, ReportGroupType, ScreenEntry};

/// Complete `DATA DIVISION` AST node across all six COBOL data sections.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DataDivision {
    /// `FILE SECTION` `FD` and `SD` entries.
    pub file_section: Vec<FdSdEntry>,
    /// `WORKING-STORAGE SECTION` entries.
    pub working_storage: Vec<DataEntry>,
    /// `LOCAL-STORAGE SECTION` entries.
    pub local_storage: Vec<DataEntry>,
    /// `LINKAGE SECTION` entries.
    pub linkage: Vec<DataEntry>,
    /// `REPORT SECTION` `RD` entries.
    pub report_section: Vec<RdEntry>,
    /// `SCREEN SECTION` entries.
    pub screen_section: Vec<ScreenEntry>,
    /// Source span if explicitly present.
    pub span: Option<SourceSpan>,
}
