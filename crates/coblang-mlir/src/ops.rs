//! Strongly-typed `cobol.*` MLIR 20 operation builders and attribute descriptors.
//!
//! Zero raw `"cobol.*"` operation name strings appear outside this module.

use std::fmt::Write as _;

use coblang_common::{CoblangError, Result};
use melior::ir::attribute::{DenseI32ArrayAttribute, IntegerAttribute, StringAttribute};
use melior::ir::operation::OperationBuilder;
use melior::ir::r#type::IntegerType;
use melior::ir::{Attribute, Identifier, Location, Operation, Region, Type, Value};

use crate::context::CobolMlirContext;

/// Exhaustive enumeration of all custom `cobol` MLIR dialect operation kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CobolOpKind {
    /// `cobol.program` — compilation unit procedure body.
    Program,
    /// `cobol.storage` — module-level static/working/external byte buffer.
    Storage,
    /// `cobol.field_view` — typed byte-window pointer into a `cobol.storage` buffer.
    FieldView,
    /// `cobol.subscript` — multi-dimensional table element offset calculation.
    Subscript,
    /// `cobol.refmod` — 1-based reference-modification slice calculation.
    RefMod,
    /// `cobol.move` — elementary or group data movement.
    Move,
    /// `cobol.init` — `INITIALIZE` byte-fill operation.
    Init,
    /// `cobol.arith` — exact decimal or binary arithmetic operation.
    Arith,
    /// `cobol.div_rem` — combined quotient + remainder division.
    DivRem,
    /// `cobol.cmp` — relational comparison returning `i1`.
    Cmp,
    /// `cobol.class_check` — `IS NUMERIC` / `IS ALPHABETIC` class condition returning `i1`.
    ClassCheck,
    /// `cobol.sign_check` — `IS POSITIVE` / `NEGATIVE` / `ZERO` sign condition returning `i1`.
    SignCheck,
    /// `cobol.perform_range` — out-of-line `PERFORM p1 THRU p2` invocation.
    PerformRange,
    /// `cobol.alter` — `ALTER p1 TO PROCEED TO p2` target mutation.
    Alter,
    /// `cobol.call` — static or dynamic subprogram `CALL`.
    Call,
    /// `cobol.cancel` — `CANCEL` subprogram state reset.
    Cancel,
    /// `cobol.display` — multi-operand `DISPLAY` output emission.
    Display,
    /// `cobol.accept` — `ACCEPT` input/temporal/environment capture.
    Accept,
    /// `cobol.string` — `STRING` concatenation verb.
    StringVerb,
    /// `cobol.unstring` — `UNSTRING` splitting verb.
    UnstringVerb,
    /// `cobol.inspect` — `INSPECT` tallying/replacing/converting verb.
    Inspect,
    /// `cobol.file_op` — `OPEN`/`CLOSE`/`READ`/`WRITE`/`REWRITE`/`DELETE`/`START` file operation.
    FileOp,
    /// `cobol.sort_merge` — `SORT` / `MERGE` session operation.
    SortMerge,
    /// `cobol.report` — `INITIATE` / `GENERATE` / `TERMINATE` Report Writer operation.
    Report,
    /// `cobol.screen` — `SCREEN SECTION` TUI display/accept operation.
    Screen,
    /// `cobol.xml_json` — `XML` / `JSON` parse or generate operation.
    XmlJson,
    /// `cobol.set_return_code` — writes `i32` into owning `RETURN-CODE` slot.
    SetReturnCode,
    /// `cobol.get_return_code` — reads `i32` from owning `RETURN-CODE` slot.
    GetReturnCode,
    /// `cobol.stop_run` — run-unit termination with `i32` exit code.
    StopRun,
    /// `cobol.goback` — `GOBACK` or `EXIT PROGRAM` control-flow return.
    Goback,
    /// `cobol.trim` — `FUNCTION TRIM` zero-copy `(ptr, len)` view computation.
    Trim,
    /// `cobol.rt_dispatch` — typed runtime descriptor/decimal/initialize/display C-ABI call.
    RtDispatch,
    /// `cobol.loop` — structured `PERFORM` loop (`cond`, `body`, and `step` regions).
    Loop,
    /// `cobol.sentence` — sentence scope anchoring `NEXT SENTENCE` transfers.
    Sentence,
    /// `cobol.flow_exit` — `EXIT PARAGRAPH/SECTION/PERFORM [CYCLE]` or `NEXT SENTENCE` transfer.
    FlowExit,
    /// `cobol.goto` — static, `DEPENDING ON`, or `ALTER`able paragraph jump.
    Goto,
    /// `cobol.yield` — region value/control yield inside `cobol.loop` and `cobol.sentence`.
    Yield,
}

impl CobolOpKind {
    /// Returns the canonical `"cobol.<op>"` MLIR operation name for `self`.
    #[must_use]
    pub const fn op_name(self) -> &'static str {
        match self {
            Self::Program => "cobol.program",
            Self::Storage => "cobol.storage",
            Self::FieldView => "cobol.field_view",
            Self::Subscript => "cobol.subscript",
            Self::RefMod => "cobol.refmod",
            Self::Move => "cobol.move",
            Self::Init => "cobol.init",
            Self::Arith => "cobol.arith",
            Self::DivRem => "cobol.div_rem",
            Self::Cmp => "cobol.cmp",
            Self::ClassCheck => "cobol.class_check",
            Self::SignCheck => "cobol.sign_check",
            Self::PerformRange => "cobol.perform_range",
            Self::Alter => "cobol.alter",
            Self::Call => "cobol.call",
            Self::Cancel => "cobol.cancel",
            Self::Display => "cobol.display",
            Self::Accept => "cobol.accept",
            Self::StringVerb => "cobol.string",
            Self::UnstringVerb => "cobol.unstring",
            Self::Inspect => "cobol.inspect",
            Self::FileOp => "cobol.file_op",
            Self::SortMerge => "cobol.sort_merge",
            Self::Report => "cobol.report",
            Self::Screen => "cobol.screen",
            Self::XmlJson => "cobol.xml_json",
            Self::SetReturnCode => "cobol.set_return_code",
            Self::GetReturnCode => "cobol.get_return_code",
            Self::StopRun => "cobol.stop_run",
            Self::Goback => "cobol.goback",
            Self::Trim => "cobol.trim",
            Self::RtDispatch => "cobol.rt_dispatch",
            Self::Loop => "cobol.loop",
            Self::Sentence => "cobol.sentence",
            Self::FlowExit => "cobol.flow_exit",
            Self::Goto => "cobol.goto",
            Self::Yield => "cobol.yield",
        }
    }

    /// Parses a `"cobol.<op>"` name into its strongly-typed [`CobolOpKind`].
    ///
    /// # Arguments
    ///
    /// * `name` - Full MLIR operation name string.
    #[must_use]
    pub fn from_op_name(name: &str) -> Option<Self> {
        match name {
            "cobol.program" => Some(Self::Program),
            "cobol.storage" => Some(Self::Storage),
            "cobol.field_view" => Some(Self::FieldView),
            "cobol.subscript" => Some(Self::Subscript),
            "cobol.refmod" => Some(Self::RefMod),
            "cobol.move" => Some(Self::Move),
            "cobol.init" => Some(Self::Init),
            "cobol.arith" => Some(Self::Arith),
            "cobol.div_rem" => Some(Self::DivRem),
            "cobol.cmp" => Some(Self::Cmp),
            "cobol.class_check" => Some(Self::ClassCheck),
            "cobol.sign_check" => Some(Self::SignCheck),
            "cobol.perform_range" => Some(Self::PerformRange),
            "cobol.alter" => Some(Self::Alter),
            "cobol.call" => Some(Self::Call),
            "cobol.cancel" => Some(Self::Cancel),
            "cobol.display" => Some(Self::Display),
            "cobol.accept" => Some(Self::Accept),
            "cobol.string" => Some(Self::StringVerb),
            "cobol.unstring" => Some(Self::UnstringVerb),
            "cobol.inspect" => Some(Self::Inspect),
            "cobol.file_op" => Some(Self::FileOp),
            "cobol.sort_merge" => Some(Self::SortMerge),
            "cobol.report" => Some(Self::Report),
            "cobol.screen" => Some(Self::Screen),
            "cobol.xml_json" => Some(Self::XmlJson),
            "cobol.set_return_code" => Some(Self::SetReturnCode),
            "cobol.get_return_code" => Some(Self::GetReturnCode),
            "cobol.stop_run" => Some(Self::StopRun),
            "cobol.goback" => Some(Self::Goback),
            "cobol.trim" => Some(Self::Trim),
            "cobol.rt_dispatch" => Some(Self::RtDispatch),
            "cobol.loop" => Some(Self::Loop),
            "cobol.sentence" => Some(Self::Sentence),
            "cobol.flow_exit" => Some(Self::FlowExit),
            "cobol.goto" => Some(Self::Goto),
            "cobol.yield" => Some(Self::Yield),
            _ => None,
        }
    }
}

/// Returns `true` when `name` uses the `"cobol."` dialect namespace prefix.
///
/// # Arguments
///
/// * `name` - Full MLIR operation name string.
#[must_use]
pub fn is_cobol_dialect_name(name: &str) -> bool {
    name.starts_with("cobol.")
}

/// Program `IS INITIAL` state reset attribute on [`CobolProgramOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InitialStateMode {
    /// Static `WORKING-STORAGE` persists across calls (default).
    Retained,
    /// `IS INITIAL` resets `WORKING-STORAGE` on each entry.
    Initial,
}

impl InitialStateMode {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retained => "retained",
            Self::Initial => "initial",
        }
    }
}

/// Program `RECURSIVE` capability attribute on [`CobolProgramOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecursivityMode {
    /// Non-recursive program (default).
    NonRecursive,
    /// `RECURSIVE` program.
    Recursive,
}

impl RecursivityMode {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NonRecursive => "non_recursive",
            Self::Recursive => "recursive",
        }
    }
}

/// Calling convention attribute on [`CobolProgramOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryConventionMode {
    /// Standard COBOL module convention.
    Cobol,
    /// Standard C calling convention.
    C,
    /// Static linkage convention.
    Static,
    /// Platform `STDCALL` convention.
    Stdcall,
}

impl EntryConventionMode {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cobol => "cobol",
            Self::C => "c",
            Self::Static => "static",
            Self::Stdcall => "stdcall",
        }
    }
}

/// Backing storage section kind for [`CobolStorageOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageKind {
    /// Program-private static `WORKING-STORAGE`.
    Working,
    /// Per-invocation `LOCAL-STORAGE`.
    Local,
    /// Run-unit shared `EXTERNAL` segment.
    External,
    /// Read-only literal pool entry.
    StaticConst,
}

impl StorageKind {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Local => "local",
            Self::External => "external",
            Self::StaticConst => "static_const",
        }
    }
}

/// Field representation descriptor carried on [`CobolFieldViewOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldAttrDesc {
    /// Runtime display/codec tag (`0` raw bytes, `1` i32, `2` u32, `3` BE signed, `4` BE unsigned).
    pub kind_tag: u8,
    /// Declared decimal digit count.
    pub digits: u8,
    /// Signed decimal scale factor.
    pub scale: i16,
}

/// Elementary data movement flavor on [`CobolMoveOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoveEncodingKind {
    /// Store `i32` SSA source into big-endian integer destination (`1`, `2`, `4`, or `8` bytes).
    Int32ToBigEndian,
    /// Store `i32` SSA source into native-endian integer destination (`1`, `2`, `4`, or `8` bytes).
    Int32ToNativeEndian,
    /// Copy/space-pad raw alphanumeric bytes from `!llvm.ptr` source to `!llvm.ptr` destination.
    RawBytesCopyPad,
}

impl MoveEncodingKind {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Int32ToBigEndian => "i32_to_be",
            Self::Int32ToNativeEndian => "i32_to_ne",
            Self::RawBytesCopyPad => "bytes_copy_pad",
        }
    }
}

/// Bounds-checking mode on [`CobolSubscriptOp`] and [`CobolRefModOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoundsCheckMode {
    /// Omit runtime bounds check (`EC-BOUND-*` disabled).
    Unchecked,
    /// Emit `EC-BOUND-*` check.
    Checked,
}

impl BoundsCheckMode {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unchecked => "unchecked",
            Self::Checked => "checked",
        }
    }
}

/// Character byte width on [`CobolRefModOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CharByteWidth {
    /// Single-byte ASCII / EBCDIC (`1` byte per character).
    SingleByte,
    /// `NATIONAL` UTF-16 (`2` bytes per code unit).
    Utf16TwoByte,
    /// `UTF-8` variable character width.
    Utf8Variable,
}

impl CharByteWidth {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleByte => "1",
            Self::Utf16TwoByte => "2",
            Self::Utf8Variable => "utf8",
        }
    }
}

/// `INITIALIZE` category policy on [`CobolInitOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InitPolicy {
    /// Fill with category default byte (`0x00` binary, `'0'` zoned, `' '` alphanumeric).
    DefaultCategory,
    /// Restore Data Division `VALUE` clause.
    DefinedValue,
}

impl InitPolicy {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DefaultCategory => "default",
            Self::DefinedValue => "defined_value",
        }
    }
}

/// Arithmetic operator selector on [`CobolArithOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArithKind {
    /// Addition (`ADD` / `+`).
    Add,
    /// Subtraction (`SUBTRACT` / `-`).
    Sub,
    /// Multiplication (`MULTIPLY` / `*`).
    Mul,
    /// Division (`DIVIDE` / `/`).
    Div,
    /// Exponentiation (`**`).
    Pow,
}

impl ArithKind {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Div => "div",
            Self::Pow => "pow",
        }
    }
}

/// Decimal rounding policy on [`CobolArithOp`] and [`CobolDivRemOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoundingMode {
    /// Unrounded truncation toward zero.
    Truncate,
    /// Standard `ROUNDED` nearest-away-from-zero.
    NearestAwayFromZero,
}

impl RoundingMode {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Truncate => "truncate",
            Self::NearestAwayFromZero => "nearest_away_from_zero",
        }
    }
}

/// `ON SIZE ERROR` handling policy on [`CobolArithOp`] and [`CobolDivRemOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeErrorBranch {
    /// No `ON SIZE ERROR` handler attached.
    Ignore,
    /// `ON SIZE ERROR` handler or `EC-SIZE-*` active.
    CheckOverflow,
}

impl SizeErrorBranch {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::CheckOverflow => "check",
        }
    }
}

/// Relational comparison operator on [`CobolCmpOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelOp {
    /// `=`
    Eq,
    /// `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl RelOp {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
        }
    }
}

/// Collating-sequence identifier newtype (`0` = native ASCII).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CollatingSeqId(pub u32);

/// Character class condition kind on [`CobolClassCheckOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClassConditionKind {
    /// `IS NUMERIC`
    Numeric,
    /// `IS ALPHABETIC`
    Alphabetic,
    /// `IS ALPHABETIC-LOWER`
    AlphabeticLower,
    /// `IS ALPHABETIC-UPPER`
    AlphabeticUpper,
}

impl ClassConditionKind {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Numeric => "numeric",
            Self::Alphabetic => "alphabetic",
            Self::AlphabeticLower => "alphabetic_lower",
            Self::AlphabeticUpper => "alphabetic_upper",
        }
    }
}

/// Algebraic sign condition kind on [`CobolSignCheckOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignConditionKind {
    /// `IS POSITIVE`
    Positive,
    /// `IS NEGATIVE`
    Negative,
    /// `IS ZERO`
    Zero,
}

impl SignConditionKind {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
            Self::Zero => "zero",
        }
    }
}

/// Subprogram resolution mode on [`CobolCallOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallDispatchKind {
    /// Direct intra-module function call (`CALL "nested-or-sibling"`).
    StaticSymbol,
    /// Runtime `coblang_rt_call_dynamic` shared-module resolution (`CALL "mod1"`).
    DynamicLiteral,
    /// Runtime variable-named dynamic `CALL identifier`.
    DynamicValue,
}

impl CallDispatchKind {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaticSymbol => "static",
            Self::DynamicLiteral => "dynamic_lit",
            Self::DynamicValue => "dynamic_val",
        }
    }
}

/// Exception branch policy on [`CobolCallOp`] and [`CobolAcceptOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExceptionBranches {
    /// No `ON EXCEPTION` clause attached.
    None,
    /// `ON EXCEPTION` / `NOT ON EXCEPTION` active.
    Handled,
}

impl ExceptionBranches {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Handled => "handled",
        }
    }
}

/// `ACCEPT` input source kind on [`CobolAcceptOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcceptSourceKind {
    /// Standard input (`SYSIN` / `CONSOLE`).
    Console,
    /// `ACCEPT FROM DATE [YYYYMMDD]`.
    Date,
    /// `ACCEPT FROM DAY [YYYYDDD]`.
    Day,
    /// `ACCEPT FROM DAY-OF-WEEK`.
    DayOfWeek,
    /// `ACCEPT FROM TIME`.
    Time,
    /// `ACCEPT FROM COMMAND-LINE` / `ARGUMENT-VALUE`.
    CommandLine,
    /// `ACCEPT FROM ENVIRONMENT-VALUE`.
    Environment,
}

impl AcceptSourceKind {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Date => "date",
            Self::Day => "day",
            Self::DayOfWeek => "day_of_week",
            Self::Time => "time",
            Self::CommandLine => "command_line",
            Self::Environment => "environment",
        }
    }
}

/// File I/O verb selector on [`CobolFileOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileVerbKind {
    /// `OPEN`
    Open,
    /// `CLOSE`
    Close,
    /// `READ`
    Read,
    /// `WRITE`
    Write,
    /// `REWRITE`
    Rewrite,
    /// `DELETE`
    Delete,
    /// `START`
    Start,
}

impl FileVerbKind {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Close => "close",
            Self::Read => "read",
            Self::Write => "write",
            Self::Rewrite => "rewrite",
            Self::Delete => "delete",
            Self::Start => "start",
        }
    }
}

/// Return semantic flavor on [`CobolGobackOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GobackMode {
    /// `GOBACK` / implicit end-of-program return.
    Goback,
    /// `EXIT PROGRAM` (returns when inside a `CALL`ed subprogram; no-op in top-level main).
    ExitProgram,
    /// Synthetic end-of-paragraph normal fallthrough (`action == 0`).
    ParaFallthrough,
}

impl GobackMode {
    /// Returns the attribute tag string for `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Goback => "goback",
            Self::ExitProgram => "exit_program",
            Self::ParaFallthrough => "para_fallthrough",
        }
    }
}

/// Encodes raw bytes as uppercase hexadecimal ASCII characters.
///
/// # Arguments
///
/// * `bytes` - Raw byte slice to encode.
#[must_use]
pub fn encode_hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &b in bytes {
        let _ = write!(out, "{b:02X}");
    }
    out
}

/// Decodes an even-length hexadecimal string produced by [`encode_hex_bytes`].
///
/// # Arguments
///
/// * `hex` - Even-length ASCII hex string.
///
/// # Errors
///
/// Returns [`CoblangError::MlirVerification`] if `hex` has odd length or non-hex digits.
pub fn decode_hex_bytes(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(CoblangError::MlirVerification {
            stage: "cobol.storage",
            ir_snippet: String::from(hex),
            message: String::from("odd-length `init_hex` attribute on `cobol.storage`"),
        });
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().as_chunks::<2>().0 {
        let s = core::str::from_utf8(pair).unwrap_or("ZZ");
        let byte = u8::from_str_radix(s, 16).map_err(|_| CoblangError::MlirVerification {
            stage: "cobol.storage",
            ir_snippet: String::from(hex),
            message: format!("invalid hex byte `{s}` in `init_hex`"),
        })?;
        out.push(byte);
    }
    Ok(out)
}

/// Finalizes an [`OperationBuilder`] into an [`Operation`], mapping failures to [`CoblangError::MlirVerification`].
///
/// # Arguments
///
/// * `builder` - Configured `melior` [`OperationBuilder`].
/// * `op_label` - Static operation name for diagnostic attribution.
///
/// # Errors
///
/// Returns [`CoblangError::MlirVerification`] if MLIR rejects operation construction.
pub fn finish_op_build<'c>(
    builder: OperationBuilder<'c>,
    op_label: &'static str,
) -> Result<Operation<'c>> {
    match builder.build() {
        Ok(op) => Ok(op),
        Err(err) => Err(CoblangError::MlirVerification {
            stage: op_label,
            ir_snippet: String::from(op_label),
            message: err.to_string(),
        }),
    }
}

/// Helper creating an `(Identifier, StringAttribute)` pair.
///
/// # Arguments
///
/// * `ctx` - Owning [`CobolMlirContext`].
/// * `key` - Attribute identifier name.
/// * `val` - String attribute payload.
fn str_attr<'c>(
    ctx: &'c CobolMlirContext,
    key: &str,
    val: &str,
) -> (Identifier<'c>, Attribute<'c>) {
    (
        Identifier::new(ctx.raw(), key),
        StringAttribute::new(ctx.raw(), val).into(),
    )
}

/// Helper creating an `(Identifier, IntegerAttribute)` `i64` pair.
///
/// # Arguments
///
/// * `ctx` - Owning [`CobolMlirContext`].
/// * `key` - Attribute identifier name.
/// * `val` - Signed 64-bit integer attribute payload.
fn i64_attr<'c>(ctx: &'c CobolMlirContext, key: &str, val: i64) -> (Identifier<'c>, Attribute<'c>) {
    let i64_ty = IntegerType::new(ctx.raw(), 64).into();
    (
        Identifier::new(ctx.raw(), key),
        IntegerAttribute::new(i64_ty, val).into(),
    )
}

/// Returns the `!llvm.ptr` opaque pointer [`Type`] in `ctx`.
///
/// # Arguments
///
/// * `ctx` - Owning [`CobolMlirContext`].
#[must_use]
pub fn llvm_ptr_type(ctx: &CobolMlirContext) -> Type<'_> {
    melior::dialect::llvm::r#type::pointer(ctx.raw(), 0)
}

/// Builder for `cobol.program`.
pub struct CobolProgramOp<'c> {
    /// Canonical mangled program symbol name.
    pub sym_name: String,
    /// Optional raw C alias (`""` when omitted).
    pub raw_c_alias: String,
    /// Owning `RETURN-CODE` storage symbol name.
    pub rc_slot_sym: String,
    /// `IS INITIAL` state policy.
    pub initial_state: InitialStateMode,
    /// `RECURSIVE` policy.
    pub recursivity: RecursivityMode,
    /// Calling convention.
    pub entry_convention: EntryConventionMode,
    /// Single-region procedure body.
    pub body: Region<'c>,
}

impl<'c> CobolProgramOp<'c> {
    /// Builds the `cobol.program` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location of the `PROGRAM-ID`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Program;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_attributes(&[
                str_attr(ctx, "sym_name", &self.sym_name),
                str_attr(ctx, "raw_c_alias", &self.raw_c_alias),
                str_attr(ctx, "rc_slot_sym", &self.rc_slot_sym),
                str_attr(ctx, "initial_state", self.initial_state.as_str()),
                str_attr(ctx, "recursivity", self.recursivity.as_str()),
                str_attr(ctx, "entry_convention", self.entry_convention.as_str()),
            ])
            .add_regions([self.body]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.storage`.
pub struct CobolStorageOp<'a> {
    /// Global storage symbol name.
    pub sym_name: &'a str,
    /// Storage lifetime/visibility category.
    pub storage_kind: StorageKind,
    /// Total static byte size (`>= 1`).
    pub byte_size: u32,
    /// Byte alignment (`1`, `2`, `4`, `8`, or `16`).
    pub alignment: u32,
    /// Initial byte image (`len == byte_size`).
    pub init_bytes: &'a [u8],
}

impl CobolStorageOp<'_> {
    /// Builds the `cobol.storage` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location of the data item or literal.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build<'c>(
        self,
        ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Storage;
        let hex = encode_hex_bytes(self.init_bytes);
        let builder = OperationBuilder::new(kind.op_name(), location).add_attributes(&[
            str_attr(ctx, "sym_name", self.sym_name),
            str_attr(ctx, "storage_kind", self.storage_kind.as_str()),
            i64_attr(ctx, "byte_size", i64::from(self.byte_size)),
            i64_attr(ctx, "alignment", i64::from(self.alignment)),
            str_attr(ctx, "init_hex", &hex),
        ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.field_view` (`-> !llvm.ptr`).
pub struct CobolFieldViewOp<'a> {
    /// Referenced `cobol.storage` symbol name.
    pub storage_sym: &'a str,
    /// Static byte offset from the start of `storage_sym`.
    pub byte_offset: u32,
    /// Byte length of the field window.
    pub byte_len: u32,
    /// Representation metadata.
    pub attr: FieldAttrDesc,
}

impl CobolFieldViewOp<'_> {
    /// Builds the `cobol.field_view` [`Operation`] producing a single `!llvm.ptr` result.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location of the place expression.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build<'c>(
        self,
        ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        let kind = CobolOpKind::FieldView;
        let ptr_ty = llvm_ptr_type(ctx);
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_attributes(&[
                str_attr(ctx, "storage_sym", self.storage_sym),
                i64_attr(ctx, "byte_offset", i64::from(self.byte_offset)),
                i64_attr(ctx, "byte_len", i64::from(self.byte_len)),
                i64_attr(ctx, "field_kind", i64::from(self.attr.kind_tag)),
                i64_attr(ctx, "digits", i64::from(self.attr.digits)),
                i64_attr(ctx, "scale", i64::from(self.attr.scale)),
            ])
            .add_results(&[ptr_ty]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.subscript` (`-> !llvm.ptr`).
pub struct CobolSubscriptOp<'c, 'a> {
    /// Base field view pointer (`!llvm.ptr`).
    pub base_ptr: Value<'c, 'a>,
    /// 1-based subscript index values (`i64`).
    pub indices: &'a [Value<'c, 'a>],
    /// Static element byte strides per dimension.
    pub strides: &'a [i32],
    /// Optional dynamic `OCCURS DEPENDING ON` upper bound (`i64`).
    pub odo_limit: Option<Value<'c, 'a>>,
    /// Runtime `EC-BOUND-SUBSCRIPT` check policy.
    pub bounds_mode: BoundsCheckMode,
}

impl<'c> CobolSubscriptOp<'c, '_> {
    /// Builds the `cobol.subscript` [`Operation`] producing `!llvm.ptr`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Subscript;
        let mut ops = Vec::with_capacity(2 + self.indices.len());
        ops.push(self.base_ptr);
        ops.extend_from_slice(self.indices);
        if let Some(lim) = self.odo_limit {
            ops.push(lim);
        }
        let strides_attr = DenseI32ArrayAttribute::new(ctx.raw(), self.strides);
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&ops)
            .add_attributes(&[
                (Identifier::new(ctx.raw(), "strides"), strides_attr.into()),
                str_attr(ctx, "bounds_mode", self.bounds_mode.as_str()),
            ])
            .add_results(&[llvm_ptr_type(ctx)]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.refmod` (`-> (!llvm.ptr, i64)`).
pub struct CobolRefModOp<'c, 'a> {
    /// Base field view pointer (`!llvm.ptr`).
    pub view_ptr: Value<'c, 'a>,
    /// Base byte length (`i64`).
    pub base_len: Value<'c, 'a>,
    /// 1-based start character offset (`i64`).
    pub one_based_offset: Value<'c, 'a>,
    /// Optional explicit character length (`i64`).
    pub opt_char_len: Option<Value<'c, 'a>>,
    /// Character width unit.
    pub char_width: CharByteWidth,
    /// Runtime `EC-BOUND-REF-MOD` check policy.
    pub bounds_mode: BoundsCheckMode,
}

impl<'c> CobolRefModOp<'c, '_> {
    /// Builds the `cobol.refmod` [`Operation`] producing `(!llvm.ptr, i64)`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::RefMod;
        let mut ops = vec![self.view_ptr, self.base_len, self.one_based_offset];
        if let Some(len_val) = self.opt_char_len {
            ops.push(len_val);
        }
        let i64_ty = IntegerType::new(ctx.raw(), 64).into();
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&ops)
            .add_attributes(&[
                str_attr(ctx, "char_width", self.char_width.as_str()),
                str_attr(ctx, "bounds_mode", self.bounds_mode.as_str()),
            ])
            .add_results(&[llvm_ptr_type(ctx), i64_ty]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.move`.
pub struct CobolMoveOp<'c, 'a> {
    /// Source SSA value (`i32` for integer moves, `!llvm.ptr` for byte moves).
    pub src: Value<'c, 'a>,
    /// Destination field pointer (`!llvm.ptr`).
    pub dst_view: Value<'c, 'a>,
    /// Conversion/encoding flavor.
    pub encoding: MoveEncodingKind,
    /// Source byte length (`0` when `src` is an SSA integer).
    pub src_byte_len: u32,
    /// Destination byte length (`>= 1`).
    pub dst_byte_len: u32,
}

impl<'c> CobolMoveOp<'c, '_> {
    /// Builds the `cobol.move` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Move;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&[self.src, self.dst_view])
            .add_attributes(&[
                str_attr(ctx, "encoding", self.encoding.as_str()),
                i64_attr(ctx, "src_byte_len", i64::from(self.src_byte_len)),
                i64_attr(ctx, "dst_byte_len", i64::from(self.dst_byte_len)),
            ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.init`.
pub struct CobolInitOp<'c, 'a> {
    /// Destination field pointer (`!llvm.ptr`).
    pub dst_view: Value<'c, 'a>,
    /// `INITIALIZE` mode.
    pub policy: InitPolicy,
    /// Byte length to fill.
    pub byte_len: u32,
    /// Fill byte (`0x00`, `0x30`, or `0x20`).
    pub fill_byte: u8,
}

impl<'c> CobolInitOp<'c, '_> {
    /// Builds the `cobol.init` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Init;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&[self.dst_view])
            .add_attributes(&[
                str_attr(ctx, "policy", self.policy.as_str()),
                i64_attr(ctx, "byte_len", i64::from(self.byte_len)),
                i64_attr(ctx, "fill_byte", i64::from(self.fill_byte)),
            ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.arith` (`-> i32`).
pub struct CobolArithOp<'c, 'a> {
    /// Arithmetic operation kind.
    pub kind: ArithKind,
    /// Left operand (`i32` or field view).
    pub lhs: Value<'c, 'a>,
    /// Right operand (`i32` or field view).
    pub rhs: Value<'c, 'a>,
    /// Rounding policy.
    pub rounding: RoundingMode,
    /// `ON SIZE ERROR` policy.
    pub size_error: SizeErrorBranch,
}

impl<'c> CobolArithOp<'c, '_> {
    /// Builds the `cobol.arith` [`Operation`] producing `i32`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let op_kind = CobolOpKind::Arith;
        let i32_ty = IntegerType::new(ctx.raw(), 32).into();
        let builder = OperationBuilder::new(op_kind.op_name(), location)
            .add_operands(&[self.lhs, self.rhs])
            .add_attributes(&[
                str_attr(ctx, "arith_kind", self.kind.as_str()),
                str_attr(ctx, "rounding", self.rounding.as_str()),
                str_attr(ctx, "size_error", self.size_error.as_str()),
            ])
            .add_results(&[i32_ty]);
        finish_op_build(builder, op_kind.op_name())
    }
}

/// Builder for `cobol.div_rem`.
pub struct CobolDivRemOp<'c, 'a> {
    /// Dividend field/value.
    pub dividend: Value<'c, 'a>,
    /// Divisor field/value.
    pub divisor: Value<'c, 'a>,
    /// Quotient destination view (`!llvm.ptr`).
    pub quotient_dst: Value<'c, 'a>,
    /// Remainder destination view (`!llvm.ptr`).
    pub remainder_dst: Value<'c, 'a>,
    /// Rounding policy.
    pub rounding: RoundingMode,
    /// `ON SIZE ERROR` policy.
    pub size_error: SizeErrorBranch,
}

impl<'c> CobolDivRemOp<'c, '_> {
    /// Builds the `cobol.div_rem` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::DivRem;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&[
                self.dividend,
                self.divisor,
                self.quotient_dst,
                self.remainder_dst,
            ])
            .add_attributes(&[
                str_attr(ctx, "rounding", self.rounding.as_str()),
                str_attr(ctx, "size_error", self.size_error.as_str()),
            ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.cmp` (`-> i1`).
pub struct CobolCmpOp<'c, 'a> {
    /// Relational predicate.
    pub rel_op: RelOp,
    /// Left operand (`i32`).
    pub lhs: Value<'c, 'a>,
    /// Right operand (`i32`).
    pub rhs: Value<'c, 'a>,
    /// Collating sequence identifier.
    pub collating_seq: CollatingSeqId,
}

impl<'c> CobolCmpOp<'c, '_> {
    /// Builds the `cobol.cmp` [`Operation`] producing `i1`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Cmp;
        let i1_ty = IntegerType::new(ctx.raw(), 1).into();
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&[self.lhs, self.rhs])
            .add_attributes(&[
                str_attr(ctx, "rel_op", self.rel_op.as_str()),
                i64_attr(ctx, "collating_seq", i64::from(self.collating_seq.0)),
            ])
            .add_results(&[i1_ty]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.class_check` (`-> i1`).
pub struct CobolClassCheckOp<'c, 'a> {
    /// Class condition kind.
    pub class_kind: ClassConditionKind,
    /// Subject field pointer (`!llvm.ptr`).
    pub view: Value<'c, 'a>,
    /// Subject byte length.
    pub byte_len: u32,
}

impl<'c> CobolClassCheckOp<'c, '_> {
    /// Builds the `cobol.class_check` [`Operation`] producing `i1`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::ClassCheck;
        let i1_ty = IntegerType::new(ctx.raw(), 1).into();
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&[self.view])
            .add_attributes(&[
                str_attr(ctx, "class_kind", self.class_kind.as_str()),
                i64_attr(ctx, "byte_len", i64::from(self.byte_len)),
            ])
            .add_results(&[i1_ty]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.sign_check` (`-> i1`).
pub struct CobolSignCheckOp<'c, 'a> {
    /// Sign condition kind.
    pub sign_kind: SignConditionKind,
    /// Subject value or field view.
    pub view: Value<'c, 'a>,
}

impl<'c> CobolSignCheckOp<'c, '_> {
    /// Builds the `cobol.sign_check` [`Operation`] producing `i1`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::SignCheck;
        let i1_ty = IntegerType::new(ctx.raw(), 1).into();
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&[self.view])
            .add_attributes(&[str_attr(ctx, "sign_kind", self.sign_kind.as_str())])
            .add_results(&[i1_ty]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.perform_range`.
pub struct CobolPerformRangeOp {
    /// Entry `ProcedureId` raw index.
    pub start_proc: u32,
    /// Inclusive exit `ProcedureId` raw index.
    pub end_proc: u32,
}

impl CobolPerformRangeOp {
    /// Builds the `cobol.perform_range` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build<'c>(
        self,
        ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        self.build_with_target_prog(None, ctx, location)
    }

    /// Builds `cobol.perform_range` optionally targeting an enclosing unit's dispatcher (`USE GLOBAL`).
    ///
    /// # Arguments
    ///
    /// * `target_prog` - Optional canonical symbol of an enclosing unit (`None` for the current unit).
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build_with_target_prog<'c>(
        self,
        target_prog: Option<&str>,
        ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        let kind = CobolOpKind::PerformRange;
        let mut attrs = vec![
            i64_attr(ctx, "start_proc", i64::from(self.start_proc)),
            i64_attr(ctx, "end_proc", i64::from(self.end_proc)),
        ];
        if let Some(tp) = target_prog {
            attrs.push(str_attr(ctx, "target_prog", tp));
        }
        let builder = OperationBuilder::new(kind.op_name(), location).add_attributes(&attrs);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.alter`.
pub struct CobolAlterOp {
    /// Altered paragraph `ProcedureId` raw index.
    pub altered_para: u32,
    /// New target `ProcedureId` raw index.
    pub new_target: u32,
}

impl CobolAlterOp {
    /// Builds the `cobol.alter` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build<'c>(
        self,
        ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Alter;
        let builder = OperationBuilder::new(kind.op_name(), location).add_attributes(&[
            i64_attr(ctx, "altered_para", i64::from(self.altered_para)),
            i64_attr(ctx, "new_target", i64::from(self.new_target)),
        ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.call`.
pub struct CobolCallOp<'c, 'a> {
    /// Static vs dynamic dispatch mode.
    pub dispatch: CallDispatchKind,
    /// Canonical symbol (when `StaticSymbol`) or unquoted module name (when `DynamicLiteral`).
    pub callee: &'a str,
    /// Arguments (prefixed by dynamic name pointer/len when `DynamicValue`).
    pub args: &'a [Value<'c, 'a>],
    /// `ON EXCEPTION` branch policy.
    pub exception_branches: ExceptionBranches,
}

impl<'c> CobolCallOp<'c, '_> {
    /// Builds the `cobol.call` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Call;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(self.args)
            .add_attributes(&[
                str_attr(ctx, "dispatch", self.dispatch.as_str()),
                str_attr(ctx, "callee", self.callee),
                str_attr(ctx, "exception_branches", self.exception_branches.as_str()),
            ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.cancel`.
pub struct CobolCancelOp<'c, 'a> {
    /// Target module name pointer (`!llvm.ptr`).
    pub target: Value<'c, 'a>,
}

impl<'c> CobolCancelOp<'c, '_> {
    /// Builds the `cobol.cancel` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let _ = ctx;
        let kind = CobolOpKind::Cancel;
        let builder = OperationBuilder::new(kind.op_name(), location).add_operands(&[self.target]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.display`.
pub struct CobolDisplayOp<'c, 'a> {
    /// Flat alternating `[ptr_0, len_0, ptr_1, len_1, ...]` operands (`!llvm.ptr`, `i64`).
    pub ptr_len_pairs: &'a [Value<'c, 'a>],
    /// Per-item [`coblang_rt_core::FieldDisplayKind`] ABI tag (`i32`).
    pub item_kinds: &'a [i32],
    /// Per-item signed decimal scale (`i32`).
    pub item_scales: &'a [i32],
    /// Destination output device (`Stdout` vs `Stderr`).
    pub device: coblang_rt_core::OutputDevice,
    /// Trailing newline policy (`AdvanceLine` vs `NoAdvancing`).
    pub advancing: coblang_rt_core::LineAdvancing,
}

impl<'c> CobolDisplayOp<'c, '_> {
    /// Builds the `cobol.display` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Display;
        let kinds_attr = DenseI32ArrayAttribute::new(ctx.raw(), self.item_kinds);
        let scales_attr = DenseI32ArrayAttribute::new(ctx.raw(), self.item_scales);
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(self.ptr_len_pairs)
            .add_attributes(&[
                i64_attr(ctx, "device", i64::from(self.device.to_abi_code())),
                i64_attr(ctx, "advancing", i64::from(self.advancing.to_abi_code())),
                (Identifier::new(ctx.raw(), "item_kinds"), kinds_attr.into()),
                (
                    Identifier::new(ctx.raw(), "item_scales"),
                    scales_attr.into(),
                ),
            ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.accept`.
pub struct CobolAcceptOp<'c, 'a> {
    /// Destination field pointer (`!llvm.ptr`).
    pub dst: Value<'c, 'a>,
    /// Input source kind.
    pub source: AcceptSourceKind,
    /// Exception branch policy.
    pub exception_branches: ExceptionBranches,
}

impl<'c> CobolAcceptOp<'c, '_> {
    /// Builds the `cobol.accept` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Accept;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&[self.dst])
            .add_attributes(&[
                str_attr(ctx, "source", self.source.as_str()),
                str_attr(ctx, "exception_branches", self.exception_branches.as_str()),
            ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.string`.
pub struct CobolStringOp<'c, 'a> {
    /// Destination buffer view (`!llvm.ptr`) and source/pointer operands.
    pub operands: &'a [Value<'c, 'a>],
}

impl<'c> CobolStringOp<'c, '_> {
    /// Builds the `cobol.string` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let _ = ctx;
        let kind = CobolOpKind::StringVerb;
        let builder = OperationBuilder::new(kind.op_name(), location).add_operands(self.operands);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.unstring`.
pub struct CobolUnstringOp<'c, 'a> {
    /// Source buffer view (`!llvm.ptr`) and destination/delimiter operands.
    pub operands: &'a [Value<'c, 'a>],
}

impl<'c> CobolUnstringOp<'c, '_> {
    /// Builds the `cobol.unstring` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let _ = ctx;
        let kind = CobolOpKind::UnstringVerb;
        let builder = OperationBuilder::new(kind.op_name(), location).add_operands(self.operands);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.inspect`.
pub struct CobolInspectOp<'c, 'a> {
    /// Target buffer view (`!llvm.ptr`) and tally/replace operands.
    pub operands: &'a [Value<'c, 'a>],
}

impl<'c> CobolInspectOp<'c, '_> {
    /// Builds the `cobol.inspect` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let _ = ctx;
        let kind = CobolOpKind::Inspect;
        let builder = OperationBuilder::new(kind.op_name(), location).add_operands(self.operands);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.file_op`.
pub struct CobolFileOp<'c, 'a> {
    /// File I/O verb selector.
    pub verb: FileVerbKind,
    /// `FileConnectorId` raw index.
    pub connector_id: u32,
    /// Optional record/key operands.
    pub operands: &'a [Value<'c, 'a>],
}

impl<'c> CobolFileOp<'c, '_> {
    /// Builds the `cobol.file_op` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::FileOp;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(self.operands)
            .add_attributes(&[
                str_attr(ctx, "verb", self.verb.as_str()),
                i64_attr(ctx, "connector_id", i64::from(self.connector_id)),
            ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.sort_merge`.
pub struct CobolSortMergeOp<'c, 'a> {
    /// Sort/merge file connector raw index.
    pub connector_id: u32,
    /// Optional record/table operands.
    pub operands: &'a [Value<'c, 'a>],
}

impl<'c> CobolSortMergeOp<'c, '_> {
    /// Builds the `cobol.sort_merge` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::SortMerge;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(self.operands)
            .add_attributes(&[i64_attr(ctx, "connector_id", i64::from(self.connector_id))]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.report`.
pub struct CobolReportOp {
    /// `ReportId` raw index.
    pub report_id: u32,
    /// Action tag (`"initiate"`, `"generate"`, `"terminate"`).
    pub action: &'static str,
}

impl CobolReportOp {
    /// Builds the `cobol.report` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build<'c>(
        self,
        ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Report;
        let builder = OperationBuilder::new(kind.op_name(), location).add_attributes(&[
            i64_attr(ctx, "report_id", i64::from(self.report_id)),
            str_attr(ctx, "action", self.action),
        ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.screen`.
pub struct CobolScreenOp<'c, 'a> {
    /// Action tag (`"display"` vs `"accept"`).
    pub action: &'static str,
    /// Screen descriptor operands.
    pub operands: &'a [Value<'c, 'a>],
}

impl<'c> CobolScreenOp<'c, '_> {
    /// Builds the `cobol.screen` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Screen;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(self.operands)
            .add_attributes(&[str_attr(ctx, "action", self.action)]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.xml_json`.
pub struct CobolXmlJsonOp<'c, 'a> {
    /// Format/action tag (`"xml_parse"`, `"xml_generate"`, `"json_parse"`, `"json_generate"`).
    pub action: &'static str,
    /// Document/target operands.
    pub operands: &'a [Value<'c, 'a>],
}

impl<'c> CobolXmlJsonOp<'c, '_> {
    /// Builds the `cobol.xml_json` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::XmlJson;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(self.operands)
            .add_attributes(&[str_attr(ctx, "action", self.action)]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.set_return_code`.
pub struct CobolSetReturnCodeOp<'c, 'a> {
    /// Signed 32-bit `RETURN-CODE` value (`i32`).
    pub code: Value<'c, 'a>,
}

impl<'c> CobolSetReturnCodeOp<'c, '_> {
    /// Builds the `cobol.set_return_code` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let _ = ctx;
        let kind = CobolOpKind::SetReturnCode;
        let builder = OperationBuilder::new(kind.op_name(), location).add_operands(&[self.code]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.get_return_code` (`-> i32`).
pub struct CobolGetReturnCodeOp;

impl CobolGetReturnCodeOp {
    /// Builds the `cobol.get_return_code` [`Operation`] producing `i32`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build<'c>(
        self,
        ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        let kind = CobolOpKind::GetReturnCode;
        let i32_ty = IntegerType::new(ctx.raw(), 32).into();
        let builder = OperationBuilder::new(kind.op_name(), location).add_results(&[i32_ty]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.stop_run`.
pub struct CobolStopRunOp<'c, 'a> {
    /// Process exit status (`i32`).
    pub status: Value<'c, 'a>,
}

impl<'c> CobolStopRunOp<'c, '_> {
    /// Builds the `cobol.stop_run` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let _ = ctx;
        let kind = CobolOpKind::StopRun;
        let builder = OperationBuilder::new(kind.op_name(), location).add_operands(&[self.status]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.goback`.
pub struct CobolGobackOp {
    /// `Goback` vs `ExitProgram` mode.
    pub mode: GobackMode,
}

impl CobolGobackOp {
    /// Builds the `cobol.goback` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build<'c>(
        self,
        ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Goback;
        let builder = OperationBuilder::new(kind.op_name(), location).add_attributes(&[str_attr(
            ctx,
            "mode",
            self.mode.as_str(),
        )]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.trim` (`-> (!llvm.ptr, i64)`).
pub struct CobolTrimOp<'c, 'a> {
    /// Source byte pointer (`!llvm.ptr`).
    pub src_ptr: Value<'c, 'a>,
    /// Source byte length (`i64`).
    pub src_len: Value<'c, 'a>,
    /// Trim side (`Both`, `Leading`, `Trailing`).
    pub direction: coblang_rt_core::TrimDirection,
}

impl<'c> CobolTrimOp<'c, '_> {
    /// Builds the `cobol.trim` [`Operation`] producing `(!llvm.ptr, i64)`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Trim;
        let i64_ty = IntegerType::new(ctx.raw(), 64).into();
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(&[self.src_ptr, self.src_len])
            .add_attributes(&[i64_attr(
                ctx,
                "direction",
                i64::from(self.direction.to_abi_code()),
            )])
            .add_results(&[llvm_ptr_type(ctx), i64_ty]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Strongly-typed C-ABI runtime target symbol for [`CobolRtDispatchOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RtDispatchTarget {
    /// `coblang_rt_move_desc`.
    MoveDesc,
    /// `coblang_rt_disp_begin`.
    DispBegin,
    /// `coblang_rt_disp_push_raw`.
    DispPushRaw,
    /// `coblang_rt_disp_push_field`.
    DispPushField,
    /// `coblang_rt_disp_push_hex`.
    DispPushHex,
    /// `coblang_rt_disp_flush`.
    DispFlush,
    /// `coblang_rt_dec_begin`.
    DecBegin,
    /// `coblang_rt_dec_push_field`.
    DecPushField,
    /// `coblang_rt_dec_push_ascii`.
    DecPushAscii,
    /// `coblang_rt_dec_neg`.
    DecNeg,
    /// `coblang_rt_dec_binop`.
    DecBinop,
    /// `coblang_rt_dec_store`.
    DecStore,
    /// `coblang_rt_dec_div_rem`.
    DecDivRem,
    /// `coblang_rt_dec_cmp`.
    DecCmp,
    /// `coblang_rt_cmp_alpha`.
    CmpAlpha,
    /// `coblang_rt_cmp_alpha_ex`.
    CmpAlphaEx,
    /// `coblang_rt_class_check`.
    ClassCheck,
    /// `coblang_rt_class_custom`.
    ClassCustom,
    /// `coblang_rt_get_switch`.
    GetSwitch,
    /// `coblang_rt_set_switch`.
    SetSwitch,
    /// `coblang_rt_init_begin`.
    InitBegin,
    /// `coblang_rt_init_push_rep`.
    InitPushRep,
    /// `coblang_rt_init_exec`.
    InitExec,
    /// `coblang_rt_check_odo`.
    CheckOdo,
    /// `coblang_rt_check_sub`.
    CheckSub,
    /// `coblang_rt_check_refmod`.
    CheckRefMod,
    /// `coblang_rt_sort_table`.
    SortTable,
    /// `coblang_rt_disp_flush_env`.
    DispFlushEnv,
    /// `coblang_rt_accept`.
    Accept,
    /// `coblang_rt_str_begin`.
    StrBegin,
    /// `coblang_rt_str_set_delim`.
    StrSetDelim,
    /// `coblang_rt_str_push_src`.
    StrPushSrc,
    /// `coblang_rt_str_exec`.
    StrExec,
    /// `coblang_rt_str_last_ptr`.
    StrLastPtr,
    /// `coblang_rt_unstr_begin`.
    UnstrBegin,
    /// `coblang_rt_unstr_push_delim`.
    UnstrPushDelim,
    /// `coblang_rt_unstr_set_delim_spec`.
    UnstrSetDelimSpec,
    /// `coblang_rt_unstr_step`.
    UnstrStep,
    /// `coblang_rt_unstr_finish`.
    UnstrFinish,
    /// `coblang_rt_unstr_tally_delta`.
    UnstrTallyDelta,
    /// `coblang_rt_insp_begin`.
    InspBegin,
    /// `coblang_rt_insp_clear_win`.
    InspClearWin,
    /// `coblang_rt_insp_set_win`.
    InspSetWin,
    /// `coblang_rt_insp_set_op`.
    InspSetOp,
    /// `coblang_rt_insp_push_tally`.
    InspPushTally,
    /// `coblang_rt_insp_push_replace`.
    InspPushReplace,
    /// `coblang_rt_insp_exec`.
    InspExec,
    /// `coblang_rt_insp_get_delta`.
    InspGetDelta,
    /// `coblang_rt_insp_convert`.
    InspConvert,
    /// `coblang_rt_alloc`.
    Alloc,
    /// `coblang_rt_alloc_fill_pattern`.
    AllocFillPattern,
    /// `coblang_rt_free`.
    Free,
    /// `coblang_rt_check_based`.
    CheckBased,
    /// `coblang_rt_resolve_entry`.
    ResolveEntry,
    /// `coblang_rt_resolve_external`.
    ResolveExternal,
    /// `coblang_rt_call_invoke`.
    CallInvoke,
    /// `coblang_rt_get_call_param`.
    GetCallParam,
    /// `coblang_rt_current_narg`.
    CurrentNarg,
    /// `coblang_rt_copy_cli_arg`.
    CopyCliArg,
    /// `coblang_rt_cancel_by_slice`.
    CancelBySlice,
    /// `coblang_rt_push_call_frame`.
    PushCallFrame,
    /// `coblang_rt_pop_call_frame`.
    PopCallFrame,
    /// `coblang_rt_raw_copy`.
    RawCopy,
    /// `coblang_rt_str_set_dst_spec`.
    StrSetDstSpec,
    /// `coblang_rt_utf8_refmod_off`.
    Utf8RefModOff,
    /// `coblang_rt_utf8_refmod_len`.
    Utf8RefModLen,
    /// `coblang_rt_file_open`.
    FileOpen,
    /// `coblang_rt_file_close`.
    FileClose,
    /// `coblang_rt_file_read`.
    FileRead,
    /// `coblang_rt_file_write`.
    FileWrite,
    /// `coblang_rt_file_rewrite`.
    FileRewrite,
    /// `coblang_rt_file_delete`.
    FileDelete,
    /// `coblang_rt_file_delete_physical`.
    FileDeletePhysical,
    /// `coblang_rt_file_start`.
    FileStart,
    /// `coblang_rt_file_unlock`.
    FileUnlock,
    /// `coblang_rt_file_last_rec_len`.
    FileLastRecLen,
    /// `coblang_rt_file_last_rel_key`.
    FileLastRelKey,
    /// `coblang_rt_file_last_linage_counter`.
    FileLastLinageCounter,
    /// `coblang_rt_file_fatal_abort`.
    FileFatalAbort,
    /// `coblang_rt_file_set_linage`.
    FileSetLinage,
    /// `coblang_rt_decl_enter`.
    DeclEnter,
    /// `coblang_rt_decl_leave`.
    DeclLeave,
    /// `coblang_rt_sort_begin`.
    SortBegin,
    /// `coblang_rt_sort_begin_merge_stream`.
    SortBeginMergeStream,
    /// `coblang_rt_sort_release`.
    SortRelease,
    /// `coblang_rt_sort_finish_input`.
    SortFinishInput,
    /// `coblang_rt_sort_return`.
    SortReturn,
    /// `coblang_rt_sort_end`.
    SortEnd,
    /// `coblang_rt_m20_reg_ptr`.
    M20RegPtr,
    /// `coblang_rt_m20_reg_len`.
    M20RegLen,
    /// `coblang_rt_m20_set_ns`.
    M20SetNs,
    /// `coblang_rt_m20_set_count_dst`.
    M20SetCountDst,
    /// `coblang_rt_m20_push_odo`.
    M20PushOdo,
    /// `coblang_rt_m20_xml_gen`.
    M20XmlGen,
    /// `coblang_rt_m20_json_gen`.
    M20JsonGen,
    /// `coblang_rt_m20_json_parse`.
    M20JsonParse,
    /// `coblang_rt_m20_xml_parse_begin`.
    M20XmlParseBegin,
    /// `coblang_rt_m20_xml_parse_feed`.
    M20XmlParseFeed,
    /// `coblang_rt_m20_xml_parse_next`.
    M20XmlParseNext,
    /// `coblang_rt_m20_xml_parse_after`.
    M20XmlParseAfter,
    /// `coblang_rt_m20_xml_parse_end`.
    M20XmlParseEnd,
    /// `coblang_rt_m20_oo_reg_class`.
    M20OoRegClass,
    /// `coblang_rt_m20_oo_reg_method`.
    M20OoRegMethod,
    /// `coblang_rt_m20_oo_self_handle`.
    M20OoSelfHandle,
    /// `coblang_rt_m20_oo_self_data_ptr`.
    M20OoSelfDataPtr,
    /// `coblang_rt_m20_oo_begin_invoke`.
    M20OoBeginInvoke,
    /// `coblang_rt_m20_oo_push_arg`.
    M20OoPushArg,
    /// `coblang_rt_m20_oo_invoke`.
    M20OoInvoke,
    /// `coblang_rt_m20_dec_push_numval`.
    M20DecPushNumval,
    /// `coblang_rt_m12_enter_module`.
    M12EnterModule,
    /// `coblang_rt_m12_leave_module`.
    M12LeaveModule,
    /// `coblang_rt_m12_begin`.
    M12Begin,
    /// `coblang_rt_m12_set_context`.
    M12SetContext,
    /// `coblang_rt_m12_pop_dec_arg`.
    M12PopDecArg,
    /// `coblang_rt_m12_push_bytes_arg`.
    M12PushBytesArg,
    /// `coblang_rt_m12_push_keyword_arg`.
    M12PushKeywordArg,
    /// `coblang_rt_m12_push_omitted_arg`.
    M12PushOmittedArg,
    /// `coblang_rt_m12_push_alg_probe`.
    M12PushAlgProbe,
    /// `coblang_rt_m12_series_begin`.
    M12SeriesBegin,
    /// `coblang_rt_m12_series_end`.
    M12SeriesEnd,
    /// `coblang_rt_m12_eval_to_dec`.
    M12EvalToDec,
    /// `coblang_rt_m12_eval_bytes`.
    M12EvalBytes,
    /// `coblang_rt_m12_format_display_dec`.
    M12FormatDisplayDec,
    /// `coblang_rt_m12_apply_refmod`.
    M12ApplyRefMod,
    /// `coblang_rt_m13_set_turn`.
    M13SetTurn,
    /// `coblang_rt_m13_last_exception_off`.
    M13LastExceptionOff,
    /// `coblang_rt_m13_set_stmt_loc`.
    M13SetStmtLoc,
    /// `coblang_rt_m13_raise`.
    M13Raise,
    /// `coblang_rt_m13_take_pending_ec`.
    M13TakePendingEc,
    /// `coblang_rt_m13_check_fatal_unhandled`.
    M13CheckFatalUnhandled,
    /// `coblang_rt_m13_enter_decl`.
    M13EnterDecl,
    /// `coblang_rt_m13_set_resume`.
    M13SetResume,
    /// `coblang_rt_m13_finish_decl`.
    M13FinishDecl,
    /// `coblang_rt_m13_debug_set_suppress`.
    M13DebugSetSuppress,
    /// `coblang_rt_m13_debug_prepare`.
    M13DebugPrepare,
    /// `coblang_rt_m13_rec_frame_begin`.
    M13RecFrameBegin,
    /// `coblang_rt_m13_rec_frame_save`.
    M13RecFrameSave,
    /// `coblang_rt_m13_rec_frame_restore`.
    M13RecFrameRestore,
    /// `coblang_rt_report_init_schema`.
    ReportInitSchema,
    /// `coblang_rt_report_config_group`.
    ReportConfigGroup,
    /// `coblang_rt_report_config_sum`.
    ReportConfigSum,
    /// `coblang_rt_report_config_sum_link`.
    ReportConfigSumLink,
    /// `coblang_rt_report_initiate`.
    ReportInitiate,
    /// `coblang_rt_report_set_dynamic_env`.
    ReportSetDynamicEnv,
    /// `coblang_rt_report_snapshot_control`.
    ReportSnapshotControl,
    /// `coblang_rt_report_begin_generate`.
    ReportBeginGenerate,
    /// `coblang_rt_report_begin_terminate`.
    ReportBeginTerminate,
    /// `coblang_rt_report_next_step`.
    ReportNextStep,
    /// `coblang_rt_report_check_after_new`.
    ReportCheckAfterNew,
    /// `coblang_rt_report_sync_sum_field`.
    ReportSyncSumField,
    /// `coblang_rt_report_submit_gate`.
    ReportSubmitGate,
    /// `coblang_rt_report_collect_line_spec`.
    ReportCollectLineSpec,
    /// `coblang_rt_report_finish_collect`.
    ReportFinishCollect,
    /// `coblang_rt_report_suppress`.
    ReportSuppress,
    /// `coblang_rt_report_after_hook`.
    ReportAfterHook,
    /// `coblang_rt_report_begin_render_line`.
    ReportBeginRenderLine,
    /// `coblang_rt_report_put_item`.
    ReportPutItem,
    /// `coblang_rt_report_finish_render_group`.
    ReportFinishRenderGroup,
    /// `coblang_rt_report_add_sum_ascii`.
    ReportAddSumAscii,
    /// `coblang_rt_screen_begin_stmt`.
    ScreenBeginStmt,
    /// `coblang_rt_screen_set_attr`.
    ScreenSetAttr,
    /// `coblang_rt_screen_stage_display_item`.
    ScreenStageDisplayItem,
    /// `coblang_rt_screen_stage_edit_item`.
    ScreenStageEditItem,
    /// `coblang_rt_screen_execute_display`.
    ScreenExecuteDisplay,
    /// `coblang_rt_screen_execute_accept`.
    ScreenExecuteAccept,
}

/// C-ABI return type classification of [`RtDispatchTarget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RtReturnKind {
    /// Void (`()`) return.
    Void,
    /// 32-bit integer (`i32`) status/outcome return.
    I32,
    /// 64-bit integer (`i64`) count/pointer/delta return.
    I64,
    /// Opaque `!llvm.ptr` pointer return (`ALLOCATE`, `SET ... TO ENTRY`).
    Ptr,
}

impl RtDispatchTarget {
    /// Returns the exact C-ABI symbol name exported by `coblang-rt-abi`.
    #[must_use]
    pub const fn symbol_name(self) -> &'static str {
        if let Some(sym) = self.core_or_io_symbol_name() {
            sym
        } else {
            self.milestone_symbol_name()
        }
    }

    /// Returns `Some(symbol)` for M5–M16 core, string, call, file-I/O, and sort targets.
    const fn core_or_io_symbol_name(self) -> Option<&'static str> {
        match self {
            Self::MoveDesc => Some("coblang_rt_move_desc"),
            Self::DispBegin => Some("coblang_rt_disp_begin"),
            Self::DispPushRaw => Some("coblang_rt_disp_push_raw"),
            Self::DispPushField => Some("coblang_rt_disp_push_field"),
            Self::DispPushHex => Some("coblang_rt_disp_push_hex"),
            Self::DispFlush => Some("coblang_rt_disp_flush"),
            Self::DecBegin => Some("coblang_rt_dec_begin"),
            Self::DecPushField => Some("coblang_rt_dec_push_field"),
            Self::DecPushAscii => Some("coblang_rt_dec_push_ascii"),
            Self::DecNeg => Some("coblang_rt_dec_neg"),
            Self::DecBinop => Some("coblang_rt_dec_binop"),
            Self::DecStore => Some("coblang_rt_dec_store"),
            Self::DecDivRem => Some("coblang_rt_dec_div_rem"),
            Self::DecCmp => Some("coblang_rt_dec_cmp"),
            Self::CmpAlpha => Some("coblang_rt_cmp_alpha"),
            Self::CmpAlphaEx => Some("coblang_rt_cmp_alpha_ex"),
            Self::ClassCheck => Some("coblang_rt_class_check"),
            Self::ClassCustom => Some("coblang_rt_class_custom"),
            Self::GetSwitch => Some("coblang_rt_get_switch"),
            Self::SetSwitch => Some("coblang_rt_set_switch"),
            Self::InitBegin => Some("coblang_rt_init_begin"),
            Self::InitPushRep => Some("coblang_rt_init_push_rep"),
            Self::InitExec => Some("coblang_rt_init_exec"),
            Self::CheckOdo => Some("coblang_rt_check_odo"),
            Self::CheckSub => Some("coblang_rt_check_sub"),
            Self::CheckRefMod => Some("coblang_rt_check_refmod"),
            Self::SortTable => Some("coblang_rt_sort_table"),
            Self::DispFlushEnv => Some("coblang_rt_disp_flush_env"),
            Self::Accept => Some("coblang_rt_accept"),
            Self::StrBegin => Some("coblang_rt_str_begin"),
            Self::StrSetDelim => Some("coblang_rt_str_set_delim"),
            Self::StrPushSrc => Some("coblang_rt_str_push_src"),
            Self::StrExec => Some("coblang_rt_str_exec"),
            Self::StrLastPtr => Some("coblang_rt_str_last_ptr"),
            Self::UnstrBegin => Some("coblang_rt_unstr_begin"),
            Self::UnstrPushDelim => Some("coblang_rt_unstr_push_delim"),
            Self::UnstrSetDelimSpec => Some("coblang_rt_unstr_set_delim_spec"),
            Self::UnstrStep => Some("coblang_rt_unstr_step"),
            Self::UnstrFinish => Some("coblang_rt_unstr_finish"),
            Self::UnstrTallyDelta => Some("coblang_rt_unstr_tally_delta"),
            Self::InspBegin => Some("coblang_rt_insp_begin"),
            Self::InspClearWin => Some("coblang_rt_insp_clear_win"),
            Self::InspSetWin => Some("coblang_rt_insp_set_win"),
            Self::InspSetOp => Some("coblang_rt_insp_set_op"),
            Self::InspPushTally => Some("coblang_rt_insp_push_tally"),
            Self::InspPushReplace => Some("coblang_rt_insp_push_replace"),
            Self::InspExec => Some("coblang_rt_insp_exec"),
            Self::InspGetDelta => Some("coblang_rt_insp_get_delta"),
            Self::InspConvert => Some("coblang_rt_insp_convert"),
            Self::Alloc => Some("coblang_rt_alloc"),
            Self::AllocFillPattern => Some("coblang_rt_alloc_fill_pattern"),
            Self::Free => Some("coblang_rt_free"),
            Self::CheckBased => Some("coblang_rt_check_based"),
            Self::ResolveEntry => Some("coblang_rt_resolve_entry"),
            Self::ResolveExternal => Some("coblang_rt_resolve_external"),
            Self::CallInvoke => Some("coblang_rt_call_invoke"),
            Self::GetCallParam => Some("coblang_rt_get_call_param"),
            Self::CurrentNarg => Some("coblang_rt_current_narg"),
            Self::CopyCliArg => Some("coblang_rt_copy_cli_arg"),
            Self::CancelBySlice => Some("coblang_rt_cancel_by_slice"),
            Self::PushCallFrame => Some("coblang_rt_push_call_frame"),
            Self::PopCallFrame => Some("coblang_rt_pop_call_frame"),
            Self::RawCopy => Some("coblang_rt_raw_copy"),
            Self::StrSetDstSpec => Some("coblang_rt_str_set_dst_spec"),
            Self::Utf8RefModOff => Some("coblang_rt_utf8_refmod_off"),
            Self::Utf8RefModLen => Some("coblang_rt_utf8_refmod_len"),
            Self::FileOpen => Some("coblang_rt_file_open"),
            Self::FileClose => Some("coblang_rt_file_close"),
            Self::FileRead => Some("coblang_rt_file_read"),
            Self::FileWrite => Some("coblang_rt_file_write"),
            Self::FileRewrite => Some("coblang_rt_file_rewrite"),
            Self::FileDelete => Some("coblang_rt_file_delete"),
            Self::FileDeletePhysical => Some("coblang_rt_file_delete_physical"),
            Self::FileStart => Some("coblang_rt_file_start"),
            Self::FileUnlock => Some("coblang_rt_file_unlock"),
            Self::FileLastRecLen => Some("coblang_rt_file_last_rec_len"),
            Self::FileLastRelKey => Some("coblang_rt_file_last_rel_key"),
            Self::FileLastLinageCounter => Some("coblang_rt_file_last_linage_counter"),
            Self::FileFatalAbort => Some("coblang_rt_file_fatal_abort"),
            Self::FileSetLinage => Some("coblang_rt_file_set_linage"),
            Self::DeclEnter => Some("coblang_rt_decl_enter"),
            Self::DeclLeave => Some("coblang_rt_decl_leave"),
            Self::SortBegin => Some("coblang_rt_sort_begin"),
            Self::SortBeginMergeStream => Some("coblang_rt_sort_begin_merge_stream"),
            Self::SortRelease => Some("coblang_rt_sort_release"),
            Self::SortFinishInput => Some("coblang_rt_sort_finish_input"),
            Self::SortReturn => Some("coblang_rt_sort_return"),
            Self::SortEnd => Some("coblang_rt_sort_end"),
            _ => None,
        }
    }

    /// Returns the C-ABI symbol name for M12, M13, M18, M19, and M20 targets.
    const fn milestone_symbol_name(self) -> &'static str {
        match self {
            Self::M20RegPtr => "coblang_rt_m20_reg_ptr",
            Self::M20RegLen => "coblang_rt_m20_reg_len",
            Self::M20SetNs => "coblang_rt_m20_set_ns",
            Self::M20SetCountDst => "coblang_rt_m20_set_count_dst",
            Self::M20PushOdo => "coblang_rt_m20_push_odo",
            Self::M20XmlGen => "coblang_rt_m20_xml_gen",
            Self::M20JsonGen => "coblang_rt_m20_json_gen",
            Self::M20JsonParse => "coblang_rt_m20_json_parse",
            Self::M20XmlParseBegin => "coblang_rt_m20_xml_parse_begin",
            Self::M20XmlParseFeed => "coblang_rt_m20_xml_parse_feed",
            Self::M20XmlParseNext => "coblang_rt_m20_xml_parse_next",
            Self::M20XmlParseAfter => "coblang_rt_m20_xml_parse_after",
            Self::M20XmlParseEnd => "coblang_rt_m20_xml_parse_end",
            Self::M20OoRegClass => "coblang_rt_m20_oo_reg_class",
            Self::M20OoRegMethod => "coblang_rt_m20_oo_reg_method",
            Self::M20OoSelfHandle => "coblang_rt_m20_oo_self_handle",
            Self::M20OoSelfDataPtr => "coblang_rt_m20_oo_self_data_ptr",
            Self::M20OoBeginInvoke => "coblang_rt_m20_oo_begin_invoke",
            Self::M20OoPushArg => "coblang_rt_m20_oo_push_arg",
            Self::M20OoInvoke => "coblang_rt_m20_oo_invoke",
            Self::M20DecPushNumval => "coblang_rt_m20_dec_push_numval",
            Self::M12EnterModule => "coblang_rt_m12_enter_module",
            Self::M12LeaveModule => "coblang_rt_m12_leave_module",
            Self::M12Begin => "coblang_rt_m12_begin",
            Self::M12SetContext => "coblang_rt_m12_set_context",
            Self::M12PopDecArg => "coblang_rt_m12_pop_dec_arg",
            Self::M12PushBytesArg => "coblang_rt_m12_push_bytes_arg",
            Self::M12PushKeywordArg => "coblang_rt_m12_push_keyword_arg",
            Self::M12PushOmittedArg => "coblang_rt_m12_push_omitted_arg",
            Self::M12PushAlgProbe => "coblang_rt_m12_push_alg_probe",
            Self::M12SeriesBegin => "coblang_rt_m12_series_begin",
            Self::M12SeriesEnd => "coblang_rt_m12_series_end",
            Self::M12EvalToDec => "coblang_rt_m12_eval_to_dec",
            Self::M12EvalBytes => "coblang_rt_m12_eval_bytes",
            Self::M12FormatDisplayDec => "coblang_rt_m12_format_display_dec",
            Self::M12ApplyRefMod => "coblang_rt_m12_apply_refmod",
            Self::M13SetTurn => "coblang_rt_m13_set_turn",
            Self::M13LastExceptionOff => "coblang_rt_m13_last_exception_off",
            Self::M13SetStmtLoc => "coblang_rt_m13_set_stmt_loc",
            Self::M13Raise => "coblang_rt_m13_raise",
            Self::M13TakePendingEc => "coblang_rt_m13_take_pending_ec",
            Self::M13CheckFatalUnhandled => "coblang_rt_m13_check_fatal_unhandled",
            Self::M13EnterDecl => "coblang_rt_m13_enter_decl",
            Self::M13SetResume => "coblang_rt_m13_set_resume",
            Self::M13FinishDecl => "coblang_rt_m13_finish_decl",
            Self::M13DebugSetSuppress => "coblang_rt_m13_debug_set_suppress",
            Self::M13DebugPrepare => "coblang_rt_m13_debug_prepare",
            Self::M13RecFrameBegin => "coblang_rt_m13_rec_frame_begin",
            Self::M13RecFrameSave => "coblang_rt_m13_rec_frame_save",
            Self::M13RecFrameRestore => "coblang_rt_m13_rec_frame_restore",
            Self::ReportInitSchema => "coblang_rt_report_init_schema",
            Self::ReportConfigGroup => "coblang_rt_report_config_group",
            Self::ReportConfigSum => "coblang_rt_report_config_sum",
            Self::ReportConfigSumLink => "coblang_rt_report_config_sum_link",
            Self::ReportInitiate => "coblang_rt_report_initiate",
            Self::ReportSetDynamicEnv => "coblang_rt_report_set_dynamic_env",
            Self::ReportSnapshotControl => "coblang_rt_report_snapshot_control",
            Self::ReportBeginGenerate => "coblang_rt_report_begin_generate",
            Self::ReportBeginTerminate => "coblang_rt_report_begin_terminate",
            Self::ReportNextStep => "coblang_rt_report_next_step",
            Self::ReportCheckAfterNew => "coblang_rt_report_check_after_new",
            Self::ReportSyncSumField => "coblang_rt_report_sync_sum_field",
            Self::ReportSubmitGate => "coblang_rt_report_submit_gate",
            Self::ReportCollectLineSpec => "coblang_rt_report_collect_line_spec",
            Self::ReportFinishCollect => "coblang_rt_report_finish_collect",
            Self::ReportSuppress => "coblang_rt_report_suppress",
            Self::ReportAfterHook => "coblang_rt_report_after_hook",
            Self::ReportBeginRenderLine => "coblang_rt_report_begin_render_line",
            Self::ReportPutItem => "coblang_rt_report_put_item",
            Self::ReportFinishRenderGroup => "coblang_rt_report_finish_render_group",
            Self::ReportAddSumAscii => "coblang_rt_report_add_sum_ascii",
            Self::ScreenBeginStmt => "coblang_rt_screen_begin_stmt",
            Self::ScreenSetAttr => "coblang_rt_screen_set_attr",
            Self::ScreenStageDisplayItem => "coblang_rt_screen_stage_display_item",
            Self::ScreenStageEditItem => "coblang_rt_screen_stage_edit_item",
            Self::ScreenExecuteAccept => "coblang_rt_screen_execute_accept",
            _ => "coblang_rt_screen_execute_display",
        }
    }

    /// Returns the C-ABI return type classification ([`RtReturnKind`]) of `self`.
    #[must_use]
    pub const fn return_kind(self) -> RtReturnKind {
        match self {
            Self::DispBegin
            | Self::DecBegin
            | Self::DecNeg
            | Self::InitBegin
            | Self::StrBegin
            | Self::InspBegin
            | Self::InspClearWin
            | Self::InspPushTally
            | Self::InspPushReplace
            | Self::CancelBySlice
            | Self::RawCopy
            | Self::M20SetNs
            | Self::M20SetCountDst
            | Self::M20PushOdo
            | Self::M20XmlParseFeed
            | Self::M20OoRegClass
            | Self::M20OoRegMethod
            | Self::M20OoBeginInvoke
            | Self::M20OoPushArg
            | Self::ReportInitSchema
            | Self::ReportConfigGroup
            | Self::ReportConfigSum
            | Self::ReportConfigSumLink
            | Self::ReportSetDynamicEnv
            | Self::ReportSnapshotControl
            | Self::ReportSyncSumField
            | Self::ReportCollectLineSpec
            | Self::ReportFinishCollect
            | Self::ReportSuppress
            | Self::ReportAfterHook
            | Self::ReportPutItem
            | Self::ReportFinishRenderGroup
            | Self::ReportAddSumAscii
            | Self::ScreenBeginStmt
            | Self::ScreenSetAttr
            | Self::ScreenStageDisplayItem
            | Self::ScreenStageEditItem => RtReturnKind::Void,
            Self::StrLastPtr
            | Self::UnstrStep
            | Self::UnstrTallyDelta
            | Self::InspGetDelta
            | Self::CopyCliArg
            | Self::Utf8RefModOff
            | Self::Utf8RefModLen
            | Self::FileLastRecLen
            | Self::FileLastRelKey
            | Self::FileLastLinageCounter
            | Self::SortReturn
            | Self::M20RegLen
            | Self::M20OoSelfHandle
            | Self::M13FinishDecl => RtReturnKind::I64,
            Self::Alloc
            | Self::AllocFillPattern
            | Self::ResolveEntry
            | Self::ResolveExternal
            | Self::GetCallParam
            | Self::M20RegPtr
            | Self::M20OoSelfDataPtr => RtReturnKind::Ptr,
            _ => RtReturnKind::I32,
        }
    }
}

/// Builder for `cobol.rt_dispatch`.
pub struct CobolRtDispatchOp<'c, 'a> {
    /// Strongly-typed C-ABI runtime target.
    pub target: RtDispatchTarget,
    /// Typed SSA operands matching `target`'s ABI signature.
    pub operands: &'a [Value<'c, 'a>],
}

impl<'c> CobolRtDispatchOp<'c, '_> {
    /// Builds the `cobol.rt_dispatch` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::RtDispatch;
        let i32_ty: Type<'c> = IntegerType::new(ctx.raw(), 32).into();
        let i64_ty: Type<'c> = IntegerType::new(ctx.raw(), 64).into();
        let ptr_ty: Type<'c> = llvm_ptr_type(ctx);
        let res_slice: &[Type<'c>] = match self.target.return_kind() {
            RtReturnKind::Void => &[],
            RtReturnKind::I32 => core::slice::from_ref(&i32_ty),
            RtReturnKind::I64 => core::slice::from_ref(&i64_ty),
            RtReturnKind::Ptr => core::slice::from_ref(&ptr_ty),
        };
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(self.operands)
            .add_attributes(&[str_attr(ctx, "callee", self.target.symbol_name())])
            .add_results(res_slice);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.loop` (`PERFORM` loop owning `cond`, `body`, and `step` regions).
pub struct CobolLoopOp<'c> {
    /// `"user_perform"` vs `"after_tier"`.
    pub loop_kind: &'static str,
    /// `"before"` vs `"after"`.
    pub test_pos: &'static str,
    /// Region evaluating `until` and yielding `i1` via `scf.yield`.
    pub cond_region: Region<'c>,
    /// Region executing loop body statements and ending in `scf.yield`.
    pub body_region: Region<'c>,
    /// Region executing end-of-iteration step statements and ending in `scf.yield`.
    pub step_region: Region<'c>,
}

impl<'c> CobolLoopOp<'c> {
    /// Builds the `cobol.loop` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Loop;
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_attributes(&[
                str_attr(ctx, "loop_kind", self.loop_kind),
                str_attr(ctx, "test_pos", self.test_pos),
            ])
            .add_regions([self.cond_region, self.body_region, self.step_region]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.sentence` anchoring `NEXT SENTENCE` branch targets.
pub struct CobolSentenceOp<'c> {
    /// Single-block sentence body region ending in `scf.yield`.
    pub body_region: Region<'c>,
}

impl<'c> CobolSentenceOp<'c> {
    /// Builds the `cobol.sentence` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let _ = ctx;
        let kind = CobolOpKind::Sentence;
        let builder =
            OperationBuilder::new(kind.op_name(), location).add_regions([self.body_region]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.flow_exit` (`EXIT PARAGRAPH/SECTION/PERFORM [CYCLE]` or `NEXT SENTENCE`).
pub struct CobolFlowExitOp {
    /// `"paragraph"`, `"section"`, `"perform_break"`, `"perform_cycle"`, or `"next_sentence"`.
    pub exit_kind: &'static str,
    /// Enclosing section's inclusive last `ProcedureId` (used when `exit_kind == "section"`).
    pub section_end: u32,
}

impl CobolFlowExitOp {
    /// Builds the `cobol.flow_exit` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build<'c>(
        self,
        ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        let kind = CobolOpKind::FlowExit;
        let builder = OperationBuilder::new(kind.op_name(), location).add_attributes(&[
            str_attr(ctx, "exit_kind", self.exit_kind),
            i64_attr(ctx, "section_end", i64::from(self.section_end)),
        ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.goto` (static `GO TO`, `GO TO ... DEPENDING ON`, or `ALTER`able `GO TO`).
pub struct CobolGotoOp<'c, 'a> {
    /// Static target `ProcedureId` array (cast to `i32`).
    pub targets: &'a [i32],
    /// Optional `@__cobol_alter_<prog>_<pid>` global symbol (`""` when non-alterable).
    pub alter_slot_sym: &'a str,
    /// Optional 1-based `i32` selector value for `GO TO ... DEPENDING ON`.
    pub selector: Option<Value<'c, 'a>>,
}

impl<'c> CobolGotoOp<'c, '_> {
    /// Builds the `cobol.goto` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(self, ctx: &'c CobolMlirContext, location: Location<'c>) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Goto;
        let ops_slice: &[Value<'c, '_>] = self.selector.as_slice();
        let builder = OperationBuilder::new(kind.op_name(), location)
            .add_operands(ops_slice)
            .add_attributes(&[
                (
                    Identifier::new(ctx.raw(), "targets"),
                    DenseI32ArrayAttribute::new(ctx.raw(), self.targets).into(),
                ),
                str_attr(ctx, "alter_slot_sym", self.alter_slot_sym),
            ]);
        finish_op_build(builder, kind.op_name())
    }
}

/// Builder for `cobol.yield` yielding zero or one value from a `cobol.loop` or `cobol.sentence` region.
#[derive(Debug, Clone, Copy)]
pub struct CobolYieldOp<'c, 'a> {
    /// Optional yielded SSA value (`Some(i1)` in `cobol.loop` condition region; `None` otherwise).
    pub value: Option<Value<'c, 'a>>,
}

impl<'c> CobolYieldOp<'c, '_> {
    /// Builds the `cobol.yield` [`Operation`].
    ///
    /// # Arguments
    ///
    /// * `_ctx` - Owning [`CobolMlirContext`].
    /// * `location` - Source location.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::MlirVerification`] if operation construction fails.
    pub fn build(
        self,
        _ctx: &'c CobolMlirContext,
        location: Location<'c>,
    ) -> Result<Operation<'c>> {
        let kind = CobolOpKind::Yield;
        let ops_slice: &[Value<'c, '_>] = self.value.as_slice();
        finish_op_build(
            OperationBuilder::new(kind.op_name(), location).add_operands(ops_slice),
            kind.op_name(),
        )
    }
}
