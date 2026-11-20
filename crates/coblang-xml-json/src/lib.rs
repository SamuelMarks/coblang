//! Pure-Rust streaming `XML PARSE` / `XML GENERATE` and `JSON PARSE` / `JSON GENERATE` engines (`Milestone M20`).

/// COBOL runtime dialect adapters for `XML GENERATE`, `JSON GENERATE`, and streaming push-mode `XML PARSE`.
pub mod cobol_runtime;
/// Pure-Rust RFC 8259 `JSON GENERATE` engine (`COUNT IN`, `NAME OF`, `OMITTED`, `SUPPRESS`, `CONVERTING`).
pub mod json_gen;
/// Pure-Rust RFC 8259 `JSON PARSE` engine (`WITH DETAIL`, `NAME OF`, `SUPPRESS`, `CONVERTING`, `JSON-CODE`, `JSON-STATUS`).
pub mod json_parse;
/// Strongly-typed domain identifiers, special-register codes, suppression rules, boolean mappings, and data trees.
pub mod types;
/// Pure-Rust `XML GENERATE` engine (`WITH ATTRIBUTES`, `WITH XML-DECLARATION`, `NAMESPACE`, `NAME OF`, `SUPPRESS`).
pub mod xml_gen;
/// Pure-Rust streaming `XML PARSE` engine (`XML-EVENT`, `XML-CODE`, `XML-TEXT`, `XML-NTEXT`, `XML-NAMESPACE`).
pub mod xml_parse;

pub use json_gen::{
    JsonGenerateOptions, JsonGenerateOutcome, escape_json_string_into, json_generate,
    json_generate_into_buffer,
};
pub use json_parse::{
    JsonDetailMode, JsonParseCompletion, JsonParseOptions, JsonParseOutcome, JsonValue, json_parse,
    parse_json_document,
};
pub use types::{
    BufferFit, ByteCount, CobolFieldName, DataNode, DataNodeKind, FieldAssignState,
    JsonBooleanMapping, JsonCode, JsonStatus, NameMapping, NameOverride, ParseTargetKind,
    ParseTargetNode, SuppressCategoryFilter, SuppressCondition, SuppressRule, SuppressTarget,
    ValueCategory, XML_EVENT_REGISTER_LEN, XmlCode, XmlEventKind, XmlNodePlacement,
    copy_into_cobol_buffer, encode_utf16_units, is_zero_numeric_str, normalize_cobol_number,
    resolve_name_override, should_suppress_item, trim_trailing_spaces,
};
pub use xml_gen::{
    XmlAttributeMode, XmlDeclarationMode, XmlGenerateOptions, XmlGenerateOutcome,
    is_valid_xml_ncname, xml_generate, xml_generate_into_buffer,
};
pub use xml_parse::{
    XmlNamespaceMode, XmlParseCompletion, XmlParseOptions, XmlParseOutcome, XmlParseSession,
    XmlRegisters, is_valid_xml_char, is_xml_name_char, is_xml_name_start, xml_parse,
};
