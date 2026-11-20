//! Runtime data-binding environment and `USE BEFORE REPORTING` hook context for `ReportState`.

#![deny(clippy::indexing_slicing)]

use std::collections::BTreeMap;

use coblang_common::{CoblangError, Result};
use coblang_decimal::FixedDecimal;
use coblang_pic::{MoveFieldDesc, decode_numeric_field};

use crate::descriptor::report_err;
use crate::types::{ConditionId, FieldId, ReportGroupKind, SumCounterId};

/// Borrowed or owned snapshot of a live COBOL Data Division field read by `ReportState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldValue {
    /// Raw storage bytes of the referenced field.
    pub bytes: Vec<u8>,
    /// Optional [`MoveFieldDesc`] describing numeric/edited storage representation (`None` for raw ASCII bytes).
    pub desc: Option<MoveFieldDesc>,
}

impl FieldValue {
    /// Constructs an untyped raw-byte [`FieldValue`].
    ///
    /// # Arguments
    ///
    /// * `bytes` - Raw storage byte slice.
    #[must_use]
    pub fn raw(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            desc: None,
        }
    }

    /// Constructs a typed [`FieldValue`] paired with its [`MoveFieldDesc`].
    ///
    /// # Arguments
    ///
    /// * `bytes` - Raw storage byte slice.
    /// * `desc` - Storage descriptor (`ZonedDecimal`, `PackedDecimal`, `BinaryInt`, `Alphanumeric`, etc.).
    #[must_use]
    pub fn typed(bytes: &[u8], desc: MoveFieldDesc) -> Self {
        Self {
            bytes: bytes.to_vec(),
            desc: Some(desc),
        }
    }

    /// Decodes this field into an exact [`FixedDecimal`] (using `decode_numeric_field` when typed, else ASCII parsing).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if storage decoding or ASCII numeric parsing fails.
    pub fn decode_decimal(&self) -> Result<FixedDecimal> {
        if let Some(desc) = &self.desc {
            return decode_numeric_field(&self.bytes, desc);
        }
        let text = std::str::from_utf8(&self.bytes).map_err(|_| CoblangError::DecimalDomain {
            operation: "FieldValue::decode_decimal",
            message: String::from("non-UTF-8 bytes in untyped numeric field"),
        })?;
        FixedDecimal::parse_str(text)
    }
}

/// Mutable declarative context exposed to `USE BEFORE REPORTING` procedures.
#[derive(Debug)]
pub struct ReportHookContext<'a> {
    /// Name of the active `RD` report.
    report_name: &'a str,
    /// Optional `01`-level group name being presented.
    group_name: Option<&'a str>,
    /// Classification of the group being presented.
    group_kind: ReportGroupKind,
    /// Current `PAGE-COUNTER` value (mutable by user declaratives).
    page_counter: &'a mut u32,
    /// Current `LINE-COUNTER` value (read-only snapshot prior to formatting the group).
    line_counter: u32,
    /// `SUPPRESS PRINTING` flag for the current group presentation.
    suppressed: &'a mut bool,
    /// Live `SUM` accumulator values indexed by [`SumCounterId`].
    sum_values: &'a [FixedDecimal],
}

impl<'a> ReportHookContext<'a> {
    /// Creates a scoped [`ReportHookContext`] for a single `USE BEFORE REPORTING` invocation.
    ///
    /// # Arguments
    ///
    /// * `report_name` - Owning `RD` identifier.
    /// * `group_name` - Optional `01` report group identifier.
    /// * `group_kind` - [`ReportGroupKind`] about to be presented.
    /// * `page_counter` - Mutable reference to `PAGE-COUNTER`.
    /// * `line_counter` - Current `LINE-COUNTER` value.
    /// * `suppressed` - Mutable `SUPPRESS PRINTING` flag for this group.
    /// * `sum_values` - Borrowed `SUM` accumulator values.
    pub(crate) fn new(
        report_name: &'a str,
        group_name: Option<&'a str>,
        group_kind: ReportGroupKind,
        page_counter: &'a mut u32,
        line_counter: u32,
        suppressed: &'a mut bool,
        sum_values: &'a [FixedDecimal],
    ) -> Self {
        Self {
            report_name,
            group_name,
            group_kind,
            page_counter,
            line_counter,
            suppressed,
            sum_values,
        }
    }

    /// Returns the `RD` report name.
    #[must_use]
    pub const fn report_name(&self) -> &'a str {
        self.report_name
    }

    /// Returns the `01`-level group identifier if the group is named.
    #[must_use]
    pub const fn group_name(&self) -> Option<&'a str> {
        self.group_name
    }

    /// Returns the [`ReportGroupKind`] being presented.
    #[must_use]
    pub const fn group_kind(&self) -> ReportGroupKind {
        self.group_kind
    }

    /// Reads the current `PAGE-COUNTER` special register.
    #[must_use]
    pub const fn page_counter(&self) -> u32 {
        *self.page_counter
    }

    /// Updates the `PAGE-COUNTER` special register from inside a `USE BEFORE REPORTING` declarative.
    ///
    /// # Arguments
    ///
    /// * `value` - New `PAGE-COUNTER` integer value.
    pub fn set_page_counter(&mut self, value: u32) {
        *self.page_counter = value;
    }

    /// Reads the current `LINE-COUNTER` special register.
    #[must_use]
    pub const fn line_counter(&self) -> u32 {
        self.line_counter
    }

    /// Executes `SUPPRESS PRINTING`, skipping emission of the current report group.
    pub fn suppress_printing(&mut self) {
        *self.suppressed = true;
    }

    /// Returns `true` if `SUPPRESS PRINTING` has been activated for this group.
    #[must_use]
    pub const fn is_suppressed(&self) -> bool {
        *self.suppressed
    }

    /// Reads the current [`FixedDecimal`] value of `SUM` counter `id`.
    ///
    /// # Arguments
    ///
    /// * `id` - Target [`SumCounterId`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ReportEngine`] if `id` is out of range.
    pub fn sum_counter(&self, id: SumCounterId) -> Result<FixedDecimal> {
        self.sum_values
            .get(id.as_usize())
            .copied()
            .ok_or_else(|| report_err(self.report_name, "invalid SumCounterId in hook"))
    }
}

/// Runtime callback interface bridging `ReportState` to live COBOL Data Division storage and declaratives.
pub trait ReportEnv {
    /// Reads the current bytes and optional storage descriptor of `field_id`.
    ///
    /// # Arguments
    ///
    /// * `field_id` - Referenced [`FieldId`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `field_id` is unbound or inaccessible.
    fn read_field(&self, field_id: FieldId) -> Result<FieldValue>;

    /// Temporarily writes saved control-break bytes back into `field_id` during `CONTROL FOOTING` presentation.
    ///
    /// # Arguments
    ///
    /// * `field_id` - Control [`FieldId`] being restored.
    /// * `bytes` - Saved prior or restored current raw storage bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `field_id` cannot be updated.
    fn write_control_bytes(&mut self, field_id: FieldId, bytes: &[u8]) -> Result<()>;

    /// Evaluates an external `PRESENT WHEN` [`ConditionId`] predicate.
    ///
    /// # Arguments
    ///
    /// * `condition_id` - Referenced [`ConditionId`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `condition_id` is unknown or evaluation fails.
    fn eval_condition(&self, condition_id: ConditionId) -> Result<bool>;

    /// Invokes any registered `USE BEFORE REPORTING` declarative for the group described by `ctx`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Mutable [`ReportHookContext`] exposing special registers and `SUPPRESS PRINTING`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if the declarative procedure raises a runtime error.
    fn before_reporting(&mut self, ctx: &mut ReportHookContext<'_>) -> Result<()>;
}

/// Boxed `USE BEFORE REPORTING` callback type used by [`MemoryReportEnv`].
pub type MemoryHookCallback =
    Box<dyn FnMut(&mut MemoryReportEnv, &mut ReportHookContext<'_>) -> Result<()>>;

/// Self-contained in-memory [`ReportEnv`] for standalone execution, C-ABI staging, and deterministic testing.
#[derive(Default)]
pub struct MemoryReportEnv {
    /// Field values indexed by [`FieldId`].
    fields: BTreeMap<FieldId, FieldValue>,
    /// Boolean condition values indexed by [`ConditionId`].
    conditions: BTreeMap<ConditionId, bool>,
    /// Optional `USE BEFORE REPORTING` callback.
    hook: Option<MemoryHookCallback>,
}

impl core::fmt::Debug for MemoryReportEnv {
    /// Formats [`MemoryReportEnv`] without requiring `Debug` on the boxed hook closure.
    ///
    /// # Arguments
    ///
    /// * `f` - Destination debug formatter.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MemoryReportEnv")
            .field("fields", &self.fields)
            .field("conditions", &self.conditions)
            .field("has_hook", &self.hook.is_some())
            .finish()
    }
}

impl MemoryReportEnv {
    /// Creates an empty [`MemoryReportEnv`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds or updates an untyped raw-byte field value.
    ///
    /// # Arguments
    ///
    /// * `field_id` - Target [`FieldId`].
    /// * `bytes` - Raw byte slice to store.
    pub fn set_raw_field(&mut self, field_id: FieldId, bytes: &[u8]) {
        self.fields.insert(field_id, FieldValue::raw(bytes));
    }

    /// Binds or updates a typed COBOL field value with its [`MoveFieldDesc`].
    ///
    /// # Arguments
    ///
    /// * `field_id` - Target [`FieldId`].
    /// * `bytes` - Raw storage bytes.
    /// * `desc` - Storage [`MoveFieldDesc`].
    pub fn set_typed_field(&mut self, field_id: FieldId, bytes: &[u8], desc: MoveFieldDesc) {
        self.fields.insert(field_id, FieldValue::typed(bytes, desc));
    }

    /// Binds or updates an external `PRESENT WHEN` [`ConditionId`] truth value.
    ///
    /// # Arguments
    ///
    /// * `condition_id` - Target [`ConditionId`].
    /// * `active` - Current boolean evaluation result.
    pub fn set_condition(&mut self, condition_id: ConditionId, active: bool) {
        self.conditions.insert(condition_id, active);
    }

    /// Installs a `USE BEFORE REPORTING` hook callback.
    ///
    /// # Arguments
    ///
    /// * `callback` - Boxed closure receiving `(&mut MemoryReportEnv, &mut ReportHookContext)`.
    pub fn set_before_reporting_hook(&mut self, callback: MemoryHookCallback) {
        self.hook = Some(callback);
    }
}

impl ReportEnv for MemoryReportEnv {
    /// Reads the stored [`FieldValue`] for `field_id`.
    ///
    /// # Arguments
    ///
    /// * `field_id` - Target [`FieldId`].
    fn read_field(&self, field_id: FieldId) -> Result<FieldValue> {
        self.fields.get(&field_id).cloned().ok_or_else(|| {
            report_err(
                "<env>",
                &format!("unbound FieldId({}) in MemoryReportEnv", field_id.as_u32()),
            )
        })
    }

    /// Updates raw bytes of `field_id` during control-break prior-value restoration.
    ///
    /// # Arguments
    ///
    /// * `field_id` - Control [`FieldId`] to restore.
    /// * `bytes` - Replacement raw storage bytes.
    fn write_control_bytes(&mut self, field_id: FieldId, bytes: &[u8]) -> Result<()> {
        let entry = self.fields.get_mut(&field_id).ok_or_else(|| {
            report_err(
                "<env>",
                &format!(
                    "cannot restore unbound control FieldId({})",
                    field_id.as_u32()
                ),
            )
        })?;
        entry.bytes.clear();
        entry.bytes.extend_from_slice(bytes);
        Ok(())
    }

    /// Looks up the boolean value bound to `condition_id`.
    ///
    /// # Arguments
    ///
    /// * `condition_id` - Target [`ConditionId`].
    fn eval_condition(&self, condition_id: ConditionId) -> Result<bool> {
        self.conditions.get(&condition_id).copied().ok_or_else(|| {
            report_err(
                "<env>",
                &format!(
                    "unbound ConditionId({}) in MemoryReportEnv",
                    condition_id.as_u32()
                ),
            )
        })
    }

    /// Invokes the registered `USE BEFORE REPORTING` hook closure if present.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Mutable [`ReportHookContext`] for the current group presentation.
    fn before_reporting(&mut self, ctx: &mut ReportHookContext<'_>) -> Result<()> {
        if let Some(mut cb) = self.hook.take() {
            let res = cb(self, ctx);
            self.hook = Some(cb);
            return res;
        }
        Ok(())
    }
}
