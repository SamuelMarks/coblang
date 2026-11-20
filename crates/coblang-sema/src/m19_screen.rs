//! Semantic validation, vendor rule enforcement (`gc`/`std`/`acu`/`mf`/`rm`/`xopen`),
//! and HIR lowering for Milestone M19 (`SCREEN SECTION`, extended `DISPLAY`/`ACCEPT`, and `SET ... ATTRIBUTE`).

#![deny(clippy::indexing_slicing)]

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{
    ByteLength, ByteOffset, DigitCount, ExpansionId, FileId, LevelNumber, ScaleFactor, SourceSpan,
    SymbolId,
};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DialectConfig, FeatureDisposition, InternalCharsetMode,
    ScreenConflictPolicy, ScreenSectionRules, SymbolInterner, WarningGroup,
};
use coblang_hir::{
    BlankZeroSpec, DataCategory, HirConstValue, HirDecimalSign, HirExpr, HirFigurative, HirM19Stmt,
    HirPlace, HirScreenBindingPlan, HirScreenColorSpec, HirScreenCoordSpec, HirScreenDisplayItem,
    HirScreenFieldPlan, HirScreenGroupPlan, HirScreenNodePlan, HirScreenOverrides, HirScreenPlan,
    HirScreenPromptSpec, HirScreenSpecialRegs, HirStmt, HirType, JustifiedSpec, NumericSignSpec,
};
use coblang_syntax::ast::{
    AcceptSource, BlankWhenZeroMode, ConsoleMode, DataDivision, DataEntryName, DisplayAdvancing,
    EnvironmentDivision, EventHandlers, Expr, FigurativeValue, JustificationMode,
    ScreenAcceptStmtAst, ScreenClauseAst, ScreenCoordAst, ScreenCoordDirAst, ScreenDisplayItemAst,
    ScreenDisplayStmtAst, ScreenEntry, SetScreenAttrToggleAst, SignClause, SpecialNamesDataRole,
    UsageKind, ValueAtom,
};

use crate::lower_hir::{
    UnitLowerCtx, lower_event_handlers, lower_expr_with_cat, resolve_place, simple_place_for_item,
};
use crate::picture::{ElementaryClauses, PictureContext, compile_elementary_type};
use crate::symbols::ResolvedSymbol;

// Mirror `coblang_screen::ScreenAttrFlags` bit positions so HIR plans remain zero- runtime-dep.
const ATTR_BLANK_SCREEN: u32 = 1 << 0;
const ATTR_BLANK_LINE: u32 = 1 << 1;
const ATTR_ERASE_EOL: u32 = 1 << 2;
const ATTR_ERASE_EOS: u32 = 1 << 3;
const ATTR_BELL: u32 = 1 << 4;
const ATTR_HIGHLIGHT: u32 = 1 << 5;
const ATTR_LOWLIGHT: u32 = 1 << 6;
const ATTR_REVERSE_VIDEO: u32 = 1 << 7;
const ATTR_UNDERLINE: u32 = 1 << 8;
const ATTR_BLINK: u32 = 1 << 9;
const ATTR_LEFTLINE: u32 = 1 << 10;
const ATTR_OVERLINE: u32 = 1 << 11;
const ATTR_AUTO: u32 = 1 << 12;
const ATTR_SECURE: u32 = 1 << 13;
const ATTR_REQUIRED: u32 = 1 << 14;
const ATTR_FULL: u32 = 1 << 15;
const ATTR_PROMPT: u32 = 1 << 16;
const ATTR_ZERO_FILL: u32 = 1 << 17;
const ATTR_BLANK_WHEN_ZERO: u32 = 1 << 18;
const ATTR_JUSTIFIED_RIGHT: u32 = 1 << 19;
const ATTR_UPDATE: u32 = 1 << 20;
const ATTR_INITIAL: u32 = 1 << 21;

/// Pre-indexed `SCREEN SECTION` & `CONSOLE IS CRT` lookup catalog for a compilation unit.
#[derive(Debug, Clone, Default)]
pub struct UnitScreenCatalog {
    /// Maps addressable `SCREEN SECTION` item symbol -> `screen_id` in `HirProgram::screen_plans`.
    pub screen_by_sym: BTreeMap<SymbolId, u32>,
    /// `true` when `SPECIAL-NAMES. CONSOLE IS CRT` is active.
    pub console_is_crt: bool,
    /// Non-CRT mnemonic/device symbols declared in `SPECIAL-NAMES` (`SYSERR`, `CONSOLE`, `SYSOUT`, etc.).
    pub non_crt_devices: BTreeSet<SymbolId>,
}

impl UnitScreenCatalog {
    /// Looks up a `SCREEN SECTION` item `screen_id` by symbol.
    ///
    /// # Arguments
    ///
    /// * `sym` - Screen item symbol.
    #[must_use]
    pub fn lookup_screen(&self, sym: SymbolId) -> Option<u32> {
        self.screen_by_sym.get(&sym).copied()
    }
}

/// Builds the [`UnitScreenCatalog`] for `env_div` and `data_div`, assigning `screen_id`s to named items.
///
/// # Arguments
///
/// * `env_div` - Unit `ENVIRONMENT DIVISION`.
/// * `data_div` - Unit `DATA DIVISION`.
/// * `interner` - Shared [`SymbolInterner`].
#[must_use]
pub fn build_unit_screen_catalog(
    env_div: &EnvironmentDivision,
    data_div: &DataDivision,
    interner: &SymbolInterner,
) -> UnitScreenCatalog {
    let mut cat = UnitScreenCatalog {
        screen_by_sym: BTreeMap::new(),
        console_is_crt: env_div.configuration.special_names.console_mode
            == ConsoleMode::ConsoleIsCrt,
        non_crt_devices: BTreeSet::new(),
    };
    for dev in &env_div.configuration.special_names.devices {
        let sys_up = interner.resolve_canonical(dev.system_device).unwrap_or("");
        if !matches!(sys_up, "CRT" | "CRT-UNDER") {
            cat.non_crt_devices.insert(dev.mnemonic);
        }
    }
    let mut next_id = 0_u32;
    for entry in &data_div.screen_section {
        if matches!(entry.level, LevelNumber::Condition88) {
            continue;
        }
        if let DataEntryName::Named(sym) = entry.name {
            cat.screen_by_sym.entry(sym).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                id
            });
        }
    }
    cat
}

/// Resolves `HirScreenSpecialRegs` and validates/lowers all `SCREEN SECTION` entries into [`HirScreenPlan`]s.
///
/// # Arguments
///
/// * `env_div` - Unit `ENVIRONMENT DIVISION`.
/// * `data_div` - Unit `DATA DIVISION`.
/// * `pic_ctx` - Active [`PictureContext`].
/// * `catalog` - Pre-built [`UnitScreenCatalog`].
/// * `ctx` - Mutable [`UnitLowerCtx`].
pub(crate) fn lower_unit_screen_plans(
    env_div: &EnvironmentDivision,
    data_div: &DataDivision,
    pic_ctx: PictureContext,
    catalog: &UnitScreenCatalog,
    ctx: &mut UnitLowerCtx<'_>,
) -> (Vec<HirScreenPlan>, HirScreenSpecialRegs) {
    let special_regs = resolve_screen_special_regs(env_div, ctx);
    let entries = &data_div.screen_section;
    if entries.is_empty() {
        return (Vec::new(), special_regs);
    }

    let mut plans_by_id: BTreeMap<u32, HirScreenPlan> = BTreeMap::new();
    let mut i = 0;
    while let Some(top) = entries.get(i) {
        if matches!(top.level, LevelNumber::Condition88) {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(top.span),
                String::from("level `88` condition-name is not allowed in `SCREEN SECTION`"),
            ));
            i += 1;
            continue;
        }
        let top_lvl = screen_level_u8(top);
        let end = find_screen_subtree_end(entries, i, top_lvl);
        let mut cur_row = 1_u16;
        let mut cur_col = 1_u16;
        let node = lower_screen_subtree(
            entries,
            i,
            end,
            0,
            pic_ctx,
            &mut cur_row,
            &mut cur_col,
            catalog,
            &mut plans_by_id,
            ctx,
        );
        if let DataEntryName::Named(sym) = top.name
            && let Some(&sid) = catalog.screen_by_sym.get(&sym)
        {
            let nm = String::from(ctx.interner.resolve_canonical(sym).unwrap_or("SCREEN"));
            plans_by_id.insert(
                sid,
                HirScreenPlan {
                    screen_id: sid,
                    name: nm,
                    root: node,
                },
            );
        }
        i = end;
    }

    let plans = plans_by_id.into_values().collect();
    (plans, special_regs)
}

/// Resolves `SPECIAL-NAMES. CURSOR IS`, `CRT STATUS IS`, Data Division `IS SPECIAL-NAMES`, and `COB-CRT-STATUS`.
fn resolve_screen_special_regs(
    env_div: &EnvironmentDivision,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirScreenSpecialRegs {
    let sn = &env_div.configuration.special_names;
    let sp = env_div.span.unwrap_or_else(|| {
        SourceSpan::new(
            FileId::new(0),
            ByteOffset::ZERO,
            ByteLength::ZERO,
            ExpansionId::ROOT,
        )
    });

    let mut cursor_place = sn
        .cursor
        .and_then(|sym| path_to_dataref(&[sym], sp))
        .and_then(|dref| resolve_place(&dref, ctx));
    let mut crt_status_place = sn
        .crt_status
        .and_then(|sym| path_to_dataref(&[sym], sp))
        .and_then(|dref| resolve_place(&dref, ctx));

    for &(role, item_id) in &ctx.layout.special_names_role_items.clone() {
        let Some(it) = ctx.layout.items.get(item_id.as_u32() as usize) else {
            continue;
        };
        let pl = simple_place_for_item(it, sp);
        match role {
            SpecialNamesDataRole::CrtStatus if crt_status_place.is_none() => {
                crt_status_place = Some(pl);
            }
            SpecialNamesDataRole::Cursor if cursor_place.is_none() => {
                cursor_place = Some(pl);
            }
            _ => {}
        }
    }

    let cob_crt_status_place = ctx
        .interner
        .lookup("COB-CRT-STATUS")
        .and_then(|sym| ctx.symtab.resolve_symbol_silent(ctx.scope, sym, &[]))
        .and_then(|res| match res {
            ResolvedSymbol::DataItem(id) => ctx
                .layout
                .items
                .get(id.as_u32() as usize)
                .map(|it| simple_place_for_item(it, sp)),
            _ => None,
        });

    HirScreenSpecialRegs {
        cursor_place,
        crt_status_place,
        cob_crt_status_place,
    }
}

/// Recursively validates and lowers a `SCREEN SECTION` node `entries[idx..end]`, registering named sub-nodes.
#[expect(
    clippy::too_many_arguments,
    reason = "Tracks inherited attributes, static (row, col) cursor, and sub-plan registration."
)]
fn lower_screen_subtree(
    entries: &[ScreenEntry],
    idx: usize,
    end: usize,
    inherited_attrs: u32,
    pic_ctx: PictureContext,
    cur_row: &mut u16,
    cur_col: &mut u16,
    catalog: &UnitScreenCatalog,
    plans_by_id: &mut BTreeMap<u32, HirScreenPlan>,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirScreenNodePlan {
    let entry = &entries.get(idx).cloned().unwrap_or(ScreenEntry {
        level: LevelNumber::Hierarchy(1),
        name: DataEntryName::Filler,
        clauses: Vec::new(),
        span: SourceSpan::new(
            FileId::new(0),
            ByteOffset::ZERO,
            ByteLength::ZERO,
            ExpansionId::ROOT,
        ),
    });

    let mut j = idx + 1;
    while let Some(peek_e) = entries.get(j) {
        if matches!(peek_e.level, LevelNumber::Condition88) {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(peek_e.span),
                String::from("level `88` condition-name is not allowed in `SCREEN SECTION`"),
            ));
            j += 1;
        } else {
            break;
        }
    }
    let has_children = j < end;

    let unpacked = unpack_and_validate_entry_clauses(entry, true, pic_ctx, ctx);

    let line_spec = unpacked.line.clone().unwrap_or(HirScreenCoordSpec::Omitted);
    let col_spec = unpacked.col.clone().unwrap_or(HirScreenCoordSpec::Omitted);
    advance_static_cursor_on_entry(&line_spec, &col_spec, cur_row, cur_col);
    let resolved_static_row = *cur_row;
    let resolved_static_col = *cur_col;

    let item_name_orig = match entry.name {
        DataEntryName::Named(sym) => {
            String::from(ctx.interner.resolve_original(sym).unwrap_or("FILLER"))
        }
        DataEntryName::Filler => String::from("FILLER"),
    };
    let opt_up_name = match entry.name {
        DataEntryName::Named(sym) => Some(String::from(
            ctx.interner.resolve_canonical(sym).unwrap_or(""),
        )),
        DataEntryName::Filler => None,
    };

    let node = if has_children {
        let next_inherited = inherited_attrs
            | (unpacked.attr_bits
                & !(ATTR_BLANK_SCREEN
                    | ATTR_BLANK_LINE
                    | ATTR_ERASE_EOL
                    | ATTR_ERASE_EOS
                    | ATTR_BELL));
        let mut children = Vec::new();
        let mut k = j;
        while k < end {
            let Some(ch) = entries.get(k) else {
                break;
            };
            if matches!(ch.level, LevelNumber::Condition88) {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0007InvalidLayout,
                    Some(ch.span),
                    String::from("level `88` condition-name is not allowed in `SCREEN SECTION`"),
                ));
                k += 1;
                continue;
            }
            let ch_lvl = screen_level_u8(ch);
            let ch_end = find_screen_subtree_end(entries, k, ch_lvl).min(end);
            let ch_node = lower_screen_subtree(
                entries,
                k,
                ch_end,
                next_inherited,
                pic_ctx,
                cur_row,
                cur_col,
                catalog,
                plans_by_id,
                ctx,
            );
            children.push(ch_node);
            k = ch_end;
        }
        HirScreenNodePlan::Group(Box::new(HirScreenGroupPlan {
            name: opt_up_name.clone(),
            line: line_spec,
            col: col_spec,
            attr_bits: unpacked.attr_bits,
            fg_color: unpacked.fg_color,
            bg_color: unpacked.bg_color,
            children,
        }))
    } else {
        validate_vendor_elementary_rules(entry, &item_name_orig, &unpacked, inherited_attrs, ctx);
        let binding = build_elementary_binding(&unpacked, ctx.dialect, ctx.interner);
        let width = binding_display_width(&binding);
        *cur_col = cur_col.saturating_add(width);
        HirScreenNodePlan::Field(Box::new(HirScreenFieldPlan {
            name: opt_up_name.clone(),
            line: line_spec,
            col: col_spec,
            attr_bits: unpacked.attr_bits,
            fg_color: unpacked.fg_color,
            bg_color: unpacked.bg_color,
            prompt: unpacked.prompt,
            size_expr: unpacked.size_expr,
            binding,
        }))
    };

    if idx > 0
        && let DataEntryName::Named(sym) = entry.name
        && let Some(&sid) = catalog.screen_by_sym.get(&sym)
        && !plans_by_id.contains_key(&sid)
    {
        let standalone_root = anchor_subordinate_node(
            &node,
            inherited_attrs,
            resolved_static_row,
            resolved_static_col,
        );
        plans_by_id.insert(
            sid,
            HirScreenPlan {
                screen_id: sid,
                name: opt_up_name.unwrap_or_else(|| String::from("SCREEN")),
                root: standalone_root,
            },
        );
    }

    node
}

/// Anchors a subordinate screen node at its statically resolved `(row, col)` with inherited parent attributes.
fn anchor_subordinate_node(
    node: &HirScreenNodePlan,
    inherited_attrs: u32,
    row: u16,
    col: u16,
) -> HirScreenNodePlan {
    match node {
        HirScreenNodePlan::Field(f) => {
            let mut copy = f.clone();
            copy.line = HirScreenCoordSpec::Absolute(row.max(1));
            copy.col = HirScreenCoordSpec::Absolute(col.max(1));
            copy.attr_bits |= inherited_attrs;
            HirScreenNodePlan::Field(copy)
        }
        HirScreenNodePlan::Group(g) => {
            let mut copy = g.clone();
            copy.line = HirScreenCoordSpec::Absolute(row.max(1));
            copy.col = HirScreenCoordSpec::Absolute(col.max(1));
            copy.attr_bits |= inherited_attrs;
            HirScreenNodePlan::Group(copy)
        }
    }
}

/// Updates compile-time `(cur_row, cur_col)` cursor estimate across sibling screen items.
fn advance_static_cursor_on_entry(
    line: &HirScreenCoordSpec,
    col: &HirScreenCoordSpec,
    cur_row: &mut u16,
    cur_col: &mut u16,
) {
    let row_changed = match *line {
        HirScreenCoordSpec::Omitted => false,
        HirScreenCoordSpec::Absolute(0) => false,
        HirScreenCoordSpec::Absolute(r) => {
            *cur_row = r;
            true
        }
        HirScreenCoordSpec::Plus(d) => {
            *cur_row = cur_row.saturating_add(d.max(1));
            true
        }
        HirScreenCoordSpec::Minus(d) => {
            *cur_row = cur_row.saturating_sub(d.max(1)).max(1);
            true
        }
        HirScreenCoordSpec::Dynamic(_)
        | HirScreenCoordSpec::DynamicPlus(_)
        | HirScreenCoordSpec::DynamicMinus(_) => false,
    };
    match *col {
        HirScreenCoordSpec::Omitted => {
            if row_changed {
                *cur_col = 1;
            }
        }
        HirScreenCoordSpec::Absolute(0) => {}
        HirScreenCoordSpec::Absolute(c) => *cur_col = c,
        HirScreenCoordSpec::Plus(d) => {
            *cur_col = if row_changed {
                1_u16.saturating_add(d)
            } else {
                cur_col.saturating_add(d.saturating_sub(1))
            };
        }
        HirScreenCoordSpec::Minus(d) => {
            *cur_col = cur_col.saturating_sub(d).max(1);
        }
        HirScreenCoordSpec::Dynamic(_)
        | HirScreenCoordSpec::DynamicPlus(_)
        | HirScreenCoordSpec::DynamicMinus(_) => {}
    }
}

/// Unpacked clause summary for a single `SCREEN SECTION` or extended I/O item.
#[expect(
    clippy::struct_excessive_bools,
    reason = "Clause-presence summary flags driving 6-way vendor compatibility diagnostics."
)]
struct UnpackedScreenClauses {
    line: Option<HirScreenCoordSpec>,
    line_is_relative: bool,
    line_is_zero_const: Option<SourceSpan>,
    col: Option<HirScreenCoordSpec>,
    col_is_relative: bool,
    col_is_zero_const: Option<SourceSpan>,
    at_combined: Option<(HirExpr, u8, SourceSpan)>,
    has_ms_pos: Option<SourceSpan>,
    pic_ty: Option<(HirType, SourceSpan)>,
    value_atom: Option<(ValueAtom, bool, SourceSpan)>,
    from_expr: Option<(HirExpr, Option<u16>, SourceSpan)>,
    to_place: Option<(HirPlace, SourceSpan)>,
    using_place: Option<(HirPlace, SourceSpan)>,
    sign: Option<(SignClause, SourceSpan)>,
    has_bwz: Option<SourceSpan>,
    has_just: Option<SourceSpan>,
    has_occurs: Option<SourceSpan>,
    attr_bits: u32,
    force_no_update: bool,
    has_tab: bool,
    fg_color: HirScreenColorSpec,
    bg_color: HirScreenColorSpec,
    prompt: HirScreenPromptSpec,
    size_expr: Option<HirExpr>,
    timeout_expr: Option<HirExpr>,
    timeout_scale_ms: u32,
    cursor_offset_place: Option<HirPlace>,
    cursor_offset_expr: Option<HirExpr>,
    scroll_lines: Option<HirExpr>,
    has_with_ext_clause: bool,
}

/// Unpacks, deduplicates, conflict-checks, and lowers a slice of [`ScreenClauseAst`]s.
fn unpack_and_validate_entry_clauses(
    entry: &ScreenEntry,
    in_screen_section: bool,
    pic_ctx: PictureContext,
    ctx: &mut UnitLowerCtx<'_>,
) -> UnpackedScreenClauses {
    unpack_screen_clause_slice(&entry.clauses, in_screen_section, pic_ctx, ctx)
}

/// Unpacks a slice of [`ScreenClauseAst`]s on either a `ScreenEntry` or extended `DISPLAY`/`ACCEPT`.
#[expect(
    clippy::too_many_lines,
    reason = "Single exhaustive pass over ScreenClauseAst variants for duplicate & conflict checks."
)]
fn unpack_screen_clause_slice(
    clauses: &[ScreenClauseAst],
    in_screen_section: bool,
    pic_ctx: PictureContext,
    ctx: &mut UnitLowerCtx<'_>,
) -> UnpackedScreenClauses {
    let mut u = UnpackedScreenClauses {
        line: None,
        line_is_relative: false,
        line_is_zero_const: None,
        col: None,
        col_is_relative: false,
        col_is_zero_const: None,
        at_combined: None,
        has_ms_pos: None,
        pic_ty: None,
        value_atom: None,
        from_expr: None,
        to_place: None,
        using_place: None,
        sign: None,
        has_bwz: None,
        has_just: None,
        has_occurs: None,
        attr_bits: 0,
        force_no_update: false,
        has_tab: false,
        fg_color: HirScreenColorSpec::Default,
        bg_color: HirScreenColorSpec::Default,
        prompt: HirScreenPromptSpec::Disabled,
        size_expr: None,
        timeout_expr: None,
        timeout_scale_ms: 1000,
        cursor_offset_place: None,
        cursor_offset_expr: None,
        scroll_lines: None,
        has_with_ext_clause: false,
    };

    let mut seen_tags: BTreeSet<&'static str> = BTreeSet::new();
    let mut seen_scroll_up = false;
    let mut seen_scroll_down = false;
    let mut seen_update_on = false;
    let mut seen_update_off = false;
    let mut raw_pic_pair: Option<(SymbolId, SourceSpan)> = None;

    for cl in clauses {
        let sp = cl.span();
        if let Some(dup_tag) = clause_dup_tag(cl)
            && !seen_tags.insert(dup_tag)
        {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(sp),
                format!("duplicate `{dup_tag}` clause"),
            ));
        }

        match cl {
            ScreenClauseAst::Line(coord) => {
                if u.at_combined.is_some() {
                    emit_screen_conflict("cannot specify both `AT` and `LINE`/`COLUMN`", sp, ctx);
                }
                let (spec, is_rel, zero_sp) = lower_screen_coord(coord, in_screen_section, ctx);
                u.line = Some(spec);
                u.line_is_relative = is_rel;
                u.line_is_zero_const = zero_sp;
            }
            ScreenClauseAst::Column(coord) => {
                if u.at_combined.is_some() {
                    emit_screen_conflict("cannot specify both `AT` and `LINE`/`COLUMN`", sp, ctx);
                }
                let (spec, is_rel, zero_sp) = lower_screen_coord(coord, in_screen_section, ctx);
                u.col = Some(spec);
                u.col_is_relative = is_rel;
                u.col_is_zero_const = zero_sp;
            }
            ScreenClauseAst::At {
                expr,
                post_span,
                span,
            } => {
                if u.line.is_some() || u.col.is_some() {
                    emit_screen_conflict(
                        "cannot specify both `AT` screen-location and `LINE` or `COLUMN`",
                        *span,
                        ctx,
                    );
                }
                if let Some((at_e, digits)) =
                    validate_and_lower_at_expr(expr, *post_span, *span, ctx)
                {
                    u.at_combined = Some((at_e, digits, *span));
                }
            }
            ScreenClauseAst::MsPosition { line, col, span } => {
                u.has_ms_pos = Some(*span);
                if let Some(le) = line {
                    let (he, _) = lower_expr_with_cat(le, ctx);
                    u.line = Some(expr_to_coord_spec(he, ScreenCoordDirAst::Absolute));
                }
                if let Some(ce) = col {
                    let (he, _) = lower_expr_with_cat(ce, ctx);
                    u.col = Some(expr_to_coord_spec(he, ScreenCoordDirAst::Absolute));
                }
            }
            ScreenClauseAst::Picture { raw, span } => {
                raw_pic_pair = Some((*raw, *span));
            }
            ScreenClauseAst::Value {
                atom,
                implicit_keyword,
                span,
            } => {
                if *implicit_keyword && ctx.dialect.screen_section_rules == ScreenSectionRules::Std
                {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(*span),
                        String::from("missing `VALUE` keyword before literal in `SCREEN SECTION`"),
                    ));
                }
                u.value_atom = Some((atom.clone(), *implicit_keyword, *span));
            }
            ScreenClauseAst::From { expr, span } => {
                let is_dummy_reserved = matches!(
                    expr,
                    Expr::Data(d) if ctx.interner.resolve_canonical(d.name.base).ok() == Some("__INVALID_RESERVED_FROM")
                );
                if !is_dummy_reserved {
                    let inferred_w = infer_from_expr_width(expr, ctx);
                    let (he, _) = lower_expr_with_cat(expr, ctx);
                    u.from_expr = Some((he, inferred_w, *span));
                }
            }
            ScreenClauseAst::To { target, span } => {
                if ctx.interner.resolve_canonical(target.name.base).ok()
                    != Some("__INVALID_RESERVED_TARGET")
                    && let Some(pl) = resolve_place(target, ctx)
                {
                    u.to_place = Some((pl, *span));
                }
            }
            ScreenClauseAst::Using { target, span } => {
                if ctx.interner.resolve_canonical(target.name.base).ok()
                    != Some("__INVALID_RESERVED_TARGET")
                    && let Some(pl) = resolve_place(target, ctx)
                {
                    u.using_place = Some((pl, *span));
                }
            }
            ScreenClauseAst::Usage { usage, span } => {
                if !matches!(
                    usage,
                    UsageKind::Display
                        | UsageKind::Display1
                        | UsageKind::National
                        | UsageKind::Utf8
                ) {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(*span),
                        String::from(
                            "only `DISPLAY`, `DISPLAY-1`, `NATIONAL`, or `UTF-8` usage is permitted in `SCREEN SECTION`",
                        ),
                    ));
                }
            }
            ScreenClauseAst::Sign { sign, span } => {
                u.sign = Some((*sign, *span));
            }
            ScreenClauseAst::BlankWhenZero(s) => {
                u.has_bwz = Some(*s);
                u.attr_bits |= ATTR_BLANK_WHEN_ZERO;
            }
            ScreenClauseAst::Justified(s) => {
                u.has_just = Some(*s);
                u.attr_bits |= ATTR_JUSTIFIED_RIGHT;
            }
            ScreenClauseAst::Occurs(occ) => {
                u.has_occurs = Some(occ.span);
                ctx.sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::Redefinition,
                    Some(occ.span),
                    String::from("`OCCURS` on screen items is not implemented"),
                ));
            }
            ScreenClauseAst::BlankScreen(s) => {
                if (u.attr_bits & ATTR_BLANK_LINE) != 0 {
                    emit_screen_conflict(
                        "cannot specify both `BLANK SCREEN` and `BLANK LINE`",
                        *s,
                        ctx,
                    );
                }
                u.attr_bits |= ATTR_BLANK_SCREEN;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::BlankLine(s) => {
                if (u.attr_bits & ATTR_BLANK_SCREEN) != 0 {
                    emit_screen_conflict(
                        "cannot specify both `BLANK LINE` and `BLANK SCREEN`",
                        *s,
                        ctx,
                    );
                }
                u.attr_bits |= ATTR_BLANK_LINE;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::EraseEol(s) => {
                if (u.attr_bits & ATTR_ERASE_EOS) != 0 {
                    emit_screen_conflict(
                        "cannot specify both `ERASE EOL` and `ERASE EOS`",
                        *s,
                        ctx,
                    );
                }
                u.attr_bits |= ATTR_ERASE_EOL;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::EraseEos(s) => {
                if (u.attr_bits & ATTR_ERASE_EOL) != 0 {
                    emit_screen_conflict(
                        "cannot specify both `ERASE EOS` and `ERASE EOL`",
                        *s,
                        ctx,
                    );
                }
                u.attr_bits |= ATTR_ERASE_EOS;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Bell(_) => {
                u.attr_bits |= ATTR_BELL;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Highlight(s) => {
                if (u.attr_bits & ATTR_LOWLIGHT) != 0 {
                    emit_screen_conflict("cannot specify both `HIGHLIGHT` and `LOWLIGHT`", *s, ctx);
                }
                u.attr_bits |= ATTR_HIGHLIGHT;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Lowlight(s) => {
                if (u.attr_bits & ATTR_HIGHLIGHT) != 0 {
                    emit_screen_conflict("cannot specify both `LOWLIGHT` and `HIGHLIGHT`", *s, ctx);
                }
                u.attr_bits |= ATTR_LOWLIGHT;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Blink(_) => {
                u.attr_bits |= ATTR_BLINK;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::ReverseVideo(_) => {
                u.attr_bits |= ATTR_REVERSE_VIDEO;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Underline(_) => {
                u.attr_bits |= ATTR_UNDERLINE;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Overline(_) => {
                u.attr_bits |= ATTR_OVERLINE;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Leftline(_) | ScreenClauseAst::Rightline(_) => {
                u.attr_bits |= ATTR_LEFTLINE;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Grid(s) => {
                ctx.sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::Redefinition,
                    Some(*s),
                    String::from("`GRID` is not implemented"),
                ));
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Auto(s) => {
                if u.has_tab {
                    emit_screen_conflict("cannot specify both `AUTO` and `TAB`", *s, ctx);
                }
                u.attr_bits |= ATTR_AUTO;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Tab(s) => {
                if (u.attr_bits & ATTR_AUTO) != 0 {
                    emit_screen_conflict("cannot specify both `TAB` and `AUTO`", *s, ctx);
                }
                u.has_tab = true;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Secure(_) => {
                u.attr_bits |= ATTR_SECURE;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Required(_) => {
                u.attr_bits |= ATTR_REQUIRED;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Full(_) => {
                u.attr_bits |= ATTR_FULL;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Prompt { char_expr, .. } => {
                u.attr_bits |= ATTR_PROMPT;
                u.has_with_ext_clause = true;
                u.prompt = lower_prompt_spec(char_expr.as_ref(), ctx);
            }
            ScreenClauseAst::Update { enabled, span } => {
                u.has_with_ext_clause = true;
                if *enabled {
                    if seen_update_off {
                        emit_screen_conflict(
                            "cannot specify both `UPDATE` and `NO UPDATE`",
                            *span,
                            ctx,
                        );
                    }
                    seen_update_on = true;
                    u.attr_bits |= ATTR_UPDATE;
                } else {
                    if seen_update_on {
                        emit_screen_conflict(
                            "cannot specify both `NO UPDATE` and `UPDATE`",
                            *span,
                            ctx,
                        );
                    }
                    seen_update_off = true;
                    u.force_no_update = true;
                    u.attr_bits &= !ATTR_UPDATE;
                }
            }
            ScreenClauseAst::ZeroFill(_) => {
                u.attr_bits |= ATTR_ZERO_FILL;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Initial(_) => {
                u.attr_bits |= ATTR_INITIAL;
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Size { expr, .. } => {
                let (he, _) = lower_expr_with_cat(expr, ctx);
                u.size_expr = Some(he);
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::ForegroundColor { expr, .. } => {
                u.fg_color = lower_color_spec(expr, ctx);
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::BackgroundColor { expr, .. } => {
                u.bg_color = lower_color_spec(expr, ctx);
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Control { expr, .. } | ScreenClauseAst::Color { expr, .. } => {
                let _ = lower_expr_with_cat(expr, ctx);
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::ModeIsBlock(_) => {
                u.has_with_ext_clause = true;
            }
            ScreenClauseAst::Conversion(s) => {
                u.has_with_ext_clause = true;
                if !in_screen_section {
                    ctx.sink.emit(Diagnostic::warning(
                        DiagnosticCode::W0006GeneralWarning,
                        WarningGroup::Redefinition,
                        Some(*s),
                        String::from("`WITH CONVERSION` is not implemented"),
                    ));
                }
            }
            ScreenClauseAst::Scroll { down, lines, span } => {
                u.has_with_ext_clause = true;
                if *down {
                    if seen_scroll_up {
                        emit_screen_conflict(
                            "cannot specify both `SCROLL DOWN` and `SCROLL UP`",
                            *span,
                            ctx,
                        );
                    }
                    seen_scroll_down = true;
                } else {
                    if seen_scroll_down {
                        emit_screen_conflict(
                            "cannot specify both `SCROLL UP` and `SCROLL DOWN`",
                            *span,
                            ctx,
                        );
                    }
                    seen_scroll_up = true;
                }
                let base_e = if let Some(le) = lines {
                    lower_expr_with_cat(le, ctx).0
                } else {
                    let one = ctx.builtins_get("1");
                    HirExpr::Const {
                        value: HirConstValue::Decimal {
                            text: one,
                            scale: ScaleFactor::ZERO,
                            sign: HirDecimalSign::UnsignedOrEmbedded,
                        },
                        span: *span,
                    }
                };
                u.scroll_lines = Some(if *down {
                    HirExpr::Neg(Box::new(base_e))
                } else {
                    base_e
                });
            }
            ScreenClauseAst::Timeout { expr, unit_ms, .. } => {
                u.has_with_ext_clause = true;
                let (he, _) = lower_expr_with_cat(expr, ctx);
                u.timeout_expr = Some(he);
                u.timeout_scale_ms = *unit_ms;
            }
            ScreenClauseAst::CursorOffset { expr, .. } => {
                u.has_with_ext_clause = true;
                if let Expr::Data(dref) = expr
                    && let Some(pl) = resolve_place(dref, ctx)
                {
                    u.cursor_offset_place = Some(pl);
                } else {
                    let (he, _) = lower_expr_with_cat(expr, ctx);
                    u.cursor_offset_expr = Some(he);
                }
            }
        }
    }

    if let Some(occ_sp) = u.has_occurs
        && !u.line_is_relative
        && !u.col_is_relative
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(occ_sp),
            String::from("relative `LINE`/`COLUMN` clause required with `OCCURS`"),
        ));
    }

    if let Some((pic_sym, pic_sp)) = raw_pic_pair {
        let pic_raw_str = String::from(ctx.interner.resolve_original(pic_sym).unwrap_or("X"));
        let disp_usage = UsageKind::Display;
        let clauses_elem = ElementaryClauses {
            raw_pic: Some((&pic_raw_str, pic_sp)),
            entry_span: pic_sp,
            pic_ctx,
            usage: Some(&disp_usage),
            sign_clause: u.sign.map(|(s, _)| s),
            justified: if u.has_just.is_some() {
                JustificationMode::JustifiedRight
            } else {
                JustificationMode::StandardLeft
            },
            blank_zero: if u.has_bwz.is_some() {
                BlankWhenZeroMode::BlankWhenZero
            } else {
                BlankWhenZeroMode::Normal
            },
        };
        let compiled_ty = compile_elementary_type(&clauses_elem, ctx.dialect, ctx.sink);
        if (u.attr_bits & ATTR_FULL) != 0 && compiled_ty.category() == DataCategory::Numeric {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::Redefinition,
                Some(pic_sp),
                String::from(
                    "`FULL` has no effect on unedited numeric items; consider `REQUIRED` or `PIC Z`",
                ),
            ));
        }
        u.pic_ty = Some((compiled_ty, pic_sp));
    }

    u
}

/// Canonical duplicate-detection key for a [`ScreenClauseAst`].
const fn clause_dup_tag(cl: &ScreenClauseAst) -> Option<&'static str> {
    match cl {
        ScreenClauseAst::Line(_) => Some("LINE"),
        ScreenClauseAst::Column(_) => Some("COLUMN"),
        ScreenClauseAst::At { .. } => Some("AT"),
        ScreenClauseAst::Picture { .. } => Some("PIC"),
        ScreenClauseAst::Value { .. } => Some("VALUE"),
        ScreenClauseAst::From { .. } => Some("FROM"),
        ScreenClauseAst::To { .. } => Some("TO"),
        ScreenClauseAst::Using { .. } => Some("USING"),
        ScreenClauseAst::Usage { .. } => Some("USAGE"),
        ScreenClauseAst::Sign { .. } => Some("SIGN"),
        ScreenClauseAst::BlankWhenZero(_) => Some("BLANK WHEN ZERO"),
        ScreenClauseAst::Justified(_) => Some("JUSTIFIED"),
        ScreenClauseAst::Occurs(_) => Some("OCCURS"),
        ScreenClauseAst::BlankScreen(_) => Some("BLANK SCREEN"),
        ScreenClauseAst::BlankLine(_) => Some("BLANK LINE"),
        ScreenClauseAst::EraseEol(_) => Some("ERASE EOL"),
        ScreenClauseAst::EraseEos(_) => Some("ERASE EOS"),
        ScreenClauseAst::Bell(_) => Some("BELL"),
        ScreenClauseAst::Highlight(_) => Some("HIGHLIGHT"),
        ScreenClauseAst::Lowlight(_) => Some("LOWLIGHT"),
        ScreenClauseAst::Blink(_) => Some("BLINK"),
        ScreenClauseAst::ReverseVideo(_) => Some("REVERSE-VIDEO"),
        ScreenClauseAst::Underline(_) => Some("UNDERLINE"),
        ScreenClauseAst::Overline(_) => Some("OVERLINE"),
        ScreenClauseAst::Leftline(_) => Some("LEFTLINE"),
        ScreenClauseAst::Rightline(_) => Some("RIGHTLINE"),
        ScreenClauseAst::Grid(_) => Some("GRID"),
        ScreenClauseAst::Auto(_) => Some("AUTO"),
        ScreenClauseAst::Tab(_) => Some("TAB"),
        ScreenClauseAst::Secure(_) => Some("SECURE"),
        ScreenClauseAst::Required(_) => Some("REQUIRED"),
        ScreenClauseAst::Full(_) => Some("FULL"),
        ScreenClauseAst::Prompt { .. } => Some("PROMPT"),
        ScreenClauseAst::Update { .. } => Some("UPDATE"),
        ScreenClauseAst::ZeroFill(_) => Some("ZERO-FILL"),
        ScreenClauseAst::Initial(_) => Some("INITIAL"),
        ScreenClauseAst::Size { .. } => Some("SIZE"),
        ScreenClauseAst::ForegroundColor { .. } => Some("FOREGROUND-COLOR"),
        ScreenClauseAst::BackgroundColor { .. } => Some("BACKGROUND-COLOR"),
        ScreenClauseAst::ModeIsBlock(_) => Some("MODE IS BLOCK"),
        ScreenClauseAst::Conversion(_) => Some("CONVERSION"),
        ScreenClauseAst::Timeout { .. } => Some("TIME-OUT"),
        ScreenClauseAst::CursorOffset { .. } => Some("CURSOR"),
        ScreenClauseAst::MsPosition { .. }
        | ScreenClauseAst::Control { .. }
        | ScreenClauseAst::Color { .. }
        | ScreenClauseAst::Scroll { .. } => None,
    }
}

/// Emits a conflicting-clause diagnostic honoring `-frelax-syntax-checks` (`Error` vs `Warning`).
fn emit_screen_conflict(msg: &str, span: SourceSpan, ctx: &mut UnitLowerCtx<'_>) {
    match ctx.dialect.screen_conflict_policy {
        ScreenConflictPolicy::StrictError => {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(span),
                String::from(msg),
            ));
        }
        ScreenConflictPolicy::RelaxedWarning => {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::Redefinition,
                Some(span),
                String::from(msg),
            ));
        }
    }
}

/// Validates all 6 `-fscreen-section-rules` (`Std`, `Acu`, `Mf`, `Rm`, `Xopen`, `Gc`) on an elementary item.
#[expect(
    clippy::too_many_lines,
    reason = "Encodes the 6-dialect vendor compatibility matrix verified by syn_screen/0016, 0018, 0021."
)]
fn validate_vendor_elementary_rules(
    entry: &ScreenEntry,
    name: &str,
    u: &UnpackedScreenClauses,
    inherited_attrs: u32,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let rules = ctx.dialect.screen_section_rules;
    let sp = entry.span;
    let eff_attrs = u.attr_bits | inherited_attrs;

    let has_pic = u.pic_ty.is_some();
    let has_val = u.value_atom.is_some();
    let val_is_num = u
        .value_atom
        .as_ref()
        .is_some_and(|(a, _, _)| matches!(a, ValueAtom::Integer(_) | ValueAtom::Decimal { .. }));
    let val_is_fig = u
        .value_atom
        .as_ref()
        .is_some_and(|(a, _, _)| matches!(a, ValueAtom::Figurative(_)));
    let has_from = u.from_expr.is_some();
    let has_to = u.to_place.is_some();
    let has_using = u.using_place.is_some();
    let has_ftu = has_from || has_to || has_using;
    let has_tu = has_to || has_using;
    let has_bell = (u.attr_bits & ATTR_BELL) != 0;
    let has_blank = (u.attr_bits & (ATTR_BLANK_SCREEN | ATTR_BLANK_LINE)) != 0;
    let has_erase = (u.attr_bits & (ATTR_ERASE_EOL | ATTR_ERASE_EOS)) != 0;
    let has_line_or_col = u.line.is_some() || u.col.is_some();

    let implied_pic_ok = if has_ftu && !has_pic {
        u.using_place.is_some()
            || u.to_place.is_some()
            || u.from_expr.as_ref().is_some_and(|(_, w, _)| w.is_some())
    } else {
        true
    };

    match rules {
        ScreenSectionRules::Std => {
            if !has_pic && !has_ftu && !has_val && !has_bell && !has_blank && !has_erase {
                emit_err(
                    sp,
                    &format!(
                        "`{name}` needs a `PIC`, `FROM`, `TO`, `USING`, `VALUE`, `BELL`, `BLANK`, or `ERASE` clause"
                    ),
                    ctx,
                );
            }
            if has_pic && !has_ftu && !(has_val && val_is_num) {
                emit_err(
                    sp,
                    &format!(
                        "`{name}` cannot have `PIC` without `FROM`, `TO`, `USING`, or numeric `VALUE`"
                    ),
                    ctx,
                );
            }
            if has_ftu && !has_pic {
                emit_err(
                    sp,
                    "cannot have `FROM`, `TO`, or `USING` without `PIC`",
                    ctx,
                );
            }
            if (eff_attrs & ATTR_FULL) != 0 && u.has_just.is_some() {
                emit_err(sp, "cannot specify both `FULL` and `JUSTIFIED`", ctx);
            }
            if val_is_fig {
                emit_err(sp, "`VALUE` may not contain a figurative constant", ctx);
            }
            if has_val && val_is_num && !has_pic {
                emit_err(sp, "cannot have numeric `VALUE` without `PIC`", ctx);
            }
        }
        ScreenSectionRules::Acu => {
            if has_pic && has_val {
                emit_err(sp, "cannot specify both `PIC` and `VALUE`", ctx);
            }
            if has_pic && !has_ftu {
                emit_err(
                    sp,
                    "cannot have `PIC` without `FROM`, `TO`, or `USING`",
                    ctx,
                );
            }
            if has_val && val_is_num {
                emit_err(sp, "`VALUE` item may not be numeric", ctx);
            }
            if has_ftu && !has_pic {
                emit_warn(
                    sp,
                    &format!(
                        "`{name}` has `FROM`, `TO`, or `USING` without `PIC`; `PIC` will be implied"
                    ),
                    ctx,
                );
            }
            if u.has_bwz.is_some() && !has_pic {
                emit_err(sp, "cannot have `BLANK WHEN ZERO` without `PIC`", ctx);
            }
            if u.has_just.is_some() && !has_pic {
                emit_err(sp, "cannot have `JUSTIFIED` without `PIC`", ctx);
            }
        }
        ScreenSectionRules::Mf => {
            if !has_pic && !has_line_or_col && !has_val && !has_bell && !has_blank {
                emit_err(
                    sp,
                    &format!(
                        "`{name}` needs a `PIC`, `COL`, `LINE`, `VALUE`, `BELL`, or `BLANK` clause"
                    ),
                    ctx,
                );
            }
            if has_pic && has_val {
                emit_err(sp, "cannot specify both `PIC` and `VALUE`", ctx);
            }
            if has_pic && !has_ftu {
                emit_err(
                    sp,
                    "cannot have `PIC` without `FROM`, `TO`, or `USING`",
                    ctx,
                );
            }
            if has_val && val_is_num {
                emit_err(sp, "`VALUE` item may not be numeric", ctx);
            }
            if has_ftu && !has_pic {
                emit_warn(
                    sp,
                    &format!(
                        "`{name}` has `FROM`, `TO`, or `USING` without `PIC`; `PIC` will be implied"
                    ),
                    ctx,
                );
            }
            if (eff_attrs & (ATTR_AUTO | ATTR_FULL | ATTR_PROMPT | ATTR_REQUIRED | ATTR_SECURE))
                != 0
                && !has_tu
            {
                emit_err(
                    sp,
                    "cannot use `AUTO`, `FULL`, `PROMPT`, `REQUIRED`, or `SECURE` on elementary item without `TO` or `USING`",
                    ctx,
                );
            }
            if (u.has_bwz.is_some()
                || u.has_just.is_some()
                || u.has_occurs.is_some()
                || u.sign.is_some())
                && !has_ftu
            {
                emit_err(
                    sp,
                    "cannot use `BLANK WHEN ZERO`, `JUSTIFIED`, `OCCURS`, or `SIGN` on item without `FROM`, `TO`, or `USING`",
                    ctx,
                );
            }
            if val_is_fig {
                emit_err(sp, "`VALUE` may not contain a figurative constant", ctx);
            }
        }
        ScreenSectionRules::Rm => {
            if has_pic && has_val {
                emit_err(sp, "cannot specify both `PIC` and `VALUE`", ctx);
            }
            if has_pic && !has_ftu {
                emit_err(
                    sp,
                    "cannot have `PIC` without `FROM`, `TO`, or `USING`",
                    ctx,
                );
            }
            if has_val && val_is_num {
                emit_err(sp, "`VALUE` item may not be numeric", ctx);
            }
            if has_ftu && !has_pic {
                emit_err(
                    sp,
                    "cannot have `FROM`, `TO`, or `USING` without `PIC`",
                    ctx,
                );
            }
            if (eff_attrs & (ATTR_AUTO | ATTR_FULL | ATTR_REQUIRED | ATTR_SECURE)) != 0 && !has_ftu
            {
                emit_err(
                    sp,
                    "cannot use `AUTO`, `FULL`, `REQUIRED`, or `SECURE` on elementary item without `FROM`, `TO`, or `USING`",
                    ctx,
                );
            }
            if (u.has_bwz.is_some() || u.has_just.is_some() || u.sign.is_some())
                && !(has_ftu && has_pic)
            {
                emit_err(
                    sp,
                    "cannot use `BLANK WHEN ZERO`, `JUSTIFIED`, or `SIGN` without `FROM`, `TO`, or `USING`",
                    ctx,
                );
            }
        }
        ScreenSectionRules::Xopen => {
            if !has_pic && !has_line_or_col && !has_val && !has_bell && !has_blank {
                emit_err(
                    sp,
                    &format!(
                        "`{name}` needs a `PIC`, `COL`, `LINE`, `VALUE`, `BELL`, or `BLANK` clause"
                    ),
                    ctx,
                );
            }
            if has_pic && has_val {
                emit_err(sp, "cannot specify both `PIC` and `VALUE`", ctx);
            }
            if has_pic && !has_ftu {
                emit_err(
                    sp,
                    "cannot have `PIC` without `FROM`, `TO`, or `USING`",
                    ctx,
                );
            }
            if has_val && val_is_num {
                emit_err(sp, "`VALUE` item may not be numeric", ctx);
            }
            if has_ftu && !has_pic {
                emit_err(
                    sp,
                    "cannot have `FROM`, `TO`, or `USING` without `PIC`",
                    ctx,
                );
            }
            if (eff_attrs & ATTR_AUTO) != 0 && !has_ftu {
                emit_err(
                    sp,
                    "cannot have `AUTO` without `FROM`, `TO`, or `USING`",
                    ctx,
                );
            }
            if (eff_attrs & (ATTR_FULL | ATTR_REQUIRED)) != 0 && !has_tu {
                emit_err(
                    sp,
                    "cannot use `FULL` or `REQUIRED` on item without `TO` or `USING`",
                    ctx,
                );
            }
            if (eff_attrs & ATTR_FULL) != 0 && u.has_just.is_some() {
                emit_err(sp, "cannot specify both `FULL` and `JUSTIFIED`", ctx);
            }
            if (eff_attrs & ATTR_SECURE) != 0 && !has_to {
                emit_err(sp, "`SECURE` must be used with `TO`", ctx);
            }
        }
        ScreenSectionRules::Gc => {
            let any_action =
                has_ftu || has_val || has_bell || has_blank || has_erase || has_line_or_col;
            if !any_action {
                emit_warn(sp, &format!("`{name}` does nothing"), ctx);
            }
            let raw_from_no_pic = !has_pic
                && entry
                    .clauses
                    .iter()
                    .any(|c| matches!(c, ScreenClauseAst::From { expr, .. } if !matches!(expr, Expr::Data(d) if ctx.interner.resolve_canonical(d.name.base).ok() == Some("__INVALID_RESERVED_FROM"))))
                && !has_ftu;
            if (has_ftu || raw_from_no_pic) && !has_pic {
                emit_warn(
                    sp,
                    &format!(
                        "`{name}` has `FROM`, `TO`, or `USING` without `PIC`; `PIC` will be implied"
                    ),
                    ctx,
                );
                if raw_from_no_pic || !implied_pic_ok {
                    emit_err(sp, &format!("`PICTURE` clause required for `{name}`"), ctx);
                }
            }
            if has_val && val_is_num && !has_pic {
                emit_warn(
                    sp,
                    &format!("`{name}` has numeric `VALUE` without `PIC`; `PIC` will be implied"),
                    ctx,
                );
            }
        }
    }
}

/// Emits a screen layout error diagnostic.
fn emit_err(sp: SourceSpan, msg: &str, ctx: &mut UnitLowerCtx<'_>) {
    ctx.sink.emit(Diagnostic::error(
        DiagnosticCode::E0007InvalidLayout,
        Some(sp),
        String::from(msg),
    ));
}

/// Emits a screen layout warning diagnostic.
fn emit_warn(sp: SourceSpan, msg: &str, ctx: &mut UnitLowerCtx<'_>) {
    ctx.sink.emit(Diagnostic::warning(
        DiagnosticCode::W0006GeneralWarning,
        WarningGroup::Redefinition,
        Some(sp),
        String::from(msg),
    ));
}

/// Infers fixed display width from a `FROM` operand (`DataRef`, string literal, or string `78` constant).
fn infer_from_expr_width(expr: &Expr, ctx: &UnitLowerCtx<'_>) -> Option<u16> {
    match expr {
        Expr::Literal {
            atom: ValueAtom::String { bytes, .. },
            ..
        } => u16::try_from(bytes.len().max(1)).ok(),
        Expr::Data(dref) => {
            if let Some(res) =
                ctx.symtab
                    .resolve_symbol_silent(ctx.scope, dref.name.base, &dref.name.qualifiers)
            {
                match res {
                    ResolvedSymbol::DataItem(id) => {
                        let it = ctx.layout.items.get(id.as_u32() as usize)?;
                        u16::try_from(it.ty.static_byte_size().max(1)).ok()
                    }
                    ResolvedSymbol::Constant(HirConstValue::Bytes(bytes)) => {
                        u16::try_from(bytes.len().max(1)).ok()
                    }
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Builds [`HirScreenBindingPlan`] for an elementary `SCREEN SECTION` leaf.
fn build_elementary_binding(
    u: &UnpackedScreenClauses,
    dialect: &DialectConfig,
    interner: &SymbolInterner,
) -> HirScreenBindingPlan {
    let default_alpha = |w: u16| HirType::Alphanumeric {
        byte_len: u32::from(w.max(1)),
        justified: if u.has_just.is_some() {
            JustifiedSpec::Right
        } else {
            JustifiedSpec::Left
        },
    };

    if let Some((pl, _)) = &u.using_place {
        let screen_ty = u
            .pic_ty
            .as_ref()
            .map_or_else(|| pl.effective_ty.clone(), |(t, _)| t.clone());
        let width = u16::try_from(screen_ty.static_byte_size().max(1)).unwrap_or(1);
        return HirScreenBindingPlan::UsingPlace {
            target: pl.clone(),
            width,
            screen_ty,
        };
    }
    if let (Some((src, inf_w, _)), Some((dst, _))) = (&u.from_expr, &u.to_place) {
        let screen_ty = u
            .pic_ty
            .as_ref()
            .map_or_else(|| dst.effective_ty.clone(), |(t, _)| t.clone());
        let width = u16::try_from(screen_ty.static_byte_size().max(1))
            .ok()
            .or(*inf_w)
            .unwrap_or(1);
        return HirScreenBindingPlan::FromAndTo {
            src: src.clone(),
            dst: dst.clone(),
            width,
            screen_ty,
        };
    }
    if let Some((src, inf_w, _)) = &u.from_expr {
        let width = u
            .pic_ty
            .as_ref()
            .and_then(|(t, _)| u16::try_from(t.static_byte_size().max(1)).ok())
            .or(*inf_w)
            .unwrap_or(1);
        let screen_ty = u
            .pic_ty
            .as_ref()
            .map_or_else(|| default_alpha(width), |(t, _)| t.clone());
        return HirScreenBindingPlan::FromExpr {
            src: src.clone(),
            width,
            screen_ty,
        };
    }
    if let Some((dst, _)) = &u.to_place {
        let screen_ty = u
            .pic_ty
            .as_ref()
            .map_or_else(|| dst.effective_ty.clone(), |(t, _)| t.clone());
        let width = u16::try_from(screen_ty.static_byte_size().max(1)).unwrap_or(1);
        return HirScreenBindingPlan::ToPlace {
            dst: dst.clone(),
            width,
            screen_ty,
        };
    }
    if let Some((atom, _, _)) = &u.value_atom {
        return build_value_binding(atom, u.pic_ty.as_ref().map(|(t, _)| t), dialect, interner);
    }
    HirScreenBindingPlan::None
}

/// Encodes a `VALUE` atom on a screen item into [`HirScreenBindingPlan`].
fn build_value_binding(
    atom: &ValueAtom,
    pic_ty: Option<&HirType>,
    dialect: &DialectConfig,
    interner: &SymbolInterner,
) -> HirScreenBindingPlan {
    let pic_w = pic_ty.and_then(|t| u16::try_from(t.static_byte_size().max(1)).ok());
    match atom {
        ValueAtom::Figurative(FigurativeValue::All(pat)) => HirScreenBindingPlan::FigurativeAll {
            pattern: if pat.is_empty() {
                vec![b' ']
            } else {
                pat.clone()
            },
            width: pic_w.unwrap_or(1),
        },
        ValueAtom::Figurative(fig) => {
            let b = match fig {
                FigurativeValue::Zero => b'0',
                FigurativeValue::Quote => b'"',
                FigurativeValue::LowValue | FigurativeValue::Null => 0x00,
                FigurativeValue::HighValue => 0xFF,
                FigurativeValue::Space | FigurativeValue::All(_) => b' ',
            };
            let w = pic_w.unwrap_or(1);
            HirScreenBindingPlan::StaticValue(vec![b; usize::from(w)])
        }
        ValueAtom::String { bytes, .. } => {
            let mut out = match dialect.internal_charset {
                InternalCharsetMode::Ascii | InternalCharsetMode::Ebcdic => bytes.clone(),
            };
            if let Some(w) = pic_w {
                out.resize(usize::from(w), b' ');
            }
            HirScreenBindingPlan::StaticValue(out)
        }
        ValueAtom::Integer(sym) | ValueAtom::Float(sym) => {
            let raw = interner.resolve_original(*sym).unwrap_or("0");
            let clean = raw.trim_start_matches('+');
            let w = usize::from(
                pic_w.unwrap_or_else(|| u16::try_from(clean.len().max(1)).unwrap_or(1)),
            );
            let padded = format!("{clean:>0width$}", width = w);
            HirScreenBindingPlan::StaticValue(padded.into_bytes())
        }
        ValueAtom::Decimal { digits, scale, .. } => {
            let raw = interner.resolve_original(*digits).unwrap_or("0");
            let sc = usize::try_from(scale.get().max(0)).unwrap_or(0);
            let formatted = if sc > 0 && raw.len() > sc {
                let split = raw.len() - sc;
                format!(
                    "{}.{}",
                    &raw.get(..split).unwrap_or("0"),
                    &raw.get(split..).unwrap_or("0")
                )
            } else {
                String::from(raw)
            };
            HirScreenBindingPlan::StaticValue(formatted.into_bytes())
        }
        ValueAtom::Identifier(sym) => {
            let s = interner.resolve_original(*sym).unwrap_or("");
            HirScreenBindingPlan::StaticValue(s.as_bytes().to_vec())
        }
    }
}

/// Returns static column width occupied by `binding`.
fn binding_display_width(binding: &HirScreenBindingPlan) -> u16 {
    match binding {
        HirScreenBindingPlan::None => 0,
        HirScreenBindingPlan::StaticValue(b) => u16::try_from(b.len()).unwrap_or(1),
        HirScreenBindingPlan::FigurativeAll { width, .. }
        | HirScreenBindingPlan::FromExpr { width, .. }
        | HirScreenBindingPlan::ToPlace { width, .. }
        | HirScreenBindingPlan::FromAndTo { width, .. }
        | HirScreenBindingPlan::UsingPlace { width, .. } => *width,
    }
}

// ============================================================================
// Procedural `DISPLAY` / `ACCEPT` / `SET ... ATTRIBUTE` Lowering
// ============================================================================

/// Attempts to lower a `Stmt::Display` as a Milestone M19 screen `DISPLAY`, returning `true` if handled.
#[expect(
    clippy::too_many_arguments,
    reason = "Inspects full AST DISPLAY parts + UnitScreenCatalog to disambiguate stream vs TUI DISPLAY."
)]
pub(crate) fn try_lower_m19_display_stmt(
    operands: &[Expr],
    upon: Option<SymbolId>,
    advancing: DisplayAdvancing,
    screen_ext: Option<&ScreenDisplayStmtAst>,
    handlers: &EventHandlers,
    span: SourceSpan,
    pic_ctx: PictureContext,
    catalog: &UnitScreenCatalog,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) -> bool {
    let any_screen_sym = operands
        .iter()
        .any(|op| expr_screen_id(op, catalog).is_some());
    let has_screen_clauses = screen_ext.is_some_and(|ext| {
        ext.items.iter().any(|it| {
            !it.clauses.is_empty()
                || it
                    .upon
                    .is_some_and(|(d, _)| is_crt_device_sym(d, ctx.interner))
        }) || ext
            .trailing_upon
            .is_some_and(|(d, _)| is_crt_device_sym(d, ctx.interner))
    });
    let upon_is_non_crt = upon.is_some_and(|u| is_non_crt_upon(u, catalog, ctx.interner));
    let console_crt_active = catalog.console_is_crt && !upon_is_non_crt;

    if !any_screen_sym && !has_screen_clauses && !console_crt_active {
        return false;
    }

    let synth_items: Vec<ScreenDisplayItemAst>;
    let items_slice: &[ScreenDisplayItemAst] = if let Some(ext) = screen_ext {
        &ext.items
    } else {
        synth_items = operands
            .iter()
            .map(|op| ScreenDisplayItemAst {
                operand: Some(op.clone()),
                clauses: Vec::new(),
                upon: None,
                span: op.span(),
            })
            .collect();
        &synth_items
    };
    let trailing_upon = screen_ext.and_then(|e| e.trailing_upon);
    let no_adv_span = screen_ext.and_then(|e| e.no_advancing_span).unwrap_or(span);

    if advancing == DisplayAdvancing::NoAdvancing {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0003Syntax,
            Some(no_adv_span),
            String::from("cannot specify `NO ADVANCING` in screen `DISPLAY`"),
        ));
    }

    let mut saw_screen_item = false;
    let mut saw_field_item = false;
    let mut saw_screen_clause_item = false;
    let mut saw_non_crt_upon_item = false;
    let mut lowered_items = Vec::with_capacity(items_slice.len());

    for item in items_slice {
        let eff_upon = item.upon.or(trailing_upon);
        let item_non_crt = eff_upon.is_some_and(|(d, _)| is_non_crt_upon(d, catalog, ctx.interner));
        if item_non_crt {
            saw_non_crt_upon_item = true;
        }
        if !item.clauses.is_empty() {
            saw_screen_clause_item = true;
            if item_non_crt {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0003Syntax,
                    Some(item.span),
                    String::from("screen clauses may only be used for `DISPLAY` on `CRT`"),
                ));
            }
        }

        let unpacked = unpack_screen_clause_slice(&item.clauses, false, pic_ctx, ctx);
        let overrides = build_hir_overrides(&unpacked);

        if let Some(op) = &item.operand {
            if let Some(sid) = expr_screen_id(op, catalog) {
                saw_screen_item = true;
                if item_non_crt {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(item.span),
                        String::from("screens may only be displayed on `CRT`"),
                    ));
                }
                check_accept_display_extensions_on_display(
                    true,
                    items_slice.len(),
                    &unpacked,
                    item.span,
                    ctx,
                );
                lowered_items.push(HirScreenDisplayItem::NamedScreen {
                    screen_id: sid,
                    overrides,
                });
            } else {
                saw_field_item = true;
                check_accept_display_extensions_on_display(
                    false,
                    items_slice.len(),
                    &unpacked,
                    item.span,
                    ctx,
                );
                let (he, _) = lower_expr_with_cat(op, ctx);
                lowered_items.push(HirScreenDisplayItem::ExtendedField {
                    expr: Some(he),
                    overrides,
                });
            }
        } else {
            saw_field_item = true;
            check_accept_display_extensions_on_display(
                false,
                items_slice.len(),
                &unpacked,
                item.span,
                ctx,
            );
            lowered_items.push(HirScreenDisplayItem::ExtendedField {
                expr: None,
                overrides,
            });
        }
    }

    if saw_screen_item && saw_field_item {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0003Syntax,
            Some(span),
            String::from("cannot mix screens and fields in the same `DISPLAY` statement"),
        ));
    } else if saw_screen_clause_item && saw_non_crt_upon_item && items_slice.len() > 1 {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0003Syntax,
            Some(span),
            String::from(
                "ambiguous `DISPLAY`; put items to display on device in separate `DISPLAY`",
            ),
        ));
    }

    let (on_exception, not_on_exception) = lower_event_handlers(handlers, ctx);
    out.push(HirStmt::M19(Box::new(HirM19Stmt::DisplayScreen {
        items: lowered_items,
        on_exception,
        not_on_exception,
        span,
    })));
    true
}

/// Attempts to lower a `Stmt::Accept` as a Milestone M19 screen `ACCEPT`, returning `true` if handled.
#[expect(
    clippy::too_many_arguments,
    reason = "Inspects full AST ACCEPT parts + UnitScreenCatalog to disambiguate console vs TUI ACCEPT."
)]
pub(crate) fn try_lower_m19_accept_stmt(
    target: &coblang_syntax::ast::DataRef,
    source: &AcceptSource,
    screen_ext: Option<&ScreenAcceptStmtAst>,
    handlers: &EventHandlers,
    span: SourceSpan,
    pic_ctx: PictureContext,
    catalog: &UnitScreenCatalog,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) -> bool {
    let sid_opt = if target.qualifiers_and_subscripts_empty_m19() {
        catalog.lookup_screen(target.name.base)
    } else {
        None
    };
    let is_screen_src = matches!(source, AcceptSource::Screen);
    let is_plain_console = matches!(source, AcceptSource::InputDevice(None));
    let has_ext = screen_ext.is_some();

    if sid_opt.is_none()
        && !is_screen_src
        && !has_ext
        && !(catalog.console_is_crt && is_plain_console)
    {
        return false;
    }

    let clauses: &[ScreenClauseAst] = screen_ext.map_or(&[], |e| e.clauses.as_slice());
    let unpacked = unpack_screen_clause_slice(clauses, false, pic_ctx, ctx);
    check_accept_display_extensions_on_accept(sid_opt.is_some(), &unpacked, span, ctx);
    let overrides = build_hir_overrides(&unpacked);

    let adhoc_place = if sid_opt.is_none() {
        resolve_place(target, ctx)
    } else {
        None
    };

    let (on_exception, not_on_exception) = lower_event_handlers(handlers, ctx);
    out.push(HirStmt::M19(Box::new(HirM19Stmt::AcceptScreen {
        screen_id: sid_opt,
        adhoc_place,
        overrides: Box::new(overrides),
        on_exception,
        not_on_exception,
        span,
    })));
    true
}

/// Lowers `ACCEPT OMITTED ...` into [`HirStmt::M19`].
pub(crate) fn lower_m19_accept_omitted(
    spec: &ScreenAcceptStmtAst,
    pic_ctx: PictureContext,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let unpacked = unpack_screen_clause_slice(&spec.clauses, false, pic_ctx, ctx);
    check_accept_display_extensions_on_accept(false, &unpacked, spec.span, ctx);
    let overrides = build_hir_overrides(&unpacked);
    let (on_exception, not_on_exception) = lower_event_handlers(&spec.handlers, ctx);
    out.push(HirStmt::M19(Box::new(HirM19Stmt::AcceptScreen {
        screen_id: None,
        adhoc_place: None,
        overrides: Box::new(overrides),
        on_exception,
        not_on_exception,
        span: spec.span,
    })));
}

/// Lowers `SET screen-name ATTRIBUTE <attr> ON/OFF ...` (`syn_screen/0011`).
pub(crate) fn lower_m19_set_screen_attributes(
    target: &coblang_syntax::ast::DataRef,
    toggles: &[SetScreenAttrToggleAst],
    span: SourceSpan,
    catalog: &UnitScreenCatalog,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut on_bits = 0_u32;
    let mut off_bits = 0_u32;
    let mut touched_bits = 0_u32;

    for tog in toggles {
        let name = ctx.interner.resolve_canonical(tog.attr_name).unwrap_or("");
        let bit = match name {
            "BELL" | "BEEP" => ATTR_BELL,
            "BLINK" => ATTR_BLINK,
            "HIGHLIGHT" => {
                if (touched_bits & ATTR_LOWLIGHT) != 0 {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(tog.span),
                        String::from("cannot specify both `HIGHLIGHT` and `LOWLIGHT`"),
                    ));
                }
                ATTR_HIGHLIGHT
            }
            "LOWLIGHT" => {
                if (touched_bits & ATTR_HIGHLIGHT) != 0 {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(tog.span),
                        String::from("cannot specify both `HIGHLIGHT` and `LOWLIGHT`"),
                    ));
                }
                ATTR_LOWLIGHT
            }
            "REVERSE-VIDEO" | "REVERSED" | "REVERSE" => ATTR_REVERSE_VIDEO,
            "UNDERLINE" => ATTR_UNDERLINE,
            "OVERLINE" => ATTR_OVERLINE,
            "LEFTLINE" => ATTR_LEFTLINE,
            "AUTO" | "AUTO-SKIP" => ATTR_AUTO,
            "SECURE" | "NO-ECHO" => ATTR_SECURE,
            "REQUIRED" | "EMPTY-CHECK" => ATTR_REQUIRED,
            "FULL" | "LENGTH-CHECK" => ATTR_FULL,
            "PROMPT" => ATTR_PROMPT,
            _ => 0,
        };
        touched_bits |= bit;
        if tog.enabled {
            on_bits |= bit;
            off_bits &= !bit;
        } else {
            off_bits |= bit;
            on_bits &= !bit;
        }
    }

    if let Some(sid) = catalog.lookup_screen(target.name.base) {
        out.push(HirStmt::M19(Box::new(HirM19Stmt::SetScreenAttr {
            screen_id: sid,
            on_bits,
            off_bits,
            span,
        })));
    } else {
        let _ = resolve_place(target, ctx);
    }
}

// ============================================================================
// Internal Coordinate, `AT`, & `-faccept-display-extensions` Helpers
// ============================================================================

/// Enforces `-faccept-display-extensions` disposition on `DISPLAY` (`syn_screen/0004`).
fn check_accept_display_extensions_on_display(
    is_screen_item: bool,
    item_count: usize,
    u: &UnpackedScreenClauses,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(&disp) = ctx.dialect.feature_rules.get("accept-display-extensions") else {
        return;
    };
    if disp == FeatureDisposition::Ok {
        return;
    }
    if let Some((_, _, at_sp)) = u.at_combined {
        emit_feature_disp(disp, "`AT` clause used", at_sp, ctx);
    }
    if let Some(z_sp) = u.line_is_zero_const {
        emit_feature_disp(disp, "`LINE 0` used", z_sp, ctx);
    }
    if let Some(z_sp) = u.col_is_zero_const {
        emit_feature_disp(disp, "`COLUMN 0` used", z_sp, ctx);
    }
    let non_std = if is_screen_item {
        item_count > 1 || u.has_with_ext_clause || u.at_combined.is_some() || u.has_ms_pos.is_some()
    } else {
        u.line.is_some()
            || u.col.is_some()
            || u.at_combined.is_some()
            || u.has_ms_pos.is_some()
            || u.has_with_ext_clause
    };
    if non_std {
        emit_feature_disp(disp, "non-standard `DISPLAY` used", span, ctx);
    }
}

/// Enforces `-faccept-display-extensions` disposition on `ACCEPT` (`syn_screen/0004`).
fn check_accept_display_extensions_on_accept(
    is_screen_item: bool,
    u: &UnpackedScreenClauses,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(&disp) = ctx.dialect.feature_rules.get("accept-display-extensions") else {
        return;
    };
    if disp == FeatureDisposition::Ok {
        return;
    }
    if let Some((_, _, at_sp)) = u.at_combined {
        emit_feature_disp(disp, "`AT` clause used", at_sp, ctx);
    }
    if let Some(z_sp) = u.line_is_zero_const {
        emit_feature_disp(disp, "`LINE 0` used", z_sp, ctx);
    }
    if let Some(z_sp) = u.col_is_zero_const {
        emit_feature_disp(disp, "`COLUMN 0` used", z_sp, ctx);
    }
    let non_std = if is_screen_item {
        u.has_with_ext_clause || u.at_combined.is_some() || u.has_ms_pos.is_some()
    } else {
        u.line.is_some()
            || u.col.is_some()
            || u.at_combined.is_some()
            || u.has_ms_pos.is_some()
            || u.has_with_ext_clause
    };
    if non_std {
        emit_feature_disp(disp, "non-standard `ACCEPT` used", span, ctx);
    }
}

/// Emits warning or error according to [`FeatureDisposition`].
fn emit_feature_disp(
    disp: FeatureDisposition,
    msg: &str,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    match disp {
        FeatureDisposition::Ok | FeatureDisposition::Ignore => {}
        FeatureDisposition::Warning
        | FeatureDisposition::Archaic
        | FeatureDisposition::Obsolete => {
            emit_warn(span, msg, ctx);
        }
        FeatureDisposition::Error | FeatureDisposition::Unconformable => {
            emit_err(span, msg, ctx);
        }
    }
}

/// Validates and lowers a `ScreenCoordAst` (`syn_screen/0004`, `0007`).
fn lower_screen_coord(
    coord: &ScreenCoordAst,
    in_screen_section: bool,
    ctx: &mut UnitLowerCtx<'_>,
) -> (HirScreenCoordSpec, bool, Option<SourceSpan>) {
    if in_screen_section && let Some(bad_sp) = coord.malformed_literal {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(bad_sp),
            String::from("unsigned integer value expected in `SCREEN SECTION` `LINE`/`COLUMN`"),
        ));
    }
    let is_rel = coord.dir != ScreenCoordDirAst::Absolute;
    let Some(expr) = &coord.expr else {
        return (HirScreenCoordSpec::Absolute(1), is_rel, None);
    };
    let (he, _) = lower_expr_with_cat(expr, ctx);
    let zero_sp = if !is_rel && is_compile_time_zero_expr(&he, ctx.interner) {
        Some(coord.span)
    } else {
        None
    };
    (expr_to_coord_spec(he, coord.dir), is_rel, zero_sp)
}

/// Converts a lowered [`HirExpr`] + direction into [`HirScreenCoordSpec`].
fn expr_to_coord_spec(expr: HirExpr, dir: ScreenCoordDirAst) -> HirScreenCoordSpec {
    if let HirExpr::Const {
        value: HirConstValue::Figurative(HirFigurative::Zero),
        ..
    } = &expr
    {
        return match dir {
            ScreenCoordDirAst::Absolute => HirScreenCoordSpec::Absolute(0),
            ScreenCoordDirAst::Plus => HirScreenCoordSpec::Plus(0),
            ScreenCoordDirAst::Minus => HirScreenCoordSpec::Minus(0),
        };
    }
    match dir {
        ScreenCoordDirAst::Absolute => HirScreenCoordSpec::Dynamic(expr),
        ScreenCoordDirAst::Plus => HirScreenCoordSpec::DynamicPlus(expr),
        ScreenCoordDirAst::Minus => HirScreenCoordSpec::DynamicMinus(expr),
    }
}

/// Evaluates whether `he` is a compile-time `0` / `ZERO` constant.
fn is_compile_time_zero_expr(he: &HirExpr, interner: &SymbolInterner) -> bool {
    match he {
        HirExpr::Const {
            value: HirConstValue::Figurative(HirFigurative::Zero),
            ..
        } => true,
        HirExpr::Const {
            value: HirConstValue::Decimal { text, .. },
            ..
        } => interner
            .resolve_original(*text)
            .is_ok_and(|s| s.bytes().all(|b| b == b'0')),
        _ => false,
    }
}

/// Validates and lowers `AT <expr>` (`syn_screen/0003`: 4 or 6 digits, numeric or group, no `ZERO`).
fn validate_and_lower_at_expr(
    expr: &Expr,
    post_span: SourceSpan,
    clause_span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<(HirExpr, u8)> {
    if matches!(
        expr,
        Expr::Literal {
            atom: ValueAtom::Figurative(FigurativeValue::Zero),
            ..
        }
    ) {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(post_span),
            String::from("cannot specify figurative constant `ZERO` in `AT` clause"),
        ));
        return None;
    }

    if let Expr::Literal {
        atom: ValueAtom::Integer(sym),
        ..
    } = expr
    {
        let clean_len = ctx
            .interner
            .resolve_original(*sym)
            .unwrap_or("")
            .trim_start_matches(['+', '-'])
            .len();
        let (he, _) = lower_expr_with_cat(expr, ctx);
        if clean_len == 4 || clean_len == 6 {
            return Some((he, u8::try_from(clean_len).unwrap_or(4)));
        }
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(clause_span),
            String::from("value in `AT` clause must have 4 or 6 digits"),
        ));
        return None;
    }

    let (he, _) = lower_expr_with_cat(expr, ctx);
    match &he {
        HirExpr::Const {
            value: HirConstValue::Decimal { text, .. },
            ..
        } => {
            let raw = ctx.interner.resolve_original(*text).unwrap_or("");
            let clean = raw.trim_start_matches(['+', '-']);
            if clean.len() == 4 || clean.len() == 6 {
                return Some((he, u8::try_from(clean.len()).unwrap_or(4)));
            }
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(clause_span),
                String::from("value in `AT` clause must have 4 or 6 digits"),
            ));
            None
        }
        HirExpr::Place(pl) => match &pl.effective_ty {
            HirType::ZonedDecimal { digits, .. }
            | HirType::PackedDecimal { digits, .. }
            | HirType::BinaryInt { digits, .. } => {
                let d = digits.get();
                if d == 4 || d == 6 {
                    Some((he, d))
                } else {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(clause_span),
                        String::from("value in `AT` clause must have 4 or 6 digits"),
                    ));
                    None
                }
            }
            HirType::Group {
                static_byte_len: byte_len,
                ..
            } => {
                if *byte_len == 4 || *byte_len == 6 {
                    let digits = u8::try_from(*byte_len).unwrap_or(4);
                    // Reinterpret group bytes as unsigned ZonedDecimal so runtime reads ASCII digits!
                    let mut num_pl = pl.clone();
                    num_pl.effective_ty = HirType::ZonedDecimal {
                        digits: DigitCount::try_new(digits).unwrap_or(DigitCount::ONE),
                        scale: ScaleFactor::ZERO,
                        sign: NumericSignSpec::Unsigned,
                        blank_zero: BlankZeroSpec::RetainZero,
                    };
                    Some((HirExpr::Place(num_pl), digits))
                } else {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(clause_span),
                        String::from("value in `AT` clause must have 4 or 6 digits"),
                    ));
                    None
                }
            }
            _ => {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(clause_span),
                    String::from("value in `AT` clause is not numeric"),
                ));
                None
            }
        },
        _ => Some((he, 4)),
    }
}

/// Lowers a `PROMPT [CHARACTER IS expr]` clause.
fn lower_prompt_spec(char_expr: Option<&Expr>, ctx: &mut UnitLowerCtx<'_>) -> HirScreenPromptSpec {
    let Some(e) = char_expr else {
        return HirScreenPromptSpec::StaticByte(b'_');
    };
    match e {
        Expr::Literal {
            atom: ValueAtom::String { bytes, .. },
            ..
        } => HirScreenPromptSpec::StaticByte(bytes.first().copied().unwrap_or(b'_')),
        Expr::Literal {
            atom: ValueAtom::Figurative(FigurativeValue::Space),
            ..
        } => HirScreenPromptSpec::StaticByte(b' '),
        Expr::Data(dref) => {
            if let Some(pl) = resolve_place(dref, ctx) {
                HirScreenPromptSpec::Dynamic(pl)
            } else {
                HirScreenPromptSpec::StaticByte(b'_')
            }
        }
        _ => {
            let _ = lower_expr_with_cat(e, ctx);
            HirScreenPromptSpec::StaticByte(b'_')
        }
    }
}

/// Lowers `FOREGROUND-COLOR` / `BACKGROUND-COLOR` into [`HirScreenColorSpec`].
fn lower_color_spec(expr: &Expr, ctx: &mut UnitLowerCtx<'_>) -> HirScreenColorSpec {
    let (he, _) = lower_expr_with_cat(expr, ctx);
    if let HirExpr::Const {
        value: HirConstValue::Decimal { text, .. },
        ..
    } = &he
        && let Ok(raw) = ctx.interner.resolve_original(*text)
        && let Ok(code) = raw.trim_start_matches('+').parse::<u8>()
    {
        return HirScreenColorSpec::Static(code);
    }
    HirScreenColorSpec::Dynamic(he)
}

/// Converts [`UnpackedScreenClauses`] into statement-level [`HirScreenOverrides`].
fn build_hir_overrides(u: &UnpackedScreenClauses) -> HirScreenOverrides {
    HirScreenOverrides {
        at_combined: u.at_combined.as_ref().map(|(e, d, _)| (e.clone(), *d)),
        line: u.line.clone().unwrap_or(HirScreenCoordSpec::Omitted),
        col: u.col.clone().unwrap_or(HirScreenCoordSpec::Omitted),
        attr_bits: u.attr_bits,
        force_no_update: u.force_no_update,
        fg_color: u.fg_color.clone(),
        bg_color: u.bg_color.clone(),
        size_expr: u.size_expr.clone(),
        prompt: u.prompt.clone(),
        timeout_expr: u.timeout_expr.clone(),
        timeout_scale_ms: u.timeout_scale_ms,
        cursor_offset_place: u.cursor_offset_place.clone(),
        cursor_offset_expr: u.cursor_offset_expr.clone(),
        scroll_lines: u.scroll_lines.clone(),
    }
}

/// Returns `Some(screen_id)` if `expr` is a bare `DataRef` naming a `SCREEN SECTION` item.
fn expr_screen_id(expr: &Expr, catalog: &UnitScreenCatalog) -> Option<u32> {
    if let Expr::Data(dref) = expr
        && dref.qualifiers_and_subscripts_empty_m19()
    {
        return catalog.lookup_screen(dref.name.base);
    }
    None
}

/// Returns `true` if `sym` is `CRT` or `CRT-UNDER`.
fn is_crt_device_sym(sym: SymbolId, interner: &SymbolInterner) -> bool {
    matches!(
        interner.resolve_canonical(sym).ok(),
        Some("CRT" | "CRT-UNDER")
    )
}

/// Returns `true` if `sym` targets a non-CRT stream (`SYSERR`, `SYSOUT`, `CONSOLE`, or declared mnemonic).
fn is_non_crt_upon(sym: SymbolId, catalog: &UnitScreenCatalog, interner: &SymbolInterner) -> bool {
    if is_crt_device_sym(sym, interner) {
        return false;
    }
    catalog.non_crt_devices.contains(&sym)
        || matches!(
            interner.resolve_canonical(sym).ok(),
            Some("SYSERR" | "STANDARD-ERROR" | "SYSOUT" | "SYSPRINT" | "PRINTER" | "CONSOLE")
        )
}

/// Extracts numeric level (`1`..=`77`) for `SCREEN SECTION` hierarchy bounds.
const fn screen_level_u8(entry: &ScreenEntry) -> u8 {
    match entry.level {
        LevelNumber::Hierarchy(n) => n,
        LevelNumber::Independent77 => 1,
        _ => 88,
    }
}

/// Finds the exclusive end index of `entries[idx]`'s subordinate subtree (`level > parent_lvl`).
fn find_screen_subtree_end(entries: &[ScreenEntry], idx: usize, parent_lvl: u8) -> usize {
    let mut cur = idx + 1;
    while let Some(nxt) = entries.get(cur) {
        if matches!(nxt.level, LevelNumber::Condition88) {
            cur += 1;
            continue;
        }
        if screen_level_u8(nxt) <= parent_lvl {
            break;
        }
        cur += 1;
    }
    cur
}

/// Converts a symbol path `[base, qual...]` into a plain [`coblang_syntax::ast::DataRef`].
fn path_to_dataref(path: &[SymbolId], span: SourceSpan) -> Option<coblang_syntax::ast::DataRef> {
    let (&base, rest) = path.split_first()?;
    Some(coblang_syntax::ast::DataRef {
        modifier: coblang_syntax::ast::DataRefModifier::Plain,
        name: coblang_syntax::ast::QualifiedName {
            base,
            qualifiers: rest.to_vec(),
            span,
        },
        subscripts: Vec::new(),
        refmod: None,
        span,
    })
}

/// Trait-like helper checking whether a `DataRef` has no qualifiers, subscripts, or refmod.
trait BareScreenRefExt {
    /// Returns `true` when `qualifiers`, `subscripts`, and `refmod` are empty.
    fn qualifiers_and_subscripts_empty_m19(&self) -> bool;
}

impl BareScreenRefExt for coblang_syntax::ast::DataRef {
    fn qualifiers_and_subscripts_empty_m19(&self) -> bool {
        self.name.qualifiers.is_empty() && self.subscripts.is_empty() && self.refmod.is_none()
    }
}
