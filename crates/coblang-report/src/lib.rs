//! Pure-Rust declarative `REPORT SECTION` (`RD`) pagination, control-break, `SUM`, and `GROUP INDICATE` engine (`Milestone M18`).

#![deny(clippy::indexing_slicing)]

/// Declarative `RD` report descriptor, hierarchical `01`..`49` group nodes, `SUM` rules, and schema validation.
pub mod descriptor;
/// Stateful `ReportState` engine executing `INITIATE`, `GENERATE`, `TERMINATE`, and `SUPPRESS PRINTING`.
pub mod engine;
/// Runtime data-binding environment (`ReportEnv`, `MemoryReportEnv`) and `USE BEFORE REPORTING` hook context.
pub mod env;
/// Strongly-typed domain primitives (`ReportGroupKind`, `LinePositionSpec`, `ControlLevelIndex`, `PageLimits`, `ReportLine`).
pub mod types;

pub use descriptor::{
    FieldFormatter, FlatReportEntry, ItemValueSource, PresentWhenSpec, ReportDesc, ReportFieldItem,
    ReportGroupDesc, ReportNode, ReportSubGroup, SumAddend, SumCounterDesc, SumUponFilter,
    build_group_from_flat_entries,
};
pub use engine::ReportState;
pub use env::{FieldValue, MemoryHookCallback, MemoryReportEnv, ReportEnv, ReportHookContext};
pub use types::{
    ColumnPositionSpec, ConditionId, ControlDef, ControlKind, ControlLevelIndex, FieldId,
    GenerateTarget, GroupIndicatePolicy, LinePositionSpec, NextGroupSpec, PageBreakStyle,
    PageLimits, RenderFormat, ReportGroupKind, ReportLifecycleState, ReportLine,
    ReportLineAdvancing, SumCounterId, render_lines_to_bytes, write_rendered_lines,
};
