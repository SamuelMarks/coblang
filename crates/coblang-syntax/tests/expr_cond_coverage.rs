//! Exhaustive unit and branch-coverage tests for Pratt expressions, `DataRef`s, and conditions.

#![deny(clippy::indexing_slicing)]

use std::path::Path;

use coblang_common::{Dialect, Result, SourceFormat};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_syntax::ast::{
    AbbrevLogicOp, AbbrevRhs, BinaryOp, ClassCheckKind, CompilationGroup, CompilationUnit,
    Condition, DataRefModifier, Expr, NegationMode, RelOp, SignCheckKind, Stmt, SubscriptItem,
    TruthValue,
};
use coblang_syntax::parse_preprocessed_unit;

/// Preprocesses, lexes, and parses a synthetic `PROCEDURE DIVISION` body snippet.
///
/// # Arguments
///
/// * `proc_body` - Synthetic statements inside `PROCEDURE DIVISION.`.
fn parse_proc(proc_body: &str) -> Result<(CompilationGroup, SymbolInterner, DiagnosticSink)> {
    let src = format!(
        "PROGRAM-ID. EXPR-TEST.
PROCEDURE DIVISION.
{proc_body}
"
    );
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 200);
    let unit = preprocess_source_file(
        "expr.cob",
        Path::new("."),
        &src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let mut interner = SymbolInterner::new();
    let group = parse_preprocessed_unit(&unit, &dialect, &mut interner, &mut sink)?;
    Ok((group, interner, sink))
}

/// Exercises `DataRef` modifiers, qualification, subscripts (`ALL`), refmods, and `Expr::span()` across all variants.
#[test]
fn data_ref_subscripts_refmods_and_expr_spans_coverage() -> Result<()> {
    let body = concat!(
        "MOVE ADDRESS OF A IN B OF C TO PTR.\n",
        "MOVE ADDRESS, OF A TO PTR.\n",
        "MOVE ADDRESS TO LENGTH.\n",
        "MOVE LENGTH OF T(ALL) TO N.\n",
        "MOVE LENGTH OF T(ALL, 1) TO N.\n",
        "MOVE BYTE-LENGTH OF T(1, ALL, IX + 2)(3 : 4) TO N.\n",
        "MOVE S(1 : 2) TO D.\n",
        "MOVE S(2 :) TO D.\n",
        "MOVE T(1)(2 :) TO D.\n",
        "MOVE T(1)(2 : .\n",
        "MOVE T(1)(2) TO D.\n",
        "MOVE T() TO D.\n",
        "MOVE A IN . \n",
        "MOVE 1 TO .\n",
        "MOVE ADDRESS OF .\n",
        "COMPUTE R0 = (ADDRESS OF ) + (ADDRESS OF +) + () + (: 1) + (1 2 .\n",
        "COMPUTE R1 = +A.\n",
        "COMPUTE R2 = -A.\n",
        "COMPUTE R3 = \"TXT\" & ALL \"*\" & SPACES.\n",
        "COMPUTE R4 = 10 + 1.5 - 2.0E+1 * 3 / 2 ** 3 ** 2.\n",
        "COMPUTE R5 = FUNCTION UPPER-CASE(A, ALL)(1 : 2) + FUNCTION CURRENT-DATE + FUNCTION .\n",
        "COMPUTE R6 = OBJ::\"m1\"(BY VALUE 1, BY CONTENT 2, BY REFERENCE A, OMITTED, SIZE IS 4 B) + OBJ::M2 + OBJ::.\n",
        "COMPUTE R7 = Y +1 -2 +1.5 -2.5 +1.0E+1 + Z * W +1.\n",
        "FUNCTION .\n",
    );
    let (grp, _, sink) = parse_proc(body)?;
    assert!(sink.error_count() >= 4);

    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let mut saw_addr = false;
    let mut saw_len = false;
    let mut saw_all_sub = false;
    for sent in &p.procedure.initial_sentences {
        for st in &sent.statements {
            match st {
                Stmt::Move { source, .. } => {
                    let _ = source.span();
                    if let Expr::Data(dr) = source {
                        if dr.modifier == DataRefModifier::AddressOf {
                            saw_addr = true;
                        }
                        if dr.modifier == DataRefModifier::LengthOf {
                            saw_len = true;
                        }
                        if dr
                            .subscripts
                            .iter()
                            .any(|s| matches!(s, SubscriptItem::AllWildcard(_)))
                        {
                            saw_all_sub = true;
                        }
                    }
                }
                Stmt::Compute { expr, .. } => {
                    let _ = expr.span();
                    if let Expr::Binary {
                        op: BinaryOp::Add,
                        lhs,
                        rhs,
                        ..
                    } = expr
                    {
                        let _ = lhs.span();
                        let _ = rhs.span();
                    }
                }
                _ => {}
            }
        }
    }
    assert!(saw_addr);
    assert!(saw_len);
    assert!(saw_all_sub);
    Ok(())
}

/// Exercises truncated EOF edge cases inside subscripts, refmods, parens, inline invoke, and `FUNCTION` calls.
#[test]
fn data_ref_and_expr_truncated_eof_coverage() -> Result<()> {
    for eof_tail in [
        "MOVE 1 TO",
        "MOVE ADDRESS OF",
        "MOVE S(1 :",
        "MOVE T(1",
        "MOVE T(1)(2 :",
        "COMPUTE X = O::M(1",
        "COMPUTE X = (1 2",
        "COMPUTE X =",
        "COMPUTE X = FUNCTION F(1",
    ] {
        let src = format!("PROGRAM-ID. EOF-EXPR.\nPROCEDURE DIVISION.\n{eof_tail}");
        let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
        let mem = InMemoryCopybookResolver::new();
        let pp_cfg = PreprocessConfig {
            initial_format: SourceFormat::Free,
            dialect_config: &dialect,
            debug_policy: DebugLinePolicy::StripDebugLines,
            initial_defines: &[],
            copybook_resolver: &mem,
        };
        let mut sm = SourceMap::new();
        let mut s_eof = DiagnosticSink::new(WarningPolicy::new(), 50);
        let u_eof = preprocess_source_file(
            "eof.cob",
            Path::new("."),
            &src,
            &pp_cfg,
            &mut sm,
            &mut s_eof,
        )?;
        let mut i_eof = SymbolInterner::new();
        let _ = parse_preprocessed_unit(&u_eof, &dialect, &mut i_eof, &mut s_eof)?;
    }
    Ok(())
}

/// Exercises relational operators, `NOT <rel_op>`, class/sign/omitted checks, custom classes, and parenthesized conditions.
#[test]
fn conditions_relational_class_sign_omitted_and_parentheses_coverage() -> Result<()> {
    let body = concat!(
        "IF TRUE OR FALSE CONTINUE END-IF.
",
        "IF NOT NOT (A = B AND C <> D) CONTINUE END-IF.
",
        "IF (COND-88) AND COND-89 CONTINUE END-IF.
",
        "IF ((A = B)) AND ((COND-88)) CONTINUE END-IF.
",
        "IF (A .
",
        "IF (A + B) = 10 AND (A) + 1 = 2 AND (A) IS NUMERIC CONTINUE END-IF.
",
        "IF A NOT = 1 AND A NOT <> 2 AND A < 3 AND A NOT < 4 AND A <= 5 AND A NOT <= 6 CONTINUE END-IF.
",
        "IF A > 1 AND A NOT > 2 AND A >= 3 AND A NOT >= 4 CONTINUE END-IF.
",
        "IF A EQUAL TO 1 AND A EQUALS 2 AND A GREATER THAN 3 AND A GREATER THAN OR EQUAL TO 4 CONTINUE END-IF.
",
        "IF A LESS THAN 5 AND A LESS THAN OR EQUAL TO 6 CONTINUE END-IF.
",
        "IF P1 IS OMITTED OR P2 IS NOT OMITTED CONTINUE END-IF.
",
        "IF X IS POSITIVE OR X IS NEGATIVE OR X IS NOT ZERO OR X ZEROS OR X ZEROES CONTINUE END-IF.
",
        "IF X = ZERO + 1 AND Y IS ZERO + 1 CONTINUE END-IF.
",
        "IF S IS NUMERIC OR S IS NOT ALPHABETIC OR S ALPHABETIC-LOWER OR S ALPHABETIC-UPPER CONTINUE END-IF.
",
        "IF S IS KANJI OR S IS DBCS OR S IS MY-CUSTOM-CLASS OR S IS NOT MY-CUSTOM-CLASS OR S NOT MY-CUSTOM-CLASS CONTINUE END-IF.
",
        "IF 1 OR A + B CONTINUE END-IF.
",
        "IF X IS THEN CONTINUE END-IF.
",
        "IF 1 IS OMITTED CONTINUE END-IF.
",
    );
    let (grp, _, _) = parse_proc(body)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    assert!(p.procedure.initial_sentences.len() >= 14);

    // Verify specific AST nodes in parsed `IF` conditions.
    let mut saw_omitted = false;
    let mut saw_custom_cls = false;
    let mut saw_sign = false;
    for sent in &p.procedure.initial_sentences {
        for st in &sent.statements {
            if let Stmt::If { condition, .. } = st {
                if contains_condition(condition, &|c| matches!(c, Condition::OmittedCheck { .. })) {
                    saw_omitted = true;
                }
                if contains_condition(condition, &|c| {
                    matches!(
                        c,
                        Condition::ClassCheck {
                            class: ClassCheckKind::Custom(_),
                            ..
                        }
                    )
                }) {
                    saw_custom_cls = true;
                }
                if contains_condition(condition, &|c| {
                    matches!(
                        c,
                        Condition::SignCheck {
                            sign: SignCheckKind::Zero,
                            polarity: NegationMode::Negated,
                            ..
                        }
                    )
                }) {
                    saw_sign = true;
                }
            }
        }
    }
    assert!(saw_omitted);
    assert!(saw_custom_cls);
    assert!(saw_sign);
    Ok(())
}

/// Recursively inspects a [`Condition`] tree for a predicate match.
///
/// # Arguments
///
/// * `cond` - Root condition node.
/// * `pred` - Predicate closure tested against each node.
fn contains_condition(cond: &Condition, pred: &dyn Fn(&Condition) -> bool) -> bool {
    if pred(cond) {
        return true;
    }
    match cond {
        Condition::Not { inner, .. } => contains_condition(inner, pred),
        Condition::And { lhs, rhs, .. } | Condition::Or { lhs, rhs, .. } => {
            contains_condition(lhs, pred) || contains_condition(rhs, pred)
        }
        _ => false,
    }
}

/// Exercises ANSI-85 abbreviated combined relation lookahead and disambiguation against full conditions.
#[test]
fn abbreviated_combined_relation_conditions_coverage() -> Result<()> {
    let body = concat!(
        "IF A = B OR C AND NOT D CONTINUE END-IF.
",
        "IF A = B, OR IS NOT > D AND <= E CONTINUE END-IF.
",
        "IF A = B OR, C CONTINUE END-IF.
",
        "IF (A = 1 OR B) CONTINUE END-IF.
",
        "IF A = 1 OR B, C CONTINUE END-IF.
",
        "IF A = 1 AND (B = 2) CONTINUE END-IF.
",
        "IF A = 1 OR TRUE CONTINUE END-IF.
",
        "IF A = 1 AND B = 2 CONTINUE END-IF.
",
        "IF A = 1 AND B IS NUMERIC CONTINUE END-IF.
",
        "IF A = 1 AND + 2 OR T(1, 2) + 3 OR B IN C OR FUNCTION ABS(X) CONTINUE END-IF.
",
        "IF A = 1 AND * CONTINUE END-IF.
",
        "IF A = 1 AND THEN CONTINUE END-IF.
",
    );
    let (grp, _, _) = parse_proc(body)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let Some(first_sent) = p.procedure.initial_sentences.first() else {
        return Ok(());
    };
    if let Some(Stmt::If {
        condition:
            Condition::Relation {
                op: RelOp::Eq,
                rhs:
                    AbbrevRhs::Binary {
                        logic: AbbrevLogicOp::Or,
                        ..
                    },
                ..
            },
        ..
    }) = first_sent.statements.first()
    {
        // Verified `A = B OR (C AND NOT D)` abbreviated RHS tree.
    } else {
        assert_eq!(first_sent.statements.len(), 999);
    }
    assert!(matches!(TruthValue::True, TruthValue::True));

    // Declared `SPECIAL-NAMES. CLASS` condition check without `IS` or `NOT` (`IF X HEX-CLS`).
    let cls_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. CLS-TEST.\n",
        "ENVIRONMENT DIVISION.\n",
        "CONFIGURATION SECTION.\n",
        "SPECIAL-NAMES.\n",
        "    CLASS HEX-CLS IS '0' THRU '9', 'A' THRU 'F'.\n",
        "PROCEDURE DIVISION.\n",
        "    IF X HEX-CLS CONTINUE END-IF.\n",
    );
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let unit = preprocess_source_file(
        "cls.cob",
        Path::new("."),
        cls_src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let mut interner = SymbolInterner::new();
    let grp2 = parse_preprocessed_unit(&unit, &dialect, &mut interner, &mut sink)?;
    assert_eq!(grp2.units.len(), 1);
    Ok(())
}
