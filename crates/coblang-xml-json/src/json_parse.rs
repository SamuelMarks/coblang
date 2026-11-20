//! Pure-Rust RFC 8259 `JSON PARSE` engine (`Milestone M20`).
//!
//! Parses RFC 8259 JSON documents and populates mutable COBOL [`ParseTargetNode`] hierarchies
//! honoring `NAME OF ... IS "alias" | OMITTED`, `SUPPRESS`, `CONVERTING ... BOOLEAN`,
//! `WITH DETAIL`, `JSON-CODE`, and `JSON-STATUS`.

use coblang_common::{CoblangError, Result};

use crate::types::{
    ByteCount, CobolFieldName, FieldAssignState, JsonBooleanMapping, JsonCode, JsonStatus,
    NameMapping, NameOverride, ParseTargetKind, ParseTargetNode, ValueCategory,
    normalize_cobol_number, resolve_name_override,
};

/// Backslash (`U+005C`).
const CH_BACKSLASH: char = '\u{005C}';
/// Double-quote (`U+0022`).
const CH_DQUOTE: char = '\u{0022}';
/// Backspace (`U+0008`).
const CH_BS: char = '\u{0008}';
/// Horizontal tab (`U+0009`).
const CH_TAB: char = '\u{0009}';
/// Line feed (`U+000A`).
const CH_LF: char = '\u{000A}';
/// Form feed (`U+000C`).
const CH_FF: char = '\u{000C}';
/// Carriage return (`U+000D`).
const CH_CR: char = '\u{000D}';

/// Exact-decimal RFC 8259 JSON value representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonValue {
    /// JSON `null` literal.
    Null,
    /// JSON `true` or `false` literal.
    Bool(bool),
    /// Exact lexical RFC 8259 number token (preserves arbitrary COBOL 38+ digit precision).
    Number(String),
    /// Decoded UTF-8 JSON string.
    Str(String),
    /// Ordered JSON array `[...]`.
    Array(Vec<JsonValue>),
    /// Ordered JSON object `{...}` preserving source member order.
    Object(Vec<(String, JsonValue)>),
}

/// Controls `WITH DETAIL` anomaly reporting on unmapped JSON properties during `JSON PARSE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonDetailMode {
    /// Silently ignores unmapped JSON object properties.
    Standard,
    /// Reports unmapped JSON properties in [`JsonStatus::UNMAPPED_KEY`] and diagnostics (`WITH DETAIL`).
    WithDetail,
}

/// Configuration options for `JSON PARSE`.
#[derive(Debug, Clone)]
pub struct JsonParseOptions {
    /// Whether `WITH DETAIL` unmapped-property reporting is enabled.
    pub detail_mode: JsonDetailMode,
    /// `NAME OF field IS "alias" | OMITTED` mappings.
    pub name_mappings: Vec<NameMapping>,
    /// Fields excluded from modification via `SUPPRESS field`.
    pub suppressed_fields: Vec<CobolFieldName>,
    /// `CONVERTING field FROM JSON BOOLEAN` mappings.
    pub boolean_mappings: Vec<JsonBooleanMapping>,
}

impl Default for JsonParseOptions {
    /// Returns default `JSON PARSE` options (`JsonDetailMode::Standard`).
    fn default() -> Self {
        Self::new()
    }
}

impl JsonParseOptions {
    /// Constructs default [`JsonParseOptions`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            detail_mode: JsonDetailMode::Standard,
            name_mappings: Vec::new(),
            suppressed_fields: Vec::new(),
            boolean_mappings: Vec::new(),
        }
    }

    /// Sets [`JsonDetailMode`] (`WITH DETAIL`).
    ///
    /// # Arguments
    /// * `mode` - Detail reporting policy.
    #[must_use]
    pub const fn with_detail_mode(mut self, mode: JsonDetailMode) -> Self {
        self.detail_mode = mode;
        self
    }

    /// Appends a [`NameMapping`] (`NAME OF` clause).
    ///
    /// # Arguments
    /// * `mapping` - Field alias or `OMITTED` wrapper directive.
    #[must_use]
    pub fn with_name_mapping(mut self, mapping: NameMapping) -> Self {
        self.name_mappings.push(mapping);
        self
    }

    /// Marks `field` as suppressed (`SUPPRESS field` in `JSON PARSE`).
    ///
    /// # Arguments
    /// * `field` - Target COBOL field identifier to leave unmodified.
    #[must_use]
    pub fn with_suppressed_field(mut self, field: CobolFieldName) -> Self {
        self.suppressed_fields.push(field);
        self
    }

    /// Appends a [`JsonBooleanMapping`] (`CONVERTING ... BOOLEAN`).
    ///
    /// # Arguments
    /// * `mapping` - Boolean true/false value mapping.
    #[must_use]
    pub fn with_boolean_mapping(mut self, mapping: JsonBooleanMapping) -> Self {
        self.boolean_mappings.push(mapping);
        self
    }
}

/// Terminal disposition of `JSON PARSE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonParseCompletion {
    /// Clean completion (`JSON-CODE = 0`, `JSON-STATUS = 0`, `NOT ON EXCEPTION` path).
    Completed,
    /// Non-fatal anomaly recorded in `JSON-STATUS` (`JSON-CODE = 0`, `JSON-STATUS > 0`).
    CompletedWithWarnings,
    /// Syntax or conversion exception (`JSON-CODE > 0`, `ON EXCEPTION` path).
    Exception,
}

/// Final registers and diagnostics produced by [`json_parse`].
#[derive(Debug, Clone)]
pub struct JsonParseOutcome {
    /// Terminal completion state.
    pub completion: JsonParseCompletion,
    /// Resulting `JSON-CODE` register.
    pub json_code: JsonCode,
    /// Resulting `JSON-STATUS` register.
    pub json_status: JsonStatus,
    /// Optional byte offset where RFC 8259 parsing failed.
    pub error_offset: Option<ByteCount>,
    /// Ordered warning/exception detail messages.
    pub diagnostics: Vec<String>,
}

/// Mutable status tracker threaded through `JSON PARSE` target population.
struct MapperState {
    /// Current `JSON-CODE` (`0` unless a hard conversion mismatch occurred).
    json_code: JsonCode,
    /// First non-zero `JSON-STATUS` observed.
    json_status: JsonStatus,
    /// Collected human-readable diagnostic messages.
    diagnostics: Vec<String>,
}

impl MapperState {
    /// Creates a clean initial [`MapperState`].
    const fn new() -> Self {
        Self {
            json_code: JsonCode::ZERO,
            json_status: JsonStatus::ZERO,
            diagnostics: Vec::new(),
        }
    }

    /// Records a non-fatal warning status if no status code has been latched yet.
    ///
    /// # Arguments
    /// * `status` - Warning [`JsonStatus`] code.
    /// * `msg` - Diagnostic message text.
    fn record_warning(&mut self, status: JsonStatus, msg: String) {
        if self.json_status.is_ok() {
            self.json_status = status;
        }
        self.diagnostics.push(msg);
    }

    /// Latches a conversion exception (`JSON-CODE = 200`) and [`JsonStatus`].
    ///
    /// # Arguments
    /// * `status` - Specific [`JsonStatus`] code.
    /// * `msg` - Diagnostic message text.
    fn record_conversion_error(&mut self, status: JsonStatus, msg: String) {
        if self.json_code.is_ok() {
            self.json_code = JsonCode::CONVERSION_ERROR;
        }
        self.json_status = status;
        self.diagnostics.push(msg);
    }
}

/// Parses an RFC 8259 JSON text and populates `target` according to COBOL `JSON PARSE` semantics.
///
/// Malformed JSON syntax or incompatible type conversions populate `json_code` / `json_status`
/// and return [`JsonParseCompletion::Exception`] so COBOL `ON EXCEPTION` handlers can inspect
/// special registers cleanly. Invalid option configurations (such as `NAME OF scalar IS OMITTED`
/// inside a group) return [`CoblangError::XmlJsonProcess`].
///
/// # Arguments
/// * `source` - Input UTF-8 JSON document string.
/// * `target` - Mutable COBOL [`ParseTargetNode`] hierarchy to populate in place.
/// * `options` - `JSON PARSE` configuration (`WITH DETAIL`, `NAME OF`, `SUPPRESS`, `CONVERTING`).
///
/// # Errors
/// Returns [`CoblangError::XmlJsonProcess`] if `options` specify `NAME OF ... IS OMITTED` on a
/// non-group subordinate item inside a group.
pub fn json_parse(
    source: &str,
    target: &mut ParseTargetNode,
    options: &JsonParseOptions,
) -> Result<JsonParseOutcome> {
    validate_target_overrides(target, options, RootFlag::Root)?;

    let doc = match parse_rfc8259_inner(source) {
        Ok(val) => val,
        Err(syn) => {
            return Ok(JsonParseOutcome {
                completion: JsonParseCompletion::Exception,
                json_code: JsonCode::SYNTAX_ERROR,
                json_status: JsonStatus::SYNTAX_ERROR,
                error_offset: Some(ByteCount::new(syn.offset)),
                diagnostics: vec![syn.message],
            });
        }
    };

    let mut state = MapperState::new();
    match resolve_name_override(&options.name_mappings, target.name.as_str()) {
        Some(NameOverride::Omitted) => {
            map_json_into_node(&doc, target, options, &mut state);
        }
        Some(NameOverride::Rename(alias)) => {
            map_wrapped_root(&doc, alias, target, options, &mut state);
        }
        None => {
            let root_key = target.name.as_str().to_owned();
            map_wrapped_root(&doc, &root_key, target, options, &mut state);
        }
    }

    let completion = if !state.json_code.is_ok() {
        JsonParseCompletion::Exception
    } else if !state.json_status.is_ok() {
        JsonParseCompletion::CompletedWithWarnings
    } else {
        JsonParseCompletion::Completed
    };

    Ok(JsonParseOutcome {
        completion,
        json_code: state.json_code,
        json_status: state.json_status,
        error_offset: None,
        diagnostics: state.diagnostics,
    })
}

/// Distinguishes root vs subordinate nodes when validating `NAME OF ... IS OMITTED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootFlag {
    /// Top-level `INTO` target node.
    Root,
    /// Subordinate child of a group.
    ChildOfGroup,
}

/// Validates that `NAME OF ... IS OMITTED` is not attached to a non-group child of a group.
///
/// # Arguments
/// * `node` - Target node to validate recursively.
/// * `options` - Active `JSON PARSE` options.
/// * `flag` - Whether `node` is the root target or a group child.
fn validate_target_overrides(
    node: &ParseTargetNode,
    options: &JsonParseOptions,
    flag: RootFlag,
) -> Result<()> {
    if flag == RootFlag::ChildOfGroup
        && matches!(
            resolve_name_override(&options.name_mappings, node.name.as_str()),
            Some(NameOverride::Omitted)
        )
        && !matches!(node.kind, ParseTargetKind::Group { .. })
    {
        return Err(CoblangError::XmlJsonProcess {
            kind: "JSON",
            offset: 0,
            message: format!(
                "NAME OF `{}` IS OMITTED inside a group requires a group target",
                node.name.as_str()
            ),
        });
    }
    match &node.kind {
        ParseTargetKind::Group { children } => {
            for child in children {
                validate_target_overrides(child, options, RootFlag::ChildOfGroup)?;
            }
        }
        ParseTargetKind::Table { elements, .. } => {
            for elem in elements {
                validate_target_overrides(elem, options, RootFlag::Root)?;
            }
        }
        ParseTargetKind::Elementary { .. } => {}
    }
    Ok(())
}

/// Matches a wrapped top-level JSON object `{"root_key": ...}` against `target`.
///
/// # Arguments
/// * `doc` - Parsed top-level [`JsonValue`].
/// * `root_key` - Expected outer JSON property name.
/// * `target` - Root COBOL receiving node.
/// * `options` - Active `JSON PARSE` options.
/// * `state` - Mutable status/diagnostic accumulator.
fn map_wrapped_root(
    doc: &JsonValue,
    root_key: &str,
    target: &mut ParseTargetNode,
    options: &JsonParseOptions,
    state: &mut MapperState,
) {
    match doc {
        JsonValue::Null => {}
        JsonValue::Object(entries) => {
            for (k, v) in entries {
                if k == root_key {
                    map_json_into_node(v, target, options, state);
                } else if options.detail_mode == JsonDetailMode::WithDetail {
                    state.record_warning(
                        JsonStatus::UNMAPPED_KEY,
                        format!("unmapped top-level JSON property `{k}`"),
                    );
                }
            }
        }
        _ => {
            state.record_conversion_error(
                JsonStatus::INCOMPATIBLE_TYPE,
                format!("expected JSON object wrapping `{root_key}`"),
            );
        }
    }
}

/// Maps `val` into `node` recursively while respecting `SUPPRESS`, `NAME OF`, and `CONVERTING`.
///
/// # Arguments
/// * `val` - Source [`JsonValue`].
/// * `node` - Receiving [`ParseTargetNode`].
/// * `options` - Active `JSON PARSE` options.
/// * `state` - Mutable status/diagnostic accumulator.
fn map_json_into_node(
    val: &JsonValue,
    node: &mut ParseTargetNode,
    options: &JsonParseOptions,
    state: &mut MapperState,
) {
    if is_field_suppressed(options, node.name.as_str()) || matches!(val, JsonValue::Null) {
        return;
    }

    let field_name = node.name.as_str().to_owned();
    match &mut node.kind {
        ParseTargetKind::Elementary {
            category,
            value,
            max_chars,
            assigned,
        } => {
            let mut slot = ScalarTargetRef {
                field_name: &field_name,
                category: *category,
                dst_value: value,
                max_chars: *max_chars,
                assigned,
            };
            assign_elementary_from_json(val, &mut slot, options, state);
        }
        ParseTargetKind::Group { children } => {
            map_json_into_group(val, &field_name, children, options, state);
        }
        ParseTargetKind::Table {
            elements,
            populated_count,
        } => {
            let JsonValue::Array(items) = val else {
                state.record_conversion_error(
                    JsonStatus::INCOMPATIBLE_TYPE,
                    format!("expected JSON array for COBOL table `{field_name}`"),
                );
                return;
            };
            let capacity = elements.len();
            for (slot, item) in elements.iter_mut().zip(items.iter()) {
                map_json_into_node(item, slot, options, state);
            }
            *populated_count = items.len().min(capacity);
            if items.len() > capacity {
                state.record_warning(
                    JsonStatus::ARRAY_OVERFLOW,
                    format!(
                        "JSON array `{field_name}` length {} exceeds OCCURS capacity {capacity}",
                        items.len()
                    ),
                );
            }
        }
    }
}

/// Maps a [`JsonValue`] into the children of a COBOL group receiving node.
///
/// # Arguments
/// * `val` - Candidate [`JsonValue`] (must be [`JsonValue::Object`]).
/// * `group_name` - Enclosing COBOL group name for diagnostics.
/// * `children` - Mutable child receiving nodes.
/// * `options` - Active `JSON PARSE` options.
/// * `state` - Mutable status/diagnostic accumulator.
fn map_json_into_group(
    val: &JsonValue,
    group_name: &str,
    children: &mut [ParseTargetNode],
    options: &JsonParseOptions,
    state: &mut MapperState,
) {
    let JsonValue::Object(entries) = val else {
        state.record_conversion_error(
            JsonStatus::INCOMPATIBLE_TYPE,
            format!("expected JSON object for COBOL group `{group_name}`"),
        );
        return;
    };

    populate_omitted_children(entries, children, options, state);

    for (json_key, json_val) in entries {
        if let Some(matched_child) = find_named_child_mut(children, json_key, options) {
            map_json_into_node(json_val, matched_child, options, state);
        } else if !claimed_by_omitted_subgroup(children, json_key, options)
            && options.detail_mode == JsonDetailMode::WithDetail
        {
            state.record_warning(
                JsonStatus::UNMAPPED_KEY,
                format!("unmapped JSON property `{json_key}` in group `{group_name}`"),
            );
        }
    }
}

/// Scans `children` for non-suppressed `OMITTED` subgroups and populates them from `entries`.
///
/// # Arguments
/// * `entries` - Parent JSON object `(key, value)` members.
/// * `children` - Mutable child receiving nodes of the enclosing group.
/// * `options` - Active `JSON PARSE` options.
/// * `state` - Mutable status/diagnostic accumulator.
fn populate_omitted_children(
    entries: &[(String, JsonValue)],
    children: &mut [ParseTargetNode],
    options: &JsonParseOptions,
    state: &mut MapperState,
) {
    for child in children.iter_mut() {
        let child_name = child.name.as_str().to_owned();
        if let (ParseTargetKind::Group { children: sub }, Some(NameOverride::Omitted)) = (
            &mut child.kind,
            resolve_name_override(&options.name_mappings, &child_name),
        ) && !is_field_suppressed(options, &child_name)
        {
            populate_omitted_children(entries, sub, options, state);
            for (json_key, json_val) in entries {
                if let Some(matched_child) = find_named_child_mut(sub, json_key, options) {
                    map_json_into_node(json_val, matched_child, options, state);
                }
            }
        }
    }
}

/// Finds a direct non-`OMITTED` child whose effective JSON property key matches `json_key`.
///
/// # Arguments
/// * `children` - Candidate child target nodes.
/// * `json_key` - Property name from the JSON object.
/// * `options` - Active `JSON PARSE` options.
fn find_named_child_mut<'a>(
    children: &'a mut [ParseTargetNode],
    json_key: &str,
    options: &JsonParseOptions,
) -> Option<&'a mut ParseTargetNode> {
    children.iter_mut().find(|child| {
        match resolve_name_override(&options.name_mappings, child.name.as_str()) {
            Some(NameOverride::Rename(alias)) => alias == json_key,
            Some(NameOverride::Omitted) => false,
            None => child.name.as_str() == json_key,
        }
    })
}

/// Checks whether `json_key` matches any descendant exposed through an `OMITTED` subgroup.
///
/// # Arguments
/// * `children` - Direct children of the enclosing group.
/// * `json_key` - Candidate JSON property key.
/// * `options` - Active `JSON PARSE` options.
fn claimed_by_omitted_subgroup(
    children: &[ParseTargetNode],
    json_key: &str,
    options: &JsonParseOptions,
) -> bool {
    for child in children {
        if let (ParseTargetKind::Group { children: sub }, Some(NameOverride::Omitted)) = (
            &child.kind,
            resolve_name_override(&options.name_mappings, child.name.as_str()),
        ) {
            let direct = sub.iter().any(|gc| {
                match resolve_name_override(&options.name_mappings, gc.name.as_str()) {
                    Some(NameOverride::Rename(alias)) => alias == json_key,
                    Some(NameOverride::Omitted) => false,
                    None => gc.name.as_str() == json_key,
                }
            });
            if direct || claimed_by_omitted_subgroup(sub, json_key, options) {
                return true;
            }
        }
    }
    false
}

/// Mutable view of an elementary receiving target field during `JSON PARSE`.
struct ScalarTargetRef<'a> {
    /// Receiving COBOL field name.
    field_name: &'a str,
    /// Receiving [`ValueCategory`].
    category: ValueCategory,
    /// Mutable destination string storage.
    dst_value: &'a mut String,
    /// Optional character capacity limit.
    max_chars: Option<usize>,
    /// Mutable assignment state tracker.
    assigned: &'a mut FieldAssignState,
}

/// Assigns an elementary COBOL target value from a scalar [`JsonValue`].
///
/// # Arguments
/// * `val` - Source JSON value.
/// * `slot` - Mutable receiving scalar field view.
/// * `options` - Active `JSON PARSE` options.
/// * `state` - Mutable status/diagnostic accumulator.
fn assign_elementary_from_json(
    val: &JsonValue,
    slot: &mut ScalarTargetRef<'_>,
    options: &JsonParseOptions,
    state: &mut MapperState,
) {
    if let Some(bool_map) = options
        .boolean_mappings
        .iter()
        .find(|m| m.field_name.as_str() == slot.field_name)
    {
        match val {
            JsonValue::Bool(b) => {
                let mapped = if *b {
                    &bool_map.true_value
                } else {
                    &bool_map.false_value
                };
                store_with_truncation(
                    mapped,
                    slot.dst_value,
                    slot.max_chars,
                    slot.field_name,
                    state,
                );
                *slot.assigned = FieldAssignState::Assigned;
            }
            _ => {
                state.record_conversion_error(
                    JsonStatus::INCOMPATIBLE_TYPE,
                    format!(
                        "expected JSON boolean for CONVERTING field `{}`",
                        slot.field_name
                    ),
                );
            }
        }
        return;
    }

    match slot.category {
        ValueCategory::Numeric => match val {
            JsonValue::Number(raw_num) | JsonValue::Str(raw_num) => {
                match normalize_json_number_for_cobol(raw_num) {
                    Ok(clean_num) => {
                        store_with_truncation(
                            &clean_num,
                            slot.dst_value,
                            slot.max_chars,
                            slot.field_name,
                            state,
                        );
                        *slot.assigned = FieldAssignState::Assigned;
                    }
                    Err(_) => {
                        state.record_conversion_error(
                            JsonStatus::INCOMPATIBLE_TYPE,
                            format!(
                                "invalid numeric value `{raw_num}` for `{}`",
                                slot.field_name
                            ),
                        );
                    }
                }
            }
            _ => {
                state.record_conversion_error(
                    JsonStatus::INCOMPATIBLE_TYPE,
                    format!("expected numeric JSON value for `{}`", slot.field_name),
                );
            }
        },
        ValueCategory::Alphanumeric | ValueCategory::National => match val {
            JsonValue::Str(s) => {
                store_with_truncation(s, slot.dst_value, slot.max_chars, slot.field_name, state);
                *slot.assigned = FieldAssignState::Assigned;
            }
            JsonValue::Number(n) => {
                store_with_truncation(n, slot.dst_value, slot.max_chars, slot.field_name, state);
                *slot.assigned = FieldAssignState::Assigned;
            }
            JsonValue::Bool(b) => {
                let text = if *b { "true" } else { "false" };
                store_with_truncation(text, slot.dst_value, slot.max_chars, slot.field_name, state);
                *slot.assigned = FieldAssignState::Assigned;
            }
            JsonValue::Array(_) | JsonValue::Object(_) | JsonValue::Null => {
                state.record_conversion_error(
                    JsonStatus::INCOMPATIBLE_TYPE,
                    format!(
                        "cannot assign structured JSON value to elementary `{}`",
                        slot.field_name
                    ),
                );
            }
        },
    }
}

/// Copies `candidate` into `dst`, truncating to `max_chars` Unicode scalar values if configured.
///
/// # Arguments
/// * `candidate` - Decoded candidate value string.
/// * `dst` - Destination field string buffer.
/// * `max_chars` - Optional maximum character capacity.
/// * `field_name` - Receiving field name for truncation diagnostics.
/// * `state` - Mutable status/diagnostic accumulator.
fn store_with_truncation(
    candidate: &str,
    dst: &mut String,
    max_chars: Option<usize>,
    field_name: &str,
    state: &mut MapperState,
) {
    if let Some(limit) = max_chars
        && candidate.chars().count() > limit
    {
        *dst = candidate.chars().take(limit).collect();
        state.record_warning(
            JsonStatus::TRUNCATED_VALUE,
            format!("value for `{field_name}` truncated to {limit} characters"),
        );
    } else {
        dst.clear();
        dst.push_str(candidate);
    }
}

/// Returns `true` if `field_name` is listed in `options.suppressed_fields`.
///
/// # Arguments
/// * `options` - Active `JSON PARSE` options.
/// * `field_name` - Candidate COBOL field name.
fn is_field_suppressed(options: &JsonParseOptions, field_name: &str) -> bool {
    options
        .suppressed_fields
        .iter()
        .any(|f| f.as_str() == field_name)
}

/// Expands optional scientific notation (`e`/`E`) and normalizes a numeric literal for COBOL.
///
/// # Arguments
/// * `raw` - Candidate numeric token from JSON number or numeric string.
fn normalize_json_number_for_cobol(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let Some((mantissa_raw, exp_raw)) = trimmed.split_once('e').or_else(|| trimmed.split_once('E'))
    else {
        return normalize_cobol_number("JSON", trimmed);
    };

    let exp: i32 = exp_raw.parse().map_err(|_| CoblangError::XmlJsonProcess {
        kind: "JSON",
        offset: 0,
        message: format!("invalid exponent in `{raw}`"),
    })?;
    let norm_mantissa = normalize_cobol_number("JSON", mantissa_raw)?;
    let (neg, unsigned) = match norm_mantissa.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, norm_mantissa.as_str()),
    };
    let (int_part, frac_part) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let digits = format!("{int_part}{frac_part}");
    let new_point_pos = i32::try_from(int_part.len()).unwrap_or(0) + exp;
    let total_digits = i32::try_from(digits.len()).unwrap_or(0);

    let shifted = if new_point_pos <= 0 {
        let zeros = usize::try_from(-new_point_pos).unwrap_or(0);
        format!("0.{}{digits}", "0".repeat(zeros))
    } else if new_point_pos >= total_digits {
        let zeros = usize::try_from(new_point_pos - total_digits).unwrap_or(0);
        format!("{digits}{}", "0".repeat(zeros))
    } else {
        let split_idx = usize::try_from(new_point_pos).unwrap_or(0);
        let lhs = digits.get(..split_idx).unwrap_or("");
        let rhs = digits.get(split_idx..).unwrap_or("");
        format!("{lhs}.{rhs}")
    };

    let signed = if neg { format!("-{shifted}") } else { shifted };
    normalize_cobol_number("JSON", &signed)
}

/// Internal RFC 8259 syntax error carrying exact byte offset and reason.
struct JsonSyntaxError {
    /// Zero-based UTF-8 byte offset where parsing failed.
    offset: usize,
    /// Diagnostic failure message.
    message: String,
}

/// Strict pure-Rust RFC 8259 JSON document parser.
///
/// # Arguments
/// * `source` - Complete UTF-8 JSON document string.
///
/// # Errors
/// Returns [`CoblangError::XmlJsonProcess`] (`kind: "JSON"`) with the exact zero-based byte
/// offset if `source` violates RFC 8259 grammar.
pub fn parse_json_document(source: &str) -> Result<JsonValue> {
    parse_rfc8259_inner(source).map_err(|syn| CoblangError::XmlJsonProcess {
        kind: "JSON",
        offset: syn.offset,
        message: syn.message,
    })
}

/// Parses `source` into a [`JsonValue`] or [`JsonSyntaxError`].
///
/// # Arguments
/// * `source` - Complete UTF-8 JSON document string.
fn parse_rfc8259_inner(source: &str) -> core::result::Result<JsonValue, JsonSyntaxError> {
    let mut parser = Rfc8259Parser { source, pos: 0 };
    parser.skip_ws();
    let value = parser.parse_value()?;
    parser.skip_ws();
    if !parser.rest().is_empty() {
        return Err(parser.err("unexpected trailing characters after JSON value"));
    }
    Ok(value)
}

/// Stateful byte-offset cursor over an RFC 8259 UTF-8 JSON slice.
struct Rfc8259Parser<'a> {
    /// Full JSON source text.
    source: &'a str,
    /// Current byte offset in `source`.
    pos: usize,
}

impl<'a> Rfc8259Parser<'a> {
    /// Returns the remaining unparsed slice starting at `self.pos`.
    fn rest(&self) -> &'a str {
        self.source.get(self.pos..).unwrap_or("")
    }

    /// Builds a [`JsonSyntaxError`] at `self.pos`.
    ///
    /// # Arguments
    /// * `msg` - Error description.
    fn err(&self, msg: &str) -> JsonSyntaxError {
        JsonSyntaxError {
            offset: self.pos,
            message: msg.to_owned(),
        }
    }

    /// Skips RFC 8259 insignificant whitespace (`0x20`, `0x09`, `0x0A`, `0x0D`).
    fn skip_ws(&mut self) {
        let n = self
            .rest()
            .bytes()
            .take_while(|b| matches!(b, b' ' | 0x09 | 0x0A | 0x0D))
            .count();
        self.pos += n;
    }

    /// Parses a single JSON value at `self.pos`.
    fn parse_value(&mut self) -> core::result::Result<JsonValue, JsonSyntaxError> {
        let Some(first) = self.rest().as_bytes().first().copied() else {
            return Err(self.err("unexpected end of JSON input"));
        };
        match first {
            b'n' => self.parse_literal("null", JsonValue::Null),
            b't' => self.parse_literal("true", JsonValue::Bool(true)),
            b'f' => self.parse_literal("false", JsonValue::Bool(false)),
            b'"' => self.parse_string().map(JsonValue::Str),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => Err(self.err("unexpected token in JSON value")),
        }
    }

    /// Consumes an exact keyword literal (`null`, `true`, `false`).
    ///
    /// # Arguments
    /// * `kw` - Expected keyword string.
    /// * `out` - [`JsonValue`] produced upon match.
    fn parse_literal(
        &mut self,
        kw: &str,
        out: JsonValue,
    ) -> core::result::Result<JsonValue, JsonSyntaxError> {
        if self.rest().starts_with(kw) {
            self.pos += kw.len();
            Ok(out)
        } else {
            Err(self.err(&format!("expected JSON literal `{kw}`")))
        }
    }

    /// Parses an RFC 8259 JSON array `[...]`.
    fn parse_array(&mut self) -> core::result::Result<JsonValue, JsonSyntaxError> {
        self.pos += 1;
        self.skip_ws();
        if self.rest().starts_with(']') {
            self.pos += 1;
            return Ok(JsonValue::Array(Vec::new()));
        }
        let mut items = Vec::new();
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            if self.rest().starts_with(',') {
                self.pos += 1;
                self.skip_ws();
            } else if self.rest().starts_with(']') {
                self.pos += 1;
                return Ok(JsonValue::Array(items));
            } else {
                return Err(self.err("expected `,` or `]` in JSON array"));
            }
        }
    }

    /// Parses an RFC 8259 JSON object `{...}`.
    fn parse_object(&mut self) -> core::result::Result<JsonValue, JsonSyntaxError> {
        self.pos += 1;
        self.skip_ws();
        if self.rest().starts_with('}') {
            self.pos += 1;
            return Ok(JsonValue::Object(Vec::new()));
        }
        let mut members = Vec::new();
        loop {
            if !self.rest().starts_with(CH_DQUOTE) {
                return Err(self.err("expected quoted property name in JSON object"));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if !self.rest().starts_with(':') {
                return Err(self.err("expected `:` after property name in JSON object"));
            }
            self.pos += 1;
            self.skip_ws();
            let val = self.parse_value()?;
            members.push((key, val));
            self.skip_ws();
            if self.rest().starts_with(',') {
                self.pos += 1;
                self.skip_ws();
            } else if self.rest().starts_with('}') {
                self.pos += 1;
                return Ok(JsonValue::Object(members));
            } else {
                return Err(self.err("expected `,` or `}` in JSON object"));
            }
        }
    }

    /// Parses an RFC 8259 quoted string, decoding all escape sequences and UTF-16 surrogate pairs.
    fn parse_string(&mut self) -> core::result::Result<String, JsonSyntaxError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(ch) = self.rest().chars().next() else {
                return Err(self.err("unterminated JSON string literal"));
            };
            if ch == CH_DQUOTE {
                self.pos += 1;
                return Ok(out);
            }
            if ch == CH_BACKSLASH {
                self.pos += 1;
                let decoded = self.parse_escape_sequence()?;
                out.push(decoded);
            } else if (ch as u32) < 0x20 {
                return Err(self.err("unescaped control character inside JSON string"));
            } else {
                self.pos += ch.len_utf8();
                out.push(ch);
            }
        }
    }

    /// Parses the character(s) following `U+005C` inside a JSON string literal.
    fn parse_escape_sequence(&mut self) -> core::result::Result<char, JsonSyntaxError> {
        let Some(esc) = self.rest().chars().next() else {
            return Err(self.err("unterminated escape sequence in JSON string"));
        };
        self.pos += esc.len_utf8();
        match esc {
            CH_DQUOTE => Ok(CH_DQUOTE),
            CH_BACKSLASH => Ok(CH_BACKSLASH),
            '/' => Ok('/'),
            'b' => Ok(CH_BS),
            'f' => Ok(CH_FF),
            'n' => Ok(CH_LF),
            'r' => Ok(CH_CR),
            't' => Ok(CH_TAB),
            'u' => self.parse_unicode_escape(),
            _ => Err(self.err("invalid escape character in JSON string")),
        }
    }

    /// Parses a 4-hex-digit unicode escape (and trailing low surrogate when needed).
    fn parse_unicode_escape(&mut self) -> core::result::Result<char, JsonSyntaxError> {
        let unit = self.read_hex_u16()?;
        if (0xD800..=0xDBFF).contains(&unit) {
            let pair_prefix = format!("{CH_BACKSLASH}u");
            if !self.rest().starts_with(&pair_prefix) {
                return Err(self.err("unpaired UTF-16 high surrogate in JSON string"));
            }
            self.pos += 2;
            let low = self.read_hex_u16()?;
            if !(0xDC00..=0xDFFF).contains(&low) {
                return Err(self.err("invalid UTF-16 low surrogate in JSON string"));
            }
            let combined = 0x10000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
            Ok(char::from_u32(combined).unwrap_or('\u{FFFD}'))
        } else if (0xDC00..=0xDFFF).contains(&unit) {
            Err(self.err("unexpected lone UTF-16 low surrogate in JSON string"))
        } else {
            Ok(char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}'))
        }
    }

    /// Reads 4 hexadecimal digits at `self.pos` into a `u16`.
    fn read_hex_u16(&mut self) -> core::result::Result<u16, JsonSyntaxError> {
        let hex = self
            .rest()
            .get(..4)
            .ok_or_else(|| self.err("incomplete unicode hex escape in JSON string"))?;
        let val = u16::from_str_radix(hex, 16)
            .map_err(|_| self.err("invalid hex digit in unicode escape"))?;
        self.pos += 4;
        Ok(val)
    }

    /// Parses an RFC 8259 JSON number `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`.
    fn parse_number(&mut self) -> core::result::Result<JsonValue, JsonSyntaxError> {
        let start = self.pos;
        if self.rest().starts_with('-') {
            self.pos += 1;
        }
        match self.rest().as_bytes().first().copied() {
            Some(b'0') => {
                self.pos += 1;
                if self
                    .rest()
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_digit)
                {
                    return Err(self.err("leading zeros are not allowed in JSON numbers"));
                }
            }
            Some(b'1'..=b'9') => {
                self.advance_digits();
            }
            _ => return Err(self.err("expected digit in JSON number")),
        }

        if self.rest().starts_with('.') {
            self.pos += 1;
            if self.advance_digits() == 0 {
                return Err(self.err("expected fraction digits after `.` in JSON number"));
            }
        }

        if self.rest().starts_with(['e', 'E']) {
            self.pos += 1;
            if self.rest().starts_with(['+', '-']) {
                self.pos += 1;
            }
            if self.advance_digits() == 0 {
                return Err(self.err("expected exponent digits in JSON number"));
            }
        }

        let slice = self.source.get(start..self.pos).unwrap_or("");
        Ok(JsonValue::Number(slice.to_owned()))
    }

    /// Advances past contiguous ASCII decimal digits (`'0'..='9'`), returning count consumed.
    fn advance_digits(&mut self) -> usize {
        let count = self.rest().bytes().take_while(u8::is_ascii_digit).count();
        self.pos += count;
        count
    }
}
