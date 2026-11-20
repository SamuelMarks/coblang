//! Pure-Rust `XML GENERATE` engine (`Milestone M20`).
//!
//! Supports `WITH ATTRIBUTES`, `WITH XML-DECLARATION`, `NAMESPACE`, `NAMESPACE-PREFIX`,
//! `NAME OF`, `TYPE OF` (`ATTRIBUTE` / `ELEMENT` / `CONTENT`), `SUPPRESS`, and `COUNT IN`
//! with fixed-buffer overflow reporting.

use coblang_common::{CoblangError, Result};

use crate::types::{
    BufferFit, ByteCount, DataNode, DataNodeKind, NameMapping, NameOverride, SuppressRule,
    ValueCategory, XmlCode, XmlNodePlacement, copy_into_cobol_buffer, normalize_cobol_number,
    resolve_name_override, should_suppress_item, trim_trailing_spaces,
};
use crate::xml_parse::{is_valid_xml_char, is_xml_name_char, is_xml_name_start};

/// Standard UTF-8 XML 1.0 declaration header emitted for `WITH XML-DECLARATION`.
const XML_DECLARATION_HEADER: &str =
    "<?xml version=\u{0022}1.0\u{0022} encoding=\u{0022}UTF-8\u{0022}?>";
/// Double-quote character (`U+0022`).
const CH_DQUOTE: char = '\u{0022}';
/// Single-quote character (`U+0027`).
const CH_SQUOTE: char = '\u{0027}';

/// Controls whether elementary group children default to child elements or XML attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlAttributeMode {
    /// Emits elementary items as child elements unless overridden by `TYPE IS ATTRIBUTE`.
    ElementsOnly,
    /// Emits eligible scalar elementary items as attributes on their parent (`WITH ATTRIBUTES`).
    WithAttributes,
}

/// Controls whether `<?xml version="1.0" encoding="UTF-8"?>` is prepended (`WITH XML-DECLARATION`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlDeclarationMode {
    /// Omits the XML declaration prolog.
    Omit,
    /// Prepends the XML 1.0 declaration prolog (`WITH XML-DECLARATION`).
    Include,
}

/// Configuration for `XML GENERATE`.
#[derive(Debug, Clone)]
pub struct XmlGenerateOptions {
    /// Default attribute vs element emission policy (`WITH ATTRIBUTES`).
    pub attribute_mode: XmlAttributeMode,
    /// XML declaration prolog policy (`WITH XML-DECLARATION`).
    pub declaration_mode: XmlDeclarationMode,
    /// Optional `NAMESPACE IS` URI string.
    pub namespace_uri: Option<String>,
    /// Optional `NAMESPACE-PREFIX IS` `NCName` string.
    pub namespace_prefix: Option<String>,
    /// `NAME OF` tag rename overrides.
    pub name_mappings: Vec<NameMapping>,
    /// `SUPPRESS` rules evaluated on candidate nodes.
    pub suppress_rules: Vec<SuppressRule>,
}

impl Default for XmlGenerateOptions {
    /// Returns default `XML GENERATE` options (elements only, no XML declaration).
    fn default() -> Self {
        Self::new()
    }
}

impl XmlGenerateOptions {
    /// Creates default [`XmlGenerateOptions`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            attribute_mode: XmlAttributeMode::ElementsOnly,
            declaration_mode: XmlDeclarationMode::Omit,
            namespace_uri: None,
            namespace_prefix: None,
            name_mappings: Vec::new(),
            suppress_rules: Vec::new(),
        }
    }

    /// Sets [`XmlAttributeMode`] (`WITH ATTRIBUTES`).
    ///
    /// # Arguments
    /// * `mode` - Attribute generation policy.
    #[must_use]
    pub const fn with_attributes(mut self, mode: XmlAttributeMode) -> Self {
        self.attribute_mode = mode;
        self
    }

    /// Sets [`XmlDeclarationMode`] (`WITH XML-DECLARATION`).
    ///
    /// # Arguments
    /// * `mode` - XML declaration prolog policy.
    #[must_use]
    pub const fn with_xml_declaration(mut self, mode: XmlDeclarationMode) -> Self {
        self.declaration_mode = mode;
        self
    }

    /// Configures `NAMESPACE IS uri` and optional `NAMESPACE-PREFIX IS prefix`.
    ///
    /// # Arguments
    /// * `uri` - Target XML namespace URI.
    /// * `prefix` - Optional XML namespace prefix (`NCName`).
    #[must_use]
    pub fn with_namespace(mut self, uri: impl Into<String>, prefix: Option<String>) -> Self {
        self.namespace_uri = Some(uri.into());
        self.namespace_prefix = prefix;
        self
    }

    /// Appends a [`NameMapping`] (`NAME OF` clause).
    ///
    /// # Arguments
    /// * `mapping` - Field-to-tag override mapping.
    #[must_use]
    pub fn with_name_mapping(mut self, mapping: NameMapping) -> Self {
        self.name_mappings.push(mapping);
        self
    }

    /// Appends a [`SuppressRule`] (`SUPPRESS` clause).
    ///
    /// # Arguments
    /// * `rule` - Field or category suppression predicate.
    #[must_use]
    pub fn with_suppress_rule(mut self, rule: SuppressRule) -> Self {
        self.suppress_rules.push(rule);
        self
    }
}

/// Output produced by [`xml_generate`] and [`xml_generate_into_buffer`].
#[derive(Debug, Clone)]
pub struct XmlGenerateOutcome {
    /// Complete generated UTF-8 XML document.
    pub xml: String,
    /// Character/byte count (`COUNT IN` target value).
    pub count: ByteCount,
    /// Resulting `XML-CODE` (`0` on success, `400` on fixed-buffer overflow).
    pub xml_code: XmlCode,
}

/// Validated namespace configuration normalized for element tag emission.
struct ResolvedNamespace<'a> {
    /// Non-empty namespace URI when active.
    uri: Option<&'a str>,
    /// Non-empty validated `NCName` prefix when active.
    prefix: Option<&'a str>,
}

/// Distinguishes root vs subordinate node rendering for namespace declaration and group pruning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeDepth {
    /// Top-level `FROM` root item (carries `xmlns` attribute).
    Root,
    /// Nested subordinate item.
    Nested,
}

/// Generates a UTF-8 XML document from a COBOL [`DataNode`] hierarchy.
///
/// # Arguments
/// * `root` - Source COBOL group or elementary [`DataNode`].
/// * `options` - `XML GENERATE` formatting, namespace, rename, and suppression options.
///
/// # Errors
/// Returns [`CoblangError::XmlJsonProcess`] if a tag/attribute name, namespace configuration,
/// numeric representation, or XML 1.0 character is invalid.
pub fn xml_generate(root: &DataNode, options: &XmlGenerateOptions) -> Result<XmlGenerateOutcome> {
    if matches!(root.kind, DataNodeKind::Table { .. }) {
        return Err(xml_err(
            "root source item of XML GENERATE cannot be a table",
        ));
    }
    let ns = validate_namespace(options)?;
    let Some(rendered_root) = render_node(root, options, &ns, NodeDepth::Root)? else {
        return Ok(XmlGenerateOutcome {
            xml: String::new(),
            count: ByteCount::ZERO,
            xml_code: XmlCode::ZERO,
        });
    };

    let mut out = String::with_capacity(rendered_root.len() + XML_DECLARATION_HEADER.len());
    if options.declaration_mode == XmlDeclarationMode::Include {
        out.push_str(XML_DECLARATION_HEADER);
    }
    out.push_str(&rendered_root);
    let count = ByteCount::new(out.len());
    Ok(XmlGenerateOutcome {
        xml: out,
        count,
        xml_code: XmlCode::ZERO,
    })
}

/// Generates XML into a fixed-length COBOL destination byte slice, preserving trailing buffer
/// bytes beyond `COUNT IN` and setting [`XmlCode::BUFFER_OVERFLOW`] (`400`) on truncation.
///
/// # Arguments
/// * `root` - Source COBOL group or elementary [`DataNode`].
/// * `options` - `XML GENERATE` configuration options.
/// * `buffer` - Destination COBOL alphanumeric byte slice.
///
/// # Errors
/// Propagates any structural or character error from [`xml_generate`].
pub fn xml_generate_into_buffer(
    root: &DataNode,
    options: &XmlGenerateOptions,
    buffer: &mut [u8],
) -> Result<XmlGenerateOutcome> {
    let mut outcome = xml_generate(root, options)?;
    let (written, fit) = copy_into_cobol_buffer(&outcome.xml, buffer);
    outcome.count = written;
    if fit == BufferFit::Overflowed {
        outcome.xml_code = XmlCode::BUFFER_OVERFLOW;
    }
    Ok(outcome)
}

/// Validates `NAMESPACE` and `NAMESPACE-PREFIX` options.
///
/// # Arguments
/// * `options` - `XML GENERATE` options to validate.
fn validate_namespace(options: &XmlGenerateOptions) -> Result<ResolvedNamespace<'_>> {
    let uri = options
        .namespace_uri
        .as_deref()
        .map(trim_trailing_spaces)
        .filter(|s| !s.is_empty());
    let prefix = options
        .namespace_prefix
        .as_deref()
        .map(trim_trailing_spaces)
        .filter(|s| !s.is_empty());

    if let Some(u) = uri
        && !u.chars().all(is_valid_xml_char)
    {
        return Err(xml_err("NAMESPACE URI contains invalid XML 1.0 character"));
    }
    if let Some(p) = prefix {
        if uri.is_none() {
            return Err(xml_err(
                "NAMESPACE-PREFIX cannot be specified without a non-empty NAMESPACE URI",
            ));
        }
        if !is_valid_xml_ncname(p) || p == "xml" || p == "xmlns" {
            return Err(xml_err(&format!("invalid XML NAMESPACE-PREFIX `{p}`")));
        }
    }
    Ok(ResolvedNamespace { uri, prefix })
}

/// Renders a group or elementary [`DataNode`] into an XML element string, or `None` if suppressed.
///
/// # Arguments
/// * `node` - Source [`DataNode`] to render.
/// * `options` - Active `XML GENERATE` options.
/// * `ns` - Validated namespace configuration.
/// * `depth` - Root vs nested placement.
fn render_node(
    node: &DataNode,
    options: &XmlGenerateOptions,
    ns: &ResolvedNamespace<'_>,
    depth: NodeDepth,
) -> Result<Option<String>> {
    let local_name = resolve_xml_tag_name(node, options)?;
    let qtag = match ns.prefix {
        Some(pfx) => format!("{pfx}:{local_name}"),
        None => local_name.to_owned(),
    };

    match &node.kind {
        DataNodeKind::Elementary { category, value } => {
            if should_suppress_item(
                &options.suppress_rules,
                node.name.as_str(),
                Some(*category),
                value,
            ) {
                return Ok(None);
            }
            let formatted = format_xml_scalar(*category, value)?;
            let mut elem = String::new();
            elem.push('<');
            elem.push_str(&qtag);
            if depth == NodeDepth::Root {
                append_xmlns_declaration(&mut elem, ns);
            }
            elem.push('>');
            escape_xml_content(&mut elem, &formatted);
            elem.push_str("</");
            elem.push_str(&qtag);
            elem.push('>');
            Ok(Some(elem))
        }
        DataNodeKind::Group { children } => {
            render_group_node(node, children, &qtag, options, ns, depth)
        }
        DataNodeKind::Table { occurrences } => {
            if should_suppress_item(&options.suppress_rules, node.name.as_str(), None, "") {
                return Ok(None);
            }
            let mut joined = String::new();
            for occ in occurrences {
                if let Some(fragment) = render_node(occ, options, ns, NodeDepth::Nested)? {
                    joined.push_str(&fragment);
                }
            }
            if joined.is_empty() {
                Ok(None)
            } else {
                Ok(Some(joined))
            }
        }
    }
}

/// Renders a group [`DataNode`], partitioning subordinate items into attributes and body content.
///
/// # Arguments
/// * `node` - Enclosing group node.
/// * `children` - Ordered child nodes of `node`.
/// * `qtag` - Qualified XML tag name for `node`.
/// * `options` - Active `XML GENERATE` options.
/// * `ns` - Validated namespace configuration.
/// * `depth` - Root vs nested placement.
fn render_group_node(
    node: &DataNode,
    children: &[DataNode],
    qtag: &str,
    options: &XmlGenerateOptions,
    ns: &ResolvedNamespace<'_>,
    depth: NodeDepth,
) -> Result<Option<String>> {
    if should_suppress_item(&options.suppress_rules, node.name.as_str(), None, "") {
        return Ok(None);
    }

    let mut attr_buf = String::new();
    let mut body_buf = String::new();
    let mut emitted_any = false;

    for child in children {
        let placement = effective_placement(child, options.attribute_mode)?;
        match (&child.kind, placement) {
            (DataNodeKind::Elementary { category, value }, XmlNodePlacement::Attribute) => {
                if !should_suppress_item(
                    &options.suppress_rules,
                    child.name.as_str(),
                    Some(*category),
                    value,
                ) {
                    let attr_name = resolve_xml_tag_name(child, options)?;
                    let formatted = format_xml_scalar(*category, value)?;
                    attr_buf.push(' ');
                    attr_buf.push_str(attr_name);
                    attr_buf.push('=');
                    attr_buf.push(CH_DQUOTE);
                    escape_xml_attribute(&mut attr_buf, &formatted);
                    attr_buf.push(CH_DQUOTE);
                    emitted_any = true;
                }
            }
            (DataNodeKind::Elementary { category, value }, XmlNodePlacement::Content) => {
                if !should_suppress_item(
                    &options.suppress_rules,
                    child.name.as_str(),
                    Some(*category),
                    value,
                ) {
                    let formatted = format_xml_scalar(*category, value)?;
                    escape_xml_content(&mut body_buf, &formatted);
                    emitted_any = true;
                }
            }
            _ => {
                if let Some(child_xml) = render_node(child, options, ns, NodeDepth::Nested)? {
                    body_buf.push_str(&child_xml);
                    emitted_any = true;
                }
            }
        }
    }

    if !children.is_empty() && !emitted_any && depth == NodeDepth::Nested {
        return Ok(None);
    }

    let mut out = String::new();
    out.push('<');
    out.push_str(qtag);
    if depth == NodeDepth::Root {
        append_xmlns_declaration(&mut out, ns);
    }
    out.push_str(&attr_buf);
    out.push('>');
    out.push_str(&body_buf);
    out.push_str("</");
    out.push_str(qtag);
    out.push('>');
    Ok(Some(out))
}

/// Determines the effective [`XmlNodePlacement`] of `child`, rejecting invalid non-elementary overrides.
///
/// # Arguments
/// * `child` - Subordinate [`DataNode`] to classify.
/// * `mode` - Statement-level [`XmlAttributeMode`].
fn effective_placement(child: &DataNode, mode: XmlAttributeMode) -> Result<XmlNodePlacement> {
    match (&child.kind, child.xml_placement) {
        (DataNodeKind::Elementary { .. }, XmlNodePlacement::Default) => match mode {
            XmlAttributeMode::ElementsOnly => Ok(XmlNodePlacement::Element),
            XmlAttributeMode::WithAttributes => Ok(XmlNodePlacement::Attribute),
        },
        (DataNodeKind::Elementary { .. }, explicit) => Ok(explicit),
        (
            DataNodeKind::Group { .. } | DataNodeKind::Table { .. },
            XmlNodePlacement::Default | XmlNodePlacement::Element,
        ) => Ok(XmlNodePlacement::Element),
        (
            DataNodeKind::Group { .. } | DataNodeKind::Table { .. },
            XmlNodePlacement::Attribute | XmlNodePlacement::Content,
        ) => Err(xml_err(&format!(
            "non-elementary item `{}` cannot have ATTRIBUTE or CONTENT XML placement",
            child.name.as_str()
        ))),
    }
}

/// Resolves and validates the local XML tag/attribute name for `node`.
///
/// # Arguments
/// * `node` - Source [`DataNode`] whose identifier is being resolved.
/// * `options` - Active `XML GENERATE` options containing `NAME OF` mappings.
fn resolve_xml_tag_name<'a>(
    node: &'a DataNode,
    options: &'a XmlGenerateOptions,
) -> Result<&'a str> {
    let candidate = match resolve_name_override(&options.name_mappings, node.name.as_str()) {
        Some(NameOverride::Rename(alias)) => alias.as_str(),
        Some(NameOverride::Omitted) => {
            return Err(xml_err(&format!(
                "NAME OF `{}` IS OMITTED is not permitted in XML GENERATE",
                node.name.as_str()
            )));
        }
        None => node.name.as_str(),
    };
    if !is_valid_xml_ncname(candidate) {
        return Err(xml_err(&format!("invalid XML local name `{candidate}`")));
    }
    Ok(candidate)
}

/// Formats and validates a COBOL scalar value for XML emission.
///
/// # Arguments
/// * `category` - COBOL [`ValueCategory`] of the scalar item.
/// * `raw` - Raw value string.
fn format_xml_scalar(category: ValueCategory, raw: &str) -> Result<String> {
    let formatted = match category {
        ValueCategory::Numeric => normalize_cobol_number("XML", raw)?,
        ValueCategory::Alphanumeric | ValueCategory::National => {
            trim_trailing_spaces(raw).to_owned()
        }
    };
    if !formatted.chars().all(is_valid_xml_char) {
        return Err(xml_err(
            "elementary value contains illegal XML 1.0 control character",
        ));
    }
    Ok(formatted)
}

/// Appends the root `xmlns` or `xmlns:prefix` attribute declaration when a namespace URI is active.
///
/// # Arguments
/// * `buf` - Destination tag buffer.
/// * `ns` - Validated namespace configuration.
fn append_xmlns_declaration(buf: &mut String, ns: &ResolvedNamespace<'_>) {
    if let Some(uri) = ns.uri {
        match ns.prefix {
            Some(pfx) => {
                buf.push_str(" xmlns:");
                buf.push_str(pfx);
                buf.push('=');
            }
            None => {
                buf.push_str(" xmlns=");
            }
        }
        buf.push(CH_DQUOTE);
        escape_xml_attribute(buf, uri);
        buf.push(CH_DQUOTE);
    }
}

/// Appends XML-escaped element character content (`&`, `<`, `>`) to `buf`.
///
/// # Arguments
/// * `buf` - Output XML buffer.
/// * `text` - Validated UTF-8 character text.
fn escape_xml_content(buf: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            other => buf.push(other),
        }
    }
}

/// Appends XML-escaped attribute character content (`&`, `<`, `>`, `"`, `'`) to `buf`.
///
/// # Arguments
/// * `buf` - Output XML attribute buffer.
/// * `text` - Validated UTF-8 attribute value.
fn escape_xml_attribute(buf: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            CH_DQUOTE => buf.push_str("&quot;"),
            CH_SQUOTE => buf.push_str("&apos;"),
            other => buf.push(other),
        }
    }
}

/// Returns `true` if `name` is a non-empty XML 1.0 `NCName` (valid XML `Name` containing no `':'`).
///
/// # Arguments
/// * `name` - Candidate XML local name or prefix.
#[must_use]
pub fn is_valid_xml_ncname(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first == ':' || !is_xml_name_start(first) {
        return false;
    }
    chars.all(|ch| ch != ':' && is_xml_name_char(ch))
}

/// Constructs an XML [`CoblangError::XmlJsonProcess`] error at offset `0`.
///
/// # Arguments
/// * `msg` - Diagnostic explanation.
fn xml_err(msg: &str) -> CoblangError {
    CoblangError::XmlJsonProcess {
        kind: "XML",
        offset: 0,
        message: msg.to_owned(),
    }
}
