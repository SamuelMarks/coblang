//! Top-level compilation group and multi-unit COBOL AST (`PROGRAM-ID`, `FUNCTION-ID`, `CLASS-ID`, `INTERFACE-ID`).

/// `DATA DIVISION` AST definitions across all six storage/report/screen sections.
pub mod data_div;
/// `ENVIRONMENT DIVISION` AST definitions (`CONFIGURATION` & `INPUT-OUTPUT` sections).
pub mod env_div;
/// `IDENTIFICATION DIVISION` AST definitions (`PROGRAM-ID`, `FUNCTION-ID`, `CLASS-ID`, `OPTIONS`).
pub mod ident_div;
/// `PROCEDURE DIVISION` AST definitions, expressions, conditions, and all 47 COBOL verbs.
pub mod proc_div;
/// Milestone M18 (`REPORT SECTION`) and Milestone M19 (`SCREEN SECTION` & TUI) AST definitions.
pub mod report_screen;

use coblang_common::{SourceSpan, SymbolId};

pub use data_div::*;
pub use env_div::*;
pub use ident_div::*;
pub use proc_div::*;
pub use report_screen::*;

/// Complete COBOL `PROGRAM-ID` compilation unit (including recursively nested subprograms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramUnit {
    /// `IDENTIFICATION DIVISION` metadata.
    pub identification: IdentificationDivision,
    /// `ENVIRONMENT DIVISION` declarations.
    pub environment: EnvironmentDivision,
    /// `DATA DIVISION` storage, file, report, and screen definitions.
    pub data: DataDivision,
    /// `PROCEDURE DIVISION` executable logic.
    pub procedure: ProcedureDivision,
    /// Contained nested programs (`IDENTIFICATION DIVISION` ... `END PROGRAM` before outer `END PROGRAM`).
    pub nested_programs: Vec<ProgramUnit>,
    /// Source span of the entire program unit.
    pub span: SourceSpan,
}

/// User-Defined Function (`FUNCTION-ID`) compilation unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionUnit {
    /// `FUNCTION-ID` header.
    pub function_id: FunctionIdClause,
    /// Optional `OPTIONS.` paragraph.
    pub options: OptionsParagraph,
    /// `ENVIRONMENT DIVISION` declarations.
    pub environment: EnvironmentDivision,
    /// `DATA DIVISION` definitions.
    pub data: DataDivision,
    /// `PROCEDURE DIVISION` logic (`USING ... RETURNING ...`).
    pub procedure: ProcedureDivision,
    /// Source span of the function unit.
    pub span: SourceSpan,
}

/// Single `METHOD-ID` definition inside an OO `FACTORY`, `OBJECT`, or `INTERFACE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodUnit {
    /// `METHOD-ID` header.
    pub method_id: MethodIdClause,
    /// `ENVIRONMENT DIVISION` declarations.
    pub environment: EnvironmentDivision,
    /// `DATA DIVISION` definitions.
    pub data: DataDivision,
    /// `PROCEDURE DIVISION` logic.
    pub procedure: ProcedureDivision,
    /// Source span.
    pub span: SourceSpan,
}

/// OO `FACTORY` or `OBJECT` sub-compartment inside a [`ClassUnit`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClassCompartment {
    /// Interfaces listed in `IMPLEMENTS ...` on this compartment.
    pub implements: Vec<(SymbolId, SourceSpan)>,
    /// Compartment-level `DATA DIVISION` (`WORKING-STORAGE` class/instance fields).
    pub data: DataDivision,
    /// Methods defined in this compartment.
    pub methods: Vec<MethodUnit>,
}

/// OO `CLASS-ID` compilation unit (`FACTORY` + `OBJECT`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassUnit {
    /// `CLASS-ID` header.
    pub class_id: ClassIdClause,
    /// Class-level `ENVIRONMENT DIVISION` (`REPOSITORY`).
    pub environment: EnvironmentDivision,
    /// Optional `FACTORY.` static compartment.
    pub factory: Option<ClassCompartment>,
    /// Optional `OBJECT.` instance compartment.
    pub object: Option<ClassCompartment>,
    /// Source span of the class unit.
    pub span: SourceSpan,
}

/// OO `INTERFACE-ID` compilation unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceUnit {
    /// `INTERFACE-ID` header.
    pub interface_id: InterfaceIdClause,
    /// Interface-level `ENVIRONMENT DIVISION`.
    pub environment: EnvironmentDivision,
    /// Method prototype declarations.
    pub methods: Vec<MethodUnit>,
    /// Source span of the interface unit.
    pub span: SourceSpan,
}

/// Top-level compilation unit variant inside a [`CompilationGroup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompilationUnit {
    /// `PROGRAM-ID` unit.
    Program(ProgramUnit),
    /// `FUNCTION-ID` unit.
    Function(FunctionUnit),
    /// `CLASS-ID` unit.
    Class(ClassUnit),
    /// `INTERFACE-ID` unit.
    Interface(InterfaceUnit),
}

/// Parsed multi-unit COBOL source file (`CompilationGroup`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilationGroup {
    /// Ordered top-level compilation units in the source file.
    pub units: Vec<CompilationUnit>,
    /// Scoped `>>TURN` and `$SET BOUND`/`SSRANGE` exception directives in source order.
    pub turn_directives: Vec<coblang_pp::TurnDirectiveRecord>,
    /// Scoped `$SET DPC-IN-DATA` directives in source order.
    pub dpc_in_data_directives: Vec<coblang_pp::DpcInDataRecord>,
    /// Enclosing span of the compilation group.
    pub span: SourceSpan,
}
