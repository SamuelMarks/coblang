//! Comprehensive integration and 100% branch-coverage test suite for `coblang-report`.

#![deny(clippy::indexing_slicing)]

use std::io::{self, Write};

use coblang_common::{CoblangError, DigitCount, LevelNumber, Result, ScaleFactor};
use coblang_decimal::FixedDecimal;
use coblang_hir::{BlankZeroSpec, HirType, JustifiedSpec, NumericSignSpec, PicEditOp, Signedness};
use coblang_pic::MoveFieldDesc;
use coblang_report::{
    ColumnPositionSpec, ConditionId, ControlDef, ControlKind, ControlLevelIndex, FieldFormatter,
    FieldId, FieldValue, FlatReportEntry, GenerateTarget, GroupIndicatePolicy, ItemValueSource,
    LinePositionSpec, MemoryReportEnv, NextGroupSpec, PageBreakStyle, PageLimits, PresentWhenSpec,
    RenderFormat, ReportDesc, ReportEnv, ReportFieldItem, ReportGroupDesc, ReportGroupKind,
    ReportHookContext, ReportLifecycleState, ReportLine, ReportLineAdvancing, ReportNode,
    ReportState, ReportSubGroup, SumAddend, SumCounterDesc, SumCounterId, SumUponFilter,
    build_group_from_flat_entries, render_lines_to_bytes, write_rendered_lines,
};

/// Mock writer that fails either on `write` or on `flush` to cover all I/O error branches.
struct MockWriter {
    /// Fail immediately during `write`.
    fail_on_write: bool,
    /// Fail during `flush`.
    fail_on_flush: bool,
}

impl Write for MockWriter {
    /// Writes `buf` or injects a synthetic I/O failure.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.fail_on_write {
            return Err(io::Error::other("injected write failure"));
        }
        Ok(buf.len())
    }

    /// Flushes or injects a synthetic I/O failure.
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_on_flush {
            return Err(io::Error::other("injected flush failure"));
        }
        Ok(())
    }
}

/// Custom [`ReportEnv`] exercising `ReportHookContext` inspection, `SUPPRESS PRINTING`, and hook errors.
struct HookProbeEnv {
    /// Behavior selector (`1` = suppress printing, `2` = return error).
    mode: u8,
}

impl ReportEnv for HookProbeEnv {
    /// Returns an empty raw [`FieldValue`].
    fn read_field(&self, _field_id: FieldId) -> Result<FieldValue> {
        Ok(FieldValue::raw(b""))
    }

    /// No-op control restoration.
    fn write_control_bytes(&mut self, _field_id: FieldId, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }

    /// Always returns `true`.
    fn eval_condition(&self, _condition_id: ConditionId) -> Result<bool> {
        Ok(true)
    }

    /// Exercises `ReportHookContext` getters, `set_page_counter`, `suppress_printing`, and hook errors.
    fn before_reporting(&mut self, ctx: &mut ReportHookContext<'_>) -> Result<()> {
        assert_eq!(ctx.report_name(), "MIN-REP");
        assert_eq!(ctx.group_name(), Some("DET-1"));
        assert_eq!(ctx.group_kind(), ReportGroupKind::Detail);
        assert_eq!(ctx.line_counter(), 0);
        if self.mode == 1 {
            assert_eq!(ctx.page_counter(), 1);
            ctx.set_page_counter(5);
            assert_eq!(ctx.page_counter(), 5);
        }
        assert!(!ctx.is_suppressed());
        assert!(ctx.sum_counter(SumCounterId::new(9)).is_err());
        if self.mode == 1 {
            ctx.suppress_printing();
            assert!(ctx.is_suppressed());
        } else if self.mode == 2 {
            return Err(CoblangError::ReportEngine {
                report_name: String::from("MIN-REP"),
                message: String::from("user hook failure"),
            });
        }
        Ok(())
    }
}

/// Helper constructing an elementary printable literal item at `col` with optional `line`.
fn lit_item(line: Option<LinePositionSpec>, col: u32, text: &[u8]) -> ReportNode {
    ReportNode::Elementary(ReportFieldItem {
        name: None,
        line,
        column: Some(ColumnPositionSpec::Absolute(col)),
        source: Some(ItemValueSource::Literal(text.to_vec())),
        formatter: FieldFormatter::RawBytes,
        group_indicate: GroupIndicatePolicy::AlwaysDisplay,
        present_when: PresentWhenSpec::Always,
    })
}

/// Helper constructing an elementary printable `SOURCE` field item at `col`.
fn src_item(
    line: Option<LinePositionSpec>,
    col: u32,
    fid: FieldId,
    indicate: GroupIndicatePolicy,
) -> ReportNode {
    ReportNode::Elementary(ReportFieldItem {
        name: None,
        line,
        column: Some(ColumnPositionSpec::Absolute(col)),
        source: Some(ItemValueSource::SourceField(fid)),
        formatter: FieldFormatter::RawBytes,
        group_indicate: indicate,
        present_when: PresentWhenSpec::Always,
    })
}

/// Helper constructing a minimal valid `ReportDesc` with one `DETAIL` group.
fn minimal_valid_desc() -> ReportDesc {
    ReportDesc {
        report_name: String::from("MIN-REP"),
        code_prefix: Vec::new(),
        page_limits: PageLimits::try_new(60, None, None, None, None).ok(),
        controls: Vec::new(),
        groups: vec![ReportGroupDesc {
            name: Some(String::from("DET-1")),
            kind: ReportGroupKind::Detail,
            line: Some(LinePositionSpec::relative(1)),
            next_group: None,
            present_when: PresentWhenSpec::Always,
            children: vec![lit_item(None, 1, b"ROW")],
        }],
        sum_counters: Vec::new(),
    }
}

/// Verifies identifier newtypes, `ControlDef`, `ReportGroupKind`, and positioning spec constructors.
#[test]
fn domain_newtypes_and_positioning_specs() -> Result<()> {
    let fid = FieldId::new(7);
    assert_eq!(fid.as_u32(), 7);
    let sid = SumCounterId::new(3);
    assert_eq!((sid.as_u32(), sid.as_usize()), (3, 3));
    let cid = ConditionId::new(11);
    assert_eq!(cid.as_u32(), 11);

    let cli = ControlLevelIndex::new(2);
    assert_eq!((cli.as_u16(), cli.as_usize()), (2, 2));
    assert_eq!(ControlLevelIndex::ZERO.as_u16(), 0);
    assert_eq!(ControlLevelIndex::try_from_usize(5)?.as_u16(), 5);
    assert!(ControlLevelIndex::try_from_usize(usize::from(u16::MAX) + 1).is_err());

    assert_eq!(ControlDef::final_control().kind, ControlKind::Final);
    assert_eq!(
        ControlDef::field("WS-DEPT", fid).kind,
        ControlKind::DataField(fid)
    );

    let kinds = [
        (ReportGroupKind::ReportHeading, false, None, "RH"),
        (ReportGroupKind::PageHeading, false, None, "PH"),
        (ReportGroupKind::ControlHeading(cli), true, Some(cli), "CH"),
        (ReportGroupKind::Detail, true, None, "DE"),
        (ReportGroupKind::ControlFooting(cli), true, Some(cli), "CF"),
        (ReportGroupKind::PageFooting, false, None, "PF"),
        (ReportGroupKind::ReportFooting, false, None, "RF"),
    ];
    for (k, is_body, lvl, code) in kinds {
        assert_eq!(
            (k.is_body_group(), k.control_level(), k.short_code()),
            (is_body, lvl, code)
        );
    }

    assert!(LinePositionSpec::try_absolute(0).is_err());
    assert_eq!(
        LinePositionSpec::try_absolute(4)?,
        LinePositionSpec::Absolute(4)
    );
    assert!(LinePositionSpec::try_on_next_page(0).is_err());
    assert_eq!(
        LinePositionSpec::try_on_next_page(2)?,
        LinePositionSpec::OnNextPage(2)
    );
    assert_eq!(LinePositionSpec::relative(0), LinePositionSpec::Relative(0));

    assert!(ColumnPositionSpec::try_absolute(0).is_err());
    assert_eq!(
        ColumnPositionSpec::try_absolute(10)?,
        ColumnPositionSpec::Absolute(10)
    );
    assert_eq!(
        ColumnPositionSpec::relative(2),
        ColumnPositionSpec::Relative(2)
    );

    assert!(NextGroupSpec::try_absolute(0).is_err());
    assert_eq!(
        NextGroupSpec::try_absolute(12)?,
        NextGroupSpec::Absolute(12)
    );
    assert!(NextGroupSpec::try_relative(0).is_err());
    assert_eq!(NextGroupSpec::try_relative(3)?, NextGroupSpec::Relative(3));
    assert_eq!(NextGroupSpec::next_page(), NextGroupSpec::NextPage);
    Ok(())
}

/// Verifies `PageLimits` default zone resolution and invalid ordering errors.
#[test]
fn page_limits_defaults_and_invariants() -> Result<()> {
    assert!(PageLimits::try_new(0, None, None, None, None).is_err());
    assert!(PageLimits::try_new(10, Some(0), None, None, None).is_err());
    assert!(PageLimits::try_new(10, Some(5), Some(3), None, None).is_err());
    assert!(PageLimits::try_new(10, Some(2), Some(5), Some(4), None).is_err());
    assert!(PageLimits::try_new(10, Some(2), Some(4), Some(6), Some(5)).is_err());
    assert!(PageLimits::try_new(10, Some(2), Some(4), Some(6), Some(11)).is_err());

    let pl_def = PageLimits::try_new(60, None, None, None, None)?;
    assert_eq!(
        (
            pl_def.page_limit(),
            pl_def.heading(),
            pl_def.first_detail(),
            pl_def.last_detail(),
            pl_def.footing()
        ),
        (60, 1, 1, 60, 60)
    );
    let pl_ld = PageLimits::try_new(60, Some(2), Some(5), Some(50), None)?;
    assert_eq!((pl_ld.last_detail(), pl_ld.footing()), (50, 50));
    let pl_ft = PageLimits::try_new(60, Some(2), None, None, Some(55))?;
    assert_eq!(
        (pl_ft.first_detail(), pl_ft.last_detail(), pl_ft.footing()),
        (2, 55, 55)
    );
    let pl_all = PageLimits::try_new(60, Some(1), Some(4), Some(50), Some(55))?;
    assert_eq!((pl_all.last_detail(), pl_all.footing()), (50, 55));
    Ok(())
}

/// Builds a 4-line fixture covering initial skip, overprint, multi-line advance, and page break.
fn sample_render_lines() -> Vec<ReportLine> {
    vec![
        ReportLine {
            page_number: 1,
            line_number: 2,
            advancing: ReportLineAdvancing::InitialLine { skip_before: 1 },
            group_kind: ReportGroupKind::PageHeading,
            code_prefix: b"R1".to_vec(),
            content: b"HEAD".to_vec(),
        },
        ReportLine {
            page_number: 1,
            line_number: 2,
            advancing: ReportLineAdvancing::SamePage { lines_down: 0 },
            group_kind: ReportGroupKind::Detail,
            code_prefix: b"R1".to_vec(),
            content: b"OVER".to_vec(),
        },
        ReportLine {
            page_number: 1,
            line_number: 4,
            advancing: ReportLineAdvancing::SamePage { lines_down: 2 },
            group_kind: ReportGroupKind::Detail,
            code_prefix: b"R1".to_vec(),
            content: b"BODY".to_vec(),
        },
        ReportLine {
            page_number: 2,
            line_number: 2,
            advancing: ReportLineAdvancing::NewPage { skip_before: 1 },
            group_kind: ReportGroupKind::PageHeading,
            code_prefix: b"R1".to_vec(),
            content: b"P2".to_vec(),
        },
    ]
}

/// Verifies `render_lines_to_bytes` and `write_rendered_lines` across all formats and writer failures.
#[test]
fn line_rendering_and_writer_io() -> Result<()> {
    assert_eq!(
        render_lines_to_bytes(
            &[],
            RenderFormat::LineSequential {
                page_break: PageBreakStyle::FormFeed
            }
        ),
        [] as [u8; 0]
    );
    let lines = sample_render_lines();

    assert_eq!(
        render_lines_to_bytes(
            &lines,
            RenderFormat::LineSequential {
                page_break: PageBreakStyle::FormFeed
            }
        ),
        b"R1\x0aR1HEAD\x0dR1OVER\x0aR1\x0aR1BODY\x0a\x0cR1\x0aR1P2\x0a"
    );
    assert_eq!(
        render_lines_to_bytes(
            &lines,
            RenderFormat::LineSequential {
                page_break: PageBreakStyle::BlankLinesOnly
            }
        ),
        b"R1\x0aR1HEAD\x0dR1OVER\x0aR1\x0aR1BODY\x0aR1\x0aR1P2\x0a"
    );
    let pres_exact =
        render_lines_to_bytes(&lines, RenderFormat::PresentedRecords { fixed_width: None });
    assert_eq!(pres_exact, b"R1HEAD\x0aR1OVER\x0aR1BODY\x0aR1P2\x0a");
    assert_eq!(
        render_lines_to_bytes(
            &lines,
            RenderFormat::PresentedRecords {
                fixed_width: Some(8)
            }
        ),
        b"R1HEAD  \x0aR1OVER  \x0aR1BODY  \x0aR1P2    \x0a"
    );
    assert_eq!(
        render_lines_to_bytes(
            &lines,
            RenderFormat::PresentedRecords {
                fixed_width: Some(4)
            }
        ),
        b"R1HE\x0aR1OV\x0aR1BO\x0aR1P2\x0a"
    );
    assert_eq!(
        render_lines_to_bytes(
            &lines,
            RenderFormat::PresentedRecords {
                fixed_width: Some(6)
            }
        ),
        b"R1HEAD\x0aR1OVER\x0aR1BODY\x0aR1P2  \x0a"
    );

    let mut ok_buf = Vec::new();
    write_rendered_lines(
        &lines,
        RenderFormat::PresentedRecords { fixed_width: None },
        &mut ok_buf,
    )?;
    assert_eq!(ok_buf, pres_exact);

    let mut fail_w = MockWriter {
        fail_on_write: true,
        fail_on_flush: false,
    };
    assert!(
        write_rendered_lines(
            &lines,
            RenderFormat::PresentedRecords { fixed_width: None },
            &mut fail_w
        )
        .is_err()
    );

    let mut fail_f = MockWriter {
        fail_on_write: false,
        fail_on_flush: true,
    };
    assert!(
        write_rendered_lines(
            &lines,
            RenderFormat::PresentedRecords { fixed_width: None },
            &mut fail_f
        )
        .is_err()
    );
    Ok(())
}

/// Verifies `build_group_from_flat_entries` root and single-entry cases.
#[test]
fn flat_entries_root_and_single_entry_cases() -> Result<()> {
    assert!(build_group_from_flat_entries(&[], "REP").is_err());

    let base = FlatReportEntry {
        level: LevelNumber::Hierarchy(1),
        name: Some(String::from("G1")),
        kind: Some(ReportGroupKind::Detail),
        line: Some(LinePositionSpec::relative(1)),
        next_group: None,
        column: None,
        source: None,
        formatter: FieldFormatter::RawBytes,
        group_indicate: GroupIndicatePolicy::AlwaysDisplay,
        present_when: PresentWhenSpec::Always,
    };

    let mut not_01 = base.clone();
    not_01.level = LevelNumber::Hierarchy(5);
    assert!(build_group_from_flat_entries(&[not_01], "REP").is_err());

    let mut no_kind = base.clone();
    no_kind.kind = None;
    assert!(build_group_from_flat_entries(&[no_kind], "REP").is_err());

    let single_empty = build_group_from_flat_entries(std::slice::from_ref(&base), "REP")?;
    assert_eq!(single_empty.children, []);

    let mut single_col = base.clone();
    single_col.column = Some(ColumnPositionSpec::Absolute(1));
    assert_eq!(
        build_group_from_flat_entries(&[single_col], "REP")?
            .children
            .len(),
        1
    );

    let mut single_src = base.clone();
    single_src.source = Some(ItemValueSource::Literal(b"X".to_vec()));
    assert_eq!(
        build_group_from_flat_entries(&[single_src], "REP")?
            .children
            .len(),
        1
    );

    let mut single_ind = base;
    single_ind.group_indicate = GroupIndicatePolicy::GroupIndicate;
    assert_eq!(
        build_group_from_flat_entries(&[single_ind], "REP")?
            .children
            .len(),
        1
    );
    Ok(())
}

/// Verifies `build_group_from_flat_entries` nested hierarchy assembly and sub-group clause rejections.
#[test]
fn flat_entries_nested_tree_and_errors() -> Result<()> {
    let root = FlatReportEntry {
        level: LevelNumber::Hierarchy(1),
        name: Some(String::from("G1")),
        kind: Some(ReportGroupKind::Detail),
        line: None,
        next_group: None,
        column: None,
        source: None,
        formatter: FieldFormatter::RawBytes,
        group_indicate: GroupIndicatePolicy::AlwaysDisplay,
        present_when: PresentWhenSpec::Always,
    };
    let leaf_05 = FlatReportEntry {
        level: LevelNumber::Hierarchy(5),
        name: None,
        kind: None,
        line: Some(LinePositionSpec::relative(1)),
        next_group: None,
        column: Some(ColumnPositionSpec::Absolute(1)),
        source: Some(ItemValueSource::Literal(b"A".to_vec())),
        formatter: FieldFormatter::RawBytes,
        group_indicate: GroupIndicatePolicy::AlwaysDisplay,
        present_when: PresentWhenSpec::Always,
    };

    let mut r_col = root.clone();
    r_col.column = Some(ColumnPositionSpec::Absolute(1));
    assert!(build_group_from_flat_entries(&[r_col, leaf_05.clone()], "REP").is_err());

    let mut r_src = root.clone();
    r_src.source = Some(ItemValueSource::PageCounter);
    assert!(build_group_from_flat_entries(&[r_src, leaf_05.clone()], "REP").is_err());

    let mut r_ind = root.clone();
    r_ind.group_indicate = GroupIndicatePolicy::GroupIndicate;
    assert!(build_group_from_flat_entries(&[r_ind, leaf_05.clone()], "REP").is_err());

    let mut bad_88 = leaf_05.clone();
    bad_88.level = LevelNumber::Condition88;
    assert!(build_group_from_flat_entries(&[root.clone(), bad_88], "REP").is_err());

    let mut bad_01 = leaf_05.clone();
    bad_01.level = LevelNumber::Hierarchy(1);
    assert!(build_group_from_flat_entries(&[root.clone(), bad_01], "REP").is_err());

    let mut c_kind = leaf_05.clone();
    c_kind.kind = Some(ReportGroupKind::Detail);
    assert!(build_group_from_flat_entries(&[root.clone(), c_kind], "REP").is_err());

    let mut c_ng = leaf_05.clone();
    c_ng.next_group = Some(NextGroupSpec::NextPage);
    assert!(build_group_from_flat_entries(&[root.clone(), c_ng], "REP").is_err());

    let mut leaf_10 = leaf_05.clone();
    leaf_10.level = LevelNumber::Hierarchy(10);
    assert!(
        build_group_from_flat_entries(&[root.clone(), leaf_05.clone(), leaf_10.clone()], "REP")
            .is_err()
    );

    let mut sub_src = leaf_05.clone();
    sub_src.column = None;
    assert!(
        build_group_from_flat_entries(&[root.clone(), sub_src, leaf_10.clone()], "REP").is_err()
    );

    let mut sub_ind = leaf_05.clone();
    sub_ind.column = None;
    sub_ind.source = None;
    sub_ind.group_indicate = GroupIndicatePolicy::GroupIndicate;
    assert!(
        build_group_from_flat_entries(&[root.clone(), sub_ind, leaf_10.clone()], "REP").is_err()
    );

    let mut clean_sub_05 = leaf_05.clone();
    clean_sub_05.column = None;
    clean_sub_05.source = None;
    let built = build_group_from_flat_entries(&[root, clean_sub_05, leaf_10, leaf_05], "REP")?;
    assert_eq!(built.children.len(), 2);
    Ok(())
}

/// Verifies `ReportDesc::validate` for controls, group multiplicities, and `NEXT GROUP` rules.
#[test]
fn descriptor_validate_controls_and_groups() -> Result<()> {
    let mut d = minimal_valid_desc();
    d.validate()?;

    d.report_name = String::from("  ");
    assert!(d.validate().is_err());

    d = minimal_valid_desc();
    d.controls = vec![ControlDef::field(" ", FieldId::new(1))];
    assert!(d.validate().is_err());

    d.controls = vec![
        ControlDef::field("DEPT", FieldId::new(1)),
        ControlDef::final_control(),
    ];
    assert!(d.validate().is_err());

    d.controls = vec![
        ControlDef::field("DEPT", FieldId::new(1)),
        ControlDef::field("dept", FieldId::new(2)),
    ];
    assert!(d.validate().is_err());

    d.controls = vec![
        ControlDef::field("DEPT", FieldId::new(1)),
        ControlDef::field("TEAM", FieldId::new(1)),
    ];
    assert!(d.validate().is_err());

    for bad_ctrl_kind in [
        ReportGroupKind::ControlHeading(ControlLevelIndex::ZERO),
        ReportGroupKind::ControlFooting(ControlLevelIndex::ZERO),
    ] {
        d = minimal_valid_desc();
        d.groups.push(ReportGroupDesc {
            name: None,
            kind: bad_ctrl_kind,
            line: Some(LinePositionSpec::relative(1)),
            next_group: None,
            present_when: PresentWhenSpec::Always,
            children: Vec::new(),
        });
        assert!(d.validate().is_err());
    }

    // Duplicate named DETAIL vs unnamed DETAILs (unnamed allowed!).
    d = minimal_valid_desc();
    if let Some(first) = d.groups.first().cloned() {
        d.groups.push(first);
    }
    assert!(d.validate().is_err());

    d = minimal_valid_desc();
    d.groups.push(ReportGroupDesc {
        name: None,
        kind: ReportGroupKind::Detail,
        line: Some(LinePositionSpec::relative(1)),
        next_group: None,
        present_when: PresentWhenSpec::Always,
        children: vec![lit_item(None, 1, b"U")],
    });
    d.validate()?;

    for dup_kind in [
        ReportGroupKind::ReportHeading,
        ReportGroupKind::PageHeading,
        ReportGroupKind::PageFooting,
        ReportGroupKind::ReportFooting,
    ] {
        d = minimal_valid_desc();
        let g = ReportGroupDesc {
            name: None,
            kind: dup_kind,
            line: Some(LinePositionSpec::Absolute(1)),
            next_group: None,
            present_when: PresentWhenSpec::Always,
            children: Vec::new(),
        };
        d.groups.push(g.clone());
        d.groups.push(g);
        assert!(d.validate().is_err());
    }
    Ok(())
}

/// Helper constructing a `01`-level `ReportGroupDesc` with `Always` visibility and no `NEXT GROUP`.
fn simple_grp(
    name: Option<&str>,
    kind: ReportGroupKind,
    line: Option<LinePositionSpec>,
    children: Vec<ReportNode>,
) -> ReportGroupDesc {
    ReportGroupDesc {
        name: name.map(String::from),
        kind,
        line,
        next_group: None,
        present_when: PresentWhenSpec::Always,
        children,
    }
}

/// Verifies `ReportDesc::validate` for unpaged constraints, `LINE`, `COLUMN`, `GROUP INDICATE`, and formatters.
#[test]
fn descriptor_validate_lines_nodes_and_formatters() {
    for unpaged_kind in [ReportGroupKind::PageHeading, ReportGroupKind::PageFooting] {
        let mut d = minimal_valid_desc();
        d.page_limits = None;
        d.groups.push(simple_grp(
            None,
            unpaged_kind,
            Some(LinePositionSpec::relative(1)),
            Vec::new(),
        ));
        assert!(d.validate().is_err());
    }

    let mut d = minimal_valid_desc();
    d.page_limits = None;
    if let Some(g) = d.groups.first_mut() {
        g.next_group = Some(NextGroupSpec::NextPage);
    }
    assert!(d.validate().is_err());

    d = minimal_valid_desc();
    d.page_limits = None;
    if let Some(g) = d.groups.first_mut() {
        g.line = Some(LinePositionSpec::OnNextPage(1));
    }
    assert!(d.validate().is_err());

    for forbidden_ng in [
        ReportGroupKind::PageHeading,
        ReportGroupKind::PageFooting,
        ReportGroupKind::ReportFooting,
    ] {
        d = minimal_valid_desc();
        let mut g = simple_grp(
            None,
            forbidden_ng,
            Some(LinePositionSpec::Absolute(1)),
            Vec::new(),
        );
        g.next_group = Some(NextGroupSpec::Relative(1));
        d.groups.push(g);
        assert!(d.validate().is_err());
    }

    d = minimal_valid_desc();
    if let Some(g) = d.groups.first_mut() {
        g.next_group = Some(NextGroupSpec::Absolute(61));
    }
    assert!(d.validate().is_err());

    for bad_line in [
        LinePositionSpec::Absolute(0),
        LinePositionSpec::Absolute(61),
        LinePositionSpec::OnNextPage(0),
        LinePositionSpec::OnNextPage(61),
    ] {
        d = minimal_valid_desc();
        if let Some(g) = d.groups.first_mut() {
            g.line = Some(bad_line);
        }
        assert!(d.validate().is_err());
    }

    d = minimal_valid_desc();
    if let Some(g) = d.groups.first_mut() {
        g.children
            .push(lit_item(Some(LinePositionSpec::OnNextPage(2)), 1, b"X"));
    }
    assert!(d.validate().is_err());

    for ph_pf in [ReportGroupKind::PageHeading, ReportGroupKind::PageFooting] {
        d = minimal_valid_desc();
        d.groups.push(simple_grp(
            None,
            ph_pf,
            Some(LinePositionSpec::OnNextPage(1)),
            Vec::new(),
        ));
        assert!(d.validate().is_err());
    }
}

/// Verifies `ReportDesc::validate` for elementary item invariants and sub-group recursion.
#[test]
fn descriptor_validate_elementary_items() -> Result<()> {
    let mut d = minimal_valid_desc();
    d.groups.push(simple_grp(
        None,
        ReportGroupKind::ReportHeading,
        Some(LinePositionSpec::Absolute(1)),
        vec![src_item(
            None,
            1,
            FieldId::new(1),
            GroupIndicatePolicy::GroupIndicate,
        )],
    ));
    assert!(d.validate().is_err());

    d = minimal_valid_desc();
    if let Some(g) = d.groups.first_mut() {
        g.line = None;
    }
    assert!(d.validate().is_err());

    let bad_items = [
        ReportFieldItem {
            name: None,
            line: None,
            column: Some(ColumnPositionSpec::Absolute(0)),
            source: None,
            formatter: FieldFormatter::RawBytes,
            group_indicate: GroupIndicatePolicy::AlwaysDisplay,
            present_when: PresentWhenSpec::Always,
        },
        ReportFieldItem {
            name: None,
            line: None,
            column: None,
            source: None,
            formatter: FieldFormatter::FixedWidthAlpha {
                width: 0,
                justified: JustifiedSpec::Left,
                space_byte: b' ',
            },
            group_indicate: GroupIndicatePolicy::AlwaysDisplay,
            present_when: PresentWhenSpec::Always,
        },
        ReportFieldItem {
            name: None,
            line: None,
            column: None,
            source: None,
            formatter: FieldFormatter::Picture {
                byte_len: 0,
                desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
                    byte_len: 1,
                    justified: JustifiedSpec::Left,
                }),
            },
            group_indicate: GroupIndicatePolicy::AlwaysDisplay,
            present_when: PresentWhenSpec::Always,
        },
        ReportFieldItem {
            name: None,
            line: None,
            column: Some(ColumnPositionSpec::Absolute(1)),
            source: Some(ItemValueSource::SumCounter(SumCounterId::new(0))),
            formatter: FieldFormatter::RawBytes,
            group_indicate: GroupIndicatePolicy::AlwaysDisplay,
            present_when: PresentWhenSpec::Always,
        },
    ];
    for item in bad_items {
        d = minimal_valid_desc();
        if let Some(g) = d.groups.first_mut() {
            g.children = vec![ReportNode::Elementary(item)];
        }
        assert!(d.validate().is_err());
    }

    d = minimal_valid_desc();
    if let Some(g) = d.groups.first_mut() {
        g.line = None;
        g.children = vec![ReportNode::SubGroup(ReportSubGroup {
            name: None,
            line: Some(LinePositionSpec::relative(1)),
            present_when: PresentWhenSpec::Always,
            children: vec![ReportNode::SubGroup(ReportSubGroup {
                name: None,
                line: None,
                present_when: PresentWhenSpec::Always,
                children: vec![lit_item(None, 1, b"OK")],
            })],
        })];
    }
    d.validate()
}

/// Verifies `ReportDesc::validate` across all invalid `SumCounterDesc` configurations.
#[test]
fn descriptor_validate_sum_counters() {
    let mk_sum = |id: u32, owner: u16, reset: u16, addends: Vec<SumAddend>, upon: SumUponFilter| {
        SumCounterDesc {
            id: SumCounterId::new(id),
            name: None,
            owner_control_level: ControlLevelIndex::new(owner),
            reset_level: ControlLevelIndex::new(reset),
            addends,
            upon,
        }
    };
    let bad_sums = [
        mk_sum(
            5,
            0,
            0,
            vec![SumAddend::SourceField(FieldId::new(2))],
            SumUponFilter::AllGenerations,
        ),
        mk_sum(
            0,
            9,
            0,
            vec![SumAddend::SourceField(FieldId::new(2))],
            SumUponFilter::AllGenerations,
        ),
        mk_sum(
            0,
            0,
            1,
            vec![SumAddend::SourceField(FieldId::new(2))],
            SumUponFilter::AllGenerations,
        ),
        mk_sum(0, 0, 0, Vec::new(), SumUponFilter::AllGenerations),
        mk_sum(
            0,
            0,
            0,
            vec![SumAddend::RollingCounter(SumCounterId::new(4))],
            SumUponFilter::AllGenerations,
        ),
        mk_sum(
            0,
            1,
            1,
            vec![SumAddend::RollingCounter(SumCounterId::new(0))],
            SumUponFilter::AllGenerations,
        ),
        mk_sum(
            0,
            0,
            0,
            vec![SumAddend::SourceField(FieldId::new(2))],
            SumUponFilter::NamedDetails(Vec::new()),
        ),
        mk_sum(
            0,
            0,
            0,
            vec![SumAddend::SourceField(FieldId::new(2))],
            SumUponFilter::NamedDetails(vec![String::from("UNKNOWN")]),
        ),
    ];
    for s in bad_sums {
        let mut rd = minimal_valid_desc();
        rd.controls = vec![
            ControlDef::final_control(),
            ControlDef::field("DEPT", FieldId::new(1)),
        ];
        rd.sum_counters = vec![s];
        assert!(rd.validate().is_err());
    }
}

/// Verifies `MemoryReportEnv`, `FieldValue`, `ReportHookContext`, and lifecycle error handling.
#[test]
fn env_bindings_hook_context_and_lifecycle() -> Result<()> {
    let mut env = MemoryReportEnv::new();
    assert!(format!("{env:?}").contains("MemoryReportEnv"));
    assert!(env.read_field(FieldId::new(99)).is_err());
    assert!(env.write_control_bytes(FieldId::new(99), b"X").is_err());
    assert!(env.eval_condition(ConditionId::new(99)).is_err());
    assert!(FieldValue::raw(&[0xFF, 0xFE]).decode_decimal().is_err());

    let zoned_desc = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: DigitCount::try_new(4)?,
        scale: ScaleFactor::try_new(0)?,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    env.set_typed_field(FieldId::new(1), b"0042", zoned_desc);
    assert_eq!(
        env.read_field(FieldId::new(1))?.decode_decimal()?,
        FixedDecimal::from_i64(42)
    );
    env.set_condition(ConditionId::new(1), true);
    assert!(env.eval_condition(ConditionId::new(1))?);

    let mut state = ReportState::try_new(minimal_valid_desc())?;
    assert_eq!(state.descriptor().report_name, "MIN-REP");
    assert_eq!(state.lifecycle(), ReportLifecycleState::Inactive);
    assert_eq!((state.page_counter(), state.line_counter()), (0, 0));
    state.set_page_counter(9);
    assert_eq!(state.page_counter(), 9);
    assert!(state.sum_value(SumCounterId::new(0)).is_err());

    assert!(state.suppress_printing().is_err());
    assert!(state.generate(&GenerateTarget::Summary, &mut env).is_err());
    assert!(state.terminate(&mut env).is_err());

    state.initiate()?;
    assert_eq!(state.lifecycle(), ReportLifecycleState::Active);
    assert!(state.initiate().is_err());
    state.suppress_printing()?;
    state.terminate(&mut env)?;
    assert_eq!(state.lifecycle(), ReportLifecycleState::Inactive);
    assert_eq!(state.emitted_lines(), []);

    let mut rd = minimal_valid_desc();
    rd.groups.insert(
        0,
        ReportGroupDesc {
            name: Some(String::from("HEAD")),
            kind: ReportGroupKind::ReportHeading,
            line: Some(LinePositionSpec::Absolute(1)),
            next_group: None,
            present_when: PresentWhenSpec::Always,
            children: vec![lit_item(None, 1, b"RH")],
        },
    );
    let mut state2 = ReportState::try_new(rd)?;
    state2.initiate()?;
    assert!(
        state2
            .generate(&GenerateTarget::DetailIndex(99), &mut env)
            .is_err()
    );
    assert!(
        state2
            .generate(&GenerateTarget::DetailIndex(0), &mut env)
            .is_err()
    );
    assert!(
        state2
            .generate(
                &GenerateTarget::DetailName(String::from("MISSING")),
                &mut env
            )
            .is_err()
    );

    let mut hook_state = ReportState::try_new(minimal_valid_desc())?;
    hook_state.initiate()?;
    let mut probe = HookProbeEnv { mode: 1 };
    hook_state.generate(&GenerateTarget::DetailIndex(0), &mut probe)?;
    assert_eq!(hook_state.emitted_lines(), []);
    assert_eq!(hook_state.page_counter(), 5);

    probe.mode = 2;
    assert!(
        hook_state
            .generate(&GenerateTarget::DetailIndex(0), &mut probe)
            .is_err()
    );
    Ok(())
}

/// Builds the heading and detail groups for `SALES-REP`.
fn sales_heading_and_detail_groups() -> Vec<ReportGroupDesc> {
    let (f_dept, f_team, f_item, f_amt, f_bonus) = (
        FieldId::new(1),
        FieldId::new(2),
        FieldId::new(3),
        FieldId::new(4),
        FieldId::new(5),
    );
    let (l_fin, l_dept, l_team) = (
        ControlLevelIndex::new(0),
        ControlLevelIndex::new(1),
        ControlLevelIndex::new(2),
    );
    let rel1 = Some(LinePositionSpec::relative(1));
    let pg_elem = ReportNode::Elementary(ReportFieldItem {
        name: None,
        line: None,
        column: Some(ColumnPositionSpec::Relative(1)),
        source: Some(ItemValueSource::PageCounter),
        formatter: FieldFormatter::RawBytes,
        group_indicate: GroupIndicatePolicy::AlwaysDisplay,
        present_when: PresentWhenSpec::Always,
    });
    vec![
        simple_grp(
            Some("RH-GRP"),
            ReportGroupKind::ReportHeading,
            Some(LinePositionSpec::Absolute(1)),
            vec![lit_item(None, 1, b"=== SALES ===")],
        ),
        simple_grp(
            Some("PH-GRP"),
            ReportGroupKind::PageHeading,
            Some(LinePositionSpec::Absolute(2)),
            vec![lit_item(None, 1, b"PG:"), pg_elem],
        ),
        simple_grp(
            Some("CH-FINAL"),
            ReportGroupKind::ControlHeading(l_fin),
            rel1,
            vec![lit_item(None, 1, b"START-ALL")],
        ),
        simple_grp(
            Some("CH-DEPT"),
            ReportGroupKind::ControlHeading(l_dept),
            rel1,
            vec![
                lit_item(None, 1, b"DEPT="),
                src_item(None, 6, f_dept, GroupIndicatePolicy::AlwaysDisplay),
            ],
        ),
        simple_grp(
            Some("CH-TEAM"),
            ReportGroupKind::ControlHeading(l_team),
            rel1,
            vec![
                lit_item(None, 1, b"TEAM="),
                src_item(None, 6, f_team, GroupIndicatePolicy::AlwaysDisplay),
            ],
        ),
        simple_grp(
            Some("DET-MAIN"),
            ReportGroupKind::Detail,
            rel1,
            vec![
                src_item(None, 1, f_dept, GroupIndicatePolicy::GroupIndicate),
                src_item(None, 5, f_item, GroupIndicatePolicy::AlwaysDisplay),
                src_item(None, 10, f_amt, GroupIndicatePolicy::AlwaysDisplay),
            ],
        ),
        simple_grp(
            Some("DET-BONUS"),
            ReportGroupKind::Detail,
            rel1,
            vec![
                lit_item(None, 1, b"BNS"),
                src_item(None, 5, f_bonus, GroupIndicatePolicy::AlwaysDisplay),
            ],
        ),
    ]
}

/// Builds the footing groups (`CF`, `PF`, `RF`) for `SALES-REP`.
fn sales_footing_groups() -> Vec<ReportGroupDesc> {
    let (f_dept, f_team) = (FieldId::new(1), FieldId::new(2));
    let (s_team, s_dept, s_run, s_bonus) = (
        SumCounterId::new(0),
        SumCounterId::new(1),
        SumCounterId::new(2),
        SumCounterId::new(3),
    );
    let (l_fin, l_dept, l_team) = (
        ControlLevelIndex::new(0),
        ControlLevelIndex::new(1),
        ControlLevelIndex::new(2),
    );
    let sum_elem = |col: u32, sid: SumCounterId| {
        ReportNode::Elementary(ReportFieldItem {
            name: None,
            line: None,
            column: Some(ColumnPositionSpec::Absolute(col)),
            source: Some(ItemValueSource::SumCounter(sid)),
            formatter: FieldFormatter::RawBytes,
            group_indicate: GroupIndicatePolicy::AlwaysDisplay,
            present_when: PresentWhenSpec::Always,
        })
    };
    let rel1 = Some(LinePositionSpec::relative(1));
    vec![
        simple_grp(
            Some("CF-TEAM"),
            ReportGroupKind::ControlFooting(l_team),
            rel1,
            vec![
                lit_item(None, 1, b"CF-T:"),
                src_item(None, 6, f_team, GroupIndicatePolicy::AlwaysDisplay),
                sum_elem(9, s_team),
            ],
        ),
        simple_grp(
            Some("CF-DEPT"),
            ReportGroupKind::ControlFooting(l_dept),
            rel1,
            vec![
                lit_item(None, 1, b"CF-D:"),
                src_item(None, 6, f_dept, GroupIndicatePolicy::AlwaysDisplay),
                sum_elem(9, s_dept),
                sum_elem(14, s_run),
            ],
        ),
        simple_grp(
            Some("CF-FINAL"),
            ReportGroupKind::ControlFooting(l_fin),
            rel1,
            vec![
                lit_item(None, 1, b"GRAND="),
                sum_elem(7, s_run),
                lit_item(None, 12, b"BN="),
                sum_elem(15, s_bonus),
            ],
        ),
        simple_grp(
            Some("PF-GRP"),
            ReportGroupKind::PageFooting,
            Some(LinePositionSpec::Absolute(19)),
            vec![lit_item(None, 1, b"--PF--")],
        ),
        simple_grp(
            Some("RF-GRP"),
            ReportGroupKind::ReportFooting,
            Some(LinePositionSpec::Absolute(20)),
            vec![lit_item(None, 1, b"=== END ===")],
        ),
    ]
}

/// Builds the multi-level `SALES-REP` descriptor (`CONTROLS ARE FINAL DEPT TEAM`, rolling & `UPON` sums).
fn build_sales_report_desc() -> Result<ReportDesc> {
    let (f_dept, f_team, f_amt, f_bonus) = (
        FieldId::new(1),
        FieldId::new(2),
        FieldId::new(4),
        FieldId::new(5),
    );
    let (s_team, s_dept, s_run, s_bonus) = (
        SumCounterId::new(0),
        SumCounterId::new(1),
        SumCounterId::new(2),
        SumCounterId::new(3),
    );
    let (l_fin, l_dept, l_team) = (
        ControlLevelIndex::new(0),
        ControlLevelIndex::new(1),
        ControlLevelIndex::new(2),
    );
    let mut groups = sales_heading_and_detail_groups();
    groups.extend(sales_footing_groups());
    Ok(ReportDesc {
        report_name: String::from("SALES-REP"),
        code_prefix: Vec::new(),
        page_limits: Some(PageLimits::try_new(
            20,
            Some(1),
            Some(3),
            Some(14),
            Some(18),
        )?),
        controls: vec![
            ControlDef::final_control(),
            ControlDef::field("DEPT", f_dept),
            ControlDef::field("TEAM", f_team),
        ],
        groups,
        sum_counters: vec![
            SumCounterDesc {
                id: s_team,
                name: Some(String::from("S-TEAM")),
                owner_control_level: l_team,
                reset_level: l_team,
                addends: vec![SumAddend::SourceField(f_amt)],
                upon: SumUponFilter::AllGenerations,
            },
            SumCounterDesc {
                id: s_dept,
                name: Some(String::from("S-DEPT")),
                owner_control_level: l_dept,
                reset_level: l_dept,
                addends: vec![SumAddend::RollingCounter(s_team)],
                upon: SumUponFilter::AllGenerations,
            },
            SumCounterDesc {
                id: s_run,
                name: Some(String::from("S-RUN")),
                owner_control_level: l_dept,
                reset_level: l_fin,
                addends: vec![SumAddend::SourceField(f_amt)],
                upon: SumUponFilter::AllGenerations,
            },
            SumCounterDesc {
                id: s_bonus,
                name: Some(String::from("S-BONUS")),
                owner_control_level: l_fin,
                reset_level: l_fin,
                addends: vec![SumAddend::SourceField(f_bonus)],
                upon: SumUponFilter::NamedDetails(vec![String::from("DET-BONUS")]),
            },
        ],
    })
}

/// Verifies hierarchical `CONTROLS ARE FINAL DEPT TEAM`, rolling `SUM`, `UPON`, `GROUP INDICATE`, and control restoration.
#[test]
fn hierarchical_controls_sum_rolling_and_group_indicate() -> Result<()> {
    let (f_dept, f_team, f_item, f_amt, f_bonus) = (
        FieldId::new(1),
        FieldId::new(2),
        FieldId::new(3),
        FieldId::new(4),
        FieldId::new(5),
    );
    let mut state = ReportState::try_new(build_sales_report_desc()?)?;
    let mut env = MemoryReportEnv::new();
    env.set_before_reporting_hook(Box::new(|mem, ctx| {
        if ctx.group_name() == Some("CF-TEAM") {
            let team_val = mem.read_field(FieldId::new(2))?;
            assert!(team_val.bytes == b"T1" || team_val.bytes == b"T2");
            assert!(!ctx.sum_counter(SumCounterId::new(0))?.is_negative());
        }
        Ok(())
    }));

    state.initiate()?;
    env.set_raw_field(f_dept, b"D1");
    env.set_raw_field(f_team, b"T1");
    env.set_raw_field(f_item, b"I1");
    env.set_raw_field(f_amt, b"10");
    env.set_raw_field(f_bonus, b"99");
    state.generate(
        &GenerateTarget::DetailName(String::from("det-main")),
        &mut env,
    )?;

    env.set_raw_field(f_item, b"I2");
    env.set_raw_field(f_amt, b"20");
    state.generate(
        &GenerateTarget::DetailName(String::from("DET-MAIN")),
        &mut env,
    )?;

    env.set_raw_field(f_team, b"T2");
    env.set_raw_field(f_item, b"I3");
    env.set_raw_field(f_amt, b"5");
    state.generate(
        &GenerateTarget::DetailName(String::from("DET-MAIN")),
        &mut env,
    )?;

    env.set_raw_field(f_amt, b"0");
    env.set_raw_field(f_bonus, b"7");
    state.generate(
        &GenerateTarget::DetailName(String::from("DET-BONUS")),
        &mut env,
    )?;

    env.set_raw_field(f_dept, b"D2");
    env.set_raw_field(f_team, b"T1");
    env.set_raw_field(f_amt, b"100");
    state.generate(&GenerateTarget::Summary, &mut env)?;
    assert_eq!(
        state.sum_value(SumCounterId::new(2))?,
        FixedDecimal::from_i64(135)
    );

    env.set_raw_field(f_dept, b"EOF");
    state.terminate(&mut env)?;
    assert_eq!(env.read_field(f_dept)?.bytes, b"EOF");

    let rendered = String::from_utf8(
        state.render_output(RenderFormat::PresentedRecords { fixed_width: None }),
    )
    .map_err(|e| CoblangError::ReportEngine {
        report_name: String::from("T"),
        message: e.to_string(),
    })?;
    for expected in [
        "=== SALES ===",
        "PG:1",
        "D1  I1   10",
        "    I2   20",
        "CF-T:T1 30",
        "D1  I3   5",
        "CF-D:D1 35   35",
        "GRAND=135  BN=7",
        "--PF--",
        "=== END ===",
    ] {
        assert!(
            rendered.contains(expected),
            "missing `{expected}` in:
{rendered}"
        );
    }

    let mut out_vec = Vec::new();
    state.write_output(
        RenderFormat::PresentedRecords { fixed_width: None },
        &mut out_vec,
    )?;
    assert_ne!(out_vec, [] as [u8; 0]);
    let drained = state.take_emitted_lines();
    assert_ne!(drained, [] as [ReportLine; 0]);
    assert_eq!(state.emitted_lines(), []);
    Ok(())
}

/// Constructs a single formatted elementary [`ReportNode`] for `FULL-FMT` tests.
///
/// # Arguments
///
/// * `col` - Horizontal column placement.
/// * `src` - Optional value source.
/// * `fmt` - Field formatter specification.
/// * `pw` - `PRESENT WHEN` predicate.
fn fmt_elem_node(
    col: ColumnPositionSpec,
    src: Option<ItemValueSource>,
    fmt: FieldFormatter,
    pw: PresentWhenSpec,
) -> ReportNode {
    ReportNode::Elementary(ReportFieldItem {
        name: None,
        line: None,
        column: Some(col),
        source: src,
        formatter: fmt,
        group_indicate: GroupIndicatePolicy::AlwaysDisplay,
        present_when: pw,
    })
}

/// Builds the 7 elementary items inside `FULL-FMT`'s `DETAIL` subgroup.
///
/// # Arguments
///
/// * `pic_num` - Numeric-edited `MoveFieldDesc`.
/// * `pic_alpha` - Alphanumeric `MoveFieldDesc`.
fn full_formatter_detail_items(
    pic_num: &MoveFieldDesc,
    pic_alpha: &MoveFieldDesc,
) -> Vec<ReportNode> {
    let (f_num, f_txt) = (FieldId::new(2), FieldId::new(3));
    let abs = ColumnPositionSpec::Absolute;
    vec![
        fmt_elem_node(
            abs(1),
            None,
            FieldFormatter::RawBytes,
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(2),
            None,
            FieldFormatter::FixedWidthAlpha {
                width: 2,
                justified: JustifiedSpec::Left,
                space_byte: b'.',
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(4),
            None,
            FieldFormatter::Picture {
                byte_len: 2,
                desc: pic_alpha.clone(),
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(6),
            Some(ItemValueSource::SourceField(f_txt)),
            FieldFormatter::FixedWidthAlpha {
                width: 4,
                justified: JustifiedSpec::Right,
                space_byte: b' ',
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(11),
            Some(ItemValueSource::LineCounter),
            FieldFormatter::Picture {
                byte_len: 3,
                desc: pic_num.clone(),
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(15),
            Some(ItemValueSource::DecimalLiteral(FixedDecimal::from_i64(9))),
            FieldFormatter::Picture {
                byte_len: 3,
                desc: pic_num.clone(),
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(19),
            Some(ItemValueSource::LineCounter),
            FieldFormatter::RawBytes,
            PresentWhenSpec::FieldNonZero(f_num),
        ),
    ]
}

/// Builds the `DETAIL` group for `FULL-FMT` exercising blank formatters, justification, and `PRESENT WHEN`.
///
/// # Arguments
///
/// * `pic_num` - Numeric-edited `MoveFieldDesc`.
/// * `pic_alpha` - Alphanumeric `MoveFieldDesc`.
fn build_full_formatter_detail(
    pic_num: &MoveFieldDesc,
    pic_alpha: &MoveFieldDesc,
) -> ReportGroupDesc {
    let (f_flag, f_num, cond_1) = (FieldId::new(1), FieldId::new(2), ConditionId::new(1));
    ReportGroupDesc {
        name: Some(String::from("DET")),
        kind: ReportGroupKind::Detail,
        line: None,
        next_group: Some(NextGroupSpec::Relative(2)),
        present_when: PresentWhenSpec::All(vec![
            PresentWhenSpec::Condition(cond_1),
            PresentWhenSpec::Not(Box::new(PresentWhenSpec::FieldEquals {
                field: f_flag,
                expected: b"SKIP".to_vec(),
            })),
        ]),
        children: vec![
            ReportNode::SubGroup(ReportSubGroup {
                name: None,
                line: Some(LinePositionSpec::relative(1)),
                present_when: PresentWhenSpec::Any(vec![
                    PresentWhenSpec::FieldEquals {
                        field: f_flag,
                        expected: b"SHOW".to_vec(),
                    },
                    PresentWhenSpec::FieldNonZero(f_num),
                ]),
                children: full_formatter_detail_items(pic_num, pic_alpha),
            }),
            ReportNode::SubGroup(ReportSubGroup {
                name: None,
                line: Some(LinePositionSpec::relative(1)),
                present_when: PresentWhenSpec::Any(Vec::new()),
                children: vec![lit_item(None, 1, b"NEVER")],
            }),
        ],
    }
}

/// Builds the 4 elementary items inside `FULL-FMT`'s `CONTROL FOOTING FINAL` group.
///
/// # Arguments
///
/// * `pic_num` - Numeric-edited `MoveFieldDesc`.
/// * `pic_alpha` - Alphanumeric `MoveFieldDesc`.
fn full_formatter_cf_items(pic_num: MoveFieldDesc, pic_alpha: MoveFieldDesc) -> Vec<ReportNode> {
    let (f_num, f_txt, sum_0) = (FieldId::new(2), FieldId::new(3), SumCounterId::new(0));
    let abs = ColumnPositionSpec::Absolute;
    vec![
        fmt_elem_node(
            abs(1),
            Some(ItemValueSource::SumCounter(sum_0)),
            FieldFormatter::Picture {
                byte_len: 3,
                desc: pic_num.clone(),
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(5),
            Some(ItemValueSource::DecimalLiteral(FixedDecimal::from_i64(7))),
            FieldFormatter::RawBytes,
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(7),
            Some(ItemValueSource::SourceField(f_num)),
            FieldFormatter::Picture {
                byte_len: 3,
                desc: pic_num,
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(11),
            Some(ItemValueSource::SourceField(f_txt)),
            FieldFormatter::Picture {
                byte_len: 4,
                desc: pic_alpha,
            },
            PresentWhenSpec::Always,
        ),
    ]
}

/// Builds a descriptor exercising `Picture`, `FixedWidthAlpha`, `PRESENT WHEN` combinators, and `RF` before `PF`.
fn build_full_formatter_desc() -> Result<ReportDesc> {
    let (f_num, sum_0) = (FieldId::new(2), SumCounterId::new(0));
    let pic_num = MoveFieldDesc::standard_gnu(HirType::NumericEdited {
        byte_len: 3,
        digits: DigitCount::try_new(3)?,
        scale: ScaleFactor::try_new(0)?,
        sign_capability: Signedness::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
        edit_ops: vec![
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::DigitSlot,
        ],
    });
    let pic_alpha = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 4,
        justified: JustifiedSpec::Left,
    });
    let mut cover = simple_grp(
        Some("COVER"),
        ReportGroupKind::ReportHeading,
        Some(LinePositionSpec::Absolute(2)),
        vec![lit_item(None, 1, b"COVER PAGE")],
    );
    cover.next_group = Some(NextGroupSpec::NextPage);
    let det = build_full_formatter_detail(&pic_num, &pic_alpha);
    let cf_items = full_formatter_cf_items(pic_num.clone(), pic_alpha.clone());
    let ph_items = vec![
        fmt_elem_node(
            ColumnPositionSpec::Absolute(1),
            Some(ItemValueSource::Literal(b"HDR".to_vec())),
            FieldFormatter::Picture {
                byte_len: 4,
                desc: pic_alpha,
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            ColumnPositionSpec::Relative(1),
            Some(ItemValueSource::PageCounter),
            FieldFormatter::Picture {
                byte_len: 3,
                desc: pic_num,
            },
            PresentWhenSpec::Always,
        ),
    ];
    Ok(ReportDesc {
        report_name: String::from("FULL-FMT"),
        code_prefix: Vec::new(),
        page_limits: Some(PageLimits::try_new(
            12,
            Some(1),
            Some(3),
            Some(8),
            Some(10),
        )?),
        controls: vec![ControlDef::final_control()],
        groups: vec![
            cover,
            simple_grp(
                None,
                ReportGroupKind::PageHeading,
                Some(LinePositionSpec::Absolute(1)),
                ph_items,
            ),
            det,
            simple_grp(
                None,
                ReportGroupKind::ControlFooting(ControlLevelIndex::ZERO),
                Some(LinePositionSpec::relative(1)),
                cf_items,
            ),
            simple_grp(
                None,
                ReportGroupKind::PageFooting,
                Some(LinePositionSpec::Absolute(12)),
                vec![lit_item(None, 1, b"PF12")],
            ),
            simple_grp(
                None,
                ReportGroupKind::ReportFooting,
                Some(LinePositionSpec::Absolute(11)),
                vec![lit_item(None, 1, b"RF11")],
            ),
        ],
        sum_counters: vec![SumCounterDesc {
            id: sum_0,
            name: None,
            owner_control_level: ControlLevelIndex::ZERO,
            reset_level: ControlLevelIndex::ZERO,
            addends: vec![SumAddend::SourceField(f_num)],
            upon: SumUponFilter::AllGenerations,
        }],
    })
}

/// Executes `FULL-FMT` verifying cover `RH`, `PRESENT WHEN`, formatters, pagination, and `RF` preceding `PF`.
#[test]
fn formatting_present_when_and_rf_preceding_pf() -> Result<()> {
    let (f_flag, f_num, f_txt, cond_1) = (
        FieldId::new(1),
        FieldId::new(2),
        FieldId::new(3),
        ConditionId::new(1),
    );
    let mut state = ReportState::try_new(build_full_formatter_desc()?)?;
    let mut env = MemoryReportEnv::new();
    env.set_condition(cond_1, true);
    env.set_raw_field(f_flag, b"SHOW");
    env.set_raw_field(f_num, b"12");
    env.set_raw_field(f_txt, b"AB");

    state.initiate()?;
    state.generate(&GenerateTarget::DetailIndex(2), &mut env)?;

    env.set_raw_field(f_flag, b"SKIP");
    state.generate(&GenerateTarget::DetailIndex(2), &mut env)?;

    env.set_raw_field(f_flag, b"NO");
    env.set_raw_field(f_txt, b"ABCDEF");
    state.generate(&GenerateTarget::DetailIndex(2), &mut env)?;
    state.generate(&GenerateTarget::DetailIndex(2), &mut env)?;
    state.generate(&GenerateTarget::DetailIndex(2), &mut env)?;
    state.terminate(&mut env)?;

    assert!(state.page_counter() >= 3);
    let last_two: Vec<&[u8]> = state
        .emitted_lines()
        .iter()
        .rev()
        .take(2)
        .map(|l| l.content.as_slice())
        .collect();
    assert_eq!(last_two.first().copied(), Some(b"PF12".as_slice()));
    assert_eq!(last_two.get(1).copied(), Some(b"RF11".as_slice()));
    Ok(())
}

/// Verifies unpaged reports, typed `SourceField` formatting, and `FixedWidthAlpha` left alignment.
#[test]
fn unpaged_reports_and_typed_sources() -> Result<()> {
    let f_zoned = FieldId::new(1);
    let zoned_desc = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: DigitCount::try_new(3)?,
        scale: ScaleFactor::try_new(0)?,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let abs = ColumnPositionSpec::Absolute;
    let d1_items = vec![
        fmt_elem_node(
            abs(1),
            Some(ItemValueSource::SourceField(f_zoned)),
            FieldFormatter::RawBytes,
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(5),
            Some(ItemValueSource::SourceField(f_zoned)),
            FieldFormatter::Picture {
                byte_len: 3,
                desc: zoned_desc.clone(),
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(9),
            Some(ItemValueSource::Literal(b"LEFTPAD".to_vec())),
            FieldFormatter::FixedWidthAlpha {
                width: 4,
                justified: JustifiedSpec::Left,
                space_byte: b' ',
            },
            PresentWhenSpec::Always,
        ),
        fmt_elem_node(
            abs(14),
            Some(ItemValueSource::Literal(b"HI".to_vec())),
            FieldFormatter::FixedWidthAlpha {
                width: 4,
                justified: JustifiedSpec::Left,
                space_byte: b' ',
            },
            PresentWhenSpec::Always,
        ),
    ];
    let mut rh = simple_grp(
        None,
        ReportGroupKind::ReportHeading,
        Some(LinePositionSpec::relative(1)),
        vec![lit_item(None, 1, b"TOP")],
    );
    rh.next_group = Some(NextGroupSpec::Absolute(3));
    let mut d1 = simple_grp(
        Some("D1"),
        ReportGroupKind::Detail,
        Some(LinePositionSpec::relative(1)),
        d1_items,
    );
    d1.next_group = Some(NextGroupSpec::Absolute(6));
    let mut d2 = simple_grp(
        Some("D2"),
        ReportGroupKind::Detail,
        Some(LinePositionSpec::relative(1)),
        vec![lit_item(None, 1, b"SECOND")],
    );
    d2.next_group = Some(NextGroupSpec::Relative(2));
    let rf = simple_grp(
        None,
        ReportGroupKind::ReportFooting,
        Some(LinePositionSpec::relative(1)),
        vec![lit_item(None, 1, b"END")],
    );
    let unpaged = ReportDesc {
        report_name: String::from("UNPAGED"),
        code_prefix: Vec::new(),
        page_limits: None,
        controls: Vec::new(),
        groups: vec![rh, d1, d2, rf],
        sum_counters: Vec::new(),
    };

    let mut st = ReportState::try_new(unpaged)?;
    let mut env = MemoryReportEnv::new();
    env.set_typed_field(f_zoned, b"045", zoned_desc);
    st.initiate()?;
    st.generate(&GenerateTarget::DetailName(String::from("D1")), &mut env)?;
    st.generate(&GenerateTarget::DetailName(String::from("D1")), &mut env)?;
    st.generate(&GenerateTarget::DetailName(String::from("D2")), &mut env)?;
    st.terminate(&mut env)?;
    assert_eq!(st.emitted_lines().len(), 5);
    Ok(())
}

/// Verifies `NEXT GROUP` variants on paged reports (`Absolute`, `NextPage`, `RH` overlapping `PH`) and `RF ON NEXT PAGE`.
#[test]
fn paged_next_group_and_rf_on_next_page() -> Result<()> {
    let mut env = MemoryReportEnv::new();
    let paged_ng = ReportDesc {
        report_name: String::from("PAGED-NG"),
        code_prefix: Vec::new(),
        page_limits: Some(PageLimits::try_new(10, Some(1), Some(3), Some(8), Some(9))?),
        controls: Vec::new(),
        groups: vec![
            ReportGroupDesc {
                name: None,
                kind: ReportGroupKind::ReportHeading,
                line: Some(LinePositionSpec::Absolute(1)),
                next_group: Some(NextGroupSpec::Relative(2)),
                present_when: PresentWhenSpec::Always,
                children: vec![lit_item(None, 1, b"RH")],
            },
            ReportGroupDesc {
                name: None,
                kind: ReportGroupKind::PageHeading,
                line: Some(LinePositionSpec::Absolute(1)),
                next_group: None,
                present_when: PresentWhenSpec::Always,
                children: vec![lit_item(None, 1, b"PH")],
            },
            ReportGroupDesc {
                name: Some(String::from("DA")),
                kind: ReportGroupKind::Detail,
                line: Some(LinePositionSpec::relative(1)),
                next_group: Some(NextGroupSpec::Absolute(6)),
                present_when: PresentWhenSpec::Always,
                children: vec![lit_item(None, 1, b"DA")],
            },
            ReportGroupDesc {
                name: Some(String::from("DB")),
                kind: ReportGroupKind::Detail,
                line: Some(LinePositionSpec::OnNextPage(4)),
                next_group: Some(NextGroupSpec::NextPage),
                present_when: PresentWhenSpec::Always,
                children: vec![lit_item(None, 1, b"DB")],
            },
            ReportGroupDesc {
                name: None,
                kind: ReportGroupKind::ReportFooting,
                line: Some(LinePositionSpec::OnNextPage(2)),
                next_group: None,
                present_when: PresentWhenSpec::Always,
                children: vec![lit_item(None, 1, b"RF-NP")],
            },
        ],
        sum_counters: Vec::new(),
    };
    let mut st = ReportState::try_new(paged_ng)?;
    st.initiate()?;
    st.generate(&GenerateTarget::DetailName(String::from("DA")), &mut env)?;
    st.generate(&GenerateTarget::DetailName(String::from("DA")), &mut env)?;
    st.generate(&GenerateTarget::DetailName(String::from("DB")), &mut env)?;
    st.generate(&GenerateTarget::DetailName(String::from("DA")), &mut env)?;
    st.terminate(&mut env)?;
    assert!(st.page_counter() >= 4);
    Ok(())
}

/// Verifies runtime overflow and non-monotonic line errors across `DE` and `RF`.
#[test]
fn runtime_zone_overflow_and_non_monotonic_errors() -> Result<()> {
    let mut env = MemoryReportEnv::new();

    let too_tall_de = ReportDesc {
        report_name: String::from("TALL-DE"),
        code_prefix: Vec::new(),
        page_limits: Some(PageLimits::try_new(10, Some(1), Some(2), Some(3), Some(8))?),
        controls: Vec::new(),
        groups: vec![ReportGroupDesc {
            name: Some(String::from("D")),
            kind: ReportGroupKind::Detail,
            line: Some(LinePositionSpec::Absolute(5)),
            next_group: None,
            present_when: PresentWhenSpec::Always,
            children: vec![lit_item(None, 1, b"TOO-LOW")],
        }],
        sum_counters: Vec::new(),
    };
    let mut st_err = ReportState::try_new(too_tall_de)?;
    st_err.initiate()?;
    assert!(
        st_err
            .generate(&GenerateTarget::DetailIndex(0), &mut env)
            .is_err()
    );

    let unpaged_bad_de = ReportDesc {
        report_name: String::from("BAD-UNPAGED"),
        code_prefix: Vec::new(),
        page_limits: None,
        controls: Vec::new(),
        groups: vec![ReportGroupDesc {
            name: Some(String::from("D")),
            kind: ReportGroupKind::Detail,
            line: Some(LinePositionSpec::Absolute(2)),
            next_group: None,
            present_when: PresentWhenSpec::Always,
            children: vec![lit_item(None, 1, b"X")],
        }],
        sum_counters: Vec::new(),
    };
    let mut st_ub = ReportState::try_new(unpaged_bad_de)?;
    st_ub.initiate()?;
    st_ub.generate(&GenerateTarget::DetailIndex(0), &mut env)?;
    assert!(
        st_ub
            .generate(&GenerateTarget::DetailIndex(0), &mut env)
            .is_err()
    );

    let unpaged_bad_rf = ReportDesc {
        report_name: String::from("BAD-RF"),
        code_prefix: Vec::new(),
        page_limits: None,
        controls: Vec::new(),
        groups: vec![
            ReportGroupDesc {
                name: Some(String::from("D")),
                kind: ReportGroupKind::Detail,
                line: Some(LinePositionSpec::Absolute(5)),
                next_group: None,
                present_when: PresentWhenSpec::Always,
                children: vec![lit_item(None, 1, b"X")],
            },
            ReportGroupDesc {
                name: None,
                kind: ReportGroupKind::ReportFooting,
                line: Some(LinePositionSpec::Absolute(2)),
                next_group: None,
                present_when: PresentWhenSpec::Always,
                children: vec![lit_item(None, 1, b"RF")],
            },
        ],
        sum_counters: Vec::new(),
    };
    let mut st_urf = ReportState::try_new(unpaged_bad_rf)?;
    st_urf.initiate()?;
    st_urf.generate(&GenerateTarget::DetailIndex(0), &mut env)?;
    assert!(st_urf.terminate(&mut env).is_err());
    Ok(())
}

/// Verifies suppressed (`PRESENT WHEN` false / `SUPPRESS PRINTING`) and empty `RH`, `PH`, `PF`, `RF`, and summary pages.
#[test]
fn suppressed_and_empty_singleton_groups() -> Result<()> {
    let cond_false = ConditionId::new(1);
    let mut env = MemoryReportEnv::new();
    env.set_condition(cond_false, false);

    // 1. RH, PH, PF suppressed by hook; RF hidden by `PRESENT WHEN` false; plus a childless empty DE!
    let rd = ReportDesc {
        report_name: String::from("SUPP-ALL"),
        code_prefix: Vec::new(),
        page_limits: Some(PageLimits::try_new(10, Some(1), Some(3), Some(7), Some(8))?),
        controls: Vec::new(),
        groups: vec![
            simple_grp(
                Some("RH"),
                ReportGroupKind::ReportHeading,
                Some(LinePositionSpec::Absolute(1)),
                vec![lit_item(None, 1, b"RH")],
            ),
            simple_grp(
                Some("PH"),
                ReportGroupKind::PageHeading,
                Some(LinePositionSpec::Absolute(2)),
                vec![lit_item(None, 1, b"PH")],
            ),
            simple_grp(Some("EMPTY-DE"), ReportGroupKind::Detail, None, Vec::new()),
            simple_grp(
                Some("DE"),
                ReportGroupKind::Detail,
                Some(LinePositionSpec::relative(5)),
                vec![lit_item(None, 1, b"ROW")],
            ),
            simple_grp(
                Some("PF"),
                ReportGroupKind::PageFooting,
                Some(LinePositionSpec::Absolute(9)),
                vec![lit_item(None, 1, b"PF")],
            ),
            ReportGroupDesc {
                name: Some(String::from("RF")),
                kind: ReportGroupKind::ReportFooting,
                line: Some(LinePositionSpec::Absolute(10)),
                next_group: None,
                present_when: PresentWhenSpec::Condition(cond_false),
                children: vec![lit_item(None, 1, b"RF")],
            },
        ],
        sum_counters: Vec::new(),
    };
    env.set_before_reporting_hook(Box::new(|_mem, ctx| {
        if matches!(
            ctx.group_kind(),
            ReportGroupKind::ReportHeading
                | ReportGroupKind::PageHeading
                | ReportGroupKind::PageFooting
        ) {
            ctx.suppress_printing();
        }
        Ok(())
    }));
    let mut st = ReportState::try_new(rd)?;
    st.initiate()?;
    st.generate(
        &GenerateTarget::DetailName(String::from("EMPTY-DE")),
        &mut env,
    )?;
    st.generate(&GenerateTarget::DetailName(String::from("DE")), &mut env)?;
    st.generate(&GenerateTarget::DetailName(String::from("DE")), &mut env)?;
    st.terminate(&mut env)?;
    assert_eq!(st.emitted_lines().len(), 2);

    // 2. Summary report with no RF (empty page at terminate) AND summary report with RF ON NEXT PAGE on empty page + hook-suppressed RF.
    let mut env2 = MemoryReportEnv::new();
    let mut rd_no_rf = minimal_valid_desc();
    let mut st_no_rf = ReportState::try_new(rd_no_rf.clone())?;
    st_no_rf.initiate()?;
    st_no_rf.generate(&GenerateTarget::Summary, &mut env2)?;
    st_no_rf.terminate(&mut env2)?;
    assert_eq!(st_no_rf.emitted_lines(), []);

    rd_no_rf.groups.push(simple_grp(
        Some("RF-EMPTY-PG"),
        ReportGroupKind::ReportFooting,
        Some(LinePositionSpec::OnNextPage(2)),
        vec![lit_item(None, 1, b"RF")],
    ));
    let mut st_rf_empty = ReportState::try_new(rd_no_rf.clone())?;
    st_rf_empty.initiate()?;
    st_rf_empty.generate(&GenerateTarget::Summary, &mut env2)?;
    st_rf_empty.terminate(&mut env2)?;
    assert_eq!(st_rf_empty.page_counter(), 1);

    env2.set_before_reporting_hook(Box::new(|_mem, ctx| {
        if ctx.group_kind() == ReportGroupKind::ReportFooting {
            ctx.suppress_printing();
        }
        Ok(())
    }));
    let mut st_rf_supp = ReportState::try_new(rd_no_rf)?;
    st_rf_supp.initiate()?;
    st_rf_supp.generate(&GenerateTarget::Summary, &mut env2)?;
    st_rf_supp.terminate(&mut env2)?;
    assert_eq!(st_rf_supp.emitted_lines(), []);
    Ok(())
}

/// Verifies multi-line items, `first_line_in_nodes` recursion, and `NEXT GROUP` footing overflow.
#[test]
fn multiline_items_and_first_line_in_nodes() -> Result<()> {
    let cond_false = ConditionId::new(1);
    let mut env = MemoryReportEnv::new();
    env.set_condition(cond_false, false);

    let mut rh = simple_grp(
        None,
        ReportGroupKind::ReportHeading,
        Some(LinePositionSpec::Absolute(2)),
        vec![lit_item(None, 1, b"RH2")],
    );
    rh.next_group = Some(NextGroupSpec::Absolute(1));
    let mut de1 = simple_grp(
        Some("DE1"),
        ReportGroupKind::Detail,
        None,
        vec![ReportNode::SubGroup(ReportSubGroup {
            name: None,
            line: None,
            present_when: PresentWhenSpec::Always,
            children: vec![
                lit_item(Some(LinePositionSpec::relative(1)), 1, b"L1"),
                fmt_elem_node(
                    ColumnPositionSpec::Absolute(5),
                    Some(ItemValueSource::Literal(b"HIDE".to_vec())),
                    FieldFormatter::RawBytes,
                    PresentWhenSpec::Condition(cond_false),
                ),
                ReportNode::Elementary(ReportFieldItem {
                    name: None,
                    line: None,
                    column: None,
                    source: None,
                    formatter: FieldFormatter::RawBytes,
                    group_indicate: GroupIndicatePolicy::AlwaysDisplay,
                    present_when: PresentWhenSpec::Always,
                }),
                lit_item(Some(LinePositionSpec::relative(1)), 1, b"L2"),
            ],
        })],
    );
    de1.next_group = Some(NextGroupSpec::Absolute(10));
    let mut de2 = simple_grp(
        Some("DE2"),
        ReportGroupKind::Detail,
        Some(LinePositionSpec::Absolute(7)),
        vec![lit_item(None, 1, b"D7")],
    );
    de2.next_group = Some(NextGroupSpec::Relative(5));

    let pf = simple_grp(
        None,
        ReportGroupKind::PageFooting,
        None,
        vec![
            ReportNode::SubGroup(ReportSubGroup {
                name: None,
                line: None,
                present_when: PresentWhenSpec::Always,
                children: Vec::new(),
            }),
            ReportNode::SubGroup(ReportSubGroup {
                name: None,
                line: Some(LinePositionSpec::Absolute(10)),
                present_when: PresentWhenSpec::Always,
                children: vec![lit_item(None, 1, b"PF10")],
            }),
        ],
    );
    let rf = simple_grp(
        None,
        ReportGroupKind::ReportFooting,
        None,
        vec![ReportNode::SubGroup(ReportSubGroup {
            name: None,
            line: None,
            present_when: PresentWhenSpec::Always,
            children: vec![lit_item(Some(LinePositionSpec::Absolute(11)), 1, b"RF11")],
        })],
    );

    let rd = ReportDesc {
        report_name: String::from("NODES-REP"),
        code_prefix: Vec::new(),
        page_limits: Some(PageLimits::try_new(12, Some(1), Some(3), Some(8), Some(9))?),
        controls: Vec::new(),
        groups: vec![rh, de1, de2, pf, rf],
        sum_counters: Vec::new(),
    };
    let mut st = ReportState::try_new(rd)?;
    st.initiate()?;
    st.generate(&GenerateTarget::DetailName(String::from("DE1")), &mut env)?;
    st.generate(&GenerateTarget::DetailName(String::from("DE2")), &mut env)?;
    st.terminate(&mut env)?;
    assert!(st.page_counter() >= 2);
    Ok(())
}

/// Verifies `first_line_in_nodes` returning `None` (unprinted elementary/empty `RF`) and unpaged `CH`.
#[test]
fn unpaged_controls_and_line_less_rf() -> Result<()> {
    let mut env = MemoryReportEnv::new();
    let unpaged_ctrl = ReportDesc {
        report_name: String::from("UNPAGED-CTRL"),
        code_prefix: Vec::new(),
        page_limits: None,
        controls: vec![ControlDef::final_control()],
        groups: vec![
            simple_grp(
                None,
                ReportGroupKind::ControlHeading(ControlLevelIndex::ZERO),
                Some(LinePositionSpec::relative(1)),
                vec![lit_item(None, 1, b"CH")],
            ),
            simple_grp(
                Some("D"),
                ReportGroupKind::Detail,
                Some(LinePositionSpec::relative(1)),
                vec![lit_item(None, 1, b"DE")],
            ),
            simple_grp(
                None,
                ReportGroupKind::ReportFooting,
                None,
                vec![ReportNode::SubGroup(ReportSubGroup {
                    name: None,
                    line: None,
                    present_when: PresentWhenSpec::Always,
                    children: vec![ReportNode::Elementary(ReportFieldItem {
                        name: None,
                        line: None,
                        column: None,
                        source: None,
                        formatter: FieldFormatter::RawBytes,
                        group_indicate: GroupIndicatePolicy::AlwaysDisplay,
                        present_when: PresentWhenSpec::Always,
                    })],
                })],
            ),
        ],
        sum_counters: Vec::new(),
    };
    let mut st_uc = ReportState::try_new(unpaged_ctrl)?;
    st_uc.initiate()?;
    st_uc.generate(&GenerateTarget::DetailName(String::from("D")), &mut env)?;
    st_uc.terminate(&mut env)?;
    assert_eq!(st_uc.emitted_lines().len(), 2);
    Ok(())
}

/// Verifies non-monotonic and `PAGE LIMIT`-exceeding errors across `RH` and `PH`.
#[test]
fn singleton_heading_overflow_and_non_monotonic_errors() -> Result<()> {
    let mut env = MemoryReportEnv::new();
    let non_mono = || {
        vec![
            lit_item(Some(LinePositionSpec::Absolute(3)), 1, b"A"),
            lit_item(Some(LinePositionSpec::Absolute(2)), 1, b"B"),
        ]
    };
    let rel_over = || vec![lit_item(Some(LinePositionSpec::relative(25)), 1, b"OVER")];
    let lim = PageLimits::try_new(10, Some(1), Some(3), Some(7), Some(8))?;

    for children in [non_mono(), rel_over()] {
        let rd = ReportDesc {
            report_name: String::from("ERR-RH"),
            code_prefix: Vec::new(),
            page_limits: Some(lim),
            controls: Vec::new(),
            groups: vec![
                simple_grp(None, ReportGroupKind::ReportHeading, None, children),
                simple_grp(
                    Some("D"),
                    ReportGroupKind::Detail,
                    Some(LinePositionSpec::relative(1)),
                    vec![lit_item(None, 1, b"D")],
                ),
            ],
            sum_counters: Vec::new(),
        };
        let mut st = ReportState::try_new(rd)?;
        st.initiate()?;
        assert!(
            st.generate(&GenerateTarget::DetailName(String::from("D")), &mut env)
                .is_err()
        );
    }

    for children in [non_mono(), rel_over()] {
        let rd = ReportDesc {
            report_name: String::from("ERR-PH"),
            code_prefix: Vec::new(),
            page_limits: Some(lim),
            controls: Vec::new(),
            groups: vec![
                simple_grp(
                    None,
                    ReportGroupKind::ReportHeading,
                    Some(LinePositionSpec::Absolute(4)),
                    vec![lit_item(None, 1, b"RH")],
                ),
                simple_grp(None, ReportGroupKind::PageHeading, None, children),
                simple_grp(
                    Some("D"),
                    ReportGroupKind::Detail,
                    Some(LinePositionSpec::relative(1)),
                    vec![lit_item(None, 1, b"D")],
                ),
            ],
            sum_counters: Vec::new(),
        };
        let mut st = ReportState::try_new(rd)?;
        st.initiate()?;
        assert!(
            st.generate(&GenerateTarget::DetailName(String::from("D")), &mut env)
                .is_err()
        );
    }
    Ok(())
}

/// Verifies overlapping and `PAGE LIMIT`-exceeding errors across `PF` and `RF`.
#[test]
fn singleton_footing_overflow_and_non_monotonic_errors() -> Result<()> {
    let mut env = MemoryReportEnv::new();
    let lim = PageLimits::try_new(10, Some(1), Some(3), Some(7), Some(8))?;

    for pf_line in [
        LinePositionSpec::Absolute(2),
        LinePositionSpec::relative(25),
    ] {
        let rd = ReportDesc {
            report_name: String::from("ERR-PF"),
            code_prefix: Vec::new(),
            page_limits: Some(lim),
            controls: Vec::new(),
            groups: vec![
                simple_grp(
                    Some("D"),
                    ReportGroupKind::Detail,
                    Some(LinePositionSpec::Absolute(5)),
                    vec![lit_item(None, 1, b"D")],
                ),
                simple_grp(
                    None,
                    ReportGroupKind::PageFooting,
                    Some(pf_line),
                    vec![lit_item(None, 1, b"PF")],
                ),
            ],
            sum_counters: Vec::new(),
        };
        let mut st = ReportState::try_new(rd)?;
        st.initiate()?;
        st.generate(&GenerateTarget::DetailName(String::from("D")), &mut env)?;
        assert!(st.terminate(&mut env).is_err());
    }

    let rd_rf = ReportDesc {
        report_name: String::from("ERR-RF"),
        code_prefix: Vec::new(),
        page_limits: Some(lim),
        controls: Vec::new(),
        groups: vec![
            simple_grp(
                Some("D"),
                ReportGroupKind::Detail,
                Some(LinePositionSpec::Absolute(5)),
                vec![lit_item(None, 1, b"D")],
            ),
            simple_grp(
                None,
                ReportGroupKind::ReportFooting,
                None,
                vec![lit_item(Some(LinePositionSpec::relative(25)), 1, b"OVER")],
            ),
        ],
        sum_counters: Vec::new(),
    };
    let mut st_rf = ReportState::try_new(rd_rf)?;
    st_rf.initiate()?;
    st_rf.generate(&GenerateTarget::DetailName(String::from("D")), &mut env)?;
    assert!(st_rf.terminate(&mut env).is_err());
    Ok(())
}

/// Covers `TERMINATE` when `PAGE FOOTING` uses relative positioning alongside `REPORT FOOTING`
/// and when an absolute `REPORT FOOTING` line precedes the current `LINE-COUNTER`.
#[test]
fn terminate_relative_pf_and_past_line_rf() -> Result<()> {
    let lim = PageLimits::try_new(12, Some(1), Some(2), Some(8), Some(9))?;
    let mut env = MemoryReportEnv::new();

    // Case 1: Relative `PAGE FOOTING` alongside `REPORT FOOTING`.
    let rd_rel_pf = ReportDesc {
        report_name: String::from("REL-PF-RF"),
        code_prefix: Vec::new(),
        page_limits: Some(lim),
        controls: Vec::new(),
        groups: vec![
            simple_grp(
                Some("D"),
                ReportGroupKind::Detail,
                Some(LinePositionSpec::Absolute(5)),
                vec![lit_item(None, 1, b"DET")],
            ),
            simple_grp(
                None,
                ReportGroupKind::PageFooting,
                Some(LinePositionSpec::relative(1)),
                vec![lit_item(None, 1, b"PF-REL")],
            ),
            simple_grp(
                None,
                ReportGroupKind::ReportFooting,
                Some(LinePositionSpec::Absolute(11)),
                vec![lit_item(None, 1, b"RF-END")],
            ),
        ],
        sum_counters: Vec::new(),
    };
    let mut st_rel = ReportState::try_new(rd_rel_pf)?;
    st_rel.initiate()?;
    st_rel.generate(&GenerateTarget::DetailName(String::from("D")), &mut env)?;
    st_rel.terminate(&mut env)?;
    assert_eq!(st_rel.emitted_lines().len(), 3);

    // Case 2: Absolute `RF` at line 4 while `LINE-COUNTER` is already 6 (`report_line <= line_counter`),
    // causing `PF` (line 10) to print on Page 1 and `RF` (line 4) to advance to Page 2.
    let rd_past_rf = ReportDesc {
        report_name: String::from("PAST-RF"),
        code_prefix: Vec::new(),
        page_limits: Some(lim),
        controls: Vec::new(),
        groups: vec![
            simple_grp(
                Some("D"),
                ReportGroupKind::Detail,
                Some(LinePositionSpec::Absolute(6)),
                vec![lit_item(None, 1, b"DET6")],
            ),
            simple_grp(
                None,
                ReportGroupKind::PageFooting,
                Some(LinePositionSpec::Absolute(10)),
                vec![lit_item(None, 1, b"PF10")],
            ),
            simple_grp(
                None,
                ReportGroupKind::ReportFooting,
                Some(LinePositionSpec::Absolute(4)),
                vec![lit_item(None, 1, b"RF4")],
            ),
        ],
        sum_counters: Vec::new(),
    };
    let mut st_past = ReportState::try_new(rd_past_rf)?;
    st_past.initiate()?;
    st_past.generate(&GenerateTarget::DetailName(String::from("D")), &mut env)?;
    st_past.terminate(&mut env)?;
    assert_eq!(st_past.page_counter(), 2);
    assert_eq!(
        st_past.emitted_lines().last().map(|l| l.line_number),
        Some(4)
    );
    Ok(())
}
