//! `IDENTIFICATION DIVISION` and `ENVIRONMENT DIVISION` parser methods.

use coblang_common::{Result, SourceSpan, SymbolId};

use crate::ast::{
    AccessMode, AlphabetDecl, AlphabetKind, AltKeySuppression, AlternateKeySpec, ArithmeticMode,
    AssignModifier, AssignTarget, ClassDecl, ClassIdClause, ClassModifierKind, ClassRange,
    CommonVisibility, ConfigurationSection, ConsoleMode, CurrencySignSpec, DebuggingMode,
    DecimalPointKind, DeviceMnemonicDecl, DuplicatePolicy, EntryConvention, EnvironmentDivision,
    FileControlEntry, FileOrganization, FunctionIdClause, InitialState, InputOutputSection,
    InterfaceIdClause, IntrinsicImportPolicy, IoControlParagraph, LockModeSpec, MethodIdClause,
    MethodPropertyRole, OptionsParagraph, ProgramAttributes, ProgramIdClause, PrototypeKind,
    RecordKeySpec, Recursivity, RepositoryParagraph, SameAreaClause, SameAreaKind,
    SelectOptionality, SharingModeSpec, SpecialNamesParagraph, SwitchDecl, SymbolicCharDecl,
};
use crate::parser::Parser;
use crate::token::TokenKind;

impl Parser<'_> {
    /// Consumes `[IDENTIFICATION | ID] DIVISION.` if present and returns `true`.
    pub(crate) fn consume_ident_division_header(&mut self) -> bool {
        self.skip_noise();
        if self.consume_word_pair("IDENTIFICATION", "DIVISION")
            || self.consume_word_pair("ID", "DIVISION")
        {
            let _ = self.consume_period();
            self.skip_comment_paragraph_headers();
            true
        } else {
            false
        }
    }

    /// Skips any residual `AUTHOR.`..`REMARKS.` paragraph header tokens inside `IDENTIFICATION DIVISION`.
    pub(crate) fn skip_comment_paragraph_headers(&mut self) {
        while matches!(
            self.current_word(),
            Some(
                "AUTHOR"
                    | "INSTALLATION"
                    | "DATE-WRITTEN"
                    | "DATE-COMPILED"
                    | "SECURITY"
                    | "REMARKS"
            )
        ) {
            self.advance();
            let _ = self.consume_period();
        }
    }

    /// Returns the canonical uppercase string of the current token if it is a `Word`.
    #[must_use]
    pub(crate) fn current_word(&self) -> Option<&str> {
        self.peek_word_str_at(0)
    }

    /// Parses `PROGRAM-ID. name [AS alias] [IS COMMON | INITIAL | RECURSIVE | PROTOTYPE].`.
    pub(crate) fn parse_program_id_clause(&mut self) -> Result<ProgramIdClause> {
        let start = self.current_span();
        self.advance();
        let _ = self.consume_period();

        let (name, name_span) = if let Some(pair) = self.eat_any_word_or_string_sym() {
            pair
        } else {
            self.emit_syntax_error(start, "expected program name after `PROGRAM-ID`");
            (self.interner.intern("__MISSING_PROGRAM_ID")?, start)
        };

        let mut external_alias = None;
        let mut attributes = ProgramAttributes::default();
        while !self.at_id_clause_end() {
            if self.consume_word("AS") {
                external_alias = self.eat_any_word_or_string_sym().map(|p| p.0);
            } else if self.consume_any_word(&["IS", "PROGRAM"]) {
                // Noise keyword inside attribute list.
            } else if self.consume_word("COMMON") {
                attributes.common = CommonVisibility::Common;
            } else if self.consume_word("INITIAL") {
                attributes.initial = InitialState::Initial;
            } else if self.consume_word("RECURSIVE") {
                attributes.recursive = Recursivity::Recursive;
            } else if self.consume_word("PROTOTYPE") {
                attributes.prototype = PrototypeKind::Prototype;
            } else {
                self.advance();
            }
        }
        let _ = self.consume_period();
        self.skip_comment_paragraph_headers();

        Ok(ProgramIdClause {
            name,
            external_alias,
            attributes,
            span: start.merge(name_span),
        })
    }

    /// Parses `FUNCTION-ID. name [AS alias] [IS PROTOTYPE].`.
    pub(crate) fn parse_function_id_clause(&mut self) -> Result<FunctionIdClause> {
        let start = self.current_span();
        self.advance();
        let _ = self.consume_period();
        let (name, name_span) = if let Some(pair) = self.eat_any_word_or_string_sym() {
            pair
        } else {
            self.emit_syntax_error(start, "expected function name after `FUNCTION-ID`");
            (self.interner.intern("__MISSING_FUNCTION_ID")?, start)
        };
        let mut external_alias = None;
        let mut prototype = PrototypeKind::Concrete;
        while !self.at_id_clause_end() {
            if self.consume_word("AS") {
                external_alias = self.eat_any_word_or_string_sym().map(|p| p.0);
            } else if self.consume_word("PROTOTYPE") {
                prototype = PrototypeKind::Prototype;
            } else {
                self.advance();
            }
        }
        let _ = self.consume_period();
        self.skip_comment_paragraph_headers();
        Ok(FunctionIdClause {
            name,
            external_alias,
            prototype,
            span: start.merge(name_span),
        })
    }

    /// Parses `CLASS-ID. name [AS alias] [INHERITS [FROM] base ...] [IS FINAL|ABSTRACT|PUBLIC|INTERNAL|PARTIAL] [USING param ...].`.
    pub(crate) fn parse_class_id_clause(&mut self) -> Result<ClassIdClause> {
        let start = self.current_span();
        self.advance();
        let _ = self.consume_period();
        let (name, name_span) = if let Some(pair) = self.eat_any_word_or_string_sym() {
            pair
        } else {
            self.emit_syntax_error(start, "expected class name after `CLASS-ID`");
            (self.interner.intern("__MISSING_CLASS_ID")?, start)
        };
        let mut external_alias = None;
        let mut inherits = Vec::new();
        let mut inherit_spans = Vec::new();
        let mut using_params = Vec::new();
        let mut modifiers = Vec::new();
        while !self.at_id_clause_end() {
            if self.consume_word("AS") {
                external_alias = self.eat_any_word_or_string_sym().map(|p| p.0);
            } else if self.consume_word("INHERITS") {
                let _ = self.consume_word("FROM");
                while !self.at_id_clause_end()
                    && !matches!(
                        self.current_word(),
                        Some(
                            "USING"
                                | "IS"
                                | "FINAL"
                                | "ABSTRACT"
                                | "PUBLIC"
                                | "INTERNAL"
                                | "PARTIAL"
                                | "AS"
                        )
                    )
                    && let Some((sym, sp, _)) = self.eat_word()
                {
                    inherits.push(sym);
                    inherit_spans.push(sp);
                }
            } else if self.consume_word("USING") {
                while !self.at_id_clause_end()
                    && !matches!(
                        self.current_word(),
                        Some(
                            "IS" | "FINAL"
                                | "ABSTRACT"
                                | "PUBLIC"
                                | "INTERNAL"
                                | "PARTIAL"
                                | "INHERITS"
                        )
                    )
                    && let Some((sym, _, _)) = self.eat_word()
                {
                    using_params.push(sym);
                }
            } else if self.consume_word("IS") {
                // Optional noise keyword preceding FINAL/ABSTRACT/PUBLIC/INTERNAL/PARTIAL.
            } else if let Some(mod_kind) = match self.current_word() {
                Some("FINAL") => Some(ClassModifierKind::Final),
                Some("ABSTRACT") => Some(ClassModifierKind::Abstract),
                Some("PUBLIC") => Some(ClassModifierKind::Public),
                Some("INTERNAL") => Some(ClassModifierKind::Internal),
                Some("PARTIAL") => Some(ClassModifierKind::Partial),
                _ => None,
            } {
                let sp = self.current_span();
                self.advance();
                modifiers.push((mod_kind, sp));
            } else {
                self.advance();
            }
        }
        let _ = self.consume_period();
        Ok(ClassIdClause {
            name,
            name_span,
            external_alias,
            inherits,
            inherit_spans,
            using_params,
            modifiers,
            span: start,
        })
    }

    /// Parses `INTERFACE-ID. name [AS alias] [INHERITS ...] [USING param ...].`.
    pub(crate) fn parse_interface_id_clause(&mut self) -> Result<InterfaceIdClause> {
        let start = self.current_span();
        self.advance();
        let _ = self.consume_period();
        let (name, name_span) = if let Some(pair) = self.eat_any_word_or_string_sym() {
            pair
        } else {
            self.emit_syntax_error(start, "expected interface name after `INTERFACE-ID`");
            (self.interner.intern("__MISSING_INTERFACE_ID")?, start)
        };
        let mut external_alias = None;
        let mut inherits = Vec::new();
        let mut inherit_spans = Vec::new();
        let mut using_params = Vec::new();
        while !self.at_id_clause_end() {
            if self.consume_word("AS") {
                external_alias = self.eat_any_word_or_string_sym().map(|p| p.0);
            } else if self.consume_word("INHERITS") {
                let _ = self.consume_word("FROM");
                while !self.at_id_clause_end()
                    && !matches!(self.current_word(), Some("USING" | "AS"))
                    && let Some((sym, sp, _)) = self.eat_word()
                {
                    inherits.push(sym);
                    inherit_spans.push(sp);
                }
            } else if self.consume_word("USING") {
                while !self.at_id_clause_end()
                    && !matches!(self.current_word(), Some("INHERITS" | "AS"))
                    && let Some((sym, _, _)) = self.eat_word()
                {
                    using_params.push(sym);
                }
            } else {
                self.advance();
            }
        }
        let _ = self.consume_period();
        Ok(InterfaceIdClause {
            name,
            name_span,
            external_alias,
            inherits,
            inherit_spans,
            using_params,
            span: start,
        })
    }

    /// Parses `METHOD-ID. {name | GET PROPERTY prop | SET PROPERTY prop} [AS alias].`.
    pub(crate) fn parse_method_id_clause(&mut self) -> Result<MethodIdClause> {
        let start = self.current_span();
        self.advance();
        let _ = self.consume_period();
        let property_role = if self.consume_word("GET") {
            let _ = self.consume_word("PROPERTY");
            MethodPropertyRole::GetProperty
        } else if self.consume_word("SET") {
            let _ = self.consume_word("PROPERTY");
            MethodPropertyRole::SetProperty
        } else {
            MethodPropertyRole::StandardMethod
        };
        let (name, name_span) = if let Some(pair) = self.eat_any_word_or_string_sym() {
            pair
        } else {
            self.emit_syntax_error(start, "expected method name after `METHOD-ID`");
            (self.interner.intern("__MISSING_METHOD_ID")?, start)
        };
        let mut external_alias = None;
        while !self.at_end_or_period() {
            if self.consume_word("AS") {
                external_alias = self.eat_any_word_or_string_sym().map(|p| p.0);
            } else {
                self.advance();
            }
        }
        let _ = self.consume_period();
        Ok(MethodIdClause {
            name,
            external_alias,
            property_role,
            span: start.merge(name_span),
        })
    }

    /// Parses optional `OPTIONS.` paragraph in `IDENTIFICATION DIVISION`.
    pub(crate) fn parse_optional_options_paragraph(&mut self) -> OptionsParagraph {
        self.skip_comment_paragraph_headers();
        if !self.at_word("OPTIONS") {
            return OptionsParagraph::default();
        }
        let start = self.current_span();
        self.advance();
        let _ = self.consume_period();

        let mut opts = OptionsParagraph {
            span: Some(start),
            ..OptionsParagraph::default()
        };

        while !self.at_eof() && !self.starts_major_division() {
            if self.consume_period() {
                continue;
            }
            if self.consume_word("ARITHMETIC") {
                let _ = self.consume_word("IS");
                opts.arithmetic = if self.consume_word("STANDARD-BINARY") {
                    Some(ArithmeticMode::StandardBinary)
                } else if self.consume_word("STANDARD-DECIMAL") {
                    Some(ArithmeticMode::StandardDecimal)
                } else if self.consume_word("STANDARD") {
                    Some(ArithmeticMode::Standard)
                } else {
                    let _ = self.consume_word("NATIVE");
                    Some(ArithmeticMode::Native)
                };
            } else if self.consume_word("DEFAULT") {
                let _ = self.consume_word("ROUNDED");
                let _ = self.consume_word("MODE");
                let _ = self.consume_word("IS");
                opts.default_rounded = self.eat_word().map(|t| t.0);
            } else if self.consume_word("ENTRY-CONVENTION") {
                let _ = self.consume_word("IS");
                opts.entry_convention = if self.consume_word("COBOL") {
                    Some(EntryConvention::Cobol)
                } else if self.consume_word("STDCALL") {
                    Some(EntryConvention::Stdcall)
                } else {
                    self.advance();
                    Some(EntryConvention::ExternC)
                };
            } else {
                self.advance();
            }
        }
        let _ = self.consume_period();
        self.skip_comment_paragraph_headers();
        opts
    }

    /// Parses optional `ENVIRONMENT DIVISION.` (`CONFIGURATION` and `INPUT-OUTPUT` sections).
    pub(crate) fn parse_optional_environment_division(&mut self) -> Result<EnvironmentDivision> {
        self.skip_comment_paragraph_headers();
        if !self.consume_word_pair("ENVIRONMENT", "DIVISION") {
            return Ok(EnvironmentDivision::default());
        }
        let start = self.current_span();
        let _ = self.consume_period();

        let mut env = EnvironmentDivision {
            span: Some(start),
            ..EnvironmentDivision::default()
        };

        while !self.at_eof() && !self.starts_after_env_division() {
            if self.consume_word_pair("CONFIGURATION", "SECTION") {
                let _ = self.consume_period();
                self.parse_configuration_section(&mut env.configuration);
            } else if self.at_input_output_section_start() {
                let _ = self.consume_word_pair("INPUT-OUTPUT", "SECTION");
                let _ = self.consume_period();
                self.parse_input_output_section(&mut env.input_output)?;
            } else if matches!(
                self.current_word(),
                Some("SOURCE-COMPUTER" | "OBJECT-COMPUTER" | "SPECIAL-NAMES" | "REPOSITORY")
            ) {
                self.parse_configuration_section(&mut env.configuration);
            } else {
                self.emit_syntax_error(
                    self.current_span(),
                    "unexpected token in ENVIRONMENT DIVISION",
                );
                self.consume_to_period();
            }
        }

        let end = self.current_span();
        env.span = Some(start.merge(end));
        Ok(env)
    }

    /// Returns `true` when at `INPUT-OUTPUT SECTION`, `FILE-CONTROL`, or `I-O-CONTROL`.
    #[must_use]
    fn at_input_output_section_start(&self) -> bool {
        self.at_word_pair("INPUT-OUTPUT", "SECTION")
            || matches!(self.current_word(), Some("FILE-CONTROL" | "I-O-CONTROL"))
    }

    /// Parses paragraphs inside `CONFIGURATION SECTION`.
    ///
    /// # Arguments
    ///
    /// * `cfg` - Mutable [`ConfigurationSection`] AST node.
    fn parse_configuration_section(&mut self, cfg: &mut ConfigurationSection) {
        while !self.at_eof()
            && !self.starts_after_env_division()
            && !self.at_input_output_section_start()
        {
            if self.consume_word("SOURCE-COMPUTER") {
                let _ = self.consume_period();
                while !self.at_env_para_stop(&[
                    "OBJECT-COMPUTER",
                    "SPECIAL-NAMES",
                    "REPOSITORY",
                    "INPUT-OUTPUT",
                    "FILE-CONTROL",
                ]) {
                    if self.consume_word("DEBUGGING") {
                        let _ = self.consume_word("MODE");
                        cfg.debugging_mode = DebuggingMode::Enabled;
                    } else if self.consume_period() {
                        if !matches!(self.current_word(), Some("WITH" | "DEBUGGING")) {
                            break;
                        }
                    } else if !self.consume_word("WITH") {
                        if cfg.source_computer.is_none()
                            && let Some((sym, _, _)) = self.eat_word()
                        {
                            cfg.source_computer = Some(sym);
                        } else {
                            self.advance();
                        }
                    }
                }
            } else if self.consume_word("OBJECT-COMPUTER") {
                let _ = self.consume_period();
                while !self.at_env_para_stop(&[
                    "SPECIAL-NAMES",
                    "REPOSITORY",
                    "INPUT-OUTPUT",
                    "FILE-CONTROL",
                ]) {
                    if self.consume_word("COLLATING")
                        || self.consume_word_pair("PROGRAM", "COLLATING")
                    {
                        let _ = self.consume_word("SEQUENCE");
                        let _ = self.consume_word("IS");
                        cfg.program_collating_sequence = self.eat_word().map(|t| t.0);
                    } else if self.consume_period() {
                        if !matches!(
                            self.current_word(),
                            Some("PROGRAM" | "COLLATING" | "MEMORY" | "SEGMENT-LIMIT")
                        ) {
                            break;
                        }
                    } else if cfg.object_computer.is_none()
                        && let Some((sym, _, _)) = self.eat_word()
                    {
                        cfg.object_computer = Some(sym);
                    } else {
                        self.advance();
                    }
                }
            } else if self.consume_word("SPECIAL-NAMES") {
                let _ = self.consume_period();
                self.parse_special_names(&mut cfg.special_names);
            } else if self.consume_word("REPOSITORY") {
                let _ = self.consume_period();
                self.parse_repository(&mut cfg.repository);
            } else {
                break;
            }
        }
    }

    /// Parses clauses inside `SPECIAL-NAMES.`.
    ///
    /// # Arguments
    ///
    /// * `sn` - Mutable [`SpecialNamesParagraph`] AST node.
    fn parse_special_names(&mut self, sn: &mut SpecialNamesParagraph) {
        while !self.at_env_para_stop(&["REPOSITORY", "INPUT-OUTPUT", "FILE-CONTROL", "I-O-CONTROL"])
        {
            if self.consume_period() {
                continue;
            }
            let clause_start = self.current_span();
            if self.consume_word("DECIMAL-POINT") {
                let _ = self.consume_word("IS");
                if self.consume_word("COMMA") {
                    sn.decimal_point = DecimalPointKind::Comma;
                }
            } else if self.consume_word("CURRENCY") {
                let _ = self.consume_word("SIGN");
                let _ = self.consume_word("IS");
                if let Some((sign_literal, _)) = self.eat_any_word_or_string_sym() {
                    let mut picture_symbol = None;
                    let _ = self.consume_word("WITH");
                    if self.consume_word("PICTURE") {
                        let _ = self.consume_word("SYMBOL");
                        picture_symbol = self.eat_any_word_or_string_sym().map(|p| p.0);
                    }
                    sn.currency_signs.push(CurrencySignSpec {
                        sign_literal,
                        picture_symbol,
                        span: clause_start,
                    });
                }
            } else if self.consume_word("ALPHABET") {
                self.parse_alphabet_clause(sn, clause_start);
            } else if self.consume_word("SYMBOLIC") {
                self.parse_symbolic_chars_clause(sn, clause_start);
            } else if self.consume_word("CLASS") {
                self.parse_class_clause(sn, clause_start);
            } else if self.consume_word_pair("CRT", "STATUS") {
                let _ = self.consume_word("IS");
                sn.crt_status = self.eat_word().map(|t| t.0);
            } else if self.consume_word("CURSOR") {
                let _ = self.consume_word("IS");
                sn.cursor = self.eat_word().map(|t| t.0);
            } else if self.consume_word("CONSOLE") {
                let _ = self.consume_word("IS");
                if self.consume_word("CRT") {
                    sn.console_mode = ConsoleMode::ConsoleIsCrt;
                }
            } else if let Some((sys_sym, _, _)) = self.eat_word() {
                self.parse_switch_or_device_clause(sn, sys_sym, clause_start);
            } else {
                self.advance();
            }
        }
        let _ = self.consume_period();
    }

    /// Parses `ALPHABET name IS {NATIVE | STANDARD-1 | STANDARD-2 | EBCDIC | custom...}`.
    ///
    /// # Arguments
    ///
    /// * `sn` - Mutable [`SpecialNamesParagraph`].
    /// * `span` - Clause start span.
    fn parse_alphabet_clause(&mut self, sn: &mut SpecialNamesParagraph, span: SourceSpan) {
        let Some((name, _, _)) = self.eat_word() else {
            return;
        };
        let _ = self.consume_word("IS");
        let kind = if self.consume_word("NATIVE") {
            AlphabetKind::Native
        } else if self.consume_any_word(&["STANDARD-1", "ASCII"]) {
            AlphabetKind::Standard1
        } else if self.consume_word("STANDARD-2") {
            AlphabetKind::Standard2
        } else if self.consume_word("EBCDIC") {
            AlphabetKind::Ebcdic
        } else {
            let mut specs = Vec::new();
            while !self.at_env_clause_end(&[
                "ALPHABET",
                "SYMBOLIC",
                "CLASS",
                "CURRENCY",
                "DECIMAL-POINT",
                "CRT",
                "CURSOR",
                "CONSOLE",
                "REPOSITORY",
                "INPUT-OUTPUT",
                "FILE-CONTROL",
                "I-O-CONTROL",
            ]) {
                if self.peek_word_str_at(1) == Some("IS") {
                    break;
                }
                if let Some(atom) = self.eat_special_names_atom() {
                    specs.push(atom);
                } else {
                    self.advance();
                }
            }
            AlphabetKind::Custom(specs)
        };
        sn.alphabets.push(AlphabetDecl { name, kind, span });
    }

    /// Parses `SYMBOLIC CHARACTERS name ... ARE ord ... [IN alphabet]`.
    ///
    /// # Arguments
    ///
    /// * `sn` - Mutable [`SpecialNamesParagraph`].
    /// * `span` - Clause start span.
    fn parse_symbolic_chars_clause(&mut self, sn: &mut SpecialNamesParagraph, span: SourceSpan) {
        let _ = self.consume_any_word(&["CHARACTERS", "CHARACTER"]);
        let mut names = Vec::new();
        let mut ordinals = Vec::new();
        while !self.at_end_or_period() {
            let _ = self.consume_any_word(&["IS", "ARE"]);
            if self.consume_word("IN") {
                let alphabet = self.eat_word().map(|t| t.0);
                sn.symbolic_chars.push(SymbolicCharDecl {
                    names,
                    ordinals,
                    alphabet,
                    span,
                });
                return;
            }
            if let TokenKind::IntegerLit(ord) = self.peek().kind {
                self.advance();
                ordinals.push(ord);
            } else if ordinals.is_empty()
                && let Some((nm, _, _)) = self.eat_word()
            {
                names.push(nm);
            } else {
                break;
            }
        }
        sn.symbolic_chars.push(SymbolicCharDecl {
            names,
            ordinals,
            alphabet: None,
            span,
        });
    }

    /// Parses `CLASS name IS low [THRU high] ... [IN alphabet]`.
    ///
    /// # Arguments
    ///
    /// * `sn` - Mutable [`SpecialNamesParagraph`].
    /// * `span` - Clause start span.
    fn parse_class_clause(&mut self, sn: &mut SpecialNamesParagraph, span: SourceSpan) {
        let Some((name, _, _)) = self.eat_word() else {
            return;
        };
        let _ = self.consume_word("IS");
        let mut ranges = Vec::new();
        let mut alphabet = None;
        while !self.at_env_clause_end(&[
            "CLASS",
            "ALPHABET",
            "SYMBOLIC",
            "CURRENCY",
            "DECIMAL-POINT",
            "REPOSITORY",
            "INPUT-OUTPUT",
            "FILE-CONTROL",
            "I-O-CONTROL",
        ]) {
            if self.consume_word("IN") {
                alphabet = self.eat_word().map(|t| t.0);
                break;
            }
            let Some(low) = self.eat_special_names_atom() else {
                break;
            };
            let high = if self.consume_any_word(&["THRU", "THROUGH"]) {
                self.eat_special_names_atom()
            } else {
                None
            };
            ranges.push(ClassRange { low, high });
        }
        self.declared_classes.insert(name);
        sn.classes.push(ClassDecl {
            name,
            ranges,
            alphabet,
            span,
        });
    }

    /// Parses a switch or device mnemonic clause starting with `sys_sym`.
    ///
    /// # Arguments
    ///
    /// * `sn` - Mutable [`SpecialNamesParagraph`].
    /// * `sys_sym` - System switch or device symbol.
    /// * `span` - Clause start span.
    fn parse_switch_or_device_clause(
        &mut self,
        sn: &mut SpecialNamesParagraph,
        sys_sym: SymbolId,
        span: SourceSpan,
    ) {
        let _ = self.consume_word("IS");
        let mnemonic = if matches!(self.current_word(), Some("ON" | "OFF")) {
            None
        } else {
            self.eat_word().map(|t| t.0)
        };
        let mut on_condition = None;
        let mut off_condition = None;
        let mut has_status = false;
        while matches!(self.current_word(), Some("ON" | "OFF")) {
            has_status = true;
            let is_on = self.consume_word("ON");
            if !is_on {
                let _ = self.consume_word("OFF");
            }
            let _ = self.consume_word("STATUS");
            let _ = self.consume_word("IS");
            let cond = self.eat_word().map(|t| t.0);
            if is_on {
                on_condition = cond;
            } else {
                off_condition = cond;
            }
        }
        let sys_name = self.sym_str(sys_sym).to_owned();
        let is_switch =
            has_status || sys_name.starts_with("SWITCH") || sys_name.starts_with("UPSI");
        if is_switch {
            sn.switches.push(SwitchDecl {
                system_name: sys_sym,
                mnemonic,
                on_condition,
                off_condition,
                span,
            });
        } else if let Some(mn) = mnemonic {
            sn.devices.push(DeviceMnemonicDecl {
                system_device: sys_sym,
                mnemonic: mn,
                span,
            });
        }
    }

    /// Parses `REPOSITORY.` entries (`FUNCTION ALL INTRINSIC`, `FUNCTION name`, `PROGRAM name`, `CLASS name`, `INTERFACE name`).
    ///
    /// # Arguments
    ///
    /// * `repo` - Mutable [`RepositoryParagraph`].
    fn parse_repository(&mut self, repo: &mut RepositoryParagraph) {
        while !self.at_env_clause_end(&["INPUT-OUTPUT", "FILE-CONTROL", "I-O-CONTROL"]) {
            if self.consume_word("FUNCTION") {
                if self.consume_word("ALL") {
                    let _ = self.consume_word("INTRINSIC");
                    repo.intrinsics = IntrinsicImportPolicy::AllIntrinsic;
                    self.bare_all_intrinsics = crate::parser::BareAllIntrinsics::Enabled;
                } else {
                    let mut names = Vec::new();
                    while !self.at_env_clause_end(&[
                        "FUNCTION",
                        "PROGRAM",
                        "CLASS",
                        "INTERFACE",
                        "INTRINSIC",
                    ]) {
                        let Some((nm, sp)) = self.eat_any_word_or_string_sym() else {
                            break;
                        };
                        let alias = if self.consume_word("AS") {
                            self.eat_any_word_or_string_sym().map(|p| p.0)
                        } else {
                            None
                        };
                        names.push((nm, alias, sp));
                    }
                    if self.consume_word("INTRINSIC") {
                        if let IntrinsicImportPolicy::ExplicitOnly = repo.intrinsics {
                            repo.intrinsics = IntrinsicImportPolicy::SpecificIntrinsics(Vec::new());
                        }
                        for (nm, _, sp) in names {
                            let _ = self.bare_functions.insert(nm);
                            repo.intrinsic_spans.push((nm, sp));
                            if let IntrinsicImportPolicy::SpecificIntrinsics(v) =
                                &mut repo.intrinsics
                            {
                                v.push(nm);
                            }
                        }
                    } else {
                        for (nm, alias, sp) in names {
                            let _ = self.bare_functions.insert(nm);
                            repo.functions.push((nm, alias));
                            repo.function_spans.push(sp);
                        }
                    }
                }
            } else if self.consume_word("PROGRAM") {
                if let Some((nm, sp)) = self.eat_any_word_or_string_sym() {
                    let alias = if self.consume_word("AS") {
                        self.eat_any_word_or_string_sym().map(|p| p.0)
                    } else {
                        None
                    };
                    repo.programs.push((nm, alias, sp));
                }
            } else if self.consume_word("CLASS") {
                if let Some((nm, sp)) = self.eat_any_word_or_string_sym() {
                    let alias = if self.consume_word("AS") {
                        self.eat_any_word_or_string_sym().map(|p| p.0)
                    } else {
                        None
                    };
                    self.skip_repository_expands_clause();
                    repo.classes.push((nm, alias));
                    repo.class_spans.push(sp);
                }
            } else if self.consume_word("INTERFACE") {
                if let Some((nm, sp)) = self.eat_any_word_or_string_sym() {
                    let alias = if self.consume_word("AS") {
                        self.eat_any_word_or_string_sym().map(|p| p.0)
                    } else {
                        None
                    };
                    self.skip_repository_expands_clause();
                    repo.interfaces.push((nm, alias));
                    repo.interface_spans.push(sp);
                }
            } else {
                self.advance();
            }
        }
        repo.terminator_span = Some(self.current_span());
        let _ = self.consume_period();
    }

    /// Consumes optional `EXPANDS base [USING param ...]` in a `REPOSITORY` `CLASS` / `INTERFACE` entry.
    fn skip_repository_expands_clause(&mut self) {
        if self.consume_word("EXPANDS") {
            let _ = self.eat_any_word_or_string_sym();
            if self.consume_word("USING") {
                while !self.at_env_clause_end(&["FUNCTION", "CLASS", "INTERFACE"]) {
                    if self.eat_any_word_or_string_sym().is_none() {
                        break;
                    }
                }
            }
        }
    }

    /// Parses `INPUT-OUTPUT SECTION.` (`FILE-CONTROL.` and `I-O-CONTROL.`).
    ///
    /// # Arguments
    ///
    /// * `io` - Mutable [`InputOutputSection`] AST node.
    fn parse_input_output_section(&mut self, io: &mut InputOutputSection) -> Result<()> {
        while !self.at_eof() && !self.starts_after_env_division() {
            if self.consume_word("FILE-CONTROL") {
                let _ = self.consume_period();
                while self.at_word("SELECT") {
                    let entry = self.parse_select_entry()?;
                    io.file_control.push(entry);
                }
            } else if self.at_word("SELECT") {
                let entry = self.parse_select_entry()?;
                io.file_control.push(entry);
            } else if self.consume_word("I-O-CONTROL") {
                let _ = self.consume_period();
                self.parse_io_control(&mut io.io_control);
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Parses a single `SELECT [OPTIONAL] file-name ASSIGN TO ... .` entry.
    fn parse_select_entry(&mut self) -> Result<FileControlEntry> {
        let start = self.current_span();
        self.advance();
        let optionality = if self.consume_word("OPTIONAL") {
            SelectOptionality::Optional
        } else {
            SelectOptionality::Required
        };
        let file_name = if let Some((sym, _, _)) = self.eat_word() {
            sym
        } else {
            self.emit_syntax_error(start, "expected file-name after `SELECT`");
            self.interner.intern("__MISSING_FILE")?
        };

        let mut entry = FileControlEntry {
            file_name,
            optionality,
            assign_targets: Vec::new(),
            organization: FileOrganization::Sequential,
            access_mode: AccessMode::Sequential,
            record_key: None,
            alternate_keys: Vec::new(),
            relative_key: None,
            file_status: None,
            secondary_status: None,
            lock_mode: None,
            sharing_mode: None,
            padding_char: None,
            collating_sequence: None,
            organization_clause_count: 0,
            span: start,
        };

        while !self.at_env_clause_end(&[]) {
            self.parse_select_clause(&mut entry);
        }
        let end = self.current_span();
        let _ = self.consume_period();
        entry.span = start.merge(end);
        Ok(entry)
    }

    /// Parses one sub-clause inside a `SELECT` file-control entry.
    ///
    /// # Arguments
    ///
    /// * `entry` - Mutable [`FileControlEntry`].
    fn parse_select_clause(&mut self, entry: &mut FileControlEntry) {
        let file_name = entry.file_name;
        if self.consume_word("ASSIGN") {
            let _ = self.consume_word("TO");
            let _ = self.consume_word("USING");
            if self.parse_assign_targets(file_name, &mut entry.assign_targets)
                && entry.organization_clause_count == 0
            {
                entry.organization = FileOrganization::LineSequential;
            }
        } else if self.consume_word("ORGANIZATION") {
            let _ = self.consume_word("IS");
            entry.organization = self.parse_file_organization();
            entry.organization_clause_count = entry.organization_clause_count.saturating_add(1);
        } else if self.consume_word_pair("RELATIVE", "KEY")
            || (self.at_word("RELATIVE")
                && matches!(self.peek_word_str_at(1), Some(w) if w == "IS" || !is_select_clause_keyword(Some(w))))
        {
            let _ = self.consume_word("RELATIVE");
            let _ = self.consume_word("KEY");
            let _ = self.consume_word("IS");
            entry.relative_key = Some(self.parse_simple_qualified_syms());
        } else if self.consume_word_pair("RECORD", "SEQUENTIAL") {
            entry.organization = FileOrganization::Sequential;
            entry.organization_clause_count = entry.organization_clause_count.saturating_add(1);
        } else if matches!(
            self.current_word(),
            Some("SEQUENTIAL" | "LINE" | "RELATIVE" | "INDEXED")
        ) {
            entry.organization = self.parse_file_organization();
            entry.organization_clause_count = entry.organization_clause_count.saturating_add(1);
        } else if self.consume_word("ACCESS") {
            let _ = self.consume_word("MODE");
            let _ = self.consume_word("IS");
            entry.access_mode = if self.consume_word("RANDOM") {
                AccessMode::Random
            } else if self.consume_word("DYNAMIC") {
                AccessMode::Dynamic
            } else {
                let _ = self.consume_word("SEQUENTIAL");
                AccessMode::Sequential
            };
        } else if self.consume_word("RECORD") {
            if self.consume_word("DELIMITER") {
                let _ = self.consume_word("IS");
                let _ = self.eat_any_word_or_string_sym();
            } else {
                let rk_span = self.current_span();
                let _ = self.consume_word("KEY");
                let _ = self.consume_word("IS");
                let (split_name, segments) = self.parse_key_segments();
                entry.record_key = Some(RecordKeySpec {
                    split_name,
                    segments,
                    span: rk_span,
                });
            }
        } else if self.consume_word("ALTERNATE") {
            let ak = self.parse_alternate_key_clause();
            entry.alternate_keys.push(ak);
        } else if self.consume_word_pair("FILE", "STATUS") || self.consume_word("STATUS") {
            let _ = self.consume_word("IS");
            entry.file_status = Some(self.parse_simple_qualified_syms());
            if !self.at_end_or_period() && !is_select_clause_keyword(self.current_word()) {
                let second = self.parse_simple_qualified_syms();
                if !second.is_empty() {
                    entry.secondary_status = Some(second);
                }
            }
        } else if self.consume_word("LOCK") {
            entry.lock_mode = Some(self.parse_lock_mode_clause());
        } else if self.consume_word("SHARING") {
            let _ = self.consume_word("WITH");
            entry.sharing_mode = if self.consume_word("NO") {
                let _ = self.consume_word("OTHER");
                Some(SharingModeSpec::NoOther)
            } else if self.consume_word("READ") {
                let _ = self.consume_word("ONLY");
                Some(SharingModeSpec::ReadOnly)
            } else {
                let _ = self.consume_word("ALL");
                let _ = self.consume_word("OTHER");
                Some(SharingModeSpec::AllOther)
            };
        } else if self.consume_word("PADDING") {
            let _ = self.consume_word("CHARACTER");
            let _ = self.consume_word("IS");
            entry.padding_char = self.eat_any_word_or_string_sym().map(|p| p.0);
        } else if self.consume_word("COLLATING") {
            let _ = self.consume_word("SEQUENCE");
            let _ = self.consume_word("IS");
            entry.collating_sequence = self.eat_word().map(|t| t.0);
        } else {
            self.advance();
        }
    }

    /// Parses `LOCK [MODE] [IS] {EXCLUSIVE|AUTOMATIC|MANUAL} [WITH LOCK ON ...]`.
    fn parse_lock_mode_clause(&mut self) -> LockModeSpec {
        let _ = self.consume_word("MODE");
        let _ = self.consume_word("IS");
        let mode = if self.consume_word("EXCLUSIVE") {
            LockModeSpec::Exclusive
        } else if self.consume_word("AUTOMATIC") {
            LockModeSpec::Automatic
        } else {
            let _ = self.consume_word("MANUAL");
            LockModeSpec::Manual
        };
        while self.consume_any_word(&[
            "WITH", "LOCK", "ON", "RECORD", "RECORDS", "MULTIPLE", "ROLLBACK",
        ]) {}
        mode
    }

    /// Parses one or more targets in `ASSIGN TO ...`.
    ///
    /// # Arguments
    ///
    /// * `default_file_sym` - Enclosing `SELECT` file connector symbol (fallback for standalone `DISK`/`PRINTER`).
    /// * `targets` - Output target list.
    fn parse_assign_targets(
        &mut self,
        default_file_sym: SymbolId,
        targets: &mut Vec<AssignTarget>,
    ) -> bool {
        let mut saw_line_advancing = false;
        while !self.at_end_or_period()
            && (!is_select_clause_keyword(self.current_word())
                || self.at_word_pair("LINE", "ADVANCING"))
        {
            if self.consume_word_pair("LINE", "ADVANCING") {
                saw_line_advancing = true;
            }
            let modifier = if self.consume_word("DYNAMIC") {
                AssignModifier::Dynamic
            } else if self.consume_word("EXTERNAL") {
                AssignModifier::External
            } else if self.consume_word("DISK") {
                AssignModifier::Disk
            } else if self.consume_word("PRINTER") {
                AssignModifier::Printer
            } else {
                AssignModifier::Unspecified
            };
            if self.consume_word_pair("LINE", "ADVANCING") {
                saw_line_advancing = true;
            }
            if matches!(modifier, AssignModifier::Disk | AssignModifier::Printer)
                && (self.at_end_or_period() || is_select_clause_keyword(self.current_word()))
            {
                targets.push(AssignTarget::Name {
                    sym: default_file_sym,
                    modifier,
                });
                break;
            }
            if matches!(self.peek().kind, TokenKind::StringLit { .. }) {
                let lit = self
                    .eat_any_word_or_string_sym()
                    .map_or(SymbolId::new(0), |p| p.0);
                if modifier == AssignModifier::Printer {
                    targets.push(AssignTarget::Name {
                        sym: lit,
                        modifier: AssignModifier::Printer,
                    });
                } else {
                    targets.push(AssignTarget::Literal(lit));
                }
            } else if let Some((sym, _, _)) = self.eat_word() {
                if matches!(self.sym_str(sym), "DISPLAY" | "KEYBOARD") {
                    targets.push(AssignTarget::Device(sym));
                } else {
                    targets.push(AssignTarget::Name { sym, modifier });
                }
            } else {
                break;
            }
        }
        saw_line_advancing
    }

    /// Parses `{[LINE] SEQUENTIAL | RELATIVE | INDEXED}`.
    fn parse_file_organization(&mut self) -> FileOrganization {
        if self.consume_word("LINE") {
            let _ = self.consume_word("SEQUENTIAL");
            FileOrganization::LineSequential
        } else if self.consume_word("RELATIVE") {
            FileOrganization::Relative
        } else if self.consume_word("INDEXED") {
            FileOrganization::Indexed
        } else {
            let _ = self.consume_word("RECORD");
            let _ = self.consume_word("BINARY");
            let _ = self.consume_word("SEQUENTIAL");
            FileOrganization::Sequential
        }
    }

    /// Parses `ALTERNATE [RECORD] [KEY] [IS] ... [WITH DUPLICATES] [SUPPRESS WHEN ...]`.
    fn parse_alternate_key_clause(&mut self) -> AlternateKeySpec {
        let ak_span = self.current_span();
        let _ = self.consume_word("RECORD");
        let _ = self.consume_word("KEY");
        let _ = self.consume_word("IS");
        let (split_name, segments) = self.parse_key_segments();
        let mut duplicates = DuplicatePolicy::Unique;
        let mut suppression = None;
        loop {
            if self.consume_word_pair("WITH", "DUPLICATES") || self.consume_word("DUPLICATES") {
                duplicates = DuplicatePolicy::WithDuplicates;
            } else if self.consume_word("SUPPRESS") {
                let _ = self.consume_word("WHEN");
                let is_all = self.consume_word("ALL");
                if let Some((lit_sym, _)) = self.eat_any_word_or_string_sym() {
                    suppression = Some(if is_all {
                        AltKeySuppression::WhenAll(lit_sym)
                    } else {
                        AltKeySuppression::WhenLiteral(lit_sym)
                    });
                }
            } else {
                break;
            }
        }
        AlternateKeySpec {
            split_name,
            segments,
            duplicates,
            suppression,
            span: ak_span,
        }
    }

    /// Parses primary or split key segments (`KEY IS seg1`, `KEY IS new = seg1 seg2`, or `KEY IS new SOURCE IS seg1, seg2`).
    fn parse_key_segments(&mut self) -> (Option<SymbolId>, Vec<Vec<SymbolId>>) {
        let first = self.parse_simple_qualified_syms();
        if self.peek().kind == TokenKind::Eq || self.consume_word("SOURCE") {
            if self.peek().kind == TokenKind::Eq {
                self.advance();
            } else {
                let _ = self.consume_word("IS");
            }
            let mut segs = Vec::new();
            while !self.at_split_key_end() {
                let seg = self.parse_simple_qualified_syms();
                if seg.is_empty() {
                    break;
                }
                segs.push(seg);
            }
            (first.first().copied(), segs)
        } else if first.is_empty() {
            (None, Vec::new())
        } else {
            (None, vec![first])
        }
    }

    /// Returns `true` when at `.`, EOF, a `SELECT` clause keyword, `WITH`/`DUPLICATES`, or `SUPPRESS`.
    #[must_use]
    fn at_split_key_end(&self) -> bool {
        self.at_end_or_period()
            || is_select_clause_keyword(self.current_word())
            || matches!(
                self.current_word(),
                Some("WITH" | "DUPLICATES" | "SUPPRESS")
            )
    }

    /// Parses `base [{IN|OF} qual ...]` into `Vec<SymbolId>`.
    pub(crate) fn parse_simple_qualified_syms(&mut self) -> Vec<SymbolId> {
        let mut path = Vec::new();
        let Some((base, _, _)) = self.eat_word() else {
            return path;
        };
        path.push(base);
        while self.consume_any_word(&["IN", "OF"]) {
            if let Some((q, _, _)) = self.eat_word() {
                path.push(q);
            } else {
                break;
            }
        }
        path
    }

    /// Parses `I-O-CONTROL.` clauses (`SAME [RECORD | SORT | SORT-MERGE] AREA FOR ...`).
    ///
    /// # Arguments
    ///
    /// * `ioc` - Mutable [`IoControlParagraph`].
    fn parse_io_control(&mut self, ioc: &mut IoControlParagraph) {
        while !self.at_eof() && !self.starts_after_env_division() {
            if self.consume_period() {
                continue;
            }
            if self.consume_word("SAME") {
                let sp = self.current_span();
                let kind = if self.consume_word("RECORD") {
                    SameAreaKind::RecordArea
                } else if self.consume_any_word(&["SORT", "SORT-MERGE"]) {
                    SameAreaKind::SortMergeArea
                } else {
                    SameAreaKind::BufferArea
                };
                let _ = self.consume_word("AREA");
                let _ = self.consume_word("FOR");
                let mut files = Vec::new();
                while !self.at_env_clause_end(&["SAME", "MULTIPLE", "APPLY", "RERUN"]) {
                    if let Some((f, _, _)) = self.eat_word() {
                        files.push(f);
                    } else {
                        break;
                    }
                }
                ioc.same_areas.push(SameAreaClause {
                    kind,
                    files,
                    span: sp,
                });
            } else {
                self.advance();
            }
        }
    }

    /// Returns `true` if the current token is `TokenKind::Period`.
    #[must_use]
    pub(crate) fn at_period(&self) -> bool {
        self.peek_non_sep_at(0)
            .is_some_and(|t| t.kind == TokenKind::Period)
    }

    /// Returns `true` when at EOF, `.`, or the start of an `IDENTIFICATION`/major-division header.
    #[must_use]
    fn at_id_clause_end(&self) -> bool {
        self.at_end_or_period() || self.starts_division_or_id_para()
    }

    /// Returns `true` when at EOF, post-`ENVIRONMENT` boundary, or any word in `stop_words`.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Paragraph header words terminating the current paragraph.
    #[must_use]
    fn at_env_para_stop(&self, stop_words: &[&str]) -> bool {
        self.at_eof()
            || self.starts_after_env_division()
            || self.current_word().is_some_and(|w| stop_words.contains(&w))
    }

    /// Returns `true` when at EOF, `.`, post-`ENVIRONMENT` boundary, or any word in `stop_words`.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Clause/paragraph header words terminating the current clause.
    #[must_use]
    fn at_env_clause_end(&self, stop_words: &[&str]) -> bool {
        self.at_end_or_period() || self.at_env_para_stop(stop_words)
    }

    /// Returns `true` when positioned at `ENVIRONMENT`, `DATA`, `PROCEDURE`, or `END <unit>`.
    #[must_use]
    pub(crate) fn starts_major_division(&self) -> bool {
        (matches!(
            self.current_word(),
            Some("ENVIRONMENT" | "DATA" | "PROCEDURE")
        ) && self.peek_word_at(1, "DIVISION"))
            || self.starts_after_env_division()
    }

    /// Returns `true` when positioned at `OPTIONS`, `AUTHOR`..`REMARKS`, or a major division.
    #[must_use]
    fn starts_division_or_id_para(&self) -> bool {
        matches!(
            self.current_word(),
            Some(
                "OPTIONS"
                    | "AUTHOR"
                    | "INSTALLATION"
                    | "DATE-WRITTEN"
                    | "DATE-COMPILED"
                    | "SECURITY"
                    | "REMARKS"
            )
        ) || self.starts_major_division()
    }

    /// Returns `true` when positioned at `DATA DIVISION`, `PROCEDURE DIVISION`, `FACTORY`, `OBJECT`, or `END <unit>`.
    #[must_use]
    pub(crate) fn starts_after_env_division(&self) -> bool {
        (matches!(self.current_word(), Some("DATA" | "PROCEDURE"))
            && self.peek_word_at(1, "DIVISION"))
            || matches!(self.current_word(), Some("FACTORY" | "OBJECT"))
            || self.starts_nested_program()
            || self.at_end_unit_header()
    }

    /// Returns the canonical uppercase string of the `offset`-th non-separator word token.
    ///
    /// # Arguments
    ///
    /// * `offset` - Logical token offset.
    #[must_use]
    pub(crate) fn peek_word_str_at(&self, offset: usize) -> Option<&str> {
        let tok = self.peek_non_sep_at(offset)?;
        if let TokenKind::Word { sym, .. } = tok.kind {
            Some(self.sym_str(sym))
        } else {
            None
        }
    }
}

/// Checks whether `w` begins a new `SELECT` sub-clause.
///
/// # Arguments
///
/// * `w` - Optional current uppercase keyword.
#[must_use]
fn is_select_clause_keyword(w: Option<&str>) -> bool {
    matches!(
        w,
        Some(
            "ASSIGN"
                | "ORGANIZATION"
                | "SEQUENTIAL"
                | "LINE"
                | "RELATIVE"
                | "INDEXED"
                | "ACCESS"
                | "RECORD"
                | "ALTERNATE"
                | "FILE"
                | "STATUS"
                | "LOCK"
                | "PADDING"
                | "COLLATING"
                | "RESERVE"
                | "SHARING"
                | "SELECT"
        )
    )
}
