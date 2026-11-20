//! Context-sensitive COBOL lexical analyzer over preprocessed [`coblang_pp::PpToken`] streams.

use coblang_common::{ByteLength, ByteOffset, Dialect, Result, ScaleFactor, SourceSpan};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, FeatureDisposition,
    RelaxedSyntaxMode, SymbolInterner, WarningGroup,
};
use coblang_pp::{MarginZone, PpToken, PpTokenKind, SpacingHint};

use crate::token::{
    DecimalCommaMode, LexerMode, LiteralSign, StringPrefix, Token, TokenKind, WordRole,
};

/// Double-quote character (`0x22`).
const DQ: char = '\u{0022}';

/// Tracks whether scanning is currently within `IDENTIFICATION DIVISION` header paragraphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentDivisionState {
    /// Inside `IDENTIFICATION DIVISION` (comment-entry paragraphs like `AUTHOR.` may appear).
    Active,
    /// Inside `ENVIRONMENT`, `DATA`, or `PROCEDURE` division.
    Exited,
}

/// Stateful context-sensitive COBOL lexer converting [`PpToken`]s into typed [`Token`]s.
pub struct Lexer<'a> {
    /// Input preprocessed token slice.
    pp_tokens: &'a [PpToken],
    /// Current read cursor into `pp_tokens`.
    cursor: usize,
    /// Active scanner mode (`Normal`, `Picture`, or `IdCommentParagraph`).
    mode: LexerMode,
    /// Active decimal-point character convention (`PeriodIsDecimal` vs `CommaIsDecimal`).
    decimal_mode: DecimalCommaMode,
    /// Tracks whether `IDENTIFICATION DIVISION` comment paragraphs are active.
    ident_state: IdentDivisionState,
    /// Active dialect configuration profile.
    dialect: &'a DialectConfig,
    /// Shared identifier/symbol interner.
    interner: &'a mut SymbolInterner,
    /// Compiler diagnostic sink.
    sink: &'a mut DiagnosticSink,
}

/// Lexes a preprocessed [`PpToken`] slice into a `TokenKind::Eof`-terminated [`Token`] vector.
///
/// # Arguments
///
/// * `pp_tokens` - Preprocessed token stream from `coblang-pp`.
/// * `eof_span` - Sentinel [`SourceSpan`] attached to the trailing `TokenKind::Eof`.
/// * `dialect` - Active [`DialectConfig`] governing reserved words and literal limits.
/// * `interner` - Mutable [`SymbolInterner`] for interning words, literals, and `PICTURE` strings.
/// * `sink` - Mutable [`DiagnosticSink`] receiving lexical warnings and errors.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if symbol interning exceeds capacity.
pub fn lex_tokens(
    pp_tokens: &[PpToken],
    eof_span: SourceSpan,
    dialect: &DialectConfig,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<Vec<Token>> {
    let mut lexer = Lexer {
        pp_tokens,
        cursor: 0,
        mode: LexerMode::Normal,
        decimal_mode: DecimalCommaMode::PeriodIsDecimal,
        ident_state: IdentDivisionState::Active,
        dialect,
        interner,
        sink,
    };
    lexer.run(eof_span)
}

impl Lexer<'_> {
    /// Drives the main scanner loop until all `pp_tokens` are consumed.
    ///
    /// # Arguments
    ///
    /// * `eof_span` - Sentinel [`SourceSpan`] for the final `TokenKind::Eof` token.
    fn run(&mut self, eof_span: SourceSpan) -> Result<Vec<Token>> {
        let mut out = Vec::with_capacity(self.pp_tokens.len() + 1);
        while let Some(pp) = self.pp_tokens.get(self.cursor).cloned() {
            match self.mode {
                LexerMode::IdCommentParagraph => {
                    if should_exit_id_comment(&pp) {
                        self.mode = LexerMode::Normal;
                    } else {
                        self.cursor += 1;
                    }
                }
                LexerMode::Picture => {
                    self.lex_picture_step(&pp, &mut out)?;
                }
                LexerMode::Normal => {
                    self.lex_normal_step(&pp, &mut out)?;
                }
            }
        }
        if self.mode == LexerMode::Picture {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0006InvalidPicture,
                Some(eof_span),
                String::from("missing PICTURE character-string before end of file"),
            ));
        }
        out.push(Token {
            kind: TokenKind::Eof,
            span: eof_span,
        });
        Ok(out)
    }

    /// Scans one step in [`LexerMode::Picture`], handling optional `IS` and assembling `PictureString`.
    ///
    /// # Arguments
    ///
    /// * `first` - Current [`PpToken`] at `self.cursor`.
    /// * `out` - Output token accumulator.
    fn lex_picture_step(&mut self, first: &PpToken, out: &mut Vec<Token>) -> Result<()> {
        if first.kind == PpTokenKind::Word && first.text.eq_ignore_ascii_case("IS") {
            let tok = self.make_word_token(&first.text, first.zone, first.span)?;
            out.push(tok);
            self.cursor += 1;
            return Ok(());
        }
        if first.kind == PpTokenKind::Period {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0006InvalidPicture,
                Some(first.span),
                String::from("missing PICTURE character-string before `.`"),
            ));
            self.mode = LexerMode::Normal;
            return Ok(());
        }

        let mut pic_buf = first.text.clone();
        let start_span = first.span;
        let mut last_span = first.span;
        self.cursor += 1;

        while let Some(next) = self.pp_tokens.get(self.cursor) {
            if next.spacing != SpacingHint::Tight || next.kind == PpTokenKind::Period {
                break;
            }
            if next.kind == PpTokenKind::SeparatorPunct {
                let keep_sep = self.pp_tokens.get(self.cursor + 1).is_some_and(|after| {
                    after.spacing == SpacingHint::Tight && after.kind != PpTokenKind::Period
                });
                if !keep_sep {
                    break;
                }
            }
            pic_buf.push_str(&next.text);
            last_span = next.span;
            self.cursor += 1;
        }

        let sym = self.interner.intern(&pic_buf)?;
        out.push(Token {
            kind: TokenKind::PictureString(sym),
            span: start_span.merge(last_span),
        });
        self.mode = LexerMode::Normal;
        Ok(())
    }

    /// Scans one [`PpToken`] (and any fused Tight neighbors) in [`LexerMode::Normal`].
    ///
    /// # Arguments
    ///
    /// * `pp` - Current [`PpToken`] at `self.cursor`.
    /// * `out` - Output token accumulator.
    fn lex_normal_step(&mut self, pp: &PpToken, out: &mut Vec<Token>) -> Result<()> {
        let span = pp.span;
        match pp.kind {
            PpTokenKind::LParen => {
                out.push(Token {
                    kind: TokenKind::LParen,
                    span,
                });
                self.cursor += 1;
            }
            PpTokenKind::RParen => {
                out.push(Token {
                    kind: TokenKind::RParen,
                    span,
                });
                self.cursor += 1;
            }
            PpTokenKind::Period => {
                out.push(Token {
                    kind: TokenKind::Period,
                    span,
                });
                self.cursor += 1;
            }
            PpTokenKind::Colon => {
                if let Some(next) = self.pp_tokens.get(self.cursor + 1)
                    && next.kind == PpTokenKind::Colon
                    && next.spacing == SpacingHint::Tight
                {
                    out.push(Token {
                        kind: TokenKind::DoubleColon,
                        span: span.merge(next.span),
                    });
                    self.cursor += 2;
                } else {
                    out.push(Token {
                        kind: TokenKind::Colon,
                        span,
                    });
                    self.cursor += 1;
                }
            }
            PpTokenKind::SeparatorPunct => {
                let kind = if pp.text == "," {
                    TokenKind::Comma
                } else {
                    TokenKind::Semicolon
                };
                out.push(Token { kind, span });
                self.cursor += 1;
            }
            PpTokenKind::PseudoDelimiter => {
                self.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0002Lexical,
                    Some(span),
                    String::from("unexpected `==` pseudo-text delimiter outside COPY/REPLACE"),
                ));
                self.cursor += 1;
            }
            PpTokenKind::StringLiteral => {
                self.cursor += 1;
                let tok = self.decode_string_literal(&pp.text, span);
                out.push(tok);
            }
            PpTokenKind::Word => {
                self.lex_word_or_operators(pp, out)?;
            }
        }
        Ok(())
    }

    /// Scans a `PpTokenKind::Word` (handling `CommaIsDecimal` fusion, operators, literals, and mode switches).
    ///
    /// # Arguments
    ///
    /// * `pp` - Current `PpTokenKind::Word` token at `self.cursor`.
    /// * `out` - Output token accumulator.
    fn lex_word_or_operators(&mut self, pp: &PpToken, out: &mut Vec<Token>) -> Result<()> {
        if self.decimal_mode == DecimalCommaMode::CommaIsDecimal
            && let Some(fused) = self.try_lex_comma_decimal(pp)?
        {
            out.push(fused);
            return Ok(());
        }

        self.cursor += 1;
        self.update_state_on_raw_word(&pp.text, pp.zone, pp.spacing, pp.span, out)
    }

    /// Updates division/comment/picture/decimal state or splits embedded operators inside `raw_text`.
    ///
    /// # Arguments
    ///
    /// * `raw_text` - Raw `PpTokenKind::Word` text.
    /// * `area` - Margin zone of the `PpToken`.
    /// * `spacing` - Preceding spacing hint.
    /// * `base_span` - Span of `raw_text`.
    /// * `out` - Output token accumulator.
    fn update_state_on_raw_word(
        &mut self,
        raw_text: &str,
        area: MarginZone,
        spacing: SpacingHint,
        base_span: SourceSpan,
        out: &mut Vec<Token>,
    ) -> Result<()> {
        if raw_text.starts_with('%') && raw_text.len() > 1 {
            out.push(self.lex_octal_literal(raw_text, base_span)?);
            return Ok(());
        }

        if self.scan_word_segments(raw_text, area, base_span, out)? {
            self.inspect_emitted_keyword(raw_text, area, spacing, base_span, out);
        }
        Ok(())
    }

    /// Checks a newly emitted single `Word` token for mode transitions (`PIC`, `DECIMAL-POINT`, `AUTHOR.`, divisions).
    ///
    /// # Arguments
    ///
    /// * `word` - Raw word text.
    /// * `area` - Margin zone of `word`.
    /// * `spacing` - Preceding spacing hint of `word`.
    /// * `span` - Source span of `word`.
    /// * `out` - Output token accumulator (used to emit trailing `.` on comment paragraphs).
    fn inspect_emitted_keyword(
        &mut self,
        word: &str,
        area: MarginZone,
        spacing: SpacingHint,
        span: SourceSpan,
        out: &mut Vec<Token>,
    ) {
        let upper = word.to_ascii_uppercase();
        match upper.as_str() {
            "IDENTIFICATION" | "ID" => {
                self.ident_state = IdentDivisionState::Active;
            }
            "ENVIRONMENT" | "DATA" | "PROCEDURE" => {
                if self.next_word_eq("DIVISION") {
                    self.ident_state = IdentDivisionState::Exited;
                }
            }
            "END" => {
                if self.next_word_is_unit_kind() {
                    self.ident_state = IdentDivisionState::Active;
                    self.decimal_mode = DecimalCommaMode::PeriodIsDecimal;
                }
            }
            "DECIMAL-POINT" => {
                if self.matches_decimal_point_is_comma() {
                    self.decimal_mode = DecimalCommaMode::CommaIsDecimal;
                }
            }
            "PIC" | "PICTURE" => {
                if !self.next_word_eq("SYMBOL") {
                    self.mode = LexerMode::Picture;
                }
            }
            "AUTHOR" | "INSTALLATION" | "DATE-WRITTEN" | "DATE-COMPILED" | "SECURITY"
            | "REMARKS" => {
                let at_header = area == MarginZone::AreaA
                    || (area == MarginZone::FreeFlow && spacing == SpacingHint::Newline);
                if self.ident_state == IdentDivisionState::Active
                    && at_header
                    && let Some(dot) = self.pp_tokens.get(self.cursor)
                    && dot.kind == PpTokenKind::Period
                {
                    let dot_span = dot.span;
                    self.cursor += 1;
                    out.push(Token {
                        kind: TokenKind::Period,
                        span: dot_span,
                    });
                    self.diagnose_comment_paragraph(&upper, span);
                    self.mode = LexerMode::IdCommentParagraph;
                }
            }
            _ => {}
        }
    }

    /// Emits dialect-appropriate obsolete warning or unconformable error on `AUTHOR.`..`REMARKS.`.
    ///
    /// # Arguments
    ///
    /// * `name` - Uppercase paragraph header name.
    /// * `span` - Source span of the paragraph header.
    fn diagnose_comment_paragraph(&mut self, name: &str, span: SourceSpan) {
        let disp = self
            .dialect
            .feature_rules
            .get("comment-paragraphs")
            .copied()
            .unwrap_or(match self.dialect.base_dialect {
                Dialect::Cobol85 => FeatureDisposition::Obsolete,
                Dialect::Cobol2002 | Dialect::Cobol2014 => {
                    if self.dialect.relaxed_syntax == RelaxedSyntaxMode::Relaxed {
                        FeatureDisposition::Warning
                    } else {
                        FeatureDisposition::Unconformable
                    }
                }
                Dialect::Cobol2023
                | Dialect::Default
                | Dialect::Gnu
                | Dialect::Ibm
                | Dialect::MicroFocus
                | Dialect::Bs2000
                | Dialect::Acu
                | Dialect::XOpen => FeatureDisposition::Ok,
            });
        emit_feature_disposition(
            self.sink,
            disp,
            span,
            &format!("`{name}` paragraph in IDENTIFICATION DIVISION"),
        );
    }

    /// Checks whether the next `PpToken` is a `Word` matching `expected` case-insensitively.
    ///
    /// # Arguments
    ///
    /// * `expected` - Uppercase word to compare against.
    #[must_use]
    fn next_word_eq(&self, expected: &str) -> bool {
        self.pp_tokens
            .get(self.cursor)
            .is_some_and(|t| t.kind == PpTokenKind::Word && t.text.eq_ignore_ascii_case(expected))
    }

    /// Checks whether the next `PpToken` starts `END PROGRAM|FUNCTION|CLASS|INTERFACE`.
    #[must_use]
    fn next_word_is_unit_kind(&self) -> bool {
        self.pp_tokens.get(self.cursor).is_some_and(|t| {
            t.kind == PpTokenKind::Word
                && (t.text.eq_ignore_ascii_case("PROGRAM")
                    || t.text.eq_ignore_ascii_case("FUNCTION")
                    || t.text.eq_ignore_ascii_case("CLASS")
                    || t.text.eq_ignore_ascii_case("INTERFACE"))
        })
    }

    /// Checks whether upcoming tokens form `[IS] COMMA` right after `DECIMAL-POINT`.
    #[must_use]
    fn matches_decimal_point_is_comma(&self) -> bool {
        let Some(first) = self.pp_tokens.get(self.cursor) else {
            return false;
        };
        if first.kind != PpTokenKind::Word {
            return false;
        }
        if first.text.eq_ignore_ascii_case("COMMA") {
            return true;
        }
        first.text.eq_ignore_ascii_case("IS")
            && self.pp_tokens.get(self.cursor + 1).is_some_and(|second| {
                second.kind == PpTokenKind::Word && second.text.eq_ignore_ascii_case("COMMA")
            })
    }

    /// Attempts to fuse `<int>,<frac>` when `DECIMAL-POINT IS COMMA` is active.
    ///
    /// # Arguments
    ///
    /// * `left` - Current `PpTokenKind::Word` token at `self.cursor`.
    fn try_lex_comma_decimal(&mut self, left: &PpToken) -> Result<Option<Token>> {
        let (Some(mid), Some(right)) = (
            self.pp_tokens.get(self.cursor + 1),
            self.pp_tokens.get(self.cursor + 2),
        ) else {
            return Ok(None);
        };
        if mid.kind != PpTokenKind::SeparatorPunct
            || mid.text != ","
            || mid.spacing != SpacingHint::Tight
            || right.kind != PpTokenKind::Word
            || right.spacing != SpacingHint::Tight
        {
            return Ok(None);
        }
        let left_clean = left
            .text
            .strip_prefix('+')
            .or_else(|| left.text.strip_prefix('-'))
            .unwrap_or(&left.text);
        if left_clean.is_empty()
            || !left_clean.bytes().all(|b| b.is_ascii_digit())
            || !right
                .text
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_digit())
        {
            return Ok(None);
        }
        let combined = format!("{}.{}", left.text, right.text);
        let merged_span = left.span.merge(right.span);
        self.cursor += 3;
        let tok = self.classify_numeric_segment(&combined, merged_span)?;
        Ok(Some(tok))
    }

    /// Splits `raw` into operators, numeric literals, and COBOL words.
    ///
    /// # Arguments
    ///
    /// * `raw` - Raw word slice from `PpTokenKind::Word`.
    /// * `area` - Margin zone for the first sub-token on the slice.
    /// * `base_span` - Enclosing [`SourceSpan`] of `raw`.
    /// * `out` - Output token accumulator.
    fn scan_word_segments(
        &mut self,
        raw: &str,
        area: MarginZone,
        base_span: SourceSpan,
        out: &mut Vec<Token>,
    ) -> Result<bool> {
        let start_out_len = out.len();
        let mut word_count = 0_usize;
        let bytes = raw.as_bytes();
        let mut idx = 0;
        while let Some(&b) = bytes.get(idx) {
            let sub_area = if idx == 0 { area } else { MarginZone::AreaB };
            if let Some((op_kind, op_len)) = match_operator_at(bytes, idx) {
                if (b == b'+' || b == b'-')
                    && let Some(num_end) = scan_signed_numeric_end(bytes, idx)
                {
                    let seg = raw.get(idx..num_end).unwrap_or("");
                    let span = slice_span(base_span, idx, num_end - idx);
                    let tok = self.classify_numeric_segment(seg, span)?;
                    out.push(tok);
                    idx = num_end;
                    continue;
                }
                out.push(Token {
                    kind: op_kind,
                    span: slice_span(base_span, idx, op_len),
                });
                idx += op_len;
                continue;
            }

            if (b == b'.' || b.is_ascii_digit())
                && let Some(num_end) = scan_unsigned_numeric_end(bytes, idx)
            {
                let seg = raw.get(idx..num_end).unwrap_or("");
                let span = slice_span(base_span, idx, num_end - idx);
                let tok = self.classify_numeric_segment(seg, span)?;
                out.push(tok);
                idx = num_end;
                continue;
            }

            if is_word_start_byte(b) {
                let end = scan_cobol_word_end(bytes, idx);
                let seg = raw.get(idx..end).unwrap_or("");
                let span = slice_span(base_span, idx, end - idx);
                if seg.ends_with('-') {
                    self.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0002Lexical,
                        Some(span),
                        format!("identifier `{seg}` cannot end with a hyphen"),
                    ));
                }
                let tok = self.make_word_token(seg, sub_area, span)?;
                out.push(tok);
                word_count += 1;
                idx = end;
                continue;
            }

            let ch_len = raw
                .get(idx..)
                .and_then(|s| s.chars().next())
                .map_or(1, char::len_utf8);
            let bad = raw.get(idx..idx + ch_len).unwrap_or("?");
            let span = slice_span(base_span, idx, ch_len);
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("invalid character `{bad}` in COBOL source"),
            ));
            idx += ch_len;
        }
        Ok(out.len() == start_out_len + 1 && word_count == 1)
    }

    /// Classifies a validated numeric candidate (`123`, `-45.67`, `1.5E+2`, `1.2.3`) and checks digit limits.
    ///
    /// # Arguments
    ///
    /// * `seg` - Candidate numeric substring (using `.` for radix point).
    /// * `span` - Source span of `seg`.
    fn classify_numeric_segment(&mut self, seg: &str, span: SourceSpan) -> Result<Token> {
        let (sign, unsigned) = if let Some(rest) = seg.strip_prefix('+') {
            (LiteralSign::Positive, rest)
        } else if let Some(rest) = seg.strip_prefix('-') {
            (LiteralSign::Negative, rest)
        } else {
            (LiteralSign::Unsigned, seg)
        };

        if let Some(exp_pos) = unsigned.find(['E', 'e']) {
            let mantissa = unsigned.get(..exp_pos).unwrap_or("");
            let exp_part = unsigned.get(exp_pos + 1..).unwrap_or("");
            let exp_digits = exp_part
                .strip_prefix('+')
                .or_else(|| exp_part.strip_prefix('-'))
                .unwrap_or(exp_part);
            let valid_mantissa = count_char(mantissa, '.') <= 1
                && mantissa.bytes().all(|b| b.is_ascii_digit() || b == b'.');
            let valid_exp = !exp_digits.is_empty()
                && exp_digits.len() <= 4
                && exp_digits.bytes().all(|b| b.is_ascii_digit());
            if !valid_mantissa || !valid_exp {
                self.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0002Lexical,
                    Some(span),
                    format!("malformed floating-point literal `{seg}`"),
                ));
            }
            let sym = self.interner.intern(seg)?;
            return Ok(Token {
                kind: TokenKind::FloatLit(sym),
                span,
            });
        }

        let dot_count = count_char(unsigned, '.');
        if dot_count == 0 {
            self.check_numeric_digit_limit(unsigned.len(), seg, span);
            let sym = self.interner.intern(seg)?;
            return Ok(Token {
                kind: TokenKind::IntegerLit(sym),
                span,
            });
        }

        if dot_count > 1 {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("malformed numeric literal `{seg}` with multiple decimal points"),
            ));
        }

        let (int_part, frac_part) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let clean_frac: String = frac_part
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>();
        let mut all_digits = String::with_capacity(int_part.len() + clean_frac.len());
        all_digits.push_str(int_part);
        all_digits.push_str(&clean_frac);
        self.check_numeric_digit_limit(all_digits.len(), seg, span);

        let raw_scale = i16::try_from(clean_frac.len()).unwrap_or(38).min(38);
        let scale = ScaleFactor::try_new(raw_scale).unwrap_or(ScaleFactor::ZERO);
        let digits_sym = self.interner.intern(&all_digits)?;
        Ok(Token {
            kind: TokenKind::DecimalLit {
                digits: digits_sym,
                scale,
                sign,
            },
            span,
        })
    }

    /// Validates numeric literal digit length against `self.dialect.max_pic_digits`.
    ///
    /// # Arguments
    ///
    /// * `digit_len` - Count of decimal digits in the literal.
    /// * `raw` - Original literal spelling for diagnostic messages.
    /// * `span` - Source span of the literal.
    fn check_numeric_digit_limit(&mut self, digit_len: usize, raw: &str, span: SourceSpan) {
        let max_digits = usize::from(self.dialect.max_pic_digits);
        if digit_len > max_digits {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("numeric literal `{raw}` has {digit_len} digits (maximum is {max_digits})"),
            ));
        }
    }

    /// Lexes an HP COBOL `%octal` literal (emitting dialect error unless `hp-octal-literals` is enabled).
    ///
    /// # Arguments
    ///
    /// * `raw` - Raw `%...` token text.
    /// * `span` - Source span.
    fn lex_octal_literal(&mut self, raw: &str, span: SourceSpan) -> Result<Token> {
        let oct = raw.get(1..).unwrap_or("");
        let disp = self
            .dialect
            .feature_rules
            .get("hp-octal-literals")
            .copied()
            .unwrap_or(FeatureDisposition::Unconformable);
        emit_feature_disposition(self.sink, disp, span, "HP COBOL octal literal");

        let mut value = 0_u128;
        let mut valid = !oct.is_empty();
        for b in oct.bytes() {
            if (b'0'..=b'7').contains(&b) {
                value = value.saturating_mul(8).saturating_add(u128::from(b - b'0'));
            } else {
                valid = false;
            }
        }
        if !valid {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("invalid octal literal `{raw}`"),
            ));
        }
        let sym = self.interner.intern(&value.to_string())?;
        Ok(Token {
            kind: TokenKind::IntegerLit(sym),
            span,
        })
    }

    /// Interns `word`, checks `max_word_length`, and classifies its [`WordRole`].
    ///
    /// # Arguments
    ///
    /// * `word` - Raw COBOL word text.
    /// * `area` - Source margin zone.
    /// * `span` - Source span.
    fn make_word_token(&mut self, word: &str, area: MarginZone, span: SourceSpan) -> Result<Token> {
        let max_len = usize::from(self.dialect.max_word_length);
        if word.len() > max_len {
            if self.dialect.relaxed_syntax == RelaxedSyntaxMode::Relaxed {
                self.sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::DialectExtension,
                    Some(span),
                    format!(
                        "word `{word}` length {} exceeds dialect limit {max_len}",
                        word.len()
                    ),
                ));
            } else {
                self.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0002Lexical,
                    Some(span),
                    format!(
                        "word `{word}` length {} exceeds maximum {max_len}",
                        word.len()
                    ),
                ));
            }
        }
        let sym = self.interner.intern(word)?;
        let upper = word.to_ascii_uppercase();
        let role = if self.dialect.reserved_words.contains(&upper) {
            WordRole::ReservedKeyword
        } else {
            WordRole::ContextSoftWord
        };
        Ok(Token {
            kind: TokenKind::Word { sym, area, role },
            span,
        })
    }

    /// Decodes a `PpTokenKind::StringLiteral` into `TokenKind::StringLit` across all 10 [`StringPrefix`] kinds.
    ///
    /// # Arguments
    ///
    /// * `raw` - Raw literal token text including prefix and quotes.
    /// * `span` - Source span of the literal.
    fn decode_string_literal(&mut self, raw: &str, span: SourceSpan) -> Token {
        let (prefix, prefix_len) = parse_string_prefix(raw);
        self.check_string_prefix_dialect(prefix, span);

        let rest = raw.get(prefix_len..).unwrap_or("");
        let quote_ch = rest.chars().next().unwrap_or(DQ);
        let closed = rest.len() >= 2 && rest.ends_with(quote_ch);
        if !closed {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                String::from("unterminated string literal"),
            ));
        }
        let inner_end = if closed {
            rest.len() - quote_ch.len_utf8()
        } else {
            rest.len()
        };
        let body = rest.get(quote_ch.len_utf8()..inner_end).unwrap_or("");

        let bytes = match prefix {
            StringPrefix::Plain => {
                let unescaped = unescape_quotes(body, quote_ch);
                self.check_alnum_literal_length(unescaped.len(), span);
                unescaped.into_bytes()
            }
            StringPrefix::NullTermZ => {
                let mut b = unescape_quotes(body, quote_ch).into_bytes();
                b.push(0);
                b
            }
            StringPrefix::Utf8U => unescape_quotes(body, quote_ch).into_bytes(),
            StringPrefix::NationalN => {
                let unescaped = unescape_quotes(body, quote_ch);
                let mut utf16 = Vec::with_capacity(unescaped.len() * 2);
                for unit in unescaped.encode_utf16() {
                    utf16.extend_from_slice(&unit.to_be_bytes());
                }
                utf16
            }
            StringPrefix::HexX => self.decode_hex_pairs(body, span, false),
            StringPrefix::HexH | StringPrefix::BitHexBX => self.decode_hex_pairs(body, span, true),
            StringPrefix::NationalHexNX => self.decode_national_hex(body, span),
            StringPrefix::Utf8HexUX => self.decode_utf8_hex(body, span),
            StringPrefix::BitB => self.decode_bit_string(body, span),
        };

        Token {
            kind: TokenKind::StringLit { bytes, prefix },
            span,
        }
    }

    /// Validates dialect support for prefixed literals (`Z`, `N`, `NX`, `B`, `BX`, `H`, `U`, `UX`).
    ///
    /// # Arguments
    ///
    /// * `prefix` - Parsed [`StringPrefix`].
    /// * `span` - Source span of the literal.
    fn check_string_prefix_dialect(&mut self, prefix: StringPrefix, span: SourceSpan) {
        let (rule_key, label) = match prefix {
            StringPrefix::Plain | StringPrefix::HexX => return,
            StringPrefix::HexH => ("hexadecimal-h-literals", "`H'...'` hexadecimal literal"),
            StringPrefix::NullTermZ => ("Null-terminated-literals", "`Z'...'` literal"),
            StringPrefix::BitB => ("binary-literals", "`B'...'` binary literal"),
            StringPrefix::BitHexBX => ("hexadecimal-boolean-literals", "`BX'...'` literal"),
            StringPrefix::NationalN => ("national-literals", "`N'...'` national literal"),
            StringPrefix::NationalHexNX => (
                "hexadecimal-national-literals",
                "`NX'...'` national hex literal",
            ),
            StringPrefix::Utf8U => ("utf8-literals", "`U'...'` UTF-8 literal"),
            StringPrefix::Utf8HexUX => ("hexadecimal-utf8-literals", "`UX'...'` UTF-8 hex literal"),
        };
        let default_disp = if self.dialect.base_dialect == Dialect::Cobol85 {
            FeatureDisposition::Unconformable
        } else {
            FeatureDisposition::Ok
        };
        let disp = self
            .dialect
            .feature_rules
            .get(rule_key)
            .copied()
            .unwrap_or(default_disp);
        emit_feature_disposition(self.sink, disp, span, label);
    }

    /// Checks plain literal byte length (zero-length and upper bound).
    ///
    /// # Arguments
    ///
    /// * `byte_len` - Unescaped byte count.
    /// * `span` - Source span of the literal.
    fn check_alnum_literal_length(&mut self, byte_len: usize, span: SourceSpan) {
        if byte_len == 0 && self.dialect.base_dialect == Dialect::Cobol85 {
            let disp = self
                .dialect
                .feature_rules
                .get("zero-length-literals")
                .copied()
                .unwrap_or(FeatureDisposition::Unconformable);
            emit_feature_disposition(self.sink, disp, span, "zero-length literal");
        }
        let max_len = if self.dialect.base_dialect == Dialect::Cobol85 {
            160_usize
        } else {
            8191_usize
        };
        if byte_len > max_len {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("alphanumeric literal length {byte_len} exceeds maximum {max_len}"),
            ));
        }
    }

    /// Decodes hex digits in `body` into bytes, optionally padding an odd leading nibble (`H`/`BX`).
    ///
    /// # Arguments
    ///
    /// * `body` - Inner literal characters between quotes.
    /// * `span` - Source span for error diagnostics.
    /// * `allow_odd_nibbles` - Whether odd hex digit counts are left-padded with `0`.
    fn decode_hex_pairs(
        &mut self,
        body: &str,
        span: SourceSpan,
        allow_odd_nibbles: bool,
    ) -> Vec<u8> {
        let Some(nibbles) = collect_hex_nibbles(body) else {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("invalid hexadecimal digit in literal `{body}`"),
            ));
            return Vec::new();
        };
        if nibbles.is_empty() {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                String::from("hexadecimal literal cannot be empty"),
            ));
            return Vec::new();
        }
        if nibbles.len() % 2 != 0 && !allow_odd_nibbles {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("hexadecimal literal `{body}` must contain an even number of hex digits"),
            ));
        }
        pack_nibbles(&nibbles)
    }

    /// Decodes `NX"..."` UTF-16BE hexadecimal literals (4 hex digits per code unit).
    ///
    /// # Arguments
    ///
    /// * `body` - Inner hex characters.
    /// * `span` - Source span for error diagnostics.
    fn decode_national_hex(&mut self, body: &str, span: SourceSpan) -> Vec<u8> {
        let Some(nibbles) = collect_hex_nibbles(body) else {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("invalid hexadecimal digit in national literal `{body}`"),
            ));
            return Vec::new();
        };
        if nibbles.is_empty() || nibbles.len() % 4 != 0 {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!(
                    "national hex literal `{body}` must contain a non-zero multiple of 4 hex digits"
                ),
            ));
        }
        pack_nibbles(&nibbles)
    }

    /// Decodes `UX"..."` UTF-8 hexadecimal literals and verifies UTF-8 validity.
    ///
    /// # Arguments
    ///
    /// * `body` - Inner hex characters.
    /// * `span` - Source span for error diagnostics.
    fn decode_utf8_hex(&mut self, body: &str, span: SourceSpan) -> Vec<u8> {
        let bytes = self.decode_hex_pairs(body, span, false);
        if core::str::from_utf8(&bytes).is_err() {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("UTF-8 hex literal `{body}` does not form valid UTF-8"),
            ));
        }
        bytes
    }

    /// Decodes `B"0101"` binary bit-string literals into packed bytes.
    ///
    /// # Arguments
    ///
    /// * `body` - Inner `'0'`/`'1'` bit characters.
    /// * `span` - Source span for error diagnostics.
    fn decode_bit_string(&mut self, body: &str, span: SourceSpan) -> Vec<u8> {
        if body.is_empty() || !body.bytes().all(|b| b == b'0' || b == b'1') {
            self.sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("invalid binary literal `{body}` (expected `'0'` and `'1'` digits)"),
            ));
            return Vec::new();
        }
        let mut out = Vec::with_capacity(body.len().div_ceil(8));
        let pad = (8 - (body.len() % 8)) % 8;
        let mut acc = 0_u8;
        let mut bit_count = pad;
        for b in body.bytes() {
            acc = (acc << 1) | u8::from(b == b'1');
            bit_count += 1;
            if bit_count == 8 {
                out.push(acc);
                acc = 0;
                bit_count = 0;
            }
        }
        out
    }
}

/// Checks whether `pp` begins a new division or `IDENTIFICATION DIVISION` paragraph ending a comment entry.
///
/// # Arguments
///
/// * `pp` - Candidate [`PpToken`] while in [`LexerMode::IdCommentParagraph`].
#[must_use]
fn should_exit_id_comment(pp: &PpToken) -> bool {
    if pp.kind != PpTokenKind::Word {
        return false;
    }
    let at_header = pp.zone == MarginZone::AreaA
        || (pp.zone == MarginZone::FreeFlow && pp.spacing == SpacingHint::Newline);
    if !at_header {
        return false;
    }
    matches!(
        pp.text.to_ascii_uppercase().as_str(),
        "PROGRAM-ID"
            | "FUNCTION-ID"
            | "CLASS-ID"
            | "INTERFACE-ID"
            | "FACTORY"
            | "OBJECT"
            | "METHOD-ID"
            | "OPTIONS"
            | "AUTHOR"
            | "INSTALLATION"
            | "DATE-WRITTEN"
            | "DATE-COMPILED"
            | "SECURITY"
            | "REMARKS"
            | "ENVIRONMENT"
            | "DATA"
            | "PROCEDURE"
            | "IDENTIFICATION"
            | "ID"
            | "END"
    )
}

/// Emits a warning or error into `sink` according to a `.conf` [`FeatureDisposition`].
///
/// # Arguments
///
/// * `sink` - Target diagnostic sink.
/// * `disp` - Configured feature disposition.
/// * `span` - Source span of the construct.
/// * `label` - Human-readable feature description.
fn emit_feature_disposition(
    sink: &mut DiagnosticSink,
    disp: FeatureDisposition,
    span: SourceSpan,
    label: &str,
) {
    match disp {
        FeatureDisposition::Ok | FeatureDisposition::Ignore => {}
        FeatureDisposition::Warning => {
            sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::DialectExtension,
                Some(span),
                format!("{label} used"),
            ));
        }
        FeatureDisposition::Archaic => {
            sink.emit(Diagnostic::warning(
                DiagnosticCode::W0001Archaic,
                WarningGroup::Archaic,
                Some(span),
                format!("{label} is archaic"),
            ));
        }
        FeatureDisposition::Obsolete => {
            sink.emit(Diagnostic::warning(
                DiagnosticCode::W0002Obsolete,
                WarningGroup::Obsolete,
                Some(span),
                format!("{label} is obsolete"),
            ));
        }
        FeatureDisposition::Error => {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0002Lexical,
                Some(span),
                format!("{label} is not permitted"),
            ));
        }
        FeatureDisposition::Unconformable => {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0009DialectUnconformable,
                Some(span),
                format!("{label} does not conform to active dialect"),
            ));
        }
    }
}

/// Matches an arithmetic, relational, or concatenation operator starting at `bytes[idx]`.
///
/// # Arguments
///
/// * `bytes` - Raw slice bytes.
/// * `idx` - Starting byte index.
#[must_use]
fn match_operator_at(bytes: &[u8], idx: usize) -> Option<(TokenKind, usize)> {
    let b0 = *bytes.get(idx)?;
    let b1 = bytes.get(idx + 1).copied();
    match (b0, b1) {
        (b'*', Some(b'*')) => Some((TokenKind::StarStar, 2)),
        (b'<', Some(b'=')) => Some((TokenKind::Le, 2)),
        (b'>', Some(b'=')) => Some((TokenKind::Ge, 2)),
        (b'<', Some(b'>')) | (b'!', Some(b'=')) => Some((TokenKind::Ne, 2)),
        (b'+', _) => Some((TokenKind::Plus, 1)),
        (b'-', _) => Some((TokenKind::Minus, 1)),
        (b'*', _) => Some((TokenKind::Star, 1)),
        (b'/', _) => Some((TokenKind::Slash, 1)),
        (b'=', _) => Some((TokenKind::Eq, 1)),
        (b'<', _) => Some((TokenKind::Lt, 1)),
        (b'>', _) => Some((TokenKind::Gt, 1)),
        (b'&', _) => Some((TokenKind::Ampersand, 1)),
        _ => None,
    }
}

/// Checks if `bytes[idx..]` begins a signed numeric literal (`+123`, `-4.5`, `-.5`) and returns its end index.
///
/// # Arguments
///
/// * `bytes` - Raw slice bytes.
/// * `idx` - Index of `+` or `-`.
#[must_use]
fn scan_signed_numeric_end(bytes: &[u8], idx: usize) -> Option<usize> {
    scan_unsigned_numeric_end(bytes, idx + 1)
}

/// Checks if `bytes[idx..]` begins an unsigned numeric/decimal/float literal (not a hyphenated word like `000-MAIN`).
///
/// # Arguments
///
/// * `bytes` - Raw slice bytes.
/// * `idx` - Candidate start index.
#[must_use]
fn scan_unsigned_numeric_end(bytes: &[u8], idx: usize) -> Option<usize> {
    let first = *bytes.get(idx)?;
    if first == b'.' && !bytes.get(idx + 1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    if first != b'.' && !first.is_ascii_digit() {
        return None;
    }

    let mut pos = idx;
    while let Some(&b) = bytes.get(pos) {
        if b.is_ascii_digit() || b == b'.' {
            pos += 1;
        } else if b == b'E' || b == b'e' {
            let after_e = bytes.get(pos + 1).copied();
            if matches!(after_e, Some(b'+' | b'-')) {
                pos += 2;
                while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                    pos += 1;
                }
                break;
            } else if after_e.is_some_and(|d| d.is_ascii_digit() || d == b'.') {
                pos += 1;
                while bytes
                    .get(pos)
                    .is_some_and(|&d| d.is_ascii_digit() || d == b'.')
                {
                    pos += 1;
                }
                break;
            } else if bytes.get(pos + 1).is_none() {
                pos += 1;
                break;
            }
            return None;
        } else {
            break;
        }
    }

    if let Some(&next_b) = bytes.get(pos)
        && (is_word_start_byte(next_b)
            || (next_b == b'-' && bytes.get(pos + 1).is_some_and(|&c| is_word_char(c))))
    {
        return None;
    }
    Some(pos)
}

/// Returns `true` when `b` can start a COBOL word (`A-Z`, `a-z`, `0-9`, `_`, `$`, `#`).
///
/// # Arguments
///
/// * `b` - Candidate ASCII byte.
#[must_use]
const fn is_word_start_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'#'
}

/// Returns `true` when `b` can continue a COBOL word (`A-Z`, `a-z`, `0-9`, `-`, `_`, `$`, `#`).
///
/// # Arguments
///
/// * `b` - Candidate ASCII byte.
#[must_use]
const fn is_word_char(b: u8) -> bool {
    is_word_start_byte(b) || b == b'-'
}

/// Scans until the end of a COBOL word starting at `bytes[idx]`.
///
/// # Arguments
///
/// * `bytes` - Raw slice bytes.
/// * `idx` - Start index.
#[must_use]
fn scan_cobol_word_end(bytes: &[u8], idx: usize) -> usize {
    let mut pos = idx;
    while bytes.get(pos).is_some_and(|&b| is_word_char(b)) {
        pos += 1;
    }
    pos
}

/// Computes a sub-span within `base` starting at relative byte `rel_start` with byte length `len`.
///
/// # Arguments
///
/// * `base` - Enclosing token span.
/// * `rel_start` - Zero-based byte offset inside `base`.
/// * `len` - Sub-token byte length.
#[must_use]
fn slice_span(base: SourceSpan, rel_start: usize, len: usize) -> SourceSpan {
    let add = u32::try_from(rel_start).unwrap_or(0);
    let sub_len = u32::try_from(len).unwrap_or(0);
    let start = ByteOffset::new(base.start.as_u32().saturating_add(add));
    SourceSpan::new(
        base.file_id,
        start,
        ByteLength::new(sub_len),
        base.expansion_id,
    )
}

/// Counts occurrences of ASCII `needle` in `s`.
///
/// # Arguments
///
/// * `s` - Input string.
/// * `needle` - Target character.
#[must_use]
fn count_char(s: &str, needle: char) -> usize {
    s.chars().filter(|&c| c == needle).count()
}

/// Parses the [`StringPrefix`] and prefix byte length from a raw string literal token.
///
/// # Arguments
///
/// * `raw` - Raw string literal token text.
#[must_use]
fn parse_string_prefix(raw: &str) -> (StringPrefix, usize) {
    let mut chars = raw.chars();
    let c0 = chars.next().unwrap_or(DQ).to_ascii_uppercase();
    let c1 = chars.next().unwrap_or(DQ).to_ascii_uppercase();
    match (c0, c1) {
        ('N', 'X') => (StringPrefix::NationalHexNX, 2),
        ('U', 'X') => (StringPrefix::Utf8HexUX, 2),
        ('B', 'X') => (StringPrefix::BitHexBX, 2),
        ('X', _) => (StringPrefix::HexX, 1),
        ('H', _) => (StringPrefix::HexH, 1),
        ('Z', _) => (StringPrefix::NullTermZ, 1),
        ('B', _) => (StringPrefix::BitB, 1),
        ('N', _) => (StringPrefix::NationalN, 1),
        ('U', _) => (StringPrefix::Utf8U, 1),
        _ => (StringPrefix::Plain, 0),
    }
}

/// Collapses doubled `quote_ch` escape pairs (`""` -> `"`, `''` -> `'`) inside `body`.
///
/// # Arguments
///
/// * `body` - Inner string literal body between outer quotes.
/// * `quote_ch` - Active delimiter character.
#[must_use]
fn unescape_quotes(body: &str, quote_ch: char) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == quote_ch && chars.peek() == Some(&quote_ch) {
            let _ = chars.next();
        }
    }
    out
}

/// Converts ASCII hex digits in `body` into `0..=15` nibble values, returning `None` on invalid chars.
///
/// # Arguments
///
/// * `body` - Raw hex literal interior.
#[must_use]
fn collect_hex_nibbles(body: &str) -> Option<Vec<u8>> {
    let mut nibbles = Vec::with_capacity(body.len());
    for b in body.bytes() {
        let val = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return None,
        };
        nibbles.push(val);
    }
    Some(nibbles)
}

/// Packs 4-bit nibbles into big-endian bytes, left-padding with a zero nibble if odd length.
///
/// # Arguments
///
/// * `nibbles` - Slice of `0..=15` nibble values.
#[must_use]
fn pack_nibbles(nibbles: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(nibbles.len().div_ceil(2));
    let mut idx = 0;
    if let Some(&first) = nibbles.first().filter(|_| nibbles.len() % 2 == 1) {
        bytes.push(first);
        idx = 1;
    }
    while let (Some(&hi), Some(&lo)) = (nibbles.get(idx), nibbles.get(idx + 1)) {
        bytes.push((hi << 4) | lo);
        idx += 2;
    }
    bytes
}
