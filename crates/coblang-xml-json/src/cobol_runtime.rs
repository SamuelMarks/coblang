//! COBOL runtime dialect adapters for `XML GENERATE`, `JSON GENERATE`, and streaming push-mode `XML PARSE` (`Milestone M20`).

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;

use crate::json_gen::escape_json_string_into;
use crate::types::{
    JsonCode, NameOverride, SuppressCategoryFilter, XmlCode, XmlNodePlacement, is_zero_numeric_str,
};
use crate::xml_gen::{XmlAttributeMode, is_valid_xml_ncname};
use crate::xml_parse::{is_valid_xml_char, is_xml_name_char, is_xml_name_start};

/// ASCII horizontal tab (`0x09`).
const CH_TAB: char = 0x09_u8 as char;
/// ASCII line-feed (`0x0A`).
const CH_LF: char = 0x0A_u8 as char;
/// ASCII carriage-return (`0x0D`).
const CH_CR: char = 0x0D_u8 as char;
/// ASCII double-quote (`0x22`).
const CH_DQUOTE: char = 0x22_u8 as char;
/// ASCII single-quote (`0x27`).
const CH_SQUOTE: char = 0x27_u8 as char;
/// Maximum buffered `CONTENT-CHARACTERS` byte count before a streaming split (`XML-INFORMATION = 2`).
const STREAM_TEXT_SPLIT_THRESHOLD: usize = 300;
/// Recoverable `XML-CODE` for an undeclared attribute namespace prefix (`201`).
pub const XML_CODE_UNDECLARED_ATTR_NS: i32 = 264_192;
/// Recoverable `XML-CODE` for an undeclared element namespace prefix (`201`).
pub const XML_CODE_UNDECLARED_ELEM_NS: i32 = 264_193;
/// Non-recoverable `XML-CODE` for malformed XML markup (`68`).
pub const XML_CODE_FATAL_SYNTAX: i32 = 786_432;
/// `XML GENERATE` exception code for invalid `NAMESPACE` URI characters (`416`).
pub const XML_GEN_CODE_INVALID_NAMESPACE: i32 = 416;
/// `XML GENERATE` exception code when invalid XML bytes were hex-encoded (`417`).
pub const XML_GEN_CODE_HEX_ENCODED: i32 = 417;
/// `XML GENERATE` exception code for invalid `NAMESPACE-PREFIX` (`419`).
pub const XML_GEN_CODE_INVALID_PREFIX: i32 = 419;

/// Controls space trimming direction on alphanumeric COBOL items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JustifiedTrim {
    /// Standard left-justified item (trims trailing spaces).
    Standard,
    /// `JUSTIFIED RIGHT` item (trims leading spaces).
    Right,
}

/// Decimal point character emitted for numeric items (`DPC-IN-DATA`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecimalSepMode {
    /// Standard ASCII period (`'.'`).
    Period,
    /// European decimal comma (`','`).
    Comma,
}

/// `WITH XML-DECLARATION` emission mode for COBOL `XML GENERATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CobolXmlDeclMode {
    /// Do not prepend an XML declaration.
    Omit,
    /// Prepend XML 1.0 version header followed by LF.
    IncludeLf,
}

/// Elementary COBOL data classification for `XML GENERATE` / `JSON GENERATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CobolGenCategory {
    /// Alphanumeric, alphabetic, or alphanumeric-edited item.
    Alphanumeric(JustifiedTrim),
    /// Exact numeric or index item.
    Numeric,
    /// Numeric-edited item (unquoted in XML, quoted string in JSON).
    NumericEdited,
    /// Boolean mapped item (`"true"` or `"false"` in JSON).
    Boolean,
}

/// Node payload inside a [`CobolGenNode`] hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CobolGenNodeKind {
    /// Leaf scalar item with decoded text and raw underlying storage bytes.
    Elementary {
        /// COBOL category.
        category: CobolGenCategory,
        /// Pre-formatted canonical string (using `.` decimal point for numbers).
        formatted: String,
        /// Raw underlying COBOL storage bytes (used for figurative `SUPPRESS` and hex fallback).
        raw_bytes: Vec<u8>,
    },
    /// Group item with ordered subordinate fields.
    Group {
        /// Subordinate items in declaration order.
        children: Vec<CobolGenNode>,
    },
    /// `OCCURS` table with `0..=N` active occurrences.
    Table {
        /// Active element nodes.
        occurrences: Vec<CobolGenNode>,
    },
}

/// Field-ID-addressed COBOL data node for `XML GENERATE` and `JSON GENERATE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CobolGenNode {
    /// Unique `DataItemId` (`u32`) for matching `NAME OF`, `TYPE OF`, and `SUPPRESS`.
    pub field_id: u32,
    /// Original source-cased COBOL identifier (`"FILLER"` when unnamed).
    pub raw_name: String,
    /// Elementary, group, or table payload.
    pub kind: CobolGenNodeKind,
}

/// Bitmask of `WHEN` figurative conditions in a COBOL `SUPPRESS` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CobolSuppressMask(u8);

impl CobolSuppressMask {
    /// Unconditional suppression (`SUPPRESS field` without `WHEN`).
    pub const UNCONDITIONAL: Self = Self(0);
    /// `WHEN ZERO` / `ZEROS` / `ZEROES`.
    pub const ZERO: u8 = 1 << 0;
    /// `WHEN SPACE` / `SPACES`.
    pub const SPACE: u8 = 1 << 1;
    /// `WHEN LOW-VALUE` / `LOW-VALUES`.
    pub const LOW_VALUE: u8 = 1 << 2;
    /// `WHEN HIGH-VALUE` / `HIGH-VALUES`.
    pub const HIGH_VALUE: u8 = 1 << 3;

    /// Constructs a mask from raw condition bits (`0` = unconditional).
    ///
    /// # Arguments
    /// * `bits` - Bitwise OR of `ZERO`, `SPACE`, `LOW_VALUE`, `HIGH_VALUE`, or `0`.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the raw bit representation.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Evaluates whether an elementary item matches this suppression mask.
    ///
    /// # Arguments
    /// * `category` - Elementary item category.
    /// * `formatted` - Canonical `.`-formatted value string.
    /// * `raw_bytes` - Underlying COBOL storage bytes.
    #[must_use]
    pub fn matches_elementary(
        self,
        category: CobolGenCategory,
        formatted: &str,
        raw_bytes: &[u8],
    ) -> bool {
        if self.0 == 0 {
            return true;
        }
        if (self.0 & Self::ZERO) != 0 {
            let is_z = match category {
                CobolGenCategory::Numeric => is_zero_numeric_str(formatted),
                CobolGenCategory::Alphanumeric(_)
                | CobolGenCategory::NumericEdited
                | CobolGenCategory::Boolean => {
                    !raw_bytes.is_empty() && raw_bytes.iter().all(|&b| b == b'0')
                }
            };
            if is_z {
                return true;
            }
        }
        if (self.0 & Self::SPACE) != 0
            && !raw_bytes.is_empty()
            && raw_bytes.iter().all(|&b| b == b' ')
        {
            return true;
        }
        if (self.0 & Self::LOW_VALUE) != 0
            && !raw_bytes.is_empty()
            && raw_bytes.iter().all(|&b| b == 0x00)
        {
            return true;
        }
        (self.0 & Self::HIGH_VALUE) != 0
            && !raw_bytes.is_empty()
            && raw_bytes.iter().all(|&b| b == 0xFF)
    }
}

/// Node placement filter on `SUPPRESS EVERY ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuppressPlacementFilter {
    /// Matches elements, attributes, and content.
    Any,
    /// Matches `ELEMENT` nodes only.
    ElementOnly,
    /// Matches `ATTRIBUTE` nodes only.
    AttributeOnly,
    /// Matches `CONTENT` nodes only.
    ContentOnly,
}

impl SuppressPlacementFilter {
    /// Checks whether `placement` satisfies this filter.
    ///
    /// # Arguments
    /// * `placement` - Effective [`XmlNodePlacement`].
    #[must_use]
    pub const fn matches(self, placement: XmlNodePlacement) -> bool {
        match self {
            Self::Any => true,
            Self::ElementOnly => matches!(placement, XmlNodePlacement::Element),
            Self::AttributeOnly => matches!(placement, XmlNodePlacement::Attribute),
            Self::ContentOnly => matches!(placement, XmlNodePlacement::Content),
        }
    }
}

/// Generic `SUPPRESS EVERY ...` rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CobolGenericSuppress {
    /// Category filter (`Any`, `NumericOnly`, `NonNumericOnly`).
    pub category: SuppressCategoryFilter,
    /// XML placement filter.
    pub placement: SuppressPlacementFilter,
    /// Figurative condition mask.
    pub mask: CobolSuppressMask,
}

/// Options governing COBOL `XML GENERATE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CobolXmlGenOptions {
    /// XML declaration header mode.
    pub xml_decl: CobolXmlDeclMode,
    /// Default attribute vs child-element mode.
    pub attr_mode: XmlAttributeMode,
    /// Decimal separator mode (`DPC-IN-DATA`).
    pub decimal_sep: DecimalSepMode,
    /// Raw `NAMESPACE` operand bytes (if clause specified).
    pub namespace_raw: Option<Vec<u8>>,
    /// Raw `NAMESPACE-PREFIX` operand bytes (if clause specified).
    pub namespace_prefix_raw: Option<Vec<u8>>,
    /// Field-ID-keyed `NAME OF` overrides.
    pub name_overrides: BTreeMap<u32, NameOverride>,
    /// Field-ID-keyed `TYPE OF` overrides.
    pub type_overrides: BTreeMap<u32, XmlNodePlacement>,
    /// Field-ID-keyed `SUPPRESS <field>` overrides (later rule for same field wins).
    pub field_suppress: BTreeMap<u32, CobolSuppressMask>,
    /// Generic `SUPPRESS EVERY ...` rules.
    pub generic_suppress: Vec<CobolGenericSuppress>,
}

impl Default for CobolXmlGenOptions {
    /// Returns default `XML GENERATE` options (`Omit` declaration, `ElementsOnly`, `Period`).
    fn default() -> Self {
        Self {
            xml_decl: CobolXmlDeclMode::Omit,
            attr_mode: XmlAttributeMode::ElementsOnly,
            decimal_sep: DecimalSepMode::Period,
            namespace_raw: None,
            namespace_prefix_raw: None,
            name_overrides: BTreeMap::new(),
            type_overrides: BTreeMap::new(),
            field_suppress: BTreeMap::new(),
            generic_suppress: Vec::new(),
        }
    }
}

/// Outcome of [`cobol_xml_generate_into_buffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CobolXmlGenResult {
    /// Resulting `XML-CODE` (`0` on success, `400`/`416`/`417`/`419` on exception).
    pub xml_code: XmlCode,
    /// Full generated character/byte count for `COUNT IN`.
    pub full_count: usize,
}

/// Shared XML namespace & hex-fallback context passed during tree traversal.
struct XmlEmitCtx<'a> {
    /// Generation options.
    opts: &'a CobolXmlGenOptions,
    /// Active namespace URI.
    ns_uri: Option<&'a str>,
    /// Active namespace prefix.
    ns_pfx: Option<&'a str>,
    /// Set to `true` when invalid XML bytes are hex-encoded (`417`).
    had_hex: bool,
}
/// Root vs child element position flag in XML emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeDepth {
    /// Outermost root node.
    Root,
    /// Nested child node.
    Child,
}

/// Generates COBOL XML into `dest`, returning `(XML-CODE, full_count)`.
///
/// # Arguments
/// * `root` - Root [`CobolGenNode`] hierarchy.
/// * `opts` - [`CobolXmlGenOptions`] configuration.
/// * `dest` - Destination COBOL receiving buffer.
pub fn cobol_xml_generate_into_buffer(
    root: &CobolGenNode,
    opts: &CobolXmlGenOptions,
    dest: &mut [u8],
) -> CobolXmlGenResult {
    let ns_uri = match validate_namespace_uri(opts.namespace_raw.as_deref()) {
        Ok(v) => v,
        Err(code) => {
            return CobolXmlGenResult {
                xml_code: code,
                full_count: 0,
            };
        }
    };
    let ns_pfx =
        match validate_namespace_prefix(opts.namespace_prefix_raw.as_deref(), ns_uri.as_deref()) {
            Ok(v) => v,
            Err(code) => {
                return CobolXmlGenResult {
                    xml_code: code,
                    full_count: 0,
                };
            }
        };

    let mut out = String::new();
    if opts.xml_decl == CobolXmlDeclMode::IncludeLf {
        let _ = write!(out, "<?xml version={CH_DQUOTE}1.0{CH_DQUOTE}?>{CH_LF}");
    }
    let mut ctx = XmlEmitCtx {
        opts,
        ns_uri: ns_uri.as_deref(),
        ns_pfx: ns_pfx.as_deref(),
        had_hex: false,
    };
    emit_xml_node(root, &mut ctx, NodeDepth::Root, &mut out);

    let full_count = out.len();
    let payload = out.as_bytes();
    if full_count <= dest.len() {
        for (d, &s) in dest.iter_mut().zip(payload.iter()) {
            *d = s;
        }
        if let Some(rem) = dest.get_mut(full_count..) {
            rem.fill(b' ');
        }
        let xml_code = if ctx.had_hex {
            XmlCode::new(XML_GEN_CODE_HEX_ENCODED)
        } else {
            XmlCode::ZERO
        };
        CobolXmlGenResult {
            xml_code,
            full_count,
        }
    } else {
        for (d, &s) in dest.iter_mut().zip(payload.iter()) {
            *d = s;
        }
        CobolXmlGenResult {
            xml_code: XmlCode::BUFFER_OVERFLOW,
            full_count: full_count.saturating_add(1),
        }
    }
}

/// Validates optional `NAMESPACE` bytes, returning trimmed URI or `Err(XmlCode(416))`.
///
/// # Arguments
/// * `raw` - Optional raw COBOL bytes of the `NAMESPACE` expression.
fn validate_namespace_uri(raw: Option<&[u8]>) -> Result<Option<String>, XmlCode> {
    let Some(bytes) = raw else {
        return Ok(None);
    };
    let Ok(s) = std::str::from_utf8(bytes) else {
        return Err(XmlCode::new(XML_GEN_CODE_INVALID_NAMESPACE));
    };
    let trimmed = s.trim_end_matches(' ');
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed
        .chars()
        .any(|c| c <= ' ' || !is_valid_xml_char(c) || c == '<' || c == '>' || c == CH_DQUOTE)
    {
        return Err(XmlCode::new(XML_GEN_CODE_INVALID_NAMESPACE));
    }
    Ok(Some(trimmed.to_owned()))
}

/// Validates optional `NAMESPACE-PREFIX` bytes, returning trimmed prefix or `Err(XmlCode(419))`.
///
/// # Arguments
/// * `raw` - Optional raw COBOL bytes of the `NAMESPACE-PREFIX` expression.
/// * `ns_uri` - Active `NAMESPACE` URI if present.
fn validate_namespace_prefix(
    raw: Option<&[u8]>,
    ns_uri: Option<&str>,
) -> Result<Option<String>, XmlCode> {
    let Some(bytes) = raw else {
        return Ok(None);
    };
    let Ok(s) = std::str::from_utf8(bytes) else {
        return Err(XmlCode::new(XML_GEN_CODE_INVALID_PREFIX));
    };
    let trimmed = s.trim_end_matches(' ');
    if trimmed.is_empty() {
        return Ok(None);
    }
    if ns_uri.is_none() || !is_valid_xml_ncname(trimmed) || trimmed == "xml" || trimmed == "xmlns" {
        return Err(XmlCode::new(XML_GEN_CODE_INVALID_PREFIX));
    }
    Ok(Some(trimmed.to_owned()))
}

/// Resolves sanitized XML tag name (`None` when `FILLER` or `NAME IS OMITTED`).
///
/// # Arguments
/// * `node` - Target [`CobolGenNode`].
/// * `overrides` - Field-ID name overrides.
fn resolve_xml_tag_name(
    node: &CobolGenNode,
    overrides: &BTreeMap<u32, NameOverride>,
) -> Option<String> {
    let base = match overrides.get(&node.field_id) {
        Some(NameOverride::Omitted) => return None,
        Some(NameOverride::Rename(s)) => s.as_str(),
        None => {
            if node.raw_name.eq_ignore_ascii_case("FILLER") || node.raw_name.is_empty() {
                return None;
            }
            node.raw_name.as_str()
        }
    };
    Some(sanitize_xml_tag(base))
}

/// Prepends `'_'` if `name` starts with an invalid XML `NCName` start char (such as `'0'..='9'`).
///
/// # Arguments
/// * `name` - Candidate tag or attribute name.
fn sanitize_xml_tag(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 1);
    if let Some(first) = name.chars().next()
        && !is_xml_name_start(first)
    {
        out.push('_');
    }
    for ch in name.chars() {
        if is_xml_name_char(ch) && ch != ':' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

/// Checks whether an elementary item should be suppressed under `XML GENERATE` or `JSON GENERATE`.
///
/// # Arguments
/// * `field_id` - Item `DataItemId` (`u32`).
/// * `category` - Item category.
/// * `placement` - Effective XML node placement.
/// * `formatted` - Canonical `.`-formatted value string.
/// * `raw_bytes` - Raw COBOL storage bytes.
/// * `field_suppress` - Field-specific rules.
/// * `generic_suppress` - Generic `EVERY` rules.
fn should_suppress_cobol_leaf(
    field_id: u32,
    category: CobolGenCategory,
    placement: XmlNodePlacement,
    formatted: &str,
    raw_bytes: &[u8],
    field_suppress: &BTreeMap<u32, CobolSuppressMask>,
    generic_suppress: &[CobolGenericSuppress],
) -> bool {
    if let Some(&mask) = field_suppress.get(&field_id) {
        return mask.matches_elementary(category, formatted, raw_bytes);
    }
    let is_num = matches!(category, CobolGenCategory::Numeric);
    generic_suppress.iter().any(|rule| {
        let cat_ok = match rule.category {
            SuppressCategoryFilter::Any => true,
            SuppressCategoryFilter::NumericOnly => is_num,
            SuppressCategoryFilter::NonNumericOnly => !is_num,
        };
        cat_ok
            && rule.placement.matches(placement)
            && rule.mask.matches_elementary(category, formatted, raw_bytes)
    })
}

/// Formats an elementary item's text for XML output, returning `Err(hex_string)` on invalid XML bytes.
///
/// # Arguments
/// * `category` - Elementary category.
/// * `formatted` - Canonical formatted value.
/// * `raw_bytes` - Raw storage bytes.
/// * `decimal_sep` - Decimal separator policy.
fn format_xml_scalar(
    category: CobolGenCategory,
    formatted: &str,
    raw_bytes: &[u8],
    decimal_sep: DecimalSepMode,
) -> Result<String, String> {
    match category {
        CobolGenCategory::Alphanumeric(just) => {
            let Ok(utf8) = std::str::from_utf8(raw_bytes) else {
                return Err(hex_lower(raw_bytes));
            };
            if utf8.chars().any(|c| !is_valid_xml_char(c)) {
                return Err(hex_lower(raw_bytes));
            }
            Ok(trim_cobol_alpha(utf8, just))
        }
        CobolGenCategory::Numeric => {
            if decimal_sep == DecimalSepMode::Comma {
                Ok(formatted.replace('.', ","))
            } else {
                Ok(formatted.to_owned())
            }
        }
        CobolGenCategory::NumericEdited | CobolGenCategory::Boolean => Ok(formatted.to_owned()),
    }
}

/// Trims COBOL alphanumeric string preserving a single `' '` when a non-empty field is all spaces.
///
/// # Arguments
/// * `s` - Raw UTF-8 string slice.
/// * `just` - Justification trimming direction.
fn trim_cobol_alpha(s: &str, just: JustifiedTrim) -> String {
    if s.is_empty() {
        return String::new();
    }
    let trimmed = match just {
        JustifiedTrim::Standard => s.trim_end_matches(' '),
        JustifiedTrim::Right => s.trim_start_matches(' '),
    };
    if trimmed.is_empty() {
        String::from(" ")
    } else {
        trimmed.to_owned()
    }
}

/// Encodes `bytes` as lowercase ASCII hexadecimal.
///
/// # Arguments
/// * `bytes` - Raw byte slice.
fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Appends XML-escaped character data (`&`, `<`, `>`).
///
/// # Arguments
/// * `text` - Unescaped string.
/// * `out` - Destination buffer.
fn push_xml_text(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

/// Appends XML-escaped attribute value (`&`, `<`, `>`, double-quote, tab, CR, LF).
///
/// # Arguments
/// * `text` - Unescaped attribute value.
/// * `out` - Destination buffer.
fn push_xml_attr(text: &str, out: &mut String) {
    for ch in text.chars() {
        if ch == '&' {
            out.push_str("&amp;");
        } else if ch == '<' {
            out.push_str("&lt;");
        } else if ch == '>' {
            out.push_str("&gt;");
        } else if ch == CH_DQUOTE {
            out.push_str("&quot;");
        } else if ch == CH_TAB {
            out.push_str("&#9;");
        } else if ch == CH_LF {
            out.push_str("&#10;");
        } else if ch == CH_CR {
            out.push_str("&#13;");
        } else {
            out.push(ch);
        }
    }
}

/// Emits a single [`CobolGenNode`] into `out`, returning `true` if anything was written.
///
/// # Arguments
/// * `node` - Current node.
/// * `ctx` - Mutable XML emission context.
/// * `depth` - `Root` vs `Child`.
/// * `out` - Output XML accumulator.
fn emit_xml_node(
    node: &CobolGenNode,
    ctx: &mut XmlEmitCtx<'_>,
    depth: NodeDepth,
    out: &mut String,
) -> bool {
    let is_root = depth == NodeDepth::Root;
    if !is_root
        && let Some(&mask) = ctx.opts.field_suppress.get(&node.field_id)
        && mask.bits() == 0
    {
        return false;
    }
    match &node.kind {
        CobolGenNodeKind::Table { occurrences } => {
            let mut any = false;
            for occ in occurrences {
                if emit_xml_node(occ, ctx, NodeDepth::Child, out) {
                    any = true;
                }
            }
            any
        }
        CobolGenNodeKind::Elementary {
            category,
            formatted,
            raw_bytes,
        } => {
            let Some(local_tag) = resolve_xml_tag_name(node, &ctx.opts.name_overrides) else {
                return false;
            };
            if should_suppress_cobol_leaf(
                node.field_id,
                *category,
                XmlNodePlacement::Element,
                formatted,
                raw_bytes,
                &ctx.opts.field_suppress,
                &ctx.opts.generic_suppress,
            ) {
                return false;
            }
            let qname = qualify_tag(ctx.ns_pfx, &local_tag);
            let xmlns_decl = root_xmlns_attr(depth, ctx.ns_uri, ctx.ns_pfx);
            match format_xml_scalar(*category, formatted, raw_bytes, ctx.opts.decimal_sep) {
                Ok(val) => {
                    if val.is_empty() {
                        let _ = write!(out, "<{qname}{xmlns_decl}/>");
                    } else {
                        let _ = write!(out, "<{qname}{xmlns_decl}>");
                        push_xml_text(&val, out);
                        let _ = write!(out, "</{qname}>");
                    }
                }
                Err(hex_str) => {
                    ctx.had_hex = true;
                    let _ = write!(out, "<hex.{qname}{xmlns_decl}>{hex_str}</hex.{qname}>");
                }
            }
            true
        }
        CobolGenNodeKind::Group { children } => emit_xml_group(node, children, ctx, depth, out),
    }
}

/// Emits a `Group` [`CobolGenNode`] into `out`, returning `true` if anything was written.
///
/// # Arguments
/// * `node` - Group node.
/// * `children` - Subordinate child nodes.
/// * `ctx` - Mutable XML emission context.
/// * `depth` - `Root` vs `Child`.
/// * `out` - Destination string buffer.
fn emit_xml_group(
    node: &CobolGenNode,
    children: &[CobolGenNode],
    ctx: &mut XmlEmitCtx<'_>,
    depth: NodeDepth,
    out: &mut String,
) -> bool {
    let is_root = depth == NodeDepth::Root;
    let tag_opt = resolve_xml_tag_name(node, &ctx.opts.name_overrides);
    if !is_root && tag_opt.is_none() {
        let mut any = false;
        for ch in children {
            if emit_xml_node(ch, ctx, NodeDepth::Child, out) {
                any = true;
            }
        }
        return any;
    }
    let mut attr_buf = root_xmlns_attr(depth, ctx.ns_uri, ctx.ns_pfx);
    let mut body_buf = String::new();
    let mut emitted_child = false;
    for ch in children {
        if emit_xml_group_child(ch, ctx, &mut attr_buf, &mut body_buf) {
            emitted_child = true;
        }
    }
    if !is_root && !emitted_child {
        return false;
    }
    if let Some(local_tag) = tag_opt {
        let qname = qualify_tag(ctx.ns_pfx, &local_tag);
        if body_buf.is_empty() {
            let _ = write!(out, "<{qname}{attr_buf}/>");
        } else {
            let _ = write!(out, "<{qname}{attr_buf}>{body_buf}</{qname}>");
        }
    } else {
        out.push_str(&body_buf);
    }
    true
}

/// Emits a single subordinate child of an XML group into `attr_buf` or `body_buf`.
///
/// # Arguments
/// * `ch` - Child node.
/// * `ctx` - Mutable XML emission context.
/// * `attr_buf` - Attribute accumulator.
/// * `body_buf` - Element body accumulator.
fn emit_xml_group_child(
    ch: &CobolGenNode,
    ctx: &mut XmlEmitCtx<'_>,
    attr_buf: &mut String,
    body_buf: &mut String,
) -> bool {
    let placement = effective_xml_placement(ch, ctx.opts);
    match (&ch.kind, placement) {
        (
            CobolGenNodeKind::Elementary {
                category,
                formatted,
                raw_bytes,
            },
            XmlNodePlacement::Attribute,
        ) => {
            let Some(attr_local) = resolve_xml_tag_name(ch, &ctx.opts.name_overrides) else {
                return false;
            };
            if should_suppress_cobol_leaf(
                ch.field_id,
                *category,
                XmlNodePlacement::Attribute,
                formatted,
                raw_bytes,
                &ctx.opts.field_suppress,
                &ctx.opts.generic_suppress,
            ) {
                return false;
            }
            let Ok(val) = format_xml_scalar(*category, formatted, raw_bytes, ctx.opts.decimal_sep)
            else {
                return false;
            };
            attr_buf.push(' ');
            attr_buf.push_str(&attr_local);
            attr_buf.push('=');
            attr_buf.push(CH_DQUOTE);
            push_xml_attr(&val, attr_buf);
            attr_buf.push(CH_DQUOTE);
            true
        }
        (
            CobolGenNodeKind::Elementary {
                category,
                formatted,
                raw_bytes,
            },
            XmlNodePlacement::Content,
        ) => {
            if should_suppress_cobol_leaf(
                ch.field_id,
                *category,
                XmlNodePlacement::Content,
                formatted,
                raw_bytes,
                &ctx.opts.field_suppress,
                &ctx.opts.generic_suppress,
            ) {
                return false;
            }
            let Ok(val) = format_xml_scalar(*category, formatted, raw_bytes, ctx.opts.decimal_sep)
            else {
                return false;
            };
            push_xml_text(&val, body_buf);
            true
        }
        _ => emit_xml_node(ch, ctx, NodeDepth::Child, body_buf),
    }
}

/// Computes the effective [`XmlNodePlacement`] for `child`.
///
/// # Arguments
/// * `child` - Subordinate node.
/// * `opts` - Generation options.
fn effective_xml_placement(child: &CobolGenNode, opts: &CobolXmlGenOptions) -> XmlNodePlacement {
    if let Some(&ov) = opts.type_overrides.get(&child.field_id) {
        return ov;
    }
    if matches!(child.kind, CobolGenNodeKind::Elementary { .. })
        && opts.attr_mode == XmlAttributeMode::WithAttributes
    {
        XmlNodePlacement::Attribute
    } else {
        XmlNodePlacement::Element
    }
}

/// Qualifies `local` with optional `prefix`.
///
/// # Arguments
/// * `prefix` - Optional namespace prefix.
/// * `local` - Local tag name.
fn qualify_tag(prefix: Option<&str>, local: &str) -> String {
    if let Some(p) = prefix {
        format!("{p}:{local}")
    } else {
        local.to_owned()
    }
}

/// Formats root `xmlns` attribute declaration if active.
///
/// # Arguments
/// * `depth` - `Root` vs `Child`.
/// * `ns_uri` - Optional namespace URI.
/// * `ns_pfx` - Optional namespace prefix.
fn root_xmlns_attr(depth: NodeDepth, ns_uri: Option<&str>, ns_pfx: Option<&str>) -> String {
    if depth != NodeDepth::Root {
        return String::new();
    }
    let Some(uri) = ns_uri else {
        return String::new();
    };
    let mut s = String::new();
    if let Some(pfx) = ns_pfx {
        let _ = write!(s, " xmlns:{pfx}={CH_DQUOTE}");
    } else {
        let _ = write!(s, " xmlns={CH_DQUOTE}");
    }
    push_xml_attr(uri, &mut s);
    s.push(CH_DQUOTE);
    s
}

/// Options governing COBOL `JSON GENERATE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CobolJsonGenOptions {
    /// Decimal separator mode (`DPC-IN-DATA`).
    pub decimal_sep: DecimalSepMode,
    /// Field-ID-keyed `NAME OF` overrides.
    pub name_overrides: BTreeMap<u32, NameOverride>,
    /// Field-ID-keyed `SUPPRESS <field>` overrides.
    pub field_suppress: BTreeMap<u32, CobolSuppressMask>,
    /// Generic `SUPPRESS EVERY ...` rules.
    pub generic_suppress: Vec<CobolGenericSuppress>,
}

impl Default for CobolJsonGenOptions {
    /// Returns default `JSON GENERATE` options (`Period` decimal separator, no overrides).
    fn default() -> Self {
        Self {
            decimal_sep: DecimalSepMode::Period,
            name_overrides: BTreeMap::new(),
            field_suppress: BTreeMap::new(),
            generic_suppress: Vec::new(),
        }
    }
}

/// Outcome of [`cobol_json_generate_into_buffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CobolJsonGenResult {
    /// Resulting `JSON-CODE` (`0` on success, `1` on buffer overflow).
    pub json_code: JsonCode,
    /// Full generated character/byte count for `COUNT IN`.
    pub full_count: usize,
}

/// Generates COBOL JSON into `dest`, returning `(JSON-CODE, full_count)`.
///
/// # Arguments
/// * `root` - Root [`CobolGenNode`] hierarchy.
/// * `opts` - [`CobolJsonGenOptions`] configuration.
/// * `dest` - Destination COBOL receiving buffer.
#[must_use]
pub fn cobol_json_generate_into_buffer(
    root: &CobolGenNode,
    opts: &CobolJsonGenOptions,
    dest: &mut [u8],
) -> CobolJsonGenResult {
    let mut out = String::new();
    match resolve_json_prop_name(root, &opts.name_overrides) {
        Some(prop) => {
            out.push('{');
            escape_json_string_into(&mut out, &prop);
            out.push(':');
            if let Some(val) = render_json_value(root, opts, NodeDepth::Root) {
                out.push_str(&val);
            } else {
                out.push_str("{}");
            }
            out.push('}');
        }
        None => {
            if let Some(val) = render_json_value(root, opts, NodeDepth::Root) {
                out.push_str(&val);
            } else {
                out.push_str("{}");
            }
        }
    }

    let full_count = out.len();
    let payload = out.as_bytes();
    if full_count <= dest.len() {
        for (d, &s) in dest.iter_mut().zip(payload.iter()) {
            *d = s;
        }
        if let Some(rem) = dest.get_mut(full_count..) {
            rem.fill(b' ');
        }
        CobolJsonGenResult {
            json_code: JsonCode::ZERO,
            full_count,
        }
    } else {
        for (d, &s) in dest.iter_mut().zip(payload.iter()) {
            *d = s;
        }
        CobolJsonGenResult {
            json_code: JsonCode::new(1),
            full_count,
        }
    }
}

/// Resolves JSON property name (`None` when `FILLER` or `NAME IS OMITTED`).
///
/// # Arguments
/// * `node` - Target node.
/// * `overrides` - Field-ID name overrides.
fn resolve_json_prop_name(
    node: &CobolGenNode,
    overrides: &BTreeMap<u32, NameOverride>,
) -> Option<String> {
    match overrides.get(&node.field_id) {
        Some(NameOverride::Omitted) => None,
        Some(NameOverride::Rename(s)) => Some(s.clone()),
        None => {
            if node.raw_name.eq_ignore_ascii_case("FILLER") || node.raw_name.is_empty() {
                None
            } else {
                Some(node.raw_name.clone())
            }
        }
    }
}

/// Renders the JSON value representation of `node`, or `None` if suppressed.
///
/// # Arguments
/// * `node` - Current node.
/// * `opts` - JSON generation options.
/// * `depth` - `Root` vs `Child`.
fn render_json_value(
    node: &CobolGenNode,
    opts: &CobolJsonGenOptions,
    depth: NodeDepth,
) -> Option<String> {
    let is_root = depth == NodeDepth::Root;
    if !is_root
        && opts
            .field_suppress
            .get(&node.field_id)
            .is_some_and(|m| m.bits() == 0)
    {
        return None;
    }
    match &node.kind {
        CobolGenNodeKind::Elementary {
            category,
            formatted,
            raw_bytes,
        } => {
            if !is_root
                && should_suppress_cobol_leaf(
                    node.field_id,
                    *category,
                    XmlNodePlacement::Element,
                    formatted,
                    raw_bytes,
                    &opts.field_suppress,
                    &opts.generic_suppress,
                )
            {
                return None;
            }
            let mut val = String::new();
            match category {
                CobolGenCategory::Alphanumeric(just) => {
                    let s = String::from_utf8_lossy(raw_bytes);
                    let trimmed = trim_cobol_alpha(&s, *just);
                    escape_json_string_into(&mut val, &trimmed);
                }
                CobolGenCategory::Numeric => {
                    if opts.decimal_sep == DecimalSepMode::Comma {
                        val.push_str(&formatted.replace('.', ","));
                    } else {
                        val.push_str(formatted);
                    }
                }
                CobolGenCategory::NumericEdited => {
                    escape_json_string_into(&mut val, formatted);
                }
                CobolGenCategory::Boolean => {
                    val.push_str(if formatted == "true" { "true" } else { "false" });
                }
            }
            Some(val)
        }
        CobolGenNodeKind::Table { occurrences } => {
            let mut elems = Vec::new();
            for occ in occurrences {
                if let Some(v) = render_json_value(occ, opts, NodeDepth::Child) {
                    elems.push(v);
                }
            }
            if elems.is_empty() && !is_root {
                return None;
            }
            Some(format!("[{}]", elems.join(",")))
        }
        CobolGenNodeKind::Group { children } => {
            let mut props = Vec::new();
            collect_json_group_props(children, opts, &mut props);
            if props.is_empty() && !is_root {
                return None;
            }
            Some(format!("{{{}}}", props.join(",")))
        }
    }
}

/// Collects `"key":value` pairs for group `children`, flattening `OMITTED`/`FILLER` subgroups.
///
/// # Arguments
/// * `children` - Subordinate group nodes.
/// * `opts` - JSON generation options.
/// * `props` - Output `"key":value` accumulator.
fn collect_json_group_props(
    children: &[CobolGenNode],
    opts: &CobolJsonGenOptions,
    props: &mut Vec<String>,
) {
    for ch in children {
        if opts
            .field_suppress
            .get(&ch.field_id)
            .is_some_and(|m| m.bits() == 0)
        {
            continue;
        }
        match resolve_json_prop_name(ch, &opts.name_overrides) {
            Some(prop) => {
                if let Some(val) = render_json_value(ch, opts, NodeDepth::Child) {
                    let mut entry = String::new();
                    escape_json_string_into(&mut entry, &prop);
                    entry.push(':');
                    entry.push_str(&val);
                    props.push(entry);
                }
            }
            None => {
                if let CobolGenNodeKind::Group { children: sub } = &ch.kind {
                    collect_json_group_props(sub, opts, props);
                }
            }
        }
    }
}

/// `XML PARSE` streaming compatibility mode (`XMLSS` vs `COMPAT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CobolXmlStreamMode {
    /// Modern XML System Services mode (emits `END-OF-INPUT` & `NAMESPACE-DECLARATION`).
    Xmlss,
    /// Legacy compatibility mode (`-fno-xml-parse-xmlss`).
    Compat,
}

/// End-of-stream confirmation flag inside [`CobolXmlStreamParser`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EofState {
    /// Current segment reached end, but handler may feed another segment (`XML-CODE = 1`).
    SegmentBoundary,
    /// Final end of input confirmed (`XML-CODE == 0` after `END-OF-INPUT` or `Compat` mode).
    Confirmed,
}

/// Single event yielded by [`CobolXmlStreamParser`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CobolXmlStreamEvent {
    /// `XML-EVENT` register string (`<= 30` ASCII bytes).
    pub event_name: &'static str,
    /// Initial `XML-CODE` for this event (`0`, `264192`, `264193`, `786432`).
    pub xml_code: i32,
    /// `XML-INFORMATION` value (`0`, `1` complete content, `2` split content).
    pub xml_information: i32,
    /// `XML-TEXT` dynamic payload.
    pub xml_text: String,
    /// `XML-NAMESPACE` dynamic URI.
    pub xml_namespace: String,
    /// `XML-NAMESPACE-PREFIX` dynamic prefix.
    pub xml_namespace_prefix: String,
    /// Optional `libcob: ...: warning: ...` diagnostic to emit on stderr before invoking handler.
    pub stderr_warning: Option<String>,
}

/// Post-handler continuation action returned by [`CobolXmlStreamParser::after_handler`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CobolXmlPostAction {
    /// Parsing has finished or aborted.
    Finished,
    /// Continue polling next event from current buffer.
    Continue,
    /// Handler set `XML-CODE = 1` on `END-OF-INPUT`; caller should feed next segment.
    NeedMoreInput,
}

/// Paired `<tag>` vs self-closing `<tag/>` classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagClosing {
    /// `<tag>...</tag>`.
    Paired,
    /// `<tag/>`.
    SelfClosing,
}

/// Resolved opening-tag header passed to `emit_open_tag_events`.
struct OpenElemHead<'a> {
    /// Element local name.
    local: &'a str,
    /// Element namespace prefix.
    prefix: &'a str,
    /// Resolved element namespace URI.
    ns: &'a str,
    /// Self-closing vs paired tag.
    closing: TagClosing,
    /// Saved `ns_bindings` length prior to this tag.
    saved_ns_len: usize,
}

/// Pending handler-feedback state inside [`CobolXmlStreamParser`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AwaitKind {
    /// Normal event delivery.
    Normal,
    /// Waiting for `XML-CODE` (`0` vs `1`) after `END-OF-INPUT`.
    EndOfInput,
    /// Waiting for `XML-CODE` (`0` recovery vs abort) after recoverable `EXCEPTION`.
    RecoverableException,
}

/// Compact bitmask of lifecycle flags inside [`CobolXmlStreamParser`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct StreamFlags(u8);

impl StreamFlags {
    /// `START-OF-DOCUMENT` has been queued.
    const STARTED_DOC: u8 = 1 << 0;
    /// Root element has been closed.
    const CLOSED_ROOT: u8 = 1 << 1;
    /// Current character run emitted at least one `>= 300`-byte split.
    const TEXT_SPLIT: u8 = 1 << 2;
    /// Session terminated via exception or `-1`.
    const HAD_EXC: u8 = 1 << 3;
    /// Reset `XML-EVENT` to spaces on exit (`XML-CODE == -1`).
    const CLEAR_EVENT: u8 = 1 << 4;
    /// Stream has finished.
    const DONE: u8 = 1 << 5;

    /// Returns `true` when `bit` is set.
    ///
    /// # Arguments
    ///
    /// * `bit` - Single-bit flag mask to test.
    const fn has(self, bit: u8) -> bool {
        (self.0 & bit) != 0
    }

    /// Sets `bit`.
    ///
    /// # Arguments
    ///
    /// * `bit` - Single-bit flag mask to set.
    fn set(&mut self, bit: u8) {
        self.0 |= bit;
    }

    /// Clears `bit`.
    ///
    /// # Arguments
    ///
    /// * `bit` - Single-bit flag mask to clear.
    fn clear(&mut self, bit: u8) {
        self.0 &= !bit;
    }
}

/// Streaming push-mode XML 1.0 + Namespaces parser for COBOL `XML PARSE`.
#[derive(Debug, Clone)]
pub struct CobolXmlStreamParser {
    /// Source file display name for runtime warnings.
    source_file: String,
    /// 1-based source line of `XML PARSE` statement.
    source_line: u32,
    /// `XMLSS` vs `COMPAT` mode.
    mode: CobolXmlStreamMode,
    /// Unconsumed UTF-8 input accumulator across segments.
    buf: String,
    /// Pending events ready for delivery.
    queue: VecDeque<CobolXmlStreamEvent>,
    /// Open element stack `(local_name, prefix, ns_uri, scope_depth)`.
    elem_stack: Vec<(String, String, String, usize)>,
    /// Active `(prefix, uri)` bindings.
    ns_bindings: Vec<(String, String)>,
    /// Accumulated `CONTENT-CHARACTERS` text inside an element.
    text_accum: String,
    /// Pending handler-feedback state.
    await_kind: AwaitKind,
    /// Lifecycle flags.
    flags: StreamFlags,
}

/// List of `(name, value)` XML attribute pairs.
type AttrPairList = Vec<(String, String)>;

impl CobolXmlStreamParser {
    /// Creates a new streaming `XML PARSE` session.
    ///
    /// # Arguments
    /// * `source_file` - Source filename for runtime warnings.
    /// * `source_line` - 1-based line number of `XML PARSE`.
    /// * `mode` - `Xmlss` or `Compat`.
    #[must_use]
    pub fn new(source_file: String, source_line: u32, mode: CobolXmlStreamMode) -> Self {
        Self {
            source_file,
            source_line,
            mode,
            buf: String::new(),
            queue: VecDeque::new(),
            elem_stack: Vec::new(),
            ns_bindings: vec![(
                String::from("xml"),
                String::from("http://www.w3.org/XML/1998/namespace"),
            )],
            text_accum: String::new(),
            await_kind: AwaitKind::Normal,
            flags: StreamFlags::default(),
        }
    }

    /// Appends an input segment to the streaming buffer.
    ///
    /// # Arguments
    /// * `bytes` - Raw input slice from the COBOL source operand.
    pub fn feed_segment(&mut self, bytes: &[u8]) {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
    }

    /// Polls the next [`CobolXmlStreamEvent`], or `None` when parsing is complete.
    pub fn next_event(&mut self) -> Option<CobolXmlStreamEvent> {
        if self.flags.has(StreamFlags::DONE) {
            return None;
        }
        if self.queue.is_empty() {
            self.pump_scanner(EofState::SegmentBoundary);
        }
        let ev = self.queue.pop_front()?;
        if ev.event_name == "END-OF-INPUT" {
            self.await_kind = AwaitKind::EndOfInput;
        } else if ev.event_name == "EXCEPTION"
            && matches!(
                ev.xml_code,
                XML_CODE_UNDECLARED_ATTR_NS | XML_CODE_UNDECLARED_ELEM_NS
            )
        {
            self.await_kind = AwaitKind::RecoverableException;
        } else {
            self.await_kind = AwaitKind::Normal;
            if ev.event_name == "EXCEPTION" {
                self.flags.set(StreamFlags::HAD_EXC | StreamFlags::DONE);
            } else if ev.event_name == "END-OF-DOCUMENT" {
                self.flags.set(StreamFlags::DONE);
            }
        }
        Some(ev)
    }

    /// Evaluates `XML-CODE` after the COBOL `PROCESSING PROCEDURE` finishes an event.
    ///
    /// # Arguments
    /// * `xml_code_after` - Current value of `XML-CODE` special register.
    pub fn after_handler(&mut self, xml_code_after: i32) -> CobolXmlPostAction {
        if xml_code_after == -1 {
            self.flags
                .set(StreamFlags::HAD_EXC | StreamFlags::CLEAR_EVENT | StreamFlags::DONE);
            return CobolXmlPostAction::Finished;
        }
        let kind = self.await_kind;
        self.await_kind = AwaitKind::Normal;
        match kind {
            AwaitKind::EndOfInput => {
                if xml_code_after == 1 {
                    return CobolXmlPostAction::NeedMoreInput;
                }
                if xml_code_after != 0 {
                    self.flags.set(StreamFlags::HAD_EXC | StreamFlags::DONE);
                    return CobolXmlPostAction::Finished;
                }
                self.pump_scanner(EofState::Confirmed);
                if self.flags.has(StreamFlags::DONE) && self.queue.is_empty() {
                    CobolXmlPostAction::Finished
                } else {
                    CobolXmlPostAction::Continue
                }
            }
            AwaitKind::RecoverableException => {
                if xml_code_after == 0 {
                    CobolXmlPostAction::Continue
                } else {
                    self.flags.set(StreamFlags::HAD_EXC | StreamFlags::DONE);
                    CobolXmlPostAction::Finished
                }
            }
            AwaitKind::Normal => {
                if self.flags.has(StreamFlags::DONE) {
                    CobolXmlPostAction::Finished
                } else if xml_code_after != 0 {
                    self.flags.set(StreamFlags::HAD_EXC | StreamFlags::DONE);
                    CobolXmlPostAction::Finished
                } else {
                    CobolXmlPostAction::Continue
                }
            }
        }
    }

    /// Returns `true` if `ON EXCEPTION` should execute when `XML PARSE` finishes.
    #[must_use]
    pub const fn had_exception(&self) -> bool {
        self.flags.has(StreamFlags::HAD_EXC)
    }

    /// Returns `true` if `XML-EVENT` should be reset to spaces (`XML-CODE == -1` abort).
    #[must_use]
    pub const fn clear_event_on_exit(&self) -> bool {
        self.flags.has(StreamFlags::CLEAR_EVENT)
    }

    /// Advances lexical scanning until at least one event is queued.
    ///
    /// # Arguments
    /// * `eof` - Segment boundary vs confirmed end-of-stream.
    fn pump_scanner(&mut self, eof: EofState) {
        let eof_confirmed = eof == EofState::Confirmed;
        while self.queue.is_empty() && !self.flags.has(StreamFlags::DONE) {
            if !self.flags.has(StreamFlags::STARTED_DOC) {
                if self.defer_until_xml_decl_ready(eof_confirmed) {
                    return;
                }
                self.flags.set(StreamFlags::STARTED_DOC);
                self.push_simple_event("START-OF-DOCUMENT", "");
                if self.buf.starts_with("<?xml")
                    && self
                        .buf
                        .get(5..)
                        .is_some_and(|r| r.starts_with(char::is_whitespace) || r.starts_with("?>"))
                {
                    if !self.scan_xml_decl() {
                        return;
                    }
                    continue;
                }
            }

            if self.elem_stack.is_empty() {
                let ws_len = self
                    .buf
                    .bytes()
                    .take_while(|&b| matches!(b, b' ' | 0x09 | 0x0D | 0x0A))
                    .count();
                if ws_len > 0 {
                    self.buf.drain(..ws_len);
                    continue;
                }
            }

            if self.buf.is_empty() {
                if eof_confirmed {
                    self.finalize_eof();
                } else {
                    self.push_end_of_input_or_finish();
                }
                return;
            }

            if self.buf.starts_with('<') {
                if !self.scan_markup_construct(eof) {
                    return;
                }
            } else if self.elem_stack.is_empty() {
                self.push_fatal_syntax(68, "StartTag: invalid element name");
                return;
            } else if !self.scan_character_data(eof) {
                return;
            }
        }
    }

    /// Defers `START-OF-DOCUMENT` via `END-OF-INPUT` if `<?xml ...?>` is split across segments.
    ///
    /// # Arguments
    /// * `eof_confirmed` - Whether input stream is finished.
    fn defer_until_xml_decl_ready(&mut self, eof_confirmed: bool) -> bool {
        if eof_confirmed {
            return false;
        }
        let needs_more = (self.buf.starts_with("<?xml")
            && self
                .buf
                .get(5..)
                .is_none_or(|r| r.starts_with(char::is_whitespace) || r.is_empty())
            && !self.buf.contains("?>"))
            || ("<?xml".starts_with(self.buf.as_str()) && !self.buf.is_empty());
        if needs_more {
            self.push_end_of_input_or_finish();
        }
        needs_more
    }

    /// Scans `<?xml ... ?>` at the start of `self.buf`.
    fn scan_xml_decl(&mut self) -> bool {
        let Some(end_idx) = self.buf.find("?>") else {
            self.push_fatal_syntax(68, "ParsePI: PI xml space expected");
            return false;
        };
        let inside = self.buf.get(5..end_idx).unwrap_or("").trim().to_owned();
        self.buf.drain(..end_idx + 2);
        let Some(attrs) = parse_tag_attrs(&inside) else {
            self.push_fatal_syntax(68, "String not started");
            return false;
        };
        for (k, v) in attrs {
            match k.as_str() {
                "version" => self.push_simple_event("VERSION-INFORMATION", &v),
                "encoding" => self.push_simple_event("ENCODING-DECLARATION", &v),
                "standalone" => self.push_simple_event("STANDALONE-DECLARATION", &v),
                _ => {}
            }
        }
        true
    }

    /// Scans a `<...>` construct, deferring to `END-OF-INPUT` if split across segments.
    ///
    /// # Arguments
    /// * `eof` - Segment boundary vs confirmed end-of-stream.
    fn scan_markup_construct(&mut self, eof: EofState) -> bool {
        let eof_confirmed = eof == EofState::Confirmed;
        if !self.text_accum.is_empty()
            && !"<![CDATA[".starts_with(self.buf.as_str())
            && !self.buf.starts_with("<![CDATA[")
        {
            self.flush_pending_text();
            return true;
        }
        if self.buf.starts_with("<!--")
            || self.buf.starts_with("<![CDATA[")
            || self.buf.starts_with("<?")
            || self.buf.starts_with("<!DOCTYPE")
        {
            return self.scan_special_markup(eof_confirmed);
        }
        if !eof_confirmed && is_incomplete_lt_prefix(&self.buf) {
            self.push_end_of_input_or_finish();
            return false;
        }
        if self.buf.starts_with("</>") {
            self.push_fatal_syntax(68, "StartTag: invalid element name");
            return false;
        }
        let Some(close_idx) = find_tag_close(&self.buf) else {
            if eof_confirmed {
                self.push_fatal_syntax(68, "Unclosed tag");
            } else {
                self.push_end_of_input_or_finish();
            }
            return false;
        };
        self.flush_pending_text();
        let raw_tag = self.buf.get(1..close_idx).unwrap_or("").to_owned();
        self.buf.drain(..=close_idx);
        if let Some(end_name_raw) = raw_tag.strip_prefix('/') {
            self.scan_close_tag(end_name_raw.trim())
        } else {
            self.scan_open_tag(&raw_tag)
        }
    }

    /// Scans `<!--...-->`, `<![CDATA[...]]>`, `<?...?>`, or `<!DOCTYPE ...>`.
    ///
    /// # Arguments
    /// * `eof_confirmed` - Whether input stream is finished.
    fn scan_special_markup(&mut self, eof_confirmed: bool) -> bool {
        if self.buf.starts_with("<!--") {
            let Some(end) = self.buf.find("-->") else {
                self.on_incomplete(eof_confirmed, "Comment not terminated");
                return false;
            };
            self.flush_pending_text();
            let text = self.buf.get(4..end).unwrap_or("").to_owned();
            self.buf.drain(..end + 3);
            self.push_simple_event("COMMENT", &text);
            return true;
        }
        if self.buf.starts_with("<![CDATA[") {
            let Some(end) = self.buf.find("]]>") else {
                self.on_incomplete(eof_confirmed, "CData section not finished");
                return false;
            };
            if self.elem_stack.is_empty() {
                self.push_fatal_syntax(68, "StartTag: invalid element name");
                return false;
            }
            self.flush_pending_text();
            let cdata = self.buf.get(9..end).unwrap_or("").to_owned();
            self.buf.drain(..end + 3);
            self.push_simple_event("START-OF-CDATA-SECTION", "");
            self.push_content_event(cdata, 1);
            self.push_simple_event("END-OF-CDATA-SECTION", "");
            return true;
        }
        if self.buf.starts_with("<?") {
            let Some(end) = self.buf.find("?>") else {
                self.on_incomplete(eof_confirmed, "ParsePI: never end");
                return false;
            };
            self.flush_pending_text();
            let body = self.buf.get(2..end).unwrap_or("").to_owned();
            self.buf.drain(..end + 2);
            let trimmed = body.trim_start();
            let tgt_len = trimmed
                .bytes()
                .take_while(|b| !b.is_ascii_whitespace())
                .count();
            let target = trimmed.get(..tgt_len).unwrap_or("");
            let data = trimmed.get(tgt_len..).unwrap_or("").trim_start();
            if target.is_empty() || target.eq_ignore_ascii_case("xml") {
                self.push_fatal_syntax(
                    68,
                    "XML declaration allowed only at the start of the document",
                );
                return false;
            }
            self.push_simple_event("PROCESSING-INSTRUCTION-TARGET", target);
            self.push_simple_event("PROCESSING-INSTRUCTION-DATA", data);
            return true;
        }
        let Some(end) = find_tag_close(&self.buf) else {
            self.on_incomplete(eof_confirmed, "DOCTYPE improperly terminated");
            return false;
        };
        let raw = self.buf.get(..=end).unwrap_or("").to_owned();
        self.buf.drain(..=end);
        self.push_simple_event("DOCUMENT-TYPE-DECLARATION", &raw);
        true
    }

    /// Emits fatal syntax error if `eof_confirmed`, otherwise `END-OF-INPUT`.
    ///
    /// # Arguments
    /// * `eof_confirmed` - Whether input stream is finished.
    /// * `msg` - Diagnostic message when truncated at EOF.
    fn on_incomplete(&mut self, eof_confirmed: bool, msg: &str) {
        if eof_confirmed {
            self.push_fatal_syntax(68, msg);
        } else {
            self.push_end_of_input_or_finish();
        }
    }

    /// Processes a closing `</qname>` tag.
    ///
    /// # Arguments
    /// * `qname` - Trimmed element `QName` after `/`.
    fn scan_close_tag(&mut self, qname: &str) -> bool {
        let Some((open_local, open_pfx, open_ns, saved_ns_len)) = self.elem_stack.pop() else {
            self.push_fatal_syntax(68, "StartTag: invalid element name");
            return false;
        };
        let (end_pfx, end_local) = split_qname(qname);
        if end_pfx != open_pfx || end_local != open_local {
            self.push_fatal_syntax(68, "Opening and ending tag mismatch");
            return false;
        }
        self.queue.push_back(CobolXmlStreamEvent {
            event_name: "END-OF-ELEMENT",
            xml_code: 0,
            xml_information: 0,
            xml_text: open_local,
            xml_namespace: open_ns,
            xml_namespace_prefix: open_pfx,
            stderr_warning: None,
        });
        self.ns_bindings.truncate(saved_ns_len);
        if self.elem_stack.is_empty() {
            self.flags.set(StreamFlags::CLOSED_ROOT);
        }
        true
    }

    /// Processes an opening `<qname ...>` or self-closing `<qname .../>` tag.
    ///
    /// # Arguments
    /// * `raw_tag` - Substring inside `<` and `>`.
    fn scan_open_tag(&mut self, raw_tag: &str) -> bool {
        let is_empty_elem = raw_tag.ends_with('/');
        let inner = if is_empty_elem {
            raw_tag.get(..raw_tag.len().saturating_sub(1)).unwrap_or("")
        } else {
            raw_tag
        };
        let trimmed = inner.trim();
        let name_len = trimmed
            .bytes()
            .take_while(|b| !b.is_ascii_whitespace())
            .count();
        let elem_qname = trimmed.get(..name_len).unwrap_or("");
        if !is_valid_qname(elem_qname) {
            self.push_fatal_syntax(68, "StartTag: invalid element name");
            return false;
        }
        let attr_part = trimmed.get(name_len..).unwrap_or("");
        let Some(raw_attrs) = parse_tag_attrs(attr_part) else {
            self.push_fatal_syntax(68, "Specification mandates value for attribute");
            return false;
        };

        let saved_ns_len = self.ns_bindings.len();
        let (ns_decls, normal_attrs) = self.bind_xmlns_attrs(raw_attrs);
        let (elem_pfx, elem_local) = split_qname(elem_qname);
        let resolved_attrs = self.resolve_tag_attrs(elem_local, normal_attrs);
        let elem_ns = self.resolve_elem_ns(elem_qname, elem_pfx, elem_local);
        let head = OpenElemHead {
            local: elem_local,
            prefix: elem_pfx,
            ns: &elem_ns,
            closing: if is_empty_elem {
                TagClosing::SelfClosing
            } else {
                TagClosing::Paired
            },
            saved_ns_len,
        };
        self.emit_open_tag_events(&head, ns_decls, resolved_attrs);
        true
    }

    /// Partition `xmlns` declarations from ordinary attributes and pushes namespace bindings.
    ///
    /// # Arguments
    /// * `raw_attrs` - Parsed `(name, value)` attribute pairs.
    fn bind_xmlns_attrs(&mut self, raw_attrs: AttrPairList) -> (AttrPairList, AttrPairList) {
        let mut ns_decls = Vec::new();
        let mut normal_attrs = Vec::new();
        for (ak, av) in raw_attrs {
            if ak == "xmlns" {
                self.ns_bindings.push((String::new(), av.clone()));
                ns_decls.push((String::new(), av));
            } else if let Some(pfx) = ak.strip_prefix("xmlns:") {
                self.ns_bindings.push((pfx.to_owned(), av.clone()));
                ns_decls.push((pfx.to_owned(), av));
            } else {
                normal_attrs.push((ak, av));
            }
        }
        (ns_decls, normal_attrs)
    }

    /// Resolves namespaces on non-`xmlns` attributes, queuing `264192` warnings when undeclared.
    ///
    /// # Arguments
    /// * `elem_local` - Enclosing element local name.
    /// * `normal_attrs` - Ordinary `(qname, value)` attributes.
    fn resolve_tag_attrs(
        &mut self,
        elem_local: &str,
        normal_attrs: Vec<(String, String)>,
    ) -> Vec<(String, String, String, String)> {
        let mut out = Vec::with_capacity(normal_attrs.len());
        for (ak, av) in normal_attrs {
            let (apfx, alocal) = split_qname(&ak);
            let ans = if apfx.is_empty() {
                String::new()
            } else if let Some(uri) = self.lookup_ns(apfx) {
                uri.to_owned()
            } else {
                let warn = format!(
                    "libcob: {}:{}: warning: XML PARSE recoverable error (201): Namespace prefix {apfx} for {alocal} on {elem_local} is not defined",
                    self.source_file, self.source_line
                );
                self.queue.push_back(CobolXmlStreamEvent {
                    event_name: "EXCEPTION",
                    xml_code: XML_CODE_UNDECLARED_ATTR_NS,
                    xml_information: 0,
                    xml_text: ak.clone(),
                    xml_namespace: String::new(),
                    xml_namespace_prefix: String::new(),
                    stderr_warning: Some(warn),
                });
                String::new()
            };
            out.push((alocal.to_owned(), apfx.to_owned(), ans, av));
        }
        out
    }

    /// Resolves element namespace URI, queuing `264193` recoverable warning when undeclared.
    ///
    /// # Arguments
    /// * `elem_qname` - Full element `QName`.
    /// * `elem_pfx` - Element prefix.
    /// * `elem_local` - Element local name.
    fn resolve_elem_ns(&mut self, elem_qname: &str, elem_pfx: &str, elem_local: &str) -> String {
        if elem_pfx.is_empty() {
            self.lookup_ns("").unwrap_or("").to_owned()
        } else if let Some(uri) = self.lookup_ns(elem_pfx) {
            uri.to_owned()
        } else {
            let warn = format!(
                "libcob: {}:{}: warning: XML PARSE recoverable error (201): Namespace prefix {elem_pfx} on {elem_local} is not defined",
                self.source_file, self.source_line
            );
            self.queue.push_back(CobolXmlStreamEvent {
                event_name: "EXCEPTION",
                xml_code: XML_CODE_UNDECLARED_ELEM_NS,
                xml_information: 0,
                xml_text: elem_qname.to_owned(),
                xml_namespace: String::new(),
                xml_namespace_prefix: String::new(),
                stderr_warning: Some(warn),
            });
            String::new()
        }
    }

    /// Queues `START-OF-ELEMENT`, `NAMESPACE-DECLARATION`, `ATTRIBUTE-*`, and optional `END-OF-ELEMENT`.
    ///
    /// # Arguments
    /// * `head` - Parsed element name/namespace header.
    /// * `ns_decls` - `xmlns` declarations on this tag.
    /// * `resolved_attrs` - Resolved attributes `(local, pfx, ns, val)`.
    fn emit_open_tag_events(
        &mut self,
        head: &OpenElemHead<'_>,
        ns_decls: Vec<(String, String)>,
        resolved_attrs: Vec<(String, String, String, String)>,
    ) {
        self.queue.push_back(CobolXmlStreamEvent {
            event_name: "START-OF-ELEMENT",
            xml_code: 0,
            xml_information: 0,
            xml_text: head.local.to_owned(),
            xml_namespace: head.ns.to_owned(),
            xml_namespace_prefix: head.prefix.to_owned(),
            stderr_warning: None,
        });
        if self.mode == CobolXmlStreamMode::Xmlss {
            for (dpfx, duri) in ns_decls {
                self.queue.push_back(CobolXmlStreamEvent {
                    event_name: "NAMESPACE-DECLARATION",
                    xml_code: 0,
                    xml_information: 0,
                    xml_text: String::new(),
                    xml_namespace: duri,
                    xml_namespace_prefix: dpfx,
                    stderr_warning: None,
                });
            }
        }
        for (alocal, apfx, ans, aval) in resolved_attrs {
            self.queue.push_back(CobolXmlStreamEvent {
                event_name: "ATTRIBUTE-NAME",
                xml_code: 0,
                xml_information: 0,
                xml_text: alocal,
                xml_namespace: ans,
                xml_namespace_prefix: apfx,
                stderr_warning: None,
            });
            self.queue.push_back(CobolXmlStreamEvent {
                event_name: "ATTRIBUTE-CHARACTERS",
                xml_code: 0,
                xml_information: 0,
                xml_text: aval,
                xml_namespace: String::new(),
                xml_namespace_prefix: String::new(),
                stderr_warning: None,
            });
        }
        if head.closing == TagClosing::SelfClosing {
            self.queue.push_back(CobolXmlStreamEvent {
                event_name: "END-OF-ELEMENT",
                xml_code: 0,
                xml_information: 0,
                xml_text: head.local.to_owned(),
                xml_namespace: head.ns.to_owned(),
                xml_namespace_prefix: head.prefix.to_owned(),
                stderr_warning: None,
            });
            self.ns_bindings.truncate(head.saved_ns_len);
            if self.elem_stack.is_empty() {
                self.flags.set(StreamFlags::CLOSED_ROOT);
            }
        } else {
            self.elem_stack.push((
                head.local.to_owned(),
                head.prefix.to_owned(),
                head.ns.to_owned(),
                head.saved_ns_len,
            ));
        }
    }

    /// Scans character data inside an open element up to `<` or segment end.
    ///
    /// # Arguments
    /// * `eof` - Segment boundary vs confirmed end-of-stream.
    fn scan_character_data(&mut self, eof: EofState) -> bool {
        let eof_confirmed = eof == EofState::Confirmed;
        let lt_pos = self.buf.find('<').unwrap_or(self.buf.len());
        let chunk = self.buf.get(..lt_pos).unwrap_or("");
        let safe_end = if lt_pos == self.buf.len() && !eof_confirmed {
            if let Some(amp_rel) = chunk.rfind('&') {
                if chunk.get(amp_rel..).unwrap_or("").contains(';') {
                    lt_pos
                } else {
                    amp_rel
                }
            } else {
                lt_pos
            }
        } else {
            lt_pos
        };

        if safe_end > 0 {
            let raw_piece = self.buf.get(..safe_end).unwrap_or("").to_owned();
            self.buf.drain(..safe_end);
            let Some(decoded) = decode_xml_entities(&raw_piece) else {
                self.push_fatal_syntax(68, "EntityRef: expecting ';'");
                return false;
            };
            self.text_accum.push_str(&decoded);
        }

        if self.buf.starts_with('<') {
            return true;
        }

        if self.text_accum.len() >= STREAM_TEXT_SPLIT_THRESHOLD && !eof_confirmed {
            let split_txt = std::mem::take(&mut self.text_accum);
            self.flags.set(StreamFlags::TEXT_SPLIT);
            self.push_content_event(split_txt, 2);
            return true;
        }

        if eof_confirmed {
            self.finalize_eof();
        } else {
            self.push_end_of_input_or_finish();
        }
        false
    }

    /// Flushes buffered `CONTENT-CHARACTERS` prior to markup (`info = 1`).
    fn flush_pending_text(&mut self) {
        if !self.text_accum.is_empty() || self.flags.has(StreamFlags::TEXT_SPLIT) {
            let txt = std::mem::take(&mut self.text_accum);
            self.flags.clear(StreamFlags::TEXT_SPLIT);
            self.push_content_event(txt, 1);
        }
    }

    /// Emits `END-OF-INPUT` in `Xmlss` mode or finalizes in `Compat` mode.
    fn push_end_of_input_or_finish(&mut self) {
        if self.mode == CobolXmlStreamMode::Xmlss {
            self.push_simple_event("END-OF-INPUT", "");
        } else {
            self.finalize_eof();
        }
    }

    /// Finalizes end-of-document after the final segment is exhausted.
    fn finalize_eof(&mut self) {
        if !self.elem_stack.is_empty() || !self.flags.has(StreamFlags::CLOSED_ROOT) {
            self.push_fatal_syntax(68, "Premature end of data in tag");
        } else {
            self.push_simple_event("END-OF-DOCUMENT", "");
        }
    }

    /// Queues a simple event with `xml_code = 0` and `xml_information = 0`.
    ///
    /// # Arguments
    /// * `event_name` - Static COBOL event name.
    /// * `text` - `XML-TEXT` payload.
    fn push_simple_event(&mut self, event_name: &'static str, text: &str) {
        self.queue.push_back(CobolXmlStreamEvent {
            event_name,
            xml_code: 0,
            xml_information: 0,
            xml_text: text.to_owned(),
            xml_namespace: String::new(),
            xml_namespace_prefix: String::new(),
            stderr_warning: None,
        });
    }

    /// Queues a `CONTENT-CHARACTERS` event with `xml_information` (`1` or `2`).
    ///
    /// # Arguments
    /// * `text` - Decoded content string.
    /// * `info` - `1` (complete) or `2` (split).
    fn push_content_event(&mut self, text: String, info: i32) {
        self.queue.push_back(CobolXmlStreamEvent {
            event_name: "CONTENT-CHARACTERS",
            xml_code: 0,
            xml_information: info,
            xml_text: text,
            xml_namespace: String::new(),
            xml_namespace_prefix: String::new(),
            stderr_warning: None,
        });
    }

    /// Queues a non-recoverable `EXCEPTION` event with `libcob` warning format.
    ///
    /// # Arguments
    /// * `err_num` - Libxml2-compatible numeric code (`68`).
    /// * `detail` - Diagnostic detail string.
    fn push_fatal_syntax(&mut self, err_num: u32, detail: &str) {
        let warn = format!(
            "libcob: {}:{}: warning: XML PARSE non-recoverable error ({err_num}): {detail}",
            self.source_file, self.source_line
        );
        let msg = format!("unhandled internal XML non-recoverable error ({err_num}): {detail}");
        self.queue.push_back(CobolXmlStreamEvent {
            event_name: "EXCEPTION",
            xml_code: XML_CODE_FATAL_SYNTAX,
            xml_information: 0,
            xml_text: msg,
            xml_namespace: String::new(),
            xml_namespace_prefix: String::new(),
            stderr_warning: Some(warn),
        });
    }

    /// Resolves the innermost active namespace URI for `prefix`.
    ///
    /// # Arguments
    /// * `prefix` - Namespace prefix (`""` for default namespace).
    fn lookup_ns(&self, prefix: &str) -> Option<&str> {
        self.ns_bindings
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .and_then(|(_, u)| if u.is_empty() { None } else { Some(u.as_str()) })
    }
}

/// Checks whether `buf` at segment end is a prefix of `<!--`, `<![CDATA[`, `<!DOCTYPE`, `<?`, or `</`.
///
/// # Arguments
/// * `buf` - Remaining input slice starting with `'<'`.
fn is_incomplete_lt_prefix(buf: &str) -> bool {
    ["<!--", "<![CDATA[", "<!DOCTYPE", "<?", "</"]
        .iter()
        .any(|pat| pat.len() > buf.len() && pat.starts_with(buf))
}

/// Finds the closing `>` outside single/double quotes.
///
/// # Arguments
/// * `buf` - Input slice starting at `<`.
fn find_tag_close(buf: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (idx, b) in buf.bytes().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            None if b == 0x22 || b == 0x27 => quote = Some(b),
            None if b == b'>' => return Some(idx),
            Some(_) | None => {}
        }
    }
    None
}

/// Splits `pfx:local` or `local`.
///
/// # Arguments
/// * `qname` - Qualified XML name.
fn split_qname(qname: &str) -> (&str, &str) {
    if let Some((p, l)) = qname.split_once(':') {
        (p, l)
    } else {
        ("", qname)
    }
}

/// Checks XML 1.0 `QName` validity.
///
/// # Arguments
/// * `qname` - Candidate XML `QName`.
fn is_valid_qname(qname: &str) -> bool {
    if qname.is_empty() {
        return false;
    }
    if let Some((p, l)) = qname.split_once(':') {
        is_valid_xml_ncname(p) && is_valid_xml_ncname(l)
    } else {
        is_valid_xml_ncname(qname)
    }
}

/// Parses XML tag attributes decoding entities and normalizing whitespace.
///
/// # Arguments
/// * `s` - Raw attribute substring inside an opening tag or XML declaration.
fn parse_tag_attrs(mut s: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start();
        if s.is_empty() {
            return Some(out);
        }
        let eq = s.find('=')?;
        let name = s.get(..eq)?.trim_end();
        if !is_valid_qname(name) {
            return None;
        }
        let after_eq = s.get(eq + 1..)?.trim_start();
        let q = after_eq.as_bytes().first().copied()?;
        if q != 0x22 && q != 0x27 {
            return None;
        }
        let qch = char::from(q);
        let rest = after_eq.get(1..)?;
        let close = rest.find(qch)?;
        let raw_val = rest.get(..close)?;
        let decoded = decode_attr_value(raw_val)?;
        out.push((name.to_owned(), decoded));
        s = rest.get(close + 1..)?;
    }
}

/// Decodes entities and normalizes literal whitespace in an attribute value.
///
/// # Arguments
/// * `raw` - Raw attribute value slice inside quotes.
fn decode_attr_value(raw: &str) -> Option<String> {
    let normalized: String = raw
        .chars()
        .map(|c| {
            if c == CH_TAB || c == CH_CR || c == CH_LF {
                ' '
            } else {
                c
            }
        })
        .collect();
    decode_xml_entities(&normalized)
}

/// Decodes `&lt;`, `&gt;`, `&amp;`, `&apos;`, `&quot;`, `&#...;`, and `&#x...;`, normalizing CR/LF.
///
/// # Arguments
/// * `raw` - Raw XML character data or normalized attribute slice.
fn decode_xml_entities(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == CH_CR {
            if chars.peek() == Some(&CH_LF) {
                let _ = chars.next();
            }
            out.push(CH_LF);
        } else if ch == '&' {
            let mut ent = String::new();
            let mut closed = false;
            for ec in chars.by_ref() {
                if ec == ';' {
                    closed = true;
                    break;
                }
                ent.push(ec);
            }
            if !closed {
                return None;
            }
            let decoded = match ent.as_str() {
                "lt" => '<',
                "gt" => '>',
                "amp" => '&',
                "apos" => CH_SQUOTE,
                "quot" => CH_DQUOTE,
                _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                    let cp = u32::from_str_radix(ent.get(2..)?, 16).ok()?;
                    let c = char::from_u32(cp)?;
                    if !is_valid_xml_char(c) {
                        return None;
                    }
                    c
                }
                _ if ent.starts_with('#') => {
                    let cp = ent.get(1..)?.parse::<u32>().ok()?;
                    let c = char::from_u32(cp)?;
                    if !is_valid_xml_char(c) {
                        return None;
                    }
                    c
                }
                _ => return None,
            };
            out.push(decoded);
        } else {
            out.push(ch);
        }
    }
    Some(out)
}
