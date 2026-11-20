//! `PROCEDURE DIVISION`, `DECLARATIVES`, section/paragraph, and 47-verb statement parser.

use coblang_common::{Result, SourceSpan};
use coblang_pp::MarginZone;

use crate::ast::data_div::{GlobalVisibility, KeyOrder};
use crate::ast::proc_div::{
    AllocateInitClause, AllocateTargetUnit, BoundSide, CloseDispositionSpec, CorrespondingMode,
    DeclarativeSection, DeleteKind, DivideOrientation, DuplicateMatchMode, EvaluateObject,
    EvaluateSubject, EvaluateWhenBranch, EventHandlers, ExitKind, Expr, InitializeFillerMode,
    InitializeValueMode, InspectBound, InspectOperation, InspectQualifier, JsonBoolClause,
    JsonDetailPresence, MarkupNameClause, MarkupNameTarget, MarkupSuppressClause,
    MarkupSuppressCond, MarkupSuppressGenericKind, MarkupSuppressNodeKind, MarkupSuppressSubject,
    NegationMode, OpenMode, OpenOptionSpec, Paragraph, ParamOptionality, PassingMode,
    PerformControl, PerformTestPosition, PerformVaryingClause, ProcedureDivision, ProcedureHeader,
    ProcedureSection, QualifiedName, ReadDirection, ReadLockSpec, RoundedTarget, RoundingSpec,
    SearchMode, Sentence, SetAction, SizeErrorHandlers, StartPositionSpec, StepDirection, Stmt,
    StopKind, StringSourceGroup, TruthValue, UnstringDelimiter, UnstringTarget, UseClause,
    UsingParam, WriteHandlerKind, WriteTargetMode, XmlAttrPresence, XmlDeclPresence,
    XmlNationalReturn, XmlNodeTypeClause, XmlTypeClause,
};
use crate::ast::proc_div::{DataRef, DataRefModifier};
use crate::parser::Parser;
use crate::token::TokenKind;

impl Parser<'_> {
    /// Parses optional `PROCEDURE DIVISION [USING ...] [RETURNING ...] [CHAINING ...].`.
    pub(crate) fn parse_optional_procedure_division(&mut self) -> Result<ProcedureDivision> {
        if !self.consume_word_pair("PROCEDURE", "DIVISION") {
            return Ok(ProcedureDivision::default());
        }
        let start = self.current_span();

        let header = self.parse_procedure_header();
        let _ = self.consume_period();

        let mut declaratives = Vec::new();
        if self.consume_word("DECLARATIVES") {
            let _ = self.consume_period();
            declaratives = self.parse_declaratives()?;
        }

        let mut initial_sentences = Vec::new();
        let mut paragraphs = Vec::new();
        let mut sections = Vec::new();

        self.parse_sentences_until_header(&mut initial_sentences)?;

        while !self.at_eof() && !self.starts_after_proc_division() {
            if self.at_section_header() {
                sections.push(self.parse_procedure_section()?);
            } else if self.at_paragraph_header() {
                paragraphs.push(self.parse_paragraph()?);
            } else {
                self.advance();
            }
        }

        Ok(ProcedureDivision {
            header,
            declaratives,
            initial_sentences,
            paragraphs,
            sections,
            span: Some(start.merge(self.current_span())),
        })
    }

    /// Parses `[USING ...] [CHAINING ...] [RETURNING/GIVING ...]` on `PROCEDURE DIVISION`.
    fn parse_procedure_header(&mut self) -> ProcedureHeader {
        let mut header = ProcedureHeader::default();
        while !self.at_end_or_period() {
            if self.consume_word("USING") {
                header.using_params = self.parse_formal_using_list();
            } else if self.consume_word("CHAINING") {
                while !self.at_end_or_period()
                    && !matches!(self.current_word(), Some("RETURNING" | "GIVING"))
                    && let Some((sym, _, _)) = self.eat_word()
                {
                    header.chaining_params.push(sym);
                }
            } else if self.consume_any_word(&["RETURNING", "GIVING"]) {
                if !self.consume_any_word(&["OMITTED", "NOTHING"]) {
                    header.returning = self.eat_word().map(|t| t.0);
                }
            } else {
                self.advance();
            }
        }
        header
    }

    /// Parses formal `[BY REFERENCE|VALUE|CONTENT] [OPTIONAL] name ...` parameter list.
    fn parse_formal_using_list(&mut self) -> Vec<UsingParam> {
        let mut params = Vec::new();
        let mut mode = PassingMode::ByReference;
        while !self.at_end_or_period()
            && !matches!(
                self.current_word(),
                Some("RETURNING" | "GIVING" | "CHAINING")
            )
        {
            let _ = self.consume_word("BY");
            if self.consume_word("VALUE") {
                mode = PassingMode::ByValue;
                continue;
            }
            if self.consume_word("CONTENT") {
                mode = PassingMode::ByContent;
                continue;
            }
            if self.consume_word("REFERENCE") {
                mode = PassingMode::ByReference;
                continue;
            }
            let optionality = if self.consume_word("OPTIONAL") {
                ParamOptionality::Optional
            } else {
                ParamOptionality::Required
            };
            let Some((name, span, _)) = self.eat_word() else {
                break;
            };
            params.push(UsingParam {
                mode,
                optionality,
                name,
                span,
            });
        }
        params
    }

    /// Returns `true` when positioned at `END DECLARATIVES`.
    #[must_use]
    fn at_end_declaratives(&self) -> bool {
        self.at_word_pair("END", "DECLARATIVES")
    }

    /// Parses `DECLARATIVES. ... END DECLARATIVES.`.
    fn parse_declaratives(&mut self) -> Result<Vec<DeclarativeSection>> {
        let mut out = Vec::new();
        while !self.at_eof() && !self.at_end_declaratives() && !self.starts_after_proc_division() {
            let Some((name, span)) = self.eat_any_word_or_string_sym() else {
                self.advance();
                continue;
            };
            let _ = self.consume_word("SECTION");
            let _ = self.eat_u32_lit();
            let _ = self.consume_period();
            let use_clause = self.parse_use_clause();
            let _ = self.consume_period();

            let mut initial_sentences = Vec::new();
            let mut paragraphs = Vec::new();
            self.parse_sentences_until_header(&mut initial_sentences)?;
            while !self.at_section_or_decl_end() {
                paragraphs.push(self.parse_paragraph()?);
            }
            out.push(DeclarativeSection {
                name,
                use_clause,
                initial_sentences,
                paragraphs,
                span,
            });
        }
        if self.consume_word_pair("END", "DECLARATIVES") {
            let _ = self.consume_period();
        }
        Ok(out)
    }

    /// Parses `USE [GLOBAL] AFTER ...` / `USE [GLOBAL] BEFORE REPORTING ...` / `USE FOR DEBUGGING ...`.
    fn parse_use_clause(&mut self) -> UseClause {
        let use_start = self.current_span();
        let _ = self.consume_word("USE");
        let global = if self.consume_word("GLOBAL") {
            GlobalVisibility::Global
        } else {
            GlobalVisibility::Local
        };
        if self.consume_word("BEFORE") {
            let _ = self.consume_word("REPORTING");
            let (group, g_sp) = self.eat_word().map_or(
                (coblang_common::SymbolId::new(0), use_start),
                |(s, sp, _)| (s, sp),
            );
            return UseClause::BeforeReporting {
                global,
                group,
                use_span: use_start.merge(g_sp),
            };
        }
        if self.consume_word_pair("FOR", "DEBUGGING") || self.consume_word("DEBUGGING") {
            let _ = self.consume_word("ON");
            let mut targets = Vec::new();
            while !self.at_end_or_period() {
                if self.at_word_pair("ALL", "PROCEDURES") {
                    let sp = self.current_span();
                    self.advance();
                    let end_sp = self.current_span();
                    self.advance();
                    targets.push(crate::ast::DebugUseTarget::AllProcedures(sp.merge(end_sp)));
                } else if self.consume_word("ALL") {
                    if self.dialect.base_dialect != coblang_common::Dialect::Acu {
                        let _ = self.consume_word("REFERENCES");
                    }
                    let _ = self.consume_word("OF");
                    if let Ok(dref) = self.parse_data_ref() {
                        targets.push(crate::ast::DebugUseTarget::AllReferencesOf(dref));
                    }
                } else if self.current_word().is_some() {
                    if let Ok(dref) = self.parse_data_ref() {
                        targets.push(crate::ast::DebugUseTarget::Target(dref));
                    }
                } else {
                    self.advance();
                }
            }
            return UseClause::ForDebugging { targets };
        }
        let _ = self.consume_word("AFTER");
        let _ = self.consume_word("STANDARD");
        let _ = self.consume_any_word(&["EXCEPTION", "ERROR"]);
        let _ = self.consume_word("PROCEDURE");
        let _ = self.consume_word("ON");
        let mut targets = Vec::new();
        while !self.at_end_or_period() {
            if self.consume_any_word(&["CONDITION", "OBJECT"]) {
                continue;
            }
            if let Some((sym, _, _)) = self.eat_word() {
                targets.push(sym);
            } else {
                self.advance();
            }
        }
        UseClause::AfterException { global, targets }
    }

    /// Parses a named `ProcedureSection` (`name SECTION [priority]. ...`).
    fn parse_procedure_section(&mut self) -> Result<ProcedureSection> {
        let start = self.current_span();
        let (name, _) = self
            .eat_any_word_or_string_sym()
            .unwrap_or((coblang_common::SymbolId::new(0), start));
        let _ = self.consume_word("SECTION");
        let segment_priority = self.eat_u32_lit().and_then(|v| u8::try_from(v).ok());
        let _ = self.consume_period();

        let mut initial_sentences = Vec::new();
        let mut paragraphs = Vec::new();
        self.parse_sentences_until_header(&mut initial_sentences)?;
        while !self.at_section_or_decl_end() {
            paragraphs.push(self.parse_paragraph()?);
        }
        Ok(ProcedureSection {
            name,
            segment_priority,
            initial_sentences,
            paragraphs,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses a named `Paragraph` (`name. sentence...`).
    fn parse_paragraph(&mut self) -> Result<Paragraph> {
        let start = self.current_span();
        let (name, _) = self
            .eat_any_word_or_string_sym()
            .unwrap_or((coblang_common::SymbolId::new(0), start));
        let _ = self.consume_period();

        let mut sentences = Vec::new();
        self.parse_sentences_until_header(&mut sentences)?;
        Ok(Paragraph {
            name,
            sentences,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses zero or more period-terminated [`Sentence`]s until a section, paragraph, or unit boundary.
    ///
    /// # Arguments
    ///
    /// * `out` - Output [`Sentence`] list.
    fn parse_sentences_until_header(&mut self, out: &mut Vec<Sentence>) -> Result<()> {
        while !self.at_sentence_boundary() {
            let sent_start = self.current_span();
            let mut statements = Vec::new();
            while !self.at_period() && !self.at_sentence_boundary() {
                if let Some(stmt) = self.parse_one_stmt()? {
                    statements.push(stmt);
                } else {
                    self.emit_syntax_error(
                        self.current_span(),
                        "unexpected token in PROCEDURE DIVISION sentence",
                    );
                    self.consume_to_period();
                    break;
                }
            }
            let sent_end = self.current_span();
            let _ = self.consume_period();
            if !statements.is_empty() {
                out.push(Sentence {
                    statements,
                    span: sent_start.merge(sent_end),
                });
            }
        }
        Ok(())
    }

    /// Parses a single COBOL statement (returning `None` on block/scope terminators like `ELSE`, `WHEN`, `END-*`).
    fn parse_one_stmt(&mut self) -> Result<Option<Stmt>> {
        self.skip_separators();
        let TokenKind::Word { sym, .. } = self.peek().kind else {
            return Ok(None);
        };
        if is_block_terminator_word(self.sym_str(sym)) {
            return Ok(None);
        }
        self.tick()?;
        self.enter_depth()?;
        let res = self.parse_one_stmt_inner(sym);
        self.exit_depth();
        res.map(Some)
    }

    /// Dispatches a verified non-terminator COBOL statement verb inside [`Self::parse_one_stmt`]'s depth guard.
    ///
    /// # Arguments
    ///
    /// * `verb_sym` - Interned symbol ID of the verb at cursor.
    fn parse_one_stmt_inner(&mut self, verb_sym: coblang_common::SymbolId) -> Result<Stmt> {
        let start = self.current_span();
        let stmt = match self.sym_str(verb_sym) {
            "ACCEPT" => self.parse_accept_stmt(start)?,
            "ADD" => self.parse_add_stmt(start)?,
            "ALLOCATE" => self.parse_allocate_stmt(start)?,
            "ALTER" => self.parse_alter_stmt(start)?,
            "CALL" => self.parse_call_stmt(start)?,
            "CANCEL" => self.parse_cancel_stmt(start)?,
            "CLOSE" => self.parse_close_stmt(start),
            "COMMIT" => {
                self.advance();
                Stmt::Commit { span: start }
            }
            "COMPUTE" => self.parse_compute_stmt(start)?,
            "CONTINUE" => self.parse_continue_stmt(start)?,
            "DELETE" => self.parse_delete_stmt(start)?,
            "DISABLE" | "ENABLE" => self.parse_comm_enable_disable_stmt(start)?,
            "DISPLAY" => self.parse_display_stmt(start)?,
            "DIVIDE" => self.parse_divide_stmt(start)?,
            "ENTRY" => self.parse_entry_stmt(start)?,
            "EVALUATE" => self.parse_evaluate_stmt(start)?,
            "EXHIBIT" => self.parse_exhibit_stmt(start)?,
            "EXIT" => self.parse_exit_stmt(start)?,
            "FREE" => self.parse_free_stmt(start)?,
            "GENERATE" => self.parse_generate_stmt(start)?,
            "GO" => self.parse_goto_stmt(start)?,
            "GOBACK" => self.parse_goback_stmt(start)?,
            "IF" => self.parse_if_stmt(start)?,
            "INITIALIZE" => self.parse_initialize_stmt(start)?,
            "INITIATE" => self.parse_initiate_stmt(start),
            "INSPECT" => self.parse_inspect_stmt(start)?,
            "INVOKE" => self.parse_invoke_stmt(start)?,
            "JSON" => self.parse_json_stmt(start)?,
            "MERGE" => self.parse_merge_stmt(start)?,
            "MOVE" => self.parse_move_stmt(start)?,
            "MULTIPLY" => self.parse_multiply_stmt(start)?,
            "NEXT" => {
                self.advance();
                let _ = self.consume_word("SENTENCE");
                Stmt::NextSentence {
                    span: start.merge(self.current_span()),
                }
            }
            "OPEN" => self.parse_open_stmt(start),
            "PERFORM" => self.parse_perform_stmt(start)?,
            "PURGE" => self.parse_comm_purge_stmt(start)?,
            "RAISE" => self.parse_raise_stmt(start)?,
            "READ" => self.parse_read_stmt(start)?,
            "RECEIVE" => self.parse_comm_receive_stmt(start)?,
            "RELEASE" => self.parse_release_stmt(start)?,
            "RESUME" => self.parse_resume_stmt(start)?,
            "RETURN" => self.parse_return_stmt(start)?,
            "REWRITE" => self.parse_rewrite_stmt(start)?,
            "ROLLBACK" => {
                self.advance();
                Stmt::Rollback { span: start }
            }
            "SEARCH" => self.parse_search_stmt(start)?,
            "SEND" => self.parse_comm_send_stmt(start)?,
            "SET" => self.parse_set_stmt(start)?,
            "SORT" => self.parse_sort_stmt(start)?,
            "START" => self.parse_start_stmt(start)?,
            "STOP" => self.parse_stop_stmt(start)?,
            "STRING" => self.parse_string_stmt(start)?,
            "SUBTRACT" => self.parse_subtract_stmt(start)?,
            "SUPPRESS" => {
                self.advance();
                let _ = self.consume_word("PRINTING");
                Stmt::Suppress { span: start }
            }
            "TERMINATE" => self.parse_terminate_stmt(start),
            "TRANSFORM" => self.parse_transform_stmt(start)?,
            "UNLOCK" => self.parse_unlock_stmt(start)?,
            "UNSTRING" => self.parse_unstring_stmt(start)?,
            "VALIDATE" => self.parse_validate_stmt(start)?,
            "WRITE" => self.parse_write_stmt(start)?,
            "XML" => self.parse_xml_stmt(start)?,
            _ => {
                let bad = self.sym_str(verb_sym).to_owned();
                self.emit_syntax_error(start, &format!("unknown or invalid COBOL verb `{bad}`"));
                self.consume_to_period();
                Stmt::Continue {
                    after_seconds: None,
                    span: start,
                }
            }
        };
        Ok(stmt)
    }

    /// Parses an imperative or nested-conditional statement sequence up to period, `END-*`, `ELSE`, `WHEN`, or handler keywords.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Additional contextual stop keywords (e.g. `&["ELSE", "END-IF"]`).
    fn parse_stmt_block(&mut self, stop_words: &[&str]) -> Result<Vec<Stmt>> {
        let mut stmts = Vec::new();
        while !self.at_period() && !self.at_sentence_boundary() {
            if let Some(cw) = self.current_word()
                && (stop_words.contains(&cw) || self.starts_compound_handler())
            {
                break;
            }
            if let Some(s) = self.parse_one_stmt()? {
                stmts.push(s);
            } else {
                break;
            }
        }
        Ok(stmts)
    }

    /// Checks for multi-word handler boundaries (`NOT ON SIZE ERROR`, `ON EXCEPTION`, `AT END`, `INVALID KEY`).
    #[must_use]
    fn starts_compound_handler(&self) -> bool {
        if self.at_word("NOT") {
            let next = self.peek_word_str_at(1);
            let third = self.peek_word_str_at(2);
            return matches!(
                (next, third),
                (
                    Some(
                        "SIZE"
                            | "EXCEPTION"
                            | "OVERFLOW"
                            | "INVALID"
                            | "AT"
                            | "END"
                            | "END-OF-PAGE"
                            | "EOP"
                    ),
                    _
                ) | (Some("ON"), Some("SIZE" | "EXCEPTION" | "OVERFLOW"))
            );
        }
        self.at_word("ON")
            && matches!(
                self.peek_word_str_at(1),
                Some("SIZE" | "EXCEPTION" | "OVERFLOW")
            )
    }

    /// Parses `ACCEPT target [FROM ...] [ON EXCEPTION ...] [END-ACCEPT]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `ACCEPT` verb.
    fn parse_accept_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        self.parse_accept_stmt_m19(start)
    }

    /// Parses `ADD [CORR] a ... [TO b ...] [GIVING c ...] [ON SIZE ERROR ...] [END-ADD]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `ADD` verb.
    fn parse_add_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mode = if self.consume_any_word(&["CORRESPONDING", "CORR"]) {
            CorrespondingMode::Corresponding
        } else {
            CorrespondingMode::Standard
        };
        let mut operands = Vec::new();
        while !self.at_end_or_period() && !matches!(self.current_word(), Some("TO" | "GIVING")) {
            operands.push(self.parse_expr()?);
        }
        let mut to_giving_mid = None;
        let mut to_targets = Vec::new();
        if self.consume_word("TO") {
            if self.lookahead_has_giving_before_end() {
                to_giving_mid = Some(self.parse_expr()?);
            } else {
                to_targets = self.parse_rounded_targets(&["GIVING", "SIZE", "END-ADD"])?;
            }
        }
        let giving_targets = if self.consume_word("GIVING") {
            self.parse_rounded_targets(&["SIZE", "END-ADD"])?
        } else {
            Vec::new()
        };
        let size_error = self.parse_size_error_handlers("END-ADD")?;
        let _ = self.consume_word("END-ADD");
        Ok(Stmt::Add {
            mode,
            operands,
            to_giving_mid,
            to_targets,
            giving_targets,
            size_error,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `ALLOCATE {expr CHARACTERS | item} [INITIALIZED [TO expr]] [RETURNING ptr]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `ALLOCATE` verb.
    fn parse_allocate_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let target = self.parse_expr()?;
        let unit = if self.consume_word("CHARACTERS") {
            AllocateTargetUnit::ExplicitCharacters
        } else {
            AllocateTargetUnit::BasedOrDefault
        };
        let initialized = if self.consume_word("INITIALIZED") {
            if self.consume_word("TO") {
                AllocateInitClause::InitializedTo(self.parse_expr()?)
            } else {
                AllocateInitClause::InitializedDefault
            }
        } else {
            AllocateInitClause::Uninitialized
        };
        let returning = if self.consume_word("RETURNING") {
            Some(self.parse_data_ref()?)
        } else {
            None
        };
        let _ = self.consume_word("END-ALLOCATE");
        Ok(Stmt::Allocate {
            target,
            unit,
            initialized,
            returning,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `ALTER p1 TO [PROCEED TO] p2 ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `ALTER` verb.
    fn parse_alter_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mut pairs = Vec::new();
        while !self.at_clause_boundary(&[]) {
            let p1 = self.parse_qualified_name()?;
            let _ = self.consume_word("TO");
            let _ = self.consume_word("PROCEED");
            let _ = self.consume_word("TO");
            let p2 = self.parse_qualified_name()?;
            pairs.push((p1, p2));
        }
        Ok(Stmt::Alter {
            pairs,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `CALL callee [USING ...] [RETURNING ...] [ON EXCEPTION/OVERFLOW ...] [END-CALL]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `CALL` verb.
    fn parse_call_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let _ = self.consume_word("PROTOTYPE");
        let callee = self.parse_expr()?;
        let mut args = Vec::new();
        if self.consume_word("USING") {
            let mut sticky_mode = PassingMode::ByReference;
            while !self.at_call_arg_boundary(&[
                "RETURNING",
                "GIVING",
                "ON",
                "EXCEPTION",
                "OVERFLOW",
                "NOT",
                "END-CALL",
            ]) {
                let arg = self.parse_single_call_arg(sticky_mode)?;
                if let crate::ast::proc_div::CallArg::Value { mode, .. } = &arg {
                    sticky_mode = *mode;
                }
                args.push(arg);
            }
        }
        let returning = if self.consume_any_word(&["RETURNING", "GIVING"]) {
            if self.consume_any_word(&["OMITTED", "NOTHING"]) {
                crate::ast::proc_div::CallReturning::Omitted
            } else {
                crate::ast::proc_div::CallReturning::Target(self.parse_data_ref()?)
            }
        } else {
            crate::ast::proc_div::CallReturning::Implicit
        };
        let handlers = self.parse_call_handlers()?;
        let _ = self.consume_word("END-CALL");
        Ok(Stmt::Call {
            callee,
            args,
            returning,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `CANCEL target ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `CANCEL` verb.
    fn parse_cancel_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mut targets = Vec::new();
        while !self.at_clause_boundary(&[]) {
            targets.push(self.parse_expr()?);
        }
        Ok(Stmt::Cancel {
            targets,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `CLOSE file [WITH LOCK | REEL | UNIT ...] ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `CLOSE` verb.
    fn parse_close_stmt(&mut self, start: SourceSpan) -> Stmt {
        self.advance();
        let mut files = Vec::new();
        let mut dispositions = Vec::new();
        while !self.at_clause_boundary(&[]) {
            let Some((f, _, _)) = self.eat_word() else {
                break;
            };
            let mut disp = CloseDispositionSpec::Standard;
            loop {
                if self.consume_any_word(&["REEL", "UNIT"]) {
                    let _ = self.consume_word("FOR");
                    disp = if self.consume_word("REMOVAL") {
                        CloseDispositionSpec::ReelUnitForRemoval
                    } else {
                        CloseDispositionSpec::ReelUnit
                    };
                } else if self.consume_word("FOR") {
                    if self.consume_word("REMOVAL") {
                        disp = CloseDispositionSpec::ReelUnitForRemoval;
                    }
                } else if self.consume_word("WITH") {
                    if self.consume_word("LOCK") {
                        disp = CloseDispositionSpec::WithLock;
                    } else if self.consume_word_pair("NO", "REWIND") || self.consume_word("NO") {
                        let _ = self.consume_word("REWIND");
                        disp = CloseDispositionSpec::WithNoRewind;
                    }
                } else if self.consume_word_pair("NO", "REWIND") {
                    disp = CloseDispositionSpec::WithNoRewind;
                } else if self.consume_word("LOCK") {
                    disp = CloseDispositionSpec::WithLock;
                } else {
                    break;
                }
            }
            files.push(f);
            dispositions.push(disp);
        }
        Stmt::Close {
            files,
            dispositions,
            span: start.merge(self.current_span()),
        }
    }

    /// Parses `COMPUTE dst [ROUNDED] ... = expr [ON SIZE ERROR ...] [END-COMPUTE]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `COMPUTE` verb.
    fn parse_compute_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mut targets = Vec::new();
        while !self.at_end_or_period()
            && self.peek().kind != TokenKind::Eq
            && !self.at_word("EQUAL")
        {
            targets.push(self.parse_single_rounded_target()?);
        }
        if self.peek().kind == TokenKind::Eq || self.consume_word("EQUAL") {
            if self.peek().kind == TokenKind::Eq {
                self.advance();
            }
        } else {
            self.emit_syntax_error(self.current_span(), "expected `=` in COMPUTE statement");
        }
        let expr = self.parse_expr()?;
        let size_error = self.parse_size_error_handlers("END-COMPUTE")?;
        let _ = self.consume_word("END-COMPUTE");
        Ok(Stmt::Compute {
            targets,
            expr,
            size_error,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `CONTINUE [AFTER expr [SECONDS]]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `CONTINUE` verb.
    fn parse_continue_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let after_seconds = if self.consume_word("AFTER") {
            let e = self.parse_expr()?;
            let _ = self.consume_any_word(&["SECONDS", "SECOND"]);
            Some(e)
        } else {
            None
        };
        Ok(Stmt::Continue {
            after_seconds,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `DELETE file [RECORD] [INVALID KEY ...] [END-DELETE]` or `DELETE FILE file ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `DELETE` verb.
    fn parse_delete_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let kind = if self.consume_word("FILE") {
            DeleteKind::PhysicalFile
        } else {
            DeleteKind::Record
        };
        let mut files = Vec::new();
        if kind == DeleteKind::PhysicalFile {
            while !self.at_clause_boundary(&["INVALID", "ON", "EXCEPTION", "NOT", "END-DELETE"])
                && let Some((f, _, _)) = self.eat_word()
            {
                files.push(f);
            }
        } else if let Some((f, _, _)) = self.eat_word() {
            files.push(f);
            let _ = self.consume_word("RECORD");
        }
        let handlers = self.parse_invalid_key_or_exception_handlers("END-DELETE")?;
        let _ = self.consume_word("END-DELETE");
        Ok(Stmt::Delete {
            files,
            kind,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `DISPLAY expr ... [UPON dev] [WITH NO ADVANCING] [ON EXCEPTION ...] [END-DISPLAY]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `DISPLAY` verb.
    fn parse_display_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        self.parse_display_stmt_m19(start)
    }

    /// Parses `DIVIDE a {INTO | BY} b [GIVING ...] [REMAINDER ...] [ON SIZE ERROR ...] [END-DIVIDE]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `DIVIDE` verb.
    fn parse_divide_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let lhs = self.parse_expr()?;
        let orientation = if self.consume_word("BY") {
            DivideOrientation::By
        } else {
            let _ = self.consume_word("INTO");
            DivideOrientation::Into
        };
        let mut rhs = None;
        let mut into_targets = Vec::new();
        if orientation == DivideOrientation::By || self.lookahead_has_giving_before_end() {
            rhs = Some(self.parse_expr()?);
        } else {
            into_targets =
                self.parse_rounded_targets(&["GIVING", "REMAINDER", "SIZE", "END-DIVIDE"])?;
        }
        let giving_targets = if self.consume_word("GIVING") {
            self.parse_rounded_targets(&["REMAINDER", "SIZE", "END-DIVIDE"])?
        } else {
            Vec::new()
        };
        let remainder = if self.consume_word("REMAINDER") {
            Some(self.parse_data_ref()?)
        } else {
            None
        };
        let size_error = self.parse_size_error_handlers("END-DIVIDE")?;
        let _ = self.consume_word("END-DIVIDE");
        Ok(Stmt::Divide {
            orientation,
            lhs,
            rhs,
            into_targets,
            giving_targets,
            remainder,
            size_error,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `ENTRY "name" [USING ...]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `ENTRY` verb.
    fn parse_entry_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let (entry_name, _) = if let Some(p) = self.eat_any_word_or_string_sym() {
            p
        } else {
            (self.interner.intern("__MISSING_ENTRY")?, start)
        };
        let using_params = if self.consume_word("USING") {
            self.parse_formal_using_list()
        } else {
            Vec::new()
        };
        Ok(Stmt::Entry {
            entry_name,
            using_params,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `EVALUATE subject [ALSO ...] WHEN ... [WHEN OTHER ...] [END-EVALUATE]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `EVALUATE` verb.
    fn parse_evaluate_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mut subjects = vec![self.parse_evaluate_subject()?];
        while self.consume_word("ALSO") {
            subjects.push(self.parse_evaluate_subject()?);
        }

        let mut branches = Vec::new();
        let mut when_other = Vec::new();

        while self.at_word("WHEN") {
            if self.peek_word_at(1, "OTHER") {
                self.advance();
                self.advance();
                when_other = self.parse_stmt_block(&["END-EVALUATE"])?;
                break;
            }
            let mut alternatives = Vec::new();
            while self.at_word("WHEN") && !self.peek_word_at(1, "OTHER") {
                self.advance();
                let mut tuple = vec![self.parse_evaluate_object()?];
                while self.consume_word("ALSO") {
                    tuple.push(self.parse_evaluate_object()?);
                }
                alternatives.push(tuple);
            }
            let body = self.parse_stmt_block(&["WHEN", "END-EVALUATE"])?;
            branches.push(EvaluateWhenBranch { alternatives, body });
        }

        let _ = self.consume_word("END-EVALUATE");
        Ok(Stmt::Evaluate {
            subjects,
            branches,
            when_other,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses one `EVALUATE` selection subject (`TRUE`, `FALSE`, condition, or value expression).
    fn parse_evaluate_subject(&mut self) -> Result<EvaluateSubject> {
        if self.at_word("TRUE") || self.at_word("FALSE") || self.subject_is_boolean_condition() {
            Ok(EvaluateSubject::Condition(self.parse_condition()?))
        } else {
            Ok(EvaluateSubject::Expr(self.parse_expr()?))
        }
    }

    /// Parses one `WHEN` selection object (`ANY`, `TRUE`/`FALSE`, `[NOT] low [THRU high]`, or condition).
    fn parse_evaluate_object(&mut self) -> Result<EvaluateObject> {
        if self.consume_word("ANY") {
            return Ok(EvaluateObject::Any);
        }
        if self.at_word("TRUE") || self.at_word("FALSE") {
            return Ok(EvaluateObject::Condition(self.parse_condition()?));
        }
        let saved = self.pos;
        let negated = if self.consume_word("NOT") {
            NegationMode::Negated
        } else {
            NegationMode::Positive
        };
        if self.subject_is_boolean_condition() {
            self.pos = saved;
            return Ok(EvaluateObject::Condition(self.parse_condition()?));
        }
        let low = self.parse_expr()?;
        let high = if self.consume_any_word(&["THRU", "THROUGH"]) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(EvaluateObject::Range { negated, low, high })
    }

    /// Checks whether upcoming tokens before `ALSO`/`WHEN`/`END-EVALUATE` contain relational/boolean operators.
    #[must_use]
    fn subject_is_boolean_condition(&self) -> bool {
        let mut idx = self.pos;
        let mut depth = 0_u32;
        while let Some(tok) = self.tokens.get(idx) {
            match tok.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => depth = depth.saturating_sub(1),
                TokenKind::Eq
                | TokenKind::Ne
                | TokenKind::Lt
                | TokenKind::Le
                | TokenKind::Gt
                | TokenKind::Ge => {
                    if depth == 0 {
                        return true;
                    }
                }
                TokenKind::Word { sym, .. } if depth == 0 => {
                    let w = self.sym_str(sym);
                    if matches!(w, "ALSO" | "WHEN" | "END-EVALUATE") || is_verb_starter(Some(w)) {
                        return false;
                    }
                    if matches!(
                        w,
                        "AND" | "OR" | "EQUAL" | "GREATER" | "LESS" | "NUMERIC" | "ALPHABETIC"
                    ) {
                        return true;
                    }
                }
                TokenKind::Period => return false,
                _ => {}
            }
            idx += 1;
        }
        false
    }

    /// Parses `EXHIBIT [NAMED] [CHANGED] expr ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `EXHIBIT` verb.
    fn parse_exhibit_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let _ = self.consume_word("NAMED");
        let _ = self.consume_word("CHANGED");
        let mut operands = Vec::new();
        while !self.at_clause_boundary(&[]) {
            operands.push(self.parse_expr()?);
        }
        Ok(Stmt::Exhibit {
            operands,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `EXIT [PROGRAM | PARAGRAPH | SECTION | PERFORM [CYCLE] | FUNCTION | METHOD]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `EXIT` verb.
    fn parse_exit_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let kind = if self.consume_word("PROGRAM") {
            let ret = if self.consume_any_word(&["RETURNING", "GIVING"]) {
                Some(self.parse_expr()?)
            } else {
                if self.consume_word("RAISING") {
                    let _ = self.consume_word("EXCEPTION");
                    let _ = self.consume_word("LAST");
                    let _ = self.eat_word();
                }
                None
            };
            ExitKind::Program(ret)
        } else if self.consume_word("PARAGRAPH") {
            ExitKind::Paragraph
        } else if self.consume_word("SECTION") {
            ExitKind::Section
        } else if self.consume_word("PERFORM") {
            if self.consume_word("CYCLE") {
                ExitKind::PerformCycle
            } else {
                ExitKind::PerformBreak
            }
        } else if self.consume_word("FUNCTION") {
            ExitKind::Function
        } else if self.consume_word("METHOD") {
            ExitKind::Method
        } else {
            ExitKind::PlainEndpoint
        };
        Ok(Stmt::Exit {
            kind,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `FREE target ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `FREE` verb.
    fn parse_free_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mut targets = Vec::new();
        while !self.at_clause_boundary(&[]) {
            targets.push(self.parse_data_ref()?);
        }
        Ok(Stmt::Free {
            targets,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `GENERATE report-or-group`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `GENERATE` verb.
    fn parse_generate_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let target = if let Some((s, _, _)) = self.eat_word() {
            s
        } else {
            self.interner.intern("__MISSING_GENERATE")?
        };
        Ok(Stmt::Generate {
            target,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `GO [TO] [proc ...] [DEPENDING [ON] expr]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `GO` verb.
    fn parse_goto_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let _ = self.consume_word("TO");
        let mut targets = Vec::new();
        while !self.at_clause_boundary(&["DEPENDING"]) {
            targets.push(self.parse_qualified_name()?);
        }
        let depending_on = if self.consume_word("DEPENDING") {
            let _ = self.consume_word("ON");
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Stmt::GoTo {
            targets,
            depending_on,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `GOBACK [RETURNING/GIVING expr]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `GOBACK` verb.
    fn parse_goback_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let has_status = if self.consume_any_word(&["RETURNING", "GIVING"]) {
            true
        } else if self.consume_word("WITH") {
            let _ = self.consume_any_word(&["NORMAL", "ERROR"]);
            let _ = self.consume_word("STATUS");
            true
        } else {
            false
        };
        let returning = if has_status && !self.at_clause_boundary(&[]) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Stmt::Goback {
            returning,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `IF cond [THEN] ... [ELSE ...] [END-IF]` (binding `ELSE` to innermost open `IF`!).
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `IF` verb.
    fn parse_if_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let condition = self.parse_condition()?;
        let _ = self.consume_word("THEN");
        let then_body = self.parse_stmt_block(&["ELSE", "END-IF"])?;
        let else_body = if self.consume_word("ELSE") {
            self.parse_stmt_block(&["ELSE", "END-IF"])?
        } else {
            Vec::new()
        };
        let _ = self.consume_word("END-IF");
        Ok(Stmt::If {
            condition,
            then_body,
            else_body,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `INITIALIZE target ... [WITH FILLER] [ALL|cat TO VALUE] [REPLACING ...] [DEFAULT]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `INITIALIZE` verb.
    fn parse_initialize_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mut targets = Vec::new();
        while !self.at_clause_boundary(&[
            "WITH",
            "FILLER",
            "ALL",
            "REPLACING",
            "DEFAULT",
            "TO",
            "VALUE",
        ]) {
            targets.push(self.parse_data_ref()?);
        }

        let mut filler = InitializeFillerMode::ExcludeFiller;
        let mut value_mode = InitializeValueMode::CategoryDefault;
        let mut replacing = Vec::new();

        while !self.at_clause_boundary(&[]) {
            let _ = self.consume_word("WITH");
            if self.consume_word("FILLER") {
                filler = InitializeFillerMode::IncludeFiller;
            } else if self.consume_any_word(&["ALL", "VALUE", "TO"]) {
                let _ = self.consume_word("TO");
                let _ = self.consume_word("VALUE");
                value_mode = InitializeValueMode::DefinedValue;
            } else if self.consume_word("REPLACING") {
                while !self.at_clause_boundary(&["WITH", "DEFAULT"])
                    && let Some((cat_sym, _, _)) = self.eat_word()
                {
                    let _ = self.consume_word("DATA");
                    let _ = self.consume_word("BY");
                    let rep = self.parse_expr()?;
                    replacing.push((cat_sym, rep));
                }
            } else if self.consume_word("DEFAULT") {
                // Explicit category default.
            } else {
                break;
            }
        }

        Ok(Stmt::Initialize {
            targets,
            filler,
            value_mode,
            replacing,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `INITIATE report ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `INITIATE` verb.
    fn parse_initiate_stmt(&mut self, start: SourceSpan) -> Stmt {
        self.advance();
        let mut reports = Vec::new();
        while !self.at_clause_boundary(&[])
            && let Some((r, _, _)) = self.eat_word()
        {
            reports.push(r);
        }
        Stmt::Initiate {
            reports,
            span: start.merge(self.current_span()),
        }
    }

    /// Parses `INSPECT target TALLYING / REPLACING / CONVERTING ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `INSPECT` verb.
    fn parse_inspect_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let target = self.parse_data_ref()?;
        let mut operations = Vec::new();

        while !self.at_end_or_period() {
            if self.consume_word("TALLYING") {
                while !self.at_clause_boundary(&["REPLACING", "CONVERTING"]) {
                    let counter = self.parse_data_ref()?;
                    let _ = self.consume_word("FOR");
                    while let Some(qual) = self.try_eat_inspect_qualifier() {
                        let pattern = if qual == InspectQualifier::Characters {
                            None
                        } else {
                            Some(self.parse_expr()?)
                        };
                        let bounds = self.parse_inspect_bounds()?;
                        operations.push(InspectOperation::Tallying {
                            counter: counter.clone(),
                            qualifier: qual,
                            pattern,
                            bounds,
                        });
                    }
                }
            } else if self.consume_word("REPLACING") {
                while let Some(qual) = self.try_eat_inspect_qualifier() {
                    let pattern = if qual == InspectQualifier::Characters {
                        None
                    } else {
                        Some(self.parse_expr()?)
                    };
                    let _ = self.consume_word("BY");
                    let by = self.parse_expr()?;
                    let bounds = self.parse_inspect_bounds()?;
                    operations.push(InspectOperation::Replacing {
                        qualifier: qual,
                        pattern,
                        by,
                        bounds,
                    });
                }
            } else if self.consume_word("CONVERTING") {
                let from = self.parse_expr()?;
                let _ = self.consume_word("TO");
                let to = self.parse_expr()?;
                let bounds = self.parse_inspect_bounds()?;
                operations.push(InspectOperation::Converting { from, to, bounds });
            } else {
                break;
            }
        }

        Ok(Stmt::Inspect {
            target,
            operations,
            span: start.merge(self.current_span()),
        })
    }

    /// Consumes `CHARACTERS`, `ALL`, `LEADING`, `FIRST`, or `TRAILING` in `INSPECT`.
    fn try_eat_inspect_qualifier(&mut self) -> Option<InspectQualifier> {
        if self.consume_word("CHARACTERS") {
            Some(InspectQualifier::Characters)
        } else if self.consume_word("ALL") {
            Some(InspectQualifier::All)
        } else if self.consume_word("LEADING") {
            Some(InspectQualifier::Leading)
        } else if self.consume_word("FIRST") {
            Some(InspectQualifier::First)
        } else if self.consume_word("TRAILING") {
            Some(InspectQualifier::Trailing)
        } else {
            None
        }
    }

    /// Parses zero or more `[BEFORE|AFTER [INITIAL] expr]` window clauses in `INSPECT`.
    fn parse_inspect_bounds(&mut self) -> Result<Vec<InspectBound>> {
        let mut bounds = Vec::new();
        while matches!(self.current_word(), Some("BEFORE" | "AFTER")) {
            let side = if self.consume_word("BEFORE") {
                BoundSide::Before
            } else {
                let _ = self.consume_word("AFTER");
                BoundSide::After
            };
            let _ = self.consume_word("INITIAL");
            let pattern = self.parse_expr()?;
            bounds.push(InspectBound { side, pattern });
        }
        Ok(bounds)
    }

    /// Parses `INVOKE receiver method [USING ...] [RETURNING ...] [END-INVOKE]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `INVOKE` verb.
    fn parse_invoke_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let receiver = self.parse_expr()?;
        let method = self.parse_expr()?;
        let mut args = Vec::new();
        if self.consume_word("USING") {
            let mut mode = PassingMode::ByReference;
            while !self.at_call_arg_boundary(&["RETURNING", "END-INVOKE"]) {
                let arg = self.parse_single_call_arg(mode)?;
                if let crate::ast::proc_div::CallArg::Value { mode: m, .. } = &arg {
                    mode = *m;
                }
                args.push(arg);
            }
        }
        let returning = if self.consume_word("RETURNING") {
            Some(self.parse_data_ref()?)
        } else {
            None
        };
        let _ = self.consume_word("END-INVOKE");
        Ok(Stmt::Invoke {
            receiver,
            method,
            args,
            returning,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `JSON GENERATE ...` or `JSON PARSE ... [END-JSON]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `JSON` verb.
    fn parse_json_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        if self.consume_word("GENERATE") {
            let destination = self.parse_data_ref()?;
            let _ = self.consume_word("FROM");
            let source = self.parse_markup_source_ref()?;
            let mut count_in = None;
            let mut name_mappings = Vec::new();
            let mut suppress_clauses = Vec::new();
            let mut converting_clauses = Vec::new();
            let stop = &[
                "COUNT",
                "NAME",
                "SUPPRESS",
                "CONVERTING",
                "ON",
                "EXCEPTION",
                "NOT",
                "END-JSON",
            ];
            while !self.at_markup_boundary(&["ON", "EXCEPTION", "NOT", "END-JSON"]) {
                if self.consume_word("COUNT") {
                    let _ = self.consume_word("IN");
                    count_in = Some(self.parse_data_ref()?);
                } else if self.consume_word("NAME") {
                    let _ = self.consume_word("OF");
                    self.parse_markup_name_clauses(stop, &mut name_mappings)?;
                } else if self.consume_word("SUPPRESS") {
                    self.parse_markup_suppress_clauses(stop, &mut suppress_clauses)?;
                } else if self.consume_word("CONVERTING") {
                    self.parse_json_converting_clauses(stop, &mut converting_clauses)?;
                } else {
                    self.advance();
                }
            }
            let handlers = self.parse_event_handlers("EXCEPTION", "END-JSON")?;
            let _ = self.consume_word("END-JSON");
            Ok(Stmt::JsonGenerate {
                destination,
                source,
                count_in,
                name_mappings,
                suppress_clauses,
                converting_clauses,
                handlers,
                span: start.merge(self.current_span()),
            })
        } else {
            let _ = self.consume_word("PARSE");
            let source = self.parse_expr()?;
            let _ = self.consume_word("INTO");
            let destination = self.parse_data_ref()?;
            let mut detail = JsonDetailPresence::Standard;
            let mut name_mappings = Vec::new();
            let mut suppress_clauses = Vec::new();
            let mut converting_clauses = Vec::new();
            let stop = &[
                "WITH",
                "DETAIL",
                "NAME",
                "SUPPRESS",
                "CONVERTING",
                "ON",
                "EXCEPTION",
                "NOT",
                "END-JSON",
            ];
            while !self.at_markup_boundary(&["ON", "EXCEPTION", "NOT", "END-JSON"]) {
                if self.consume_word("WITH") {
                    if self.consume_word("DETAIL") {
                        detail = JsonDetailPresence::WithDetail;
                    }
                } else if self.consume_word("DETAIL") {
                    detail = JsonDetailPresence::WithDetail;
                } else if self.consume_word("NAME") {
                    let _ = self.consume_word("OF");
                    self.parse_markup_name_clauses(stop, &mut name_mappings)?;
                } else if self.consume_word("SUPPRESS") {
                    self.parse_markup_suppress_clauses(stop, &mut suppress_clauses)?;
                } else if self.consume_word("CONVERTING") {
                    self.parse_json_converting_clauses(stop, &mut converting_clauses)?;
                } else {
                    self.advance();
                }
            }
            let handlers = self.parse_event_handlers("EXCEPTION", "END-JSON")?;
            let _ = self.consume_word("END-JSON");
            Ok(Stmt::JsonParse {
                source,
                destination,
                detail,
                name_mappings,
                suppress_clauses,
                converting_clauses,
                handlers,
                span: start.merge(self.current_span()),
            })
        }
    }

    /// Parses `MERGE sd-file ON KEY ... USING ... {GIVING ... | OUTPUT PROCEDURE ...}`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `MERGE` verb.
    fn parse_merge_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let sd_file = if let Some((s, _, _)) = self.eat_word() {
            s
        } else {
            self.interner.intern("__MISSING_MERGE")?
        };
        let keys = self.parse_sort_merge_keys()?;
        let mut collating_sequence = None;
        let mut using_files = Vec::new();
        let mut giving_files = Vec::new();
        let mut output_proc = None;

        while !self.at_clause_boundary(&[]) {
            if self.consume_word("COLLATING") || self.at_word("SEQUENCE") {
                let _ = self.consume_word("SEQUENCE");
                let _ = self.consume_word("IS");
                collating_sequence = self.eat_word().map(|t| t.0);
            } else if self.consume_word("USING") {
                while !self.at_clause_boundary(&["GIVING", "OUTPUT"])
                    && let Some((f, _, _)) = self.eat_word()
                {
                    using_files.push(f);
                }
            } else if self.consume_word("GIVING") {
                while !self.at_clause_boundary(&["OUTPUT"])
                    && let Some((f, _, _)) = self.eat_word()
                {
                    giving_files.push(f);
                }
            } else if self.consume_word("OUTPUT") {
                let _ = self.consume_word("PROCEDURE");
                let _ = self.consume_word("IS");
                output_proc = Some(self.parse_proc_range()?);
            } else {
                break;
            }
        }

        Ok(Stmt::Merge {
            sd_file,
            keys,
            collating_sequence,
            using_files,
            giving_files,
            output_proc,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `MOVE [CORRESPONDING|CORR] src TO dst ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `MOVE` verb.
    fn parse_move_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mode = if self.consume_any_word(&["CORRESPONDING", "CORR"]) {
            CorrespondingMode::Corresponding
        } else {
            CorrespondingMode::Standard
        };
        let source = self.parse_expr()?;
        if !self.consume_word("TO") {
            self.emit_syntax_error(self.current_span(), "expected `TO` in MOVE statement");
        }
        let mut targets = Vec::new();
        while !self.at_clause_boundary(&[]) {
            targets.push(self.parse_data_ref()?);
        }
        Ok(Stmt::Move {
            mode,
            source,
            targets,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `MULTIPLY a BY b ... [GIVING dst ...] [ON SIZE ERROR ...] [END-MULTIPLY]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `MULTIPLY` verb.
    fn parse_multiply_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let lhs = self.parse_expr()?;
        if !self.consume_word("BY") {
            self.emit_syntax_error(self.current_span(), "expected `BY` in MULTIPLY statement");
        }
        let mut rhs = None;
        let mut by_targets = Vec::new();
        if self.lookahead_has_giving_before_end() {
            rhs = Some(self.parse_expr()?);
        } else {
            by_targets = self.parse_rounded_targets(&["GIVING", "SIZE", "END-MULTIPLY"])?;
        }
        let giving_targets = if self.consume_word("GIVING") {
            self.parse_rounded_targets(&["SIZE", "END-MULTIPLY"])?
        } else {
            Vec::new()
        };
        let size_error = self.parse_size_error_handlers("END-MULTIPLY")?;
        let _ = self.consume_word("END-MULTIPLY");
        Ok(Stmt::Multiply {
            lhs,
            rhs,
            by_targets,
            giving_targets,
            size_error,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `OPEN {INPUT | OUTPUT | I-O | EXTEND} file ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `OPEN` verb.
    fn parse_open_stmt(&mut self, start: SourceSpan) -> Stmt {
        self.advance();
        let mut entries = Vec::new();
        let mut options = Vec::new();
        let mut mode = OpenMode::Input;
        while !self.at_clause_boundary(&[]) {
            if self.consume_word("INPUT") {
                mode = OpenMode::Input;
            } else if self.consume_word("OUTPUT") {
                mode = OpenMode::Output;
            } else if self.consume_word("I-O") {
                mode = OpenMode::Io;
            } else if self.consume_word("EXTEND") {
                mode = OpenMode::Extend;
            } else if let Some((f, _, _)) = self.eat_word() {
                let opt = self.parse_open_entry_option();
                entries.push((mode, f));
                options.push(opt);
            } else {
                break;
            }
        }
        Stmt::Open {
            entries,
            options,
            span: start.merge(self.current_span()),
        }
    }

    /// Parses optional trailing `WITH NO REWIND` / `REVERSED` / `WITH LOCK` / `SHARING ...` on one `OPEN` entry.
    fn parse_open_entry_option(&mut self) -> OpenOptionSpec {
        let mut opt = OpenOptionSpec::Standard;
        loop {
            if self.consume_word("REVERSED") {
                opt = OpenOptionSpec::Reversed;
            } else if self.consume_word("SHARING") {
                let _ = self.consume_word("WITH");
                opt = if self.consume_word("NO") {
                    let _ = self.consume_word("OTHER");
                    OpenOptionSpec::SharingNoOther
                } else if self.consume_word("READ") {
                    let _ = self.consume_word("ONLY");
                    OpenOptionSpec::SharingReadOnly
                } else {
                    let _ = self.consume_word("ALL");
                    let _ = self.consume_word("OTHER");
                    OpenOptionSpec::SharingAllOther
                };
            } else if self.consume_word("WITH") {
                if self.consume_word("LOCK") {
                    opt = OpenOptionSpec::WithLock;
                } else if self.consume_word("NO") {
                    if self.consume_word("REWIND") {
                        opt = OpenOptionSpec::WithNoRewind;
                    } else {
                        let _ = self.consume_word("OTHER");
                    }
                }
            } else if self.consume_word_pair("NO", "REWIND") {
                opt = OpenOptionSpec::WithNoRewind;
            } else {
                break;
            }
        }
        opt
    }

    /// Parses inline (`PERFORM [control] body END-PERFORM`) and out-of-line (`PERFORM p1 [THRU p2] [control]`) `PERFORM`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `PERFORM` verb.
    fn parse_perform_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        if self.starts_inline_perform() {
            let control = self.parse_perform_control()?;
            let body = self.parse_stmt_block(&["END-PERFORM"])?;
            let _ = self.consume_word("END-PERFORM");
            Ok(Stmt::PerformInline {
                control,
                body,
                span: start.merge(self.current_span()),
            })
        } else {
            let (proc_start, thru) = self.parse_proc_range()?;
            let control = self.parse_perform_control()?;
            let _ = self.consume_word("END-PERFORM");
            Ok(Stmt::PerformOutofLine {
                start: proc_start,
                thru,
                control,
                span: start.merge(self.current_span()),
            })
        }
    }

    /// Checks whether the tokens right after `PERFORM` begin an inline `PERFORM` rather than a procedure name.
    #[must_use]
    fn starts_inline_perform(&self) -> bool {
        // `PERFORM 5 TIMES body... END-PERFORM` vs `PERFORM 0100.` (`0100` is an integer paragraph name!).
        if matches!(self.peek().kind, TokenKind::IntegerLit(_)) {
            return self.peek_word_at(1, "TIMES")
                && !self.peek_at(2).is_some_and(|t| t.kind == TokenKind::Period);
        }
        let Some(cw) = self.current_word() else {
            return true;
        };
        if matches!(
            cw,
            "UNTIL" | "VARYING" | "WITH" | "TEST" | "END-PERFORM" | "FOREVER"
        ) || is_verb_starter(Some(cw))
        {
            return true;
        }
        // `PERFORM WS-COUNT TIMES body... END-PERFORM` where `TIMES` is followed by inline statements!
        if self.peek_word_at(1, "TIMES") {
            let after_times = self.peek_word_str_at(2);
            return is_verb_starter(after_times) || after_times == Some("END-PERFORM");
        }
        false
    }

    /// Parses `[n TIMES | [WITH TEST BEFORE|AFTER] {UNTIL cond | VARYING ...}]`.
    fn parse_perform_control(&mut self) -> Result<PerformControl> {
        let mut test_pos = PerformTestPosition::Before;
        let _ = self.consume_word("WITH");
        if self.consume_word("TEST") {
            if self.consume_word("AFTER") {
                test_pos = PerformTestPosition::After;
            } else {
                let _ = self.consume_word("BEFORE");
            }
        }
        if self.consume_word("FOREVER") {
            let f_sp = self.current_span();
            return Ok(PerformControl::Until {
                test_pos,
                condition: crate::ast::proc_div::Condition::BooleanConst {
                    value: TruthValue::False,
                    span: f_sp,
                },
            });
        }
        if self.consume_word("UNTIL") {
            let condition = self.parse_condition()?;
            return Ok(PerformControl::Until {
                test_pos,
                condition,
            });
        }
        if self.consume_word("VARYING") {
            let mut clauses = vec![self.parse_varying_clause()?];
            while self.consume_word("AFTER") {
                clauses.push(self.parse_varying_clause()?);
            }
            return Ok(PerformControl::Varying { test_pos, clauses });
        }
        // Check `<expr> TIMES`
        if !self.at_clause_boundary(&[]) {
            let saved = self.pos;
            let count_expr = self.parse_expr()?;
            if self.consume_word("TIMES") {
                return Ok(PerformControl::Times(count_expr));
            }
            self.pos = saved;
        }
        Ok(PerformControl::Once)
    }

    /// Parses `counter FROM expr BY expr UNTIL cond`.
    fn parse_varying_clause(&mut self) -> Result<PerformVaryingClause> {
        let counter = self.parse_data_ref()?;
        let _ = self.consume_word("FROM");
        let from = self.parse_expr()?;
        let _ = self.consume_word("BY");
        let by = self.parse_expr()?;
        let _ = self.consume_word("UNTIL");
        let until = self.parse_condition()?;
        Ok(PerformVaryingClause {
            counter,
            from,
            by,
            until,
        })
    }

    /// Parses `proc-1 [{THRU | THROUGH} proc-2]`.
    fn parse_proc_range(&mut self) -> Result<(QualifiedName, Option<QualifiedName>)> {
        let p1 = self.parse_qualified_name()?;
        let p2 = if self.consume_any_word(&["THRU", "THROUGH"]) {
            Some(self.parse_qualified_name()?)
        } else {
            None
        };
        Ok((p1, p2))
    }

    /// Parses `RAISE [EXCEPTION ec-name | obj]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `RAISE` verb.
    fn parse_raise_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let _ = self.consume_word("EXCEPTION");
        let target = if self.at_clause_boundary(&[]) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        Ok(Stmt::Raise {
            target,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `READ file [NEXT|PREVIOUS] [RECORD] [INTO dst] [KEY IS k] [AT END / INVALID KEY ...] [END-READ]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `READ` verb.
    fn parse_read_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let file = if let Some((s, _, _)) = self.eat_word() {
            s
        } else {
            self.interner.intern("__MISSING_READ_FILE")?
        };
        let mut direction = if self.consume_word("PREVIOUS") {
            ReadDirection::Previous
        } else if self.consume_word("NEXT") {
            ReadDirection::ExplicitNext
        } else {
            ReadDirection::NextOrDefault
        };
        let _ = self.consume_word("RECORD");
        let mut into = None;
        let mut key = None;
        let mut lock_mode = ReadLockSpec::Default;

        while !self.at_clause_boundary(&["AT", "END", "INVALID", "NOT", "END-READ"]) {
            if self.consume_word("INTO") {
                into = Some(self.parse_data_ref()?);
            } else if self.consume_word("KEY") {
                let _ = self.consume_word("IS");
                key = Some(self.parse_data_ref()?);
                direction = ReadDirection::Random;
            } else if self.consume_word("IGNORING") || self.consume_word("IGNORE") {
                let _ = self.consume_word("LOCK");
                lock_mode = ReadLockSpec::IgnoreLock;
            } else if self.consume_word("WITH") {
                if self.consume_word("NO") {
                    let _ = self.consume_word("LOCK");
                    lock_mode = ReadLockSpec::WithNoLock;
                } else if self.consume_word("KEPT") {
                    let _ = self.consume_word("LOCK");
                    lock_mode = ReadLockSpec::WithKeptLock;
                } else if self.consume_word("LOCK") {
                    lock_mode = ReadLockSpec::WithLock;
                } else {
                    let _ = self.consume_word("WAIT");
                }
            } else if self.consume_word_pair("NO", "LOCK") {
                lock_mode = ReadLockSpec::WithNoLock;
            } else if self.consume_word("LOCK") {
                lock_mode = ReadLockSpec::WithLock;
            } else if self.consume_word("WAIT") {
                // Consume optional WAIT modifier.
            } else {
                break;
            }
        }
        let handlers = self.parse_read_or_return_handlers("END-READ")?;
        let _ = self.consume_word("END-READ");
        Ok(Stmt::Read {
            file,
            direction,
            into,
            key,
            lock_mode,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `RELEASE record [FROM src]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `RELEASE` verb.
    fn parse_release_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let record = self.parse_data_ref()?;
        let from = if self.consume_word("FROM") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Stmt::Release {
            record,
            from,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `RESUME [AT NEXT STATEMENT | AT proc-name]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `RESUME` verb.
    fn parse_resume_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let _ = self.consume_word("AT");
        let target = if self.consume_word("NEXT") {
            let _ = self.consume_word("STATEMENT");
            None
        } else if !self.at_clause_boundary(&[]) {
            Some(self.parse_qualified_name()?)
        } else {
            None
        };
        Ok(Stmt::Resume {
            target,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `RETURN sd-file [RECORD] [INTO dst] [AT END ...] [END-RETURN]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `RETURN` verb.
    fn parse_return_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let sd_file = if let Some((s, _, _)) = self.eat_word() {
            s
        } else {
            self.interner.intern("__MISSING_RETURN_SD")?
        };
        let _ = self.consume_word("RECORD");
        let into = if self.consume_word("INTO") {
            Some(self.parse_data_ref()?)
        } else {
            None
        };
        let handlers = self.parse_read_or_return_handlers("END-RETURN")?;
        let _ = self.consume_word("END-RETURN");
        Ok(Stmt::Return {
            sd_file,
            into,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `REWRITE record [FROM src] [INVALID KEY ...] [END-REWRITE]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `REWRITE` verb.
    fn parse_rewrite_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let target_mode = if self.consume_word("FILE") {
            WriteTargetMode::FileName
        } else {
            WriteTargetMode::RecordName
        };
        let record = self.parse_data_ref()?;
        let from = if self.consume_word("FROM") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        while self.consume_any_word(&["WITH", "NO", "LOCK"]) {}
        let handlers = self.parse_invalid_key_or_exception_handlers("END-REWRITE")?;
        let _ = self.consume_word("END-REWRITE");
        Ok(Stmt::Rewrite {
            record,
            target_mode,
            from,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `SEARCH [ALL] table [VARYING idx] [AT END ...] WHEN cond ... [END-SEARCH]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `SEARCH` verb.
    fn parse_search_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mode = if self.consume_word("ALL") {
            SearchMode::BinaryAll
        } else {
            SearchMode::Serial
        };
        let table = self.parse_data_ref()?;
        let varying = if self.consume_word("VARYING") {
            Some(self.parse_data_ref()?)
        } else {
            None
        };
        let at_end = if self.consume_word_pair("AT", "END") || self.consume_word("END") {
            self.parse_stmt_block(&["WHEN", "END-SEARCH"])?
        } else {
            Vec::new()
        };
        let mut when_arms = Vec::new();
        while self.consume_word("WHEN") {
            let cond = self.parse_condition()?;
            let body = self.parse_stmt_block(&["WHEN", "END-SEARCH"])?;
            when_arms.push((cond, body));
        }
        let _ = self.consume_word("END-SEARCH");
        Ok(Stmt::Search {
            mode,
            table,
            varying,
            at_end,
            when_arms,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `SET ...` across index, pointer, `88` condition-name, and switch forms.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `SET` verb.
    fn parse_set_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mut actions = Vec::new();

        while !self.at_clause_boundary(&[]) {
            if self.consume_word_pair("LAST", "EXCEPTION") {
                let _ = self.consume_word("TO");
                let _ = self.consume_word("OFF");
                actions.push(SetAction::LastExceptionOff);
                continue;
            }
            if self.consume_word("ENVIRONMENT") {
                let name = self.parse_expr()?;
                let _ = self.consume_word("WITH");
                let _ = self.consume_word("TO");
                let value = self.parse_expr()?;
                actions.push(SetAction::Environment { name, value });
                continue;
            }
            let mut targets = Vec::new();
            while !self.at_end_or_period()
                && !matches!(
                    self.current_word(),
                    Some("TO" | "UP" | "DOWN" | "ATTRIBUTE" | "ATTRIBUTES")
                )
            {
                targets.push(self.parse_data_ref()?);
            }
            if let Some(first) = targets.first().cloned()
                && let Some(act) = self.try_parse_set_screen_attribute(first)
            {
                actions.push(act);
            } else if self.consume_word("UP") {
                let _ = self.consume_word("BY");
                let amount = self.parse_expr()?;
                actions.push(SetAction::StepIndex {
                    targets,
                    direction: StepDirection::Up,
                    amount,
                });
            } else if self.consume_word("DOWN") {
                let _ = self.consume_word("BY");
                let amount = self.parse_expr()?;
                actions.push(SetAction::StepIndex {
                    targets,
                    direction: StepDirection::Down,
                    amount,
                });
            } else if self.consume_word("TO") {
                if self.consume_word("TRUE") {
                    actions.push(SetAction::ConditionState {
                        targets,
                        state: TruthValue::True,
                    });
                } else if self.consume_word("FALSE") {
                    actions.push(SetAction::ConditionState {
                        targets,
                        state: TruthValue::False,
                    });
                } else if self.consume_word("ON") {
                    if let Some(first) = targets.first() {
                        actions.push(SetAction::SwitchState {
                            switch_name: first.name.base,
                            state: TruthValue::True,
                        });
                    }
                } else if self.consume_word("OFF") {
                    if let Some(first) = targets.first() {
                        actions.push(SetAction::SwitchState {
                            switch_name: first.name.base,
                            state: TruthValue::False,
                        });
                    }
                } else if self.consume_word("ENTRY") {
                    let entry = self.parse_expr()?;
                    actions.push(SetAction::ToEntry { targets, entry });
                } else if self.peek_address_of_program_or_entry() {
                    let _ = self.consume_word("ADDRESS");
                    let _ = self.consume_word("OF");
                    self.advance();
                    let entry = self.parse_expr()?;
                    actions.push(SetAction::ToEntry { targets, entry });
                } else {
                    let value = self.parse_expr()?;
                    actions.push(SetAction::ToValue { targets, value });
                }
            } else {
                break;
            }
        }

        Ok(Stmt::Set {
            actions,
            span: start.merge(self.current_span()),
        })
    }

    /// Returns `true` when positioned at `ADDRESS [OF] {PROGRAM | ENTRY}`.
    #[must_use]
    fn peek_address_of_program_or_entry(&self) -> bool {
        if !self.peek_word_at(0, "ADDRESS") {
            return false;
        }
        let next_idx = usize::from(self.peek_word_at(1, "OF"));
        matches!(
            self.peek_word_str_at(1 + next_idx),
            Some("PROGRAM" | "ENTRY")
        )
    }

    /// Parses file `SORT` and ISO 2002+ in-memory table `SORT`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `SORT` verb.
    fn parse_sort_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let first_ref = self.parse_data_ref()?;
        let keys = self.parse_sort_merge_keys()?;
        let mut duplicates_in_order = TruthValue::False;
        let mut collating_sequence = None;
        let mut using_files = Vec::new();
        let mut input_proc = None;
        let mut giving_files = Vec::new();
        let mut output_proc = None;

        while !self.at_clause_boundary(&[]) {
            let _ = self.consume_word("WITH");
            if self.consume_word("DUPLICATES") {
                let _ = self.consume_word("IN");
                let _ = self.consume_word("ORDER");
                duplicates_in_order = TruthValue::True;
            } else if self.consume_word("COLLATING") || self.at_word("SEQUENCE") {
                let _ = self.consume_word("SEQUENCE");
                let _ = self.consume_word("IS");
                collating_sequence = self.eat_word().map(|t| t.0);
            } else if self.consume_word("USING") {
                while !self.at_clause_boundary(&["GIVING", "OUTPUT"])
                    && let Some((f, _, _)) = self.eat_word()
                {
                    using_files.push(f);
                }
            } else if self.consume_word("INPUT") {
                let _ = self.consume_word("PROCEDURE");
                let _ = self.consume_word("IS");
                input_proc = Some(self.parse_proc_range()?);
            } else if self.consume_word("GIVING") {
                while !self.at_clause_boundary(&["OUTPUT"])
                    && let Some((f, _, _)) = self.eat_word()
                {
                    giving_files.push(f);
                }
            } else if self.consume_word("OUTPUT") {
                let _ = self.consume_word("PROCEDURE");
                let _ = self.consume_word("IS");
                output_proc = Some(self.parse_proc_range()?);
            } else {
                break;
            }
        }

        let is_file_sort = !using_files.is_empty()
            || input_proc.is_some()
            || !giving_files.is_empty()
            || output_proc.is_some();
        if is_file_sort {
            Ok(Stmt::SortFile {
                sd_file: first_ref.name.base,
                keys,
                duplicates_in_order,
                collating_sequence,
                using_files,
                input_proc,
                giving_files,
                output_proc,
                span: start.merge(self.current_span()),
            })
        } else {
            Ok(Stmt::SortTable {
                table: first_ref,
                keys,
                duplicates_in_order,
                collating_sequence,
                span: start.merge(self.current_span()),
            })
        }
    }

    /// Parses `[ON] {ASCENDING | DESCENDING} [KEY] [IS] key ...` clauses for `SORT`/`MERGE`.
    fn parse_sort_merge_keys(&mut self) -> Result<Vec<(KeyOrder, Vec<DataRef>)>> {
        let mut keys = Vec::new();
        loop {
            let _ = self.consume_word("ON");
            let order = if self.consume_word("ASCENDING") {
                KeyOrder::Ascending
            } else if self.consume_word("DESCENDING") {
                KeyOrder::Descending
            } else {
                break;
            };
            let _ = self.consume_word("KEY");
            let _ = self.consume_word("IS");
            let mut key_refs = Vec::new();
            while !self.at_clause_boundary(&[
                "ON",
                "ASCENDING",
                "DESCENDING",
                "WITH",
                "DUPLICATES",
                "COLLATING",
                "SEQUENCE",
                "USING",
                "INPUT",
                "GIVING",
                "OUTPUT",
            ]) {
                key_refs.push(self.parse_data_ref()?);
            }
            keys.push((order, key_refs));
        }
        Ok(keys)
    }

    /// Parses `START file [KEY IS rel_op key] [INVALID KEY ...] [END-START]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `START` verb.
    fn parse_start_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let file = if let Some((s, _, _)) = self.eat_word() {
            s
        } else {
            self.interner.intern("__MISSING_START_FILE")?
        };
        let mut position = StartPositionSpec::KeyRelation;
        let mut key_rel = None;
        let mut key_length = None;
        if self.consume_word("FIRST") {
            position = StartPositionSpec::First;
        } else if self.consume_word("LAST") {
            position = StartPositionSpec::Last;
        } else if self.consume_word("KEY") {
            let _ = self.consume_word("IS");
            if self.consume_word("FIRST") {
                position = StartPositionSpec::First;
            } else if self.consume_word("LAST") {
                position = StartPositionSpec::Last;
            } else {
                let neg = self.consume_word("NOT");
                let base_op = self
                    .try_eat_rel_op_without_not()
                    .unwrap_or(crate::ast::proc_div::RelOp::Eq);
                let op = if neg {
                    match base_op {
                        crate::ast::proc_div::RelOp::Lt => crate::ast::proc_div::RelOp::Ge,
                        crate::ast::proc_div::RelOp::Gt => crate::ast::proc_div::RelOp::Le,
                        _ => crate::ast::proc_div::RelOp::Ne,
                    }
                } else {
                    base_op
                };
                let k = self.parse_data_ref()?;
                key_rel = Some((op, k));
            }
        }
        let _ = self.consume_word("WITH");
        if self.consume_any_word(&["SIZE", "LENGTH"]) {
            let _ = self.consume_word("IS");
            key_length = Some(self.parse_expr()?);
        }
        let handlers = self.parse_invalid_key_or_exception_handlers("END-START")?;
        let _ = self.consume_word("END-START");
        Ok(Stmt::Start {
            file,
            key_rel,
            position,
            key_length,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `STOP {RUN [RETURNING/WITH STATUS expr] | literal}`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `STOP` verb.
    fn parse_stop_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let kind = if self.consume_word("RUN") {
            let status = if self.consume_any_word(&["RETURNING", "GIVING"]) {
                Some(self.parse_expr()?)
            } else if self.consume_word("WITH") {
                let is_err = self.consume_word("ERROR");
                if !is_err {
                    let _ = self.consume_word("NORMAL");
                }
                let _ = self.consume_word("STATUS");
                if self.at_clause_boundary(&[]) {
                    let code_sym = self.interner.intern(if is_err { "1" } else { "0" })?;
                    Some(Expr::Literal {
                        atom: crate::ast::data_div::ValueAtom::Integer(code_sym),
                        span: start,
                    })
                } else {
                    Some(self.parse_expr()?)
                }
            } else if !self.at_clause_boundary(&[]) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            StopKind::Run(status)
        } else {
            StopKind::Literal(self.parse_expr()?)
        };
        Ok(Stmt::Stop {
            kind,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `STRING src ... DELIMITED BY {SIZE|pat} ... INTO dst [WITH POINTER ptr] [ON OVERFLOW ...] [END-STRING]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `STRING` verb.
    fn parse_string_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mut groups = Vec::new();
        while !self.at_end_or_period() && !self.at_word("INTO") {
            let mut sources = Vec::new();
            while !self.at_end_or_period()
                && !matches!(self.current_word(), Some("DELIMITED" | "INTO"))
            {
                sources.push(self.parse_expr()?);
            }
            let delimiter = if self.consume_word("DELIMITED") {
                let _ = self.consume_word("BY");
                if self.consume_word("SIZE") {
                    None
                } else {
                    Some(self.parse_expr()?)
                }
            } else {
                None
            };
            if sources.is_empty() {
                break;
            }
            groups.push(StringSourceGroup { sources, delimiter });
        }
        let _ = self.consume_word("INTO");
        let into = self.parse_data_ref()?;
        let _ = self.consume_word("WITH");
        let pointer = if self.consume_word("POINTER") {
            Some(self.parse_data_ref()?)
        } else {
            None
        };
        let handlers = self.parse_event_handlers("OVERFLOW", "END-STRING")?;
        let _ = self.consume_word("END-STRING");
        Ok(Stmt::StringVerb {
            groups,
            into,
            pointer,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `SUBTRACT [CORR] a ... FROM b ... [GIVING dst ...] [ON SIZE ERROR ...] [END-SUBTRACT]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `SUBTRACT` verb.
    fn parse_subtract_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mode = if self.consume_any_word(&["CORRESPONDING", "CORR"]) {
            CorrespondingMode::Corresponding
        } else {
            CorrespondingMode::Standard
        };
        let mut subtrahends = Vec::new();
        while !self.at_end_or_period() && !self.at_word("FROM") {
            subtrahends.push(self.parse_expr()?);
        }
        let _ = self.consume_word("FROM");
        let mut from_giving_minuend = None;
        let mut from_targets = Vec::new();
        if self.lookahead_has_giving_before_end() {
            from_giving_minuend = Some(self.parse_expr()?);
        } else {
            from_targets = self.parse_rounded_targets(&["GIVING", "SIZE", "END-SUBTRACT"])?;
        }
        let giving_targets = if self.consume_word("GIVING") {
            self.parse_rounded_targets(&["SIZE", "END-SUBTRACT"])?
        } else {
            Vec::new()
        };
        let size_error = self.parse_size_error_handlers("END-SUBTRACT")?;
        let _ = self.consume_word("END-SUBTRACT");
        Ok(Stmt::Subtract {
            mode,
            subtrahends,
            from_giving_minuend,
            from_targets,
            giving_targets,
            size_error,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `TERMINATE report ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `TERMINATE` verb.
    fn parse_terminate_stmt(&mut self, start: SourceSpan) -> Stmt {
        self.advance();
        let mut reports = Vec::new();
        while !self.at_clause_boundary(&[])
            && let Some((r, _, _)) = self.eat_word()
        {
            reports.push(r);
        }
        Stmt::Terminate {
            reports,
            span: start.merge(self.current_span()),
        }
    }

    /// Parses `TRANSFORM target [CHARACTERS] FROM from-alpha TO to-alpha`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `TRANSFORM` verb.
    fn parse_transform_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let target = self.parse_data_ref()?;
        let _ = self.consume_word("CHARACTERS");
        let _ = self.consume_word("FROM");
        let from = self.parse_expr()?;
        let _ = self.consume_word("TO");
        let to = self.parse_expr()?;
        Ok(Stmt::Transform {
            target,
            from,
            to,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `UNLOCK file [RECORD | RECORDS]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `UNLOCK` verb.
    fn parse_unlock_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let file = if let Some((s, _, _)) = self.eat_word() {
            s
        } else {
            self.interner.intern("__MISSING_UNLOCK")?
        };
        let _ = self.consume_any_word(&["RECORD", "RECORDS"]);
        Ok(Stmt::Unlock {
            file,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `UNSTRING src [DELIMITED BY ...] INTO ... [WITH POINTER p] [TALLYING IN t] [ON OVERFLOW ...] [END-UNSTRING]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `UNSTRING` verb.
    fn parse_unstring_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let source = self.parse_data_ref()?;
        let mut delimiters = Vec::new();
        if self.consume_word("DELIMITED") {
            let _ = self.consume_word("BY");
            loop {
                let all_occurrences = if self.consume_word("ALL") {
                    DuplicateMatchMode::AllRepeated
                } else {
                    DuplicateMatchMode::Single
                };
                let pattern = self.parse_expr()?;
                delimiters.push(UnstringDelimiter {
                    all_occurrences,
                    pattern,
                });
                if !self.consume_word("OR") {
                    break;
                }
            }
        }
        let _ = self.consume_word("INTO");
        let mut targets = Vec::new();
        while !self.at_clause_boundary(&[
            "WITH",
            "POINTER",
            "TALLYING",
            "ON",
            "OVERFLOW",
            "NOT",
            "END-UNSTRING",
        ]) {
            let into = self.parse_data_ref()?;
            let delimiter_in = if self.consume_word("DELIMITER") {
                let _ = self.consume_word("IN");
                Some(self.parse_data_ref()?)
            } else {
                None
            };
            let count_in = if self.consume_word("COUNT") {
                let _ = self.consume_word("IN");
                Some(self.parse_data_ref()?)
            } else {
                None
            };
            targets.push(UnstringTarget {
                into,
                delimiter_in,
                count_in,
            });
        }
        let mut pointer = None;
        let mut tallying = None;
        loop {
            let _ = self.consume_word("WITH");
            if self.consume_word("POINTER") {
                pointer = Some(self.parse_data_ref()?);
            } else if self.consume_word("TALLYING") {
                let _ = self.consume_word("IN");
                tallying = Some(self.parse_data_ref()?);
            } else {
                break;
            }
        }
        let handlers = self.parse_event_handlers("OVERFLOW", "END-UNSTRING")?;
        let _ = self.consume_word("END-UNSTRING");
        Ok(Stmt::Unstring {
            source,
            delimiters,
            targets,
            pointer,
            tallying,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `VALIDATE target ...`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `VALIDATE` verb.
    fn parse_validate_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let mut targets = Vec::new();
        while !self.at_clause_boundary(&[]) {
            targets.push(self.parse_data_ref()?);
        }
        Ok(Stmt::Validate {
            targets,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `WRITE record [FROM src] [{BEFORE|AFTER} ADVANCING ...] [AT EOP / INVALID KEY ...] [END-WRITE]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `WRITE` verb.
    fn parse_write_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let target_mode = if self.consume_word("FILE") {
            WriteTargetMode::FileName
        } else {
            WriteTargetMode::RecordName
        };
        let record = self.parse_data_ref()?;
        let from = if self.consume_word("FROM") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let mut advancing = None;
        while !self.at_clause_boundary(&["AT", "END-OF-PAGE", "EOP", "INVALID", "NOT", "END-WRITE"])
        {
            if matches!(self.current_word(), Some("BEFORE" | "AFTER")) {
                let side = if self.consume_word("BEFORE") {
                    BoundSide::Before
                } else {
                    let _ = self.consume_word("AFTER");
                    BoundSide::After
                };
                let _ = self.consume_word("ADVANCING");
                let amt = self.parse_expr()?;
                let _ = self.consume_any_word(&["LINE", "LINES"]);
                advancing = Some((side, amt));
            } else if self.consume_any_word(&["WITH", "NO", "LOCK"]) {
                // Consume WRITE locking options.
            } else {
                break;
            }
        }
        let (handler_kind, handlers) = self.parse_write_handlers()?;
        let _ = self.consume_word("END-WRITE");
        Ok(Stmt::Write {
            record,
            target_mode,
            from,
            advancing,
            handler_kind,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `XML GENERATE ...` or `XML PARSE ... [END-XML]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `XML` verb.
    fn parse_xml_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        if self.consume_word("GENERATE") {
            self.parse_xml_generate_stmt(start)
        } else {
            self.parse_xml_parse_stmt(start)
        }
    }

    /// Parses `XML GENERATE` clauses following the `GENERATE` keyword.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `XML` verb.
    fn parse_xml_generate_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        let destination = self.parse_data_ref()?;
        let _ = self.consume_word("FROM");
        let source = self.parse_markup_source_ref()?;
        let mut count_in = None;
        let mut encoding = None;
        let mut xml_declaration = XmlDeclPresence::Omit;
        let mut attributes = XmlAttrPresence::ElementsOnly;
        let mut namespace = None;
        let mut namespace_prefix = None;
        let mut name_mappings = Vec::new();
        let mut type_mappings = Vec::new();
        let mut suppress_clauses = Vec::new();
        let stop = &[
            "COUNT",
            "WITH",
            "ENCODING",
            "XML-DECLARATION",
            "ATTRIBUTES",
            "NAMESPACE",
            "NAMESPACE-PREFIX",
            "NAME",
            "TYPE",
            "SUPPRESS",
            "ON",
            "EXCEPTION",
            "NOT",
            "END-XML",
        ];
        while !self.at_markup_boundary(&["ON", "EXCEPTION", "NOT", "END-XML"]) {
            if self.consume_word("WITH") {
                // Optional noise word preceding ENCODING / XML-DECLARATION / ATTRIBUTES.
            } else if self.consume_word("COUNT") {
                let _ = self.consume_word("IN");
                count_in = Some(self.parse_data_ref()?);
            } else if self.consume_word("ENCODING") {
                let _ = self.consume_word("IS");
                encoding = Some(Box::new(self.parse_expr()?));
            } else if self.consume_word("XML-DECLARATION") {
                xml_declaration = XmlDeclPresence::Include;
            } else if self.consume_word("ATTRIBUTES") {
                attributes = XmlAttrPresence::WithAttributes;
            } else if self.consume_word("NAMESPACE") {
                let _ = self.consume_word("IS");
                namespace = Some(Box::new(self.parse_expr()?));
            } else if self.consume_word("NAMESPACE-PREFIX") {
                let _ = self.consume_word("IS");
                namespace_prefix = Some(Box::new(self.parse_expr()?));
            } else if self.consume_word("NAME") {
                let _ = self.consume_word("OF");
                self.parse_markup_name_clauses(stop, &mut name_mappings)?;
            } else if self.consume_word("TYPE") {
                let _ = self.consume_word("OF");
                self.parse_xml_type_clauses(stop, &mut type_mappings)?;
            } else if self.consume_word("SUPPRESS") {
                self.parse_markup_suppress_clauses(stop, &mut suppress_clauses)?;
            } else {
                self.advance();
            }
        }
        let handlers = self.parse_event_handlers("EXCEPTION", "END-XML")?;
        let _ = self.consume_word("END-XML");
        Ok(Stmt::XmlGenerate {
            destination,
            source,
            count_in,
            encoding,
            xml_declaration,
            attributes,
            namespace,
            namespace_prefix,
            name_mappings,
            type_mappings,
            suppress_clauses,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses `XML PARSE` clauses following the `XML` keyword.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of the `XML` verb.
    fn parse_xml_parse_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        let _ = self.consume_word("PARSE");
        let source = self.parse_expr()?;
        let mut encoding = None;
        let mut returning_national = XmlNationalReturn::Alphanumeric;
        let mut validating = None;
        while !self.at_clause_boundary(&["ON", "EXCEPTION", "NOT", "END-XML"]) {
            if self.consume_word("PROCESSING") {
                let _ = self.consume_word("PROCEDURE");
                break;
            } else if self.consume_word("PROCEDURE") {
                break;
            } else if self.consume_word("WITH") {
                // Optional noise word preceding ENCODING / VALIDATING.
            } else if self.consume_word("ENCODING") {
                let _ = self.consume_word("IS");
                encoding = Some(self.parse_expr()?);
            } else if self.consume_word("RETURNING") {
                if self.consume_word("NATIONAL") {
                    returning_national = XmlNationalReturn::National;
                }
            } else if self.consume_word("VALIDATING") {
                let _ = self.consume_word("WITH");
                let _ = self.consume_word("FILE");
                validating = Some(self.parse_expr()?);
            } else {
                self.advance();
            }
        }
        let _ = self.consume_word("IS");
        let processing_proc = self.parse_proc_range()?;
        let handlers = self.parse_event_handlers("EXCEPTION", "END-XML")?;
        let _ = self.consume_word("END-XML");
        Ok(Stmt::XmlParse {
            source,
            encoding,
            returning_national,
            validating,
            processing_proc,
            handlers,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses the `FROM` record data reference of `XML GENERATE` / `JSON GENERATE`, rejecting `FUNCTION`.
    fn parse_markup_source_ref(&mut self) -> Result<DataRef> {
        if self.at_word("FUNCTION") {
            let err_sp = self.current_span();
            self.emit_syntax_error(err_sp, "unexpected intrinsic function name");
            let _ = self.parse_expr()?;
            let sym = self.interner.intern("__INVALID_MARKUP_SRC")?;
            return Ok(DataRef {
                modifier: DataRefModifier::Plain,
                name: QualifiedName {
                    base: sym,
                    qualifiers: Vec::new(),
                    span: err_sp,
                },
                subscripts: Vec::new(),
                refmod: None,
                span: err_sp,
            });
        }
        self.parse_data_ref()
    }

    /// Returns `true` when at sentence end, `stop_words`, or a statement/block boundary other than `SUPPRESS`/`WHEN`/`TYPE`.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Clause-stopping keywords recognized at the current markup nesting level.
    fn at_markup_boundary(&mut self, stop_words: &[&str]) -> bool {
        self.skip_separators();
        if self.at_end_or_period() {
            return true;
        }
        if let Some(w) = self.current_word() {
            if stop_words.contains(&w) {
                return true;
            }
            if matches!(w, "SUPPRESS" | "WHEN" | "TYPE") {
                return false;
            }
            return is_verb_starter(Some(w))
                || is_block_terminator_word(w)
                || self.at_paragraph_header()
                || self.at_section_header();
        }
        false
    }

    /// Parses one or more `item [IS] {literal | OMITTED}` entries inside `NAME [OF]`.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Clause stop keywords.
    /// * `out` - Output [`MarkupNameClause`] list.
    fn parse_markup_name_clauses(
        &mut self,
        stop_words: &[&str],
        out: &mut Vec<MarkupNameClause>,
    ) -> Result<()> {
        while !self.at_markup_boundary(stop_words) {
            let target = self.parse_data_ref()?;
            let _ = self.consume_word("IS");
            let (override_name, end_sp) = if self.consume_word("OMITTED") {
                (MarkupNameTarget::Omitted, self.current_span())
            } else if let Some((sym, sp)) = self.eat_any_word_or_string_sym() {
                (MarkupNameTarget::Literal(sym), sp)
            } else {
                break;
            };
            let span = target.span.merge(end_sp);
            out.push(MarkupNameClause {
                target,
                override_name,
                span,
            });
        }
        Ok(())
    }

    /// Parses one or more `item [IS] {ATTRIBUTE | ELEMENT | CONTENT}` entries inside `TYPE [OF]`.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Clause stop keywords.
    /// * `out` - Output [`XmlTypeClause`] list.
    fn parse_xml_type_clauses(
        &mut self,
        stop_words: &[&str],
        out: &mut Vec<XmlTypeClause>,
    ) -> Result<()> {
        while !self.at_markup_boundary(stop_words) {
            let target = self.parse_data_ref()?;
            let _ = self.consume_word("IS");
            let end_sp = self.current_span();
            let node_type = if self.consume_word("ATTRIBUTE") {
                XmlNodeTypeClause::Attribute
            } else if self.consume_word("CONTENT") {
                XmlNodeTypeClause::Content
            } else {
                let _ = self.consume_word("ELEMENT");
                XmlNodeTypeClause::Element
            };
            let span = target.span.merge(end_sp);
            out.push(XmlTypeClause {
                target,
                node_type,
                span,
            });
        }
        Ok(())
    }

    /// Parses one or more `SUPPRESS` subclauses (`EVERY ...`, `WHEN ...`, or `item [WHEN ...]`).
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Clause stop keywords.
    /// * `out` - Output [`MarkupSuppressClause`] list.
    fn parse_markup_suppress_clauses(
        &mut self,
        stop_words: &[&str],
        out: &mut Vec<MarkupSuppressClause>,
    ) -> Result<()> {
        while !self.at_markup_boundary(stop_words) {
            let sub_start = self.current_span();
            if self.consume_word("WHEN") {
                let conditions = self.parse_markup_suppress_conds();
                out.push(MarkupSuppressClause {
                    subject: MarkupSuppressSubject::Generic {
                        category: MarkupSuppressGenericKind::Any,
                        node_kind: MarkupSuppressNodeKind::Any,
                    },
                    conditions,
                    span: sub_start.merge(self.current_span()),
                });
            } else if self.consume_word("EVERY") {
                let category = if self.consume_word("NUMERIC") {
                    MarkupSuppressGenericKind::Numeric
                } else if self.consume_word("NONNUMERIC") {
                    MarkupSuppressGenericKind::Nonnumeric
                } else {
                    MarkupSuppressGenericKind::Any
                };
                let node_kind = if self.consume_word("ELEMENT") {
                    MarkupSuppressNodeKind::Element
                } else if self.consume_word("ATTRIBUTE") {
                    MarkupSuppressNodeKind::Attribute
                } else if self.consume_word("CONTENT") {
                    MarkupSuppressNodeKind::Content
                } else {
                    MarkupSuppressNodeKind::Any
                };
                let conditions = if self.consume_word("WHEN") {
                    self.parse_markup_suppress_conds()
                } else {
                    Vec::new()
                };
                out.push(MarkupSuppressClause {
                    subject: MarkupSuppressSubject::Generic {
                        category,
                        node_kind,
                    },
                    conditions,
                    span: sub_start.merge(self.current_span()),
                });
            } else {
                let target = self.parse_data_ref()?;
                let conditions = if self.consume_word("WHEN") {
                    self.parse_markup_suppress_conds()
                } else {
                    Vec::new()
                };
                let span = target.span.merge(self.current_span());
                out.push(MarkupSuppressClause {
                    subject: MarkupSuppressSubject::Field(target),
                    conditions,
                    span,
                });
            }
        }
        Ok(())
    }

    /// Parses `[OR]`-separated figurative predicates after `SUPPRESS ... WHEN`.
    fn parse_markup_suppress_conds(&mut self) -> Vec<MarkupSuppressCond> {
        let mut conds = Vec::new();
        loop {
            let cond = match self.current_word() {
                Some("ZERO" | "ZEROS" | "ZEROES") => Some(MarkupSuppressCond::Zero),
                Some("SPACE" | "SPACES") => Some(MarkupSuppressCond::Space),
                Some("LOW-VALUE" | "LOW-VALUES") => Some(MarkupSuppressCond::LowValue),
                Some("HIGH-VALUE" | "HIGH-VALUES") => Some(MarkupSuppressCond::HighValue),
                _ => None,
            };
            if let Some(c) = cond {
                self.advance();
                conds.push(c);
                if !self.consume_word("OR") {
                    break;
                }
            } else {
                break;
            }
        }
        conds
    }

    /// Parses `CONVERTING item TO/FROM [JSON] BOOLEAN [USING cond]` entries on `JSON`.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Clause stop keywords.
    /// * `out` - Output [`JsonBoolClause`] list.
    fn parse_json_converting_clauses(
        &mut self,
        stop_words: &[&str],
        out: &mut Vec<JsonBoolClause>,
    ) -> Result<()> {
        while !self.at_markup_boundary(stop_words) {
            let target = self.parse_data_ref()?;
            let _ = self.consume_any_word(&["TO", "FROM"]);
            let _ = self.consume_word("JSON");
            let _ = self.consume_word("BOOLEAN");
            let (cond_sym, false_sym) = if self.consume_any_word(&["USING", "WHEN"]) {
                let c = self.eat_any_word_or_string_sym().map(|p| p.0);
                let f = if self.consume_word("AND") {
                    self.eat_any_word_or_string_sym().map(|p| p.0)
                } else {
                    None
                };
                (c, f)
            } else {
                (None, None)
            };
            let span = target.span.merge(self.current_span());
            out.push(JsonBoolClause {
                target,
                cond_sym,
                false_sym,
                span,
            });
            let _ = self.consume_word("ALSO");
        }
        Ok(())
    }

    /// Parses one or more `target [ROUNDED [MODE IS m]]` destinations until `stop_words` or next verb.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Clause stop keywords.
    fn parse_rounded_targets(&mut self, stop_words: &[&str]) -> Result<Vec<RoundedTarget>> {
        let mut out = Vec::new();
        while !self.at_clause_boundary(stop_words)
            && !matches!(self.current_word(), Some("ON" | "NOT"))
        {
            out.push(self.parse_single_rounded_target()?);
        }
        Ok(out)
    }

    /// Parses a single `target [ROUNDED [MODE IS mode-sym]]`.
    fn parse_single_rounded_target(&mut self) -> Result<RoundedTarget> {
        let target = self.parse_data_ref()?;
        let rounding = if self.consume_word("ROUNDED") {
            let mode = if self.consume_word("MODE") {
                let _ = self.consume_word("IS");
                self.eat_word().map(|t| t.0)
            } else {
                None
            };
            RoundingSpec::Rounded(mode)
        } else {
            RoundingSpec::Unrounded
        };
        Ok(RoundedTarget { target, rounding })
    }

    /// Parses `[ON] SIZE ERROR ... [NOT [ON] SIZE ERROR ...]` handlers.
    ///
    /// # Arguments
    ///
    /// * `end_kw` - Explicit scope terminator (`"END-ADD"`, `"END-COMPUTE"`, etc.).
    fn parse_size_error_handlers(&mut self, end_kw: &str) -> Result<SizeErrorHandlers> {
        let ev = self.parse_generic_event_handlers(&["SIZE"], end_kw)?;
        Ok(SizeErrorHandlers {
            on_size_error: ev.on_event,
            not_on_size_error: ev.not_on_event,
        })
    }

    /// Parses `[ON] <event_kw> ... [NOT [ON] <event_kw> ...]` (`EXCEPTION` or `OVERFLOW`).
    ///
    /// # Arguments
    ///
    /// * `event_kw` - `"EXCEPTION"` or `"OVERFLOW"`.
    /// * `end_kw` - Scope terminator keyword.
    fn parse_event_handlers(&mut self, event_kw: &str, end_kw: &str) -> Result<EventHandlers> {
        self.parse_generic_event_handlers(&[event_kw], end_kw)
    }

    /// Parses `CALL` `ON EXCEPTION` / `ON OVERFLOW` / `NOT ON EXCEPTION` handlers.
    fn parse_call_handlers(&mut self) -> Result<EventHandlers> {
        self.parse_generic_event_handlers(&["EXCEPTION", "OVERFLOW"], "END-CALL")
    }

    /// Parses `READ` / `RETURN` `[AT] END` and `INVALID KEY` handlers.
    ///
    /// # Arguments
    ///
    /// * `end_kw` - `"END-READ"` or `"END-RETURN"`.
    fn parse_read_or_return_handlers(&mut self, end_kw: &str) -> Result<EventHandlers> {
        self.parse_generic_event_handlers(&["END", "INVALID"], end_kw)
    }

    /// Parses `INVALID KEY` / `NOT INVALID KEY` handlers.
    ///
    /// # Arguments
    ///
    /// * `end_kw` - Scope terminator keyword.
    fn parse_invalid_key_or_exception_handlers(&mut self, end_kw: &str) -> Result<EventHandlers> {
        self.parse_generic_event_handlers(&["INVALID", "EXCEPTION"], end_kw)
    }

    /// Parses `WRITE` `[AT] END-OF-PAGE` / `EOP` / `INVALID KEY` handlers alongside their kind.
    fn parse_write_handlers(&mut self) -> Result<(WriteHandlerKind, EventHandlers)> {
        let kind = if self.at_event_offset(0, &["END-OF-PAGE", "EOP"])
            || (self.at_word("NOT") && self.at_event_offset(1, &["END-OF-PAGE", "EOP"]))
        {
            WriteHandlerKind::EndOfPage
        } else if self.at_event_offset(0, &["INVALID"])
            || (self.at_word("NOT") && self.at_event_offset(1, &["INVALID"]))
        {
            WriteHandlerKind::InvalidKey
        } else {
            WriteHandlerKind::None
        };
        let handlers =
            self.parse_generic_event_handlers(&["END-OF-PAGE", "EOP", "INVALID"], "END-WRITE")?;
        Ok((kind, handlers))
    }

    /// Parses `[ON|AT] <event_kws> [ERROR|KEY] ...` and `NOT [ON|AT] <event_kws> [ERROR|KEY] ...`.
    ///
    /// # Arguments
    ///
    /// * `event_kws` - Event trigger keywords (e.g. `&["SIZE"]`, `&["END", "INVALID"]`).
    /// * `end_kw` - Enclosing verb scope terminator.
    pub(crate) fn parse_generic_event_handlers(
        &mut self,
        event_kws: &[&str],
        end_kw: &str,
    ) -> Result<EventHandlers> {
        let mut h = EventHandlers::default();
        let mut stop_for_not: Vec<&str> = event_kws.to_vec();
        stop_for_not.extend_from_slice(&["AT", end_kw]);
        loop {
            if self.at_word("NOT") && self.at_event_offset(1, event_kws) {
                self.advance();
                let _ = self.consume_any_word(&["ON", "AT"]);
                let _ = self.consume_any_word(event_kws);
                let _ = self.consume_any_word(&["ERROR", "KEY"]);
                h.not_on_event = self.parse_stmt_block(&stop_for_not)?;
            } else if self.at_event_offset(0, event_kws) {
                let _ = self.consume_any_word(&["ON", "AT"]);
                let _ = self.consume_any_word(event_kws);
                let _ = self.consume_any_word(&["ERROR", "KEY"]);
                h.on_event = self.parse_stmt_block(&[end_kw, "NOT"])?;
            } else {
                break;
            }
        }
        Ok(h)
    }

    /// Checks whether token `offset` starts `[ON|AT] <event_kws>`.
    ///
    /// # Arguments
    ///
    /// * `offset` - Token lookahead offset (`0` for positive, `1` after `NOT`).
    /// * `event_kws` - Permitted event keywords.
    #[must_use]
    pub(crate) fn at_event_offset(&self, offset: usize, event_kws: &[&str]) -> bool {
        let Some(first) = self.peek_word_str_at(offset) else {
            return false;
        };
        event_kws.contains(&first)
            || (matches!(first, "ON" | "AT")
                && self
                    .peek_word_str_at(offset + 1)
                    .is_some_and(|second| event_kws.contains(&second)))
    }

    /// Returns `true` when at `.`, EOF, a contextual `stop_words` entry, a verb starter, or `END-*`.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Contextual stop keywords.
    #[must_use]
    pub(crate) fn at_clause_boundary(&self, stop_words: &[&str]) -> bool {
        self.at_end_or_period()
            || self.starts_compound_handler()
            || self.current_word().is_some_and(|w| {
                stop_words.contains(&w) || is_verb_starter(Some(w)) || is_block_terminator_word(w)
            })
    }

    /// Returns `true` when at `.`, EOF, `stop_words`, a non-call-arg verb starter, or `END-*`.
    ///
    /// # Arguments
    ///
    /// * `stop_words` - Contextual stop keywords.
    #[must_use]
    fn at_call_arg_boundary(&self, stop_words: &[&str]) -> bool {
        self.at_end_or_period()
            || self.starts_compound_handler()
            || self.current_word().is_some_and(|w| {
                stop_words.contains(&w)
                    || is_verb_starter_excluding_call_arg(Some(w))
                    || is_block_terminator_word(w)
            })
    }

    /// Returns `true` when at EOF, `END DECLARATIVES`, a section header, or post-`PROCEDURE` boundary.
    #[must_use]
    fn at_section_or_decl_end(&self) -> bool {
        self.at_eof()
            || self.starts_after_proc_division()
            || self.at_end_declaratives()
            || self.at_section_header()
    }

    /// Returns `true` when at EOF, `END DECLARATIVES`, a section, paragraph, or post-`PROCEDURE` boundary.
    #[must_use]
    fn at_sentence_boundary(&self) -> bool {
        self.at_section_or_decl_end() || self.at_paragraph_header()
    }

    /// Scans ahead within the current arithmetic statement to check if `GIVING` precedes the next verb/period.
    #[must_use]
    fn lookahead_has_giving_before_end(&self) -> bool {
        let mut idx = self.pos;
        let mut depth = 0_u32;
        while let Some(tok) = self.tokens.get(idx) {
            match tok.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => depth = depth.saturating_sub(1),
                TokenKind::Period => return false,
                TokenKind::Word { sym, .. } if depth == 0 => {
                    let w = self.sym_str(sym);
                    if w == "GIVING" {
                        return true;
                    }
                    if is_verb_starter(Some(w))
                        || is_block_terminator_word(w)
                        || matches!(w, "SIZE" | "REMAINDER")
                    {
                        return false;
                    }
                }
                _ => {}
            }
            idx += 1;
        }
        false
    }

    /// Returns `true` when positioned at `<name> SECTION [seg-no].`.
    #[must_use]
    pub(crate) fn at_section_header(&self) -> bool {
        let first = self.peek_non_sep_at(0).unwrap_or(&super::SENTINEL_EOF);
        if !matches!(
            first.kind,
            TokenKind::Word { .. } | TokenKind::IntegerLit(_)
        ) {
            return false;
        }
        if let TokenKind::Word { sym, .. } = first.kind
            && self.sym_str(sym) == "EXIT"
        {
            return false;
        }
        if !self.peek_word_at(1, "SECTION") {
            return false;
        }
        let third = self.peek_non_sep_at(2).unwrap_or(&super::SENTINEL_EOF);
        third.kind == TokenKind::Period
            || (matches!(third.kind, TokenKind::IntegerLit(_))
                && self
                    .peek_non_sep_at(3)
                    .is_some_and(|t| t.kind == TokenKind::Period))
    }

    /// Returns `true` when positioned at `<para-name>.` (Area A in fixed format, non-verb in free format).
    #[must_use]
    pub(crate) fn at_paragraph_header(&self) -> bool {
        let first = self.peek_non_sep_at(0).unwrap_or(&super::SENTINEL_EOF);
        let second = self.peek_non_sep_at(1).unwrap_or(&super::SENTINEL_EOF);
        if second.kind != TokenKind::Period {
            return false;
        }
        match first.kind {
            TokenKind::IntegerLit(_) => true,
            TokenKind::Word { sym, area, .. } => {
                let w = self.sym_str(sym);
                if is_verb_starter(Some(w)) || is_block_terminator_word(w) || w == "END" {
                    return false;
                }
                matches!(area, MarginZone::AreaA | MarginZone::FreeFlow)
            }
            _ => false,
        }
    }

    /// Returns `true` when positioned at `END PROGRAM/FUNCTION/CLASS/INTERFACE/METHOD` or a nested program.
    #[must_use]
    pub(crate) fn starts_after_proc_division(&self) -> bool {
        self.starts_nested_program() || self.at_end_unit_header()
    }

    /// Parses COBOL-85 Communication Module `ENABLE` / `DISABLE` statements.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of `ENABLE` or `DISABLE`.
    fn parse_comm_enable_disable_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let _ = self.consume_any_word(&["INPUT", "I-O", "OUTPUT"]);
        let _ = self.consume_word("TERMINAL");
        let _ = self.parse_data_ref()?;
        let _ = self.consume_word("WITH");
        if self.consume_word("KEY") {
            let _ = self.parse_expr()?;
        }
        Ok(Stmt::Continue {
            after_seconds: None,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses COBOL-85 Communication Module `PURGE cd-name` statements.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of `PURGE`.
    fn parse_comm_purge_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let _ = self.parse_data_ref()?;
        Ok(Stmt::Continue {
            after_seconds: None,
            span: start.merge(self.current_span()),
        })
    }

    /// Parses COBOL-85 Communication Module `RECEIVE cd-name {MESSAGE | SEGMENT} INTO id [NO DATA ...] [WITH DATA ...] [END-RECEIVE]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of `RECEIVE`.
    fn parse_comm_receive_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let _ = self.parse_data_ref()?;
        let _ = self.consume_any_word(&["MESSAGE", "SEGMENT"]);
        let _ = self.consume_word("INTO");
        let _ = self.parse_data_ref()?;
        let mut branch_stmts = Vec::new();
        while (self.at_word("NO") && self.peek_word_str_at(1) == Some("DATA"))
            || (self.at_word("WITH") && self.peek_word_str_at(1) == Some("DATA"))
        {
            self.advance();
            self.advance();
            while !self.at_comm_receive_branch_end() {
                let Some(inner) = self.parse_one_stmt()? else {
                    break;
                };
                branch_stmts.push(inner);
            }
        }
        let _ = self.consume_word("END-RECEIVE");
        let span = start.merge(self.current_span());
        if let Some(first) = branch_stmts.into_iter().next() {
            Ok(first)
        } else {
            Ok(Stmt::Continue {
                after_seconds: None,
                span,
            })
        }
    }

    /// Returns `true` when positioned at a `RECEIVE ... NO DATA / WITH DATA` branch delimiter.
    #[must_use]
    fn at_comm_receive_branch_end(&self) -> bool {
        self.at_end_or_period()
            || self.at_word("END-RECEIVE")
            || (self.at_word("NO") && self.peek_word_str_at(1) == Some("DATA"))
            || (self.at_word("WITH") && self.peek_word_str_at(1) == Some("DATA"))
            || matches!(self.current_word(), Some("ELSE" | "WHEN"))
    }

    /// Parses COBOL-85 Communication Module `SEND cd-name [FROM expr] [WITH ...] [{BEFORE | AFTER} ...] [END-SEND]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening span of `SEND`.
    fn parse_comm_send_stmt(&mut self, start: SourceSpan) -> Result<Stmt> {
        self.advance();
        let _ = self.parse_data_ref()?;
        while !self.at_end_or_period() {
            if self.consume_word("FROM") {
                let _ = self.parse_expr()?;
            } else if self.consume_word("WITH") {
                if !self.consume_any_word(&["ESI", "EMI", "EGI"]) {
                    let _ = self.parse_expr()?;
                }
            } else if self.consume_any_word(&["BEFORE", "AFTER"]) {
                let _ = self.consume_word("ADVANCING");
                if !self.consume_word("PAGE") {
                    let _ = self.parse_expr()?;
                    let _ = self.consume_any_word(&["LINE", "LINES"]);
                }
            } else if self.consume_word("REPLACING") {
                let _ = self.consume_word("LINE");
            } else {
                break;
            }
        }
        let _ = self.consume_word("END-SEND");
        Ok(Stmt::Continue {
            after_seconds: None,
            span: start.merge(self.current_span()),
        })
    }
}

/// Checks if `w` is a COBOL statement verb starter.
///
/// # Arguments
///
/// * `w` - Optional uppercase word.
#[must_use]
pub(crate) fn is_verb_starter(w: Option<&str>) -> bool {
    matches!(
        w,
        Some(
            "ACCEPT"
                | "ADD"
                | "ALLOCATE"
                | "ALTER"
                | "CALL"
                | "CANCEL"
                | "CLOSE"
                | "COMMIT"
                | "COMPUTE"
                | "CONTINUE"
                | "DELETE"
                | "DISABLE"
                | "DISPLAY"
                | "DIVIDE"
                | "ENABLE"
                | "ENTRY"
                | "EVALUATE"
                | "EXHIBIT"
                | "EXIT"
                | "FREE"
                | "GENERATE"
                | "GO"
                | "GOBACK"
                | "IF"
                | "INITIALIZE"
                | "INITIATE"
                | "INSPECT"
                | "INVOKE"
                | "JSON"
                | "MERGE"
                | "MOVE"
                | "MULTIPLY"
                | "NEXT"
                | "OPEN"
                | "PERFORM"
                | "PURGE"
                | "RAISE"
                | "READ"
                | "RECEIVE"
                | "RELEASE"
                | "RESUME"
                | "RETURN"
                | "REWRITE"
                | "ROLLBACK"
                | "SEARCH"
                | "SEND"
                | "SET"
                | "SORT"
                | "START"
                | "STOP"
                | "STRING"
                | "SUBTRACT"
                | "SUPPRESS"
                | "TERMINATE"
                | "TRANSFORM"
                | "UNLOCK"
                | "UNSTRING"
                | "VALIDATE"
                | "WRITE"
                | "XML"
        )
    )
}

/// Checks if `w` is a verb starter excluding `OMITTED` / `FUNCTION` inside `CALL ... USING`.
///
/// # Arguments
///
/// * `w` - Optional uppercase word.
#[must_use]
fn is_verb_starter_excluding_call_arg(w: Option<&str>) -> bool {
    is_verb_starter(w)
}

/// Checks if `w` is an explicit `END-*` scope terminator or mid-construct branch delimiter (`ELSE`, `WHEN`).
///
/// # Arguments
///
/// * `w` - Uppercase word.
#[must_use]
pub(crate) fn is_block_terminator_word(w: &str) -> bool {
    matches!(
        w,
        "ELSE"
            | "WHEN"
            | "END-ACCEPT"
            | "END-ADD"
            | "END-CALL"
            | "END-COMPUTE"
            | "END-DELETE"
            | "END-DISPLAY"
            | "END-DIVIDE"
            | "END-EVALUATE"
            | "END-IF"
            | "END-INVOKE"
            | "END-JSON"
            | "END-MULTIPLY"
            | "END-PERFORM"
            | "END-READ"
            | "END-RECEIVE"
            | "END-RETURN"
            | "END-REWRITE"
            | "END-SEARCH"
            | "END-SEND"
            | "END-START"
            | "END-STRING"
            | "END-SUBTRACT"
            | "END-UNSTRING"
            | "END-WRITE"
            | "END-XML"
    )
}
