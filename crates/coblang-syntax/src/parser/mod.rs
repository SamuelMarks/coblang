//! Resilient recursive-descent COBOL parser cursor and multi-unit compilation-group driver.

/// `DATA DIVISION` section, `FD`/`SD`/`RD`, screen, and arbitrary-order `01`..`88` entry parser.
pub mod data;
/// Pratt expression, `DataRef` subscript/refmod, and abbreviated relation condition parser.
pub mod expr_cond;
/// `IDENTIFICATION` and `ENVIRONMENT` division parser.
pub mod ident_env;
/// Milestone M18 (`REPORT SECTION`) and Milestone M19 (`SCREEN SECTION` & extended I/O) parser.
pub mod report_screen;
/// `PROCEDURE DIVISION`, `DECLARATIVES`, section/paragraph, and 47-verb statement parser.
pub mod stmt;

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{CoblangError, Dialect, ResourceLimitKind, Result, SourceSpan, SymbolId};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, RelaxedSyntaxMode, SymbolInterner,
};
use coblang_pp::MarginZone;

use crate::ast::{
    ClassCompartment, ClassUnit, CompilationGroup, CompilationUnit, DataDivision,
    EnvironmentDivision, FunctionUnit, IdentificationDivision, InterfaceUnit, MethodUnit,
    OptionsParagraph, ProcedureDivision, ProgramAttributes, ProgramIdClause, ProgramUnit,
};
use crate::token::{Token, TokenKind};

/// Deterministic step and recursion ceilings guarding [`Parser`] against runaway loops or stack overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserLimits {
    /// Maximum parser cursor ticks permitted per compilation group (`4_000_000` by default).
    pub max_steps: usize,
    /// Maximum recursive-descent nesting depth (`256` by default).
    pub max_depth: usize,
}

impl Default for ParserLimits {
    /// Returns production parser safeguard ceilings (`4_000_000` steps, `256` depth).
    fn default() -> Self {
        Self::new(4_000_000, 256)
    }
}

impl ParserLimits {
    /// Constructs explicit [`ParserLimits`].
    ///
    /// # Arguments
    ///
    /// * `max_steps` - Maximum parser cursor ticks permitted.
    /// * `max_depth` - Maximum recursive-descent nesting depth permitted.
    #[must_use]
    pub const fn new(max_steps: usize, max_depth: usize) -> Self {
        Self {
            max_steps,
            max_depth,
        }
    }
}

/// Tracks whether `REPOSITORY. FUNCTION ALL INTRINSIC` is active in the current unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum BareAllIntrinsics {
    /// Only explicitly listed bare functions are active.
    #[default]
    Disabled,
    /// `FUNCTION ALL INTRINSIC` was specified in `REPOSITORY`.
    Enabled,
}

/// Stateful token-stream cursor for resilient COBOL parsing.
pub struct Parser<'a> {
    /// Lexed token slice ending with `TokenKind::Eof`.
    pub(crate) tokens: &'a [Token],
    /// Current read index in `tokens`.
    pub(crate) pos: usize,
    /// Fallback EOF span.
    pub(crate) eof_span: SourceSpan,
    /// Active dialect configuration.
    pub(crate) dialect: &'a DialectConfig,
    /// Symbol interner for looking up canonical uppercase spellings and synthesizing fallback IDs.
    pub(crate) interner: &'a mut SymbolInterner,
    /// Compiler diagnostic sink.
    pub(crate) sink: &'a mut DiagnosticSink,
    /// Current parser operation count.
    pub(crate) steps: usize,
    /// Maximum permitted parser operation count.
    pub(crate) max_steps: usize,
    /// Current recursive-descent nesting depth.
    pub(crate) depth: usize,
    /// Maximum permitted recursive-descent nesting depth.
    pub(crate) max_depth: usize,
    /// Active open-parenthesis depth inside expressions, subscripts, and conditions.
    pub(crate) paren_depth: u32,
    /// Non-zero when parsing direct argument expressions inside a `FUNCTION` / bare UDF call.
    pub(crate) in_fn_arg_list: u32,
    /// `SPECIAL-NAMES. CLASS` symbols declared in the active compilation unit.
    pub(crate) declared_classes: BTreeSet<SymbolId>,
    /// Symbols declared in `REPOSITORY. FUNCTION ...` for bare (no-`FUNCTION`) invocation.
    pub(crate) bare_functions: BTreeSet<SymbolId>,
    /// Whether `REPOSITORY. FUNCTION ALL INTRINSIC` was declared in the active unit.
    pub(crate) bare_all_intrinsics: BareAllIntrinsics,
    /// Data item names declared in the active unit's `DATA DIVISION`.
    pub(crate) declared_data_names: BTreeSet<SymbolId>,
    /// Integer `78`-level / `01 ... CONSTANT` values declared in the active unit (`OCCURS MAX-SUB`).
    pub(crate) level_78_ints: BTreeMap<SymbolId, u32>,
}

/// Parses a `TokenKind::Eof`-terminated [`Token`] slice into a [`CompilationGroup`] under default [`ParserLimits`].
///
/// # Arguments
///
/// * `tokens` - Lexed token stream ending in `TokenKind::Eof`.
/// * `eof_span` - Span covering EOF when `tokens` has no body tokens.
/// * `dialect` - Active [`DialectConfig`].
/// * `interner` - Mutable [`SymbolInterner`].
/// * `sink` - Mutable [`DiagnosticSink`] receiving syntax diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError`] if symbol interning or parser budgets fail.
pub fn parse_tokens(
    tokens: &[Token],
    eof_span: SourceSpan,
    dialect: &DialectConfig,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<CompilationGroup> {
    parse_tokens_with_limits(
        tokens,
        eof_span,
        dialect,
        ParserLimits::default(),
        interner,
        sink,
    )
}

/// Parses a `TokenKind::Eof`-terminated [`Token`] slice under explicit [`ParserLimits`].
///
/// # Arguments
///
/// * `tokens` - Lexed token stream ending in `TokenKind::Eof`.
/// * `eof_span` - Span covering EOF when `tokens` has no body tokens.
/// * `dialect` - Active [`DialectConfig`].
/// * `limits` - Explicit [`ParserLimits`] step and recursion ceilings.
/// * `interner` - Mutable [`SymbolInterner`].
/// * `sink` - Mutable [`DiagnosticSink`] receiving syntax diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ResourceLimitExceeded`] if `limits` are exceeded, or propagates
/// symbol interning capacity errors.
pub fn parse_tokens_with_limits(
    tokens: &[Token],
    eof_span: SourceSpan,
    dialect: &DialectConfig,
    limits: ParserLimits,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<CompilationGroup> {
    let mut parser = Parser {
        tokens,
        pos: 0,
        eof_span,
        dialect,
        interner,
        sink,
        steps: 0,
        max_steps: limits.max_steps,
        depth: 0,
        max_depth: limits.max_depth,
        paren_depth: 0,
        in_fn_arg_list: 0,
        declared_classes: BTreeSet::new(),
        bare_functions: BTreeSet::new(),
        bare_all_intrinsics: BareAllIntrinsics::Disabled,
        declared_data_names: BTreeSet::new(),
        level_78_ints: BTreeMap::new(),
    };
    parser.parse_compilation_group()
}

impl Parser<'_> {
    /// Increments the parser step counter and enforces `self.max_steps`.
    pub(crate) fn tick(&mut self) -> Result<()> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.max_steps {
            return Err(CoblangError::ResourceLimitExceeded {
                resource: ResourceLimitKind::ParseSteps,
                limit: self.max_steps,
                observed: self.steps,
            });
        }
        Ok(())
    }

    /// Increments recursion depth and enforces `self.max_depth`.
    pub(crate) fn enter_depth(&mut self) -> Result<()> {
        let next = self.depth.saturating_add(1);
        if next > self.max_depth {
            return Err(CoblangError::ResourceLimitExceeded {
                resource: ResourceLimitKind::ParseRecursionDepth,
                limit: self.max_depth,
                observed: next,
            });
        }
        self.depth = next;
        Ok(())
    }

    /// Decrements recursion depth when leaving a nested production.
    pub(crate) fn exit_depth(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Advances past an opening `(` and increments `self.paren_depth`.
    pub(crate) fn enter_paren(&mut self) {
        self.advance();
        self.paren_depth = self.paren_depth.saturating_add(1);
    }

    /// Decrements `self.paren_depth` and consumes the matching `)` (or emits a syntax error).
    pub(crate) fn exit_paren(&mut self) -> SourceSpan {
        self.paren_depth = self.paren_depth.saturating_sub(1);
        self.consume_rparen()
    }

    /// Parses all top-level compilation units (`PROGRAM`, `FUNCTION`, `CLASS`, `INTERFACE`) in the file.
    pub(crate) fn parse_compilation_group(&mut self) -> Result<CompilationGroup> {
        let start_span = self.current_span();
        let mut units = Vec::new();
        self.skip_noise();

        while !self.at_eof() {
            self.tick()?;
            let before = self.pos;
            let unit = self.parse_single_top_unit()?;
            units.push(unit);
            self.skip_noise();
            if self.pos == before {
                self.advance();
            }
        }

        let end_span = units.last().map_or(start_span, unit_span);
        Ok(CompilationGroup {
            units,
            turn_directives: Vec::new(),
            dpc_in_data_directives: Vec::new(),
            span: start_span.merge(end_span),
        })
    }

    /// Dispatches a top-level unit (`PROGRAM`, `FUNCTION`, `CLASS`, or `INTERFACE`).
    fn parse_single_top_unit(&mut self) -> Result<CompilationUnit> {
        self.declared_classes.clear();
        self.bare_functions.clear();
        self.bare_all_intrinsics = BareAllIntrinsics::Disabled;
        self.declared_data_names.clear();
        self.level_78_ints.clear();
        let start_span = self.current_span();
        let had_ident_header = self.consume_ident_division_header();

        if self.at_word("FUNCTION-ID") {
            let func = self.parse_function_unit(start_span)?;
            return Ok(CompilationUnit::Function(func));
        }
        if self.at_word("CLASS-ID") {
            let cls = self.parse_class_unit(start_span)?;
            return Ok(CompilationUnit::Class(cls));
        }
        if self.at_word("INTERFACE-ID") {
            let iface = self.parse_interface_unit(start_span)?;
            return Ok(CompilationUnit::Interface(iface));
        }

        let prog = self.parse_program_unit(start_span, had_ident_header)?;
        Ok(CompilationUnit::Program(prog))
    }

    /// Parses a `PROGRAM-ID` compilation unit (and any contained nested subprograms).
    ///
    /// # Arguments
    ///
    /// * `start_span` - Opening span of the unit.
    /// * `had_ident_header` - Whether an explicit `IDENTIFICATION DIVISION.` header was consumed.
    pub(crate) fn parse_program_unit(
        &mut self,
        start_span: SourceSpan,
        had_ident_header: bool,
    ) -> Result<ProgramUnit> {
        if !had_ident_header
            && self.dialect.relaxed_syntax == RelaxedSyntaxMode::Strict
            && matches!(
                self.dialect.base_dialect,
                Dialect::Cobol85 | Dialect::Ibm | Dialect::Bs2000
            )
            && !self.at_word("PROGRAM-ID")
        {
            self.emit_syntax_error(
                self.current_span(),
                "expected `IDENTIFICATION DIVISION.` header",
            );
        }

        let program_id = if self.at_word("PROGRAM-ID") {
            self.parse_program_id_clause()?
        } else {
            if self.dialect.relaxed_syntax == RelaxedSyntaxMode::Strict {
                self.emit_syntax_error(self.current_span(), "missing `PROGRAM-ID.` paragraph");
            }
            let anon = self.interner.intern("__ANONYMOUS_PROGRAM")?;
            ProgramIdClause {
                name: anon,
                external_alias: None,
                attributes: ProgramAttributes::default(),
                span: start_span,
            }
        };

        let options = self.parse_optional_options_paragraph();
        let ident_end = options.span.unwrap_or(program_id.span);
        let identification = IdentificationDivision {
            program_id,
            options,
            span: start_span.merge(ident_end),
        };

        let environment = self.parse_optional_environment_division()?;
        let data = self.parse_optional_data_division()?;
        let procedure = self.parse_optional_procedure_division()?;

        let mut nested_programs = Vec::new();
        while !self.at_eof() && self.starts_nested_program() {
            self.enter_depth()?;
            self.declared_classes.clear();
            self.bare_functions.clear();
            self.bare_all_intrinsics = BareAllIntrinsics::Disabled;
            self.declared_data_names.clear();
            let nested_start = self.current_span();
            let nested_hdr = self.consume_ident_division_header();
            if self.at_word("FUNCTION-ID") {
                self.emit_syntax_error(
                    self.current_span(),
                    "functions may not be defined within a program/function",
                );
                let nested_fn = self.parse_function_unit(nested_start);
                self.exit_depth();
                let _ = nested_fn?;
                continue;
            }
            let nested_res = self.parse_program_unit(nested_start, nested_hdr);
            self.exit_depth();
            nested_programs.push(nested_res?);
        }

        let mut end_span = procedure
            .span
            .or(data.span)
            .or(environment.span)
            .unwrap_or(identification.span);
        if self.at_word("END") && self.peek_word_at(1, "PROGRAM") {
            end_span =
                self.consume_end_unit_marker("PROGRAM", Some(identification.program_id.name));
        }

        Ok(ProgramUnit {
            identification,
            environment,
            data,
            procedure,
            nested_programs,
            span: start_span.merge(end_span),
        })
    }

    /// Parses a `FUNCTION-ID` User-Defined Function unit.
    ///
    /// # Arguments
    ///
    /// * `start_span` - Opening span of the unit.
    fn parse_function_unit(&mut self, start_span: SourceSpan) -> Result<FunctionUnit> {
        let function_id = self.parse_function_id_clause()?;
        if function_id.prototype == crate::ast::PrototypeKind::Concrete {
            let _ = self.bare_functions.insert(function_id.name);
        }
        let options: OptionsParagraph = self.parse_optional_options_paragraph();
        let environment: EnvironmentDivision = self.parse_optional_environment_division()?;
        let data: DataDivision = self.parse_optional_data_division()?;
        let procedure: ProcedureDivision = self.parse_optional_procedure_division()?;
        if procedure.span.is_none() && function_id.prototype == crate::ast::PrototypeKind::Concrete
        {
            let fname = self
                .interner
                .resolve_original(function_id.name)
                .unwrap_or("?")
                .to_owned();
            self.emit_syntax_error(
                self.current_span(),
                &format!("FUNCTION '{fname}' has no PROCEDURE DIVISION"),
            );
        }
        while !self.at_eof() && (self.starts_nested_program() || self.at_word("FUNCTION-ID")) {
            self.enter_depth()?;
            let nested_start = self.current_span();
            let nested_hdr = self.consume_ident_division_header();
            if self.at_word("FUNCTION-ID") {
                self.emit_syntax_error(
                    self.current_span(),
                    "functions may not be defined within a program/function",
                );
                let nested_fn = self.parse_function_unit(nested_start);
                self.exit_depth();
                let _ = nested_fn?;
            } else {
                self.emit_syntax_error(
                    self.current_span(),
                    "programs may not be defined within a function",
                );
                let nested_pr = self.parse_program_unit(nested_start, nested_hdr);
                self.exit_depth();
                let _ = nested_pr?;
            }
        }
        let mut end_span = procedure
            .span
            .or(data.span)
            .or(environment.span)
            .unwrap_or(function_id.span);
        if self.at_word("END") && self.peek_word_at(1, "FUNCTION") {
            end_span = self.consume_end_unit_marker("FUNCTION", Some(function_id.name));
        }
        Ok(FunctionUnit {
            function_id,
            options,
            environment,
            data,
            procedure,
            span: start_span.merge(end_span),
        })
    }

    /// Parses an OO `CLASS-ID` unit (`FACTORY` and `OBJECT` compartments).
    ///
    /// # Arguments
    ///
    /// * `start_span` - Opening span of the unit.
    fn parse_class_unit(&mut self, start_span: SourceSpan) -> Result<ClassUnit> {
        let class_id = self.parse_class_id_clause()?;
        let environment = self.parse_optional_environment_division()?;
        let mut factory = None;
        let mut object = None;

        while !self.at_eof() {
            let _ = self.consume_ident_division_header();
            if self.at_word("FACTORY") {
                factory = Some(self.parse_class_compartment("FACTORY")?);
            } else if self.at_word("OBJECT") {
                object = Some(self.parse_class_compartment("OBJECT")?);
            } else {
                break;
            }
        }

        let mut end_span = class_id.span;
        if self.at_word("END") && self.peek_word_at(1, "CLASS") {
            end_span = self.consume_end_unit_marker("CLASS", Some(class_id.name));
        } else if self.at_word("END") && self.peek_word_at(1, "PROGRAM") {
            let err_sp = self.current_span();
            self.emit_syntax_error(
                err_sp,
                "syntax error, unexpected END PROGRAM, expecting FACTORY or OBJECT",
            );
            end_span = self.consume_end_unit_marker("PROGRAM", None);
        }
        Ok(ClassUnit {
            class_id,
            environment,
            factory,
            object,
            span: start_span.merge(end_span),
        })
    }

    /// Parses a `FACTORY.` or `OBJECT.` compartment inside a `CLASS-ID` unit.
    ///
    /// # Arguments
    ///
    /// * `kind_kw` - `"FACTORY"` or `"OBJECT"`.
    fn parse_class_compartment(&mut self, kind_kw: &str) -> Result<ClassCompartment> {
        self.advance();
        let mut implements = Vec::new();
        if self.consume_word("IMPLEMENTS") {
            self.collect_implements_targets(&mut implements);
        }
        self.consume_to_period();
        if self.consume_word("IMPLEMENTS") {
            self.collect_implements_targets(&mut implements);
            let _ = self.consume_period();
        }
        let _ = self.parse_optional_environment_division()?;
        let data = self.parse_optional_data_division()?;
        let _ = self.consume_word("PROCEDURE");
        let _ = self.consume_word("DIVISION");
        let _ = self.consume_period();

        let mut methods = Vec::new();
        while !self.at_eof() {
            let _ = self.consume_ident_division_header();
            if self.at_word("METHOD-ID") {
                methods.push(self.parse_method_unit()?);
            } else {
                break;
            }
        }
        if self.at_word("END") && self.peek_word_at(1, kind_kw) {
            let _ = self.consume_end_unit_marker(kind_kw, None);
        }
        Ok(ClassCompartment {
            implements,
            data,
            methods,
        })
    }

    /// Collects interface symbols after `IMPLEMENTS` until `.` or major division header.
    ///
    /// # Arguments
    ///
    /// * `out` - Output `(interface_sym, span)` vector.
    fn collect_implements_targets(&mut self, out: &mut Vec<(SymbolId, SourceSpan)>) {
        while !self.at_end_or_period() && !self.starts_major_division() {
            if let Some((sym, sp, _)) = self.eat_word() {
                out.push((sym, sp));
            } else {
                break;
            }
        }
    }

    /// Parses an OO `INTERFACE-ID` unit.
    ///
    /// # Arguments
    ///
    /// * `start_span` - Opening span of the unit.
    fn parse_interface_unit(&mut self, start_span: SourceSpan) -> Result<InterfaceUnit> {
        let interface_id = self.parse_interface_id_clause()?;
        let environment = self.parse_optional_environment_division()?;
        let _ = self.consume_word("PROCEDURE");
        let _ = self.consume_word("DIVISION");
        let _ = self.consume_period();

        let mut methods = Vec::new();
        while !self.at_eof() {
            let _ = self.consume_ident_division_header();
            if self.at_word("METHOD-ID") {
                methods.push(self.parse_method_unit()?);
            } else {
                break;
            }
        }
        let mut end_span = interface_id.span;
        if self.at_word("END") && self.peek_word_at(1, "INTERFACE") {
            end_span = self.consume_end_unit_marker("INTERFACE", Some(interface_id.name));
        }
        Ok(InterfaceUnit {
            interface_id,
            environment,
            methods,
            span: start_span.merge(end_span),
        })
    }

    /// Parses a single `METHOD-ID` unit.
    fn parse_method_unit(&mut self) -> Result<MethodUnit> {
        let start = self.current_span();
        let method_id = self.parse_method_id_clause()?;
        let environment = self.parse_optional_environment_division()?;
        let data = self.parse_optional_data_division()?;
        let procedure = self.parse_optional_procedure_division()?;
        let mut end_span = procedure.span.unwrap_or(method_id.span);
        let expected = match method_id.property_role {
            crate::ast::MethodPropertyRole::StandardMethod => Some(method_id.name),
            crate::ast::MethodPropertyRole::GetProperty
            | crate::ast::MethodPropertyRole::SetProperty => None,
        };
        if self.at_word("END") && self.peek_word_at(1, "METHOD") {
            end_span = self.consume_end_unit_marker("METHOD", expected);
        }
        Ok(MethodUnit {
            method_id,
            environment,
            data,
            procedure,
            span: start.merge(end_span),
        })
    }

    /// Checks whether the current cursor starts a nested `IDENTIFICATION DIVISION` / `PROGRAM-ID`.
    #[must_use]
    pub(crate) fn starts_nested_program(&self) -> bool {
        self.at_word("PROGRAM-ID")
            || ((self.at_word("IDENTIFICATION") || self.at_word("ID"))
                && self.peek_word_at(1, "DIVISION"))
    }

    /// Consumes `END <unit_kw> [name].`, validating matching unit identifier if both are present.
    ///
    /// # Arguments
    ///
    /// * `unit_kw` - Expected keyword (`"PROGRAM"`, `"FUNCTION"`, `"CLASS"`, `"INTERFACE"`, `"FACTORY"`, `"OBJECT"`, `"METHOD"`).
    /// * `expected_name` - Optional expected `SymbolId` declared in `<UNIT>-ID`.
    fn consume_end_unit_marker(
        &mut self,
        unit_kw: &str,
        expected_name: Option<SymbolId>,
    ) -> SourceSpan {
        self.advance();
        self.advance();
        if let Some((end_sym, name_span)) = self.eat_any_word_or_string_sym() {
            if let Some(exp) = expected_name
                && end_sym != exp
            {
                let end_str = self.interner.resolve_original(end_sym).unwrap_or("?");
                let exp_str = self.interner.resolve_original(exp).unwrap_or("?");
                self.emit_syntax_error(
                    name_span,
                    &format!(
                        "END {unit_kw} '{end_str}' is different from {unit_kw}-ID '{exp_str}'"
                    ),
                );
            }
        } else if matches!(unit_kw, "CLASS" | "INTERFACE") {
            let kind_lower = unit_kw.to_ascii_lowercase();
            self.emit_syntax_error(
                self.current_span(),
                &format!("syntax error, unexpected ., expecting {kind_lower}-name"),
            );
        }
        let end_span = self.current_span();
        let _ = self.consume_period();
        end_span
    }

    /// Returns a reference to the current [`Token`] (or synthesized EOF).
    #[must_use]
    pub(crate) fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&SENTINEL_EOF)
    }

    /// Returns a reference to the token at relative lookahead `offset`, if within bounds.
    ///
    /// # Arguments
    ///
    /// * `offset` - Non-negative token offset from `self.pos`.
    #[must_use]
    pub(crate) fn peek_at(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.pos + offset)
    }

    /// Returns the [`SourceSpan`] at `self.pos`.
    #[must_use]
    pub(crate) fn current_span(&self) -> SourceSpan {
        self.tokens.get(self.pos).map_or(self.eof_span, |t| t.span)
    }

    /// Returns `true` when at `TokenKind::Eof` or past the end of `self.tokens`.
    #[must_use]
    pub(crate) fn at_eof(&self) -> bool {
        self.tokens
            .get(self.pos)
            .is_none_or(|t| t.kind == TokenKind::Eof)
    }

    /// Returns `true` when the next non-separator token is `TokenKind::Eof` (or slice end) or `TokenKind::Period`.
    #[must_use]
    pub(crate) fn at_end_or_period(&self) -> bool {
        matches!(
            self.peek_non_sep_at(0).unwrap_or(&SENTINEL_EOF).kind,
            TokenKind::Eof | TokenKind::Period
        )
    }

    /// Checks whether the next two non-separator tokens are words matching `(first, second)`.
    ///
    /// # Arguments
    ///
    /// * `first` - Expected first canonical uppercase keyword.
    /// * `second` - Expected second canonical uppercase keyword.
    #[must_use]
    pub(crate) fn at_word_pair(&self, first: &str, second: &str) -> bool {
        (self.peek_word_str_at(0), self.peek_word_str_at(1)) == (Some(first), Some(second))
    }

    /// Consumes two consecutive keywords `first` and `second` if both match, returning `true`.
    ///
    /// # Arguments
    ///
    /// * `first` - Expected first canonical uppercase keyword.
    /// * `second` - Expected second canonical uppercase keyword.
    pub(crate) fn consume_word_pair(&mut self, first: &str, second: &str) -> bool {
        self.skip_separators();
        if self.at_word_pair(first, second) {
            self.advance();
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consumes the current token if it is a `Word` matching any entry in `words`.
    ///
    /// # Arguments
    ///
    /// * `words` - Slice of canonical uppercase keywords to match.
    pub(crate) fn consume_any_word(&mut self, words: &[&str]) -> bool {
        self.skip_separators();
        if self.current_word().is_some_and(|w| words.contains(&w)) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Returns `true` when positioned at `END {PROGRAM | FUNCTION | CLASS | INTERFACE | FACTORY | OBJECT | METHOD}`.
    #[must_use]
    pub(crate) fn at_end_unit_header(&self) -> bool {
        matches!(
            (self.peek_word_str_at(0), self.peek_word_str_at(1)),
            (
                Some("END"),
                Some(
                    "PROGRAM"
                        | "FUNCTION"
                        | "CLASS"
                        | "INTERFACE"
                        | "FACTORY"
                        | "OBJECT"
                        | "METHOD"
                )
            )
        )
    }

    /// Advances past one token (if not at `Eof`) and skips separator commas/semicolons.
    pub(crate) fn advance(&mut self) {
        if !self.at_eof() {
            self.pos += 1;
        }
        self.skip_separators();
    }

    /// Skips optional COBOL noise separators (`,` and `;`).
    pub(crate) fn skip_separators(&mut self) {
        while let Some(tok) = self.tokens.get(self.pos) {
            if matches!(tok.kind, TokenKind::Comma | TokenKind::Semicolon) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Returns `true` when at least one `,` or `;` separator immediately preceded `tokens[self.pos]`.
    #[must_use]
    pub(crate) fn preceded_by_separator(&self) -> bool {
        self.pos
            .checked_sub(1)
            .and_then(|prev| self.tokens.get(prev))
            .is_some_and(|tok| matches!(tok.kind, TokenKind::Comma | TokenKind::Semicolon))
    }

    /// Skips separator commas, semicolons, and stray empty periods.
    pub(crate) fn skip_noise(&mut self) {
        while let Some(tok) = self.tokens.get(self.pos) {
            if matches!(
                tok.kind,
                TokenKind::Comma | TokenKind::Semicolon | TokenKind::Period
            ) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Resolves the canonical uppercase string of `sym` (`""` if unknown).
    ///
    /// # Arguments
    ///
    /// * `sym` - Interned symbol ID.
    #[must_use]
    pub(crate) fn sym_str(&self, sym: SymbolId) -> &str {
        self.interner.resolve_canonical(sym).unwrap_or("")
    }

    /// Checks whether the current token is a `TokenKind::Word` matching `kw` (uppercase).
    ///
    /// # Arguments
    ///
    /// * `kw` - Canonical uppercase keyword to match.
    #[must_use]
    pub(crate) fn at_word(&self, kw: &str) -> bool {
        self.peek_word_at(0, kw)
    }

    /// Checks whether the token at `self.pos + offset` (ignoring `,`/`;` only at 0) matches `kw`.
    ///
    /// # Arguments
    ///
    /// * `offset` - Token lookahead offset.
    /// * `kw` - Canonical uppercase keyword to match.
    #[must_use]
    pub(crate) fn peek_word_at(&self, offset: usize, kw: &str) -> bool {
        let Some(tok) = self.peek_non_sep_at(offset) else {
            return false;
        };
        match tok.kind {
            TokenKind::Word { sym, .. } => self.sym_str(sym) == kw,
            _ => false,
        }
    }

    /// Peeks the `offset`-th non-comma/semicolon token starting from `self.pos`.
    ///
    /// # Arguments
    ///
    /// * `offset` - Logical non-separator token index.
    #[must_use]
    pub(crate) fn peek_non_sep_at(&self, offset: usize) -> Option<&Token> {
        let mut idx = self.pos;
        let mut remaining = offset;
        while let Some(tok) = self.tokens.get(idx) {
            if matches!(tok.kind, TokenKind::Comma | TokenKind::Semicolon) {
                idx += 1;
                continue;
            }
            if remaining == 0 {
                return Some(tok);
            }
            remaining -= 1;
            idx += 1;
        }
        None
    }

    /// Consumes the current token if it is `kw`, returning `true`.
    ///
    /// # Arguments
    ///
    /// * `kw` - Canonical uppercase keyword.
    pub(crate) fn consume_word(&mut self, kw: &str) -> bool {
        self.skip_separators();
        if self.at_word(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consumes a single `TokenKind::Period` if present, returning `true`.
    pub(crate) fn consume_period(&mut self) -> bool {
        self.skip_separators();
        if self
            .tokens
            .get(self.pos)
            .is_some_and(|t| t.kind == TokenKind::Period)
        {
            self.pos += 1;
            while self.tokens.get(self.pos).is_some_and(|t| {
                matches!(
                    t.kind,
                    TokenKind::Period | TokenKind::Comma | TokenKind::Semicolon
                )
            }) {
                self.pos += 1;
            }
            true
        } else {
            false
        }
    }

    /// Synchronizes forward to and consumes the next `TokenKind::Period` (stopping before `EOF`).
    pub(crate) fn consume_to_period(&mut self) {
        while !self.at_eof() {
            if self.consume_period() {
                return;
            }
            self.pos += 1;
        }
    }

    /// Consumes any `Word` token and returns `(SymbolId, SourceSpan, MarginZone)`.
    #[must_use]
    pub(crate) fn eat_word(&mut self) -> Option<(SymbolId, SourceSpan, MarginZone)> {
        self.skip_separators();
        let tok = self.tokens.get(self.pos)?;
        if let TokenKind::Word { sym, area, .. } = tok.kind {
            let span = tok.span;
            self.advance();
            Some((sym, span, area))
        } else {
            None
        }
    }

    /// Consumes a `Word` or `StringLit` or `IntegerLit` as a `SymbolId` (used for program/entry names).
    pub(crate) fn eat_any_word_or_string_sym(&mut self) -> Option<(SymbolId, SourceSpan)> {
        self.skip_separators();
        let tok = self.tokens.get(self.pos)?.clone();
        match tok.kind {
            TokenKind::Word { sym, .. } | TokenKind::IntegerLit(sym) => {
                self.advance();
                Some((sym, tok.span))
            }
            TokenKind::StringLit { bytes, .. } => {
                self.advance();
                let s = String::from_utf8_lossy(&bytes);
                let sym = self.interner.intern(s.trim_end_matches('\0')).ok()?;
                Some((sym, tok.span))
            }
            _ => None,
        }
    }

    /// Consumes a `Word`, `IntegerLit`, or `StringLit` into a strongly-typed [`crate::ast::SpecialNamesAtom`].
    pub(crate) fn eat_special_names_atom(&mut self) -> Option<crate::ast::SpecialNamesAtom> {
        self.skip_separators();
        let tok = self.tokens.get(self.pos)?.clone();
        match tok.kind {
            TokenKind::Word { sym, .. } => {
                self.advance();
                Some(crate::ast::SpecialNamesAtom::Word(sym))
            }
            TokenKind::IntegerLit(sym) => {
                self.advance();
                Some(crate::ast::SpecialNamesAtom::OrdinalOneBased(sym))
            }
            TokenKind::StringLit { bytes, .. } => {
                self.advance();
                Some(crate::ast::SpecialNamesAtom::StringBytes(bytes))
            }
            _ => None,
        }
    }

    /// Emits a syntax error [`Diagnostic`] into `self.sink`.
    ///
    /// # Arguments
    ///
    /// * `span` - Error location span.
    /// * `msg` - Diagnostic message.
    pub(crate) fn emit_syntax_error(&mut self, span: SourceSpan, msg: &str) {
        self.sink.emit(Diagnostic::error(
            DiagnosticCode::E0003Syntax,
            Some(span),
            String::from(msg),
        ));
    }
}

/// Static sentinel EOF token returned by [`Parser::peek`] when `pos` is past the slice end.
pub(crate) static SENTINEL_EOF: Token = Token {
    kind: TokenKind::Eof,
    span: SourceSpan {
        file_id: coblang_common::FileId::new(0),
        start: coblang_common::ByteOffset::ZERO,
        len: coblang_common::ByteLength::ZERO,
        expansion_id: coblang_common::ExpansionId::ROOT,
    },
};

/// Extracts the [`SourceSpan`] of a [`CompilationUnit`].
///
/// # Arguments
///
/// * `unit` - Compilation unit reference.
#[must_use]
fn unit_span(unit: &CompilationUnit) -> SourceSpan {
    match unit {
        CompilationUnit::Program(p) => p.span,
        CompilationUnit::Function(f) => f.span,
        CompilationUnit::Class(c) => c.span,
        CompilationUnit::Interface(i) => i.span,
    }
}
