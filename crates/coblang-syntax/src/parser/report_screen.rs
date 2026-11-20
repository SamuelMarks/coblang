//! Recursive-descent parsers for Milestone M18 (`REPORT SECTION`) and Milestone M19 (`SCREEN SECTION`, extended `DISPLAY`/`ACCEPT`, and `SET ... ATTRIBUTE`).

#![deny(clippy::indexing_slicing)]

use coblang_common::{Dialect, LevelNumber, Result, SourceSpan, SymbolId};

use crate::ast::data_div::{
    BlankWhenZeroMode, DataEntryName, GlobalVisibility, JustificationMode, SignClause,
    SignPlacement, SignSeparation, UsageKind, ValueAtom,
};
use crate::ast::proc_div::{AcceptSource, DataRef, DisplayAdvancing, Expr, SetAction, Stmt};
use crate::ast::report_screen::{
    RdCodeClauseAst, RdColLimitAst, RdEntry, ReportAfterNewAtomAst, ReportColAlignAst,
    ReportColCoordAst, ReportControlAst, ReportGroupEntry, ReportGroupType, ReportLineClauseAst,
    ReportNextGroupAst, ReportOccursExtAst, ReportPresentGateAst, ReportPresentPolarity,
    ReportSumClauseAst, ScreenAcceptStmtAst, ScreenAcceptTargetAst, ScreenClauseAst,
    ScreenCoordAst, ScreenCoordDirAst, ScreenDisplayItemAst, ScreenDisplayStmtAst, ScreenEntry,
    SetScreenAttrToggleAst, SpecialNamesDataRole,
};
use crate::parser::Parser;
use crate::parser::data::{is_bare_usage_keyword, match_figurative_word};
use crate::token::{TokenKind, WordRole};

/// Accumulator for `RD` header clauses during [`Parser::parse_rd_entry_m18`].
struct RdHeaderAccum {
    /// `GLOBAL` vs `LOCAL` visibility.
    global: GlobalVisibility,
    /// Optional `CODE IS ...` prefix clause.
    code_clause: Option<RdCodeClauseAst>,
    /// Declared control data-item references.
    control_items: Vec<ReportControlAst>,
    /// Legacy control symbol paths.
    controls: Vec<Vec<SymbolId>>,
    /// Optional `PAGE LIMIT` row count.
    page_limit: Option<u32>,
    /// Span of `PAGE LIMIT`.
    page_limit_span: Option<SourceSpan>,
    /// Optional `COL LIMIT` column width.
    col_limit: Option<RdColLimitAst>,
    /// Optional `HEADING` line number.
    heading: Option<u32>,
    /// Span of `HEADING`.
    heading_span: Option<SourceSpan>,
    /// Optional `FIRST DETAIL` line number.
    first_detail: Option<u32>,
    /// Span of `FIRST DETAIL`.
    first_detail_span: Option<SourceSpan>,
    /// Optional `LAST DETAIL` line number.
    last_detail: Option<u32>,
    /// Span of `LAST DETAIL`.
    last_detail_span: Option<SourceSpan>,
    /// Optional `FOOTING` line number.
    footing: Option<u32>,
    /// Span of `FOOTING`.
    footing_span: Option<SourceSpan>,
    /// Bitmask of duplicate zone/limit clauses encountered.
    dup_mask: u8,
}

impl RdHeaderAccum {
    /// Duplicate `HEADING` clause bit.
    const DUP_HEADING: u8 = 1 << 0;
    /// Duplicate `FIRST DETAIL` clause bit.
    const DUP_FIRST_DETAIL: u8 = 1 << 1;
    /// Duplicate `LAST DETAIL` clause bit.
    const DUP_LAST_DETAIL: u8 = 1 << 2;
    /// Duplicate `FOOTING` clause bit.
    const DUP_FOOTING: u8 = 1 << 3;
    /// Duplicate `LINE LIMIT` / `COL LIMIT` clause bit.
    const DUP_COL_LIMIT: u8 = 1 << 4;
}

impl Default for RdHeaderAccum {
    /// Returns an empty `RD` header clause accumulator (`Local` visibility, no clauses).
    fn default() -> Self {
        Self {
            global: GlobalVisibility::Local,
            code_clause: None,
            control_items: Vec::new(),
            controls: Vec::new(),
            page_limit: None,
            page_limit_span: None,
            col_limit: None,
            heading: None,
            heading_span: None,
            first_detail: None,
            first_detail_span: None,
            last_detail: None,
            last_detail_span: None,
            footing: None,
            footing_span: None,
            dup_mask: 0,
        }
    }
}

impl Parser<'_> {
    /// Parses `[IS] SPECIAL-NAMES {CRT STATUS | CURSOR}` on a Data Division entry.
    pub(crate) fn try_parse_special_names_data_role(&mut self) -> Option<SpecialNamesDataRole> {
        if !(self.at_word("SPECIAL-NAMES")
            || self.at_word("IS") && self.peek_word_at(1, "SPECIAL-NAMES"))
        {
            return None;
        }
        let _ = self.consume_word("IS");
        let _ = self.consume_word("SPECIAL-NAMES");
        if self.consume_word("CRT") {
            let _ = self.consume_word("STATUS");
            Some(SpecialNamesDataRole::CrtStatus)
        } else if self.consume_word("CURSOR") {
            Some(SpecialNamesDataRole::Cursor)
        } else {
            None
        }
    }

    // ========================================================================
    // Milestone M18: `REPORT SECTION` (`RD` + `01`..`49` Report Groups)
    // ========================================================================

    /// Parses `REPORT SECTION` (`RD` entries and subordinate `01`..`49` report groups).
    ///
    /// # Arguments
    ///
    /// * `out` - Output `RD` descriptor accumulator.
    pub(crate) fn parse_report_section_m18(&mut self, out: &mut Vec<RdEntry>) -> Result<()> {
        while !self.at_eof()
            && !self.starts_after_data_division()
            && !self.starts_other_data_section()
        {
            self.tick()?;
            self.skip_separators();
            if self.at_period() {
                self.advance();
                continue;
            }
            if self.at_word("RD") {
                out.push(self.parse_rd_entry_m18()?);
            } else if !self.at_eof()
                && !self.starts_after_data_division()
                && !self.starts_other_data_section()
            {
                self.emit_syntax_error(self.current_span(), "expected `RD` in REPORT SECTION");
                self.consume_to_period();
            }
        }
        Ok(())
    }

    /// Parses a single `RD report-name ... .` entry and its subordinate `01`..`49` items.
    fn parse_rd_entry_m18(&mut self) -> Result<RdEntry> {
        let start = self.current_span();
        self.advance();
        let (report_name, name_span) = if let Some((sym, sp, _)) = self.eat_word() {
            (sym, sp)
        } else {
            self.emit_syntax_error(start, "expected report-name after `RD`");
            (self.interner.intern("__MISSING_RD")?, start)
        };
        let mut hdr = RdHeaderAccum::default();

        while !self.at_end_or_period() {
            self.tick()?;
            let _ = self.consume_word("IS");
            let _ = self.consume_word("ARE");
            let clause_sp = self.current_span();
            self.parse_one_rd_header_clause(clause_sp, &mut hdr);
        }
        let end_hdr = self.current_span();
        for (bit, label) in [
            (RdHeaderAccum::DUP_HEADING, "duplicate `HEADING` clause"),
            (
                RdHeaderAccum::DUP_FIRST_DETAIL,
                "duplicate `FIRST DETAIL` clause",
            ),
            (
                RdHeaderAccum::DUP_LAST_DETAIL,
                "duplicate `LAST DETAIL` clause",
            ),
            (RdHeaderAccum::DUP_FOOTING, "duplicate `FOOTING` clause"),
            (
                RdHeaderAccum::DUP_COL_LIMIT,
                "duplicate `LINE LIMIT` clause",
            ),
        ] {
            if (hdr.dup_mask & bit) != 0 {
                self.emit_syntax_error(end_hdr, label);
            }
        }
        if hdr.page_limit.is_none()
            && (hdr.heading.is_some()
                || hdr.first_detail.is_some()
                || hdr.last_detail.is_some()
                || hdr.footing.is_some())
        {
            self.emit_syntax_error(
                end_hdr,
                "cannot specify `HEADING`/`DETAIL`/`FOOTING` without number of lines on page",
            );
        }
        let _ = self.consume_period();
        let groups = self.parse_rd_subordinate_groups()?;

        Ok(RdEntry {
            report_name,
            global: hdr.global,
            code_clause: hdr.code_clause,
            control_items: hdr.control_items,
            controls: hdr.controls,
            page_limit: hdr.page_limit,
            page_limit_span: hdr.page_limit_span,
            col_limit: hdr.col_limit,
            heading: hdr.heading,
            heading_span: hdr.heading_span,
            first_detail: hdr.first_detail,
            first_detail_span: hdr.first_detail_span,
            last_detail: hdr.last_detail,
            last_detail_span: hdr.last_detail_span,
            footing: hdr.footing,
            footing_span: hdr.footing_span,
            groups,
            span: start.merge(name_span).merge(end_hdr),
        })
    }

    /// Parses one header clause inside an `RD` entry into `hdr`.
    ///
    /// # Arguments
    ///
    /// * `clause_sp` - Starting span of the clause.
    /// * `hdr` - Mutable `RD` header clause accumulator.
    fn parse_one_rd_header_clause(&mut self, clause_sp: SourceSpan, hdr: &mut RdHeaderAccum) {
        if self.consume_word("GLOBAL") {
            hdr.global = GlobalVisibility::Global;
        } else if self.consume_word("CODE") {
            let _ = self.consume_word("IS");
            hdr.code_clause = self.parse_rd_code_clause(clause_sp);
        } else if self.consume_any_word(&["CONTROL", "CONTROLS"]) {
            let _ = self.consume_word("IS");
            let _ = self.consume_word("ARE");
            self.parse_rd_controls(&mut hdr.control_items, &mut hdr.controls);
        } else if self.at_word("PAGE")
            || (self.at_word("LINE")
                && matches!(self.peek_word_str_at(1), Some("LIMIT" | "LIMITS")))
            || matches!(
                self.current_word(),
                Some("COL" | "COLS" | "COLUMN" | "COLUMNS")
            )
        {
            let mut dup = false;
            self.parse_rd_page_or_col_limit(
                clause_sp,
                &mut hdr.page_limit,
                &mut hdr.page_limit_span,
                &mut hdr.col_limit,
                &mut dup,
            );
            if dup {
                hdr.dup_mask |= RdHeaderAccum::DUP_COL_LIMIT;
            }
        } else if self.consume_word("HEADING") {
            let mut dup = false;
            self.parse_rd_zone_line_clause(
                clause_sp,
                &mut hdr.heading,
                &mut hdr.heading_span,
                &mut dup,
            );
            if dup {
                hdr.dup_mask |= RdHeaderAccum::DUP_HEADING;
            }
        } else if self.consume_word_pair("FIRST", "DETAIL") {
            let mut dup = false;
            self.parse_rd_zone_line_clause(
                clause_sp,
                &mut hdr.first_detail,
                &mut hdr.first_detail_span,
                &mut dup,
            );
            if dup {
                hdr.dup_mask |= RdHeaderAccum::DUP_FIRST_DETAIL;
            }
        } else if self.consume_word_pair("LAST", "DETAIL") {
            let mut dup = false;
            self.parse_rd_zone_line_clause(
                clause_sp,
                &mut hdr.last_detail,
                &mut hdr.last_detail_span,
                &mut dup,
            );
            if dup {
                hdr.dup_mask |= RdHeaderAccum::DUP_LAST_DETAIL;
            }
        } else if self.consume_word("FOOTING") {
            let mut dup = false;
            self.parse_rd_zone_line_clause(
                clause_sp,
                &mut hdr.footing,
                &mut hdr.footing_span,
                &mut dup,
            );
            if dup {
                hdr.dup_mask |= RdHeaderAccum::DUP_FOOTING;
            }
        } else {
            self.advance();
        }
    }

    /// Parses `PAGE LIMIT`, `LINE LIMIT`, or `COL LIMIT` clauses on an `RD` entry.
    ///
    /// # Arguments
    ///
    /// * `clause_sp` - Starting span of the clause keyword.
    /// * `page_limit` - Output slot for `PAGE LIMIT` line count.
    /// * `page_limit_span` - Output slot for `PAGE LIMIT` span.
    /// * `col_limit` - Output slot for `COL` / `LINE LIMIT` horizontal column limit.
    /// * `dup_col_limit` - Set to `true` when duplicate horizontal limits occur.
    fn parse_rd_page_or_col_limit(
        &mut self,
        clause_sp: SourceSpan,
        page_limit: &mut Option<u32>,
        page_limit_span: &mut Option<SourceSpan>,
        col_limit: &mut Option<RdColLimitAst>,
        dup_col_limit: &mut bool,
    ) {
        if self.consume_word("PAGE") {
            let _ = self.consume_any_word(&["LIMIT", "LIMITS"]);
            let _ = self.consume_word("IS");
            let _ = self.consume_word("ARE");
            if let Some((v, v_sp)) = self.eat_u32_with_span() {
                if self.consume_any_word(&["COL", "COLS", "COLUMN", "COLUMNS"]) {
                    if col_limit.is_some() {
                        *dup_col_limit = true;
                    }
                    *col_limit = Some(RdColLimitAst::Literal(v, clause_sp.merge(v_sp)));
                } else {
                    let _ = self.consume_any_word(&["LINE", "LINES"]);
                    *page_limit = Some(v);
                    *page_limit_span = Some(clause_sp.merge(v_sp));
                    if let Some(cl) = self.try_parse_rd_trailing_columns() {
                        if col_limit.is_some() {
                            *dup_col_limit = true;
                        }
                        *col_limit = Some(cl);
                    }
                }
            } else if let Some(cl) = self.try_parse_rd_trailing_columns() {
                if col_limit.is_some() {
                    *dup_col_limit = true;
                }
                *col_limit = Some(cl);
            }
        } else if self.consume_word_pair("LINE", "LIMIT")
            || self.consume_word_pair("LINE", "LIMITS")
        {
            let _ = self.consume_word("IS");
            if col_limit.is_some() {
                *dup_col_limit = true;
            }
            if let Some((v, v_sp)) = self.eat_u32_with_span() {
                let _ = self.consume_any_word(&["COL", "COLS", "COLUMN", "COLUMNS"]);
                *col_limit = Some(RdColLimitAst::Literal(v, clause_sp.merge(v_sp)));
            } else if !is_rd_clause_starter(self.current_word()) && !self.at_end_or_period() {
                let p_sp = self.current_span();
                let path = self.parse_simple_qualified_syms();
                let _ = self.consume_any_word(&["COL", "COLS", "COLUMN", "COLUMNS"]);
                if !path.is_empty() {
                    *col_limit = Some(RdColLimitAst::DataName(path, clause_sp.merge(p_sp)));
                }
            }
        } else if self.consume_any_word(&["COL", "COLS", "COLUMN", "COLUMNS"]) {
            let _ = self.consume_any_word(&["LIMIT", "LIMITS"]);
            let _ = self.consume_word("IS");
            if col_limit.is_some() {
                *dup_col_limit = true;
            }
            if let Some((v, v_sp)) = self.eat_u32_with_span() {
                *col_limit = Some(RdColLimitAst::Literal(v, clause_sp.merge(v_sp)));
            }
        }
    }

    /// Parses `HEADING`, `FIRST DETAIL`, `LAST DETAIL`, or `FOOTING` line number on an `RD` entry.
    ///
    /// # Arguments
    ///
    /// * `clause_sp` - Starting span of the zone keyword.
    /// * `slot` - Output line-number slot.
    /// * `span_slot` - Output span slot for diagnostics.
    /// * `dup_flag` - Set to `true` when the clause is duplicated.
    fn parse_rd_zone_line_clause(
        &mut self,
        clause_sp: SourceSpan,
        slot: &mut Option<u32>,
        span_slot: &mut Option<SourceSpan>,
        dup_flag: &mut bool,
    ) {
        let _ = self.consume_word("IS");
        if slot.is_some() {
            *dup_flag = true;
        }
        if let Some((v, v_sp)) = self.eat_u32_with_span() {
            *slot = Some(v);
            *span_slot = Some(clause_sp.merge(v_sp));
        } else if !is_rd_clause_starter(self.current_word()) && !self.at_end_or_period() {
            let p_sp = self.current_span();
            let _ = self.parse_simple_qualified_syms();
            *slot = Some(1);
            *span_slot = Some(clause_sp.merge(p_sp));
        }
        let _ = self.try_consume_rd_optional_lines_word();
    }

    /// Parses subordinate `01`..`49` `ReportGroupEntry` nodes following an `RD` header.
    fn parse_rd_subordinate_groups(&mut self) -> Result<Vec<ReportGroupEntry>> {
        let mut groups = Vec::new();
        while !self.at_eof()
            && !self.starts_after_data_division()
            && !self.starts_other_data_section()
            && !self.at_word("RD")
        {
            self.tick()?;
            self.skip_separators();
            if self.at_period() {
                self.advance();
                continue;
            }
            if let Some((lvl, lvl_span)) = self.eat_level_number() {
                groups.push(self.parse_report_group_entry_m18(lvl, lvl_span)?);
            } else if !self.at_eof()
                && !self.starts_after_data_division()
                && !self.starts_other_data_section()
                && !self.at_word("RD")
            {
                self.emit_syntax_error(
                    self.current_span(),
                    "expected level-number in REPORT SECTION",
                );
                self.consume_to_period();
            }
        }
        Ok(groups)
    }

    /// Parses `CODE IS <string-lit | qualified-data-name>`.
    ///
    /// # Arguments
    ///
    /// * `clause_sp` - Starting span of `CODE`.
    fn parse_rd_code_clause(&mut self, clause_sp: SourceSpan) -> Option<RdCodeClauseAst> {
        self.skip_separators();
        let tok = self.tokens.get(self.pos)?.clone();
        if let TokenKind::StringLit { bytes, .. } = tok.kind {
            self.advance();
            let text = String::from_utf8_lossy(&bytes);
            let sym = self.interner.intern(&text).ok()?;
            Some(RdCodeClauseAst::Literal(sym, clause_sp.merge(tok.span)))
        } else {
            let sp = self.current_span();
            let path = self.parse_simple_qualified_syms();
            if path.is_empty() {
                None
            } else {
                Some(RdCodeClauseAst::DataName(path, clause_sp.merge(sp)))
            }
        }
    }

    /// Consumes optional `LINE` / `LINES` noise word unless `LINE LIMIT(S)` starts the next `RD` clause.
    fn try_consume_rd_optional_lines_word(&mut self) -> bool {
        let save = self.pos;
        if self.consume_any_word(&["LINE", "LINES"]) {
            if self.at_word("LIMIT") || self.at_word("LIMITS") {
                self.pos = save;
                return false;
            }
            return true;
        }
        false
    }

    /// Parses optional `<n | data-name> COLUMNS` immediately following `PAGE LIMIT n LINES`.
    fn try_parse_rd_trailing_columns(&mut self) -> Option<RdColLimitAst> {
        let save = self.pos;
        if let Some((c, c_sp)) = self.eat_u32_with_span() {
            if self.consume_any_word(&["COL", "COLS", "COLUMN", "COLUMNS"]) {
                return Some(RdColLimitAst::Literal(c, c_sp));
            }
            self.pos = save;
            return None;
        }
        if !is_rd_clause_starter(self.current_word()) && !self.at_end_or_period() {
            let sp = self.current_span();
            let path = self.parse_simple_qualified_syms();
            if !path.is_empty() && self.consume_any_word(&["COL", "COLS", "COLUMN", "COLUMNS"]) {
                return Some(RdColLimitAst::DataName(path, sp));
            }
            self.pos = save;
        }
        None
    }

    /// Parses `CONTROLS ARE {FINAL | qualified-name} ...` inside an `RD` entry.
    ///
    /// # Arguments
    ///
    /// * `items` - Span-annotated control item accumulator.
    /// * `legacy` - Symbol-path accumulator.
    fn parse_rd_controls(
        &mut self,
        items: &mut Vec<ReportControlAst>,
        legacy: &mut Vec<Vec<SymbolId>>,
    ) {
        while !self.at_end_or_period() && !is_rd_clause_starter(self.current_word()) {
            let item_sp = self.current_span();
            if self.consume_word("FINAL") {
                if let Ok(sym) = self.interner.intern("FINAL") {
                    legacy.push(vec![sym]);
                }
                items.push(ReportControlAst::Final(item_sp));
            } else {
                let path = self.parse_simple_qualified_syms();
                if path.is_empty() {
                    break;
                }
                legacy.push(path.clone());
                items.push(ReportControlAst::Field(path, item_sp));
            }
        }
    }

    /// Parses a single `01`..`49` `ReportGroupEntry` inside `REPORT SECTION`.
    ///
    /// # Arguments
    ///
    /// * `level` - Validated level number.
    /// * `start` - Level token span.
    fn parse_report_group_entry_m18(
        &mut self,
        level: LevelNumber,
        start: SourceSpan,
    ) -> Result<ReportGroupEntry> {
        let name = if self.consume_word("FILLER") {
            DataEntryName::Filler
        } else if !self.at_end_or_period() && !is_report_clause_starter(self.current_word()) {
            if let Some((sym, _, _)) = self.eat_word() {
                self.declared_data_names.insert(sym);
                DataEntryName::Named(sym)
            } else {
                DataEntryName::Filler
            }
        } else {
            DataEntryName::Filler
        };

        let mut entry = ReportGroupEntry {
            level,
            name,
            group_type: None,
            group_type_span: None,
            type_control: None,
            line_clauses: Vec::new(),
            next_group: None,
            columns: Vec::new(),
            picture: None,
            usage: None,
            sign: None,
            justified: JustificationMode::StandardLeft,
            blank_when_zero: BlankWhenZeroMode::Normal,
            group_indicate: None,
            sources: Vec::new(),
            sums: Vec::new(),
            values: Vec::new(),
            present_gate: None,
            occurs: None,
            span: start,
        };
        let mut pending_step: Option<u32> = None;
        let mut pending_varying: Option<(SymbolId, i64, i64, SourceSpan)> = None;

        while !self.at_end_or_period() {
            self.tick()?;
            self.parse_one_report_group_clause(
                &mut entry,
                &mut pending_step,
                &mut pending_varying,
            )?;
        }
        let end_sp = self.current_span();
        let _ = self.consume_period();
        entry.span = start.merge(end_sp);
        Ok(entry)
    }

    /// Parses one clause on a `01`..`49` `ReportGroupEntry`.
    ///
    /// # Arguments
    ///
    /// * `entry` - Target [`ReportGroupEntry`] being populated.
    /// * `pending_step` - Pre-`OCCURS` `STEP` value if encountered before `OCCURS`.
    /// * `pending_varying` - Pre-`OCCURS` `VARYING` spec if encountered before `OCCURS`.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if subordinate expression or condition parsing fails.
    fn parse_one_report_group_clause(
        &mut self,
        entry: &mut ReportGroupEntry,
        pending_step: &mut Option<u32>,
        pending_varying: &mut Option<(SymbolId, i64, i64, SourceSpan)>,
    ) -> Result<()> {
        let cl_sp = self.current_span();
        if self.consume_word("TYPE") {
            let _ = self.consume_word("IS");
            if let Some(gt) = self.parse_report_group_type_m18() {
                entry.group_type = Some(gt);
                entry.group_type_span = Some(cl_sp.merge(self.current_span()));
                if matches!(
                    gt,
                    ReportGroupType::ControlHeading | ReportGroupType::ControlFooting
                ) {
                    entry.type_control = self.try_parse_report_control_ref();
                }
            } else {
                self.emit_syntax_error(self.current_span(), "expected valid report group `TYPE`");
                if !self.at_end_or_period() {
                    self.advance();
                }
            }
        } else if self.consume_word_pair("NEXT", "GROUP") {
            let _ = self.consume_word("IS");
            entry.next_group = self.parse_report_next_group_m18(cl_sp);
        } else if self.consume_any_word(&["LINE", "LINES"]) {
            let _ = self.consume_any_word(&["NUMBER", "NUMBERS"]);
            let _ = self.consume_word("IS");
            let _ = self.consume_word("ARE");
            self.parse_report_line_clauses_m18(cl_sp, &mut entry.line_clauses);
        } else if self.consume_any_word(&["COLUMN", "COL", "COLUMNS", "COLS"]) {
            let _ = self.consume_any_word(&["NUMBER", "NUMBERS"]);
            let _ = self.consume_word("IS");
            let _ = self.consume_word("ARE");
            self.parse_report_column_coords_m18(cl_sp, &mut entry.columns);
        } else if matches!(self.current_word(), Some("LEFT" | "CENTER" | "RIGHT")) {
            self.parse_report_column_coords_m18(cl_sp, &mut entry.columns);
        } else if self.at_word("PIC") || self.at_word("PICTURE") {
            entry.picture = self.eat_picture_clause_m18_m19();
        } else if self.consume_word("USAGE") || is_bare_usage_keyword(self.current_word()) {
            let _ = self.consume_word("IS");
            let u_sp = self.current_span();
            let uk = self.parse_usage_kind();
            if let Some(ref k) = uk
                && !matches!(
                    k,
                    UsageKind::Display | UsageKind::National | UsageKind::Utf8
                )
            {
                self.emit_syntax_error(
                    u_sp,
                    "only `DISPLAY`, `NATIONAL`, or `UTF-8` `USAGE` is permitted in `REPORT SECTION`",
                );
            } else {
                entry.usage = uk;
            }
        } else if self.at_word("SIGN") || self.at_word("LEADING") || self.at_word("TRAILING") {
            entry.sign = Some(self.parse_report_or_screen_sign_clause());
        } else if self.consume_any_word(&["JUSTIFIED", "JUST"]) {
            let _ = self.consume_word("RIGHT");
            entry.justified = JustificationMode::JustifiedRight;
        } else if self.consume_word("BLANK") {
            let _ = self.consume_word("WHEN");
            let _ = self.consume_any_word(&["ZERO", "ZEROS", "ZEROES"]);
            entry.blank_when_zero = BlankWhenZeroMode::BlankWhenZero;
        } else if self.consume_word("GROUP") {
            let _ = self.consume_word("INDICATE");
            entry.group_indicate = Some(cl_sp);
        } else if self.consume_word("SOURCE") {
            let _ = self.consume_word("IS");
            let expr = self.parse_report_source_expr()?;
            let end_sp = expr.span();
            entry.sources.push((expr, cl_sp.merge(end_sp)));
        } else if self.consume_word("SUM") {
            entry.sums.push(self.parse_report_sum_clause_m18(cl_sp)?);
        } else if self.consume_any_word(&["VALUE", "VALUES"]) {
            let _ = self.consume_word("IS");
            let _ = self.consume_word("ARE");
            while !self.at_end_or_period() && !is_report_clause_starter(self.current_word()) {
                let a_sp = self.current_span();
                let Some(atom) = self.parse_value_atom()? else {
                    break;
                };
                entry.values.push((atom, a_sp));
            }
        } else if self.at_word("PRESENT")
            || self.at_word("ABSENT")
            || (self.at_word("NOT") && self.peek_word_at(1, "PRESENT"))
        {
            entry.present_gate = self.parse_report_present_gate_m18(cl_sp)?;
        } else {
            self.parse_report_occurs_or_skip(entry, pending_step, pending_varying);
        }
        Ok(())
    }

    /// Parses `OCCURS`, `STEP`, `VARYING`, or skips an unknown token inside `REPORT SECTION`.
    ///
    /// # Arguments
    ///
    /// * `entry` - Target [`ReportGroupEntry`] being populated.
    /// * `pending_step` - Pre-`OCCURS` `STEP` accumulator.
    /// * `pending_varying` - Pre-`OCCURS` `VARYING` accumulator.
    fn parse_report_occurs_or_skip(
        &mut self,
        entry: &mut ReportGroupEntry,
        pending_step: &mut Option<u32>,
        pending_varying: &mut Option<(SymbolId, i64, i64, SourceSpan)>,
    ) {
        if self.consume_word("OCCURS") {
            let base = self.parse_occurs_clause();
            let mut step = pending_step.take();
            let mut varying = pending_varying.take();
            loop {
                if self.consume_word("STEP") {
                    step = self.eat_u32_lit();
                } else if self.at_word("VARYING") {
                    varying = self.parse_report_varying_clause();
                } else {
                    break;
                }
            }
            entry.occurs = Some(ReportOccursExtAst {
                base,
                step,
                varying,
            });
        } else if self.consume_word("STEP") {
            let st = self.eat_u32_lit();
            if let Some(occ) = &mut entry.occurs {
                occ.step = st;
            } else {
                *pending_step = st;
            }
        } else if self.at_word("VARYING") {
            let var = self.parse_report_varying_clause();
            if let Some(occ) = &mut entry.occurs {
                occ.varying = var;
            } else {
                *pending_varying = var;
            }
        } else {
            self.advance();
        }
    }

    /// Parses `TYPE IS ...` keyword combinations and ISO/ANSI-85 abbreviations.
    fn parse_report_group_type_m18(&mut self) -> Option<ReportGroupType> {
        if self.consume_word_pair("REPORT", "HEADING") || self.consume_word("RH") {
            Some(ReportGroupType::ReportHeading)
        } else if self.consume_word_pair("PAGE", "HEADING") || self.consume_word("PH") {
            Some(ReportGroupType::PageHeading)
        } else if self.consume_word_pair("CONTROL", "HEADING") || self.consume_word("CH") {
            let _ = self.consume_word("FOR");
            let _ = self.consume_word("OR");
            Some(ReportGroupType::ControlHeading)
        } else if self.consume_word("DETAIL") || self.consume_word("DE") {
            Some(ReportGroupType::Detail)
        } else if self.consume_word_pair("CONTROL", "FOOTING") || self.consume_word("CF") {
            let _ = self.consume_word("FOR");
            let _ = self.consume_word("OR");
            Some(ReportGroupType::ControlFooting)
        } else if self.consume_word_pair("PAGE", "FOOTING") || self.consume_word("PF") {
            Some(ReportGroupType::PageFooting)
        } else if self.consume_word_pair("REPORT", "FOOTING") || self.consume_word("RF") {
            Some(ReportGroupType::ReportFooting)
        } else {
            None
        }
    }

    /// Parses optional `{FINAL | qualified-control-name}` following `CH`, `CF`, or `RESET ON`.
    fn try_parse_report_control_ref(&mut self) -> Option<ReportControlAst> {
        let sp = self.current_span();
        if self.consume_word("FINAL") {
            return Some(ReportControlAst::Final(sp));
        }
        if !self.at_end_or_period() && !is_report_clause_starter(self.current_word()) {
            let path = self.parse_simple_qualified_syms();
            if !path.is_empty() {
                return Some(ReportControlAst::Field(path, sp));
            }
        }
        None
    }

    /// Parses `LINE [NUMBER IS] ...` (supporting `[n] [ON] NEXT PAGE`, `PLUS/+ n`, or `n`).
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Span of `LINE`.
    /// * `out` - Destination clause vector.
    fn parse_report_line_clauses_m18(
        &mut self,
        cl_sp: SourceSpan,
        out: &mut Vec<ReportLineClauseAst>,
    ) {
        let _ = self.consume_word("ON");
        if self.consume_word_pair("NEXT", "PAGE") {
            out.push(ReportLineClauseAst::OnNextPage {
                line: 1,
                span: cl_sp,
            });
            return;
        }
        if self.consume_word("PLUS") || self.peek().kind == TokenKind::Plus {
            if self.peek().kind == TokenKind::Plus {
                self.advance();
            }
            let off = if self.consume_any_word(&["ZERO", "ZEROS", "ZEROES"]) {
                0
            } else if let Some((v, _)) = self.eat_u32_with_span() {
                v
            } else {
                if self.peek().kind == TokenKind::Minus
                    || matches!(
                        self.peek().kind,
                        TokenKind::IntegerLit(_) | TokenKind::DecimalLit { .. }
                    )
                {
                    let bad_sp = self.current_span();
                    if self.peek().kind == TokenKind::Minus {
                        self.advance();
                    }
                    if matches!(
                        self.peek().kind,
                        TokenKind::IntegerLit(_) | TokenKind::DecimalLit { .. }
                    ) {
                        self.advance();
                    }
                    self.emit_syntax_error(bad_sp, "unsigned integer value expected");
                }
                1
            };
            out.push(ReportLineClauseAst::Relative {
                offset: off,
                span: cl_sp,
            });
            return;
        }
        if let TokenKind::IntegerLit(sym) = self.peek().kind
            && let Some(rest) = self.sym_str(sym).strip_prefix('+')
            && let Ok(off) = rest.parse::<u32>()
        {
            let sp = self.current_span();
            self.advance();
            if self.dialect.base_dialect == Dialect::MicroFocus {
                self.emit_syntax_error(sp, "unsigned integer value expected");
            }
            out.push(ReportLineClauseAst::Relative {
                offset: off,
                span: cl_sp,
            });
            return;
        }
        let mut any_num = false;
        while let Some((ln, ln_sp)) = self.eat_u32_with_span() {
            any_num = true;
            let _ = self.consume_word("ON");
            if self.consume_word_pair("NEXT", "PAGE") {
                out.push(ReportLineClauseAst::OnNextPage {
                    line: ln,
                    span: cl_sp.merge(ln_sp),
                });
            } else {
                out.push(ReportLineClauseAst::Absolute {
                    line: ln,
                    span: cl_sp.merge(ln_sp),
                });
            }
        }
        if !any_num && (self.at_end_or_period() || is_report_clause_starter(self.current_word())) {
            out.push(ReportLineClauseAst::OnNextPage {
                line: 1,
                span: cl_sp,
            });
        }
    }

    /// Parses `NEXT GROUP IS {NEXT PAGE | PLUS/+ n | n}`.
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Span of `NEXT GROUP`.
    fn parse_report_next_group_m18(&mut self, cl_sp: SourceSpan) -> Option<ReportNextGroupAst> {
        let _ = self.consume_word("ON");
        if self.consume_word_pair("NEXT", "PAGE") {
            return Some(ReportNextGroupAst::NextPage { span: cl_sp });
        }
        if self.consume_word("PLUS") || self.peek().kind == TokenKind::Plus {
            if self.peek().kind == TokenKind::Plus {
                self.advance();
            }
            let off = self.eat_signed_or_u32_abs().unwrap_or(1);
            return Some(ReportNextGroupAst::Relative {
                offset: off,
                span: cl_sp,
            });
        }
        if let TokenKind::IntegerLit(sym) = self.peek().kind
            && let Some(rest) = self.sym_str(sym).strip_prefix('+')
            && let Ok(off) = rest.parse::<u32>()
        {
            self.advance();
            return Some(ReportNextGroupAst::Relative {
                offset: off,
                span: cl_sp,
            });
        }
        let (ln, ln_sp) = self.eat_u32_with_span()?;
        Some(ReportNextGroupAst::Absolute {
            line: ln,
            span: cl_sp.merge(ln_sp),
        })
    }

    /// Parses one or more `COLUMN [LEFT|CENTER|RIGHT] [PLUS/+] n` coordinates.
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Span of `COLUMN`.
    /// * `out` - Destination column coordinate list.
    fn parse_report_column_coords_m18(
        &mut self,
        cl_sp: SourceSpan,
        out: &mut Vec<ReportColCoordAst>,
    ) {
        let mut first = true;
        loop {
            let item_sp = if first { cl_sp } else { self.current_span() };
            let mut align = self.eat_report_col_align();
            let is_rel = if self.consume_word("PLUS") {
                true
            } else if self.peek().kind == TokenKind::Plus {
                self.advance();
                true
            } else {
                false
            };
            if align == ReportColAlignAst::LeftDefault {
                align = self.eat_report_col_align();
            }
            if is_rel {
                let off = if self.consume_any_word(&["ZERO", "ZEROS", "ZEROES"]) {
                    0
                } else if let Some((v, _)) = self.eat_u32_with_span() {
                    v
                } else {
                    let bad_sp = self.current_span();
                    if self.peek().kind == TokenKind::Minus {
                        self.advance();
                    }
                    if matches!(
                        self.peek().kind,
                        TokenKind::IntegerLit(_) | TokenKind::DecimalLit { .. }
                    ) {
                        self.advance();
                    }
                    self.emit_syntax_error(bad_sp, "unsigned integer value expected");
                    1
                };
                if align == ReportColAlignAst::LeftDefault {
                    align = self.eat_report_col_align();
                }
                out.push(ReportColCoordAst::Relative {
                    offset: off,
                    align,
                    span: item_sp,
                });
            } else if let TokenKind::IntegerLit(sym) = self.peek().kind
                && let Some(rest) = self.sym_str(sym).strip_prefix('+')
                && let Ok(off) = rest.parse::<u32>()
            {
                let sp = self.current_span();
                self.advance();
                if self.dialect.base_dialect == Dialect::MicroFocus {
                    self.emit_syntax_error(sp, "unsigned integer value expected");
                }
                out.push(ReportColCoordAst::Relative {
                    offset: off,
                    align,
                    span: item_sp,
                });
            } else if let Some((col, col_sp)) = self.eat_u32_with_span() {
                if align == ReportColAlignAst::LeftDefault {
                    align = self.eat_report_col_align();
                }
                out.push(ReportColCoordAst::Absolute {
                    col,
                    align,
                    span: item_sp.merge(col_sp),
                });
            } else if self.peek().kind == TokenKind::Minus
                || matches!(
                    self.peek().kind,
                    TokenKind::IntegerLit(_) | TokenKind::DecimalLit { .. }
                )
            {
                let bad_sp = self.current_span();
                if self.peek().kind == TokenKind::Minus {
                    self.advance();
                }
                if matches!(
                    self.peek().kind,
                    TokenKind::IntegerLit(_) | TokenKind::DecimalLit { .. }
                ) {
                    self.advance();
                }
                self.emit_syntax_error(bad_sp, "unsigned integer value expected");
                out.push(ReportColCoordAst::Absolute {
                    col: 1,
                    align,
                    span: item_sp.merge(bad_sp),
                });
            } else {
                if first && align != ReportColAlignAst::LeftDefault {
                    out.push(ReportColCoordAst::Absolute {
                        col: 1,
                        align,
                        span: item_sp,
                    });
                }
                break;
            }
            first = false;
        }
    }

    /// Consumes optional `LEFT`, `CENTER`, or `RIGHT` alignment keyword.
    fn eat_report_col_align(&mut self) -> ReportColAlignAst {
        if self.consume_word("LEFT") {
            ReportColAlignAst::ExplicitLeft
        } else if self.consume_word("CENTER") || self.consume_word("CENTRE") {
            ReportColAlignAst::Center
        } else if self.consume_word("RIGHT") {
            ReportColAlignAst::Right
        } else {
            ReportColAlignAst::LeftDefault
        }
    }

    /// Parses a `SOURCE IS <expr>` operand (typically a `DataRef` or arithmetic expression).
    fn parse_report_source_expr(&mut self) -> Result<Expr> {
        self.parse_expr()
    }

    /// Parses `SUM operand ... [UPON detail ...] [RESET [ON] {FINAL | control}]`.
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Span of `SUM`.
    fn parse_report_sum_clause_m18(&mut self, cl_sp: SourceSpan) -> Result<ReportSumClauseAst> {
        let mut operands = Vec::new();
        let mut upon_details = Vec::new();
        let mut reset_on = None;

        while !self.at_end_or_period()
            && !is_report_clause_starter(self.current_word())
            && !self.at_word("UPON")
            && !self.at_word("RESET")
        {
            operands.push(self.parse_expr()?);
        }
        if self.consume_word("UPON") {
            while !self.at_end_or_period()
                && !is_report_clause_starter(self.current_word())
                && !self.at_word("RESET")
            {
                let Some((d_sym, d_sp, _)) = self.eat_word() else {
                    break;
                };
                upon_details.push((d_sym, d_sp));
            }
        }
        if self.consume_word("RESET") {
            let _ = self.consume_word("ON");
            reset_on = self.try_parse_report_control_ref();
        }
        Ok(ReportSumClauseAst {
            operands,
            upon_details,
            reset_on,
            span: cl_sp.merge(self.current_span()),
        })
    }

    /// Parses `[NOT] PRESENT {WHEN cond | AFTER [NEW] ...}` or `ABSENT {WHEN cond | AFTER [NEW] ...}`.
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Starting clause span.
    fn parse_report_present_gate_m18(
        &mut self,
        cl_sp: SourceSpan,
    ) -> Result<Option<ReportPresentGateAst>> {
        let polarity = if self.consume_word("NOT") {
            let _ = self.consume_word("PRESENT");
            ReportPresentPolarity::Absent
        } else if self.consume_word("ABSENT") {
            ReportPresentPolarity::Absent
        } else {
            let _ = self.consume_word("PRESENT");
            ReportPresentPolarity::Present
        };
        let _ = self.consume_word("JUST");
        if self.consume_word("AFTER") {
            let _ = self.consume_word("NEW");
            let mut triggers = Vec::new();
            loop {
                let _ = self.consume_word("NEW");
                let a_sp = self.current_span();
                if self.consume_word("PAGE") {
                    triggers.push(ReportAfterNewAtomAst::Page(a_sp));
                } else if self.consume_word("FINAL") {
                    triggers.push(ReportAfterNewAtomAst::Final(a_sp));
                } else if !self.at_end_or_period() && !is_report_clause_starter(self.current_word())
                {
                    let path = self.parse_simple_qualified_syms();
                    if path.is_empty() {
                        break;
                    }
                    triggers.push(ReportAfterNewAtomAst::Control(path, a_sp));
                } else {
                    break;
                }
                if !self.consume_word("OR") {
                    break;
                }
            }
            Ok(Some(ReportPresentGateAst::AfterNew {
                polarity,
                triggers,
                span: cl_sp.merge(self.current_span()),
            }))
        } else {
            let _ = self.consume_word("WHEN");
            let cond = self.parse_condition()?;
            Ok(Some(ReportPresentGateAst::WhenCondition {
                polarity,
                condition: Box::new(cond),
                span: cl_sp.merge(self.current_span()),
            }))
        }
    }

    /// Parses `VARYING idx [FROM start] [BY step]` on a report `OCCURS` item.
    fn parse_report_varying_clause(&mut self) -> Option<(SymbolId, i64, i64, SourceSpan)> {
        let sp = self.current_span();
        if !self.consume_word("VARYING") {
            return None;
        }
        let (sym, _, _) = self.eat_word()?;
        self.declared_data_names.insert(sym);
        let from_val = if self.consume_word("FROM") {
            self.eat_i64_lit().unwrap_or(1)
        } else {
            1
        };
        let by_val = if self.consume_word("BY") {
            self.eat_i64_lit().unwrap_or(1)
        } else {
            1
        };
        Some((sym, from_val, by_val, sp))
    }

    /// Parses `SIGN [IS] {LEADING | TRAILING} [SEPARATE [CHARACTER]]`.
    fn parse_report_or_screen_sign_clause(&mut self) -> SignClause {
        let _ = self.consume_word("SIGN");
        let _ = self.consume_word("IS");
        let placement = if self.consume_word("LEADING") {
            SignPlacement::Leading
        } else {
            let _ = self.consume_word("TRAILING");
            SignPlacement::Trailing
        };
        let separation = if self.consume_word("SEPARATE") {
            let _ = self.consume_word("CHARACTER");
            SignSeparation::SeparateCharacter
        } else {
            SignSeparation::Embedded
        };
        SignClause {
            placement,
            separation,
        }
    }

    /// Consumes `PIC`/`PICTURE` `[IS] <picture-string>`.
    fn eat_picture_clause_m18_m19(&mut self) -> Option<(SymbolId, SourceSpan)> {
        if !self.consume_any_word(&["PIC", "PICTURE"]) {
            return None;
        }
        let _ = self.consume_word("IS");
        if let TokenKind::PictureString(sym) = self.peek().kind {
            let sp = self.current_span();
            self.advance();
            Some((sym, sp))
        } else {
            self.emit_syntax_error(self.current_span(), "expected PICTURE string after `PIC`");
            None
        }
    }

    // ========================================================================
    // Milestone M19: `SCREEN SECTION` & Extended `DISPLAY` / `ACCEPT` / `SET`
    // ========================================================================

    /// Parses `SCREEN SECTION` entries (`01`..`49`, `77`, plus `88` captured for semantic rejection).
    ///
    /// # Arguments
    ///
    /// * `out` - Output screen entry vector.
    pub(crate) fn parse_screen_section_m19(&mut self, out: &mut Vec<ScreenEntry>) -> Result<()> {
        while !self.at_eof()
            && !self.starts_after_data_division()
            && !self.starts_other_data_section()
        {
            self.tick()?;
            self.skip_separators();
            if self.at_period() {
                self.advance();
                continue;
            }
            let Some((level, start)) = self.try_eat_screen_level_number() else {
                if !self.at_eof()
                    && !self.starts_after_data_division()
                    && !self.starts_other_data_section()
                {
                    self.emit_syntax_error(
                        self.current_span(),
                        "expected level-number in SCREEN SECTION",
                    );
                    self.consume_to_period();
                }
                continue;
            };
            let name = if self.consume_word("FILLER") {
                DataEntryName::Filler
            } else if !self.at_end_or_period()
                && !is_screen_clause_starter(self.current_word(), self.peek())
            {
                if let Some((sym, _, _)) = self.eat_word() {
                    self.declared_data_names.insert(sym);
                    DataEntryName::Named(sym)
                } else {
                    DataEntryName::Filler
                }
            } else {
                DataEntryName::Filler
            };

            let mut clauses = Vec::new();
            while !self.at_end_or_period() {
                self.tick()?;
                if let Some(cl) = self.try_parse_screen_section_clause()? {
                    clauses.push(cl);
                } else {
                    self.emit_syntax_error(
                        self.current_span(),
                        "unrecognized clause in SCREEN SECTION entry",
                    );
                    self.advance();
                }
            }
            let end_sp = self.current_span();
            let _ = self.consume_period();
            out.push(ScreenEntry {
                level,
                name,
                clauses,
                span: start.merge(end_sp),
            });
        }
        Ok(())
    }

    /// Consumes a level number in `SCREEN SECTION` (allowing `88` without a parser error so sema reports it).
    fn try_eat_screen_level_number(&mut self) -> Option<(LevelNumber, SourceSpan)> {
        self.skip_separators();
        let tok = self.tokens.get(self.pos)?;
        let raw_num = match tok.kind {
            TokenKind::IntegerLit(sym) | TokenKind::Word { sym, .. } => {
                let s = self.sym_str(sym);
                if s.len() > 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                s.parse::<u8>().ok()?
            }
            _ => return None,
        };
        let lvl = LevelNumber::parse(raw_num).ok()?;
        let sp = tok.span;
        self.advance();
        Some((lvl, sp))
    }

    /// Parses a single clause inside a `SCREEN SECTION` entry.
    fn try_parse_screen_section_clause(&mut self) -> Result<Option<ScreenClauseAst>> {
        self.skip_separators();
        let cl_sp = self.current_span();
        if let TokenKind::StringLit { .. } = self.peek().kind
            && let Some(atom) = self.parse_value_atom()?
        {
            return Ok(Some(ScreenClauseAst::Value {
                atom,
                implicit_keyword: true,
                span: cl_sp,
            }));
        }
        if self.at_word("PIC") || self.at_word("PICTURE") {
            if let Some((raw, sp)) = self.eat_picture_clause_m18_m19() {
                return Ok(Some(ScreenClauseAst::Picture { raw, span: sp }));
            }
            return Ok(None);
        }
        if self.consume_word("VALUE") || self.consume_word("VALUES") {
            let _ = self.consume_word("IS");
            let _ = self.consume_word("ARE");
            if let Some(atom) = self.parse_value_atom()? {
                return Ok(Some(ScreenClauseAst::Value {
                    atom,
                    implicit_keyword: false,
                    span: cl_sp.merge(self.current_span()),
                }));
            }
            return Ok(None);
        }
        if self.consume_word("FROM") {
            let expr = self.parse_screen_from_expr()?;
            return Ok(Some(ScreenClauseAst::From {
                span: cl_sp.merge(expr.span()),
                expr,
            }));
        }
        if self.consume_word("TO") {
            let target = self.parse_screen_target_ref()?;
            return Ok(Some(ScreenClauseAst::To {
                span: cl_sp.merge(target.span),
                target,
            }));
        }
        if self.consume_word("USING") {
            let target = self.parse_screen_target_ref()?;
            return Ok(Some(ScreenClauseAst::Using {
                span: cl_sp.merge(target.span),
                target,
            }));
        }
        if self.consume_word("USAGE") {
            let _ = self.consume_word("IS");
            if let Some(usage) = self.parse_usage_kind() {
                return Ok(Some(ScreenClauseAst::Usage { usage, span: cl_sp }));
            }
            return Ok(None);
        }
        if is_bare_usage_keyword(self.current_word()) {
            if let Some(usage) = self.parse_usage_kind() {
                return Ok(Some(ScreenClauseAst::Usage { usage, span: cl_sp }));
            }
            return Ok(None);
        }
        if self.at_word("SIGN") || self.at_word("LEADING") || self.at_word("TRAILING") {
            let sign = self.parse_report_or_screen_sign_clause();
            return Ok(Some(ScreenClauseAst::Sign {
                span: cl_sp.merge(self.current_span()),
                sign,
            }));
        }
        if self.consume_any_word(&["JUSTIFIED", "JUST"]) {
            let _ = self.consume_word("RIGHT");
            return Ok(Some(ScreenClauseAst::Justified(cl_sp)));
        }
        if self.consume_word("OCCURS") {
            let occ = self.parse_occurs_clause();
            return Ok(Some(ScreenClauseAst::Occurs(occ)));
        }
        self.try_parse_common_screen_attr_clause(cl_sp, true)
    }

    /// Parses `FROM <expr>` in `SCREEN SECTION`, rejecting dialect-reserved words used as data names.
    fn parse_screen_from_expr(&mut self) -> Result<Expr> {
        self.skip_separators();
        if let TokenKind::Word {
            sym,
            role: WordRole::ReservedKeyword,
            ..
        } = self.peek().kind
        {
            let w = self.sym_str(sym);
            if match_figurative_word(w).is_none() && !is_valid_reserved_expr_starter(w) {
                let sp = self.current_span();
                self.emit_syntax_error(
                    sp,
                    &format!("unexpected reserved word `{w}` in `FROM` clause"),
                );
                self.advance();
                let dummy = self.interner.intern("__INVALID_RESERVED_FROM")?;
                return Ok(Expr::Data(DataRef {
                    modifier: crate::ast::proc_div::DataRefModifier::Plain,
                    name: crate::ast::proc_div::QualifiedName {
                        base: dummy,
                        qualifiers: Vec::new(),
                        span: sp,
                    },
                    subscripts: Vec::new(),
                    refmod: None,
                    span: sp,
                }));
            }
        }
        self.parse_screen_bounded_expr()
    }

    /// Parses `TO <data-ref>` or `USING <data-ref>` in `SCREEN SECTION`, rejecting reserved words.
    fn parse_screen_target_ref(&mut self) -> Result<DataRef> {
        self.skip_separators();
        if let TokenKind::Word {
            sym,
            role: WordRole::ReservedKeyword,
            ..
        } = self.peek().kind
        {
            let w = self.sym_str(sym);
            if !is_valid_reserved_expr_starter(w) {
                let sp = self.current_span();
                self.emit_syntax_error(sp, &format!("unexpected reserved word `{w}`"));
                self.advance();
                let dummy = self.interner.intern("__INVALID_RESERVED_TARGET")?;
                return Ok(DataRef {
                    modifier: crate::ast::proc_div::DataRefModifier::Plain,
                    name: crate::ast::proc_div::QualifiedName {
                        base: dummy,
                        qualifiers: Vec::new(),
                        span: sp,
                    },
                    subscripts: Vec::new(),
                    refmod: None,
                    span: sp,
                });
            }
        }
        self.parse_data_ref()
    }

    /// Parses screen positioning, visual, and input behavior clauses shared by `SCREEN SECTION`, `DISPLAY`, and `ACCEPT`.
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Starting span of the clause.
    /// * `in_screen_section` - `true` inside `SCREEN SECTION` (enables `BLANK WHEN ZERO`).
    fn try_parse_common_screen_attr_clause(
        &mut self,
        cl_sp: SourceSpan,
        in_screen_section: bool,
    ) -> Result<Option<ScreenClauseAst>> {
        let had_with = self.consume_word("WITH");
        let cl_sp = if had_with { cl_sp } else { self.current_span() };

        if self.consume_word("AT") {
            if self.at_word("LINE") || self.at_word("COLUMN") || self.at_word("COL") {
                // `AT LINE ...` / `AT COLUMN ...`: fall through to `LINE` / `COLUMN`.
            } else {
                let expr = self.parse_screen_bounded_expr()?;
                let post_span = self.current_span();
                return Ok(Some(ScreenClauseAst::At {
                    span: cl_sp.merge(expr.span()),
                    expr,
                    post_span,
                }));
            }
        }
        if self.consume_word("LINE") {
            let _ = self.consume_word("NUMBER");
            let _ = self.consume_word("IS");
            let coord = self.parse_screen_coord_ast(cl_sp, in_screen_section)?;
            return Ok(Some(ScreenClauseAst::Line(coord)));
        }
        if !self.col_is_custom_register() && self.consume_any_word(&["COLUMN", "COL", "POS"]) {
            let _ = self.consume_word("NUMBER");
            let _ = self.consume_word("IS");
            let coord = self.parse_screen_coord_ast(cl_sp, in_screen_section)?;
            return Ok(Some(ScreenClauseAst::Column(coord)));
        }
        if self.consume_word("COLUMN") {
            let _ = self.consume_word("NUMBER");
            let _ = self.consume_word("IS");
            let coord = self.parse_screen_coord_ast(cl_sp, in_screen_section)?;
            return Ok(Some(ScreenClauseAst::Column(coord)));
        }
        if self.consume_word("BLANK") {
            if self.consume_word("SCREEN") {
                return Ok(Some(ScreenClauseAst::BlankScreen(cl_sp)));
            }
            if self.consume_word("LINE") {
                return Ok(Some(ScreenClauseAst::BlankLine(cl_sp)));
            }
            if in_screen_section {
                let _ = self.consume_word("WHEN");
                let _ = self.consume_any_word(&["ZERO", "ZEROS", "ZEROES"]);
                return Ok(Some(ScreenClauseAst::BlankWhenZero(cl_sp)));
            }
            return Ok(Some(ScreenClauseAst::BlankScreen(cl_sp)));
        }
        if self.consume_word("ERASE") {
            let _ = self.consume_word("TO");
            let _ = self.consume_word("END");
            let _ = self.consume_word("OF");
            if self.consume_any_word(&["EOL", "LINE"]) {
                return Ok(Some(ScreenClauseAst::EraseEol(cl_sp)));
            }
            let _ = self.consume_any_word(&["EOS", "SCREEN"]);
            return Ok(Some(ScreenClauseAst::EraseEos(cl_sp)));
        }
        if let Some(flag_cl) = self.try_parse_simple_screen_flag(cl_sp) {
            return Ok(Some(flag_cl));
        }
        self.try_parse_parameterized_screen_clause(cl_sp, in_screen_section)
    }

    /// Parses zero-operand visual/editing screen attribute flags (`BELL`, `HIGHLIGHT`, `AUTO`, etc.).
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Starting span of the attribute keyword.
    fn try_parse_simple_screen_flag(&mut self, cl_sp: SourceSpan) -> Option<ScreenClauseAst> {
        if self.consume_any_word(&["BELL", "BEEP"]) {
            Some(ScreenClauseAst::Bell(cl_sp))
        } else if self.consume_any_word(&["HIGHLIGHT", "BRI", "BRIGHT"]) {
            Some(ScreenClauseAst::Highlight(cl_sp))
        } else if self.consume_word("LOWLIGHT") {
            Some(ScreenClauseAst::Lowlight(cl_sp))
        } else if self.consume_word("BLINK") {
            Some(ScreenClauseAst::Blink(cl_sp))
        } else if self.consume_any_word(&["REVERSE-VIDEO", "REVERSED", "REVERSE"]) {
            Some(ScreenClauseAst::ReverseVideo(cl_sp))
        } else if self.consume_word("UNDERLINE") {
            Some(ScreenClauseAst::Underline(cl_sp))
        } else if self.consume_word("OVERLINE") {
            Some(ScreenClauseAst::Overline(cl_sp))
        } else if self.consume_word("LEFTLINE") {
            Some(ScreenClauseAst::Leftline(cl_sp))
        } else if self.consume_word("RIGHTLINE") {
            Some(ScreenClauseAst::Rightline(cl_sp))
        } else if self.consume_word("GRID") {
            Some(ScreenClauseAst::Grid(cl_sp))
        } else if self.consume_any_word(&["AUTO", "AUTO-SKIP", "AUTOTERMINATE"]) {
            Some(ScreenClauseAst::Auto(cl_sp))
        } else if self.consume_word("TAB") {
            Some(ScreenClauseAst::Tab(cl_sp))
        } else if self.consume_any_word(&["SECURE", "NO-ECHO", "OFF"]) {
            Some(ScreenClauseAst::Secure(cl_sp))
        } else if self.consume_any_word(&["REQUIRED", "EMPTY-CHECK"]) {
            Some(ScreenClauseAst::Required(cl_sp))
        } else if self.consume_any_word(&["FULL", "LENGTH-CHECK"]) {
            Some(ScreenClauseAst::Full(cl_sp))
        } else if self.consume_word("ZERO-FILL") {
            Some(ScreenClauseAst::ZeroFill(cl_sp))
        } else if self.consume_word("INITIAL") {
            Some(ScreenClauseAst::Initial(cl_sp))
        } else {
            None
        }
    }

    /// Parses parameterized screen clauses (`PROMPT`, `UPDATE`, `SIZE`, colors, `SCROLL`, `TIMEOUT`, `CURSOR`).
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Starting span of the screen clause.
    /// * `in_screen_section` - `true` when parsing inside `SCREEN SECTION` rather than `DISPLAY`/`ACCEPT`.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if expression parsing fails.
    fn try_parse_parameterized_screen_clause(
        &mut self,
        cl_sp: SourceSpan,
        in_screen_section: bool,
    ) -> Result<Option<ScreenClauseAst>> {
        if self.consume_word("PROMPT") {
            let _ = self.consume_word("CHARACTER");
            let _ = self.consume_word("IS");
            let char_expr = if matches!(self.peek().kind, TokenKind::StringLit { .. })
                || match_figurative_word(self.current_word().unwrap_or("")).is_some()
                || (!self.at_end_or_period()
                    && !is_screen_clause_starter(self.current_word(), self.peek())
                    && !self.at_screen_io_boundary())
            {
                Some(self.parse_screen_bounded_expr()?)
            } else {
                None
            };
            return Ok(Some(ScreenClauseAst::Prompt {
                char_expr,
                span: cl_sp,
            }));
        }
        if self.consume_word_pair("NO", "UPDATE") {
            return Ok(Some(ScreenClauseAst::Update {
                enabled: false,
                span: cl_sp,
            }));
        }
        if self.consume_any_word(&["UPDATE", "DEFAULT"]) {
            return Ok(Some(ScreenClauseAst::Update {
                enabled: true,
                span: cl_sp,
            }));
        }
        if self.consume_word("SIZE") {
            let _ = self.consume_word("IS");
            let expr = self.parse_screen_bounded_expr()?;
            return Ok(Some(ScreenClauseAst::Size {
                span: cl_sp.merge(expr.span()),
                expr,
            }));
        }
        if self.consume_any_word(&["FOREGROUND-COLOR", "FOREGROUND-COLOUR", " FOREGROUND"]) {
            let _ = self.consume_word("IS");
            let expr = self.parse_screen_color_expr()?;
            return Ok(Some(ScreenClauseAst::ForegroundColor {
                span: cl_sp.merge(expr.span()),
                expr,
            }));
        }
        if self.consume_any_word(&["BACKGROUND-COLOR", "BACKGROUND-COLOUR", "BACKGROUND"]) {
            let _ = self.consume_word("IS");
            let expr = self.parse_screen_color_expr()?;
            return Ok(Some(ScreenClauseAst::BackgroundColor {
                span: cl_sp.merge(expr.span()),
                expr,
            }));
        }
        if self.consume_any_word(&["COLOR", "COLOUR"]) {
            let _ = self.consume_word("IS");
            let expr = self.parse_screen_bounded_expr()?;
            return Ok(Some(ScreenClauseAst::Color {
                span: cl_sp.merge(expr.span()),
                expr,
            }));
        }
        if self.consume_word("CONTROL") {
            let _ = self.consume_word("IS");
            let expr = self.parse_screen_bounded_expr()?;
            return Ok(Some(ScreenClauseAst::Control {
                span: cl_sp.merge(expr.span()),
                expr,
            }));
        }
        if self.consume_word("MODE") {
            let _ = self.consume_word("IS");
            let _ = self.consume_word("BLOCK");
            return Ok(Some(ScreenClauseAst::ModeIsBlock(cl_sp)));
        }
        if self.consume_word("CONVERSION") {
            return Ok(Some(ScreenClauseAst::Conversion(cl_sp)));
        }
        self.try_parse_scroll_timeout_cursor_clause(cl_sp, in_screen_section)
    }

    /// Parses `SCROLL`, `TIME-OUT` / `BEFORE TIME`, and `CURSOR IS` screen clauses.
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Starting span of the screen clause.
    /// * `in_screen_section` - `true` when parsing inside `SCREEN SECTION`.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if expression or data-ref parsing fails.
    fn try_parse_scroll_timeout_cursor_clause(
        &mut self,
        cl_sp: SourceSpan,
        in_screen_section: bool,
    ) -> Result<Option<ScreenClauseAst>> {
        if self.consume_word("SCROLL") {
            let down = if self.consume_word("DOWN") {
                true
            } else {
                let _ = self.consume_word("UP");
                false
            };
            let _ = self.consume_word("BY");
            self.skip_separators();
            let lines = if !self.at_end_or_period()
                && !is_screen_clause_starter(self.current_word(), self.peek())
                && !self.at_screen_io_boundary()
            {
                let e = self.parse_screen_bounded_expr()?;
                let _ = self.consume_any_word(&["LINE", "LINES"]);
                Some(e)
            } else {
                None
            };
            let scroll_sp = if let Some(ref e) = lines {
                e.span()
            } else {
                self.skip_separators();
                self.current_span()
            };
            return Ok(Some(ScreenClauseAst::Scroll {
                down,
                lines,
                span: scroll_sp,
            }));
        }
        if self.consume_any_word(&["TIME-OUT", "TIMEOUT"]) {
            let _ = self.consume_word("AFTER");
            let expr = self.parse_screen_bounded_expr()?;
            let _ = self.consume_any_word(&["SECOND", "SECONDS"]);
            return Ok(Some(ScreenClauseAst::Timeout {
                span: cl_sp.merge(expr.span()),
                expr,
                unit_ms: 1000,
            }));
        }
        if self.consume_word_pair("BEFORE", "TIME") {
            let expr = self.parse_screen_bounded_expr()?;
            return Ok(Some(ScreenClauseAst::Timeout {
                span: cl_sp.merge(expr.span()),
                expr,
                unit_ms: 10,
            }));
        }
        if !in_screen_section && self.consume_word("CURSOR") {
            let _ = self.consume_word("IS");
            let expr = self.parse_screen_bounded_expr()?;
            return Ok(Some(ScreenClauseAst::CursorOffset {
                span: cl_sp.merge(expr.span()),
                expr,
            }));
        }
        Ok(None)
    }

    /// Parses a `LINE` or `COLUMN` coordinate, detecting malformed inline signed or decimal literals.
    ///
    /// # Arguments
    ///
    /// * `cl_sp` - Span of `LINE` / `COLUMN`.
    /// * `in_screen_section` - `true` when inside `SCREEN SECTION`.
    fn parse_screen_coord_ast(
        &mut self,
        cl_sp: SourceSpan,
        in_screen_section: bool,
    ) -> Result<ScreenCoordAst> {
        self.skip_separators();
        let dir = if self.consume_word("PLUS") {
            ScreenCoordDirAst::Plus
        } else if self.consume_word("MINUS") {
            ScreenCoordDirAst::Minus
        } else if self.peek().kind == TokenKind::Plus {
            self.advance();
            ScreenCoordDirAst::Plus
        } else if self.peek().kind == TokenKind::Minus {
            self.advance();
            ScreenCoordDirAst::Minus
        } else {
            ScreenCoordDirAst::Absolute
        };
        self.skip_separators();
        if self.at_end_or_period()
            || (in_screen_section && is_screen_clause_starter(self.current_word(), self.peek()))
            || (!in_screen_section && self.at_screen_io_coord_end())
        {
            let default_expr = if dir == ScreenCoordDirAst::Absolute {
                None
            } else {
                let one_sym = self.interner.intern("1")?;
                Some(Expr::Literal {
                    atom: ValueAtom::Integer(one_sym),
                    span: cl_sp,
                })
            };
            return Ok(ScreenCoordAst {
                dir,
                expr: default_expr,
                malformed_literal: None,
                span: cl_sp,
            });
        }
        let tok_sp = self.current_span();
        let malformed_literal = match self.peek().kind {
            TokenKind::IntegerLit(sym) if self.sym_str(sym).starts_with(['+', '-']) => Some(tok_sp),
            TokenKind::DecimalLit { .. } => Some(tok_sp),
            _ => None,
        };
        let expr = self.parse_screen_bounded_expr()?;
        let end_sp = expr.span();
        Ok(ScreenCoordAst {
            dir,
            expr: Some(expr),
            malformed_literal,
            span: cl_sp.merge(end_sp),
        })
    }

    /// Parses a color value (`BLACK`..`WHITE` named constant or numeric expression).
    fn parse_screen_color_expr(&mut self) -> Result<Expr> {
        self.skip_separators();
        if let Some(w) = self.current_word()
            && !self.declared_data_names.contains(
                &self
                    .peek_word_sym()
                    .unwrap_or_else(|| SymbolId::new(u32::MAX)),
            )
            && let Some(code_str) = named_screen_color_digit(w)
        {
            let sp = self.current_span();
            self.advance();
            let raw = self.interner.intern(code_str)?;
            return Ok(Expr::Literal {
                atom: ValueAtom::Integer(raw),
                span: sp,
            });
        }
        self.parse_screen_bounded_expr()
    }

    /// Parses a scalar operand inside a screen clause without greedily consuming binary `+`/`-` across clauses.
    fn parse_screen_bounded_expr(&mut self) -> Result<Expr> {
        if self.peek().kind == TokenKind::LParen {
            return self.parse_expr();
        }
        self.parse_prefix_expr_for_screen()
    }

    /// Parses a single primary/atom expression (`IntLit`, `StringLit`, `Figurative`, `FunctionCall`, or `DataRef`).
    fn parse_prefix_expr_for_screen(&mut self) -> Result<Expr> {
        self.in_fn_arg_list = self.in_fn_arg_list.saturating_add(1);
        let res = self.parse_prefix_expr();
        self.in_fn_arg_list = self.in_fn_arg_list.saturating_sub(1);
        res
    }

    /// Checks whether `(` starts an MS-COBOL `( [line] , [col] )` position spec and parses it.
    fn try_parse_ms_position_clause(&mut self) -> Result<Option<ScreenClauseAst>> {
        self.skip_separators();
        if self.peek().kind != TokenKind::LParen || !self.lparen_has_top_level_comma() {
            return Ok(None);
        }
        let start = self.current_span();
        self.enter_paren();
        self.in_fn_arg_list = self.in_fn_arg_list.saturating_add(1);
        let line = if self
            .tokens
            .get(self.pos)
            .is_some_and(|t| t.kind == TokenKind::Comma)
        {
            self.pos += 1;
            None
        } else {
            let e = match self.parse_expr() {
                Ok(v) => v,
                Err(err) => {
                    self.in_fn_arg_list = self.in_fn_arg_list.saturating_sub(1);
                    self.paren_depth = self.paren_depth.saturating_sub(1);
                    return Err(err);
                }
            };
            if self
                .tokens
                .get(self.pos)
                .is_some_and(|t| t.kind == TokenKind::Comma)
            {
                self.pos += 1;
            }
            Some(e)
        };
        let col = if self.tokens.get(self.pos).is_some_and(|t| {
            !matches!(
                t.kind,
                TokenKind::RParen | TokenKind::Period | TokenKind::Eof
            )
        }) {
            match self.parse_expr() {
                Ok(v) => Some(v),
                Err(err) => {
                    self.in_fn_arg_list = self.in_fn_arg_list.saturating_sub(1);
                    self.paren_depth = self.paren_depth.saturating_sub(1);
                    return Err(err);
                }
            }
        } else {
            None
        };
        self.in_fn_arg_list = self.in_fn_arg_list.saturating_sub(1);
        let end = self.exit_paren();
        Ok(Some(ScreenClauseAst::MsPosition {
            line,
            col,
            span: start.merge(end),
        }))
    }

    /// Scans ahead from `self.pos` (`TokenKind::LParen`) to check for a top-level `,` before `)`.
    #[must_use]
    fn lparen_has_top_level_comma(&self) -> bool {
        let mut idx = self.pos.saturating_add(1);
        let mut depth = 1_u32;
        while let Some(tok) = self.tokens.get(idx) {
            match tok.kind {
                TokenKind::LParen => depth = depth.saturating_add(1),
                TokenKind::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return false;
                    }
                }
                TokenKind::Comma if depth == 1 => return true,
                TokenKind::Period | TokenKind::Eof => return false,
                _ => {}
            }
            idx += 1;
        }
        false
    }

    /// Parses `DISPLAY` supporting both stream I/O and extended M19 screen items/clauses.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening `DISPLAY` verb span.
    pub(crate) fn parse_display_stmt_m19(&mut self, start: SourceSpan) -> Result<Stmt> {
        let mut operands = Vec::new();
        let mut acc = DisplayParseAccum {
            items: Vec::new(),
            upon: None,
            trailing_upon: None,
            advancing: DisplayAdvancing::AdvanceLine,
            no_advancing_span: None,
            has_any_screen_syntax: false,
        };
        let mut leading_clauses: Vec<ScreenClauseAst> = Vec::new();

        while !self.at_clause_boundary(&["END-DISPLAY"])
            && !self.at_event_offset(0, &["EXCEPTION"])
            && !self.at_not_event_offset(&["EXCEPTION"])
        {
            self.tick()?;
            if self.try_parse_display_upon_or_advancing(&mut acc) {
                continue;
            }
            if let Some(ms_pos) = self.try_parse_ms_position_clause()? {
                acc.has_any_screen_syntax = true;
                leading_clauses.push(ms_pos);
                continue;
            }
            let cl_sp = self.current_span();
            if let Some(sc) = self.try_parse_common_screen_attr_clause(cl_sp, false)? {
                acc.has_any_screen_syntax = true;
                if let Some(last) = acc.items.last_mut() {
                    last.clauses.push(sc);
                } else {
                    leading_clauses.push(sc);
                }
                continue;
            }
            let item_sp = self.current_span();
            let expr = self.parse_display_operand_expr()?;
            operands.push(expr.clone());
            let mut item_clauses = std::mem::take(&mut leading_clauses);
            self.parse_display_item_trailing_clauses(
                &mut item_clauses,
                &mut leading_clauses,
                &mut acc.has_any_screen_syntax,
            )?;
            acc.items.push(ScreenDisplayItemAst {
                operand: Some(expr),
                clauses: item_clauses,
                upon: None,
                span: item_sp,
            });
        }

        if !leading_clauses.is_empty() {
            let sp = leading_clauses.first().map_or(start, ScreenClauseAst::span);
            acc.items.push(ScreenDisplayItemAst {
                operand: None,
                clauses: leading_clauses,
                upon: None,
                span: sp,
            });
        }

        let handlers = self.parse_generic_event_handlers(&["EXCEPTION"], "END-DISPLAY")?;
        let end_span = self.current_span();
        let _ = self.consume_word("END-DISPLAY");
        if acc.advancing == DisplayAdvancing::NoAdvancing && acc.no_advancing_span.is_none() {
            acc.no_advancing_span = Some(end_span);
        }
        let stmt_span = start.merge(end_span);
        let screen_ext = if acc.has_any_screen_syntax
            || acc.advancing == DisplayAdvancing::NoAdvancing
            || acc.items.len() > 1
            || acc.upon.is_some()
        {
            Some(Box::new(ScreenDisplayStmtAst {
                items: acc.items,
                trailing_upon: acc.trailing_upon,
                advancing: acc.advancing,
                no_advancing_span: acc.no_advancing_span,
                handlers: handlers.clone(),
                span: stmt_span,
            }))
        } else {
            None
        };
        Ok(Stmt::Display {
            operands,
            upon: acc.upon,
            advancing: acc.advancing,
            screen_ext,
            handlers,
            span: stmt_span,
        })
    }

    /// Consumes `UPON <device>` or `[WITH] [NO] ADVANCING` inside `DISPLAY`, returning `true` if matched.
    ///
    /// # Arguments
    ///
    /// * `acc` - Mutable `DISPLAY` statement parse accumulator.
    fn try_parse_display_upon_or_advancing(&mut self, acc: &mut DisplayParseAccum) -> bool {
        if self.consume_word("UPON") {
            if let Some((dev, dev_sp, _)) = self.eat_word() {
                let dev_s = self.sym_str(dev);
                if matches!(dev_s, "CRT" | "CRT-UNDER") {
                    acc.has_any_screen_syntax = true;
                }
                acc.upon = Some(dev);
                if !self.at_clause_boundary(&["END-DISPLAY"])
                    && !self.at_word("WITH")
                    && !self.at_word("NO")
                    && !self.at_event_offset(0, &["EXCEPTION"])
                    && !self.at_not_event_offset(&["EXCEPTION"])
                    && let Some(last) = acc.items.last_mut()
                    && last.upon.is_none()
                {
                    last.upon = Some((dev, dev_sp));
                } else {
                    acc.trailing_upon = Some((dev, dev_sp));
                }
            }
            return true;
        }
        if self.at_word("NO") && self.peek_word_at(1, "ADVANCING") {
            self.advance();
            self.advance();
            acc.advancing = DisplayAdvancing::NoAdvancing;
            acc.no_advancing_span = Some(self.current_span());
            return true;
        }
        if self.at_word("WITH") && (self.peek_word_at(1, "NO") || self.peek_word_at(1, "ADVANCING"))
        {
            self.advance();
            if self.consume_word("NO") {
                let _ = self.consume_word("ADVANCING");
                acc.advancing = DisplayAdvancing::NoAdvancing;
                acc.no_advancing_span = Some(self.current_span());
            } else {
                let _ = self.consume_word("ADVANCING");
            }
            return true;
        }
        false
    }

    /// Parses a single `DISPLAY` operand without letting Pratt `+`/`-` eat `LINE + 1` or `,`.
    fn parse_display_operand_expr(&mut self) -> Result<Expr> {
        self.in_fn_arg_list = self.in_fn_arg_list.saturating_add(1);
        let res = self.parse_expr();
        self.in_fn_arg_list = self.in_fn_arg_list.saturating_sub(1);
        res
    }

    /// Parses trailing screen clauses belonging to a single `DISPLAY` operand.
    ///
    /// # Arguments
    ///
    /// * `item_clauses` - Destination vector for operand-scoped screen clauses.
    /// * `leading_clauses` - Destination vector for next-item leading position clauses.
    /// * `has_any_screen_syntax` - Set to `true` when any M19 screen clause is parsed.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if subordinate expression parsing fails.
    fn parse_display_item_trailing_clauses(
        &mut self,
        item_clauses: &mut Vec<ScreenClauseAst>,
        leading_clauses: &mut Vec<ScreenClauseAst>,
        has_any_screen_syntax: &mut bool,
    ) -> Result<()> {
        while !self.at_clause_boundary(&["END-DISPLAY"])
            && !self.at_event_offset(0, &["EXCEPTION"])
            && !self.at_not_event_offset(&["EXCEPTION"])
        {
            if self.at_word("UPON")
                || (self.at_word("NO") && self.peek_word_at(1, "ADVANCING"))
                || (self.at_word("WITH")
                    && (self.peek_word_at(1, "NO") || self.peek_word_at(1, "ADVANCING")))
            {
                break;
            }
            if let Some(ms_pos) = self.try_parse_ms_position_clause()? {
                *has_any_screen_syntax = true;
                leading_clauses.push(ms_pos);
                break;
            }
            let sub_sp = self.current_span();
            if let Some(sc) = self.try_parse_common_screen_attr_clause(sub_sp, false)? {
                *has_any_screen_syntax = true;
                item_clauses.push(sc);
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Parses `ACCEPT` supporting system/console registers, `ACCEPT OMITTED`, and M19 screen clauses.
    ///
    /// # Arguments
    ///
    /// * `start` - Opening `ACCEPT` verb span.
    pub(crate) fn parse_accept_stmt_m19(&mut self, start: SourceSpan) -> Result<Stmt> {
        let mut clauses = Vec::new();
        while let Some(ms_pos) = self.try_parse_ms_position_clause()? {
            clauses.push(ms_pos);
        }
        if self.consume_word("OMITTED") {
            let mut from_crt = false;
            self.parse_accept_screen_tail(&mut clauses, &mut from_crt)?;
            let handlers =
                self.parse_generic_event_handlers(&["EXCEPTION", "ESCAPE"], "END-ACCEPT")?;
            let _ = self.consume_word("END-ACCEPT");
            let span = start.merge(self.current_span());
            return Ok(Stmt::AcceptOmitted(Box::new(ScreenAcceptStmtAst {
                target: ScreenAcceptTargetAst::Omitted(start),
                clauses,
                from_crt,
                handlers,
                span,
            })));
        }

        let target = self.parse_data_ref()?;
        if clauses.is_empty() && self.consume_word_pair("MESSAGE", "COUNT") {
            return Ok(Stmt::Continue {
                after_seconds: None,
                span: start.merge(self.current_span()),
            });
        }
        let mut source = AcceptSource::InputDevice(None);
        let mut from_crt = false;
        let mut is_non_screen_from = false;

        if clauses.is_empty() && self.consume_word("FROM") {
            self.parse_accept_from_clause(&mut source, &mut from_crt, &mut is_non_screen_from)?;
        }

        if !is_non_screen_from {
            self.parse_accept_screen_tail(&mut clauses, &mut from_crt)?;
            if from_crt {
                source = AcceptSource::Screen;
            }
        }

        let handlers = self.parse_generic_event_handlers(&["EXCEPTION", "ESCAPE"], "END-ACCEPT")?;
        let _ = self.consume_word("END-ACCEPT");
        let span = start.merge(self.current_span());
        let screen_ext = if !clauses.is_empty() || from_crt {
            Some(Box::new(ScreenAcceptStmtAst {
                target: ScreenAcceptTargetAst::Item(target.clone()),
                clauses,
                from_crt,
                handlers: handlers.clone(),
                span,
            }))
        } else {
            None
        };
        Ok(Stmt::Accept {
            target,
            source,
            screen_ext,
            handlers,
            span,
        })
    }

    /// Parses `FROM <register | device | CRT>` on an `ACCEPT` statement.
    ///
    /// # Arguments
    ///
    /// * `source` - Output [`AcceptSource`] slot.
    /// * `from_crt` - Set to `true` when `FROM CRT` is specified.
    /// * `is_non_screen_from` - Set to `true` for date/time/environment/console `FROM` sources.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if date/time qualifier parsing fails.
    fn parse_accept_from_clause(
        &mut self,
        source: &mut AcceptSource,
        from_crt: &mut bool,
        is_non_screen_from: &mut bool,
    ) -> Result<()> {
        if self.consume_word_pair("ESCAPE", "KEY") {
            if let Ok(sym) = self.interner.intern("ESCAPE-KEY") {
                *source = AcceptSource::ProcessInfo(sym);
                *is_non_screen_from = true;
            }
        } else if self.consume_word_pair("EXCEPTION", "STATUS") {
            if let Ok(sym) = self.interner.intern("EXCEPTION-STATUS") {
                *source = AcceptSource::ProcessInfo(sym);
                *is_non_screen_from = true;
            }
        } else if self.consume_word_pair("USER", "NAME") {
            if let Ok(sym) = self.interner.intern("USER-NAME") {
                *source = AcceptSource::ProcessInfo(sym);
                *is_non_screen_from = true;
            }
        } else if let Some((src_sym, _, _)) = self.eat_word() {
            let s = self.sym_str(src_sym).to_owned();
            if matches!(
                s.as_str(),
                "DATE" | "DAY" | "DAY-OF-WEEK" | "TIME" | "MICROSECOND-TIME"
            ) {
                let final_sym = if self.at_word("YYYYMMDD") || self.at_word("YYYYDDD") {
                    let fmt_sym = self.eat_word().map_or(src_sym, |t| t.0);
                    let combined = format!("{s} {}", self.sym_str(fmt_sym));
                    self.interner.intern(&combined)?
                } else {
                    src_sym
                };
                *source = AcceptSource::SystemClock(final_sym);
                *is_non_screen_from = true;
            } else if matches!(
                s.as_str(),
                "ARGUMENT-NUMBER"
                    | "ARGUMENT-VALUE"
                    | "COMMAND-LINE"
                    | "ENVIRONMENT-NAME"
                    | "ENVIRONMENT-VALUE"
                    | "LINES"
                    | "COLUMNS"
            ) {
                *source = AcceptSource::ProcessInfo(src_sym);
                *is_non_screen_from = true;
            } else if s == "CRT" {
                *source = AcceptSource::Screen;
                *from_crt = true;
            } else {
                *source = AcceptSource::InputDevice(Some(src_sym));
                *is_non_screen_from = true;
            }
        }
        Ok(())
    }

    /// Parses trailing screen clauses on `ACCEPT` up to `ON EXCEPTION` / `END-ACCEPT` / next verb.
    ///
    /// # Arguments
    ///
    /// * `clauses` - Clause accumulator.
    /// * `from_crt` - Set to `true` when `FROM CRT` is encountered.
    fn parse_accept_screen_tail(
        &mut self,
        clauses: &mut Vec<ScreenClauseAst>,
        from_crt: &mut bool,
    ) -> Result<()> {
        while !self.at_clause_boundary(&["END-ACCEPT"])
            && !self.at_event_offset(0, &["EXCEPTION", "ESCAPE"])
            && !self.at_not_event_offset(&["EXCEPTION", "ESCAPE"])
        {
            self.tick()?;
            if self.consume_word_pair("FROM", "CRT") {
                *from_crt = true;
                continue;
            }
            if let Some(ms_pos) = self.try_parse_ms_position_clause()? {
                clauses.push(ms_pos);
                continue;
            }
            let cl_sp = self.current_span();
            if let Some(sc) = self.try_parse_common_screen_attr_clause(cl_sp, false)? {
                if matches!(sc, ScreenClauseAst::Timeout { .. }) {
                    let _ = self.consume_word("KEY");
                }
                clauses.push(sc);
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Parses `ATTRIBUTE <attr> [IS | TO] {ON | OFF} ...` inside `SET <screen-item> ATTRIBUTE ...`.
    ///
    /// # Arguments
    ///
    /// * `target` - Screen item `DataRef` preceding `ATTRIBUTE`.
    pub(crate) fn try_parse_set_screen_attribute(&mut self, target: DataRef) -> Option<SetAction> {
        if !self.consume_any_word(&["ATTRIBUTE", "ATTRIBUTES"]) {
            return None;
        }
        let mut toggles = Vec::new();
        while !self.at_clause_boundary(&[]) {
            let Some((attr_name, a_sp, _)) = self.eat_word() else {
                break;
            };
            let _ = self.consume_any_word(&["IS", "TO"]);
            let enabled = if self.consume_any_word(&["ON", "TRUE"]) {
                true
            } else {
                !self.consume_any_word(&["OFF", "FALSE"])
            };
            toggles.push(SetScreenAttrToggleAst {
                attr_name,
                enabled,
                span: a_sp,
            });
        }
        Some(SetAction::ScreenAttributes { target, toggles })
    }

    // ========================================================================
    // Shared Lexeme & Boundary Helpers
    // ========================================================================

    /// Returns `true` when `-fregister=COL` is active, treating `COL` as a register rather than `COLUMN`.
    #[must_use]
    pub(crate) fn col_is_custom_register(&self) -> bool {
        self.dialect.custom_registers.contains("COL")
    }

    /// Consumes an unsigned integer token and returns `(value, span)`.
    pub(crate) fn eat_u32_with_span(&mut self) -> Option<(u32, SourceSpan)> {
        self.skip_separators();
        let tok = self.tokens.get(self.pos)?;
        if let TokenKind::IntegerLit(sym) = tok.kind {
            let v = self.sym_str(sym).parse::<u32>().ok()?;
            let sp = tok.span;
            self.advance();
            Some((v, sp))
        } else {
            None
        }
    }

    /// Consumes an unsigned or `+`-prefixed integer magnitude.
    fn eat_signed_or_u32_abs(&mut self) -> Option<u32> {
        self.skip_separators();
        let tok = self.tokens.get(self.pos)?;
        if let TokenKind::IntegerLit(sym) = tok.kind {
            let s = self.sym_str(sym).trim_start_matches('+');
            let v = s.parse::<u32>().ok()?;
            self.advance();
            Some(v)
        } else {
            None
        }
    }

    /// Consumes a signed or unsigned `i64` integer literal.
    fn eat_i64_lit(&mut self) -> Option<i64> {
        self.skip_separators();
        let sign = if self.peek().kind == TokenKind::Minus {
            self.advance();
            -1_i64
        } else {
            if self.peek().kind == TokenKind::Plus {
                self.advance();
            }
            1_i64
        };
        let tok = self.tokens.get(self.pos)?;
        if let TokenKind::IntegerLit(sym) = tok.kind {
            let v = self.sym_str(sym).parse::<i64>().ok()?;
            self.advance();
            Some(sign.saturating_mul(v))
        } else {
            None
        }
    }

    /// Peeks the current `Word` symbol without advancing.
    #[must_use]
    fn peek_word_sym(&self) -> Option<SymbolId> {
        if let TokenKind::Word { sym, .. } = self.peek().kind {
            Some(sym)
        } else {
            None
        }
    }

    /// Returns `true` when at `NOT [ON] <event_kw>`.
    ///
    /// # Arguments
    ///
    /// * `event_kws` - Event keywords (`EXCEPTION`, `ESCAPE`, etc.) to match after `NOT [ON]`.
    #[must_use]
    fn at_not_event_offset(&self, event_kws: &[&str]) -> bool {
        self.at_word("NOT") && self.at_event_offset(1, event_kws)
    }

    /// Returns `true` at statement/handler boundaries in extended `DISPLAY`/`ACCEPT`.
    #[must_use]
    fn at_screen_io_boundary(&self) -> bool {
        self.at_clause_boundary(&["END-DISPLAY", "END-ACCEPT"])
            || self.at_event_offset(0, &["EXCEPTION", "ESCAPE"])
            || self.at_not_event_offset(&["EXCEPTION", "ESCAPE"])
            || self.at_word("UPON")
    }

    /// Returns `true` when a bare `LINE` or `COLUMN` clause in `DISPLAY`/`ACCEPT` has no coordinate expression.
    #[must_use]
    fn at_screen_io_coord_end(&self) -> bool {
        self.at_screen_io_boundary()
            || matches!(
                self.current_word(),
                Some(
                    "LINE"
                        | "COLUMN"
                        | "AT"
                        | "WITH"
                        | "HIGHLIGHT"
                        | "LOWLIGHT"
                        | "BLINK"
                        | "REVERSE-VIDEO"
                        | "REVERSED"
                        | "UNDERLINE"
                        | "OVERLINE"
                        | "LEFTLINE"
                        | "RIGHTLINE"
                        | "GRID"
                        | "BELL"
                        | "BEEP"
                        | "BLANK"
                        | "ERASE"
                        | "AUTO"
                        | "AUTO-SKIP"
                        | "AUTOTERMINATE"
                        | "SECURE"
                        | "NO-ECHO"
                        | "REQUIRED"
                        | "EMPTY-CHECK"
                        | "FULL"
                        | "LENGTH-CHECK"
                        | "UPDATE"
                        | "DEFAULT"
                        | "PROMPT"
                        | "SIZE"
                        | "CURSOR"
                        | "MODE"
                        | "SCROLL"
                        | "TIME-OUT"
                        | "TIMEOUT"
                        | "CONVERSION"
                        | "FOREGROUND-COLOR"
                        | "FOREGROUND-COLOUR"
                        | "BACKGROUND-COLOR"
                        | "BACKGROUND-COLOUR"
                        | "COLOR"
                        | "COLOUR"
                        | "CONTROL"
                        | "NO"
                )
            )
            || (!self.col_is_custom_register()
                && matches!(self.current_word(), Some("COL" | "POS")))
    }
}

/// Returns `true` if `w` starts an `RD`-level clause.
///
/// # Arguments
///
/// * `w` - Optional uppercase word at the current token position.
#[must_use]
fn is_rd_clause_starter(w: Option<&str>) -> bool {
    matches!(
        w,
        Some(
            "GLOBAL"
                | "CODE"
                | "CONTROL"
                | "CONTROLS"
                | "PAGE"
                | "LINE"
                | "COL"
                | "COLS"
                | "COLUMN"
                | "COLUMNS"
                | "HEADING"
                | "FIRST"
                | "LAST"
                | "FOOTING"
                | "IS"
                | "ARE"
        )
    )
}

/// Returns `true` if `w` starts a `01`..`49` `REPORT SECTION` clause rather than an item name.
///
/// # Arguments
///
/// * `w` - Optional uppercase word at the current token position.
#[must_use]
fn is_report_clause_starter(w: Option<&str>) -> bool {
    matches!(
        w,
        Some(
            "TYPE"
                | "LINE"
                | "LINES"
                | "NEXT"
                | "COLUMN"
                | "COL"
                | "COLUMNS"
                | "COLS"
                | "LEFT"
                | "CENTER"
                | "CENTRE"
                | "RIGHT"
                | "PIC"
                | "PICTURE"
                | "SOURCE"
                | "SUM"
                | "VALUE"
                | "VALUES"
                | "GROUP"
                | "PRESENT"
                | "ABSENT"
                | "NOT"
                | "USAGE"
                | "SIGN"
                | "LEADING"
                | "TRAILING"
                | "JUSTIFIED"
                | "JUST"
                | "BLANK"
                | "OCCURS"
                | "STEP"
                | "VARYING"
                | "DISPLAY"
                | "NATIONAL"
                | "UTF-8"
                | "COMP"
                | "COMP-3"
                | "COMP-4"
                | "COMP-5"
                | "BINARY"
                | "PACKED-DECIMAL"
        )
    )
}

/// Mutable accumulator for `parse_display_stmt_m19`.
struct DisplayParseAccum {
    /// Staged `DISPLAY` operand items.
    items: Vec<ScreenDisplayItemAst>,
    /// Last `UPON <device>` target.
    upon: Option<SymbolId>,
    /// Trailing statement-level `UPON` target with span.
    trailing_upon: Option<(SymbolId, SourceSpan)>,
    /// Line advancing mode.
    advancing: DisplayAdvancing,
    /// Span of `NO ADVANCING`.
    no_advancing_span: Option<SourceSpan>,
    /// Whether any M19 screen clause or `UPON CRT` appeared.
    has_any_screen_syntax: bool,
}

/// Returns `true` if `w` (or implicit string literal `tok`) starts a `SCREEN SECTION` clause.
///
/// # Arguments
///
/// * `w` - Optional uppercase word at the current token position.
/// * `tok` - Current [`crate::token::Token`] for string-literal detection.
#[must_use]
fn is_screen_clause_starter(w: Option<&str>, tok: &crate::token::Token) -> bool {
    if matches!(tok.kind, TokenKind::StringLit { .. }) || is_bare_usage_keyword(w) {
        return true;
    }
    matches!(
        w,
        Some(
            "PIC"
                | "PICTURE"
                | "LINE"
                | "COLUMN"
                | "COL"
                | "POS"
                | "AT"
                | "VALUE"
                | "VALUES"
                | "FROM"
                | "TO"
                | "USING"
                | "BLANK"
                | "ERASE"
                | "BELL"
                | "BEEP"
                | "HIGHLIGHT"
                | "LOWLIGHT"
                | "BLINK"
                | "REVERSE-VIDEO"
                | "REVERSED"
                | "REVERSE"
                | "UNDERLINE"
                | "OVERLINE"
                | "LEFTLINE"
                | "RIGHTLINE"
                | "GRID"
                | "AUTO"
                | "AUTO-SKIP"
                | "AUTOTERMINATE"
                | "TAB"
                | "SECURE"
                | "NO-ECHO"
                | "OFF"
                | "REQUIRED"
                | "EMPTY-CHECK"
                | "FULL"
                | "LENGTH-CHECK"
                | "PROMPT"
                | "UPDATE"
                | "NO"
                | "DEFAULT"
                | "ZERO-FILL"
                | "INITIAL"
                | "SIZE"
                | "FOREGROUND-COLOR"
                | "FOREGROUND-COLOUR"
                | "BACKGROUND-COLOR"
                | "BACKGROUND-COLOUR"
                | "COLOR"
                | "COLOUR"
                | "CONTROL"
                | "SCROLL"
                | "TIME-OUT"
                | "TIMEOUT"
                | "MODE"
                | "CONVERSION"
                | "CURSOR"
                | "SIGN"
                | "LEADING"
                | "TRAILING"
                | "JUSTIFIED"
                | "JUST"
                | "OCCURS"
                | "USAGE"
                | "WITH"
        )
    )
}

/// Returns `true` if a reserved word is a valid expression prefix/register (`FUNCTION`, `LENGTH`, etc.).
///
/// # Arguments
///
/// * `w` - Uppercase token text to classify.
#[must_use]
fn is_valid_reserved_expr_starter(w: &str) -> bool {
    matches!(
        w,
        "FUNCTION"
            | "LENGTH"
            | "BYTE-LENGTH"
            | "ADDRESS"
            | "RETURN-CODE"
            | "TALLY"
            | "SORT-RETURN"
            | "LINAGE-COUNTER"
            | "LINE-COUNTER"
            | "PAGE-COUNTER"
            | "NUMBER-OF-CALL-PARAMETERS"
            | "COB-CRT-STATUS"
            | "XML-CODE"
            | "XML-EVENT"
            | "XML-TEXT"
            | "XML-NTEXT"
            | "XML-INFORMATION"
            | "JSON-CODE"
            | "JSON-STATUS"
    )
}

/// Maps standard COBOL color keywords (`BLACK`..`WHITE`) to `"0"`..`"7"`.
///
/// # Arguments
///
/// * `w` - Uppercase color keyword.
#[must_use]
fn named_screen_color_digit(w: &str) -> Option<&'static str> {
    match w {
        "BLACK" => Some("0"),
        "BLUE" => Some("1"),
        "GREEN" => Some("2"),
        "CYAN" => Some("3"),
        "RED" => Some("4"),
        "MAGENTA" => Some("5"),
        "BROWN" | "YELLOW" => Some("6"),
        "WHITE" => Some("7"),
        _ => None,
    }
}
