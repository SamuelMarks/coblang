//! Hierarchical `SCREEN SECTION` layout compiler, clause inheritance/override engine, and `EC-SCREEN-*` validator.

use coblang_common::{CoblangError, DataItemId, LevelNumber, Result, SymbolId};
use coblang_hir::HirType;
use coblang_pic::{MoveFieldDesc, PictureProgram};

use crate::types::{
    CellStyle, CursorPosition, PromptSpec, ScreenAttrFlags, ScreenCol, ScreenColor,
    ScreenDimensions, ScreenExceptionKind, ScreenRow,
};

/// Vertical `LINE` positioning clause on a `SCREEN SECTION` entry or `DISPLAY`/`ACCEPT ... AT` phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineSpec {
    /// No `LINE` clause specified; stay on the current sequential line.
    #[default]
    Omitted,
    /// `LINE 0`: start at row `1` if at screen start, otherwise advance to the next row.
    Zero,
    /// `LINE NUMBER IS n`: absolute 1-based row coordinate.
    Absolute(ScreenRow),
    /// `LINE NUMBER IS PLUS n`: advance downward by `n` rows from current row.
    RelativePlus(u16),
    /// `LINE NUMBER IS MINUS n`: move upward by `n` rows from current row.
    RelativeMinus(u16),
}

/// Horizontal `COLUMN` / `COL` positioning clause on a `SCREEN SECTION` entry or `DISPLAY`/`ACCEPT ... AT` phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColSpec {
    /// No `COLUMN` clause specified (`col 1` when `LINE` is given, else sequential column after prior item).
    #[default]
    Omitted,
    /// `COLUMN 0`: place at column `1`.
    Zero,
    /// `COLUMN NUMBER IS c`: absolute 1-based column coordinate.
    Absolute(ScreenCol),
    /// `COLUMN NUMBER IS PLUS c`: advance rightward by `c` columns from current sequential column.
    RelativePlus(u16),
    /// `COLUMN NUMBER IS MINUS c`: move leftward by `c` columns from current sequential column.
    RelativeMinus(u16),
}

/// Decodes extended `DISPLAY` / `ACCEPT ... AT llcc` or `AT lllccc` compact integer coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AtCompactSpec {
    /// Decoded vertical `LINE` specification.
    pub line: LineSpec,
    /// Decoded horizontal `COLUMN` specification.
    pub col: ColSpec,
}

impl AtCompactSpec {
    /// Decodes a 4-digit `LLCC` integer (`0000..=9999`, where `LL = llcc / 100` and `CC = llcc % 100`).
    ///
    /// # Arguments
    ///
    /// * `llcc` - Compact 4-digit `LLCC` coordinate (`> 9999` is rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `llcc > 9999`.
    pub fn from_four_digit_llcc(llcc: u16) -> Result<Self> {
        if llcc > 9999 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "AtCompactSpec::LLCC",
                value: llcc.to_string(),
                reason: String::from("4-digit AT coordinate must be in 0..=9999"),
            });
        }
        let raw_line = llcc / 100;
        let raw_col = llcc % 100;
        Ok(Self::from_raw_components(raw_line, raw_col))
    }

    /// Decodes a 6-digit `LLLCCC` integer (`000000..=999999`, where `LLL = val / 1000` and `CCC = val % 1000`).
    ///
    /// # Arguments
    ///
    /// * `lllccc` - Compact 6-digit `LLLCCC` coordinate (`> 999_999` is rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `lllccc > 999_999`.
    pub fn from_six_digit_lllccc(lllccc: u32) -> Result<Self> {
        if lllccc > 999_999 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "AtCompactSpec::LLLCCC",
                value: lllccc.to_string(),
                reason: String::from("6-digit AT coordinate must be in 0..=999999"),
            });
        }
        let raw_line = u16::try_from(lllccc / 1000).unwrap_or(0);
        let raw_col = u16::try_from(lllccc % 1000).unwrap_or(0);
        Ok(Self::from_raw_components(raw_line, raw_col))
    }

    /// Converts bounded `0..=999` raw line and column integers into [`LineSpec`] and [`ColSpec`].
    ///
    /// # Arguments
    ///
    /// * `raw_line` - Bounded line component in `0..=999`.
    /// * `raw_col` - Bounded column component in `0..=999`.
    fn from_raw_components(raw_line: u16, raw_col: u16) -> Self {
        let line = match ScreenRow::try_new(raw_line) {
            Ok(r) => LineSpec::Absolute(r),
            Err(_) => LineSpec::Zero,
        };
        let col = match ScreenCol::try_new(raw_col) {
            Ok(c) => ColSpec::Absolute(c),
            Err(_) => ColSpec::Zero,
        };
        Self { line, col }
    }
}

/// Tiling policy for `VALUE` literals in `SCREEN SECTION` (`VALUE "abc"` vs `VALUE ALL "-"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LiteralTileMode {
    /// Emit literal bytes once, padding with spaces if `explicit_size` exceeds literal length.
    #[default]
    Single,
    /// Repeat literal pattern cyclically across `explicit_size` (`VALUE ALL "pattern"`).
    RepeatAll,
}

/// Validated `PICTURE` and COBOL storage type descriptor for a data-bound screen field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenFieldPic {
    /// Visible character cell width on screen (`>= 1`).
    display_width: u16,
    /// Category-aware `coblang-pic` field descriptor driving `MOVE` and `DISPLAY` formatting.
    move_desc: MoveFieldDesc,
    /// Optional compiled `PictureProgram` VM for edited formatting and numeric de-editing.
    edit_program: Option<PictureProgram>,
}

impl ScreenFieldPic {
    /// Constructs a validated [`ScreenFieldPic`] with non-zero `display_width`.
    ///
    /// # Arguments
    ///
    /// * `display_width` - Visible cell width on screen (`1..=999`).
    /// * `move_desc` - Underlying COBOL [`MoveFieldDesc`].
    /// * `edit_program` - Optional compiled [`PictureProgram`] bytecode VM.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `display_width == 0` or `display_width > 999`.
    pub fn try_new(
        display_width: u16,
        move_desc: MoveFieldDesc,
        edit_program: Option<PictureProgram>,
    ) -> Result<Self> {
        if !(1..=ScreenCol::MAX_COL).contains(&display_width) {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "ScreenFieldPic::display_width",
                value: display_width.to_string(),
                reason: String::from("screen field width must be in 1..=999"),
            });
        }
        Ok(Self {
            display_width,
            move_desc,
            edit_program,
        })
    }

    /// Constructs a standard ASCII `PIC X(width)` alphanumeric [`ScreenFieldPic`].
    ///
    /// # Arguments
    ///
    /// * `width` - Alphanumeric character width (`1..=999`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `width == 0` or `width > 999`.
    pub fn alphanumeric(width: u16) -> Result<Self> {
        let ty = HirType::Alphanumeric {
            byte_len: u32::from(width),
            justified: coblang_hir::JustifiedSpec::Left,
        };
        Self::try_new(width, MoveFieldDesc::standard_gnu(ty), None)
    }

    /// Returns the visible screen cell width (`1..=999`).
    #[must_use]
    pub const fn display_width(&self) -> u16 {
        self.display_width
    }

    /// Returns the underlying [`MoveFieldDesc`].
    #[must_use]
    pub const fn move_desc(&self) -> &MoveFieldDesc {
        &self.move_desc
    }

    /// Returns the optional compiled [`PictureProgram`].
    #[must_use]
    pub const fn edit_program(&self) -> Option<&PictureProgram> {
        self.edit_program.as_ref()
    }
}

/// Data-flow binding mode of an elementary `SCREEN SECTION` item (`VALUE`, `FROM`, `TO`, `USING`, or directive-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenDataBinding {
    /// Action-only item (`01 SCR-CLEAR BLANK SCREEN.`) carrying no visible field payload.
    ActionOnly,
    /// Constant `VALUE` label or `VALUE ALL` banner (`05 VALUE "Customer ID:"`).
    Literal {
        /// Raw literal bytes.
        bytes: Vec<u8>,
        /// Single emission vs `ALL` cyclic tiling.
        tile: LiteralTileMode,
        /// Optional explicit `SIZE IS n` cell width override.
        explicit_size: Option<u16>,
    },
    /// Output-only field (`PIC ... FROM src`).
    From {
        /// Bound source Working/Local/Linkage item handle.
        source_id: DataItemId,
        /// `PICTURE` and storage descriptor.
        pic: ScreenFieldPic,
        /// Optional explicit `SIZE IS n` cell width override.
        explicit_size: Option<u16>,
    },
    /// Input-only field (`PIC ... TO dst`).
    To {
        /// Bound destination Working/Local/Linkage item handle.
        target_id: DataItemId,
        /// `PICTURE` and storage descriptor.
        pic: ScreenFieldPic,
        /// Optional explicit `SIZE IS n` cell width override.
        explicit_size: Option<u16>,
    },
    /// Bidirectional display + edit field (`PIC ... USING var`).
    Using {
        /// Bound Working/Local/Linkage item handle read on display/entry and written on accept.
        target_id: DataItemId,
        /// `PICTURE` and storage descriptor.
        pic: ScreenFieldPic,
        /// Optional explicit `SIZE IS n` cell width override.
        explicit_size: Option<u16>,
    },
}

impl ScreenDataBinding {
    /// Computes the visible cell width occupied by this binding (`0` for [`Self::ActionOnly`]).
    #[must_use]
    pub fn effective_width(&self) -> u16 {
        match self {
            Self::ActionOnly => 0,
            Self::Literal {
                bytes,
                explicit_size,
                ..
            } => explicit_size.unwrap_or_else(|| u16::try_from(bytes.len()).unwrap_or(999)),
            Self::From {
                pic, explicit_size, ..
            }
            | Self::To {
                pic, explicit_size, ..
            }
            | Self::Using {
                pic, explicit_size, ..
            } => explicit_size.unwrap_or_else(|| pic.display_width()),
        }
    }

    /// Returns `true` when this field accepts interactive keyboard input (`TO` or `USING`).
    #[must_use]
    pub const fn is_editable(&self) -> bool {
        matches!(self, Self::To { .. } | Self::Using { .. })
    }

    /// Returns the [`DataItemId`] written on `ACCEPT` completion (`Some` for `TO` and `USING`).
    #[must_use]
    pub const fn target_field_id(&self) -> Option<DataItemId> {
        match self {
            Self::To { target_id, .. } | Self::Using { target_id, .. } => Some(*target_id),
            Self::ActionOnly | Self::Literal { .. } | Self::From { .. } => None,
        }
    }

    /// Returns the [`ScreenFieldPic`] when bound to a typed COBOL data item (`FROM`, `TO`, `USING`).
    #[must_use]
    pub const fn field_pic(&self) -> Option<&ScreenFieldPic> {
        match self {
            Self::From { pic, .. } | Self::To { pic, .. } | Self::Using { pic, .. } => Some(pic),
            Self::ActionOnly | Self::Literal { .. } => None,
        }
    }
}

/// Complete visual, color, and input-editing attribute bundle declared on a `SCREEN SECTION` item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ScreenAttributeSet {
    /// Bitmask of visual and editing flags (`HIGHLIGHT`, `REVERSE_VIDEO`, `AUTO`, `SECURE`, etc.).
    pub flags: ScreenAttrFlags,
    /// Optional `FOREGROUND-COLOR` (`0..=7`).
    pub fg_color: Option<ScreenColor>,
    /// Optional `BACKGROUND-COLOR` (`0..=7`).
    pub bg_color: Option<ScreenColor>,
    /// Optional `PROMPT [CHARACTER IS ch]` filler specification.
    pub prompt: Option<PromptSpec>,
}

impl ScreenAttributeSet {
    /// Constructs an empty default [`ScreenAttributeSet`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            flags: ScreenAttrFlags::NONE,
            fg_color: None,
            bg_color: None,
            prompt: None,
        }
    }

    /// Returns `self` with `flags` unioned into its attribute flags.
    ///
    /// # Arguments
    ///
    /// * `flags` - Additional [`ScreenAttrFlags`] to enable.
    #[must_use]
    pub const fn with_flags(mut self, flags: ScreenAttrFlags) -> Self {
        self.flags = self.flags.union(flags);
        self
    }

    /// Returns `self` with `FOREGROUND-COLOR` set to `color`.
    ///
    /// # Arguments
    ///
    /// * `color` - Foreground [`ScreenColor`].
    #[must_use]
    pub const fn with_fg_color(mut self, color: ScreenColor) -> Self {
        self.fg_color = Some(color);
        self
    }

    /// Returns `self` with `BACKGROUND-COLOR` set to `color`.
    ///
    /// # Arguments
    ///
    /// * `color` - Background [`ScreenColor`].
    #[must_use]
    pub const fn with_bg_color(mut self, color: ScreenColor) -> Self {
        self.bg_color = Some(color);
        self
    }

    /// Returns `self` with `PROMPT` enabled using `prompt`.
    ///
    /// # Arguments
    ///
    /// * `prompt` - [`PromptSpec`] character configuration.
    #[must_use]
    pub const fn with_prompt(mut self, prompt: PromptSpec) -> Self {
        self.prompt = Some(prompt);
        self.flags = self.flags.union(ScreenAttrFlags::PROMPT);
        self
    }

    /// Propagates inheritable parent group attributes onto `child`, allowing `child` clauses
    /// (`FOREGROUND-COLOR`, `BACKGROUND-COLOR`, `PROMPT`, `HIGHLIGHT` vs `LOWLIGHT`) to override parent defaults.
    ///
    /// # Arguments
    ///
    /// * `child` - Explicitly declared child attribute bundle.
    #[must_use]
    pub fn inherit_into_child(&self, child: &Self) -> Self {
        let mut flags = self.flags.inherit_into_child(child.flags);
        let prompt = child.prompt.or(self.prompt);
        if prompt.is_some() {
            flags.insert(ScreenAttrFlags::PROMPT);
        }
        Self {
            flags,
            fg_color: child.fg_color.or(self.fg_color),
            bg_color: child.bg_color.or(self.bg_color),
            prompt,
        }
    }

    /// Extracts the [`CellStyle`] for rendering cells belonging to this item.
    #[must_use]
    pub const fn cell_style(&self) -> CellStyle {
        CellStyle::new(self.fg_color, self.bg_color, self.flags)
    }

    /// Returns the active [`PromptSpec`] when `PROMPT` is enabled on this item.
    #[must_use]
    pub fn effective_prompt(&self) -> Option<PromptSpec> {
        if self.flags.contains(ScreenAttrFlags::PROMPT) {
            Some(self.prompt.unwrap_or(PromptSpec::UNDERSCORE))
        } else {
            None
        }
    }
}

/// Elementary `SCREEN SECTION` item specification prior to coordinate resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenFieldSpec {
    /// Optional screen item identifier (`None` for `FILLER`).
    pub name: Option<SymbolId>,
    /// Vertical `LINE` positioning clause.
    pub line: LineSpec,
    /// Horizontal `COLUMN` positioning clause.
    pub col: ColSpec,
    /// Declared visual and input attributes.
    pub attrs: ScreenAttributeSet,
    /// Data-flow binding (`ActionOnly`, `Literal`, `From`, `To`, `Using`).
    pub binding: ScreenDataBinding,
}

/// Hierarchical `SCREEN SECTION` node (`Group` with nested children or `Elementary` leaf item).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenNode {
    /// Group screen entry (`01`..`48`) propagating colors/attributes and optional starting coordinates.
    Group {
        /// Optional group screen name (`None` for `FILLER`).
        name: Option<SymbolId>,
        /// Group-entry `LINE` clause.
        line: LineSpec,
        /// Group-entry `COLUMN` clause.
        col: ColSpec,
        /// Group-level visual/editing attributes and entry directives (`BLANK SCREEN`, `BELL`, etc.).
        attrs: ScreenAttributeSet,
        /// Ordered child group/elementary nodes.
        children: Vec<ScreenNode>,
    },
    /// Elementary screen item (`ActionOnly`, `Literal`, `From`, `To`, `Using`).
    Elementary(ScreenFieldSpec),
}

/// Flat COBOL level-numbered `SCREEN SECTION` entry (`01`..`49`) used to construct a [`ScreenNode`] hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelScreenEntry {
    /// COBOL hierarchy level (`01`..=`49`).
    pub level: LevelNumber,
    /// Optional screen item name (`None` for `FILLER`).
    pub name: Option<SymbolId>,
    /// `LINE` positioning clause.
    pub line: LineSpec,
    /// `COLUMN` positioning clause.
    pub col: ColSpec,
    /// Declared attributes on this entry.
    pub attrs: ScreenAttributeSet,
    /// Data binding (`None` when the entry may act as a parent group if followed by deeper levels).
    pub binding: ScreenDataBinding,
}

/// Coordinate-resolved elementary field inside a compiled [`ScreenLayout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScreenField {
    /// Optional screen item symbol.
    pub name: Option<SymbolId>,
    /// Resolved 1-based starting `(row, col)` (clamped to viewport dimensions).
    pub pos: CursorPosition,
    /// Visible cell width (`>= 1`).
    pub width: u16,
    /// Merged attribute set after group inheritance and child overrides.
    pub attrs: ScreenAttributeSet,
    /// Data-flow binding (`Literal`, `From`, `To`, `Using`).
    pub binding: ScreenDataBinding,
}

/// Single executable step in a compiled [`ScreenLayout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedScreenStep {
    /// One-shot clear, erase, or audible bell directive (`BLANK SCREEN`, `BLANK LINE`, `ERASE EOL/EOS`, `BELL`).
    Directive {
        /// Resolved cursor position when the directive executes.
        pos: CursorPosition,
        /// Directive flags (`GROUP_ENTRY_DIRECTIVES` subset).
        flags: ScreenAttrFlags,
        /// Background/visual cell style used for blanking cells.
        style: CellStyle,
    },
    /// Visible literal or data-bound field.
    Field(ResolvedScreenField),
}

/// Mutable layout-resolution cursor and occupancy tracker.
struct LayoutCursorState {
    /// Target viewport dimensions.
    dims: ScreenDimensions,
    /// Current 1-based row (`1..=dims.rows`).
    cur_row: u16,
    /// Current 1-based sequential column (`1..=dims.cols + 1`).
    next_col: u16,
    /// Whether any directive or field has already advanced or anchored screen output.
    started: bool,
    /// Per-cell occupancy bitmap (`dims.rows * dims.cols`) detecting `EC-SCREEN-FIELD-OVERLAP`.
    occupied: Vec<bool>,
    /// Accumulated `EC-SCREEN-*` boundary and overlap conditions.
    exceptions: Vec<ScreenExceptionKind>,
    /// Emitted resolved layout steps.
    steps: Vec<ResolvedScreenStep>,
}

impl LayoutCursorState {
    /// Creates a fresh [`LayoutCursorState`] at `(1, 1)` for `dims`.
    ///
    /// # Arguments
    ///
    /// * `dims` - Target terminal grid dimensions.
    fn new(dims: ScreenDimensions) -> Self {
        Self {
            dims,
            cur_row: 1,
            next_col: 1,
            started: false,
            occupied: vec![false; dims.cell_count()],
            exceptions: Vec::new(),
            steps: Vec::new(),
        }
    }

    /// Resolves `(line, col)` relative to the current sequential cursor, recording `EC-SCREEN-*`
    /// conditions when raw coordinates exceed `dims`.
    ///
    /// # Arguments
    ///
    /// * `line` - Vertical [`LineSpec`].
    /// * `col` - Horizontal [`ColSpec`].
    fn resolve_coords(&mut self, line: LineSpec, col: ColSpec) -> CursorPosition {
        let line_explicit = !matches!(line, LineSpec::Omitted);
        let raw_row = match line {
            LineSpec::Omitted => self.cur_row,
            LineSpec::Zero => {
                if self.started {
                    self.cur_row.saturating_add(1)
                } else {
                    1
                }
            }
            LineSpec::Absolute(r) => r.get(),
            LineSpec::RelativePlus(delta) => self.cur_row.saturating_add(delta),
            LineSpec::RelativeMinus(delta) => self.cur_row.saturating_sub(delta).max(1),
        };

        let max_r = self.dims.rows.get();
        let clamped_row = if raw_row > max_r {
            self.exceptions.push(ScreenExceptionKind::LineOutOfBounds {
                requested_row: raw_row,
                max_row: max_r,
            });
            max_r
        } else {
            raw_row
        };

        let base_col = if line_explicit { 1 } else { self.next_col };
        let raw_col = match col {
            ColSpec::Omitted => base_col,
            ColSpec::Zero => 1,
            ColSpec::Absolute(c) => c.get(),
            ColSpec::RelativePlus(delta) => base_col.saturating_add(delta),
            ColSpec::RelativeMinus(delta) => base_col.saturating_sub(delta).max(1),
        };

        let max_c = self.dims.cols.get();
        let clamped_col = if raw_col > max_c {
            self.exceptions
                .push(ScreenExceptionKind::StartingColumnOutOfBounds {
                    requested_col: raw_col,
                    max_col: max_c,
                });
            max_c
        } else {
            raw_col
        };

        self.cur_row = clamped_row;
        self.next_col = clamped_col;

        let row = ScreenRow::try_new(clamped_row).unwrap_or(ScreenRow::ONE);
        let col_val = ScreenCol::try_new(clamped_col).unwrap_or(ScreenCol::ONE);
        CursorPosition::new(row, col_val)
    }

    /// Marks cells `[pos.col .. pos.col + width)` on `pos.row` as occupied, recording
    /// [`ScreenExceptionKind::FieldOverlap`] or [`ScreenExceptionKind::StartingColumnOutOfBounds`] when exceeded.
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `width` - Field cell width (`>= 1`).
    fn mark_field_span(&mut self, pos: CursorPosition, width: u16) {
        let max_c = self.dims.cols.get();
        let end_exclusive = pos.col.get().saturating_add(width);
        if end_exclusive.saturating_sub(1) > max_c {
            self.exceptions
                .push(ScreenExceptionKind::StartingColumnOutOfBounds {
                    requested_col: end_exclusive.saturating_sub(1),
                    max_col: max_c,
                });
        }
        let cols_usize = self.dims.cols.get() as usize;
        let row_start = pos.row.zero_based_index().saturating_mul(cols_usize);
        let start_col_idx = pos.col.zero_based_index();
        let capped_end_col = (end_exclusive.saturating_sub(1).min(max_c)) as usize;
        let span_len = capped_end_col.saturating_sub(start_col_idx);
        let mut reported_overlap = false;
        for (offset, slot) in self
            .occupied
            .iter_mut()
            .skip(row_start.saturating_add(start_col_idx))
            .take(span_len)
            .enumerate()
        {
            if *slot && !reported_overlap {
                let c_idx = start_col_idx.saturating_add(offset);
                let overlap_col = u16::try_from(c_idx.saturating_add(1)).unwrap_or(1);
                let col_typed = ScreenCol::try_new(overlap_col).unwrap_or(ScreenCol::ONE);
                self.exceptions.push(ScreenExceptionKind::FieldOverlap {
                    row: pos.row,
                    col: col_typed,
                });
                reported_overlap = true;
            }
            *slot = true;
        }
        self.next_col = end_exclusive;
        self.started = true;
    }

    /// Clears occupancy tracking when `BLANK SCREEN` or `BLANK LINE` resets prior screen content.
    ///
    /// # Arguments
    ///
    /// * `pos` - Directive [`CursorPosition`].
    /// * `flags` - Directive [`ScreenAttrFlags`].
    fn apply_directive_occupancy_reset(&mut self, pos: CursorPosition, flags: ScreenAttrFlags) {
        if flags.contains(ScreenAttrFlags::BLANK_SCREEN) {
            self.occupied.fill(false);
            self.cur_row = 1;
            self.next_col = 1;
        } else if flags.contains(ScreenAttrFlags::BLANK_LINE) {
            let cols_usize = self.dims.cols.get() as usize;
            let row_start = pos.row.zero_based_index().saturating_mul(cols_usize);
            for slot in self.occupied.iter_mut().skip(row_start).take(cols_usize) {
                *slot = false;
            }
        }
        self.started = true;
    }

    /// Recursively lowers `node` under `inherited_attrs`.
    ///
    /// # Arguments
    ///
    /// * `node` - [`ScreenNode`] subtree.
    /// * `inherited_attrs` - Parent group attribute set.
    fn lower_node(&mut self, node: &ScreenNode, inherited_attrs: &ScreenAttributeSet) {
        match node {
            ScreenNode::Group {
                line,
                col,
                attrs,
                children,
                ..
            } => {
                let merged = inherited_attrs.inherit_into_child(attrs);
                if !matches!(line, LineSpec::Omitted) || !matches!(col, ColSpec::Omitted) {
                    let _ = self.resolve_coords(*line, *col);
                }
                let entry_dirs = ScreenAttrFlags::from_bits_retain(
                    attrs.flags.bits() & ScreenAttrFlags::GROUP_ENTRY_DIRECTIVES.bits(),
                );
                if !entry_dirs.is_empty() {
                    let pos = CursorPosition::new(
                        ScreenRow::try_new(self.cur_row).unwrap_or(ScreenRow::ONE),
                        ScreenCol::try_new(self.next_col.min(self.dims.cols.get()))
                            .unwrap_or(ScreenCol::ONE),
                    );
                    self.steps.push(ResolvedScreenStep::Directive {
                        pos,
                        flags: entry_dirs,
                        style: merged.cell_style(),
                    });
                    self.apply_directive_occupancy_reset(pos, entry_dirs);
                }
                for child in children {
                    self.lower_node(child, &merged);
                }
            }
            ScreenNode::Elementary(spec) => {
                let merged = inherited_attrs.inherit_into_child(&spec.attrs);
                let pos = self.resolve_coords(spec.line, spec.col);
                let entry_dirs = ScreenAttrFlags::from_bits_retain(
                    spec.attrs.flags.bits() & ScreenAttrFlags::GROUP_ENTRY_DIRECTIVES.bits(),
                );
                if !entry_dirs.is_empty() {
                    self.steps.push(ResolvedScreenStep::Directive {
                        pos,
                        flags: entry_dirs,
                        style: merged.cell_style(),
                    });
                    self.apply_directive_occupancy_reset(pos, entry_dirs);
                }
                let width = spec.binding.effective_width();
                if width > 0 {
                    let field_pos = if entry_dirs.contains(ScreenAttrFlags::BLANK_SCREEN)
                        && matches!(spec.line, LineSpec::Omitted)
                        && matches!(spec.col, ColSpec::Omitted)
                    {
                        CursorPosition::HOME
                    } else {
                        pos
                    };
                    self.mark_field_span(field_pos, width);
                    self.steps
                        .push(ResolvedScreenStep::Field(ResolvedScreenField {
                            name: spec.name,
                            pos: field_pos,
                            width,
                            attrs: merged,
                            binding: spec.binding.clone(),
                        }));
                }
            }
        }
    }
}

/// Compiled, coordinate-resolved `SCREEN SECTION` layout ready for `DISPLAY` and `ACCEPT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenLayout {
    /// Target viewport dimensions against which coordinates were resolved.
    dims: ScreenDimensions,
    /// Ordered sequence of directives and fields.
    steps: Vec<ResolvedScreenStep>,
    /// Any `EC-SCREEN-*` boundary or field-overlap conditions detected during compilation.
    exceptions: Vec<ScreenExceptionKind>,
}

impl ScreenLayout {
    /// Compiles one or more root [`ScreenNode`] hierarchies against `dims`.
    ///
    /// # Arguments
    ///
    /// * `dims` - Target [`ScreenDimensions`] (e.g. [`ScreenDimensions::STANDARD_24X80`]).
    /// * `roots` - Root `SCREEN SECTION` nodes (`01`-level groups or elementary items).
    #[must_use]
    pub fn compile(dims: ScreenDimensions, roots: &[ScreenNode]) -> Self {
        let mut state = LayoutCursorState::new(dims);
        let root_attrs = ScreenAttributeSet::new();
        for node in roots {
            state.lower_node(node, &root_attrs);
        }
        Self {
            dims,
            steps: state.steps,
            exceptions: state.exceptions,
        }
    }

    /// Builds nested [`ScreenNode`] hierarchies from flat COBOL `01`..`49` [`LevelScreenEntry`] items
    /// and compiles them into a [`ScreenLayout`].
    ///
    /// Entries whose next sibling has a strictly greater numeric level number become [`ScreenNode::Group`]
    /// parents; leaf entries become [`ScreenNode::Elementary`].
    ///
    /// # Arguments
    ///
    /// * `dims` - Target [`ScreenDimensions`].
    /// * `entries` - Ordered `01`..`49` screen description entries.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if an entry uses level `66`, `77`, `78`, or `88`.
    pub fn from_level_entries(
        dims: ScreenDimensions,
        entries: &[LevelScreenEntry],
    ) -> Result<Self> {
        for entry in entries {
            if !matches!(entry.level, LevelNumber::Hierarchy(_)) {
                return Err(CoblangError::InvalidDomainValue {
                    domain_type: "LevelScreenEntry::level",
                    value: entry.level.as_u8().to_string(),
                    reason: String::from(
                        "SCREEN SECTION entries must use hierarchy levels 01..=49",
                    ),
                });
            }
        }
        let (roots, _) = build_hierarchy_slice(entries, 0, 0);
        Ok(Self::compile(dims, &roots))
    }

    /// Constructs a single-field [`ScreenLayout`] for extended `DISPLAY` / `ACCEPT ... AT LINE l COLUMN c`.
    ///
    /// # Arguments
    ///
    /// * `dims` - Target [`ScreenDimensions`].
    /// * `spec` - Elementary [`ScreenFieldSpec`] to place on screen.
    #[must_use]
    pub fn single_field(dims: ScreenDimensions, spec: ScreenFieldSpec) -> Self {
        Self::compile(dims, &[ScreenNode::Elementary(spec)])
    }

    /// Returns the [`ScreenDimensions`] used to compile this layout.
    #[must_use]
    pub const fn dimensions(&self) -> ScreenDimensions {
        self.dims
    }

    /// Returns all ordered [`ResolvedScreenStep`] items in this layout.
    #[must_use]
    pub fn steps(&self) -> &[ResolvedScreenStep] {
        &self.steps
    }

    /// Collects references to all visible [`ResolvedScreenField`] items in display order.
    #[must_use]
    pub fn fields(&self) -> Vec<&ResolvedScreenField> {
        let mut out = Vec::new();
        for step in &self.steps {
            if let ResolvedScreenStep::Field(f) = step {
                out.push(f);
            }
        }
        out
    }

    /// Collects references to all editable (`TO` / `USING`) [`ResolvedScreenField`] items in tab order.
    #[must_use]
    pub fn editable_fields(&self) -> Vec<&ResolvedScreenField> {
        let mut out = Vec::new();
        for step in &self.steps {
            if let ResolvedScreenStep::Field(f) = step
                && f.binding.is_editable()
            {
                out.push(f);
            }
        }
        out
    }

    /// Returns all `EC-SCREEN-*` boundary or overlap conditions detected during compilation.
    #[must_use]
    pub fn exceptions(&self) -> &[ScreenExceptionKind] {
        &self.exceptions
    }
}

/// Recursively assembles flat [`LevelScreenEntry`] items starting at `start_idx` whose level exceeds `min_level`.
///
/// # Arguments
///
/// * `entries` - Full slice of level-numbered entries.
/// * `start_idx` - Starting index within `entries`.
/// * `min_level` - Exclusive minimum parent level (`0` at root).
fn build_hierarchy_slice(
    entries: &[LevelScreenEntry],
    start_idx: usize,
    min_level: u8,
) -> (Vec<ScreenNode>, usize) {
    let mut nodes = Vec::new();
    let mut idx = start_idx;
    while let Some(cur) = entries.get(idx) {
        let cur_lvl = cur.level.as_u8();
        if cur_lvl <= min_level {
            break;
        }
        let next_is_child = entries
            .get(idx.saturating_add(1))
            .is_some_and(|nxt| nxt.level.as_u8() > cur_lvl);
        if next_is_child {
            let (children, next_idx) =
                build_hierarchy_slice(entries, idx.saturating_add(1), cur_lvl);
            nodes.push(ScreenNode::Group {
                name: cur.name,
                line: cur.line,
                col: cur.col,
                attrs: cur.attrs,
                children,
            });
            idx = next_idx;
        } else {
            nodes.push(ScreenNode::Elementary(ScreenFieldSpec {
                name: cur.name,
                line: cur.line,
                col: cur.col,
                attrs: cur.attrs,
                binding: cur.binding.clone(),
            }));
            idx = idx.saturating_add(1);
        }
    }
    (nodes, idx)
}
