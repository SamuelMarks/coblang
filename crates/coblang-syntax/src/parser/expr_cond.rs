//! Pratt expression, `DataRef` subscript/refmod, and ANSI-85 abbreviated condition parser.

use coblang_common::{Result, SourceSpan};

use crate::ast::data_div::ValueAtom;
use crate::ast::proc_div::{
    AbbrevLogicOp, AbbrevRhs, BinaryOp, CallArg, ClassCheckKind, Condition, DataRef,
    DataRefModifier, Expr, FunctionCallSyntax, NegationMode, PassingMode, QualifiedName,
    RefModSpec, RelOp, SignCheckKind, SubscriptItem, TruthValue, UnaryOp,
};
use crate::parser::Parser;
use crate::parser::data::match_figurative_word;
use crate::token::{LiteralSign, TokenKind};

impl Parser<'_> {
    /// Parses a qualified name (`base [{IN | OF} qual ...]`).
    pub(crate) fn parse_qualified_name(&mut self) -> Result<QualifiedName> {
        self.tick()?;
        let start = self.current_span();
        let (base, mut end_span) = if let Some(pair) = self.eat_any_word_or_string_sym() {
            pair
        } else {
            self.emit_syntax_error(start, "expected identifier");
            let stop_for_rparen = self.paren_depth > 0 && self.peek().kind == TokenKind::RParen;
            if !self.at_eof() && !self.at_period() && !stop_for_rparen {
                self.advance();
            }
            (self.interner.intern("__MISSING_IDENT")?, start)
        };
        let mut qualifiers = Vec::new();
        while self.consume_word("IN") || self.consume_word("OF") {
            if let Some((q, q_span, _)) = self.eat_word() {
                qualifiers.push(q);
                end_span = q_span;
            } else {
                self.emit_syntax_error(self.current_span(), "expected qualifier after `IN`/`OF`");
                break;
            }
        }
        Ok(QualifiedName {
            base,
            qualifiers,
            span: start.merge(end_span),
        })
    }

    /// Parses a [`DataRef`] (`[ADDRESS OF | LENGTH OF] base [IN/OF ...] [(subs...)] [(off:len)]`).
    pub(crate) fn parse_data_ref(&mut self) -> Result<DataRef> {
        let start = self.current_span();
        let modifier = if self.at_word("ADDRESS") && self.peek_word_at(1, "OF") {
            self.advance();
            self.advance();
            DataRefModifier::AddressOf
        } else if (self.at_word("LENGTH") || self.at_word("BYTE-LENGTH"))
            && self.peek_word_at(1, "OF")
        {
            self.advance();
            self.advance();
            DataRefModifier::LengthOf
        } else {
            DataRefModifier::Plain
        };

        let name = self.parse_qualified_name()?;
        let mut subscripts = Vec::new();
        let mut refmod = None;
        let mut end_span = name.span;

        if !self.preceded_by_separator() && self.peek().kind == TokenKind::LParen {
            end_span = self.parse_data_ref_parens(&mut subscripts, &mut refmod)?;
        }

        if refmod.is_none()
            && !self.preceded_by_separator()
            && self.peek().kind == TokenKind::LParen
        {
            let rm = self.parse_explicit_refmod()?;
            end_span = rm.span;
            refmod = Some(rm);
        }

        Ok(DataRef {
            modifier,
            name,
            subscripts,
            refmod,
            span: start.merge(end_span),
        })
    }

    /// Parses the first `(...)` group after a data-name (subscripts, `ALL`, or `(offset:length)`).
    ///
    /// # Arguments
    ///
    /// * `subscripts` - Output accumulator for parsed subscript items.
    /// * `refmod` - Output slot populated when the parentheses encode `(offset:length)`.
    fn parse_data_ref_parens(
        &mut self,
        subscripts: &mut Vec<SubscriptItem>,
        refmod: &mut Option<RefModSpec>,
    ) -> Result<SourceSpan> {
        let paren_start = self.current_span();
        self.enter_paren();
        if self.at_word("ALL") {
            let all_sp = self.current_span();
            self.advance();
            subscripts.push(SubscriptItem::AllWildcard(all_sp));
            while !self.at_eof() && !self.at_period() && self.peek().kind != TokenKind::RParen {
                if self.at_word("ALL") {
                    let sp = self.current_span();
                    self.advance();
                    subscripts.push(SubscriptItem::AllWildcard(sp));
                } else {
                    match self.parse_expr() {
                        Ok(e) => subscripts.push(SubscriptItem::Expr(e)),
                        Err(err) => {
                            self.paren_depth = self.paren_depth.saturating_sub(1);
                            return Err(err);
                        }
                    }
                }
            }
            Ok(self.exit_paren())
        } else if self.peek().kind == TokenKind::RParen {
            self.emit_syntax_error(paren_start, "empty subscript `()` is not permitted");
            Ok(self.exit_paren())
        } else {
            let first_expr = match self.parse_expr() {
                Ok(e) => e,
                Err(err) => {
                    self.paren_depth = self.paren_depth.saturating_sub(1);
                    return Err(err);
                }
            };
            if self.peek().kind == TokenKind::Colon {
                self.advance();
                let length =
                    if self.peek().kind != TokenKind::RParen && !self.at_period() && !self.at_eof()
                    {
                        match self.parse_expr() {
                            Ok(e) => Some(Box::new(e)),
                            Err(err) => {
                                self.paren_depth = self.paren_depth.saturating_sub(1);
                                return Err(err);
                            }
                        }
                    } else {
                        None
                    };
                let end_sp = self.exit_paren();
                *refmod = Some(RefModSpec {
                    offset: Box::new(first_expr),
                    length,
                    span: paren_start.merge(end_sp),
                });
                Ok(end_sp)
            } else {
                subscripts.push(SubscriptItem::Expr(first_expr));
                while !self.at_eof() && !self.at_period() && self.peek().kind != TokenKind::RParen {
                    if self.consume_word("ALL") {
                        subscripts.push(SubscriptItem::AllWildcard(self.current_span()));
                    } else {
                        match self.parse_expr() {
                            Ok(e) => subscripts.push(SubscriptItem::Expr(e)),
                            Err(err) => {
                                self.paren_depth = self.paren_depth.saturating_sub(1);
                                return Err(err);
                            }
                        }
                    }
                }
                Ok(self.exit_paren())
            }
        }
    }

    /// Parses a trailing `(offset : [length])` reference modification slice.
    pub(crate) fn parse_explicit_refmod(&mut self) -> Result<RefModSpec> {
        let start = self.current_span();
        self.enter_paren();
        let offset = match self.parse_expr() {
            Ok(e) => e,
            Err(err) => {
                self.paren_depth = self.paren_depth.saturating_sub(1);
                return Err(err);
            }
        };
        if self.peek().kind == TokenKind::Colon {
            self.advance();
        } else {
            self.emit_syntax_error(
                self.current_span(),
                "expected `:` in reference modification `(offset : [length])`",
            );
        }
        let length = if self.peek().kind != TokenKind::RParen && !self.at_period() && !self.at_eof()
        {
            match self.parse_expr() {
                Ok(e) => Some(Box::new(e)),
                Err(err) => {
                    self.paren_depth = self.paren_depth.saturating_sub(1);
                    return Err(err);
                }
            }
        } else {
            None
        };
        let end = self.exit_paren();
        Ok(RefModSpec {
            offset: Box::new(offset),
            length,
            span: start.merge(end),
        })
    }

    /// Consumes `)` if present or emits a syntax error.
    pub(crate) fn consume_rparen(&mut self) -> SourceSpan {
        let sp = self.current_span();
        if self.peek().kind == TokenKind::RParen {
            self.advance();
        } else {
            self.emit_syntax_error(sp, "expected closing `)`");
        }
        sp
    }

    /// Parses an arithmetic/string/intrinsic COBOL [`Expr`] using Pratt operator precedence.
    pub(crate) fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_expr_bp(0)
    }

    /// Pratt expression loop with minimum binding power `min_bp`.
    ///
    /// # Arguments
    ///
    /// * `min_bp` - Minimum infix binding power.
    fn parse_expr_bp(&mut self, min_bp: u8) -> Result<Expr> {
        self.tick()?;
        self.enter_depth()?;
        let res = self.parse_expr_bp_inner(min_bp);
        self.exit_depth();
        res
    }

    /// Inner Pratt expression loop called inside [`Self::parse_expr_bp`]'s recursion guard.
    ///
    /// # Arguments
    ///
    /// * `min_bp` - Minimum infix binding power.
    fn parse_expr_bp_inner(&mut self, min_bp: u8) -> Result<Expr> {
        let mut lhs = self.parse_prefix_expr()?;

        loop {
            self.skip_separators();
            if self.in_fn_arg_list > 0 && self.preceded_by_separator() {
                break;
            }
            if self.peek().kind == TokenKind::DoubleColon {
                self.advance();
                let (method, m_span) = self
                    .eat_any_word_or_string_sym()
                    .unwrap_or((lhs_dummy_sym(self)?, self.current_span()));
                let mut args = Vec::new();
                let mut end_sp = m_span;
                if self.peek().kind == TokenKind::LParen {
                    self.enter_paren();
                    while !self.at_eof()
                        && !self.at_period()
                        && self.peek().kind != TokenKind::RParen
                    {
                        match self.parse_single_call_arg(PassingMode::ByReference) {
                            Ok(arg) => args.push(arg),
                            Err(err) => {
                                self.paren_depth = self.paren_depth.saturating_sub(1);
                                return Err(err);
                            }
                        }
                    }
                    end_sp = self.exit_paren();
                }
                let span = lhs.span().merge(end_sp);
                lhs = Expr::InlineInvoke {
                    receiver: Box::new(lhs),
                    method,
                    args,
                    span,
                };
                continue;
            }

            if let Some((op, l_bp, r_bp)) = self.peek_infix_op() {
                if l_bp < min_bp {
                    break;
                }
                self.advance();
                let rhs = self.parse_expr_bp(r_bp)?;
                let span = lhs.span().merge(rhs.span());
                lhs = Expr::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    span,
                };
                continue;
            }

            // Handle Tight-signed numeric tokens immediately following an operand (`Y +1` / `Y -1`).
            if 4 >= min_bp && self.peek_tight_signed_literal() {
                let rhs = self.parse_prefix_expr()?;
                let span = lhs.span().merge(rhs.span());
                lhs = Expr::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    span,
                };
                continue;
            }

            break;
        }

        Ok(lhs)
    }

    /// Parses a prefix/primary [`Expr`] (`+e`, `-e`, `(e)`, `FUNCTION f(...)`, literal, or `DataRef`).
    pub(crate) fn parse_prefix_expr(&mut self) -> Result<Expr> {
        self.skip_separators();
        let start = self.current_span();
        match self.peek().kind.clone() {
            TokenKind::Plus => {
                self.advance();
                let operand = self.parse_expr_bp(8)?;
                let span = start.merge(operand.span());
                Ok(Expr::Unary {
                    op: UnaryOp::Plus,
                    operand: Box::new(operand),
                    span,
                })
            }
            TokenKind::Minus => {
                self.advance();
                let operand = self.parse_expr_bp(8)?;
                let span = start.merge(operand.span());
                Ok(Expr::Unary {
                    op: UnaryOp::Minus,
                    operand: Box::new(operand),
                    span,
                })
            }
            TokenKind::LParen => {
                let saved_fn_arg = self.in_fn_arg_list;
                self.in_fn_arg_list = 0;
                self.enter_paren();
                let inner = match self.parse_expr() {
                    Ok(e) => e,
                    Err(err) => {
                        self.in_fn_arg_list = saved_fn_arg;
                        self.paren_depth = self.paren_depth.saturating_sub(1);
                        return Err(err);
                    }
                };
                while !self.at_eof() && !self.at_period() && self.peek().kind != TokenKind::RParen {
                    if let Err(err) = self.parse_expr() {
                        self.in_fn_arg_list = saved_fn_arg;
                        self.paren_depth = self.paren_depth.saturating_sub(1);
                        return Err(err);
                    }
                }
                self.in_fn_arg_list = saved_fn_arg;
                let _ = self.exit_paren();
                Ok(inner)
            }
            TokenKind::StringLit { bytes, prefix } => {
                Ok(self.parse_string_lit_expr(bytes, prefix, start))
            }
            TokenKind::IntegerLit(sym) => {
                self.advance();
                Ok(Expr::Literal {
                    atom: ValueAtom::Integer(sym),
                    span: start,
                })
            }
            TokenKind::DecimalLit {
                digits,
                scale,
                sign,
            } => {
                self.advance();
                Ok(Expr::Literal {
                    atom: ValueAtom::Decimal {
                        digits,
                        scale,
                        sign,
                    },
                    span: start,
                })
            }
            TokenKind::FloatLit(sym) => {
                self.advance();
                Ok(Expr::Literal {
                    atom: ValueAtom::Float(sym),
                    span: start,
                })
            }
            TokenKind::Word { sym, .. } => self.parse_word_expr(sym, start),
            _ => {
                self.emit_syntax_error(start, "expected expression");
                let dummy = self.interner.intern("0")?;
                let stop_for_rparen = self.paren_depth > 0 && self.peek().kind == TokenKind::RParen;
                if !self.at_eof() && !self.at_period() && !stop_for_rparen {
                    self.advance();
                }
                Ok(Expr::Literal {
                    atom: ValueAtom::Integer(dummy),
                    span: start,
                })
            }
        }
    }

    /// Parses a string literal expression including any `'-` continuation segments.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Initial decoded byte payload of the opening string token.
    /// * `prefix` - String literal prefix kind (`None`, `X`, `N`, `Z`, `NX`, etc.).
    /// * `start` - Opening source span of the string literal.
    fn parse_string_lit_expr(
        &mut self,
        mut bytes: Vec<u8>,
        prefix: crate::StringPrefix,
        start: SourceSpan,
    ) -> Expr {
        self.advance();
        while self.peek().kind == TokenKind::Minus {
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
        Expr::Literal {
            atom: ValueAtom::String { bytes, prefix },
            span: start,
        }
    }

    /// Parses an expression starting with a `Word` token (`FUNCTION`, `ALL`, figurative constant, bare UDF/intrinsic, or `DataRef`).
    ///
    /// # Arguments
    ///
    /// * `sym` - Interned symbol ID of the word.
    /// * `start` - Opening span.
    fn parse_word_expr(
        &mut self,
        sym: coblang_common::SymbolId,
        start: SourceSpan,
    ) -> Result<Expr> {
        let w = self.sym_str(sym);
        if w == "FUNCTION" {
            return self.parse_explicit_function_call(start);
        }
        if w == "ALL" {
            let atom = self.parse_all_figurative_atom();
            return Ok(Expr::Literal {
                atom,
                span: start.merge(self.current_span()),
            });
        }
        if let Some(fig) = match_figurative_word(w) {
            self.advance();
            return Ok(Expr::Literal {
                atom: ValueAtom::Figurative(fig),
                span: start,
            });
        }
        if self.is_active_bare_function(sym)
            && !self.peek_word_at(1, "IN")
            && !self.peek_word_at(1, "OF")
        {
            return self.parse_bare_function_call(sym, start);
        }
        Ok(Expr::Data(self.parse_data_ref()?))
    }

    /// Returns `true` when `sym` is an imported bare UDF/intrinsic not shadowed by a `DATA DIVISION` entry.
    ///
    /// # Arguments
    ///
    /// * `sym` - Candidate word symbol.
    #[must_use]
    fn is_active_bare_function(&self, sym: coblang_common::SymbolId) -> bool {
        if self.declared_data_names.contains(&sym) {
            return false;
        }
        if self.bare_functions.contains(&sym) {
            return true;
        }
        let w = self.sym_str(sym);
        (self.bare_all_intrinsics == crate::parser::BareAllIntrinsics::Enabled
            || self.dialect.is_bare_intrinsic_imported(w))
            && is_known_intrinsic_name(w)
            && self.dialect.is_intrinsic_enabled(w)
    }

    /// Parses a bare `REPOSITORY` / `-fintrinsics=` function invocation (`name [(args...)] [(off:len)]`).
    ///
    /// # Arguments
    ///
    /// * `name` - Resolved function symbol.
    /// * `start` - Opening span of `name`.
    fn parse_bare_function_call(
        &mut self,
        name: coblang_common::SymbolId,
        start: SourceSpan,
    ) -> Result<Expr> {
        self.advance();
        self.finish_function_call_tail(name, FunctionCallSyntax::BareRepository, start, start)
    }

    /// Parses `FUNCTION name [( [arg ...] )] [(off:len)]`.
    ///
    /// # Arguments
    ///
    /// * `start` - Span of the `FUNCTION` keyword.
    fn parse_explicit_function_call(&mut self, start: SourceSpan) -> Result<Expr> {
        self.advance();
        let (name, end_span, _) = if let Some(triple) = self.eat_word() {
            triple
        } else {
            self.emit_syntax_error(start, "expected function name after `FUNCTION`");
            (
                self.interner.intern("__MISSING_FN")?,
                start,
                coblang_pp::MarginZone::AreaB,
            )
        };
        self.finish_function_call_tail(name, FunctionCallSyntax::ExplicitKeyword, start, end_span)
    }

    /// Parses optional `(args...)` and optional `(off:len)` suffix for a function call.
    ///
    /// # Arguments
    ///
    /// * `name` - Function symbol.
    /// * `syntax` - `ExplicitKeyword` vs `BareRepository`.
    /// * `start` - Opening span of the call expression.
    /// * `name_end` - Ending span of the function identifier.
    fn finish_function_call_tail(
        &mut self,
        name: coblang_common::SymbolId,
        syntax: FunctionCallSyntax,
        start: SourceSpan,
        mut name_end: SourceSpan,
    ) -> Result<Expr> {
        let mut args = Vec::new();
        if !self.preceded_by_separator()
            && self.peek().kind == TokenKind::LParen
            && !self.lparen_has_top_level_colon()
        {
            self.enter_paren();
            self.in_fn_arg_list = self.in_fn_arg_list.saturating_add(1);
            while !self.at_eof() && !self.at_period() && self.peek().kind != TokenKind::RParen {
                if self.at_word("ALL") {
                    let sp = self.current_span();
                    self.advance();
                    args.push(SubscriptItem::AllWildcard(sp));
                } else {
                    match self.parse_expr() {
                        Ok(e) => args.push(SubscriptItem::Expr(e)),
                        Err(err) => {
                            self.in_fn_arg_list = self.in_fn_arg_list.saturating_sub(1);
                            self.paren_depth = self.paren_depth.saturating_sub(1);
                            return Err(err);
                        }
                    }
                }
            }
            self.in_fn_arg_list = self.in_fn_arg_list.saturating_sub(1);
            name_end = self.exit_paren();
        }
        let refmod = if !self.preceded_by_separator() && self.peek().kind == TokenKind::LParen {
            let rm = self.parse_explicit_refmod()?;
            name_end = rm.span;
            Some(rm)
        } else {
            None
        };
        Ok(Expr::FunctionCall {
            name,
            syntax,
            args,
            refmod,
            span: start.merge(name_end),
        })
    }

    /// Returns `true` when `tokens[self.pos]` is `(` and contains a `:` at depth `1` before `)`.
    #[must_use]
    fn lparen_has_top_level_colon(&self) -> bool {
        if self.peek().kind != TokenKind::LParen {
            return false;
        }
        let mut idx = self.pos + 1;
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
                TokenKind::Colon if depth == 1 => return true,
                TokenKind::Period | TokenKind::Eof => return false,
                _ => {}
            }
            idx += 1;
        }
        false
    }

    /// Parses one argument inside `CALL` or inline `::` method invocation.
    ///
    /// # Arguments
    ///
    /// * `default_mode` - Active sticky `BY REFERENCE/CONTENT/VALUE` mode.
    pub(crate) fn parse_single_call_arg(&mut self, default_mode: PassingMode) -> Result<CallArg> {
        let _ = self.consume_word("BY");
        let mode = if self.consume_word("VALUE") {
            PassingMode::ByValue
        } else if self.consume_word("CONTENT") {
            PassingMode::ByContent
        } else if self.consume_word("REFERENCE") {
            PassingMode::ByReference
        } else {
            default_mode
        };
        if self.at_word("OMITTED") {
            let sp = self.current_span();
            self.advance();
            return Ok(CallArg::Omitted(sp));
        }
        let _ = self.consume_word("SIZE");
        let _ = self.consume_word("IS");
        let expr = self.parse_expr()?;
        Ok(CallArg::Value { mode, expr })
    }

    /// Peeks binary operator and `(left_bp, right_bp)` at current token.
    #[must_use]
    fn peek_infix_op(&self) -> Option<(BinaryOp, u8, u8)> {
        match self.peek().kind {
            TokenKind::Ampersand => Some((BinaryOp::Concat, 2, 3)),
            TokenKind::Plus => Some((BinaryOp::Add, 4, 5)),
            TokenKind::Minus => Some((BinaryOp::Sub, 4, 5)),
            TokenKind::Star => Some((BinaryOp::Mul, 6, 7)),
            TokenKind::Slash => Some((BinaryOp::Div, 6, 7)),
            TokenKind::StarStar => Some((BinaryOp::Pow, 10, 9)),
            _ => None,
        }
    }

    /// Returns `true` if the current token is an `IntegerLit`, `DecimalLit`, or `FloatLit` with explicit `+`/`-` sign.
    #[must_use]
    fn peek_tight_signed_literal(&self) -> bool {
        if self.in_fn_arg_list > 0 || self.preceded_by_separator() {
            return false;
        }
        match self.peek().kind {
            TokenKind::IntegerLit(sym) | TokenKind::FloatLit(sym) => {
                let s = self.sym_str(sym);
                s.starts_with('+') || s.starts_with('-')
            }
            TokenKind::DecimalLit { sign, .. } => sign != LiteralSign::Unsigned,
            _ => false,
        }
    }

    /// Parses a full boolean [`Condition`] (`OR` -> `AND` -> `NOT` -> relation/class/sign/condition-name).
    pub(crate) fn parse_condition(&mut self) -> Result<Condition> {
        let mut lhs = self.parse_and_condition()?;
        while self.consume_word("OR") {
            let rhs = self.parse_and_condition()?;
            let span = condition_span(&lhs).merge(condition_span(&rhs));
            lhs = Condition::Or {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    /// Parses `AND`-conjoined conditions.
    fn parse_and_condition(&mut self) -> Result<Condition> {
        let mut lhs = self.parse_not_condition()?;
        while self.consume_word("AND") {
            let rhs = self.parse_not_condition()?;
            let span = condition_span(&lhs).merge(condition_span(&rhs));
            lhs = Condition::And {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    /// Parses optional leading `NOT` condition negation.
    fn parse_not_condition(&mut self) -> Result<Condition> {
        if self.at_word("NOT") {
            self.enter_depth()?;
            let start = self.current_span();
            self.advance();
            let inner_res = self.parse_not_condition();
            self.exit_depth();
            let inner = inner_res?;
            let span = start.merge(condition_span(&inner));
            Ok(Condition::Not {
                inner: Box::new(inner),
                span,
            })
        } else {
            self.parse_primary_condition()
        }
    }

    /// Parses a primary condition (`(cond)`, `TRUE`/`FALSE`, class/sign/omitted check, relation + abbreviated tail, or `88` ref).
    fn parse_primary_condition(&mut self) -> Result<Condition> {
        self.tick()?;
        self.skip_separators();
        if self.peek().kind == TokenKind::LParen && self.paren_contains_condition_syntax() {
            self.enter_depth()?;
            self.enter_paren();
            let cond_res = self.parse_condition();
            let _ = self.exit_paren();
            self.exit_depth();
            return cond_res;
        }

        if self.at_word("TRUE") {
            let sp = self.current_span();
            self.advance();
            return Ok(Condition::BooleanConst {
                value: TruthValue::True,
                span: sp,
            });
        }
        if self.at_word("FALSE") {
            let sp = self.current_span();
            self.advance();
            return Ok(Condition::BooleanConst {
                value: TruthValue::False,
                span: sp,
            });
        }

        let subject = self.parse_expr()?;
        let start = subject.span();
        self.parse_post_subject_condition(subject, start)
    }

    /// Parses `[IS] [NOT] {OMITTED | Sign | Class | RelOp | CustomClass}` after `subject`, or falls back.
    ///
    /// # Arguments
    ///
    /// * `subject` - Parsed left-hand / subject expression.
    /// * `start` - Start span of `subject`.
    fn parse_post_subject_condition(
        &mut self,
        subject: Expr,
        start: SourceSpan,
    ) -> Result<Condition> {
        let saved_pos = self.pos;
        let had_is = self.consume_word("IS");
        let polarity = if self.consume_word("NOT") {
            NegationMode::Negated
        } else {
            NegationMode::Positive
        };

        if self.consume_word("OMITTED")
            && let Expr::Data(target) = subject
        {
            return Ok(Condition::OmittedCheck {
                target,
                polarity,
                span: start.merge(self.current_span()),
            });
        }

        if let Some(sign) = self.try_eat_sign_keyword()
            && !self.looks_like_rel_op_continuation()
        {
            return Ok(Condition::SignCheck {
                subject,
                polarity,
                sign,
                span: start.merge(self.current_span()),
            });
        }

        if let Some(class) = self.try_eat_builtin_class_keyword() {
            return Ok(Condition::ClassCheck {
                subject,
                polarity,
                class,
                span: start.merge(self.current_span()),
            });
        }

        if let Some(base_op) = self.try_eat_rel_op_without_not() {
            let op = if polarity == NegationMode::Negated {
                negate_rel_op(base_op)
            } else {
                base_op
            };
            let first_obj = self.parse_expr()?;
            let obj_sp = first_obj.span();
            let first_leaf = AbbrevRhs::Leaf {
                negated: NegationMode::Positive,
                op: None,
                object: first_obj,
                span: obj_sp,
            };
            let rhs = self.parse_abbrev_tail(first_leaf)?;
            return Ok(Condition::Relation {
                subject,
                op,
                rhs,
                span: start.merge(self.current_span()),
            });
        }

        // Custom `SPECIAL-NAMES` `CLASS` check: `expr [IS] [NOT] custom-class-name`!
        let next_is_declared_class = match self.peek().kind {
            TokenKind::Word { sym, .. } => self.declared_classes.contains(&sym),
            _ => false,
        };
        if (had_is || polarity == NegationMode::Negated || next_is_declared_class)
            && !is_condition_terminator(self.current_word())
            && let Some((cls_sym, cls_sp, _)) = self.eat_word()
        {
            return Ok(Condition::ClassCheck {
                subject,
                polarity,
                class: ClassCheckKind::Custom(cls_sym),
                span: start.merge(cls_sp),
            });
        }

        self.pos = saved_pos;
        self.fallback_condition_from_subject(subject, start)
    }

    /// Converts a standalone `Expr` with no trailing predicate into `ConditionRef` or `subject <> 0`.
    ///
    /// # Arguments
    ///
    /// * `subject` - Parsed expression.
    /// * `start` - Expression span.
    fn fallback_condition_from_subject(
        &mut self,
        subject: Expr,
        start: SourceSpan,
    ) -> Result<Condition> {
        if let Expr::Data(dref) = subject {
            Ok(Condition::ConditionRef(dref))
        } else {
            let zero_sym = self.interner.intern("0")?;
            let zero_expr = Expr::Literal {
                atom: ValueAtom::Integer(zero_sym),
                span: start,
            };
            Ok(Condition::Relation {
                subject,
                op: RelOp::Ne,
                rhs: AbbrevRhs::Leaf {
                    negated: NegationMode::Positive,
                    op: None,
                    object: zero_expr,
                    span: start,
                },
                span: start,
            })
        }
    }

    /// Parses optional ANSI-85 abbreviated `AND`/`OR` relation continuations (`IF A = B OR C AND NOT > D`).
    ///
    /// # Arguments
    ///
    /// * `first` - Initial right-hand leaf.
    fn parse_abbrev_tail(&mut self, first: AbbrevRhs) -> Result<AbbrevRhs> {
        let mut lhs = self.parse_abbrev_and_chain(first)?;
        while self.at_word("OR") && self.next_after_logic_is_abbreviated() {
            self.advance();
            let atom = self.parse_abbrev_atom()?;
            let rhs = self.parse_abbrev_and_chain(atom)?;
            lhs = AbbrevRhs::Binary {
                logic: AbbrevLogicOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    /// Parses `AND`-chained abbreviated relation leaves (binding tighter than `OR`).
    ///
    /// # Arguments
    ///
    /// * `first` - Left-hand abbreviated operand.
    fn parse_abbrev_and_chain(&mut self, mut first: AbbrevRhs) -> Result<AbbrevRhs> {
        while self.at_word("AND") && self.next_after_logic_is_abbreviated() {
            self.advance();
            let rhs = self.parse_abbrev_atom()?;
            first = AbbrevRhs::Binary {
                logic: AbbrevLogicOp::And,
                lhs: Box::new(first),
                rhs: Box::new(rhs),
            };
        }
        Ok(first)
    }

    /// Parses one abbreviated right-hand atom (`[NOT] [rel_op] expr`).
    fn parse_abbrev_atom(&mut self) -> Result<AbbrevRhs> {
        let start = self.current_span();
        let _ = self.consume_word("IS");
        let negated = if self.consume_word("NOT") {
            NegationMode::Negated
        } else {
            NegationMode::Positive
        };
        let op = self.try_eat_rel_op_without_not();
        let object = self.parse_expr()?;
        let span = start.merge(object.span());
        Ok(AbbrevRhs::Leaf {
            negated,
            op,
            object,
            span,
        })
    }

    /// Looks ahead past `AND`/`OR` to determine whether the following clause is an abbreviated relation.
    #[must_use]
    fn next_after_logic_is_abbreviated(&self) -> bool {
        let mut idx = self.pos + 1;
        while self
            .tokens
            .get(idx)
            .is_some_and(|t| matches!(t.kind, TokenKind::Comma | TokenKind::Semicolon))
        {
            idx += 1;
        }
        if self.word_at_raw(idx) == Some("IS") {
            idx += 1;
        }
        if self.word_at_raw(idx) == Some("NOT") {
            idx += 1;
        }
        // Case 1: Explicit relational operator immediately after `AND`/`OR [NOT]` (`OR > D`, `AND NOT = E`).
        if self.starts_rel_op_at_raw(idx) {
            return true;
        }
        // If `(` follows `AND`/`OR [NOT]`, treat as full parenthesized condition unless followed by nothing relational.
        if self
            .tokens
            .get(idx)
            .is_some_and(|t| t.kind == TokenKind::LParen)
        {
            return false;
        }
        if matches!(self.word_at_raw(idx), Some("TRUE" | "FALSE")) {
            return false;
        }
        // Scan past one simple expression candidate and check if a full condition operator follows it!
        let Some(after_expr) = self.scan_past_expr_at_raw(idx) else {
            return false;
        };
        !self.starts_condition_predicate_at_raw(after_expr)
    }

    /// Scans past a primary/arithmetic expression starting at `start_idx` for lookahead only.
    ///
    /// # Arguments
    ///
    /// * `start_idx` - Starting token index.
    #[must_use]
    fn scan_past_expr_at_raw(&self, start_idx: usize) -> Option<usize> {
        let tok = self.tokens.get(start_idx)?;
        if let TokenKind::Word { sym, .. } = tok.kind
            && is_condition_terminator(Some(self.sym_str(sym)))
        {
            return None;
        }
        let mut idx = start_idx;
        if matches!(tok.kind, TokenKind::Plus | TokenKind::Minus) {
            idx += 1;
        }
        let mut depth = 0_u32;
        let mut consumed = false;
        while let Some(t) = self.tokens.get(idx) {
            match t.kind {
                TokenKind::LParen => {
                    depth += 1;
                    consumed = true;
                    idx += 1;
                }
                TokenKind::RParen => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    idx += 1;
                }
                TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::StarStar
                | TokenKind::Ampersand
                | TokenKind::Colon
                | TokenKind::DoubleColon => {
                    if !consumed {
                        break;
                    }
                    idx += 1;
                }
                TokenKind::StringLit { .. }
                | TokenKind::IntegerLit(_)
                | TokenKind::DecimalLit { .. }
                | TokenKind::FloatLit(_) => {
                    consumed = true;
                    idx += 1;
                }
                TokenKind::Word { sym, .. } => {
                    let w = self.sym_str(sym);
                    if depth == 0
                        && consumed
                        && !matches!(w, "IN" | "OF")
                        && !matches!(
                            self.word_at_raw(idx.saturating_sub(1)),
                            Some("IN" | "OF" | "FUNCTION" | "ALL" | "LENGTH" | "ADDRESS")
                        )
                    {
                        break;
                    }
                    consumed = true;
                    idx += 1;
                }
                TokenKind::Comma | TokenKind::Semicolon => {
                    if depth == 0 {
                        break;
                    }
                    idx += 1;
                }
                _ => break,
            }
        }
        consumed.then_some(idx)
    }

    /// Checks whether `tokens[idx..]` begins a relational, `IS`, `NOT`, class, sign, or `OMITTED` predicate.
    ///
    /// # Arguments
    ///
    /// * `idx` - Token index immediately after a candidate subject expression.
    #[must_use]
    fn starts_condition_predicate_at_raw(&self, idx: usize) -> bool {
        if self.starts_rel_op_at_raw(idx) {
            return true;
        }
        matches!(
            self.word_at_raw(idx),
            Some(
                "IS" | "NOT"
                    | "NUMERIC"
                    | "ALPHABETIC"
                    | "ALPHABETIC-LOWER"
                    | "ALPHABETIC-UPPER"
                    | "POSITIVE"
                    | "NEGATIVE"
                    | "ZERO"
                    | "ZEROS"
                    | "ZEROES"
                    | "OMITTED"
            )
        )
    }

    /// Checks whether `tokens[idx]` starts a relational operator (`=`, `<`, `>`, `EQUAL`, `GREATER`, `LESS`).
    ///
    /// # Arguments
    ///
    /// * `idx` - Raw token index.
    #[must_use]
    fn starts_rel_op_at_raw(&self, idx: usize) -> bool {
        let Some(tok) = self.tokens.get(idx) else {
            return false;
        };
        match tok.kind {
            TokenKind::Eq
            | TokenKind::Ne
            | TokenKind::Lt
            | TokenKind::Le
            | TokenKind::Gt
            | TokenKind::Ge => true,
            TokenKind::Word { sym, .. } => {
                matches!(self.sym_str(sym), "EQUAL" | "EQUALS" | "GREATER" | "LESS")
            }
            _ => false,
        }
    }

    /// Returns the uppercase word at raw token index `idx`, if it is a `Word`.
    ///
    /// # Arguments
    ///
    /// * `idx` - Raw token index.
    #[must_use]
    fn word_at_raw(&self, idx: usize) -> Option<&str> {
        let tok = self.tokens.get(idx)?;
        if let TokenKind::Word { sym, .. } = tok.kind {
            Some(self.sym_str(sym))
        } else {
            None
        }
    }

    /// Checks if the current `(` ... `)` group is a parenthesized [`Condition`] rather than an arithmetic `(expr)`.
    #[must_use]
    fn paren_contains_condition_syntax(&self) -> bool {
        let mut idx = self.pos + 1;
        let mut depth = 1_u32;
        let mut has_cond_op = false;
        while let Some(tok) = self.tokens.get(idx) {
            match tok.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        idx += 1;
                        break;
                    }
                }
                TokenKind::Eq
                | TokenKind::Ne
                | TokenKind::Lt
                | TokenKind::Le
                | TokenKind::Gt
                | TokenKind::Ge => {
                    if depth == 1 {
                        has_cond_op = true;
                    }
                }
                TokenKind::Word { sym, .. } => {
                    if depth == 1
                        && matches!(
                            self.sym_str(sym),
                            "AND"
                                | "OR"
                                | "NOT"
                                | "IS"
                                | "EQUAL"
                                | "EQUALS"
                                | "GREATER"
                                | "LESS"
                                | "NUMERIC"
                                | "ALPHABETIC"
                                | "ALPHABETIC-LOWER"
                                | "ALPHABETIC-UPPER"
                                | "POSITIVE"
                                | "NEGATIVE"
                                | "OMITTED"
                                | "TRUE"
                                | "FALSE"
                        )
                    {
                        has_cond_op = true;
                    }
                }
                TokenKind::Eof | TokenKind::Period => break,
                _ => {}
            }
            idx += 1;
        }
        if has_cond_op {
            return true;
        }
        // `(COND-88)` is a condition ONLY if nothing arithmetic or relational follows `)`.
        let next_after = self.tokens.get(idx);
        !next_after.is_some_and(|t| {
            matches!(
                t.kind,
                TokenKind::Plus
                    | TokenKind::Minus
                    | TokenKind::Star
                    | TokenKind::Slash
                    | TokenKind::StarStar
                    | TokenKind::Ampersand
                    | TokenKind::Eq
                    | TokenKind::Ne
                    | TokenKind::Lt
                    | TokenKind::Le
                    | TokenKind::Gt
                    | TokenKind::Ge
            )
        }) && !self.starts_condition_predicate_at_raw(idx)
    }

    /// Consumes `POSITIVE`, `NEGATIVE`, or `ZERO`/`ZEROS`/`ZEROES` if present.
    fn try_eat_sign_keyword(&mut self) -> Option<SignCheckKind> {
        if self.consume_word("POSITIVE") {
            Some(SignCheckKind::Positive)
        } else if self.consume_word("NEGATIVE") {
            Some(SignCheckKind::Negative)
        } else if self.consume_word("ZERO")
            || self.consume_word("ZEROS")
            || self.consume_word("ZEROES")
        {
            Some(SignCheckKind::Zero)
        } else {
            None
        }
    }

    /// Checks if a binary arithmetic operator immediately follows (`IF X IS ZERO + 1` impossible, `IF X = ZERO + 1` handled).
    #[must_use]
    fn looks_like_rel_op_continuation(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Plus | TokenKind::Minus | TokenKind::Star | TokenKind::Slash
        )
    }

    /// Consumes built-in character-class keywords (`NUMERIC`, `ALPHABETIC*`, `KANJI`).
    fn try_eat_builtin_class_keyword(&mut self) -> Option<ClassCheckKind> {
        if self.consume_word("NUMERIC") {
            Some(ClassCheckKind::Numeric)
        } else if self.consume_word("ALPHABETIC-LOWER") {
            Some(ClassCheckKind::AlphabeticLower)
        } else if self.consume_word("ALPHABETIC-UPPER") {
            Some(ClassCheckKind::AlphabeticUpper)
        } else if self.consume_word("ALPHABETIC") {
            Some(ClassCheckKind::Alphabetic)
        } else if self.consume_word("KANJI") || self.consume_word("DBCS") {
            Some(ClassCheckKind::Kanji)
        } else {
            None
        }
    }

    /// Consumes a relational operator (`=`, `<>`, `<`, `<=`, `>`, `>=`, `EQUAL [TO]`, `GREATER [THAN] [OR EQUAL [TO]]`, `LESS [THAN] [OR EQUAL [TO]]`).
    pub(crate) fn try_eat_rel_op_without_not(&mut self) -> Option<RelOp> {
        self.skip_separators();
        match self.peek().kind {
            TokenKind::Eq => {
                self.advance();
                return Some(RelOp::Eq);
            }
            TokenKind::Ne => {
                self.advance();
                return Some(RelOp::Ne);
            }
            TokenKind::Lt => {
                self.advance();
                return Some(RelOp::Lt);
            }
            TokenKind::Le => {
                self.advance();
                return Some(RelOp::Le);
            }
            TokenKind::Gt => {
                self.advance();
                return Some(RelOp::Gt);
            }
            TokenKind::Ge => {
                self.advance();
                return Some(RelOp::Ge);
            }
            _ => {}
        }

        if self.consume_word("EQUAL") || self.consume_word("EQUALS") {
            let _ = self.consume_word("TO");
            Some(RelOp::Eq)
        } else if self.consume_word("GREATER") {
            let _ = self.consume_word("THAN");
            if self.consume_word("OR") {
                let _ = self.consume_word("EQUAL");
                let _ = self.consume_word("TO");
                Some(RelOp::Ge)
            } else {
                Some(RelOp::Gt)
            }
        } else if self.consume_word("LESS") {
            let _ = self.consume_word("THAN");
            if self.consume_word("OR") {
                let _ = self.consume_word("EQUAL");
                let _ = self.consume_word("TO");
                Some(RelOp::Le)
            } else {
                Some(RelOp::Lt)
            }
        } else {
            None
        }
    }
}

/// Fallback interned symbol when a method name is missing after `::`.
///
/// # Arguments
///
/// * `parser` - Active parser.
fn lhs_dummy_sym(parser: &mut Parser<'_>) -> Result<coblang_common::SymbolId> {
    parser.interner.intern("__MISSING_METHOD")
}

/// Negates a [`RelOp`] under `NOT <rel_op>`.
///
/// # Arguments
///
/// * `op` - Unnegated relational operator.
#[must_use]
const fn negate_rel_op(op: RelOp) -> RelOp {
    match op {
        RelOp::Eq => RelOp::Ne,
        RelOp::Ne => RelOp::Eq,
        RelOp::Lt => RelOp::Ge,
        RelOp::Ge => RelOp::Lt,
        RelOp::Gt => RelOp::Le,
        RelOp::Le => RelOp::Gt,
    }
}

/// Extracts the [`SourceSpan`] of a [`Condition`].
///
/// # Arguments
///
/// * `cond` - Condition reference.
#[must_use]
pub(crate) fn condition_span(cond: &Condition) -> SourceSpan {
    match cond {
        Condition::ConditionRef(d) => d.span,
        Condition::Relation { span, .. }
        | Condition::ClassCheck { span, .. }
        | Condition::SignCheck { span, .. }
        | Condition::OmittedCheck { span, .. }
        | Condition::BooleanConst { span, .. }
        | Condition::Not { span, .. }
        | Condition::And { span, .. }
        | Condition::Or { span, .. } => *span,
    }
}

/// Checks if `w` terminates a condition (`THEN`, `ELSE`, `END-*`, verb starters).
///
/// # Arguments
///
/// * `w` - Candidate uppercase word.
#[must_use]
fn is_condition_terminator(w: Option<&str>) -> bool {
    matches!(
        w,
        Some(
            "THEN"
                | "ELSE"
                | "END-IF"
                | "END-PERFORM"
                | "END-EVALUATE"
                | "END-SEARCH"
                | "WHEN"
                | "ALSO"
                | "MOVE"
                | "DISPLAY"
                | "COMPUTE"
                | "ADD"
                | "SUBTRACT"
                | "MULTIPLY"
                | "DIVIDE"
                | "PERFORM"
                | "GO"
                | "GOBACK"
                | "STOP"
                | "EXIT"
                | "SET"
                | "CALL"
                | "CONTINUE"
                | "NEXT"
        )
    )
}

/// Returns `true` when `name` is a known ISO 2023 or `GnuCOBOL` intrinsic function name.
///
/// # Arguments
///
/// * `name` - Canonical uppercase identifier.
#[must_use]
fn is_known_intrinsic_name(name: &str) -> bool {
    is_known_intrinsic_a_to_l(name) || is_known_intrinsic_m_to_z(name)
}

/// Checks ISO/dialect intrinsic names beginning with `A`..`L`.
///
/// # Arguments
///
/// * `name` - Canonical uppercase identifier.
#[must_use]
fn is_known_intrinsic_a_to_l(name: &str) -> bool {
    matches!(
        name,
        "ABS"
            | "ACOS"
            | "ANNUITY"
            | "ASIN"
            | "ATAN"
            | "BASECONVERT"
            | "BIT-OF"
            | "BIT-TO-CHAR"
            | "BOOLEAN-OF-INTEGER"
            | "BYTE-LENGTH"
            | "CHAR"
            | "CHAR-NATIONAL"
            | "COMBINED-DATETIME"
            | "CONCAT"
            | "CONCATENATE"
            | "CONTENT-LENGTH"
            | "CONTENT-OF"
            | "CONVERT"
            | "COS"
            | "CURRENCY-SYMBOL"
            | "CURRENT-DATE"
            | "DATE-OF-INTEGER"
            | "DATE-TO-YYYYMMDD"
            | "DAY-OF-INTEGER"
            | "DAY-TO-YYYYDDD"
            | "DISPLAY-OF"
            | "E"
            | "EXCEPTION-FILE"
            | "EXCEPTION-FILE-N"
            | "EXCEPTION-LOCATION"
            | "EXCEPTION-LOCATION-N"
            | "EXCEPTION-STATEMENT"
            | "EXCEPTION-STATUS"
            | "EXP"
            | "EXP10"
            | "FACTORIAL"
            | "FIND-STRING"
            | "FORMATTED-CURRENT-DATE"
            | "FORMATTED-DATE"
            | "FORMATTED-DATETIME"
            | "FORMATTED-TIME"
            | "FRACTION-PART"
            | "HEX-OF"
            | "HEX-TO-CHAR"
            | "HIGHEST-ALGEBRAIC"
            | "INTEGER"
            | "INTEGER-OF-BOOLEAN"
            | "INTEGER-OF-DATE"
            | "INTEGER-OF-DAY"
            | "INTEGER-OF-FORMATTED-DATE"
            | "INTEGER-PART"
            | "LENGTH"
            | "LENGTH-AN"
            | "LOCALE-COMPARE"
            | "LOCALE-DATE"
            | "LOCALE-TIME"
            | "LOCALE-TIME-FROM-SECONDS"
            | "LOG"
            | "LOG10"
            | "LOWER-CASE"
            | "LOWEST-ALGEBRAIC"
    )
}

/// Checks ISO/dialect intrinsic names beginning with `M`..`Z`.
///
/// # Arguments
///
/// * `name` - Canonical uppercase identifier.
#[must_use]
fn is_known_intrinsic_m_to_z(name: &str) -> bool {
    matches!(
        name,
        "MAX"
            | "MEAN"
            | "MEDIAN"
            | "MIDRANGE"
            | "MIN"
            | "MOD"
            | "MODULE-CALLER-ID"
            | "MODULE-DATE"
            | "MODULE-FORMATTED-DATE"
            | "MODULE-ID"
            | "MODULE-NAME"
            | "MODULE-PATH"
            | "MODULE-SOURCE"
            | "MODULE-TIME"
            | "MONETARY-DECIMAL-POINT"
            | "MONETARY-THOUSANDS-SEPARATOR"
            | "NATIONAL-OF"
            | "NUMERIC-DECIMAL-POINT"
            | "NUMERIC-THOUSANDS-SEPARATOR"
            | "NUMVAL"
            | "NUMVAL-C"
            | "NUMVAL-F"
            | "ORD"
            | "ORD-MAX"
            | "ORD-MIN"
            | "PI"
            | "PRESENT-VALUE"
            | "RANDOM"
            | "RANGE"
            | "REM"
            | "REVERSE"
            | "SECONDS-FROM-FORMATTED-TIME"
            | "SECONDS-PAST-MIDNIGHT"
            | "SIGN"
            | "SIN"
            | "SMALLEST-ALGEBRAIC"
            | "SQRT"
            | "STANDARD-COMPARE"
            | "STANDARD-DEVIATION"
            | "STORED-CHAR-LENGTH"
            | "SUBSTITUTE"
            | "SUBSTITUTE-CASE"
            | "SUM"
            | "TAN"
            | "TEST-DATE-YYYYMMDD"
            | "TEST-DAY-YYYYDDD"
            | "TEST-FORMATTED-DATETIME"
            | "TEST-NUMVAL"
            | "TEST-NUMVAL-C"
            | "TEST-NUMVAL-F"
            | "TRIM"
            | "ULENGTH"
            | "UPOS"
            | "UPPER-CASE"
            | "USUBSTR"
            | "USUPPLEMENTARY"
            | "UUID4"
            | "UVALID"
            | "UWIDTH"
            | "VARIANCE"
            | "WHEN-COMPILED"
            | "YEAR-TO-YYYY"
    )
}
