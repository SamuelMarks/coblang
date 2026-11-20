//! `DATA DIVISION` section, `FD`/`SD`/`RD`, `SCREEN`, and arbitrary-order `01`..`88` entry parser.

use coblang_common::{LevelNumber, Result, SourceSpan, SymbolId};
use coblang_diag::{Diagnostic, DiagnosticCode};

use crate::ast::{
    BlankWhenZeroMode, DataClauseCounts, DataDivision, DataEntry, DataEntryName, ExternalLinkage,
    FdOrSd, FdSdEntry, FigurativeValue, GlobalVisibility, JustificationMode, KeyOrder,
    LengthVariability, LinageSpec, OccursClause, OccursKeySpec, OccursUpperBound, RdEntry,
    RecordSizeClause, RenamesRange, ResidencyKind, ScreenEntry, SignClause, SignPlacement,
    SignSeparation, StorageLinkageFlags, SyncAlignment, TypeRole, UsageKind, ValueAtom,
    ValueClause, ValueRange,
};
use crate::parser::Parser;
use crate::token::TokenKind;

impl Parser<'_> {
    /// Parses optional `DATA DIVISION.` across all six COBOL data sections.
    pub(crate) fn parse_optional_data_division(&mut self) -> Result<DataDivision> {
        if !self.at_word_pair("DATA", "DIVISION") {
            return Ok(DataDivision::default());
        }
        let start = self.current_span();
        self.advance();
        self.advance();
        let _ = self.consume_period();

        let mut data = DataDivision {
            span: Some(start),
            ..DataDivision::default()
        };

        while !self.at_eof() && !self.starts_after_data_division() {
            if self.consume_word_pair("FILE", "SECTION") {
                let _ = self.consume_period();
                self.parse_file_section(&mut data.file_section)?;
            } else if self.consume_word_pair("WORKING-STORAGE", "SECTION") {
                let _ = self.consume_period();
                self.parse_storage_entries(&mut data.working_storage)?;
            } else if self.consume_word_pair("LOCAL-STORAGE", "SECTION") {
                let _ = self.consume_period();
                self.parse_storage_entries(&mut data.local_storage)?;
            } else if self.consume_word_pair("LINKAGE", "SECTION") {
                let _ = self.consume_period();
                self.parse_storage_entries(&mut data.linkage)?;
            } else if self.consume_word_pair("REPORT", "SECTION") {
                let _ = self.consume_period();
                self.parse_report_section(&mut data.report_section)?;
            } else if self.consume_word_pair("SCREEN", "SECTION") {
                let _ = self.consume_period();
                self.parse_screen_section(&mut data.screen_section)?;
            } else if self.consume_word_pair("COMMUNICATION", "SECTION") {
                let _ = self.consume_period();
                self.parse_communication_section(&mut data.working_storage)?;
            } else if self.peek_level_number().is_some() {
                // Relaxed COBOL allows unnamed top-level data items directly inside DATA DIVISION.
                self.parse_storage_entries(&mut data.working_storage)?;
            } else {
                self.emit_syntax_error(self.current_span(), "unexpected token in DATA DIVISION");
                self.consume_to_period();
            }
        }

        data.span = Some(start.merge(self.current_span()));
        Ok(data)
    }

    /// Parses `FD` and `SD` entries inside `FILE SECTION.`.
    ///
    /// # Arguments
    ///
    /// * `out` - Output `FD`/`SD` vector.
    fn parse_file_section(&mut self, out: &mut Vec<FdSdEntry>) -> Result<()> {
        while !self.at_eof()
            && !self.starts_after_data_division()
            && !self.starts_other_data_section()
        {
            let kind = if self.consume_word("FD") {
                FdOrSd::Fd
            } else if self.consume_word("SD") {
                FdOrSd::Sd
            } else {
                self.emit_syntax_error(
                    self.current_span(),
                    "expected `FD` or `SD` in FILE SECTION",
                );
                self.consume_to_period();
                continue;
            };
            out.push(self.parse_single_fd_sd_entry(kind)?);
        }
        Ok(())
    }

    /// Parses a single `FD` or `SD` entry and its subordinate record hierarchy.
    ///
    /// # Arguments
    ///
    /// * `kind` - Whether the entry header opened with `FD` or `SD`.
    fn parse_single_fd_sd_entry(&mut self, kind: FdOrSd) -> Result<FdSdEntry> {
        let start = self.current_span();
        let file_name = if let Some((sym, _, _)) = self.eat_word() {
            sym
        } else {
            self.emit_syntax_error(start, "expected file-name after `FD`/`SD`");
            self.interner.intern("__MISSING_FD")?
        };

        let mut external = ExternalLinkage::Internal;
        let mut global = GlobalVisibility::Local;
        let mut record_size = None;
        let mut linage = None;
        let mut reports = Vec::new();
        let mut report_spans = Vec::new();
        let mut label_records_span = None;
        let mut data_records_span = None;
        let mut record_clause_span = None;
        let mut code_set = None;
        let mut emitted_eof_err = false;

        while !self.at_end_or_period() && !self.starts_after_data_division() {
            let _ = self.consume_word("IS");
            let cl_sp = self.current_span();
            if self.consume_word("EXTERNAL") {
                let alias = if self.consume_word("AS") {
                    self.eat_any_word_or_string_sym().map(|p| p.0)
                } else {
                    None
                };
                external = ExternalLinkage::External(alias);
            } else if self.consume_word("GLOBAL") {
                global = GlobalVisibility::Global;
            } else if self.consume_word("LABEL") {
                label_records_span = Some(cl_sp);
                let _ = self.consume_any_word(&["RECORD", "RECORDS"]);
                let _ = self.consume_any_word(&["IS", "ARE"]);
                while !self.at_end_or_period() && !is_fd_clause_keyword(self.current_word()) {
                    self.advance();
                }
            } else if self.consume_word("DATA") {
                data_records_span = Some(cl_sp);
                let _ = self.consume_any_word(&["RECORD", "RECORDS"]);
                let _ = self.consume_any_word(&["IS", "ARE"]);
                while !self.at_end_or_period() && !is_fd_clause_keyword(self.current_word()) {
                    self.advance();
                }
            } else if self.consume_word("BLOCK") {
                let _ = self.consume_word("CONTAINS");
                while !self.at_end_or_period() && !is_fd_clause_keyword(self.current_word()) {
                    self.advance();
                }
            } else if self.consume_word("RECORD") {
                if self.consume_word("DELIMITER") {
                    let _ = self.consume_word("IS");
                    let _ = self.eat_word();
                } else {
                    record_clause_span = Some(cl_sp);
                    record_size = Some(self.parse_record_size_clause());
                }
            } else if self.consume_word("LINAGE") {
                linage = Some(self.parse_linage_clause());
            } else if self.consume_any_word(&["REPORT", "REPORTS"]) {
                self.parse_fd_report_list(&mut reports, &mut report_spans, &mut emitted_eof_err);
            } else if self.consume_word("CODE-SET") {
                let _ = self.consume_word("IS");
                code_set = self.eat_word().map(|t| t.0);
            } else {
                self.advance();
            }
        }
        if !self.consume_period() && self.at_eof() && !emitted_eof_err {
            self.emit_syntax_error(self.current_span(), "expected `.` at end of `FD` entry");
        }
        let after_fd_span = self.current_span();
        let mut records = Vec::new();
        self.parse_storage_entries(&mut records)?;
        Ok(FdSdEntry {
            kind,
            file_name,
            external,
            global,
            record_size,
            linage,
            reports,
            report_spans,
            label_records_span,
            data_records_span,
            record_clause_span,
            after_fd_span,
            code_set,
            records,
            span: start,
        })
    }

    /// Parses `REPORT IS / REPORTS ARE r1 r2 ...` on an `FD` entry.
    ///
    /// # Arguments
    ///
    /// * `reports` - Output list of referenced report symbols.
    /// * `report_spans` - Output list of `(report_sym, span)` pairs.
    /// * `emitted_eof_err` - Flag set when a missing report-name error already reported EOF.
    fn parse_fd_report_list(
        &mut self,
        reports: &mut Vec<SymbolId>,
        report_spans: &mut Vec<(SymbolId, SourceSpan)>,
        emitted_eof_err: &mut bool,
    ) {
        let _ = self.consume_word("IS");
        let _ = self.consume_word("ARE");
        let mut saw_rep = false;
        while let Some((r, r_span, _)) = self.eat_word() {
            saw_rep = true;
            reports.push(r);
            report_spans.push((r, r_span));
            if is_fd_clause_keyword(self.current_word()) || self.at_period() {
                break;
            }
        }
        if !saw_rep {
            self.emit_syntax_error(self.current_span(), "expected report-name after `REPORT`");
            *emitted_eof_err = self.at_eof();
        }
    }

    /// Parses `RECORD CONTAINS ...` or `RECORD IS VARYING ...` on an `FD`/`SD`.
    fn parse_record_size_clause(&mut self) -> RecordSizeClause {
        let _ = self.consume_word("IS");
        if self.consume_word("VARYING") {
            let _ = self.consume_word("IN");
            let _ = self.consume_word("SIZE");
            let mut min_bytes = None;
            let mut max_bytes = None;
            let mut depending_on = None;
            if self.consume_word("FROM") {
                min_bytes = self.eat_u32_lit();
            } else if let Some(n) = self.eat_u32_lit() {
                min_bytes = Some(n);
            }
            if self.consume_word("TO") {
                max_bytes = self.eat_u32_lit();
            }
            let _ = self.consume_word("CHARACTERS");
            let _ = self.consume_word("BYTES");
            if self.consume_word("DEPENDING") {
                let _ = self.consume_word("ON");
                depending_on = Some(self.parse_simple_qualified_syms());
            }
            RecordSizeClause::Varying {
                min_bytes,
                max_bytes,
                depending_on,
            }
        } else {
            let _ = self.consume_word("CONTAINS");
            let min_bytes = self.eat_u32_lit().unwrap_or(0);
            let max_bytes = if self.consume_word("TO") {
                self.eat_u32_lit()
            } else {
                None
            };
            let _ = self.consume_word("CHARACTERS");
            let _ = self.consume_word("BYTES");
            if self.consume_word("DEPENDING") {
                let _ = self.consume_word("ON");
                let dep = self.parse_simple_qualified_syms();
                RecordSizeClause::Varying {
                    min_bytes: Some(min_bytes),
                    max_bytes,
                    depending_on: Some(dep),
                }
            } else {
                RecordSizeClause::Contains {
                    min_bytes,
                    max_bytes,
                }
            }
        }
    }

    /// Parses `LINAGE IS n LINES [WITH FOOTING AT f] [LINES AT TOP t] [LINES AT BOTTOM b]`.
    fn parse_linage_clause(&mut self) -> LinageSpec {
        let span = self.current_span();
        let _ = self.consume_word("IS");
        let body_lines = self.parse_int_or_path();
        let _ = self.consume_word("LINES");
        let mut footing_at = None;
        let mut lines_at_top = None;
        let mut lines_at_bottom = None;
        while !self.at_end_or_period() && !is_fd_clause_keyword(self.current_word()) {
            let _ = self.consume_word("WITH");
            if self.consume_word("FOOTING") {
                let _ = self.consume_word("AT");
                footing_at = Some(self.parse_int_or_path());
            } else if self.consume_any_word(&["LINES", "AT"]) {
                let _ = self.consume_word("AT");
                if self.consume_word("TOP") {
                    lines_at_top = Some(self.parse_int_or_path());
                } else if self.consume_word("BOTTOM") {
                    lines_at_bottom = Some(self.parse_int_or_path());
                }
            } else {
                break;
            }
        }
        LinageSpec {
            body_lines,
            footing_at,
            lines_at_top,
            lines_at_bottom,
            span,
        }
    }

    /// Parses either a single `IntegerLit` symbol or a qualified identifier path.
    fn parse_int_or_path(&mut self) -> Vec<SymbolId> {
        if let TokenKind::IntegerLit(sym) = self.peek().kind {
            self.advance();
            vec![sym]
        } else {
            self.parse_simple_qualified_syms()
        }
    }

    /// Parses `01`..`88` [`DataEntry`] items until the next section, `FD`/`SD`, or division.
    ///
    /// # Arguments
    ///
    /// * `out` - Output [`DataEntry`] list.
    pub(crate) fn parse_storage_entries(&mut self, out: &mut Vec<DataEntry>) -> Result<()> {
        while !self.at_eof()
            && !self.starts_after_data_division()
            && !self.starts_other_data_section()
            && !matches!(self.current_word(), Some("FD" | "SD" | "RD"))
        {
            let Some((level, start_span)) = self.eat_level_number() else {
                self.emit_syntax_error(
                    self.current_span(),
                    "expected level-number (`01`..`49`, `66`, `77`, `78`, `88`) in DATA DIVISION",
                );
                self.consume_to_period();
                continue;
            };
            let entry = self.parse_single_data_entry(level, start_span)?;
            out.push(entry);
        }
        Ok(())
    }

    /// Parses legacy ANSI-85 `COMMUNICATION SECTION.` (`CD cd-name ...` + optional `01`..`88` records).
    ///
    /// # Arguments
    ///
    /// * `out` - Storage entry accumulator registering synthesized `CD` descriptors and record items.
    fn parse_communication_section(&mut self, out: &mut Vec<DataEntry>) -> Result<()> {
        let pic_sym = self.interner.intern("X(87)")?;
        while !self.at_eof()
            && !self.starts_after_data_division()
            && !self.starts_other_data_section()
        {
            if self.consume_word("CD") {
                let start_span = self.current_span();
                if let Some((cd_sym, cd_span, _)) = self.eat_word() {
                    let _ = self.declared_data_names.insert(cd_sym);
                    out.push(DataEntry {
                        level: LevelNumber::Hierarchy(1),
                        name: DataEntryName::Named(cd_sym),
                        redefines: None,
                        renames: None,
                        picture: Some((pic_sym, cd_span)),
                        usage: None,
                        sign: None,
                        occurs: None,
                        sync: SyncAlignment::Unsynchronized,
                        justified: JustificationMode::StandardLeft,
                        blank_when_zero: BlankWhenZeroMode::Normal,
                        value: None,
                        flags: StorageLinkageFlags::default(),
                        clause_counts: DataClauseCounts {
                            pic_count: 1,
                            ..DataClauseCounts::default()
                        },
                        span: start_span.merge(cd_span),
                    });
                }
                self.consume_to_period();
            } else if let Some((level, start_span)) = self.eat_level_number() {
                let entry = self.parse_single_data_entry(level, start_span)?;
                out.push(entry);
            } else {
                self.consume_to_period();
            }
        }
        Ok(())
    }

    /// Parses a single `DataEntry` whose level number `level` has just been consumed.
    ///
    /// # Arguments
    ///
    /// * `level` - Validated [`LevelNumber`].
    /// * `start_span` - Span of the level number token.
    fn parse_single_data_entry(
        &mut self,
        level: LevelNumber,
        start_span: SourceSpan,
    ) -> Result<DataEntry> {
        let name = if self.consume_word("FILLER") {
            DataEntryName::Filler
        } else if !self.at_period() && !is_data_clause_starter(self.current_word()) {
            if let Some((sym, _, _)) = self.eat_word() {
                let _ = self.declared_data_names.insert(sym);
                DataEntryName::Named(sym)
            } else {
                DataEntryName::Filler
            }
        } else {
            DataEntryName::Filler
        };

        let mut entry = DataEntry {
            level,
            name,
            redefines: None,
            renames: None,
            picture: None,
            usage: None,
            sign: None,
            occurs: None,
            sync: SyncAlignment::Unsynchronized,
            justified: JustificationMode::StandardLeft,
            blank_when_zero: BlankWhenZeroMode::Normal,
            value: None,
            flags: if level == LevelNumber::Constant78 {
                StorageLinkageFlags {
                    residency: ResidencyKind::CompileTimeConstant,
                    ..StorageLinkageFlags::default()
                }
            } else {
                StorageLinkageFlags::default()
            },
            clause_counts: DataClauseCounts::default(),
            span: start_span,
        };

        while !self.at_end_or_period() && !self.starts_after_data_division() {
            self.tick()?;
            self.parse_data_entry_clause(&mut entry)?;
        }

        let end_span = self.current_span();
        if !self.consume_period() {
            self.emit_syntax_error(end_span, "expected `.` at end of data description entry");
        }
        entry.span = start_span.merge(end_span);
        if entry.level == LevelNumber::Constant78
            && let DataEntryName::Named(sym) = entry.name
            && let Some(vc) = &entry.value
            && let Some(vr) = vc.values.first()
            && let ValueAtom::Integer(vsym) = vr.low
            && let Ok(v) = self.sym_str(vsym).parse::<u32>()
        {
            self.level_78_ints.insert(sym, v);
        }
        Ok(entry)
    }

    /// Parses one clause inside a `DataEntry` body.
    ///
    /// # Arguments
    ///
    /// * `entry` - Mutable [`DataEntry`] being populated.
    fn parse_data_entry_clause(&mut self, entry: &mut DataEntry) -> Result<()> {
        let _ = self.consume_word("IS");
        if self.at_end_or_period() || self.starts_after_data_division() {
            return Ok(());
        }
        if self.consume_word("REDEFINES") {
            entry.clause_counts.redefines_count =
                entry.clause_counts.redefines_count.saturating_add(1);
            entry.redefines = self.eat_word().map(|t| t.0);
        } else if self.consume_word("RENAMES") {
            let r_span = self.current_span();
            let from = self.parse_simple_qualified_syms();
            let thru = if self.consume_any_word(&["THRU", "THROUGH"]) {
                Some(self.parse_simple_qualified_syms())
            } else {
                None
            };
            entry.renames = Some(RenamesRange {
                from,
                thru,
                span: r_span,
            });
        } else if self.consume_any_word(&["PIC", "PICTURE"]) {
            let _ = self.consume_word("IS");
            entry.clause_counts.pic_count = entry.clause_counts.pic_count.saturating_add(1);
            if let TokenKind::PictureString(sym) = self.peek().kind {
                let pic_span = self.current_span();
                self.advance();
                entry.picture = Some((sym, pic_span));
            }
        } else if self.consume_word("USAGE") {
            let _ = self.consume_word("IS");
            entry.clause_counts.usage_count = entry.clause_counts.usage_count.saturating_add(1);
            entry.usage = self.parse_usage_kind();
        } else if is_bare_usage_keyword(self.current_word()) {
            entry.clause_counts.usage_count = entry.clause_counts.usage_count.saturating_add(1);
            entry.usage = self.parse_usage_kind();
        } else if self.consume_word("SIGN") {
            let _ = self.consume_word("IS");
            entry.sign = Some(self.parse_sign_clause());
        } else if matches!(self.current_word(), Some("LEADING" | "TRAILING")) {
            entry.sign = Some(self.parse_sign_clause());
        } else if self.consume_word("OCCURS") {
            entry.clause_counts.occurs_count = entry.clause_counts.occurs_count.saturating_add(1);
            entry.occurs = Some(self.parse_occurs_clause());
        } else if self.consume_any_word(&["SYNCHRONIZED", "SYNC"]) {
            entry.sync = if self.consume_word("LEFT") {
                SyncAlignment::SyncLeft
            } else if self.consume_word("RIGHT") {
                SyncAlignment::SyncRight
            } else {
                SyncAlignment::SyncDefault
            };
        } else if self.consume_any_word(&["JUSTIFIED", "JUST"]) {
            let _ = self.consume_word("RIGHT");
            entry.justified = JustificationMode::JustifiedRight;
        } else if self.consume_word("BLANK") {
            let _ = self.consume_word("WHEN");
            let _ = self.consume_word("ZERO");
            let _ = self.consume_word("ZEROS");
            let _ = self.consume_word("ZEROES");
            entry.blank_when_zero = BlankWhenZeroMode::BlankWhenZero;
        } else if self.consume_any_word(&["VALUE", "VALUES"]) {
            entry.clause_counts.value_count = entry.clause_counts.value_count.saturating_add(1);
            entry.value = Some(self.parse_value_clause()?);
        } else {
            self.parse_data_storage_flag_clause(entry)?;
        }
        Ok(())
    }

    /// Parses storage residency/visibility/typedef clauses (`CONSTANT`, `EXTERNAL`, `GLOBAL`, `BASED`, `ANY/DYNAMIC LENGTH`, `TYPEDEF`, `TYPE`, `SAME AS`).
    ///
    /// # Arguments
    ///
    /// * `entry` - Mutable [`DataEntry`] being populated.
    fn parse_data_storage_flag_clause(&mut self, entry: &mut DataEntry) -> Result<()> {
        if self.consume_word("CONSTANT") {
            entry.flags.residency = ResidencyKind::CompileTimeConstant;
            let _ = self.consume_word("IS");
            if self.consume_word("GLOBAL") {
                entry.flags.global = GlobalVisibility::Global;
            }
            let has_as = self.consume_any_word(&["AS", "FROM"]);
            if (has_as
                || matches!(
                    self.dialect.base_dialect,
                    coblang_common::Dialect::Gnu | coblang_common::Dialect::Default
                ))
                && !self.at_end_or_period()
            {
                entry.value = Some(self.parse_value_clause()?);
            }
        } else if self.consume_word("EXTERNAL") {
            let alias = if self.consume_word("AS") {
                self.eat_any_word_or_string_sym().map(|p| p.0)
            } else {
                None
            };
            entry.flags.external = ExternalLinkage::External(alias);
        } else if self.consume_word("GLOBAL") {
            entry.flags.global = GlobalVisibility::Global;
        } else if self.consume_word("BASED") {
            entry.flags.residency = ResidencyKind::Based;
        } else if self.consume_word_pair("ANY", "LENGTH")
            || self.consume_word_pair("ANY", "NUMERIC")
        {
            entry.flags.length_mode = LengthVariability::AnyLength;
        } else if self.consume_word("DYNAMIC") {
            let _ = self.consume_word("LENGTH");
            entry.flags.length_mode = LengthVariability::DynamicLength;
        } else if self.consume_word("TYPEDEF") {
            let _ = self.consume_word("STRONG");
            entry.flags.type_role = TypeRole::Typedef;
        } else if self.consume_word("TYPE") {
            let _ = self.consume_word("TO");
            if let Some((ty_sym, _, _)) = self.eat_word() {
                entry.flags.type_role = TypeRole::TypeInstance(ty_sym);
            }
        } else if self.consume_word_pair("SAME", "AS") {
            entry.flags.type_role = TypeRole::SameAs(self.parse_simple_qualified_syms());
        } else if let Some(sn_role) = self.try_parse_special_names_data_role() {
            entry.flags.special_names_role = Some(sn_role);
        } else {
            if entry.picture.is_none()
                && entry.usage.is_none()
                && entry.value.is_none()
                && entry.occurs.is_none()
                && entry.redefines.is_none()
                && let TokenKind::IntegerLit(sym) = self.peek().kind
                && self
                    .sym_str(sym)
                    .parse::<u8>()
                    .is_ok_and(|lv| (2..=49).contains(&lv))
            {
                entry.name = DataEntryName::Filler;
            }
            self.emit_syntax_error(
                self.current_span(),
                "unrecognized clause in data description entry",
            );
            self.advance();
        }
        Ok(())
    }

    /// Parses one of the 28 COBOL [`UsageKind`] storage representations.
    pub(crate) fn parse_usage_kind(&mut self) -> Option<UsageKind> {
        let (sym, _, _) = self.eat_word()?;
        let u = match self.sym_str(sym) {
            "DISPLAY" => UsageKind::Display,
            "DISPLAY-1" => UsageKind::Display1,
            "NATIONAL" => UsageKind::National,
            "UTF-8" => UsageKind::Utf8,
            "BINARY" | "COMP" | "COMPUTATIONAL" | "COMP-4" | "COMPUTATIONAL-4" => UsageKind::Binary,
            "COMP-5" | "COMPUTATIONAL-5" => UsageKind::Comp5,
            "PACKED-DECIMAL" | "COMP-3" | "COMPUTATIONAL-3" => UsageKind::PackedDecimal,
            "COMP-6" | "COMPUTATIONAL-6" => UsageKind::Comp6,
            "COMP-X" | "COMPUTATIONAL-X" => UsageKind::CompX,
            "COMP-N" | "COMPUTATIONAL-N" => UsageKind::CompN,
            "COMP-1" | "COMPUTATIONAL-1" | "FLOAT-SHORT" => UsageKind::FloatShort,
            "COMP-2" | "COMPUTATIONAL-2" | "FLOAT-LONG" => UsageKind::FloatLong,
            "FLOAT-EXTENDED" => UsageKind::FloatExtended,
            "FLOAT-DECIMAL-16" => UsageKind::FloatDecimal16,
            "FLOAT-DECIMAL-34" => UsageKind::FloatDecimal34,
            "BINARY-CHAR" => {
                if self.consume_word("UNSIGNED") {
                    UsageKind::BinaryCharUnsigned
                } else {
                    let _ = self.consume_word("SIGNED");
                    UsageKind::BinaryCharSigned
                }
            }
            "BINARY-SHORT" => {
                if self.consume_word("UNSIGNED") {
                    UsageKind::BinaryShortUnsigned
                } else {
                    let _ = self.consume_word("SIGNED");
                    UsageKind::BinaryShortSigned
                }
            }
            "BINARY-LONG" | "BINARY-INT" => {
                if self.consume_word("UNSIGNED") {
                    UsageKind::BinaryLongUnsigned
                } else {
                    let _ = self.consume_word("SIGNED");
                    UsageKind::BinaryLongSigned
                }
            }
            "SIGNED-CHAR" => UsageKind::BinaryCharSigned,
            "UNSIGNED-CHAR" => UsageKind::BinaryCharUnsigned,
            "SIGNED-SHORT" => UsageKind::BinaryShortSigned,
            "UNSIGNED-SHORT" => UsageKind::BinaryShortUnsigned,
            "SIGNED-INT" | "SIGNED-LONG" => UsageKind::BinaryLongSigned,
            "UNSIGNED-INT" | "UNSIGNED-LONG" => UsageKind::BinaryLongUnsigned,
            "BINARY-DOUBLE" => {
                if self.consume_word("UNSIGNED") {
                    UsageKind::BinaryDoubleUnsigned
                } else {
                    let _ = self.consume_word("SIGNED");
                    UsageKind::BinaryDoubleSigned
                }
            }
            "BINARY-C-LONG" => {
                if self.consume_word("UNSIGNED") {
                    UsageKind::BinaryCLongUnsigned
                } else {
                    let _ = self.consume_word("SIGNED");
                    UsageKind::BinaryCLongSigned
                }
            }
            "BIT" => UsageKind::Bit,
            "INDEX" => UsageKind::Index,
            "POINTER" => UsageKind::Pointer,
            "PROGRAM-POINTER" => UsageKind::ProgramPointer,
            "FUNCTION-POINTER" => UsageKind::FunctionPointer,
            "OBJECT" => {
                let _ = self.consume_word("REFERENCE");
                let _ = self.consume_word("FACTORY");
                let _ = self.consume_word("OF");
                let _ = self.consume_word("ONLY");
                let cls = if self.at_word("ACTIVE-CLASS") {
                    self.eat_word().map(|t| t.0)
                } else if !self.at_end_or_period() && !is_data_clause_starter(self.current_word()) {
                    self.eat_word().map(|t| t.0)
                } else {
                    None
                };
                UsageKind::ObjectReference(cls)
            }
            _ => return None,
        };
        Some(u)
    }

    /// Parses `[SIGN IS] {LEADING | TRAILING} [SEPARATE [CHARACTER]]`.
    fn parse_sign_clause(&mut self) -> SignClause {
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

    /// Parses `OCCURS [min TO] {max | UNBOUNDED} [TIMES] [DEPENDING ON ...] [KEY ...] [INDEXED BY ...]`.
    pub(crate) fn parse_occurs_clause(&mut self) -> OccursClause {
        let span = self.current_span();
        let first = if self.consume_word("DYNAMIC") {
            None
        } else {
            self.eat_occurs_bound()
        };
        let (min, max) = match first {
            None => {
                let _ = self.consume_word("CAPACITY");
                let _ = self.eat_word();
                (0, OccursUpperBound::Unbounded)
            }
            Some(lo) => {
                if self.consume_word("TO") {
                    if self.consume_word("UNBOUNDED") {
                        (lo, OccursUpperBound::Unbounded)
                    } else {
                        let hi = self.eat_occurs_bound().unwrap_or(lo);
                        (lo, OccursUpperBound::Bounded(hi))
                    }
                } else {
                    (lo, OccursUpperBound::Bounded(lo))
                }
            }
        };
        let _ = self.consume_word("TIMES");

        let mut depending_on = None;
        let mut keys = Vec::new();
        let mut indexed_by = Vec::new();

        while !self.at_end_or_period() {
            if self.consume_word("DEPENDING") {
                let _ = self.consume_word("ON");
                depending_on = Some(self.parse_simple_qualified_syms());
            } else if matches!(self.current_word(), Some("ASCENDING" | "DESCENDING")) {
                let order = if self.consume_word("ASCENDING") {
                    KeyOrder::Ascending
                } else {
                    let _ = self.consume_word("DESCENDING");
                    KeyOrder::Descending
                };
                let _ = self.consume_word("KEY");
                let _ = self.consume_word("IS");
                let mut key_paths = Vec::new();
                while !self.at_end_or_period()
                    && !is_data_clause_starter(self.current_word())
                    && !matches!(
                        self.current_word(),
                        Some("ASCENDING" | "DESCENDING" | "INDEXED" | "DEPENDING")
                    )
                {
                    let p = self.parse_simple_qualified_syms();
                    if p.is_empty() {
                        break;
                    }
                    key_paths.push(p);
                }
                keys.push(OccursKeySpec {
                    order,
                    keys: key_paths,
                });
            } else if self.consume_word("INDEXED") {
                let _ = self.consume_word("BY");
                while !self.at_end_or_period()
                    && !is_data_clause_starter(self.current_word())
                    && !matches!(
                        self.current_word(),
                        Some("ASCENDING" | "DESCENDING" | "DEPENDING")
                    )
                {
                    if let Some((idx_sym, _, _)) = self.eat_word() {
                        indexed_by.push(idx_sym);
                    } else {
                        break;
                    }
                }
            } else {
                break;
            }
        }

        OccursClause {
            min,
            max,
            depending_on,
            keys,
            indexed_by,
            span,
        }
    }

    /// Parses `[IS | ARE] val [THRU high] ... [WHEN SET TO FALSE IS atom]`.
    fn parse_value_clause(&mut self) -> Result<ValueClause> {
        let span = self.current_span();
        let _ = self.consume_word("IS");
        let _ = self.consume_word("ARE");
        let _ = self.consume_word("FROM");

        let mut values = Vec::new();
        let mut false_value = None;

        while !self.at_end_or_period() && !is_non_value_clause_starter(self.current_word()) {
            if self.consume_word("WHEN") {
                let _ = self.consume_word("SET");
                let _ = self.consume_word("TO");
                let _ = self.consume_word("FALSE");
                let _ = self.consume_word("IS");
                false_value = self.parse_value_atom()?;
                break;
            }
            let r_span = self.current_span();
            let Some(low) = self.parse_value_atom()? else {
                break;
            };
            let high = if self.consume_any_word(&["THRU", "THROUGH"]) {
                self.parse_value_atom()?
            } else {
                None
            };
            values.push(ValueRange {
                low,
                high,
                span: r_span,
            });
        }

        Ok(ValueClause {
            values,
            false_value,
            span,
        })
    }

    /// Parses an `ALL <literal | figurative>` atom assuming `self.at_word("ALL")` is true.
    pub(crate) fn parse_all_figurative_atom(&mut self) -> ValueAtom {
        self.advance();
        let bytes = match &self.peek().kind {
            TokenKind::StringLit { bytes, .. } => {
                let b = bytes.clone();
                self.advance();
                b
            }
            TokenKind::Word { sym, .. } => {
                let b = match self.sym_str(*sym) {
                    "ZERO" | "ZEROS" | "ZEROES" => vec![b'0'],
                    "SPACE" | "SPACES" => vec![b' '],
                    "QUOTE" | "QUOTES" => vec![b'"'],
                    "HIGH-VALUE" | "HIGH-VALUES" => vec![0xFF],
                    "LOW-VALUE" | "LOW-VALUES" => vec![0x00],
                    _ => Vec::new(),
                };
                self.advance();
                b
            }
            _ => Vec::new(),
        };
        ValueAtom::Figurative(FigurativeValue::All(bytes))
    }

    /// Parses a single literal, figurative constant, or constant identifier [`ValueAtom`].
    pub(crate) fn parse_value_atom(&mut self) -> Result<Option<ValueAtom>> {
        self.skip_separators();
        if self.at_word("ALL") {
            return Ok(Some(self.parse_all_figurative_atom()));
        }

        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::StringLit { mut bytes, prefix } => {
                self.advance();
                while matches!(self.peek().kind, TokenKind::Minus | TokenKind::Ampersand) {
                    if let Some(next_tok) = self.peek_non_sep_at(1)
                        && let TokenKind::StringLit { bytes: more, .. } = &next_tok.kind
                    {
                        let extra = more.clone();
                        self.advance();
                        self.advance();
                        bytes.extend_from_slice(&extra);
                    } else {
                        break;
                    }
                }
                Ok(Some(ValueAtom::String { bytes, prefix }))
            }
            TokenKind::IntegerLit(sym) => {
                self.advance();
                Ok(Some(ValueAtom::Integer(sym)))
            }
            TokenKind::DecimalLit {
                digits,
                scale,
                sign,
            } => {
                self.advance();
                Ok(Some(ValueAtom::Decimal {
                    digits,
                    scale,
                    sign,
                }))
            }
            TokenKind::FloatLit(sym) => {
                self.advance();
                Ok(Some(ValueAtom::Float(sym)))
            }
            TokenKind::Plus | TokenKind::Minus => {
                let is_neg = tok.kind == TokenKind::Minus;
                self.advance();
                match self.peek().kind {
                    TokenKind::IntegerLit(sym) => {
                        let raw = self.sym_str(sym).to_owned();
                        self.advance();
                        let signed = if is_neg {
                            format!("-{raw}")
                        } else {
                            format!("+{raw}")
                        };
                        let s_sym = self.interner.intern(&signed)?;
                        Ok(Some(ValueAtom::Integer(s_sym)))
                    }
                    TokenKind::DecimalLit { digits, scale, .. } => {
                        self.advance();
                        let sign = if is_neg {
                            crate::token::LiteralSign::Negative
                        } else {
                            crate::token::LiteralSign::Positive
                        };
                        Ok(Some(ValueAtom::Decimal {
                            digits,
                            scale,
                            sign,
                        }))
                    }
                    _ => Ok(None),
                }
            }
            TokenKind::Word { sym, .. } => {
                if let Some(fig) = match_figurative_word(self.sym_str(sym)) {
                    self.advance();
                    Ok(Some(ValueAtom::Figurative(fig)))
                } else {
                    self.advance();
                    Ok(Some(ValueAtom::Identifier(sym)))
                }
            }
            _ => Ok(None),
        }
    }

    /// Parses `RD` entries and report groups inside `REPORT SECTION.`.
    ///
    /// # Arguments
    ///
    /// * `out` - Output `RD` list.
    fn parse_report_section(&mut self, out: &mut Vec<RdEntry>) -> Result<()> {
        self.parse_report_section_m18(out)
    }

    /// Parses `01`..`49` screen description entries inside `SCREEN SECTION.`.
    ///
    /// # Arguments
    ///
    /// * `out` - Output screen entry list.
    fn parse_screen_section(&mut self, out: &mut Vec<ScreenEntry>) -> Result<()> {
        self.parse_screen_section_m19(out)
    }

    /// Peeks whether the current token is an `IntegerLit` (level number candidate).
    #[must_use]
    fn peek_level_number(&self) -> Option<u8> {
        let tok = self.peek_non_sep_at(0)?;
        if let TokenKind::IntegerLit(sym) = tok.kind {
            self.sym_str(sym).parse::<u8>().ok()
        } else {
            None
        }
    }

    /// Consumes a level-number `IntegerLit` (`01`..`49`, `66`, `77`, `78`, `88`), diagnosing invalid numbers.
    pub(crate) fn eat_level_number(&mut self) -> Option<(LevelNumber, SourceSpan)> {
        self.skip_separators();
        let tok = self.tokens.get(self.pos)?;
        let TokenKind::IntegerLit(sym) = tok.kind else {
            return None;
        };
        let span = tok.span;
        let raw_str = self.sym_str(sym).to_owned();
        self.advance();
        let raw_num = raw_str.parse::<u8>().unwrap_or(0);
        if let Ok(lvl) = LevelNumber::parse(raw_num) {
            Some((lvl, span))
        } else {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(span),
                format!("invalid COBOL level-number `{raw_str}`"),
            ));
            Some((LevelNumber::Hierarchy(1), span))
        }
    }

    /// Consumes an unsigned `u32` integer literal if present.
    pub(crate) fn eat_u32_lit(&mut self) -> Option<u32> {
        self.skip_separators();
        if let TokenKind::IntegerLit(sym) = self.peek().kind {
            let val = self.sym_str(sym).parse::<u32>().ok()?;
            self.advance();
            Some(val)
        } else {
            None
        }
    }

    /// Consumes an `OCCURS` integer bound (literal or `78`-level constant symbol).
    fn eat_occurs_bound(&mut self) -> Option<u32> {
        if let Some(n) = self.eat_u32_lit() {
            return Some(n);
        }
        if let TokenKind::Word { sym, .. } = self.peek().kind
            && let Some(&val) = self.level_78_ints.get(&sym)
        {
            self.advance();
            return Some(val);
        }
        None
    }

    /// Returns `true` when positioned at `PROCEDURE DIVISION` or unit end.
    #[must_use]
    pub(crate) fn starts_after_data_division(&self) -> bool {
        self.at_word_pair("PROCEDURE", "DIVISION")
            || self.at_word("FACTORY")
            || (self.at_word("OBJECT") && !self.peek_word_at(1, "REFERENCE"))
            || self.starts_nested_program()
            || self.at_end_unit_header()
    }

    /// Returns `true` when positioned at the header of another `DATA DIVISION` section.
    #[must_use]
    pub(crate) fn starts_other_data_section(&self) -> bool {
        matches!(
            self.current_word(),
            Some(
                "FILE"
                    | "WORKING-STORAGE"
                    | "LOCAL-STORAGE"
                    | "LINKAGE"
                    | "REPORT"
                    | "SCREEN"
                    | "COMMUNICATION"
            )
        ) && self.peek_word_at(1, "SECTION")
    }
}

/// Maps a canonical uppercase COBOL word to its [`FigurativeValue`], if it is a figurative constant.
///
/// # Arguments
///
/// * `word` - Canonical uppercase word.
#[must_use]
pub(crate) fn match_figurative_word(word: &str) -> Option<FigurativeValue> {
    match word {
        "ZERO" | "ZEROS" | "ZEROES" => Some(FigurativeValue::Zero),
        "SPACE" | "SPACES" => Some(FigurativeValue::Space),
        "HIGH-VALUE" | "HIGH-VALUES" => Some(FigurativeValue::HighValue),
        "LOW-VALUE" | "LOW-VALUES" => Some(FigurativeValue::LowValue),
        "QUOTE" | "QUOTES" => Some(FigurativeValue::Quote),
        "NULL" | "NULLS" => Some(FigurativeValue::Null),
        _ => None,
    }
}

/// Checks if `w` starts an `FD`/`SD` sub-clause.
///
/// # Arguments
///
/// * `w` - Current uppercase word.
#[must_use]
fn is_fd_clause_keyword(w: Option<&str>) -> bool {
    matches!(
        w,
        Some(
            "EXTERNAL"
                | "GLOBAL"
                | "BLOCK"
                | "RECORD"
                | "LABEL"
                | "VALUE"
                | "DATA"
                | "LINAGE"
                | "REPORT"
                | "REPORTS"
                | "CODE-SET"
        )
    )
}

/// Checks if `w` is an unadorned `USAGE` representation keyword (`COMP`, `COMP-3`, `COMP-5`, `BINARY`, etc.).
///
/// # Arguments
///
/// * `w` - Current uppercase word.
#[must_use]
pub(crate) fn is_bare_usage_keyword(w: Option<&str>) -> bool {
    matches!(
        w,
        Some(
            "DISPLAY"
                | "DISPLAY-1"
                | "NATIONAL"
                | "UTF-8"
                | "BINARY"
                | "COMP"
                | "COMPUTATIONAL"
                | "COMP-1"
                | "COMPUTATIONAL-1"
                | "COMP-2"
                | "COMPUTATIONAL-2"
                | "COMP-3"
                | "COMPUTATIONAL-3"
                | "COMP-4"
                | "COMPUTATIONAL-4"
                | "COMP-5"
                | "COMPUTATIONAL-5"
                | "COMP-6"
                | "COMPUTATIONAL-6"
                | "COMP-X"
                | "COMPUTATIONAL-X"
                | "COMP-N"
                | "COMPUTATIONAL-N"
                | "PACKED-DECIMAL"
                | "FLOAT-SHORT"
                | "FLOAT-LONG"
                | "FLOAT-EXTENDED"
                | "FLOAT-DECIMAL-16"
                | "FLOAT-DECIMAL-34"
                | "BINARY-CHAR"
                | "BINARY-SHORT"
                | "BINARY-INT"
                | "BINARY-LONG"
                | "BINARY-DOUBLE"
                | "BINARY-C-LONG"
                | "SIGNED-CHAR"
                | "UNSIGNED-CHAR"
                | "SIGNED-SHORT"
                | "UNSIGNED-SHORT"
                | "SIGNED-INT"
                | "UNSIGNED-INT"
                | "SIGNED-LONG"
                | "UNSIGNED-LONG"
                | "BIT"
                | "INDEX"
                | "POINTER"
                | "PROGRAM-POINTER"
                | "FUNCTION-POINTER"
                | "OBJECT"
        )
    )
}

/// Checks if `w` begins any Data Division clause (used to detect unnamed `FILLER` entries).
///
/// # Arguments
///
/// * `w` - Current uppercase word.
#[must_use]
fn is_data_clause_starter(w: Option<&str>) -> bool {
    is_non_value_clause_starter(w) || matches!(w, Some("VALUE" | "VALUES"))
}

/// Checks if `w` begins a non-`VALUE` Data Division clause.
///
/// # Arguments
///
/// * `w` - Current uppercase word.
#[must_use]
fn is_non_value_clause_starter(w: Option<&str>) -> bool {
    is_bare_usage_keyword(w)
        || matches!(
            w,
            Some(
                "REDEFINES"
                    | "RENAMES"
                    | "PIC"
                    | "PICTURE"
                    | "USAGE"
                    | "SIGN"
                    | "LEADING"
                    | "TRAILING"
                    | "OCCURS"
                    | "SYNCHRONIZED"
                    | "SYNC"
                    | "JUSTIFIED"
                    | "JUST"
                    | "BLANK"
                    | "EXTERNAL"
                    | "GLOBAL"
                    | "BASED"
                    | "ANY"
                    | "DYNAMIC"
                    | "TYPEDEF"
                    | "TYPE"
                    | "SAME"
                    | "CONSTANT"
                    | "SPECIAL-NAMES"
            )
        )
}
