//! Strongly-typed domain identifiers, special-register codes, suppression rules, boolean mappings,
//! and hierarchical COBOL data trees for `XML` and `JSON` processing (`Milestone M20`).

use coblang_common::{CoblangError, Result};

/// Fixed byte width of the COBOL `XML-EVENT` special register (`PIC X(30)`).
pub const XML_EVENT_REGISTER_LEN: usize = 30;

/// Strongly-typed COBOL `XML-EVENT` special-register event classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlEventKind {
    /// `"START-OF-DOCUMENT"` — emitted once before any XML content is scanned.
    StartOfDocument,
    /// `"VERSION-INFORMATION"` — emitted for `version="..."` inside `<?xml ...?>`.
    VersionInformation,
    /// `"ENCODING-DECLARATION"` — emitted for `encoding="..."` inside `<?xml ...?>`.
    EncodingDeclaration,
    /// `"STANDALONE-DECLARATION"` — emitted for `standalone="..."` inside `<?xml ...?>`.
    StandaloneDeclaration,
    /// `"DOCUMENT-TYPE-DECLARATION"` — emitted for `<!DOCTYPE ...>`.
    DocumentTypeDeclaration,
    /// `"START-OF-ELEMENT"` — emitted when an opening or empty-element tag begins.
    StartOfElement,
    /// `"ATTRIBUTE-NAME"` — emitted for each non-namespace attribute name on an element.
    AttributeName,
    /// `"ATTRIBUTE-CHARACTERS"` — emitted with the decoded value of the preceding attribute.
    AttributeCharacters,
    /// `"START-OF-CDATA-SECTION"` — emitted at `<![CDATA[`.
    StartOfCdataSection,
    /// `"CONTENT-CHARACTERS"` — emitted for element character data or CDATA payload.
    ContentCharacters,
    /// `"END-OF-CDATA-SECTION"` — emitted at `]]>`.
    EndOfCdataSection,
    /// `"COMMENT"` — emitted for `<!-- ... -->` comment text.
    Comment,
    /// `"PROCESSING-INSTRUCTION-TARGET"` — emitted for `<?target ...?>` target identifier.
    ProcessingInstructionTarget,
    /// `"PROCESSING-INSTRUCTION-DATA"` — emitted for `<?target data?>` payload text.
    ProcessingInstructionData,
    /// `"END-OF-ELEMENT"` — emitted when an element closes (`</tag>` or `<tag/>`).
    EndOfElement,
    /// `"EXCEPTION"` — emitted when well-formedness or namespace validation fails.
    Exception,
    /// `"END-OF-DOCUMENT"` — emitted once after the root element and trailing misc complete.
    EndOfDocument,
}

impl XmlEventKind {
    /// Returns the canonical COBOL `XML-EVENT` uppercase string token.
    #[must_use]
    pub const fn as_cobol_str(self) -> &'static str {
        match self {
            Self::StartOfDocument => "START-OF-DOCUMENT",
            Self::VersionInformation => "VERSION-INFORMATION",
            Self::EncodingDeclaration => "ENCODING-DECLARATION",
            Self::StandaloneDeclaration => "STANDALONE-DECLARATION",
            Self::DocumentTypeDeclaration => "DOCUMENT-TYPE-DECLARATION",
            Self::StartOfElement => "START-OF-ELEMENT",
            Self::AttributeName => "ATTRIBUTE-NAME",
            Self::AttributeCharacters => "ATTRIBUTE-CHARACTERS",
            Self::StartOfCdataSection => "START-OF-CDATA-SECTION",
            Self::ContentCharacters => "CONTENT-CHARACTERS",
            Self::EndOfCdataSection => "END-OF-CDATA-SECTION",
            Self::Comment => "COMMENT",
            Self::ProcessingInstructionTarget => "PROCESSING-INSTRUCTION-TARGET",
            Self::ProcessingInstructionData => "PROCESSING-INSTRUCTION-DATA",
            Self::EndOfElement => "END-OF-ELEMENT",
            Self::Exception => "EXCEPTION",
            Self::EndOfDocument => "END-OF-DOCUMENT",
        }
    }

    /// Formats the event token into the fixed 30-byte space-padded `XML-EVENT` register layout.
    #[must_use]
    pub fn to_register_bytes(self) -> [u8; XML_EVENT_REGISTER_LEN] {
        let mut out = [b' '; XML_EVENT_REGISTER_LEN];
        for (dst, src) in out.iter_mut().zip(self.as_cobol_str().as_bytes()) {
            *dst = *src;
        }
        out
    }

    /// Parses a COBOL `XML-EVENT` string (ignoring trailing spaces) into [`XmlEventKind`].
    ///
    /// # Arguments
    /// * `raw` - Raw or space-padded `XML-EVENT` string token.
    ///
    /// # Errors
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw` is not a recognized `XML-EVENT`.
    pub fn from_cobol_str(raw: &str) -> Result<Self> {
        let trimmed = trim_trailing_spaces(raw);
        match trimmed {
            "START-OF-DOCUMENT" => Ok(Self::StartOfDocument),
            "VERSION-INFORMATION" => Ok(Self::VersionInformation),
            "ENCODING-DECLARATION" => Ok(Self::EncodingDeclaration),
            "STANDALONE-DECLARATION" => Ok(Self::StandaloneDeclaration),
            "DOCUMENT-TYPE-DECLARATION" => Ok(Self::DocumentTypeDeclaration),
            "START-OF-ELEMENT" => Ok(Self::StartOfElement),
            "ATTRIBUTE-NAME" => Ok(Self::AttributeName),
            "ATTRIBUTE-CHARACTERS" => Ok(Self::AttributeCharacters),
            "START-OF-CDATA-SECTION" => Ok(Self::StartOfCdataSection),
            "CONTENT-CHARACTERS" => Ok(Self::ContentCharacters),
            "END-OF-CDATA-SECTION" => Ok(Self::EndOfCdataSection),
            "COMMENT" => Ok(Self::Comment),
            "PROCESSING-INSTRUCTION-TARGET" => Ok(Self::ProcessingInstructionTarget),
            "PROCESSING-INSTRUCTION-DATA" => Ok(Self::ProcessingInstructionData),
            "END-OF-ELEMENT" => Ok(Self::EndOfElement),
            "EXCEPTION" => Ok(Self::Exception),
            "END-OF-DOCUMENT" => Ok(Self::EndOfDocument),
            _ => Err(CoblangError::InvalidDomainValue {
                domain_type: "XmlEventKind",
                value: raw.to_owned(),
                reason: "unrecognized COBOL XML-EVENT register name".to_owned(),
            }),
        }
    }
}

/// Strongly-typed wrapper for the COBOL `XML-CODE` special register (`PIC S9(9) COMP-5`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct XmlCode(i32);

impl XmlCode {
    /// Normal completion / continue-parsing code (`0`).
    pub const ZERO: Self = Self(0);
    /// User-requested immediate termination code (`MOVE -1 TO XML-CODE`).
    pub const USER_TERMINATE: Self = Self(-1);
    /// Malformed XML markup or invalid token syntax (`100`).
    pub const MALFORMED_SYNTAX: Self = Self(100);
    /// Unexpected end of XML document inside open construct (`101`).
    pub const UNEXPECTED_EOF: Self = Self(101);
    /// Closing element tag does not match currently open element (`102`).
    pub const MISMATCHED_END_TAG: Self = Self(102);
    /// Duplicate attribute specification on the same start tag (`103`).
    pub const DUPLICATE_ATTRIBUTE: Self = Self(103);
    /// Unknown entity reference or out-of-range character reference (`104`).
    pub const INVALID_ENTITY_OR_CHAR_REF: Self = Self(104);
    /// Undeclared XML namespace prefix on element or attribute (`105`).
    pub const UNBOUND_NAMESPACE_PREFIX: Self = Self(105);
    /// Malformed or misplaced `<?xml ...?>` declaration (`106`).
    pub const INVALID_XML_DECLARATION: Self = Self(106);
    /// Forbidden XML 1.0 control character encountered (`107`).
    pub const INVALID_CHARACTER: Self = Self(107);
    /// Target buffer capacity exceeded during `XML GENERATE` (`400`).
    pub const BUFFER_OVERFLOW: Self = Self(400);
    /// Invalid `NAMESPACE` / `NAMESPACE-PREFIX` or identifier configuration (`417`).
    pub const INVALID_NAMESPACE_CONFIG: Self = Self(417);

    /// Wraps a raw signed 32-bit `XML-CODE` register value.
    ///
    /// # Arguments
    /// * `raw` - Signed 32-bit `XML-CODE` integer.
    #[must_use]
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }

    /// Returns the underlying signed 32-bit `XML-CODE` value.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Returns `true` when `XML-CODE` equals `0`.
    #[must_use]
    pub const fn is_ok(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` when the processing procedure set `XML-CODE` to `-1`.
    #[must_use]
    pub const fn is_user_terminate(self) -> bool {
        self.0 == -1
    }
}

/// Strongly-typed wrapper for the COBOL `JSON-CODE` special register (`PIC S9(9) COMP-5`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JsonCode(i32);

impl JsonCode {
    /// Normal completion (`0`).
    pub const ZERO: Self = Self(0);
    /// Malformed RFC 8259 JSON syntax (`100`).
    pub const SYNTAX_ERROR: Self = Self(100);
    /// Incompatible JSON-to-COBOL or COBOL-to-JSON data conversion (`200`).
    pub const CONVERSION_ERROR: Self = Self(200);
    /// Destination buffer too small during `JSON GENERATE` (`400`).
    pub const BUFFER_OVERFLOW: Self = Self(400);

    /// Wraps a raw signed 32-bit `JSON-CODE` register value.
    ///
    /// # Arguments
    /// * `raw` - Signed 32-bit `JSON-CODE` integer.
    #[must_use]
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }

    /// Returns the underlying signed 32-bit `JSON-CODE` integer.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Returns `true` when `JSON-CODE` equals `0`.
    #[must_use]
    pub const fn is_ok(self) -> bool {
        self.0 == 0
    }
}

/// Strongly-typed wrapper for the COBOL `JSON-STATUS` special register (`PIC S9(9) COMP-5`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JsonStatus(i32);

impl JsonStatus {
    /// Clean completion with no warnings or exceptions (`0`).
    pub const ZERO: Self = Self(0);
    /// JSON object contained one or more keys with no matching COBOL target (`1`).
    pub const UNMAPPED_KEY: Self = Self(1);
    /// Elementary string value exceeded target field `max_chars` and was truncated (`2`).
    pub const TRUNCATED_VALUE: Self = Self(2);
    /// JSON array contained more elements than the COBOL `OCCURS` table capacity (`3`).
    pub const ARRAY_OVERFLOW: Self = Self(3);
    /// JSON value category conflicted with target COBOL field category (`4`).
    pub const INCOMPATIBLE_TYPE: Self = Self(4);
    /// Malformed RFC 8259 JSON document (`100`).
    pub const SYNTAX_ERROR: Self = Self(100);
    /// Target buffer overflow during `JSON GENERATE` (`400`).
    pub const BUFFER_OVERFLOW: Self = Self(400);

    /// Wraps a raw signed 32-bit `JSON-STATUS` register value.
    ///
    /// # Arguments
    /// * `raw` - Signed 32-bit `JSON-STATUS` integer.
    #[must_use]
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }

    /// Returns the underlying signed 32-bit `JSON-STATUS` value.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Returns `true` when `JSON-STATUS` equals `0`.
    #[must_use]
    pub const fn is_ok(self) -> bool {
        self.0 == 0
    }
}

/// Validated byte/character count reported by `XML GENERATE` / `JSON GENERATE` `COUNT IN`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteCount(usize);

impl ByteCount {
    /// Zero generated bytes (`0`).
    pub const ZERO: Self = Self(0);

    /// Wraps a byte/character count.
    ///
    /// # Arguments
    /// * `raw` - Count of generated UTF-8 bytes/characters.
    #[must_use]
    pub const fn new(raw: usize) -> Self {
        Self(raw)
    }

    /// Returns the underlying count as `usize`.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Validated non-empty COBOL data-item identifier used in markup trees and clause mappings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CobolFieldName(String);

impl CobolFieldName {
    /// Validates and constructs a [`CobolFieldName`].
    ///
    /// # Arguments
    /// * `raw` - Candidate COBOL data-item name (trimmed of leading/trailing whitespace).
    ///
    /// # Errors
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw` is empty or whitespace-only.
    pub fn try_new(raw: impl Into<String>) -> Result<Self> {
        let owned = raw.into();
        let trimmed = owned.trim();
        if trimmed.is_empty() {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "CobolFieldName",
                value: owned,
                reason: "field name must not be empty or whitespace".to_owned(),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Borrows the validated field identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Elementary COBOL data category governing XML/JSON formatting and conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueCategory {
    /// Alphanumeric or alphabetic item (`PIC X` / `PIC A`).
    Alphanumeric,
    /// Exact numeric or numeric-edited value (`PIC 9` / `PIC S9` / `COMP*`).
    Numeric,
    /// National UTF-16 character item (`PIC N`).
    National,
}

/// `SUPPRESS` condition predicate for `XML GENERATE`, `JSON GENERATE`, and `JSON PARSE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppressCondition {
    /// Unconditionally omit or ignore the targeted data item.
    Always,
    /// Suppress when the item represents numeric zero (`0`, `+0`, `-0`, `0.00`, `000`).
    WhenZero,
    /// Suppress when the item is empty or contains only space (`' '`) characters.
    WhenSpace,
    /// Suppress when the item is non-empty and contains only `LOW-VALUE` (`'\0'`) characters.
    WhenLowValue,
    /// Suppress when the item is non-empty and contains only `HIGH-VALUE` (`'\u{00FF}'`) characters.
    WhenHighValue,
    /// Suppress when the item is non-empty and contains only double-quote (`'"'`) characters.
    WhenQuote,
    /// Suppress when the item (exact or trailing-space-trimmed) equals the given literal.
    WhenLiteral(String),
}

impl SuppressCondition {
    /// Evaluates whether `raw_value` of `category` satisfies this suppression predicate.
    ///
    /// # Arguments
    /// * `category` - COBOL [`ValueCategory`] of the candidate elementary item.
    /// * `raw_value` - Raw string representation of the item's current storage value.
    #[must_use]
    pub fn matches(&self, category: ValueCategory, raw_value: &str) -> bool {
        match self {
            Self::Always => true,
            Self::WhenZero => match category {
                ValueCategory::Numeric => is_zero_numeric_str(raw_value),
                ValueCategory::Alphanumeric | ValueCategory::National => {
                    !raw_value.is_empty() && raw_value.bytes().all(|b| b == b'0')
                }
            },
            Self::WhenSpace => raw_value.bytes().all(|b| b == b' '),
            Self::WhenLowValue => !raw_value.is_empty() && raw_value.bytes().all(|b| b == 0),
            Self::WhenHighValue => {
                !raw_value.is_empty() && raw_value.chars().all(|ch| ch == '\u{00FF}')
            }
            Self::WhenQuote => !raw_value.is_empty() && raw_value.bytes().all(|b| b == b'"'),
            Self::WhenLiteral(expected) => {
                raw_value == expected
                    || trim_trailing_spaces(raw_value) == trim_trailing_spaces(expected)
            }
        }
    }
}

/// Category filter for generic `SUPPRESS EVERY [NUMERIC | NONNUMERIC]` clauses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuppressCategoryFilter {
    /// Matches elementary items of any [`ValueCategory`].
    Any,
    /// Matches only [`ValueCategory::Numeric`] items.
    NumericOnly,
    /// Matches [`ValueCategory::Alphanumeric`] and [`ValueCategory::National`] items.
    NonNumericOnly,
}

impl SuppressCategoryFilter {
    /// Returns `true` when `category` satisfies this filter.
    ///
    /// # Arguments
    /// * `category` - Candidate item [`ValueCategory`].
    #[must_use]
    pub const fn accepts(self, category: ValueCategory) -> bool {
        match self {
            Self::Any => true,
            Self::NumericOnly => matches!(category, ValueCategory::Numeric),
            Self::NonNumericOnly => {
                matches!(
                    category,
                    ValueCategory::Alphanumeric | ValueCategory::National
                )
            }
        }
    }
}

/// Selector identifying which field(s) a [`SuppressRule`] applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppressTarget {
    /// Targets a specific COBOL field by name.
    Field(CobolFieldName),
    /// Targets every elementary item matching the category filter.
    Every(SuppressCategoryFilter),
}

/// Complete `SUPPRESS` clause rule pairing a target selector with a [`SuppressCondition`].
#[derive(Debug, Clone)]
pub struct SuppressRule {
    /// Field or category target selector.
    pub target: SuppressTarget,
    /// Suppression predicate evaluated against candidate items.
    pub condition: SuppressCondition,
}

impl SuppressRule {
    /// Creates a field-specific [`SuppressRule`].
    ///
    /// # Arguments
    /// * `field` - Target COBOL field identifier.
    /// * `condition` - Predicate determining when `field` is suppressed.
    #[must_use]
    pub const fn for_field(field: CobolFieldName, condition: SuppressCondition) -> Self {
        Self {
            target: SuppressTarget::Field(field),
            condition,
        }
    }

    /// Creates a generic category-wide [`SuppressRule`].
    ///
    /// # Arguments
    /// * `filter` - Category filter (`Any`, `NumericOnly`, `NonNumericOnly`).
    /// * `condition` - Predicate determining when matching items are suppressed.
    #[must_use]
    pub const fn for_every(filter: SuppressCategoryFilter, condition: SuppressCondition) -> Self {
        Self {
            target: SuppressTarget::Every(filter),
            condition,
        }
    }
}

/// `NAME OF field IS "alias" | OMITTED` specification for XML and JSON verbs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameOverride {
    /// Replaces the emitted/matched tag or property key with a custom string.
    Rename(String),
    /// Omits the enclosing JSON object wrapper (`NAME OF group IS OMITTED`).
    Omitted,
}

impl NameOverride {
    /// Validates and constructs a [`NameOverride::Rename`] entry.
    ///
    /// # Arguments
    /// * `alias` - Replacement XML tag or JSON property name (must not be empty).
    ///
    /// # Errors
    /// Returns [`CoblangError::InvalidDomainValue`] if `alias` is empty.
    pub fn try_rename(alias: impl Into<String>) -> Result<Self> {
        let owned = alias.into();
        if owned.is_empty() {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "NameOverride",
                value: owned,
                reason: "renamed markup identifier must not be empty".to_owned(),
            });
        }
        Ok(Self::Rename(owned))
    }
}

/// Binds a [`CobolFieldName`] to a [`NameOverride`] (`NAME OF` clause entry).
#[derive(Debug, Clone)]
pub struct NameMapping {
    /// COBOL field whose markup name is overridden.
    pub field_name: CobolFieldName,
    /// Replacement alias or `OMITTED` wrapper directive.
    pub override_spec: NameOverride,
}

impl NameMapping {
    /// Constructs a [`NameMapping`] entry.
    ///
    /// # Arguments
    /// * `field_name` - Target COBOL field identifier.
    /// * `override_spec` - Alias or `OMITTED` specification.
    #[must_use]
    pub const fn new(field_name: CobolFieldName, override_spec: NameOverride) -> Self {
        Self {
            field_name,
            override_spec,
        }
    }
}

/// `CONVERTING field TO/FROM BOOLEAN` mapping between COBOL storage literals and JSON booleans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonBooleanMapping {
    /// Target COBOL elementary field identifier.
    pub field_name: CobolFieldName,
    /// COBOL value representing JSON `true` (e.g. level-88 `VALUE` `"Y"` or `"1"`).
    pub true_value: String,
    /// COBOL value representing JSON `false` (e.g. `WHEN SET TO FALSE` `"N"` or `"0"`).
    pub false_value: String,
}

impl JsonBooleanMapping {
    /// Creates a custom [`JsonBooleanMapping`].
    ///
    /// # Arguments
    /// * `field_name` - Target COBOL elementary field identifier.
    /// * `true_value` - Storage string corresponding to JSON `true`.
    /// * `false_value` - Storage string corresponding to JSON `false`.
    #[must_use]
    pub fn new(
        field_name: CobolFieldName,
        true_value: impl Into<String>,
        false_value: impl Into<String>,
    ) -> Self {
        Self {
            field_name,
            true_value: true_value.into(),
            false_value: false_value.into(),
        }
    }

    /// Creates a `"Y"` / `"N"` [`JsonBooleanMapping`] preset.
    ///
    /// # Arguments
    /// * `field_name` - Target COBOL elementary field identifier.
    #[must_use]
    pub fn yes_no(field_name: CobolFieldName) -> Self {
        Self::new(field_name, "Y", "N")
    }

    /// Creates a `"1"` / `"0"` [`JsonBooleanMapping`] preset.
    ///
    /// # Arguments
    /// * `field_name` - Target COBOL elementary field identifier.
    #[must_use]
    pub fn one_zero(field_name: CobolFieldName) -> Self {
        Self::new(field_name, "1", "0")
    }

    /// Creates a `"T"` / `"F"` [`JsonBooleanMapping`] preset.
    ///
    /// # Arguments
    /// * `field_name` - Target COBOL elementary field identifier.
    #[must_use]
    pub fn true_false_char(field_name: CobolFieldName) -> Self {
        Self::new(field_name, "T", "F")
    }

    /// Returns `true` if `raw_value` matches this mapping's `true_value` under COBOL comparison.
    ///
    /// # Arguments
    /// * `raw_value` - Current storage string of the mapped COBOL field.
    #[must_use]
    pub fn matches_true(&self, raw_value: &str) -> bool {
        raw_value == self.true_value
            || trim_trailing_spaces(raw_value) == trim_trailing_spaces(&self.true_value)
    }
}

/// Per-field `TYPE OF field IS {ATTRIBUTE | ELEMENT | CONTENT}` placement in `XML GENERATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlNodePlacement {
    /// Uses the statement-level `WITH ATTRIBUTES` / default element policy.
    Default,
    /// Forces emission as an XML attribute on the parent element (`TYPE IS ATTRIBUTE`).
    Attribute,
    /// Forces emission as a child XML element (`TYPE IS ELEMENT`).
    Element,
    /// Emits character data directly inside the parent element (`TYPE IS CONTENT`).
    Content,
}

/// Structural payload of a read-only COBOL [`DataNode`] for `XML GENERATE` / `JSON GENERATE`.
#[derive(Debug, Clone)]
pub enum DataNodeKind {
    /// Elementary scalar value (`PIC X`, `PIC 9`, `PIC N`).
    Elementary {
        /// COBOL data category.
        category: ValueCategory,
        /// Current raw value string.
        value: String,
    },
    /// Group record containing ordered subordinate items.
    Group {
        /// Ordered child nodes.
        children: Vec<DataNode>,
    },
    /// `OCCURS` table containing zero or more homogeneous occurrences.
    Table {
        /// Active table element occurrences (`1..=current_odo`).
        occurrences: Vec<DataNode>,
    },
}

/// Read-only hierarchical COBOL data node consumed by `XML GENERATE` and `JSON GENERATE`.
#[derive(Debug, Clone)]
pub struct DataNode {
    /// Declared COBOL identifier of this node.
    pub name: CobolFieldName,
    /// Optional `TYPE OF` XML placement override.
    pub xml_placement: XmlNodePlacement,
    /// Elementary, group, or `OCCURS` table payload.
    pub kind: DataNodeKind,
}

impl DataNode {
    /// Constructs an elementary [`DataNode`].
    ///
    /// # Arguments
    /// * `name` - Validated COBOL field name.
    /// * `category` - Elementary [`ValueCategory`].
    /// * `value` - Current raw string value.
    #[must_use]
    pub fn elementary(
        name: CobolFieldName,
        category: ValueCategory,
        value: impl Into<String>,
    ) -> Self {
        Self {
            name,
            xml_placement: XmlNodePlacement::Default,
            kind: DataNodeKind::Elementary {
                category,
                value: value.into(),
            },
        }
    }

    /// Constructs a group [`DataNode`].
    ///
    /// # Arguments
    /// * `name` - Validated COBOL group name.
    /// * `children` - Ordered subordinate data nodes.
    #[must_use]
    pub const fn group(name: CobolFieldName, children: Vec<Self>) -> Self {
        Self {
            name,
            xml_placement: XmlNodePlacement::Default,
            kind: DataNodeKind::Group { children },
        }
    }

    /// Constructs an `OCCURS` table [`DataNode`].
    ///
    /// # Arguments
    /// * `name` - Validated COBOL table item name.
    /// * `occurrences` - Active occurrence nodes.
    #[must_use]
    pub const fn table(name: CobolFieldName, occurrences: Vec<Self>) -> Self {
        Self {
            name,
            xml_placement: XmlNodePlacement::Default,
            kind: DataNodeKind::Table { occurrences },
        }
    }

    /// Attaches an explicit [`XmlNodePlacement`] (`TYPE OF`) override to `self`.
    ///
    /// # Arguments
    /// * `placement` - Desired XML node placement (`Attribute`, `Element`, or `Content`).
    #[must_use]
    pub const fn with_xml_placement(mut self, placement: XmlNodePlacement) -> Self {
        self.xml_placement = placement;
        self
    }
}

/// Tracks whether a [`ParseTargetNode`] elementary item was modified during `JSON PARSE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldAssignState {
    /// Field was absent, suppressed, or `null` in the JSON source (original COBOL value kept).
    Unmodified,
    /// Field was matched and populated from a non-null JSON value.
    Assigned,
}

/// Structural payload of a mutable target node populated by `JSON PARSE`.
#[derive(Debug, Clone)]
pub enum ParseTargetKind {
    /// Mutable elementary receiving item.
    Elementary {
        /// Receiving COBOL data category.
        category: ValueCategory,
        /// Current/resulting character representation.
        value: String,
        /// Optional maximum character capacity (truncates right and sets [`JsonStatus::TRUNCATED_VALUE`]).
        max_chars: Option<usize>,
        /// Assignment state after `JSON PARSE`.
        assigned: FieldAssignState,
    },
    /// Mutable group receiving item.
    Group {
        /// Subordinate receiving items.
        children: Vec<ParseTargetNode>,
    },
    /// Fixed/bounded `OCCURS` table receiving item.
    Table {
        /// Pre-allocated occurrence slots populated up to capacity.
        elements: Vec<ParseTargetNode>,
        /// Count of JSON array elements stored into `elements`.
        populated_count: usize,
    },
}

/// Mutable COBOL receiving hierarchy populated in-place by `JSON PARSE`.
#[derive(Debug, Clone)]
pub struct ParseTargetNode {
    /// Declared COBOL identifier of this receiving item.
    pub name: CobolFieldName,
    /// Elementary, group, or `OCCURS` table receiving storage.
    pub kind: ParseTargetKind,
}

impl ParseTargetNode {
    /// Creates an elementary receiving node with an initial COBOL storage value.
    ///
    /// # Arguments
    /// * `name` - Validated COBOL field name.
    /// * `category` - Receiving [`ValueCategory`].
    /// * `initial_value` - Existing value preserved if the JSON input omits or nulls the key.
    /// * `max_chars` - Optional character length limit (`PIC X(n)`).
    #[must_use]
    pub fn elementary(
        name: CobolFieldName,
        category: ValueCategory,
        initial_value: impl Into<String>,
        max_chars: Option<usize>,
    ) -> Self {
        Self {
            name,
            kind: ParseTargetKind::Elementary {
                category,
                value: initial_value.into(),
                max_chars,
                assigned: FieldAssignState::Unmodified,
            },
        }
    }

    /// Creates a group receiving node.
    ///
    /// # Arguments
    /// * `name` - Validated COBOL group name.
    /// * `children` - Subordinate receiving nodes.
    #[must_use]
    pub const fn group(name: CobolFieldName, children: Vec<Self>) -> Self {
        Self {
            name,
            kind: ParseTargetKind::Group { children },
        }
    }

    /// Creates an `OCCURS` table receiving node with pre-allocated element prototypes.
    ///
    /// # Arguments
    /// * `name` - Validated COBOL table name.
    /// * `elements` - Pre-allocated occurrence target nodes.
    #[must_use]
    pub const fn table(name: CobolFieldName, elements: Vec<Self>) -> Self {
        Self {
            name,
            kind: ParseTargetKind::Table {
                elements,
                populated_count: 0,
            },
        }
    }

    /// Looks up a direct child of a group node by COBOL field name.
    ///
    /// # Arguments
    /// * `child_name` - Unqualified COBOL child identifier to locate.
    #[must_use]
    pub fn child(&self, child_name: &str) -> Option<&Self> {
        match &self.kind {
            ParseTargetKind::Group { children } => {
                children.iter().find(|c| c.name.as_str() == child_name)
            }
            ParseTargetKind::Elementary { .. } | ParseTargetKind::Table { .. } => None,
        }
    }

    /// Returns the elementary string value if `self` is [`ParseTargetKind::Elementary`].
    #[must_use]
    pub fn elementary_value(&self) -> Option<&str> {
        match &self.kind {
            ParseTargetKind::Elementary { value, .. } => Some(value.as_str()),
            ParseTargetKind::Group { .. } | ParseTargetKind::Table { .. } => None,
        }
    }

    /// Returns the [`FieldAssignState`] if `self` is [`ParseTargetKind::Elementary`].
    #[must_use]
    pub const fn assign_state(&self) -> Option<FieldAssignState> {
        match &self.kind {
            ParseTargetKind::Elementary { assigned, .. } => Some(*assigned),
            ParseTargetKind::Group { .. } | ParseTargetKind::Table { .. } => None,
        }
    }

    /// Returns `(populated_count, elements_slice)` if `self` is [`ParseTargetKind::Table`].
    #[must_use]
    pub fn table_elements(&self) -> Option<(usize, &[Self])> {
        match &self.kind {
            ParseTargetKind::Table {
                elements,
                populated_count,
            } => Some((*populated_count, elements.as_slice())),
            ParseTargetKind::Elementary { .. } | ParseTargetKind::Group { .. } => None,
        }
    }
}

/// Strips trailing ASCII space (`0x20`) padding without removing tabs or newlines.
///
/// # Arguments
/// * `raw` - Input string slice potentially padded on the right with COBOL spaces.
#[must_use]
pub fn trim_trailing_spaces(raw: &str) -> &str {
    raw.trim_end_matches(' ')
}

/// Encodes a UTF-8 string into UTF-16 code units for `XML-NTEXT` / `NATIONAL` special registers.
///
/// # Arguments
/// * `text` - UTF-8 string slice to encode.
#[must_use]
pub fn encode_utf16_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Checks whether a COBOL numeric text representation denotes algebraic zero.
///
/// # Arguments
/// * `raw` - Candidate numeric text (e.g. `"0"`, `"-0.00"`, `" +000 "`, `"00-"`).
#[must_use]
pub fn is_zero_numeric_str(raw: &str) -> bool {
    let trimmed = raw.trim();
    let unsigned = trimmed
        .strip_prefix(['+', '-'])
        .or_else(|| trimmed.strip_suffix(['+', '-']))
        .unwrap_or(trimmed);
    if unsigned.is_empty() {
        return false;
    }
    let mut saw_digit = false;
    let mut saw_dot = false;
    for b in unsigned.bytes() {
        if b == b'0' {
            saw_digit = true;
        } else if b == b'.' && !saw_dot {
            saw_dot = true;
        } else {
            return false;
        }
    }
    saw_digit
}

/// Validates and normalizes a COBOL numeric string into canonical RFC 8259 / XML decimal form.
///
/// Handles leading or trailing signs (`+12`, `-12`, `12-`, `12+`), strips redundant leading
/// integer zeros (`0042.50` -> `42.50`), inserts `0` before a leading decimal point (`.5` -> `0.5`),
/// and canonicalizes negative zero (`-0.00` -> `0.00`).
///
/// # Arguments
/// * `kind` - `"XML"` or `"JSON"` subsystem tag for error reporting.
/// * `raw` - Raw COBOL numeric text to normalize.
///
/// # Errors
/// Returns [`CoblangError::XmlJsonProcess`] if `raw` is not a valid signed decimal representation.
pub fn normalize_cobol_number(kind: &'static str, raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let (negative, unsigned) = if let Some(rest) = trimmed.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (false, rest)
    } else if let Some(rest) = trimmed.strip_suffix('-') {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_suffix('+') {
        (false, rest)
    } else {
        (false, trimmed)
    };

    let (int_raw, frac_opt) = match unsigned.split_once('.') {
        Some((lhs, rhs)) => (lhs, Some(rhs)),
        None => (unsigned, None),
    };

    if (int_raw.is_empty() && frac_opt.is_none_or(str::is_empty))
        || !int_raw.bytes().all(|b| b.is_ascii_digit())
        || frac_opt.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(CoblangError::XmlJsonProcess {
            kind,
            offset: 0,
            message: format!("invalid COBOL numeric value `{raw}`"),
        });
    }

    let int_trimmed = int_raw.trim_start_matches('0');
    let int_norm = if int_trimmed.is_empty() {
        "0"
    } else {
        int_trimmed
    };

    let all_zero = int_norm == "0" && frac_opt.is_none_or(|frac| frac.bytes().all(|b| b == b'0'));

    let mut out = String::new();
    if negative && !all_zero {
        out.push('-');
    }
    out.push_str(int_norm);
    if let Some(frac) = frac_opt {
        out.push('.');
        out.push_str(frac);
    }
    Ok(out)
}

/// Evaluates whether a field should be omitted under an ordered list of [`SuppressRule`]s.
///
/// # Arguments
/// * `rules` - Active `SUPPRESS` clause rules.
/// * `field_name` - Unqualified COBOL field name of the candidate item.
/// * `category` - Optional [`ValueCategory`] (`Some` for elementary items, `None` for groups/tables).
/// * `raw_value` - Raw elementary value string (empty for groups/tables).
#[must_use]
pub fn should_suppress_item(
    rules: &[SuppressRule],
    field_name: &str,
    category: Option<ValueCategory>,
    raw_value: &str,
) -> bool {
    for rule in rules {
        match &rule.target {
            SuppressTarget::Field(target_name) if target_name.as_str() == field_name => {
                match category {
                    Some(cat) => {
                        if rule.condition.matches(cat, raw_value) {
                            return true;
                        }
                    }
                    None => {
                        if matches!(rule.condition, SuppressCondition::Always) {
                            return true;
                        }
                    }
                }
            }
            SuppressTarget::Field(_) => {}
            SuppressTarget::Every(filter) => {
                if let Some(cat) = category
                    && filter.accepts(cat)
                    && rule.condition.matches(cat, raw_value)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Resolves any configured [`NameOverride`] for `field_name`.
///
/// # Arguments
/// * `mappings` - Active `NAME OF` mappings.
/// * `field_name` - Candidate COBOL field name.
#[must_use]
pub fn resolve_name_override<'a>(
    mappings: &'a [NameMapping],
    field_name: &str,
) -> Option<&'a NameOverride> {
    mappings
        .iter()
        .find(|m| m.field_name.as_str() == field_name)
        .map(|m| &m.override_spec)
}

///Copies generated UTF-8 markup bytes into a fixed-length COBOL buffer slice without touching
/// trailing bytes beyond the generated prefix, returning `(written_count, overflowed)`.
///
/// # Arguments
/// * `generated` - Generated UTF-8 XML or JSON text.
/// * `buffer` - Mutable fixed-size COBOL destination byte slice.
#[must_use]
pub fn copy_into_cobol_buffer(generated: &str, buffer: &mut [u8]) -> (ByteCount, BufferFit) {
    let src = generated.as_bytes();
    let written = src.len().min(buffer.len());
    for (dst, byte) in buffer.iter_mut().zip(src.iter().copied()) {
        *dst = byte;
    }
    let fit = if src.len() > buffer.len() {
        BufferFit::Overflowed
    } else {
        BufferFit::Fits
    };
    (ByteCount::new(written), fit)
}

/// Outcome of fitting generated markup into a fixed-capacity COBOL destination buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferFit {
    /// Entire generated payload fit within the destination slice.
    Fits,
    /// Generated payload exceeded destination capacity and was truncated at buffer end.
    Overflowed,
}
