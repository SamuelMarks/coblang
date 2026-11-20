//! Recursive ISO COBOL `INITIALIZE` engine (`REPLACING`, `TO VALUE`, `WITH FILLER`, `REDEFINES` exclusion).

use coblang_common::{CoblangError, Result};
use coblang_hir::{DataCategory, HirFigurative};

use crate::move_engine::{MoveFieldDesc, MoveSource, execute_move};

/// Bitmask of ISO COBOL [`DataCategory`] targets for `INITIALIZE ... TO VALUE` or `DEFAULT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct InitCategoryMask(u16);

impl InitCategoryMask {
    /// Empty category mask (`0`).
    pub const NONE: Self = Self(0);

    /// All categories (`ALL TO VALUE`).
    pub const ALL: Self = Self(0xFFFF);

    /// Returns the single-category bitmask for `cat`.
    ///
    /// # Arguments
    ///
    /// * `cat` - Target [`DataCategory`].
    #[must_use]
    pub const fn from_category(cat: DataCategory) -> Self {
        let bit = match cat {
            DataCategory::Alphabetic => 1 << 0,
            DataCategory::Alphanumeric => 1 << 1,
            DataCategory::AlphanumericEdited => 1 << 2,
            DataCategory::Numeric => 1 << 3,
            DataCategory::NumericEdited => 1 << 4,
            DataCategory::National => 1 << 5,
            DataCategory::NationalEdited => 1 << 6,
            DataCategory::Utf8 => 1 << 7,
            DataCategory::BooleanBit => 1 << 8,
            DataCategory::Pointer => 1 << 9,
            DataCategory::Index => 1 << 10,
            DataCategory::ObjectReference => 1 << 11,
            DataCategory::Group => 1 << 12,
        };
        Self(bit)
    }

    /// Combines two [`InitCategoryMask`] bitsets (`self | other`).
    ///
    /// # Arguments
    ///
    /// * `other` - Second category mask.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns `true` if `cat` is enabled in `self`.
    ///
    /// # Arguments
    ///
    /// * `cat` - Category to test.
    #[must_use]
    pub const fn contains(self, cat: DataCategory) -> bool {
        (self.0 & Self::from_category(cat).0) != 0
    }

    /// Returns the raw 16-bit category bitmask.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Constructs an [`InitCategoryMask`] from raw 16-bit flags.
    ///
    /// # Arguments
    ///
    /// * `raw` - Raw bitmask integer.
    #[must_use]
    pub const fn from_bits(raw: u16) -> Self {
        Self(raw)
    }
}

/// Elementary target field entry within a group or standalone `INITIALIZE` target.
#[derive(Debug, Clone)]
pub struct InitTargetField<'a> {
    /// Byte offset relative to the start of the `INITIALIZE` target buffer.
    pub offset: usize,
    /// Storage length in bytes of this elementary occurrence.
    pub byte_len: usize,
    /// Field descriptor (`HirType` + dialect formatting parameters).
    pub desc: MoveFieldDesc,
    /// `true` when the item is unnamed or explicitly `FILLER`.
    pub is_filler: bool,
    /// `true` when the item or an ancestor subordinate to the target has a `REDEFINES` clause.
    pub is_redefined: bool,
    /// Pre-encoded `VALUE` clause bytes for `INITIALIZE ... TO VALUE`, if declared on this item.
    pub value_bytes: Option<&'a [u8]>,
}

/// Policy flags for `INITIALIZE` (`WITH FILLER`, `DEFAULT` pointer/index zeroing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillerPolicy {
    /// Skip `FILLER` / unnamed elementary items (standard COBOL default).
    SkipFiller,
    /// Initialize `FILLER` items as well (`INITIALIZE ... WITH FILLER`).
    IncludeFiller,
}

/// Strongly-typed `WITH FILLER` inclusion policy for [`InitializePlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WithFiller {
    /// Exclude unnamed and explicit `FILLER` items (ISO COBOL default).
    ExcludeFiller,
    /// Include `FILLER` items (`INITIALIZE ... WITH FILLER`).
    IncludeFiller,
}

impl WithFiller {
    /// Converts `self` into the runtime [`FillerPolicy`].
    #[must_use]
    pub const fn to_filler_policy(self) -> FillerPolicy {
        match self {
            Self::ExcludeFiller => FillerPolicy::SkipFiller,
            Self::IncludeFiller => FillerPolicy::IncludeFiller,
        }
    }

    /// Converts a [`FillerPolicy`] into [`WithFiller`].
    ///
    /// # Arguments
    ///
    /// * `policy` - Runtime [`FillerPolicy`].
    #[must_use]
    pub const fn from_filler_policy(policy: FillerPolicy) -> Self {
        match policy {
            FillerPolicy::SkipFiller => Self::ExcludeFiller,
            FillerPolicy::IncludeFiller => Self::IncludeFiller,
        }
    }
}

/// Initialization value-source mode (`DEFAULT` category initialization vs `TO VALUE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueMode {
    /// Initialize eligible items using their ISO category defaults (`ZERO` or `SPACE`).
    DefaultCategoryInit,
    /// Restore declared `VALUE` clauses on matching categories (`INITIALIZE ... TO VALUE`).
    ToDefinedValue,
}

/// Single `REPLACING <category> DATA BY <source>` rule inside [`InitializePlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacingRule<'a> {
    /// Target [`DataCategory`] matched by this rule.
    pub category: DataCategory,
    /// Replacement [`MoveSource`] moved into every matching elementary item.
    pub source: MoveSource<'a>,
}

/// Strongly-typed `INITIALIZE` execution plan combining [`WithFiller`], [`ValueMode`], and [`ReplacingRule`]s.
#[derive(Debug, Clone)]
pub struct InitializePlan<'a> {
    /// Whether `FILLER` items are excluded or included.
    pub with_filler: WithFiller,
    /// Whether declared `VALUE` clauses are restored (`ToDefinedValue`) or ignored (`DefaultCategoryInit`).
    pub value_mode: ValueMode,
    /// Category mask eligible for `TO VALUE` when `value_mode == ValueMode::ToDefinedValue`.
    pub value_categories: InitCategoryMask,
    /// Ordered `REPLACING` category table.
    pub replacing: &'a [ReplacingRule<'a>],
    /// Pointer/index/bit handling mode (`StandardCobol` vs `IncludePointersAndBit`).
    pub default_mode: DefaultInitMode,
}

impl Default for InitializePlan<'_> {
    /// Returns standard `INITIALIZE` (`ExcludeFiller`, `DefaultCategoryInit`, empty `replacing`, `StandardCobol`).
    fn default() -> Self {
        Self {
            with_filler: WithFiller::ExcludeFiller,
            value_mode: ValueMode::DefaultCategoryInit,
            value_categories: InitCategoryMask::ALL,
            replacing: &[],
            default_mode: DefaultInitMode::StandardCobol,
        }
    }
}

/// Executes a strongly-typed [`InitializePlan`] across `fields` inside `target_buf`.
///
/// # Arguments
///
/// * `target_buf` - Mutable byte slice of the elementary or group target item.
/// * `fields` - Flattened elementary subordinate items.
/// * `plan` - [`InitializePlan`] governing [`WithFiller`], [`ValueMode`], and [`ReplacingRule`]s.
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] if any field range lies outside `target_buf` or `MOVE` fails.
pub fn execute_initialize_plan(
    target_buf: &mut [u8],
    fields: &[InitTargetField<'_>],
    plan: &InitializePlan<'_>,
) -> Result<()> {
    let pairs: Vec<(DataCategory, MoveSource<'_>)> = plan
        .replacing
        .iter()
        .map(|r| (r.category, r.source.clone()))
        .collect();
    let to_value_mask = match plan.value_mode {
        ValueMode::DefaultCategoryInit => InitCategoryMask::NONE,
        ValueMode::ToDefinedValue => plan.value_categories,
    };
    let spec = InitializeSpec {
        filler_policy: plan.with_filler.to_filler_policy(),
        to_value_mask,
        replacements: &pairs,
        default_mode: plan.default_mode,
    };
    execute_initialize(target_buf, fields, &spec)
}

/// Pointer/index/object-ref handling mode during `INITIALIZE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultInitMode {
    /// Standard `INITIALIZE`: leave `POINTER`, `INDEX`, and `OBJECT REFERENCE` untouched unless explicitly replaced.
    StandardCobol,
    /// `INITIALIZE ... DEFAULT`: also zero `POINTER`, `INDEX`, `OBJECT REFERENCE`, and `BIT` items.
    IncludePointersAndBit,
}

/// Complete specification of an `INITIALIZE` statement execution.
#[derive(Debug, Clone)]
pub struct InitializeSpec<'a> {
    /// Whether `FILLER` items participate.
    pub filler_policy: FillerPolicy,
    /// Categories restored from their declared `VALUE` clause (`TO VALUE`).
    pub to_value_mask: InitCategoryMask,
    /// Explicit `(category, replacement_source)` pairs (`REPLACING ... BY ...`).
    pub replacements: &'a [(DataCategory, MoveSource<'a>)],
    /// Whether `DEFAULT` mode zeroes pointers/indices/bits when `replacements` is empty.
    pub default_mode: DefaultInitMode,
}

impl Default for InitializeSpec<'_> {
    /// Returns standard plain `INITIALIZE` (`SkipFiller`, no `TO VALUE`, no `REPLACING`, `StandardCobol`).
    fn default() -> Self {
        Self {
            filler_policy: FillerPolicy::SkipFiller,
            to_value_mask: InitCategoryMask::NONE,
            replacements: &[],
            default_mode: DefaultInitMode::StandardCobol,
        }
    }
}

/// Executes ISO COBOL `INITIALIZE` across `fields` inside `target_buf`.
///
/// # Arguments
///
/// * `target_buf` - Mutable byte slice of the elementary or group target item.
/// * `fields` - Flattened elementary subordinate items (including expanded `OCCURS` elements if any).
/// * `spec` - [`InitializeSpec`] governing `FILLER`, `REDEFINES`, `REPLACING`, and `TO VALUE`.
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] if any field range lies outside `target_buf` or `MOVE` fails.
pub fn execute_initialize(
    target_buf: &mut [u8],
    fields: &[InitTargetField<'_>],
    spec: &InitializeSpec<'_>,
) -> Result<()> {
    for field in fields {
        // ISO COBOL: subordinate REDEFINES items are always excluded from INITIALIZE.
        if field.is_redefined {
            continue;
        }
        if field.is_filler && spec.filler_policy == FillerPolicy::SkipFiller {
            continue;
        }

        let buf_len = target_buf.len();
        let end = field.offset.saturating_add(field.byte_len);
        let dst_slice =
            target_buf
                .get_mut(field.offset..end)
                .ok_or_else(|| CoblangError::PictureCodec {
                    picture: String::from("INITIALIZE"),
                    message: format!(
                        "field range {}..{end} exceeds buffer length {buf_len}",
                        field.offset,
                    ),
                })?;

        let cat = field.desc.category();

        // 1. Explicit `REPLACING <cat> BY <src>` takes highest precedence.
        if let Some((_, rep_src)) = spec.replacements.iter().find(|(c, _)| *c == cat) {
            execute_move(rep_src.clone(), dst_slice, &field.desc)?;
            continue;
        }

        // If `REPLACING` was specified and did NOT match `cat` (and `to_value_mask` doesn't match),
        // ISO COBOL leaves non-matching categories unchanged!
        if spec.to_value_mask.contains(cat)
            && let Some(val_b) = field.value_bytes
        {
            let copy_len = dst_slice.len().min(val_b.len());
            dst_slice
                .get_mut(..copy_len)
                .unwrap_or(&mut [])
                .copy_from_slice(val_b.get(..copy_len).unwrap_or(&[]));
            continue;
        }

        if !spec.replacements.is_empty() {
            continue;
        }

        // 3. Default category initialization (`ZERO` for numeric/numeric-edited, `SPACE` for text).
        match cat {
            DataCategory::Numeric | DataCategory::NumericEdited => {
                execute_move(
                    MoveSource::Figurative(HirFigurative::Zero),
                    dst_slice,
                    &field.desc,
                )?;
            }
            DataCategory::Alphabetic
            | DataCategory::Alphanumeric
            | DataCategory::AlphanumericEdited
            | DataCategory::National
            | DataCategory::NationalEdited
            | DataCategory::Utf8
            | DataCategory::Group => {
                execute_move(
                    MoveSource::Figurative(HirFigurative::Space),
                    dst_slice,
                    &field.desc,
                )?;
            }
            DataCategory::BooleanBit
            | DataCategory::Pointer
            | DataCategory::Index
            | DataCategory::ObjectReference => {
                if spec.default_mode == DefaultInitMode::IncludePointersAndBit {
                    dst_slice.fill(0);
                }
            }
        }
    }
    Ok(())
}
