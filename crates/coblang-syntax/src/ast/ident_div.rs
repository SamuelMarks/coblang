//! Typed AST nodes for the COBOL `IDENTIFICATION DIVISION` across programs, functions, and OO units.

use coblang_common::{SourceSpan, SymbolId};

/// Sibling call visibility of a nested program (`IS COMMON`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommonVisibility {
    /// Callable only by the directly enclosing parent program.
    PrivateToParent,
    /// Callable by sibling programs contained in the same parent (`IS COMMON`).
    Common,
}

/// Working-storage re-initialization policy on entry (`IS INITIAL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InitialState {
    /// Retains state across invocations unless `CANCEL`ed.
    Resident,
    /// Resets `WORKING-STORAGE` to initial `VALUE`s on every entry (`IS INITIAL`).
    Initial,
}

/// Re-entrancy policy of a program (`RECURSIVE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Recursivity {
    /// Non-recursive program.
    NonRecursive,
    /// Recursive program (`RECURSIVE`).
    Recursive,
}

/// Prototype declaration status (`IS PROTOTYPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrototypeKind {
    /// Concrete definition.
    Concrete,
    /// External prototype declaration (`IS PROTOTYPE`).
    Prototype,
}

/// Combined `PROGRAM-ID` attribute flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramAttributes {
    /// `COMMON` visibility attribute.
    pub common: CommonVisibility,
    /// `INITIAL` state attribute.
    pub initial: InitialState,
    /// `RECURSIVE` attribute.
    pub recursive: Recursivity,
    /// `PROTOTYPE` attribute.
    pub prototype: PrototypeKind,
}

impl Default for ProgramAttributes {
    /// Returns default `PROGRAM-ID` attributes (`PrivateToParent`, `Resident`, `NonRecursive`, `Concrete`).
    fn default() -> Self {
        Self {
            common: CommonVisibility::PrivateToParent,
            initial: InitialState::Resident,
            recursive: Recursivity::NonRecursive,
            prototype: PrototypeKind::Concrete,
        }
    }
}

/// `PROGRAM-ID` paragraph AST node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramIdClause {
    /// Internal COBOL program identifier.
    pub name: SymbolId,
    /// Optional `AS "external-name"` literal alias.
    pub external_alias: Option<SymbolId>,
    /// Program visibility/lifecycle attributes.
    pub attributes: ProgramAttributes,
    /// Source span of the `PROGRAM-ID` paragraph.
    pub span: SourceSpan,
}

/// `FUNCTION-ID` paragraph AST node for User-Defined Functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionIdClause {
    /// Internal COBOL function identifier.
    pub name: SymbolId,
    /// Optional `AS "external-name"` literal alias.
    pub external_alias: Option<SymbolId>,
    /// Prototype vs concrete definition status.
    pub prototype: PrototypeKind,
    /// Source span of the `FUNCTION-ID` paragraph.
    pub span: SourceSpan,
}

/// Modifier keywords permitted on a `CLASS-ID` paragraph (`FINAL`, `ABSTRACT`, `PUBLIC`, `INTERNAL`, `PARTIAL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClassModifierKind {
    /// `[IS] FINAL` — class cannot be subclassed.
    Final,
    /// `[IS] ABSTRACT` — class cannot be directly instantiated.
    Abstract,
    /// `[IS] PUBLIC` — external visibility modifier.
    Public,
    /// `[IS] INTERNAL` — assembly-internal visibility modifier.
    Internal,
    /// `[IS] PARTIAL` — partial class declaration.
    Partial,
}

/// `CLASS-ID` paragraph AST node for OO COBOL classes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassIdClause {
    /// Class identifier.
    pub name: SymbolId,
    /// Source span of the class identifier token.
    pub name_span: SourceSpan,
    /// Optional `AS "external-name"` alias.
    pub external_alias: Option<SymbolId>,
    /// Superclass identifiers in `INHERITS [FROM] ...`.
    pub inherits: Vec<SymbolId>,
    /// Source spans parallel to `inherits`.
    pub inherit_spans: Vec<SourceSpan>,
    /// Generic parameter identifiers in `USING ...`.
    pub using_params: Vec<SymbolId>,
    /// Modifier clauses encountered in source order (`FINAL`, `ABSTRACT`, `PUBLIC`, `INTERNAL`, `PARTIAL`).
    pub modifiers: Vec<(ClassModifierKind, SourceSpan)>,
    /// Source span of the `CLASS-ID` paragraph.
    pub span: SourceSpan,
}

/// `INTERFACE-ID` paragraph AST node for OO COBOL interfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceIdClause {
    /// Interface identifier.
    pub name: SymbolId,
    /// Source span of the interface identifier token.
    pub name_span: SourceSpan,
    /// Optional `AS "external-name"` alias.
    pub external_alias: Option<SymbolId>,
    /// Super-interface identifiers in `INHERITS [FROM] ...`.
    pub inherits: Vec<SymbolId>,
    /// Source spans parallel to `inherits`.
    pub inherit_spans: Vec<SourceSpan>,
    /// Generic parameter identifiers in `USING ...`.
    pub using_params: Vec<SymbolId>,
    /// Source span of the `INTERFACE-ID` paragraph.
    pub span: SourceSpan,
}

/// Role of an OO `METHOD-ID` declaration (`GET PROPERTY`, `SET PROPERTY`, or regular method).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MethodPropertyRole {
    /// Ordinary instance or factory method.
    StandardMethod,
    /// Property getter (`GET PROPERTY`).
    GetProperty,
    /// Property setter (`SET PROPERTY`).
    SetProperty,
}

/// `METHOD-ID` paragraph AST node inside `FACTORY` or `OBJECT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodIdClause {
    /// Method or property name.
    pub name: SymbolId,
    /// Optional `AS "external-name"` alias.
    pub external_alias: Option<SymbolId>,
    /// Property accessor role.
    pub property_role: MethodPropertyRole,
    /// Source span of the `METHOD-ID` paragraph.
    pub span: SourceSpan,
}

/// ISO 2023 `ARITHMETIC IS ...` mode inside `OPTIONS.` paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArithmeticMode {
    /// Native / dialect default intermediate precision (`NATIVE`).
    Native,
    /// ISO standard decimal intermediate arithmetic (`STANDARD`).
    Standard,
    /// Binary IEEE floating intermediate (`STANDARD-BINARY`).
    StandardBinary,
    /// Decimal IEEE 754-2008 floating intermediate (`STANDARD-DECIMAL`).
    StandardDecimal,
}

/// Entry linkage calling convention inside `OPTIONS.` paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryConvention {
    /// Standard COBOL linkage convention.
    Cobol,
    /// C / external platform ABI convention (`EXTERN` / `C`).
    ExternC,
    /// Win32 / stdcall convention (`STDCALL`).
    Stdcall,
}

/// `OPTIONS.` paragraph AST node inside `IDENTIFICATION DIVISION`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OptionsParagraph {
    /// Optional `ARITHMETIC IS ...` mode.
    pub arithmetic: Option<ArithmeticMode>,
    /// Optional `DEFAULT ROUNDED MODE IS ...` symbol.
    pub default_rounded: Option<SymbolId>,
    /// Optional `ENTRY-CONVENTION IS ...` setting.
    pub entry_convention: Option<EntryConvention>,
    /// Source span when explicitly present.
    pub span: Option<SourceSpan>,
}

/// Complete `IDENTIFICATION DIVISION` header for a `PROGRAM-ID` unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentificationDivision {
    /// `PROGRAM-ID` clause (synthesized if omitted in relaxed syntax mode).
    pub program_id: ProgramIdClause,
    /// Optional ISO `OPTIONS.` paragraph.
    pub options: OptionsParagraph,
    /// Source span of the division.
    pub span: SourceSpan,
}
