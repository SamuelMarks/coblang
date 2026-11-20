//! ISO/IEC 1989:2023 `EC-*` exception hierarchy, `>>TURN` mask state, declarative guard,
//! `RESUME` continuation target, and NIST CCVS-85 `DEBUG-ITEM` special-register state (`Milestone M13`).

use coblang_common::{CoblangError, Result};
use coblang_decimal::SizeErrorStatus;

/// Total number of canonical ISO/IEC 1989:2023 `EC-*` selectors and leaf conditions in [`EXCEPTION_CATALOG`].
pub const EXCEPTION_CATALOG_LEN: usize = 146;

/// Fixed 56-byte header prefix of the NIST CCVS-85 `DEBUG-ITEM` special register preceding `DEBUG-CONTENTS`.
pub const DEBUG_ITEM_HEADER_LEN: usize = 56;

/// Hierarchical depth of an [`ExceptionCode`] in the ISO 2023 `EC-*` tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExceptionLevel {
    /// Root `EC-ALL` wildcard matching every exception condition.
    All,
    /// Level-2 category node (e.g. `EC-SIZE`, `EC-BOUND`, `EC-I-O`).
    Category,
    /// Level-3 leaf condition (e.g. `EC-SIZE-ZERO-DIVIDE`, `EC-BOUND-SUBSCRIPT`).
    Leaf,
}

/// Default ISO/IEC 1989:2023 unhandled-exception termination classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExceptionSeverity {
    /// Non-leaf selector (`EC-ALL` or `EC-<CATEGORY>`) used in `>>TURN` and `USE AFTER EXCEPTION`.
    Selector,
    /// Fatal exception condition (terminates run unit abnormally if enabled and unhandled).
    Fatal,
    /// Non-fatal exception condition (continues at next statement when unhandled).
    NonFatal,
}

/// Cause disambiguator when mapping [`SizeErrorStatus::Overflowed`] to an ISO `EC-SIZE-*` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeOverflowCause {
    /// High-order target capacity or 256-bit intermediate overflow (`EC-SIZE-OVERFLOW`).
    HighOrderOverflow,
    /// Non-zero fractional digit discarded under `ROUNDED MODE IS PROHIBITED` (`EC-SIZE-TRUNCATION`).
    ProhibitedTruncation,
}

/// ISO/IEC 1989:2023 `EC-*` exception category grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExceptionCategory {
    /// Root `EC-ALL` wildcard.
    All,
    /// `EC-ARGUMENT-*` intrinsic/UDF argument violations.
    Argument,
    /// `EC-BOUND-*` subscript, `ODO`, `RefMod`, pointer, and table-limit bounds violations.
    Bound,
    /// `EC-CONTINUE-*` `CONTINUE AFTER` negative-duration violations.
    Continue,
    /// `EC-DATA-*` incompatible data, NaN/infinity, conversion, and null-pointer dereferences.
    Data,
    /// `EC-FLOW-*` control-flow, `COMMIT`/`ROLLBACK`, `GLOBAL EXIT`/`GOBACK`, `SEARCH`, and `USE` violations.
    Flow,
    /// `EC-FUNCTION-*` user-defined and intrinsic function call/pointer violations.
    Function,
    /// `EC-I-O-*` file status and `AT END` / `INVALID KEY` / `EOP` conditions.
    Io,
    /// `EC-IMP-*` implementor-defined exception conditions.
    Imp,
    /// `EC-LOCALE-*` locale loading and formatting conditions.
    Locale,
    /// `EC-OO-*` object-oriented conformance, null-reference, and method lookup conditions.
    Oo,
    /// `EC-ORDER-*` ordering/collation support conditions.
    Order,
    /// `EC-OVERFLOW-*` `STRING` and `UNSTRING` overflow conditions.
    Overflow,
    /// `EC-PROGRAM-*` `CALL`/`CANCEL`, recursion, and argument mismatch conditions.
    Program,
    /// `EC-RAISING-*` undeclared propagated exception conditions.
    Raising,
    /// `EC-RANGE-*` `INDEX`, `INSPECT`, `PERFORM VARYING`, and `SEARCH` range conditions.
    Range,
    /// `EC-REPORT-*` Report Writer lifecycle and page-layout conditions.
    Report,
    /// `EC-SCREEN-*` `SCREEN SECTION` coordinate and field-overlap conditions.
    Screen,
    /// `EC-SIZE-*` fixed-point overflow, zero-divide, truncation, and exponentiation conditions.
    Size,
    /// `EC-SORT-MERGE-*` active sort/merge session and `RELEASE`/`RETURN`/sequence conditions.
    SortMerge,
    /// `EC-STORAGE-*` `ALLOCATE`/`FREE` heap availability and ownership conditions.
    Storage,
    /// `EC-USER-*` user-defined `RAISE` exception conditions.
    User,
    /// `EC-VALIDATE-*` `VALIDATE` verb format/content/relation conditions.
    Validate,
}

/// Strongly-typed ISO/IEC 1989:2023 `EC-*` exception condition identifier (`0..=145`).
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExceptionCode {
    /// `EC-ALL` root wildcard.
    EcAll = 0,
    /// `EC-ARGUMENT` category.
    EcArgument = 1,
    /// `EC-ARGUMENT-FUNCTION` invalid intrinsic/UDF argument domain.
    EcArgumentFunction = 2,
    /// `EC-ARGUMENT-IMP` implementor argument condition.
    EcArgumentImp = 3,
    /// `EC-BOUND` category.
    EcBound = 4,
    /// `EC-BOUND-IMP` implementor bound condition.
    EcBoundImp = 5,
    /// `EC-BOUND-ODO` `OCCURS DEPENDING ON` object out of `min..=max`.
    EcBoundOdo = 6,
    /// `EC-BOUND-OVERFLOW` dynamic capacity overflow.
    EcBoundOverflow = 7,
    /// `EC-BOUND-PTR` pointer arithmetic out of allocated block bounds.
    EcBoundPtr = 8,
    /// `EC-BOUND-REF-MOD` reference modification offset/length out of bounds.
    EcBoundRefMod = 9,
    /// `EC-BOUND-SET` invalid `SET` index/pointer bound.
    EcBoundSet = 10,
    /// `EC-BOUND-SUBSCRIPT` table subscript out of `1..=current_occurs`.
    EcBoundSubscript = 11,
    /// `EC-BOUND-TABLE-LIMIT` dynamic table capacity exceeded.
    EcBoundTableLimit = 12,
    /// `EC-DATA` category.
    EcData = 13,
    /// `EC-DATA-CONVERSION` character set transcoding failure.
    EcDataConversion = 14,
    /// `EC-DATA-IMP` implementor data condition.
    EcDataImp = 15,
    /// `EC-DATA-INCOMPATIBLE` non-numeric data in numeric operation.
    EcDataIncompatible = 16,
    /// `EC-DATA-INFINITY` positive infinity operand/result.
    EcDataInfinity = 17,
    /// `EC-DATA-NEGATIVE-INFINITY` negative infinity operand/result.
    EcDataNegativeInfinity = 18,
    /// `EC-DATA-NOT-A-NUMBER` IEEE `NaN` operand/result.
    EcDataNotANumber = 19,
    /// `EC-DATA-PTR-NULL` dereference of `BASED`/`LINKAGE` item with `NULL` base address.
    EcDataPtrNull = 20,
    /// `EC-FLOW` category.
    EcFlow = 21,
    /// `EC-FLOW-GLOBAL-EXIT` invalid `EXIT` across global scope.
    EcFlowGlobalExit = 22,
    /// `EC-FLOW-GLOBAL-GOBACK` invalid `GOBACK` across global scope.
    EcFlowGlobalGoback = 23,
    /// `EC-FLOW-IMP` implementor control-flow condition.
    EcFlowImp = 24,
    /// `EC-FLOW-RELEASE` `RELEASE` executed outside `SORT` input procedure.
    EcFlowRelease = 25,
    /// `EC-FLOW-REPORT` report control-flow violation.
    EcFlowReport = 26,
    /// `EC-FLOW-RETURN` `RETURN` executed outside `SORT`/`MERGE` output procedure.
    EcFlowReturn = 27,
    /// `EC-FLOW-SEARCH` `SEARCH` control-flow violation.
    EcFlowSearch = 28,
    /// `EC-FLOW-USE` recursive `USE` declarative re-entrancy violation.
    EcFlowUse = 29,
    /// `EC-FUNCTION` category.
    EcFunction = 30,
    /// `EC-FUNCTION-ARG-OMITTED` required function argument omitted.
    EcFunctionArgOmitted = 31,
    /// `EC-FUNCTION-IMP` implementor function condition.
    EcFunctionImp = 32,
    /// `EC-FUNCTION-NOT-FOUND` dynamically resolved function not found.
    EcFunctionNotFound = 33,
    /// `EC-FUNCTION-PTR-INVALID` invalid `FUNCTION-POINTER` target.
    EcFunctionPtrInvalid = 34,
    /// `EC-FUNCTION-PTR-NULL` `NULL` `FUNCTION-POINTER` invocation.
    EcFunctionPtrNull = 35,
    /// `EC-I-O` category.
    EcIo = 36,
    /// `EC-I-O-AT-END` sequential `AT END` status `1x`.
    EcIoAtEnd = 37,
    /// `EC-I-O-EOP` end-of-page condition.
    EcIoEop = 38,
    /// `EC-I-O-EOP-OVERFLOW` page overflow condition.
    EcIoEopOverflow = 39,
    /// `EC-I-O-FILE-SHARING` record/file lock contention (`6x` / `5x`).
    EcIoFileSharing = 40,
    /// `EC-I-O-IMP` implementor I/O status `9x`.
    EcIoImp = 41,
    /// `EC-I-O-INVALID-KEY` indexed/relative key error status `2x`.
    EcIoInvalidKey = 42,
    /// `EC-I-O-LINAGE` invalid `LINAGE` parameter value.
    EcIoLinage = 43,
    /// `EC-I-O-LOGIC-ERROR` open/close/read/write protocol error status `4x`.
    EcIoLogicError = 44,
    /// `EC-I-O-PERMANENT-ERROR` physical/OS permanent I/O error status `3x`.
    EcIoPermanentError = 45,
    /// `EC-I-O-RECORD-CONTENT` invalid record content.
    EcIoRecordContent = 46,
    /// `EC-I-O-RECORD-OPERATION` invalid record operation.
    EcIoRecordOperation = 47,
    /// `EC-I-O-WARNING` non-fatal I/O completion warning status `0x` (`!= 00`).
    EcIoWarning = 48,
    /// `EC-IMP` category.
    EcImp = 49,
    /// `EC-IMP-ACCEPT` implementor `ACCEPT` failure.
    EcImpAccept = 50,
    /// `EC-IMP-DISPLAY` implementor `DISPLAY` failure.
    EcImpDisplay = 51,
    /// `EC-LOCALE` category.
    EcLocale = 52,
    /// `EC-LOCALE-IMP` implementor locale condition.
    EcLocaleImp = 53,
    /// `EC-LOCALE-INCOMPATIBLE` incompatible locale category.
    EcLocaleIncompatible = 54,
    /// `EC-LOCALE-INVALID` malformed locale specification.
    EcLocaleInvalid = 55,
    /// `EC-LOCALE-INVALID-PTR` invalid locale pointer.
    EcLocaleInvalidPtr = 56,
    /// `EC-LOCALE-MISSING` requested locale not installed.
    EcLocaleMissing = 57,
    /// `EC-LOCALE-SIZE` locale formatted output truncated.
    EcLocaleSize = 58,
    /// `EC-OO` category.
    EcOo = 59,
    /// `EC-OO-ARG-OMITTED` omitted required method argument.
    EcOoArgOmitted = 60,
    /// `EC-OO-CONFORMANCE` interface/class conformance failure.
    EcOoConformance = 61,
    /// `EC-OO-EXCEPTION` unhandled object exception.
    EcOoException = 62,
    /// `EC-OO-IMP` implementor OO condition.
    EcOoImp = 63,
    /// `EC-OO-METHOD` method resolution failure.
    EcOoMethod = 64,
    /// `EC-OO-NULL` `INVOKE` on `NULL` object reference.
    EcOoNull = 65,
    /// `EC-OO-RESOURCE` object allocation resource exhaustion.
    EcOoResource = 66,
    /// `EC-OO-UNIVERSAL` universal object violation.
    EcOoUniversal = 67,
    /// `EC-ORDER` category.
    EcOrder = 68,
    /// `EC-ORDER-IMP` implementor ordering condition.
    EcOrderImp = 69,
    /// `EC-ORDER-NOT-SUPPORTED` unsupported collation/order table.
    EcOrderNotSupported = 70,
    /// `EC-OVERFLOW` category.
    EcOverflow = 71,
    /// `EC-OVERFLOW-IMP` implementor overflow condition.
    EcOverflowImp = 72,
    /// `EC-OVERFLOW-STRING` `STRING` pointer overflow.
    EcOverflowString = 73,
    /// `EC-OVERFLOW-UNSTRING` `UNSTRING` receiving field/pointer overflow.
    EcOverflowUnstring = 74,
    /// `EC-PROGRAM` category.
    EcProgram = 75,
    /// `EC-PROGRAM-ARG-MISMATCH` `CALL` argument count/descriptor mismatch.
    EcProgramArgMismatch = 76,
    /// `EC-PROGRAM-ARG-OMITTED` non-`OPTIONAL` `LINKAGE` parameter omitted.
    EcProgramArgOmitted = 77,
    /// `EC-PROGRAM-CANCEL-ACTIVE` `CANCEL` of currently active program on call stack.
    EcProgramCancelActive = 78,
    /// `EC-PROGRAM-IMP` implementor subprogram condition.
    EcProgramImp = 79,
    /// `EC-PROGRAM-NOT-FOUND` dynamic `CALL` / `SET ... TO ENTRY` target not found.
    EcProgramNotFound = 80,
    /// `EC-PROGRAM-PTR-NULL` `CALL` through `NULL` `PROGRAM-POINTER`.
    EcProgramPtrNull = 81,
    /// `EC-PROGRAM-RECURSIVE-CALL` recursive call to non-`RECURSIVE` program.
    EcProgramRecursiveCall = 82,
    /// `EC-PROGRAM-RESOURCES` subprogram load resource exhaustion.
    EcProgramResources = 83,
    /// `EC-RAISING` category.
    EcRaising = 84,
    /// `EC-RAISING-IMP` implementor raising condition.
    EcRaisingImp = 85,
    /// `EC-RAISING-NOT-SPECIFIED` non-`RAISING` callee propagated an exception.
    EcRaisingNotSpecified = 86,
    /// `EC-RANGE` category.
    EcRange = 87,
    /// `EC-RANGE-IMP` implementor range condition.
    EcRangeImp = 88,
    /// `EC-RANGE-INDEX` index value outside representable range.
    EcRangeIndex = 89,
    /// `EC-RANGE-INSPECT-SIZE` `INSPECT` `REPLACING`/`CONVERTING` length mismatch.
    EcRangeInspectSize = 90,
    /// `EC-RANGE-INVALID` invalid range expression bounds.
    EcRangeInvalid = 91,
    /// `EC-RANGE-PERFORM-VARYING` invalid `PERFORM VARYING` step (`BY 0`).
    EcRangePerformVarying = 92,
    /// `EC-RANGE-PTR` invalid pointer displacement range.
    EcRangePtr = 93,
    /// `EC-RANGE-SEARCH-INDEX` `SEARCH` index out of table range.
    EcRangeSearchIndex = 94,
    /// `EC-RANGE-SEARCH-NO-MATCH` `SEARCH` exhausted table without match.
    EcRangeSearchNoMatch = 95,
    /// `EC-REPORT` category.
    EcReport = 96,
    /// `EC-REPORT-ACTIVE` `INITIATE` on already-active report.
    EcReportActive = 97,
    /// `EC-REPORT-COLUMN-OVERLAP` overlapping report column positions.
    EcReportColumnOverlap = 98,
    /// `EC-REPORT-FILE-MODE` report file not open for output.
    EcReportFileMode = 99,
    /// `EC-REPORT-IMP` implementor report condition.
    EcReportImp = 100,
    /// `EC-REPORT-INACTIVE` `GENERATE`/`TERMINATE` on inactive report.
    EcReportInactive = 101,
    /// `EC-REPORT-LINE-OVERLAP` non-monotonic report line progression.
    EcReportLineOverlap = 102,
    /// `EC-REPORT-NOT-TERMINATED` file closed with unterminated report.
    EcReportNotTerminated = 103,
    /// `EC-REPORT-PAGE-LIMIT` report line exceeds `PAGE LIMIT`.
    EcReportPageLimit = 104,
    /// `EC-REPORT-PAGE-WIDTH` report column exceeds page width.
    EcReportPageWidth = 105,
    /// `EC-REPORT-SUM-SIZE` report `SUM` accumulator size overflow.
    EcReportSumSize = 106,
    /// `EC-REPORT-VARYING` report `VARYING` clause error.
    EcReportVarying = 107,
    /// `EC-SCREEN` category.
    EcScreen = 108,
    /// `EC-SCREEN-FIELD-OVERLAP` overlapping screen fields.
    EcScreenFieldOverlap = 109,
    /// `EC-SCREEN-IMP` implementor screen condition.
    EcScreenImp = 110,
    /// `EC-SCREEN-LINE-NUMBER` screen line outside terminal height.
    EcScreenLineNumber = 111,
    /// `EC-SCREEN-STARTING-COLUMN` screen column outside terminal width.
    EcScreenStartingColumn = 112,
    /// `EC-SIZE` category.
    EcSize = 113,
    /// `EC-SIZE-ADDRESS` pointer/address conversion size error.
    EcSizeAddress = 114,
    /// `EC-SIZE-EXPONENTIATION` `0 ** 0`, `0 ** neg`, or `neg ** frac` domain error.
    EcSizeExponentiation = 115,
    /// `EC-SIZE-IMP` implementor size condition.
    EcSizeImp = 116,
    /// `EC-SIZE-OVERFLOW` arithmetic high-order overflow.
    EcSizeOverflow = 117,
    /// `EC-SIZE-TRUNCATION` significant high-order or prohibited fractional digit truncation.
    EcSizeTruncation = 118,
    /// `EC-SIZE-UNDERFLOW` floating-point underflow.
    EcSizeUnderflow = 119,
    /// `EC-SIZE-ZERO-DIVIDE` division or remainder by zero.
    EcSizeZeroDivide = 120,
    /// `EC-SORT-MERGE` category.
    EcSortMerge = 121,
    /// `EC-SORT-MERGE-ACTIVE` nested `SORT`/`MERGE` while session active.
    EcSortMergeActive = 122,
    /// `EC-SORT-MERGE-FILE-OPEN` `USING`/`GIVING` file already open.
    EcSortMergeFileOpen = 123,
    /// `EC-SORT-MERGE-IMP` implementor sort/merge condition.
    EcSortMergeImp = 124,
    /// `EC-SORT-MERGE-RELEASE` invalid `RELEASE` record length.
    EcSortMergeRelease = 125,
    /// `EC-SORT-MERGE-RETURN` `RETURN` past end of sort/merge stream.
    EcSortMergeReturn = 126,
    /// `EC-SORT-MERGE-SEQUENCE` `MERGE` input file out of key sequence.
    EcSortMergeSequence = 127,
    /// `EC-STORAGE` category.
    EcStorage = 128,
    /// `EC-STORAGE-IMP` implementor storage condition.
    EcStorageImp = 129,
    /// `EC-STORAGE-NOT-ALLOC` `FREE` of pointer not allocated by `ALLOCATE`.
    EcStorageNotAlloc = 130,
    /// `EC-STORAGE-NOT-AVAIL` `ALLOCATE` heap exhaustion.
    EcStorageNotAvail = 131,
    /// `EC-USER` category.
    EcUser = 132,
    /// `EC-USER-EXCEPTION` user-defined exception leaf.
    EcUserException = 133,
    /// `EC-VALIDATE` category.
    EcValidate = 134,
    /// `EC-VALIDATE-CONTENT` `VALIDATE` content error.
    EcValidateContent = 135,
    /// `EC-VALIDATE-FORMAT` `VALIDATE` format error.
    EcValidateFormat = 136,
    /// `EC-VALIDATE-IMP` implementor validate condition.
    EcValidateImp = 137,
    /// `EC-VALIDATE-RELATION` `VALIDATE` relation error.
    EcValidateRelation = 138,
    /// `EC-VALIDATE-VARYING` `VALIDATE` varying clause error.
    EcValidateVarying = 139,
    /// `EC-CONTINUE` category (`ISO/IEC 1989:2023`).
    EcContinue = 140,
    /// `EC-CONTINUE-IMP` implementor `CONTINUE` condition (`ISO/IEC 1989:2023`).
    EcContinueImp = 141,
    /// `EC-CONTINUE-LESS-THAN-ZERO` negative `CONTINUE AFTER` duration (`ISO/IEC 1989:2023`).
    EcContinueLessThanZero = 142,
    /// `EC-FLOW-APPLY-COMMIT` error applying transaction `COMMIT` (`ISO/IEC 1989:2023`).
    EcFlowApplyCommit = 143,
    /// `EC-FLOW-COMMIT` general transaction `COMMIT` failure (`ISO/IEC 1989:2023`).
    EcFlowCommit = 144,
    /// `EC-FLOW-ROLLBACK` transaction `ROLLBACK` failure (`ISO/IEC 1989:2023`).
    EcFlowRollback = 145,
}

/// Static descriptor entry in [`EXCEPTION_CATALOG`] indexed by `ExceptionCode as usize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExceptionSpec {
    /// Strongly-typed [`ExceptionCode`] variant.
    pub code: ExceptionCode,
    /// Canonical uppercase ISO 2023 condition name (`<= 31` ASCII characters).
    pub name: &'static str,
    /// Owning [`ExceptionCategory`].
    pub category: ExceptionCategory,
    /// Tree level (`All`, `Category`, or `Leaf`).
    pub level: ExceptionLevel,
}

/// Root fallback descriptor (`EC-ALL`).
const ROOT_EXCEPTION_SPEC: ExceptionSpec = ExceptionSpec {
    code: ExceptionCode::EcAll,
    name: "EC-ALL",
    category: ExceptionCategory::All,
    level: ExceptionLevel::All,
};

/// Complete ordered catalog of all `146` ISO/IEC 1989:2023 `EC-*` selectors and leaf conditions.
pub const EXCEPTION_CATALOG: [ExceptionSpec; EXCEPTION_CATALOG_LEN] = {
    use ExceptionCategory as C;
    use ExceptionCode as E;
    use ExceptionLevel as L;
    [
        ExceptionSpec {
            code: E::EcAll,
            name: "EC-ALL",
            category: C::All,
            level: L::All,
        },
        ExceptionSpec {
            code: E::EcArgument,
            name: "EC-ARGUMENT",
            category: C::Argument,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcArgumentFunction,
            name: "EC-ARGUMENT-FUNCTION",
            category: C::Argument,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcArgumentImp,
            name: "EC-ARGUMENT-IMP",
            category: C::Argument,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcBound,
            name: "EC-BOUND",
            category: C::Bound,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcBoundImp,
            name: "EC-BOUND-IMP",
            category: C::Bound,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcBoundOdo,
            name: "EC-BOUND-ODO",
            category: C::Bound,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcBoundOverflow,
            name: "EC-BOUND-OVERFLOW",
            category: C::Bound,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcBoundPtr,
            name: "EC-BOUND-PTR",
            category: C::Bound,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcBoundRefMod,
            name: "EC-BOUND-REF-MOD",
            category: C::Bound,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcBoundSet,
            name: "EC-BOUND-SET",
            category: C::Bound,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcBoundSubscript,
            name: "EC-BOUND-SUBSCRIPT",
            category: C::Bound,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcBoundTableLimit,
            name: "EC-BOUND-TABLE-LIMIT",
            category: C::Bound,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcData,
            name: "EC-DATA",
            category: C::Data,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcDataConversion,
            name: "EC-DATA-CONVERSION",
            category: C::Data,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcDataImp,
            name: "EC-DATA-IMP",
            category: C::Data,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcDataIncompatible,
            name: "EC-DATA-INCOMPATIBLE",
            category: C::Data,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcDataInfinity,
            name: "EC-DATA-INFINITY",
            category: C::Data,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcDataNegativeInfinity,
            name: "EC-DATA-NEGATIVE-INFINITY",
            category: C::Data,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcDataNotANumber,
            name: "EC-DATA-NOT-A-NUMBER",
            category: C::Data,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcDataPtrNull,
            name: "EC-DATA-PTR-NULL",
            category: C::Data,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlow,
            name: "EC-FLOW",
            category: C::Flow,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcFlowGlobalExit,
            name: "EC-FLOW-GLOBAL-EXIT",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowGlobalGoback,
            name: "EC-FLOW-GLOBAL-GOBACK",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowImp,
            name: "EC-FLOW-IMP",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowRelease,
            name: "EC-FLOW-RELEASE",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowReport,
            name: "EC-FLOW-REPORT",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowReturn,
            name: "EC-FLOW-RETURN",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowSearch,
            name: "EC-FLOW-SEARCH",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowUse,
            name: "EC-FLOW-USE",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFunction,
            name: "EC-FUNCTION",
            category: C::Function,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcFunctionArgOmitted,
            name: "EC-FUNCTION-ARG-OMITTED",
            category: C::Function,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFunctionImp,
            name: "EC-FUNCTION-IMP",
            category: C::Function,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFunctionNotFound,
            name: "EC-FUNCTION-NOT-FOUND",
            category: C::Function,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFunctionPtrInvalid,
            name: "EC-FUNCTION-PTR-INVALID",
            category: C::Function,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFunctionPtrNull,
            name: "EC-FUNCTION-PTR-NULL",
            category: C::Function,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIo,
            name: "EC-I-O",
            category: C::Io,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcIoAtEnd,
            name: "EC-I-O-AT-END",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoEop,
            name: "EC-I-O-EOP",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoEopOverflow,
            name: "EC-I-O-EOP-OVERFLOW",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoFileSharing,
            name: "EC-I-O-FILE-SHARING",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoImp,
            name: "EC-I-O-IMP",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoInvalidKey,
            name: "EC-I-O-INVALID-KEY",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoLinage,
            name: "EC-I-O-LINAGE",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoLogicError,
            name: "EC-I-O-LOGIC-ERROR",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoPermanentError,
            name: "EC-I-O-PERMANENT-ERROR",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoRecordContent,
            name: "EC-I-O-RECORD-CONTENT",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoRecordOperation,
            name: "EC-I-O-RECORD-OPERATION",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcIoWarning,
            name: "EC-I-O-WARNING",
            category: C::Io,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcImp,
            name: "EC-IMP",
            category: C::Imp,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcImpAccept,
            name: "EC-IMP-ACCEPT",
            category: C::Imp,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcImpDisplay,
            name: "EC-IMP-DISPLAY",
            category: C::Imp,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcLocale,
            name: "EC-LOCALE",
            category: C::Locale,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcLocaleImp,
            name: "EC-LOCALE-IMP",
            category: C::Locale,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcLocaleIncompatible,
            name: "EC-LOCALE-INCOMPATIBLE",
            category: C::Locale,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcLocaleInvalid,
            name: "EC-LOCALE-INVALID",
            category: C::Locale,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcLocaleInvalidPtr,
            name: "EC-LOCALE-INVALID-PTR",
            category: C::Locale,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcLocaleMissing,
            name: "EC-LOCALE-MISSING",
            category: C::Locale,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcLocaleSize,
            name: "EC-LOCALE-SIZE",
            category: C::Locale,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOo,
            name: "EC-OO",
            category: C::Oo,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcOoArgOmitted,
            name: "EC-OO-ARG-OMITTED",
            category: C::Oo,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOoConformance,
            name: "EC-OO-CONFORMANCE",
            category: C::Oo,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOoException,
            name: "EC-OO-EXCEPTION",
            category: C::Oo,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOoImp,
            name: "EC-OO-IMP",
            category: C::Oo,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOoMethod,
            name: "EC-OO-METHOD",
            category: C::Oo,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOoNull,
            name: "EC-OO-NULL",
            category: C::Oo,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOoResource,
            name: "EC-OO-RESOURCE",
            category: C::Oo,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOoUniversal,
            name: "EC-OO-UNIVERSAL",
            category: C::Oo,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOrder,
            name: "EC-ORDER",
            category: C::Order,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcOrderImp,
            name: "EC-ORDER-IMP",
            category: C::Order,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOrderNotSupported,
            name: "EC-ORDER-NOT-SUPPORTED",
            category: C::Order,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOverflow,
            name: "EC-OVERFLOW",
            category: C::Overflow,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcOverflowImp,
            name: "EC-OVERFLOW-IMP",
            category: C::Overflow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOverflowString,
            name: "EC-OVERFLOW-STRING",
            category: C::Overflow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcOverflowUnstring,
            name: "EC-OVERFLOW-UNSTRING",
            category: C::Overflow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcProgram,
            name: "EC-PROGRAM",
            category: C::Program,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcProgramArgMismatch,
            name: "EC-PROGRAM-ARG-MISMATCH",
            category: C::Program,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcProgramArgOmitted,
            name: "EC-PROGRAM-ARG-OMITTED",
            category: C::Program,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcProgramCancelActive,
            name: "EC-PROGRAM-CANCEL-ACTIVE",
            category: C::Program,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcProgramImp,
            name: "EC-PROGRAM-IMP",
            category: C::Program,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcProgramNotFound,
            name: "EC-PROGRAM-NOT-FOUND",
            category: C::Program,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcProgramPtrNull,
            name: "EC-PROGRAM-PTR-NULL",
            category: C::Program,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcProgramRecursiveCall,
            name: "EC-PROGRAM-RECURSIVE-CALL",
            category: C::Program,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcProgramResources,
            name: "EC-PROGRAM-RESOURCES",
            category: C::Program,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRaising,
            name: "EC-RAISING",
            category: C::Raising,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcRaisingImp,
            name: "EC-RAISING-IMP",
            category: C::Raising,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRaisingNotSpecified,
            name: "EC-RAISING-NOT-SPECIFIED",
            category: C::Raising,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRange,
            name: "EC-RANGE",
            category: C::Range,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcRangeImp,
            name: "EC-RANGE-IMP",
            category: C::Range,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRangeIndex,
            name: "EC-RANGE-INDEX",
            category: C::Range,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRangeInspectSize,
            name: "EC-RANGE-INSPECT-SIZE",
            category: C::Range,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRangeInvalid,
            name: "EC-RANGE-INVALID",
            category: C::Range,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRangePerformVarying,
            name: "EC-RANGE-PERFORM-VARYING",
            category: C::Range,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRangePtr,
            name: "EC-RANGE-PTR",
            category: C::Range,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRangeSearchIndex,
            name: "EC-RANGE-SEARCH-INDEX",
            category: C::Range,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcRangeSearchNoMatch,
            name: "EC-RANGE-SEARCH-NO-MATCH",
            category: C::Range,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReport,
            name: "EC-REPORT",
            category: C::Report,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcReportActive,
            name: "EC-REPORT-ACTIVE",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportColumnOverlap,
            name: "EC-REPORT-COLUMN-OVERLAP",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportFileMode,
            name: "EC-REPORT-FILE-MODE",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportImp,
            name: "EC-REPORT-IMP",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportInactive,
            name: "EC-REPORT-INACTIVE",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportLineOverlap,
            name: "EC-REPORT-LINE-OVERLAP",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportNotTerminated,
            name: "EC-REPORT-NOT-TERMINATED",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportPageLimit,
            name: "EC-REPORT-PAGE-LIMIT",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportPageWidth,
            name: "EC-REPORT-PAGE-WIDTH",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportSumSize,
            name: "EC-REPORT-SUM-SIZE",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcReportVarying,
            name: "EC-REPORT-VARYING",
            category: C::Report,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcScreen,
            name: "EC-SCREEN",
            category: C::Screen,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcScreenFieldOverlap,
            name: "EC-SCREEN-FIELD-OVERLAP",
            category: C::Screen,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcScreenImp,
            name: "EC-SCREEN-IMP",
            category: C::Screen,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcScreenLineNumber,
            name: "EC-SCREEN-LINE-NUMBER",
            category: C::Screen,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcScreenStartingColumn,
            name: "EC-SCREEN-STARTING-COLUMN",
            category: C::Screen,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSize,
            name: "EC-SIZE",
            category: C::Size,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcSizeAddress,
            name: "EC-SIZE-ADDRESS",
            category: C::Size,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSizeExponentiation,
            name: "EC-SIZE-EXPONENTIATION",
            category: C::Size,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSizeImp,
            name: "EC-SIZE-IMP",
            category: C::Size,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSizeOverflow,
            name: "EC-SIZE-OVERFLOW",
            category: C::Size,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSizeTruncation,
            name: "EC-SIZE-TRUNCATION",
            category: C::Size,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSizeUnderflow,
            name: "EC-SIZE-UNDERFLOW",
            category: C::Size,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSizeZeroDivide,
            name: "EC-SIZE-ZERO-DIVIDE",
            category: C::Size,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSortMerge,
            name: "EC-SORT-MERGE",
            category: C::SortMerge,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcSortMergeActive,
            name: "EC-SORT-MERGE-ACTIVE",
            category: C::SortMerge,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSortMergeFileOpen,
            name: "EC-SORT-MERGE-FILE-OPEN",
            category: C::SortMerge,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSortMergeImp,
            name: "EC-SORT-MERGE-IMP",
            category: C::SortMerge,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSortMergeRelease,
            name: "EC-SORT-MERGE-RELEASE",
            category: C::SortMerge,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSortMergeReturn,
            name: "EC-SORT-MERGE-RETURN",
            category: C::SortMerge,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcSortMergeSequence,
            name: "EC-SORT-MERGE-SEQUENCE",
            category: C::SortMerge,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcStorage,
            name: "EC-STORAGE",
            category: C::Storage,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcStorageImp,
            name: "EC-STORAGE-IMP",
            category: C::Storage,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcStorageNotAlloc,
            name: "EC-STORAGE-NOT-ALLOC",
            category: C::Storage,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcStorageNotAvail,
            name: "EC-STORAGE-NOT-AVAIL",
            category: C::Storage,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcUser,
            name: "EC-USER",
            category: C::User,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcUserException,
            name: "EC-USER-EXCEPTION",
            category: C::User,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcValidate,
            name: "EC-VALIDATE",
            category: C::Validate,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcValidateContent,
            name: "EC-VALIDATE-CONTENT",
            category: C::Validate,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcValidateFormat,
            name: "EC-VALIDATE-FORMAT",
            category: C::Validate,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcValidateImp,
            name: "EC-VALIDATE-IMP",
            category: C::Validate,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcValidateRelation,
            name: "EC-VALIDATE-RELATION",
            category: C::Validate,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcValidateVarying,
            name: "EC-VALIDATE-VARYING",
            category: C::Validate,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcContinue,
            name: "EC-CONTINUE",
            category: C::Continue,
            level: L::Category,
        },
        ExceptionSpec {
            code: E::EcContinueImp,
            name: "EC-CONTINUE-IMP",
            category: C::Continue,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcContinueLessThanZero,
            name: "EC-CONTINUE-LESS-THAN-ZERO",
            category: C::Continue,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowApplyCommit,
            name: "EC-FLOW-APPLY-COMMIT",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowCommit,
            name: "EC-FLOW-COMMIT",
            category: C::Flow,
            level: L::Leaf,
        },
        ExceptionSpec {
            code: E::EcFlowRollback,
            name: "EC-FLOW-ROLLBACK",
            category: C::Flow,
            level: L::Leaf,
        },
    ]
};

impl ExceptionCode {
    /// Returns the complete [`EXCEPTION_CATALOG`] slice.
    #[must_use]
    pub const fn catalog() -> &'static [ExceptionSpec; EXCEPTION_CATALOG_LEN] {
        &EXCEPTION_CATALOG
    }

    /// Encodes this [`ExceptionCode`] as its stable `u16` C-ABI code (`0..=145`).
    #[must_use]
    pub const fn to_abi_id(self) -> u16 {
        self as u16
    }

    /// Decodes a `u16` C-ABI code into an [`ExceptionCode`].
    ///
    /// # Arguments
    ///
    /// * `raw` - Raw numeric C-ABI exception ID (`0..=145`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw >= 146`.
    pub fn from_abi_id(raw: u16) -> Result<Self> {
        EXCEPTION_CATALOG
            .get(usize::from(raw))
            .map(|spec| spec.code)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "ExceptionCode",
                value: raw.to_string(),
                reason: String::from("ABI exception ID out of range 0..=145"),
            })
    }

    /// Parses a case-insensitive COBOL `EC-*` name (e.g. `"ec-size-zero-divide"`).
    ///
    /// # Arguments
    ///
    /// * `raw_name` - Source or runtime `EC-*` identifier string.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw_name` is not a recognized ISO `EC-*` name.
    pub fn from_cobol_name(raw_name: &str) -> Result<Self> {
        let upper = raw_name.trim().to_ascii_uppercase();
        EXCEPTION_CATALOG
            .iter()
            .find(|spec| spec.name == upper)
            .map(|spec| spec.code)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "ExceptionCode",
                value: String::from(raw_name),
                reason: String::from("unrecognized ISO 2023 EC-* exception name"),
            })
    }

    /// Maps a decimal arithmetic [`SizeErrorStatus`] to its corresponding `EC-SIZE-*` leaf condition.
    ///
    /// # Arguments
    ///
    /// * `status` - Arithmetic [`SizeErrorStatus`] produced by `coblang-decimal`.
    /// * `overflow_cause` - Distinguishes high-order overflow from `ROUNDED MODE IS PROHIBITED` truncation.
    #[must_use]
    pub const fn from_size_error(
        status: SizeErrorStatus,
        overflow_cause: SizeOverflowCause,
    ) -> Option<Self> {
        match status {
            SizeErrorStatus::Exact => None,
            SizeErrorStatus::DivisionByZero => Some(Self::EcSizeZeroDivide),
            SizeErrorStatus::InvalidExponent => Some(Self::EcSizeExponentiation),
            SizeErrorStatus::Overflowed => match overflow_cause {
                SizeOverflowCause::HighOrderOverflow => Some(Self::EcSizeOverflow),
                SizeOverflowCause::ProhibitedTruncation => Some(Self::EcSizeTruncation),
            },
        }
    }

    /// Maps a 2-byte COBOL `FILE STATUS` code to its ISO/IEC 1989:2023 `EC-I-O-*` condition (`None` for `"00"`).
    ///
    /// # Arguments
    ///
    /// * `status` - Two-character ASCII `I-O` status bytes (e.g. `*b"00"`, `*b"10"`, `*b"35"`).
    #[must_use]
    pub const fn from_io_status_bytes(status: [u8; 2]) -> Option<Self> {
        match status {
            [b'0', b'0'] => None,
            [b'0', _] => Some(Self::EcIoWarning),
            [b'1', _] => Some(Self::EcIoAtEnd),
            [b'2', _] => Some(Self::EcIoInvalidKey),
            [b'3', _] => Some(Self::EcIoPermanentError),
            [b'4', _] => Some(Self::EcIoLogicError),
            [b'5', b'7'] => Some(Self::EcIoLinage),
            [b'5' | b'6', _] => Some(Self::EcIoFileSharing),
            [b'7', _] => Some(Self::EcIoRecordContent),
            _ => Some(Self::EcIoImp),
        }
    }

    /// Returns the [`ExceptionSpec`] metadata for `self`.
    #[must_use]
    pub fn spec(self) -> ExceptionSpec {
        EXCEPTION_CATALOG
            .get(usize::from(self.to_abi_id()))
            .copied()
            .unwrap_or(ROOT_EXCEPTION_SPEC)
    }

    /// Returns the canonical uppercase COBOL `EC-*` name for `self`.
    #[must_use]
    pub fn cobol_name(self) -> &'static str {
        self.spec().name
    }

    /// Returns the [`ExceptionCategory`] of `self`.
    #[must_use]
    pub fn category(self) -> ExceptionCategory {
        self.spec().category
    }

    /// Returns the [`ExceptionLevel`] (`All`, `Category`, or `Leaf`) of `self`.
    #[must_use]
    pub fn level(self) -> ExceptionLevel {
        self.spec().level
    }

    /// Returns the standard ISO/IEC 1989:2023 [`ExceptionSeverity`] for `self`.
    #[must_use]
    pub fn default_severity(self) -> ExceptionSeverity {
        match self.level() {
            ExceptionLevel::All | ExceptionLevel::Category => ExceptionSeverity::Selector,
            ExceptionLevel::Leaf => match self {
                Self::EcIoAtEnd
                | Self::EcIoEop
                | Self::EcIoEopOverflow
                | Self::EcIoInvalidKey
                | Self::EcIoWarning
                | Self::EcImpAccept
                | Self::EcImpDisplay
                | Self::EcOverflowImp
                | Self::EcOverflowString
                | Self::EcOverflowUnstring
                | Self::EcRangeImp
                | Self::EcRangeSearchIndex
                | Self::EcRangeSearchNoMatch
                | Self::EcScreenFieldOverlap
                | Self::EcScreenImp
                | Self::EcScreenLineNumber
                | Self::EcScreenStartingColumn
                | Self::EcStorageImp
                | Self::EcStorageNotAlloc
                | Self::EcUserException
                | Self::EcValidateContent
                | Self::EcValidateFormat
                | Self::EcValidateImp
                | Self::EcValidateRelation => ExceptionSeverity::NonFatal,
                _ => ExceptionSeverity::Fatal,
            },
        }
    }

    /// Formats `self` into the 31-byte space-padded ISO `FUNCTION EXCEPTION-STATUS` buffer.
    #[must_use]
    pub fn status_buffer_31(self) -> [u8; 31] {
        pad_ascii_31(self.cobol_name().as_bytes())
    }

    /// Returns `true` if `self` matches the `>>TURN` or `USE AFTER EXCEPTION` `selector`.
    ///
    /// # Arguments
    ///
    /// * `selector` - `EC-ALL`, category (`EC-SIZE`), or exact leaf (`EC-SIZE-ZERO-DIVIDE`) selector.
    #[must_use]
    pub fn matches_selector(self, selector: Self) -> bool {
        match selector.level() {
            ExceptionLevel::All => true,
            ExceptionLevel::Category => self.category() == selector.category(),
            ExceptionLevel::Leaf => self == selector,
        }
    }
}

/// Formats `bytes` into a fixed 31-byte right-space-padded ASCII buffer.
///
/// # Arguments
///
/// * `bytes` - Source ASCII identifier bytes.
#[must_use]
fn pad_ascii_31(bytes: &[u8]) -> [u8; 31] {
    let mut out = [b' '; 31];
    for (dst, &src) in out.iter_mut().zip(bytes.iter()) {
        *dst = src;
    }
    out
}

/// Checking toggle mode controlled by `>>TURN ec-name CHECKING {ON [[WITH] LOCATION] | OFF}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnCheckingMode {
    /// Disable runtime checking and location tracking for matching `EC-*` conditions (`0`).
    Disabled,
    /// Enable runtime checking without source location metadata (`1`).
    Enabled,
    /// Enable runtime checking and populate `FUNCTION EXCEPTION-LOCATION` (`2`).
    EnabledWithLocation,
}

impl TurnCheckingMode {
    /// Decodes a C-ABI integer mode (`0` = `Disabled`, `1` = `Enabled`, `2` = `EnabledWithLocation`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw ABI mode integer.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        match code {
            1 => Self::Enabled,
            2 => Self::EnabledWithLocation,
            _ => Self::Disabled,
        }
    }

    /// Encodes `self` into its C-ABI integer representation (`0..=2`).
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::Disabled => 0,
            Self::Enabled => 1,
            Self::EnabledWithLocation => 2,
        }
    }
}

/// Re-entrancy guard state for `DECLARATIVES` `USE AFTER EXCEPTION` procedures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeclarativeGuardState {
    /// Normal procedure execution (declarative dispatch permitted).
    Idle,
    /// Currently executing inside a declarative section (nested `USE` triggers `EC-FLOW-USE`).
    InDeclarative,
}

/// Continuation target recorded by a `RESUME` statement inside an active `USE` declarative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResumeTarget {
    /// No `RESUME` executed inside the active declarative.
    Unset,
    /// `RESUME [AT NEXT STATEMENT]` — resume immediately after the raising statement.
    NextStatement,
    /// `RESUME AT <procedure-name>` — branch to target paragraph/section index.
    ProcedureId(u32),
}

/// Snapshot of the most recently raised exception for `FUNCTION EXCEPTION-*` intrinsics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionRecord {
    /// Raised [`ExceptionCode`].
    pub code: ExceptionCode,
    /// Statement verb name (e.g. `"DIVIDE"`, `"READ"`, `"ALLOCATE"`).
    pub statement: String,
    /// `PROGRAM-ID; paragraph/section; line` location string for `FUNCTION EXCEPTION-LOCATION`.
    pub location: String,
    /// Associated file connector name (`""` for non-I/O exceptions) for `FUNCTION EXCEPTION-FILE`.
    pub file_name: String,
}

/// Per-run-unit ISO 2023 exception mask, `>>PUSH`/`>>POP TURN` stack, last-exception snapshot,
/// `RESUME` target, and declarative re-entrancy guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionRuntimeState {
    /// 192-bit `[u64; 3]` bitmap indexed by `ExceptionCode::to_abi_id()` (`1` = checking enabled).
    enabled_bits: [u64; 3],
    /// 192-bit `[u64; 3]` bitmap indexed by `ExceptionCode::to_abi_id()` (`1` = `WITH LOCATION` enabled).
    location_bits: [u64; 3],
    /// Saved `(enabled_bits, location_bits)` snapshots pushed via `>>PUSH TURN` or call frames.
    turn_stack: Vec<([u64; 3], [u64; 3])>,
    /// Most recently raised exception record (`None` when clear / `SET LAST EXCEPTION TO OFF`).
    last_record: Option<ExceptionRecord>,
    /// Two-character ASCII `FILE STATUS` associated with the most recent I/O exception (`*b"00"` otherwise).
    last_io_status: [u8; 2],
    /// Re-entrancy state preventing recursive `USE` declarative loops.
    declarative_guard: DeclarativeGuardState,
    /// Pending continuation target chosen by `RESUME` inside the active declarative.
    resume_target: ResumeTarget,
}

impl Default for ExceptionRuntimeState {
    /// Creates a clean [`ExceptionRuntimeState`] with checking initially disabled and no active exception.
    fn default() -> Self {
        Self::new()
    }
}

impl ExceptionRuntimeState {
    /// Creates a clean [`ExceptionRuntimeState`] with all `>>TURN` bits cleared and no active exception.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            enabled_bits: [0; 3],
            location_bits: [0; 3],
            turn_stack: Vec::new(),
            last_record: None,
            last_io_status: *b"00",
            declarative_guard: DeclarativeGuardState::Idle,
            resume_target: ResumeTarget::Unset,
        }
    }

    /// Resets `>>TURN` masks, `>>PUSH` stack, `LAST EXCEPTION`, I/O status, `RESUME`, and declarative guard.
    pub fn reset(&mut self) {
        self.enabled_bits = [0; 3];
        self.location_bits = [0; 3];
        self.turn_stack.clear();
        self.last_record = None;
        self.last_io_status = *b"00";
        self.declarative_guard = DeclarativeGuardState::Idle;
        self.resume_target = ResumeTarget::Unset;
    }

    /// Clears `LAST EXCEPTION` and resets `last_io_status` to `"00"` (`SET LAST EXCEPTION TO OFF`).
    pub fn clear_last_exception(&mut self) {
        self.last_record = None;
        self.last_io_status = *b"00";
    }

    /// Pushes the active `(enabled_bits, location_bits)` `>>TURN` mask snapshot onto `turn_stack`.
    pub fn push_turn_state(&mut self) {
        self.turn_stack
            .push((self.enabled_bits, self.location_bits));
    }

    /// Pops and restores the most recently saved `>>TURN` mask snapshot from `turn_stack`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `turn_stack` is empty.
    pub fn pop_turn_state(&mut self) -> Result<()> {
        let (enabled, location) =
            self.turn_stack
                .pop()
                .ok_or_else(|| CoblangError::InvalidDomainValue {
                    domain_type: "ExceptionRuntimeState",
                    value: String::from("empty turn_stack"),
                    reason: String::from("unbalanced >>POP TURN on empty turn stack"),
                })?;
        self.enabled_bits = enabled;
        self.location_bits = location;
        Ok(())
    }

    /// Applies `>>TURN selector CHECKING {ON [[WITH] LOCATION] | OFF}` across all matching [`EXCEPTION_CATALOG`] entries.
    ///
    /// # Arguments
    ///
    /// * `selector` - `EC-ALL`, category (`EC-BOUND`), or leaf (`EC-BOUND-SUBSCRIPT`).
    /// * `mode` - [`TurnCheckingMode`] (`Disabled`, `Enabled`, or `EnabledWithLocation`).
    pub fn apply_turn(&mut self, selector: ExceptionCode, mode: TurnCheckingMode) {
        for spec in &EXCEPTION_CATALOG {
            if spec.code.matches_selector(selector) {
                let id = usize::from(spec.code.to_abi_id());
                let word_idx = id / 64;
                let bit_mask = 1_u64 << (id % 64);
                let _ = self.enabled_bits.get_mut(word_idx).map(|word| match mode {
                    TurnCheckingMode::Enabled | TurnCheckingMode::EnabledWithLocation => {
                        *word |= bit_mask;
                    }
                    TurnCheckingMode::Disabled => *word &= !bit_mask,
                });
                let _ = self.location_bits.get_mut(word_idx).map(|word| match mode {
                    TurnCheckingMode::EnabledWithLocation => *word |= bit_mask,
                    TurnCheckingMode::Enabled | TurnCheckingMode::Disabled => *word &= !bit_mask,
                });
            }
        }
    }

    /// Returns whether runtime checking is currently enabled for `code`.
    ///
    /// # Arguments
    ///
    /// * `code` - [`ExceptionCode`] condition to query.
    #[must_use]
    pub fn is_enabled(&self, code: ExceptionCode) -> bool {
        let id = usize::from(code.to_abi_id());
        let word_idx = id / 64;
        let bit_mask = 1_u64 << (id % 64);
        self.enabled_bits
            .get(word_idx)
            .is_some_and(|w| (*w & bit_mask) != 0)
    }

    /// Returns whether `WITH LOCATION` provenance capture is currently enabled for `code`.
    ///
    /// # Arguments
    ///
    /// * `code` - [`ExceptionCode`] condition to query.
    #[must_use]
    pub fn is_location_enabled(&self, code: ExceptionCode) -> bool {
        let id = usize::from(code.to_abi_id());
        let word_idx = id / 64;
        let bit_mask = 1_u64 << (id % 64);
        self.location_bits
            .get(word_idx)
            .is_some_and(|w| (*w & bit_mask) != 0)
    }

    /// Records a raised [`ExceptionRecord`] into `LAST EXCEPTION` verbatim.
    ///
    /// # Arguments
    ///
    /// * `record` - Structured [`ExceptionRecord`] snapshot.
    pub fn raise(&mut self, record: ExceptionRecord) {
        self.last_io_status = *b"00";
        self.last_record = Some(record);
    }

    /// Records `record` while eliding `record.location` unless [`Self::is_location_enabled`] holds for `record.code`.
    ///
    /// # Arguments
    ///
    /// * `record` - Candidate [`ExceptionRecord`] snapshot.
    pub fn raise_respecting_turn(&mut self, mut record: ExceptionRecord) {
        if !self.is_location_enabled(record.code) {
            record.location.clear();
        }
        self.last_io_status = *b"00";
        self.last_record = Some(record);
    }

    /// Records an `EC-I-O-*` exception together with its 2-character ASCII `FILE STATUS` code.
    ///
    /// # Arguments
    ///
    /// * `io_status` - Two-byte ASCII `FILE STATUS` (e.g. `*b"35"`, `*b"10"`, `*b"23"`).
    /// * `record` - Structured [`ExceptionRecord`] snapshot containing `file_name` and `statement`.
    pub fn raise_io(&mut self, io_status: [u8; 2], mut record: ExceptionRecord) {
        if !self.is_location_enabled(record.code) {
            record.location.clear();
        }
        self.last_io_status = io_status;
        self.last_record = Some(record);
    }

    /// Records an `EC-I-O-*` exception together with its 2-character ASCII `FILE STATUS` code,
    /// preserving `record.location` unconditionally for `FUNCTION EXCEPTION-LOCATION`.
    ///
    /// # Arguments
    ///
    /// * `io_status` - Two-byte ASCII `FILE STATUS` (e.g. `*b"35"`, `*b"10"`, `*b"23"`).
    /// * `record` - Structured [`ExceptionRecord`] snapshot containing `file_name`, `statement`, and `location`.
    pub fn raise_io_verbatim(&mut self, io_status: [u8; 2], record: ExceptionRecord) {
        self.last_io_status = io_status;
        self.last_record = Some(record);
    }

    /// Records `code` with minimal metadata when raised from compact runtime helpers.
    ///
    /// # Arguments
    ///
    /// * `code` - Raised [`ExceptionCode`].
    pub fn raise_code(&mut self, code: ExceptionCode) {
        self.last_io_status = *b"00";
        self.last_record = Some(ExceptionRecord {
            code,
            statement: String::new(),
            location: String::new(),
            file_name: String::new(),
        });
    }

    /// Returns a reference to the active [`ExceptionRecord`], if any.
    #[must_use]
    pub const fn last_record(&self) -> Option<&ExceptionRecord> {
        self.last_record.as_ref()
    }

    /// Returns the 2-byte ASCII `FILE STATUS` recorded for the active I/O exception (`*b"00"` otherwise).
    #[must_use]
    pub const fn last_io_status(&self) -> [u8; 2] {
        self.last_io_status
    }

    /// Formats `FUNCTION EXCEPTION-STATUS` (31 ASCII bytes; all spaces when no exception is active).
    #[must_use]
    pub fn exception_status_31(&self) -> [u8; 31] {
        self.last_record
            .as_ref()
            .map_or([b' '; 31], |rec| rec.code.status_buffer_31())
    }

    /// Formats `FUNCTION EXCEPTION-STATEMENT` (31 uppercase space-padded ASCII bytes).
    #[must_use]
    pub fn exception_statement_31(&self) -> [u8; 31] {
        self.last_record.as_ref().map_or([b' '; 31], |rec| {
            pad_ascii_31(rec.statement.to_ascii_uppercase().as_bytes())
        })
    }

    /// Returns raw `location` string (`""` when no exception or location is active).
    #[must_use]
    pub fn exception_location(&self) -> &str {
        self.last_record
            .as_ref()
            .map_or("", |rec| rec.location.as_str())
    }

    /// Returns ISO/IEC 1989:2023 `FUNCTION EXCEPTION-LOCATION` (`" "` single space when unset).
    #[must_use]
    pub fn format_exception_location_iso(&self) -> &str {
        let loc = self.exception_location();
        if loc.is_empty() { " " } else { loc }
    }

    /// Returns raw `file_name` string (`""` when no file exception is active).
    #[must_use]
    pub fn exception_file(&self) -> &str {
        self.last_record
            .as_ref()
            .map_or("", |rec| rec.file_name.as_str())
    }

    /// Formats ISO/IEC 1989:2023 `FUNCTION EXCEPTION-FILE` (`"00"` when no I/O exception is active,
    /// else `<2-char status><file-name>`).
    #[must_use]
    pub fn format_exception_file_iso(&self) -> String {
        if self.last_io_status == *b"00" {
            return String::from("00");
        }
        let status_str = String::from_utf8_lossy(&self.last_io_status);
        let file = self.exception_file();
        let mut out = String::with_capacity(2_usize.saturating_add(file.len()));
        out.push_str(&status_str);
        out.push_str(file);
        out
    }

    /// Enters a `DECLARATIVES` `USE` handler, raising `EC-FLOW-USE` if already inside a declarative.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] after recording [`ExceptionCode::EcFlowUse`] on nested re-entry.
    pub fn enter_declarative(&mut self) -> Result<()> {
        match self.declarative_guard {
            DeclarativeGuardState::Idle => {
                self.declarative_guard = DeclarativeGuardState::InDeclarative;
                self.resume_target = ResumeTarget::Unset;
                Ok(())
            }
            DeclarativeGuardState::InDeclarative => {
                self.raise_code(ExceptionCode::EcFlowUse);
                Err(CoblangError::InvalidDomainValue {
                    domain_type: "DeclarativeGuardState",
                    value: String::from("InDeclarative"),
                    reason: String::from("recursive USE declarative invocation (EC-FLOW-USE)"),
                })
            }
        }
    }

    /// Records the [`ResumeTarget`] selected by a `RESUME` statement inside an active declarative.
    ///
    /// # Arguments
    ///
    /// * `target` - Chosen [`ResumeTarget`] (`NextStatement` or `ProcedureId`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] after raising [`ExceptionCode::EcFlowUse`] when not inside a declarative.
    pub fn set_resume_target(&mut self, target: ResumeTarget) -> Result<()> {
        match self.declarative_guard {
            DeclarativeGuardState::InDeclarative => {
                self.resume_target = target;
                Ok(())
            }
            DeclarativeGuardState::Idle => {
                self.raise_code(ExceptionCode::EcFlowUse);
                Err(CoblangError::InvalidDomainValue {
                    domain_type: "ResumeTarget",
                    value: String::from("Idle"),
                    reason: String::from("RESUME executed outside active USE declarative"),
                })
            }
        }
    }

    /// Returns the [`ResumeTarget`] most recently selected in the active or just-completed declarative.
    #[must_use]
    pub const fn resume_target(&self) -> ResumeTarget {
        self.resume_target
    }

    /// Marks completion of the active `DECLARATIVES` `USE` handler.
    pub fn leave_declarative(&mut self) {
        self.declarative_guard = DeclarativeGuardState::Idle;
    }

    /// Returns the current [`DeclarativeGuardState`].
    #[must_use]
    pub const fn declarative_guard(&self) -> DeclarativeGuardState {
        self.declarative_guard
    }
}

/// Compile-time `SOURCE-COMPUTER ... WITH DEBUGGING MODE` program attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgramDebugMode {
    /// Compiled with `WITH DEBUGGING MODE` (`USE FOR DEBUGGING` sections compiled in).
    WithDebuggingMode,
    /// Compiled without `WITH DEBUGGING MODE`.
    WithoutDebuggingMode,
}

/// Runtime activation override for `USE FOR DEBUGGING` declaratives (`COB_SET_DEBUG`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DebugRuntimeSwitch {
    /// Follow the program's compile-time [`ProgramDebugMode`] (`COB_SET_DEBUG` unset).
    DefaultFromProgram,
    /// Explicitly enable runtime `USE FOR DEBUGGING` execution (`COB_SET_DEBUG=Y|1|ON|YES|TRUE`).
    ForceEnabled,
    /// Explicitly suppress runtime `USE FOR DEBUGGING` execution (`COB_SET_DEBUG=N|0|OFF|NO|FALSE`).
    ForceDisabled,
}

/// Parses a `COB_SET_DEBUG` environment string into [`DebugRuntimeSwitch`].
///
/// # Arguments
///
/// * `raw` - Raw environment variable string value.
#[must_use]
pub fn parse_cob_set_debug(raw: &str) -> DebugRuntimeSwitch {
    match raw.trim().to_ascii_uppercase().as_str() {
        "1" | "Y" | "YES" | "ON" | "TRUE" => DebugRuntimeSwitch::ForceEnabled,
        "0" | "N" | "NO" | "OFF" | "FALSE" => DebugRuntimeSwitch::ForceDisabled,
        _ => DebugRuntimeSwitch::DefaultFromProgram,
    }
}

/// NIST CCVS-85 `DEBUG-ITEM` special register state (`ANSI X3.23-1985` Debug Module).
///
/// Fixed layout prefix (`56` bytes before variable `DEBUG-CONTENTS`):
/// - `DEBUG-LINE` (`PIC X(6)`)
/// - `FILLER` (`PIC X VALUE SPACE`)
/// - `DEBUG-NAME` (`PIC X(30)`)
/// - `FILLER` (`PIC X VALUE SPACE`)
/// - `DEBUG-SUB-1` (`PIC S9999 SIGN IS LEADING SEPARATE` — 5 bytes)
/// - `FILLER` (`PIC X VALUE SPACE`)
/// - `DEBUG-SUB-2` (`PIC S9999 SIGN IS LEADING SEPARATE` — 5 bytes)
/// - `FILLER` (`PIC X VALUE SPACE`)
/// - `DEBUG-SUB-3` (`PIC S9999 SIGN IS LEADING SEPARATE` — 5 bytes)
/// - `FILLER` (`PIC X VALUE SPACE`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugItemState {
    /// `DEBUG-LINE` (`PIC X(6)`).
    pub debug_line: [u8; 6],
    /// `DEBUG-NAME` (`PIC X(30)`).
    pub debug_name: [u8; 30],
    /// Up to three signed 4-digit subscripts (`DEBUG-SUB-1..3`, `None` -> spaces).
    pub debug_subs: [Option<i16>; 3],
    /// Variable-length `DEBUG-CONTENTS` payload bytes.
    pub debug_contents: Vec<u8>,
    /// Runtime activation switch controlled by `COB_SET_DEBUG`.
    runtime_switch: DebugRuntimeSwitch,
}

impl Default for DebugItemState {
    /// Creates a space-initialized [`DebugItemState`].
    fn default() -> Self {
        Self::new()
    }
}

impl DebugItemState {
    /// Creates a space-initialized [`DebugItemState`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            debug_line: [b' '; 6],
            debug_name: [b' '; 30],
            debug_subs: [None; 3],
            debug_contents: Vec::new(),
            runtime_switch: DebugRuntimeSwitch::DefaultFromProgram,
        }
    }

    /// Resets all `DEBUG-ITEM` subfields to spaces and restores [`DebugRuntimeSwitch::DefaultFromProgram`].
    pub fn clear(&mut self) {
        self.debug_line = [b' '; 6];
        self.debug_name = [b' '; 30];
        self.debug_subs = [None; 3];
        self.debug_contents.clear();
        self.runtime_switch = DebugRuntimeSwitch::DefaultFromProgram;
    }

    /// Updates the runtime debugging switch (`COB_SET_DEBUG`).
    ///
    /// # Arguments
    ///
    /// * `switch` - Desired [`DebugRuntimeSwitch`] policy.
    pub fn set_runtime_switch(&mut self, switch: DebugRuntimeSwitch) {
        self.runtime_switch = switch;
    }

    /// Returns the active [`DebugRuntimeSwitch`] policy.
    #[must_use]
    pub const fn runtime_switch(&self) -> DebugRuntimeSwitch {
        self.runtime_switch
    }

    /// Returns whether `USE FOR DEBUGGING` declaratives should execute for a program with `program_mode`.
    ///
    /// # Arguments
    ///
    /// * `program_mode` - Compile-time [`ProgramDebugMode`] of the executing program.
    #[must_use]
    pub const fn is_active_for_program(&self, program_mode: ProgramDebugMode) -> bool {
        match self.runtime_switch {
            DebugRuntimeSwitch::ForceEnabled => true,
            DebugRuntimeSwitch::ForceDisabled => false,
            DebugRuntimeSwitch::DefaultFromProgram => {
                matches!(program_mode, ProgramDebugMode::WithDebuggingMode)
            }
        }
    }

    /// Populates `DEBUG-ITEM` prior to invoking a `USE FOR DEBUGGING` declarative section.
    ///
    /// # Arguments
    ///
    /// * `line_str` - Source line number string (`DEBUG-LINE`).
    /// * `name` - Referenced identifier, file, or procedure name (`DEBUG-NAME`).
    /// * `subs` - Optional subscript values (`DEBUG-SUB-1..3`).
    /// * `contents` - `DEBUG-CONTENTS` payload (e.g. `"START PROGRAM"`, `"FALL THROUGH"`, or field bytes).
    pub fn populate(
        &mut self,
        line_str: &str,
        name: &str,
        subs: [Option<i16>; 3],
        contents: &[u8],
    ) {
        self.debug_line = [b' '; 6];
        for (d, &s) in self.debug_line.iter_mut().zip(line_str.as_bytes().iter()) {
            *d = s;
        }
        self.debug_name = [b' '; 30];
        for (d, &s) in self.debug_name.iter_mut().zip(name.as_bytes().iter()) {
            *d = s;
        }
        self.debug_subs = subs;
        self.debug_contents.clear();
        self.debug_contents.extend_from_slice(contents);
    }

    /// Writes the formatted `DEBUG-ITEM` record directly into `dst` (zero heap allocation),
    /// space-padding any trailing bytes beyond `56 + debug_contents.len()`.
    ///
    /// # Arguments
    ///
    /// * `dst` - Destination `DEBUG-ITEM` record slice (typically `>= 57` bytes).
    pub fn write_into_buffer(&self, dst: &mut [u8]) {
        dst.fill(b' ');
        let mut header = [b' '; DEBUG_ITEM_HEADER_LEN];
        for (d, &s) in header.iter_mut().zip(self.debug_line.iter()) {
            *d = s;
        }
        for (d, &s) in header.iter_mut().skip(7).zip(self.debug_name.iter()) {
            *d = s;
        }
        for (idx, sub_opt) in self.debug_subs.into_iter().enumerate() {
            let formatted = format_debug_sub(sub_opt);
            let offset = 38_usize.saturating_add(idx.saturating_mul(6));
            for (d, &s) in header.iter_mut().skip(offset).zip(formatted.iter()) {
                *d = s;
            }
        }
        for (d, &s) in dst.iter_mut().zip(header.iter()) {
            *d = s;
        }
        for (d, &s) in dst
            .iter_mut()
            .skip(DEBUG_ITEM_HEADER_LEN)
            .zip(self.debug_contents.iter())
        {
            *d = s;
        }
    }

    /// Materializes `DEBUG-ITEM` into a flat record of `56 + content_width` bytes.
    ///
    /// # Arguments
    ///
    /// * `content_width` - Declared byte width of `DEBUG-CONTENTS` (`>= 1` in `DEBUG-ITEM`).
    #[must_use]
    pub fn to_record_bytes(&self, content_width: usize) -> Vec<u8> {
        let mut buf = vec![b' '; DEBUG_ITEM_HEADER_LEN.saturating_add(content_width)];
        self.write_into_buffer(&mut buf);
        buf
    }
}

/// Formats one `DEBUG-SUB-n` (`PIC S9999 SIGN IS LEADING SEPARATE`, 5 bytes or spaces when `None`).
///
/// # Arguments
///
/// * `sub_opt` - Optional signed subscript (`-9999..=9999`).
#[must_use]
fn format_debug_sub(sub_opt: Option<i16>) -> [u8; 5] {
    let Some(val) = sub_opt else {
        return [b' '; 5];
    };
    let sign = if val < 0 { b'-' } else { b'+' };
    let mag = val.unsigned_abs().min(9999);
    [
        sign,
        b'0' + u8::try_from((mag / 1000) % 10).unwrap_or(0),
        b'0' + u8::try_from((mag / 100) % 10).unwrap_or(0),
        b'0' + u8::try_from((mag / 10) % 10).unwrap_or(0),
        b'0' + u8::try_from(mag % 10).unwrap_or(0),
    ]
}
