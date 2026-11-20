//! Compiler warning groups and `-W*` / `-Werror` CLI flag state machine.

use std::collections::BTreeMap;

use coblang_common::{CoblangError, DiagnosticSeverity, Result};

/// Strongly-typed compiler warning group controlled by `-W<name>` / `-Wno-<name>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WarningGroup {
    /// Archaic COBOL language elements (`-Warchaic`).
    Archaic,
    /// Obsolete COBOL language elements (`-Wobsolete`).
    Obsolete,
    /// Incompatible or larger `REDEFINES` declarations (`-Wredefinition`).
    Redefinition,
    /// Potential literal or field truncation (`-Wtruncate`).
    Truncate,
    /// Strict category/type checking warnings (`-Wstrict-typing`).
    StrictTyping,
    /// Implicitly defined variables or special registers (`-Wimplicit-define`).
    ImplicitDefine,
    /// Ambiguous operator/condition parenthesization (`-Wparentheses`).
    Parentheses,
    /// Missing explicit `END-*` scope terminators (`-Wterminator`).
    Terminator,
    /// Non-numeric or out-of-order cols 1–6 sequence numbers (`-Wsequence`).
    Sequence,
    /// Non-blank text truncated past column 72 in fixed form (`-Wcolumn-overflow`).
    ColumnOverflow,
    /// Mismatched `CALL` argument count or size (`-Wcall-params`).
    CallParams,
    /// Non-standard vendor dialect extensions (`-Wdialect`).
    DialectExtension,
    /// Unreachable statements following `GO TO` / `STOP RUN` (`-Wunreachable`).
    Unreachable,
    /// Declared but unreferenced data items or paragraphs (`-Wunused`).
    Unused,
}

impl WarningGroup {
    /// Ordered slice of all warning groups.
    pub const ALL: &'static [Self] = &[
        Self::Archaic,
        Self::Obsolete,
        Self::Redefinition,
        Self::Truncate,
        Self::StrictTyping,
        Self::ImplicitDefine,
        Self::Parentheses,
        Self::Terminator,
        Self::Sequence,
        Self::ColumnOverflow,
        Self::CallParams,
        Self::DialectExtension,
        Self::Unreachable,
        Self::Unused,
    ];

    /// Subset of groups enabled by `-Wall`.
    pub const WALL_GROUPS: &'static [Self] = &[
        Self::Obsolete,
        Self::Redefinition,
        Self::Truncate,
        Self::ImplicitDefine,
        Self::Parentheses,
        Self::Terminator,
        Self::ColumnOverflow,
        Self::CallParams,
    ];

    /// Canonical CLI flag stem (following `-W` or `-Wno-`).
    #[must_use]
    pub const fn flag_name(self) -> &'static str {
        match self {
            Self::Archaic => "archaic",
            Self::Obsolete => "obsolete",
            Self::Redefinition => "redefinition",
            Self::Truncate => "truncate",
            Self::StrictTyping => "strict-typing",
            Self::ImplicitDefine => "implicit-define",
            Self::Parentheses => "parentheses",
            Self::Terminator => "terminator",
            Self::Sequence => "sequence",
            Self::ColumnOverflow => "column-overflow",
            Self::CallParams => "call-params",
            Self::DialectExtension => "dialect",
            Self::Unreachable => "unreachable",
            Self::Unused => "unused",
        }
    }

    /// Parses a [`WarningGroup`] from its CLI stem.
    ///
    /// # Arguments
    ///
    /// * `name` - Flag stem following `-W`, `-Wno-`, `-Werror=`, or `-Wno-error=`.
    #[must_use]
    pub fn from_flag_name(name: &str) -> Option<Self> {
        match name {
            "archaic" | "segment" => Some(Self::Archaic),
            "obsolete" => Some(Self::Obsolete),
            "redefinition" | "possible-overlap" => Some(Self::Redefinition),
            "truncate" | "truncation" | "possible-truncate" => Some(Self::Truncate),
            "strict-typing" | "typing" | "constant-expression" | "constant-numlit-expression" => {
                Some(Self::StrictTyping)
            }
            "implicit-define" | "linkage" => Some(Self::ImplicitDefine),
            "parentheses" => Some(Self::Parentheses),
            "terminator" => Some(Self::Terminator),
            "sequence" => Some(Self::Sequence),
            "column-overflow" => Some(Self::ColumnOverflow),
            "call-params" => Some(Self::CallParams),
            "dialect" | "additional" | "others" | "any-length" | "ibm-cdf" | "recording-mode"
            | "unfinished" | "pending" | "unsupported" | "ignored-error" => {
                Some(Self::DialectExtension)
            }
            "unreachable"
            | "goto-section"
            | "goto-different-section"
            | "suspicious-perform-thru" => Some(Self::Unreachable),
            "unused" => Some(Self::Unused),
            _ => None,
        }
    }
}

/// Per-group warning disposition inside [`WarningPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupLevel {
    /// Suppressed (`-w` or `-Wno-<group>`).
    Suppressed,
    /// Emitted as `warning:`.
    Warn,
    /// Promoted to `error:` (`-Werror` or `-Werror=<group>`).
    PromoteToError,
}

/// Stateful warning configuration updated left-to-right by CLI flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarningPolicy {
    /// Global `-w` inhibit-all-warnings state.
    inhibit_all: GroupStateToggle,
    /// Global `-Werror` promote-all-warnings state.
    global_werror: GroupStateToggle,
    /// Per-group enabled/promoted disposition map.
    levels: BTreeMap<WarningGroup, GroupLevel>,
}

/// Explicit two-state toggle avoiding raw boolean fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupStateToggle {
    /// Inactive.
    Inactive,
    /// Active.
    Active,
}

impl Default for WarningPolicy {
    /// Creates the default [`WarningPolicy`] (core safety warnings enabled, `-Werror` off).
    fn default() -> Self {
        Self::new()
    }
}

impl WarningPolicy {
    /// Creates the default [`WarningPolicy`].
    #[must_use]
    pub fn new() -> Self {
        let mut levels = BTreeMap::new();
        for &grp in WarningGroup::ALL {
            let default_level = match grp {
                WarningGroup::Obsolete
                | WarningGroup::Redefinition
                | WarningGroup::ColumnOverflow
                | WarningGroup::CallParams => GroupLevel::Warn,
                WarningGroup::Archaic
                | WarningGroup::Truncate
                | WarningGroup::StrictTyping
                | WarningGroup::ImplicitDefine
                | WarningGroup::Parentheses
                | WarningGroup::Terminator
                | WarningGroup::Sequence
                | WarningGroup::DialectExtension
                | WarningGroup::Unreachable
                | WarningGroup::Unused => GroupLevel::Suppressed,
            };
            levels.insert(grp, default_level);
        }
        Self {
            inhibit_all: GroupStateToggle::Inactive,
            global_werror: GroupStateToggle::Inactive,
            levels,
        }
    }

    /// Applies a single warning CLI flag (`-w`, `-Wall`, `-Wextra`, `-W*`, `-Wno-*`, `-Werror*`).
    ///
    /// # Arguments
    ///
    /// * `flag` - Raw CLI option token starting with `-w` or `-W`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidCli`] if `flag` names an unrecognized warning group.
    pub fn apply_cli_flag(&mut self, flag: &str) -> Result<()> {
        match flag {
            "-w" | "-Wno-all" => {
                self.inhibit_all = GroupStateToggle::Active;
                Ok(())
            }
            "-Wall" => {
                self.inhibit_all = GroupStateToggle::Inactive;
                for &grp in WarningGroup::WALL_GROUPS {
                    self.enable_group(grp);
                }
                Ok(())
            }
            "-Wextra" | "-W" => {
                self.inhibit_all = GroupStateToggle::Inactive;
                for &grp in WarningGroup::ALL {
                    self.enable_group(grp);
                }
                Ok(())
            }
            "-Werror" => {
                self.global_werror = GroupStateToggle::Active;
                Ok(())
            }
            "-Wfatal-errors" | "-Wno-fatal-errors" => Ok(()),
            "-Wno-error" => {
                self.global_werror = GroupStateToggle::Inactive;
                for val in self.levels.values_mut() {
                    if *val == GroupLevel::PromoteToError {
                        *val = GroupLevel::Warn;
                    }
                }
                Ok(())
            }
            _ => self.apply_named_group_flag(flag),
        }
    }

    /// Parses and applies `-Werror=<grp>`, `-Wno-error=<grp>`, `-Wno-<grp>`, or `-W<grp>`.
    ///
    /// # Arguments
    ///
    /// * `flag` - Raw CLI flag string.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidCli`] if `flag` is not a recognized warning option.
    fn apply_named_group_flag(&mut self, flag: &str) -> Result<()> {
        if let Some(name) = flag.strip_prefix("-Werror=") {
            let grp = parse_group_or_err(name, flag)?;
            self.inhibit_all = GroupStateToggle::Inactive;
            self.levels.insert(grp, GroupLevel::PromoteToError);
            return Ok(());
        }
        if let Some(name) = flag.strip_prefix("-Wno-error=") {
            let grp = parse_group_or_err(name, flag)?;
            if self.levels.get(&grp) == Some(&GroupLevel::PromoteToError) {
                self.levels.insert(grp, GroupLevel::Warn);
            }
            return Ok(());
        }
        if let Some(name) = flag.strip_prefix("-Wno-") {
            let grp = parse_group_or_err(name, flag)?;
            self.levels.insert(grp, GroupLevel::Suppressed);
            return Ok(());
        }
        if let Some(name) = flag.strip_prefix("-W") {
            let grp = parse_group_or_err(name, flag)?;
            self.inhibit_all = GroupStateToggle::Inactive;
            self.enable_group(grp);
            return Ok(());
        }
        Err(CoblangError::InvalidCli {
            message: format!("unrecognized warning flag `{flag}`"),
        })
    }

    /// Marks `group` as active (preserving `PromoteToError` if already promoted).
    ///
    /// # Arguments
    ///
    /// * `group` - Warning group to enable.
    fn enable_group(&mut self, group: WarningGroup) {
        if self.levels.get(&group) != Some(&GroupLevel::PromoteToError) {
            self.levels.insert(group, GroupLevel::Warn);
        }
    }

    /// Resolves the effective [`DiagnosticSeverity`] for a warning belonging to `group`.
    ///
    /// Returns `None` when the warning is suppressed by `-w` or `-Wno-<group>`.
    ///
    /// # Arguments
    ///
    /// * `group` - Warning group to classify.
    #[must_use]
    pub fn classify(&self, group: WarningGroup) -> Option<DiagnosticSeverity> {
        if self.inhibit_all == GroupStateToggle::Active {
            return None;
        }
        match self
            .levels
            .get(&group)
            .copied()
            .unwrap_or(GroupLevel::Suppressed)
        {
            GroupLevel::Suppressed => None,
            GroupLevel::PromoteToError => Some(DiagnosticSeverity::Error),
            GroupLevel::Warn => {
                if self.global_werror == GroupStateToggle::Active {
                    Some(DiagnosticSeverity::Error)
                } else {
                    Some(DiagnosticSeverity::Warning)
                }
            }
        }
    }
}

/// Resolves `name` to a [`WarningGroup`] or constructs [`CoblangError::InvalidCli`].
///
/// # Arguments
///
/// * `name` - Group stem string.
/// * `full_flag` - Full CLI token for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] when `name` is unknown.
fn parse_group_or_err(name: &str, full_flag: &str) -> Result<WarningGroup> {
    WarningGroup::from_flag_name(name).ok_or_else(|| CoblangError::InvalidCli {
        message: format!("unknown warning option `{full_flag}`"),
    })
}
