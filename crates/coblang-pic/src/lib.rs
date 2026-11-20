//! Pure-Rust `PICTURE` numeric/alphanumeric editing, de-editing, `MOVE`, and `INITIALIZE` engine.

/// Numeric-edited and alphanumeric string de-editing into exact `FixedDecimal`.
pub mod de_edit;
/// Strongly-typed `PictureProgram` bytecode VM (`DecimalPointKind`, `CurrencySignSpec`, multi-byte `CURRENCY SIGN`).
pub mod edit;
/// Alphanumeric, alphabetic, alphanumeric-edited, and UTF-16LE national formatting.
pub mod edit_alpha;
/// Full `PicEditOp` numeric-edited bytecode formatter (`Z`, `*`, `$$`, `++`, `--`, `CR`, `DB`, `,`, `.`).
pub mod edit_numeric;
/// Recursive `INITIALIZE` engine (`REPLACING`, `TO VALUE`, `WITH FILLER`, `REDEFINES` exclusion).
pub mod initialize;
/// ISO COBOL `INSPECT` (`TALLYING`, `REPLACING`, `CONVERTING`) and `TRANSFORM` engine.
pub mod inspect;
/// Category-by-category ISO COBOL `MOVE` engine with overlap-safe buffer execution and `ALL` tiling.
pub mod move_engine;
/// Strongly-typed `(SourceCategory, DestCategory)` `MOVE` matrix, `Justification`, and M17 transcoding helpers.
pub mod move_matrix;
/// ISO COBOL `STRING` and `UNSTRING` execution engines.
pub mod string_verbs;

pub use de_edit::{de_edit_to_decimal, de_edit_to_decimal_with_currency};
pub use edit::{
    CurrencySignSpec, DecimalPointKind, NumericPictureMeta, PictureProgram, PictureProgramKind,
};
pub use edit_alpha::{
    ascii_to_national_utf16le, format_alphanumeric, format_alphanumeric_edited, format_national,
    format_national_edited,
};
pub use edit_numeric::{NumericEditConfig, format_numeric_edited};
pub use initialize::{
    DefaultInitMode, FillerPolicy, InitCategoryMask, InitTargetField, InitializePlan,
    InitializeSpec, ReplacingRule, ValueMode, WithFiller, execute_initialize,
    execute_initialize_plan,
};
pub use inspect::{
    InspectConvertSpec, InspectMatchKind, InspectOperand, InspectReplaceClause, InspectTallyClause,
    InspectWindowSpec, execute_inspect_converting, execute_inspect_replacing,
    execute_inspect_tallying, execute_inspect_tallying_and_replacing, execute_transform,
};
pub use move_engine::{
    MoveFieldDesc, MoveSource, check_field_is_numeric, decode_numeric_field,
    encode_zoned_negative_zero, execute_move, execute_move_overlapping, format_field_for_display,
    is_zoned_negative_zero, store_decimal_to_field,
};
pub use move_matrix::{
    ALL_DEST_CATEGORIES, ALL_SOURCE_CATEGORIES, DestCategory, Justification, MoveConversionKind,
    SourceCategory, classify_move, encoding_space_byte, pad_or_truncate_national_utf16be,
    pad_or_truncate_national_utf16le, pad_or_truncate_sbcs, pad_or_truncate_utf8,
    pad_or_truncate_utf8_bytes_lossless, transcode_national_utf16le_to_sbcs,
    transcode_to_national_utf16le, transcode_to_utf8,
};
pub use string_verbs::{
    StringDelimiter, StringFlowOutcome, StringSourcePiece, UnstringDelimSpec, UnstringIntoSlot,
    UnstringMatchMode, execute_string, execute_unstring, normalize_display_operand_bytes,
};
