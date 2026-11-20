//! Strongly-typed Milestone M20 HIR nodes for `XML`, `JSON`, and Object-Oriented COBOL (`CLASS-ID` / `INVOKE`).

use coblang_common::{DataItemId, ProcedureId, SourceSpan};

use crate::{HirCallArg, HirDataItem, HirExpr, HirPlace, HirStmt, HirType};

/// `[WITH] XML-DECLARATION` mode on [`HirXmlGenerateStmt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirXmlDeclMode {
    /// Do not prepend XML declaration.
    Omit,
    /// Prepend XML 1.0 version declaration header.
    Include,
}

/// `[WITH] ATTRIBUTES` mode on [`HirXmlGenerateStmt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirXmlAttrMode {
    /// Elementary children default to child elements.
    ElementsOnly,
    /// Elementary children default to XML attributes (`WITH ATTRIBUTES`).
    WithAttributes,
}

/// Decimal point character mode (`DPC-IN-DATA`) on `XML GENERATE` / `JSON GENERATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirDecimalCommaMode {
    /// Format decimal numbers with `.`.
    Period,
    /// Format decimal numbers with `,`.
    Comma,
}

/// `NAME OF` replacement target in [`HirXmlGenerateStmt`], [`HirJsonGenerateStmt`], and [`HirJsonParseStmt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirMarkupNameOverride {
    /// `OMITTED` anonymous wrapper suppression.
    Omitted,
    /// Explicit XML tag or JSON property string.
    Rename(String),
}

/// `TYPE OF` XML placement override on [`HirXmlGenerateStmt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirXmlNodeType {
    /// `ATTRIBUTE`.
    Attribute,
    /// `ELEMENT`.
    Element,
    /// `CONTENT`.
    Content,
}

/// Generic `SUPPRESS EVERY ...` specification in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirGenericSuppressRule {
    /// Category code (`0` = Any, `1` = Numeric, `2` = Nonnumeric).
    pub category_code: u8,
    /// XML placement code (`0` = Any, `1` = Element, `2` = Attribute, `3` = Content).
    pub placement_code: u8,
    /// Bitmask of `CobolSuppressMask` (`0` = unconditional, `1` = ZERO, `2` = SPACE, `4` = LOW-VALUE, `8` = HIGH-VALUE).
    pub cond_mask: u8,
}

/// Payload of `XML GENERATE` in [`HirM20Stmt::XmlGenerate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirXmlGenerateStmt {
    /// Destination XML buffer place.
    pub destination: HirPlace,
    /// Source COBOL group or elementary place.
    pub source: HirPlace,
    /// Optional `COUNT IN` receiving place.
    pub count_in: Option<HirPlace>,
    /// `WITH XML-DECLARATION` mode.
    pub xml_decl: HirXmlDeclMode,
    /// `WITH ATTRIBUTES` mode.
    pub attr_mode: HirXmlAttrMode,
    /// Active `DPC-IN-DATA` decimal point mode.
    pub dpc_comma: HirDecimalCommaMode,
    /// Optional `NAMESPACE` URI expression.
    pub namespace: Option<HirExpr>,
    /// Optional `NAMESPACE-PREFIX` expression.
    pub namespace_prefix: Option<HirExpr>,
    /// `(DataItemId, override)` entries from `NAME OF`.
    pub name_overrides: Vec<(DataItemId, HirMarkupNameOverride)>,
    /// `(DataItemId, node_type)` entries from `TYPE OF`.
    pub type_overrides: Vec<(DataItemId, HirXmlNodeType)>,
    /// `(DataItemId, cond_mask)` entries from `SUPPRESS <field>` (later rule for same field wins).
    pub field_suppress: Vec<(DataItemId, u8)>,
    /// Generic `SUPPRESS EVERY ...` entries.
    pub generic_suppress: Vec<HirGenericSuppressRule>,
    /// `ON EXCEPTION` handler statements.
    pub on_exception: Vec<HirStmt>,
    /// `NOT ON EXCEPTION` handler statements.
    pub not_on_exception: Vec<HirStmt>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// `XMLSS` vs `COMPAT` streaming mode on [`HirXmlParseStmt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirXmlStreamMode {
    /// Modern XML System Services mode (`-fxml-parse-xmlss`).
    Xmlss,
    /// Legacy compatibility mode (`-fno-xml-parse-xmlss`).
    Compat,
}

/// Payload of `XML PARSE` in [`HirM20Stmt::XmlParse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirXmlParseStmt {
    /// Input XML document or segment expression (re-evaluated on `END-OF-INPUT` + `XML-CODE = 1`).
    pub source: HirExpr,
    /// `XMLSS` vs `COMPAT` mode.
    pub stream_mode: HirXmlStreamMode,
    /// Entry [`ProcedureId`] of `PROCESSING PROCEDURE`.
    pub proc_start: ProcedureId,
    /// Inclusive end [`ProcedureId`] of `PROCESSING PROCEDURE`.
    pub proc_end: ProcedureId,
    /// `ON EXCEPTION` handler statements.
    pub on_exception: Vec<HirStmt>,
    /// `NOT ON EXCEPTION` handler statements.
    pub not_on_exception: Vec<HirStmt>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Payload of `JSON GENERATE` in [`HirM20Stmt::JsonGenerate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirJsonGenerateStmt {
    /// Destination JSON buffer place.
    pub destination: HirPlace,
    /// Source COBOL group or elementary place.
    pub source: HirPlace,
    /// Optional `COUNT IN` receiving place.
    pub count_in: Option<HirPlace>,
    /// Active `DPC-IN-DATA` decimal point mode.
    pub dpc_comma: HirDecimalCommaMode,
    /// `(DataItemId, override)` entries from `NAME OF`.
    pub name_overrides: Vec<(DataItemId, HirMarkupNameOverride)>,
    /// `(DataItemId, cond_mask)` entries from `SUPPRESS <field>`.
    pub field_suppress: Vec<(DataItemId, u8)>,
    /// Generic `SUPPRESS EVERY ...` entries.
    pub generic_suppress: Vec<HirGenericSuppressRule>,
    /// `(DataItemId, truth_literal)` entries from `CONVERTING ... TO BOOLEAN`.
    pub bool_fields: Vec<(DataItemId, String)>,
    /// `ON EXCEPTION` handler statements.
    pub on_exception: Vec<HirStmt>,
    /// `NOT ON EXCEPTION` handler statements.
    pub not_on_exception: Vec<HirStmt>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// `WITH DETAIL` mode on [`HirJsonParseStmt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirJsonDetailMode {
    /// Standard anomaly handling.
    Standard,
    /// `WITH DETAIL` anomaly reporting (`JSON-CODE = 1`, `JSON-STATUS = 104`).
    WithDetail,
}

/// Payload of `JSON PARSE` in [`HirM20Stmt::JsonParse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirJsonParseStmt {
    /// Input JSON text expression.
    pub source: HirExpr,
    /// Target COBOL group or elementary place.
    pub destination: HirPlace,
    /// `WITH DETAIL` mode.
    pub detail: HirJsonDetailMode,
    /// `(DataItemId, override)` entries from `NAME OF`.
    pub name_overrides: Vec<(DataItemId, HirMarkupNameOverride)>,
    /// Unconditionally suppressed target `DataItemId`s.
    pub suppressed_fields: Vec<DataItemId>,
    /// `(DataItemId, true_literal, false_literal)` entries from `CONVERTING ... FROM BOOLEAN`.
    pub bool_fields: Vec<(DataItemId, String, String)>,
    /// `ON EXCEPTION` handler statements.
    pub on_exception: Vec<HirStmt>,
    /// `NOT ON EXCEPTION` handler statements.
    pub not_on_exception: Vec<HirStmt>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Receiver of an OO COBOL `INVOKE` statement or inline `receiver::"method"(...)` expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirOoReceiver {
    /// Static `FACTORY` dispatch on external class name (`INVOKE ClassName "new" ...`).
    FactoryClass(String),
    /// Dynamic instance/factory dispatch on an `OBJECT REFERENCE` expression (`INVOKE obj "meth" ...`).
    ObjectExpr(HirExpr),
    /// Dispatch on active `SELF` receiver inside a `METHOD-ID`.
    SelfRef,
    /// Dispatch starting at `parent_external` in the superclass chain (`INVOKE SUPER "meth" ...`).
    SuperRef {
        /// External name of the immediate superclass to start method lookup at.
        parent_external: String,
    },
}

/// Payload of `INVOKE` in [`HirM20Stmt::OoInvoke`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirOoInvokeStmt {
    /// Target class/instance/`SELF`/`SUPER` receiver.
    pub receiver: HirOoReceiver,
    /// Method name expression (literal `"new"` / `"deposit"` or dynamic string).
    pub method_name: HirExpr,
    /// Ordered `USING` arguments.
    pub args: Vec<HirCallArg>,
    /// Optional `RETURNING` destination place.
    pub returning: Option<HirPlace>,
    /// Statement source span.
    pub span: SourceSpan,
}

/// Milestone M20 statement variants carried inside [`HirStmt::M20`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirM20Stmt {
    /// `XML GENERATE dst FROM src ...`.
    XmlGenerate(Box<HirXmlGenerateStmt>),
    /// `XML PARSE src PROCESSING PROCEDURE ...`.
    XmlParse(Box<HirXmlParseStmt>),
    /// `JSON GENERATE dst FROM src ...`.
    JsonGenerate(Box<HirJsonGenerateStmt>),
    /// `JSON PARSE src INTO dst ...`.
    JsonParse(Box<HirJsonParseStmt>),
    /// `INVOKE receiver "method" [USING ...] [RETURNING ...]`.
    OoInvoke(Box<HirOoInvokeStmt>),
    /// Binds a synthesized `BasedStorage` `01 __OO_INSTANCE_SELF` root to `coblang_rt_oo_self_data_ptr()`.
    OoBindSelfStorage {
        /// Synthesized `BasedStorage` root [`DataItemId`] holding instance `WORKING-STORAGE`.
        based_root: DataItemId,
        /// Method entry span.
        span: SourceSpan,
    },
    /// `SET obj-ref TO {SELF | NULL | other-ref | inline-invoke}`.
    OoSetRef(Box<(HirPlace, HirExpr, SourceSpan)>),
}

impl HirM20Stmt {
    /// Returns the enclosing [`SourceSpan`] of this M20 statement.
    #[must_use]
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::XmlGenerate(s) => s.span,
            Self::XmlParse(s) => s.span,
            Self::JsonGenerate(s) => s.span,
            Self::JsonParse(s) => s.span,
            Self::OoInvoke(s) => s.span,
            Self::OoBindSelfStorage { span, .. } => *span,
            Self::OoSetRef(b) => b.2,
        }
    }
}

/// Payload of [`HirM20Expr::OoInlineInvoke`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirOoInlineInvokeExpr {
    /// Target class/instance/`SELF`/`SUPER` receiver.
    pub receiver: HirOoReceiver,
    /// Method name expression.
    pub method_name: HirExpr,
    /// Ordered arguments.
    pub args: Vec<HirCallArg>,
    /// Static return [`HirType`] of the invoked method.
    pub return_ty: HirType,
    /// Expression source span.
    pub span: SourceSpan,
}

/// Milestone M20 expression variants carried inside [`HirExpr::M20`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirM20Expr {
    /// `SELF` object reference handle inside a `METHOD-ID`.
    OoSelf {
        /// Expression source span.
        span: SourceSpan,
    },
    /// Inline method invocation `receiver :: "method" (args...)`.
    OoInlineInvoke(Box<HirOoInlineInvokeExpr>),
}

impl HirM20Expr {
    /// Returns the [`SourceSpan`] of this M20 expression.
    #[must_use]
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::OoSelf { span } => *span,
            Self::OoInlineInvoke(e) => e.span,
        }
    }
}

/// Compartment (`FACTORY` vs `OBJECT`) of a [`HirOoMethodSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HirOoCompartment {
    /// Class/static `FACTORY` method.
    Factory,
    /// Per-instance `OBJECT` method.
    Object,
}

/// Lowered method entry inside [`HirOoClassSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirOoMethodSpec {
    /// `Factory` vs `Object` compartment.
    pub compartment: HirOoCompartment,
    /// External method name (used by `INVOKE`).
    pub method_name: String,
    /// Index into the parent [`crate::HirClass::methods`] vector.
    pub method_index: usize,
    /// Byte size of the `RETURNING` linkage item (`0` when void).
    pub return_byte_len: u32,
}

/// Complete lowered runtime registration metadata for an OO `CLASS-ID` unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirOoClassSpec {
    /// External class name (`AS "ext"` if present, else source class name).
    pub external_name: String,
    /// Ordered external names of inherited superclasses.
    pub parent_externals: Vec<String>,
    /// Total byte size of per-instance `OBJECT` `WORKING-STORAGE` (including inherited fields).
    pub instance_byte_size: u32,
    /// Template `HirDataItem` arena used to compute initial `VALUE` bytes for `"new"` instances.
    pub instance_template_items: Vec<HirDataItem>,
    /// Registered factory and instance methods.
    pub method_specs: Vec<HirOoMethodSpec>,
}
