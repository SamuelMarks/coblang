//! Typed AST nodes for the COBOL `ENVIRONMENT DIVISION` (`CONFIGURATION` and `INPUT-OUTPUT` sections).

use coblang_common::{SourceSpan, SymbolId};

/// Compile-time `WITH DEBUGGING MODE` setting on `SOURCE-COMPUTER`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DebuggingMode {
    /// Debugging sections and `'D'` indicator hooks disabled.
    Disabled,
    /// `WITH DEBUGGING MODE` active.
    Enabled,
}

/// Decimal separator convention declared in `SPECIAL-NAMES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecimalPointKind {
    /// Standard period `.` decimal point.
    Period,
    /// European comma `,` decimal point (`DECIMAL-POINT IS COMMA`).
    Comma,
}

/// Single `CURRENCY SIGN IS lit [WITH PICTURE SYMBOL sym]` specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrencySignSpec {
    /// Displayed currency string symbol (e.g. `"EUR"` or `"$"`).
    pub sign_literal: SymbolId,
    /// Single-character `PICTURE` symbol (defaults to `sign_literal` when 1 char).
    pub picture_symbol: Option<SymbolId>,
    /// Source span of the clause.
    pub span: SourceSpan,
}

/// Strongly-typed atom inside `SPECIAL-NAMES` `ALPHABET` and `CLASS` specifications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecialNamesAtom {
    /// Quoted string or hex literal bytes (`'0'`, `"ABC"`, `X"00"`).
    StringBytes(Vec<u8>),
    /// 1-based ordinal integer token (`1`..`256`, e.g. `66` -> `'A'`).
    OrdinalOneBased(SymbolId),
    /// Figurative constant or syntax keyword (`SPACE`, `ZERO`, `THRU`, `ALSO`, ...).
    Word(SymbolId),
}

/// Kind of `ALPHABET name IS ...` collating sequence definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlphabetKind {
    /// Host native collating sequence (`NATIVE`).
    Native,
    /// ISO 646 / ASCII sequence (`STANDARD-1`).
    Standard1,
    /// ISO 646 international sequence (`STANDARD-2`).
    Standard2,
    /// EBCDIC collating sequence (`EBCDIC`).
    Ebcdic,
    /// Explicit character/ordinal sequence (`literal [THRU literal | ALSO literal ...]`).
    Custom(Vec<SpecialNamesAtom>),
}

/// `ALPHABET name IS ...` declaration in `SPECIAL-NAMES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphabetDecl {
    /// Alphabet name.
    pub name: SymbolId,
    /// Alphabet definition kind.
    pub kind: AlphabetKind,
    /// Source span.
    pub span: SourceSpan,
}

/// `SYMBOLIC CHARACTERS name ... ARE ordinal ... [IN alphabet]` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicCharDecl {
    /// Declared symbolic character constant names.
    pub names: Vec<SymbolId>,
    /// Corresponding 1-based ordinal integer tokens.
    pub ordinals: Vec<SymbolId>,
    /// Optional target `ALPHABET` name.
    pub alphabet: Option<SymbolId>,
    /// Source span.
    pub span: SourceSpan,
}

/// Single character or inclusive range inside `CLASS name IS ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRange {
    /// Start literal or constant atom.
    pub low: SpecialNamesAtom,
    /// Optional inclusive end atom (`THRU` / `THROUGH`).
    pub high: Option<SpecialNamesAtom>,
}

/// User-defined character `CLASS name IS ...` declaration in `SPECIAL-NAMES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDecl {
    /// Class condition name.
    pub name: SymbolId,
    /// Allowed character literals and ranges.
    pub ranges: Vec<ClassRange>,
    /// Optional `IN alphabet` reference.
    pub alphabet: Option<SymbolId>,
    /// Source span.
    pub span: SourceSpan,
}

/// External hardware/software switch binding (`SWITCH-0 IS sw ON STATUS IS on-cond OFF STATUS IS off-cond`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchDecl {
    /// System switch name (e.g. `SWITCH-0`, `UPSI-0`).
    pub system_name: SymbolId,
    /// Optional user mnemonic name.
    pub mnemonic: Option<SymbolId>,
    /// Optional condition-name for `ON` state.
    pub on_condition: Option<SymbolId>,
    /// Optional condition-name for `OFF` state.
    pub off_condition: Option<SymbolId>,
    /// Source span.
    pub span: SourceSpan,
}

/// Mnemonic binding for an I/O device or console (`SYSERR IS ERR-OUT`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceMnemonicDecl {
    /// System device identifier (`CONSOLE`, `SYSIN`, `SYSOUT`, `SYSERR`, `C01`..`C12`).
    pub system_device: SymbolId,
    /// User-defined mnemonic name.
    pub mnemonic: SymbolId,
    /// Source span.
    pub span: SourceSpan,
}

/// Console display mode in `SPECIAL-NAMES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConsoleMode {
    /// Standard stream I/O.
    StandardStream,
    /// `CONSOLE IS CRT` TUI mode.
    ConsoleIsCrt,
}

/// `SPECIAL-NAMES.` paragraph AST node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialNamesParagraph {
    /// Decimal-point character convention (`Period` or `Comma`).
    pub decimal_point: DecimalPointKind,
    /// Declared currency sign specifications.
    pub currency_signs: Vec<CurrencySignSpec>,
    /// Declared `ALPHABET` tables.
    pub alphabets: Vec<AlphabetDecl>,
    /// Declared `SYMBOLIC CHARACTERS`.
    pub symbolic_chars: Vec<SymbolicCharDecl>,
    /// Declared `CLASS` character sets.
    pub classes: Vec<ClassDecl>,
    /// Declared switch mnemonics and status conditions.
    pub switches: Vec<SwitchDecl>,
    /// Declared device mnemonics.
    pub devices: Vec<DeviceMnemonicDecl>,
    /// Optional `CRT STATUS IS data-name` target.
    pub crt_status: Option<SymbolId>,
    /// Optional `CURSOR IS data-name` target.
    pub cursor: Option<SymbolId>,
    /// `CONSOLE IS CRT` setting.
    pub console_mode: ConsoleMode,
}

impl Default for SpecialNamesParagraph {
    /// Returns an empty `SPECIAL-NAMES` paragraph with standard period decimal point.
    fn default() -> Self {
        Self {
            decimal_point: DecimalPointKind::Period,
            currency_signs: Vec::new(),
            alphabets: Vec::new(),
            symbolic_chars: Vec::new(),
            classes: Vec::new(),
            switches: Vec::new(),
            devices: Vec::new(),
            crt_status: None,
            cursor: None,
            console_mode: ConsoleMode::StandardStream,
        }
    }
}

/// Intrinsic function unqualified import policy in `REPOSITORY.`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IntrinsicImportPolicy {
    /// Intrinsics require explicit `FUNCTION` keyword.
    #[default]
    ExplicitOnly,
    /// `FUNCTION ALL INTRINSIC` active.
    AllIntrinsic,
    /// Specific intrinsic names imported without `FUNCTION` keyword.
    SpecificIntrinsics(Vec<SymbolId>),
}

/// `REPOSITORY.` paragraph AST node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepositoryParagraph {
    /// Intrinsic bare-call import policy.
    pub intrinsics: IntrinsicImportPolicy,
    /// Explicit `(name, span)` pairs recorded from `FUNCTION ... INTRINSIC`.
    pub intrinsic_spans: Vec<(SymbolId, SourceSpan)>,
    /// Imported user-defined `FUNCTION` names (`internal_name`, optional `external_alias`).
    pub functions: Vec<(SymbolId, Option<SymbolId>)>,
    /// Source spans parallel to `functions`.
    pub function_spans: Vec<SourceSpan>,
    /// Imported `PROGRAM` prototypes (`internal_name`, optional `external_alias`, `span`).
    pub programs: Vec<(SymbolId, Option<SymbolId>, SourceSpan)>,
    /// Imported `CLASS` names (`internal_name`, optional `external_alias`).
    pub classes: Vec<(SymbolId, Option<SymbolId>)>,
    /// Source spans parallel to `classes`.
    pub class_spans: Vec<SourceSpan>,
    /// Imported `INTERFACE` names (`internal_name`, optional `external_alias`).
    pub interfaces: Vec<(SymbolId, Option<SymbolId>)>,
    /// Source spans parallel to `interfaces`.
    pub interface_spans: Vec<SourceSpan>,
    /// Optional closing period or clause-end span of the `REPOSITORY.` paragraph.
    pub terminator_span: Option<SourceSpan>,
}

/// `CONFIGURATION SECTION.` AST node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationSection {
    /// Optional `SOURCE-COMPUTER` name.
    pub source_computer: Option<SymbolId>,
    /// `WITH DEBUGGING MODE` state.
    pub debugging_mode: DebuggingMode,
    /// Optional `OBJECT-COMPUTER` name.
    pub object_computer: Option<SymbolId>,
    /// Optional `PROGRAM COLLATING SEQUENCE IS alphabet-name`.
    pub program_collating_sequence: Option<SymbolId>,
    /// Parsed `SPECIAL-NAMES.` declarations.
    pub special_names: SpecialNamesParagraph,
    /// Parsed `REPOSITORY.` declarations.
    pub repository: RepositoryParagraph,
}

impl Default for ConfigurationSection {
    /// Returns an empty default `CONFIGURATION SECTION`.
    fn default() -> Self {
        Self {
            source_computer: None,
            debugging_mode: DebuggingMode::Disabled,
            object_computer: None,
            program_collating_sequence: None,
            special_names: SpecialNamesParagraph::default(),
            repository: RepositoryParagraph::default(),
        }
    }
}

/// Whether a `SELECT` file is `OPTIONAL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectOptionality {
    /// File must exist on `OPEN INPUT` (`"35"` otherwise).
    Required,
    /// `SELECT OPTIONAL` (`"05"` when absent).
    Optional,
}

/// Target binding of an `ASSIGN TO ...` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignTarget {
    /// String literal path (`ASSIGN TO "myfile.dat"`).
    Literal(SymbolId),
    /// Unquoted identifier or external name (`ASSIGN TO WS-PATH` or `ASSIGN TO EXTERNAL DDNAME`).
    Name {
        /// Identifier or external DD symbol.
        sym: SymbolId,
        /// Explicit `DYNAMIC` or `EXTERNAL` modifier if specified.
        modifier: AssignModifier,
    },
    /// Standard device keyword (`DISPLAY`, `KEYBOARD`).
    Device(SymbolId),
}

/// Explicit binding modifier inside `ASSIGN TO [DISK | PRINTER | EXTERNAL | DYNAMIC] name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignModifier {
    /// Governed by dialect `assign-clause` default.
    Unspecified,
    /// `ASSIGN TO DISK [name-or-lit]`.
    Disk,
    /// `ASSIGN TO PRINTER [name-or-lit]`.
    Printer,
    /// `ASSIGN TO DYNAMIC data-name`.
    Dynamic,
    /// `ASSIGN TO EXTERNAL env-name`.
    External,
}

/// File organization in `ORGANIZATION IS ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileOrganization {
    /// Record-sequential (`ORGANIZATION IS SEQUENTIAL`, default).
    Sequential,
    /// Newline-delimited text (`ORGANIZATION IS LINE SEQUENTIAL`).
    LineSequential,
    /// Slot-numbered relative records (`ORGANIZATION IS RELATIVE`).
    Relative,
    /// B+Tree keyed ISAM records (`ORGANIZATION IS INDEXED`).
    Indexed,
}

/// File access mode in `ACCESS MODE IS ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessMode {
    /// Sequential traversal (`ACCESS MODE IS SEQUENTIAL`, default).
    Sequential,
    /// Direct key/slot lookup (`ACCESS MODE IS RANDOM`).
    Random,
    /// Mixed sequential and random access (`ACCESS MODE IS DYNAMIC`).
    Dynamic,
}

/// Duplicate key policy on `ALTERNATE RECORD KEY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DuplicatePolicy {
    /// Keys must be unique.
    Unique,
    /// `WITH DUPLICATES` permitted (FIFO order).
    WithDuplicates,
}

/// Primary `RECORD KEY IS ...` specification (supporting split keys `name = seg1 seg2` / `name SOURCE IS seg1, seg2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordKeySpec {
    /// Optional synthetic split-key alias symbol (`Some(name)` when `=` or `SOURCE IS` was used).
    pub split_name: Option<SymbolId>,
    /// Key data-item segments (1 for standard key, >1 for split key).
    pub segments: Vec<Vec<SymbolId>>,
    /// Source span of the `RECORD KEY` clause.
    pub span: SourceSpan,
}

/// Sparse-index suppression clause on `ALTERNATE RECORD KEY` (`SUPPRESS WHEN [ALL] lit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AltKeySuppression {
    /// Suppress index entry when every byte equals the repeated literal/figurative (`SUPPRESS WHEN ALL lit`).
    WhenAll(SymbolId),
    /// Suppress index entry when key equals the exact literal/figurative (`SUPPRESS WHEN lit`).
    WhenLiteral(SymbolId),
}

/// Secondary `ALTERNATE RECORD KEY IS ... [WITH DUPLICATES] [SUPPRESS WHEN ...]` specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlternateKeySpec {
    /// Optional synthetic split-key alias symbol (`Some(name)` when `=` or `SOURCE IS` was used).
    pub split_name: Option<SymbolId>,
    /// Key data-item segments.
    pub segments: Vec<Vec<SymbolId>>,
    /// Duplicate key policy.
    pub duplicates: DuplicatePolicy,
    /// Optional sparse-index suppression rule.
    pub suppression: Option<AltKeySuppression>,
    /// Source span.
    pub span: SourceSpan,
}

/// File `SHARING WITH ...` concurrency specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SharingModeSpec {
    /// Concurrent read/write sharing permitted (`SHARING WITH ALL OTHER`).
    AllOther,
    /// Exclusive access (`SHARING WITH NO OTHER`).
    NoOther,
    /// Concurrent readers permitted, writers blocked (`SHARING WITH READ ONLY`).
    ReadOnly,
}

/// File `LOCK MODE IS ...` specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockModeSpec {
    /// Manual record/file locking (`LOCK MODE IS MANUAL`).
    Manual,
    /// Automatic record locking (`LOCK MODE IS AUTOMATIC`).
    Automatic,
    /// Exclusive file lock (`LOCK MODE IS EXCLUSIVE`).
    Exclusive,
}

/// Single `SELECT [OPTIONAL] file-name ASSIGN TO ...` entry in `FILE-CONTROL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileControlEntry {
    /// Internal COBOL file connector symbol.
    pub file_name: SymbolId,
    /// `SELECT OPTIONAL` flag.
    pub optionality: SelectOptionality,
    /// `ASSIGN TO` target list.
    pub assign_targets: Vec<AssignTarget>,
    /// File organization (defaults to `Sequential`).
    pub organization: FileOrganization,
    /// File access mode (defaults to `Sequential`).
    pub access_mode: AccessMode,
    /// Primary `RECORD KEY` (required for `Indexed`).
    pub record_key: Option<RecordKeySpec>,
    /// Secondary `ALTERNATE RECORD KEY` list.
    pub alternate_keys: Vec<AlternateKeySpec>,
    /// Qualified `RELATIVE KEY` path (for `Relative` random/dynamic access).
    pub relative_key: Option<Vec<SymbolId>>,
    /// Primary `FILE STATUS IS ...` qualified path.
    pub file_status: Option<Vec<SymbolId>>,
    /// Optional secondary VSAM/extended status target.
    pub secondary_status: Option<Vec<SymbolId>>,
    /// Optional `LOCK MODE` setting.
    pub lock_mode: Option<LockModeSpec>,
    /// Optional `SHARING WITH` setting.
    pub sharing_mode: Option<SharingModeSpec>,
    /// Optional `PADDING CHARACTER` symbol.
    pub padding_char: Option<SymbolId>,
    /// Optional `COLLATING SEQUENCE` alphabet symbol.
    pub collating_sequence: Option<SymbolId>,
    /// Count of duplicate `ORGANIZATION` clauses seen (for semantic diagnostics).
    pub organization_clause_count: u8,
    /// Source span of the `SELECT` entry.
    pub span: SourceSpan,
}

/// Kind of `SAME [RECORD | SORT | SORT-MERGE] AREA` sharing clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SameAreaKind {
    /// `SAME AREA FOR ...` buffer sharing.
    BufferArea,
    /// `SAME RECORD AREA FOR ...` record storage sharing.
    RecordArea,
    /// `SAME SORT AREA` or `SAME SORT-MERGE AREA`.
    SortMergeArea,
}

/// `SAME ... AREA FOR file-1 file-2 ...` clause inside `I-O-CONTROL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SameAreaClause {
    /// Area sharing kind.
    pub kind: SameAreaKind,
    /// Participating file names.
    pub files: Vec<SymbolId>,
    /// Source span.
    pub span: SourceSpan,
}

/// `I-O-CONTROL.` paragraph AST node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IoControlParagraph {
    /// `SAME ... AREA` clauses.
    pub same_areas: Vec<SameAreaClause>,
}

/// `INPUT-OUTPUT SECTION.` AST node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputOutputSection {
    /// `FILE-CONTROL` entries.
    pub file_control: Vec<FileControlEntry>,
    /// `I-O-CONTROL` declarations.
    pub io_control: IoControlParagraph,
}

/// Complete `ENVIRONMENT DIVISION` AST node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentDivision {
    /// `CONFIGURATION SECTION` declarations.
    pub configuration: ConfigurationSection,
    /// `INPUT-OUTPUT SECTION` declarations.
    pub input_output: InputOutputSection,
    /// Source span if explicitly present.
    pub span: Option<SourceSpan>,
}
