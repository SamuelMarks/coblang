//! Context-sensitive COBOL lexer, four-division typed AST, and resilient recursive-descent/Pratt parser.

/// Four-division, UDF, and OO COBOL typed Abstract Syntax Tree definitions.
pub mod ast;
/// Context-sensitive COBOL lexical analyzer over preprocessed token streams.
pub mod lexer;
/// Resilient recursive-descent and Pratt parser across all four COBOL divisions.
pub mod parser;
/// Strongly-typed lexical token, literal prefix, and scanner mode types.
pub mod token;

use coblang_common::{ByteLength, ExpansionId, Result, SourceSpan};
use coblang_diag::{DiagnosticSink, DialectConfig, SymbolInterner};
use coblang_pp::PreprocessedUnit;

pub use ast::{CompilationGroup, CompilationUnit, ProgramUnit};
pub use lexer::lex_tokens;
pub use parser::{ParserLimits, parse_tokens, parse_tokens_with_limits};
pub use token::{
    DecimalCommaMode, LexerMode, LiteralSign, StringPrefix, Token, TokenKind, WordRole,
};

/// Lexes and parses a [`PreprocessedUnit`] into a [`CompilationGroup`] under default [`ParserLimits`].
///
/// # Arguments
///
/// * `unit` - Preprocessed unit produced by `coblang-pp`.
/// * `dialect` - Active [`DialectConfig`] profile.
/// * `interner` - Mutable [`SymbolInterner`] shared across frontend phases.
/// * `sink` - Mutable [`DiagnosticSink`] collecting lexical and syntactic diagnostics.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if symbol interning or parser budgets exceed capacity.
pub fn parse_preprocessed_unit(
    unit: &PreprocessedUnit,
    dialect: &DialectConfig,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<CompilationGroup> {
    parse_preprocessed_unit_with_limits(unit, dialect, ParserLimits::default(), interner, sink)
}

/// Lexes and parses a [`PreprocessedUnit`] under explicit [`ParserLimits`] step and recursion budgets.
///
/// # Arguments
///
/// * `unit` - Preprocessed unit produced by `coblang-pp`.
/// * `dialect` - Active [`DialectConfig`] profile.
/// * `limits` - Explicit [`ParserLimits`] step and recursion ceilings.
/// * `interner` - Mutable [`SymbolInterner`] shared across frontend phases.
/// * `sink` - Mutable [`DiagnosticSink`] collecting lexical and syntactic diagnostics.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError::ResourceLimitExceeded`] if `limits` are exceeded, or
/// propagates symbol interning capacity errors.
pub fn parse_preprocessed_unit_with_limits(
    unit: &PreprocessedUnit,
    dialect: &DialectConfig,
    limits: ParserLimits,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<CompilationGroup> {
    let eof_span = SourceSpan::new(
        unit.root_file_id,
        unit.root_eof_offset,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let tokens = lex_tokens(&unit.tokens, eof_span, dialect, interner, sink)?;
    let mut group = parse_tokens_with_limits(&tokens, eof_span, dialect, limits, interner, sink)?;
    group.turn_directives.clone_from(&unit.cdf_env.turn_records);
    group
        .dpc_in_data_directives
        .clone_from(&unit.cdf_env.dpc_in_data_records);
    Ok(group)
}
