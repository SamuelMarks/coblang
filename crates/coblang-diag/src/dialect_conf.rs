//! Strongly-typed COBOL dialect policies and clean-room `.conf` configuration loader.

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{CoblangError, Dialect, Result};

/// Single-quote character (`0x27`).
const SQ: char = '\u{0027}';

/// Tab-separated `include` directive prefix.
const INCLUDE_TAB: &str = "include\u{0009}";
/// Tab-separated `includeif` optional directive prefix.
const INCLUDEIF_TAB: &str = "includeif\u{0009}";

/// Endianness of `USAGE BINARY` / `COMP` / `COMP-4` storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryByteOrder {
    /// Host CPU native byte order (`native`).
    Native,
    /// Network / mainframe big-endian byte order (`big-endian`).
    BigEndian,
}

/// Storage byte-width allocation policy for `USAGE BINARY` (`binary-size`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinarySizePolicy {
    /// Allocate 1, 2, 4, or 8 bytes according to digit capacity (`1-2-4-8`).
    OneTwoFourEight,
    /// Traditional IBM S/390 allocation of 2, 4, or 8 bytes (`2-4-8`).
    TwoFourEight,
    /// Exact minimal byte count 1..=8 (`1--8`).
    OneToEight,
}

/// High-order decimal truncation policy for `USAGE BINARY` targets (`binary-truncate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TruncationMode {
    /// Truncate to `PICTURE` decimal power of ten (`yes`).
    TruncateToPic,
    /// Allow full native binary bit width (`no` / `-fnotrunc`).
    NoTrunc,
}

/// Numeric `DISPLAY` formatting convention (`pretty-display`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayFormatMode {
    /// Strict ISO raw zoned/numeric formatting (`no`).
    IsoStandard,
    /// `GnuCOBOL` human-friendly sign/decimal formatting (`yes`).
    GnuPretty,
}

/// Zoned decimal embedded overpunch sign table (`-fsign=ascii` vs `-fsign=ebcdic`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplaySignMode {
    /// `GnuCOBOL` ASCII sign table (`'0'..'9'` positive, `'p'..'y'` negative).
    Ascii,
    /// COBOL-85 / IBM EBCDIC-mapped overpunch table (`'{'`,`'A'..'I'` positive, `'}'`,`'J'..'R'` negative).
    Ebcdic,
}

/// Legality of complex `OCCURS DEPENDING ON` layouts (`complex-odo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OdoComplexity {
    /// Standard ISO trailing-only simple `ODO` (`no`).
    SimpleIsoOnly,
    /// Variably-located / nested `ODO` items permitted (`yes`).
    ComplexAllowed,
}

/// Variably-located field offset sliding policy (`odoslide`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OdoSlidePolicy {
    /// Keep subordinate fields at their maximum static offsets (`no`).
    FixedMaxOffsets,
    /// Slide siblings following an `OCCURS DEPENDING ON` table (`yes` / `ibm`).
    SlideVariablyLocated,
}

/// Default program collating sequence when `OBJECT-COMPUTER` omits `PROGRAM COLLATING SEQUENCE` (`default-colseq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefaultColSeqMode {
    /// Host ASCII / native byte order (`ascii` / `native`).
    AsciiNative,
    /// EBCDIC collation order (`ebcdic`).
    Ebcdic,
}

/// Runtime fatal bounds-check toggle for `EC-BOUND-*` categories (`-debug` / `-fec=` / `-fno-ec=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoundCheckToggle {
    /// Bounds check disabled by default unless enabled inline via `>>TURN` / `$SET`.
    Disabled,
    /// Bounds check enabled with `libcob`-compatible diagnostic abort (`exit 1`).
    EnabledFatal,
}

/// Initial per-category `EC-BOUND-*` state derived from CLI flags (`-debug`, `-fec=`, `-fno-ec=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InitialBoundsChecks {
    /// Subscript range checking (`EC-BOUND-SUBSCRIPT`).
    pub subscript: BoundCheckToggle,
    /// `OCCURS DEPENDING ON` object range checking (`EC-BOUND-ODO`).
    pub odo: BoundCheckToggle,
    /// Reference-modification offset/length checking (`EC-BOUND-REF-MOD`).
    pub refmod: BoundCheckToggle,
}

impl Default for InitialBoundsChecks {
    /// Returns all bounds checks disabled by default.
    fn default() -> Self {
        Self {
            subscript: BoundCheckToggle::Disabled,
            odo: BoundCheckToggle::Disabled,
            refmod: BoundCheckToggle::Disabled,
        }
    }
}

impl InitialBoundsChecks {
    /// Applies a CLI `-fec=<spec>` or `-fno-ec=<spec>` exception selector.
    ///
    /// # Arguments
    ///
    /// * `toggle` - Target [`BoundCheckToggle`] state (`EnabledFatal` or `Disabled`).
    /// * `raw_spec` - Exception name selector (e.g. `"ALL"`, `"BOUND"`, `"EC-BOUND-SUBSCRIPT"`).
    pub fn apply_ec_cli_flag(&mut self, toggle: BoundCheckToggle, raw_spec: &str) {
        let lower = raw_spec.trim().to_ascii_lowercase();
        let norm = lower.strip_prefix("ec-").unwrap_or(&lower);
        match norm {
            "all" | "bound" => {
                self.subscript = toggle;
                self.odo = toggle;
                self.refmod = toggle;
            }
            "bound-subscript" => self.subscript = toggle,
            "bound-odo" => self.odo = toggle,
            "bound-ref-mod" => self.refmod = toggle,
            _ => {}
        }
    }
}

/// Legality of `REDEFINES` targeting another `REDEFINES` entry (`indirect-redefines`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RedefinesChainPolicy {
    /// Must directly reference the original non-redefining entry (`no`).
    DirectOnly,
    /// Indirect chained `REDEFINES` permitted (`yes`).
    IndirectAllowed,
}

/// Parser strictness for minor vendor syntax relaxations (`relax-syntax-checks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelaxedSyntaxMode {
    /// Standard grammar enforcement.
    Strict,
    /// Relaxed vendor compatibility grammar (`-frelax-syntax-checks`).
    Relaxed,
}

/// `LINKAGE SECTION` parameter stickiness (`sticky-linkage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StickyLinkageMode {
    /// Standard positional `USING` binding only (`no`).
    Disabled,
    /// Retain prior `ENTRY` linkage bindings when omitted (`yes`).
    Sticky,
}

/// Out-of-line `PERFORM` frame return discipline (`perform-osvs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PerformReturnStyle {
    /// Standard ISO stack-disciplined `PERFORM` return (`no`).
    IsoStack,
    /// IBM OS/VS continuation-slot `PERFORM` semantics (`yes`).
    OsVsContinuation,
}

/// Default `ASSIGN TO` name resolution convention (`assign-clause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignClauseDefault {
    /// Resolve unquoted names as dynamic variables or literals (`dynamic`).
    Dynamic,
    /// Resolve via environment variable mapping (`external` / `ibm`).
    External,
    /// Micro Focus `ASSIGN` resolution (`mf`).
    MicroFocus,
}

/// Per-feature dialect support disposition in `.conf` files (`ok`, `warning`, `error`, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureDisposition {
    /// Supported silently (`ok` / `yes`).
    Ok,
    /// Supported with a general warning (`warning`).
    Warning,
    /// Supported with an `-Warchaic` warning (`archaic`).
    Archaic,
    /// Supported with an `-Wobsolete` warning (`obsolete`).
    Obsolete,
    /// Parsed and ignored without effect (`skip` / `ignore`).
    Ignore,
    /// Rejected as a syntax/semantic error (`error` / `no`).
    Error,
    /// Rejected as non-conforming under the active standard (`unconformable`).
    Unconformable,
}

impl FeatureDisposition {
    /// Parses a `.conf` disposition value keyword.
    ///
    /// # Arguments
    ///
    /// * `val` - Trimmed lowercase configuration value string.
    #[must_use]
    pub fn parse_value(val: &str) -> Option<Self> {
        match val {
            "ok" | "yes" | "true" => Some(Self::Ok),
            "warning" => Some(Self::Warning),
            "archaic" => Some(Self::Archaic),
            "obsolete" => Some(Self::Obsolete),
            "skip" | "ignore" => Some(Self::Ignore),
            "error" | "no" | "false" => Some(Self::Error),
            "unconformable" => Some(Self::Unconformable),
            _ => None,
        }
    }
}

/// Policy controlling whether `DECIMAL-POINT IS COMMA` applies inside `XML`/`JSON` output (`dpc-in-data`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DpcInDataMode {
    /// Emit `.` for both `XML GENERATE` and `JSON GENERATE` (`none`).
    #[default]
    None,
    /// Emit `,` inside `XML GENERATE` when `DECIMAL-POINT IS COMMA` (`xml`).
    Xml,
    /// Emit `,` inside `JSON GENERATE` when `DECIMAL-POINT IS COMMA` (`json`).
    Json,
    /// Emit `,` inside both `XML GENERATE` and `JSON GENERATE` when `DECIMAL-POINT IS COMMA` (`all`).
    All,
}

impl DpcInDataMode {
    /// Parses a case-insensitive `dpc-in-data` token (`"none"`, `"xml"`, `"json"`, `"all"`).
    ///
    /// # Arguments
    ///
    /// * `raw` - Trimmed option value.
    #[must_use]
    pub fn parse_token(raw: &str) -> Option<Self> {
        let lower = raw
            .trim()
            .trim_matches('"')
            .trim_matches(SQ)
            .to_ascii_lowercase();
        match lower.as_str() {
            "none" | "no" | "false" => Some(Self::None),
            "xml" => Some(Self::Xml),
            "json" => Some(Self::Json),
            "all" | "yes" | "true" => Some(Self::All),
            _ => None,
        }
    }

    /// Returns `true` if `XML GENERATE` should format decimal points with `,` under `DECIMAL-POINT IS COMMA`.
    #[must_use]
    pub const fn applies_to_xml(self) -> bool {
        matches!(self, Self::Xml | Self::All)
    }

    /// Returns `true` if `JSON GENERATE` should format decimal points with `,` under `DECIMAL-POINT IS COMMA`.
    #[must_use]
    pub const fn applies_to_json(self) -> bool {
        matches!(self, Self::Json | Self::All)
    }
}

/// Policy controlling `XML PARSE` event stream compatibility (`xml-parse-xmlss`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum XmlParseMode {
    /// Modern XML System Services mode (`-fxml-parse-xmlss`, emits `END-OF-INPUT` & namespace events).
    #[default]
    Xmlss,
    /// Legacy compatibility mode (`-fno-xml-parse-xmlss`).
    Compat,
}

/// CLI/configuration policy importing intrinsic functions without the `FUNCTION` keyword (`-fintrinsics=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CliIntrinsicImportMode {
    /// Only intrinsics declared in `REPOSITORY` may be invoked without `FUNCTION`.
    #[default]
    None,
    /// Every enabled intrinsic may be invoked without `FUNCTION` (`-fintrinsics=ALL`).
    All,
    /// Specific intrinsics listed in `DialectConfig::bare_intrinsics_set` may omit `FUNCTION`.
    Listed,
}

/// Vendor compatibility rule set governing `SCREEN SECTION` clauses (`-fscreen-section-rules=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScreenSectionRules {
    /// `GnuCOBOL` superset (`gc`).
    #[default]
    Gc,
    /// Strict ISO/IEC 1989:2002+ `SCREEN SECTION` (`std`).
    Std,
    /// ACUCOBOL-GT `SCREEN SECTION` (`acu`).
    Acu,
    /// Micro Focus `SCREEN SECTION` (`mf`).
    Mf,
    /// RM/COBOL `SCREEN SECTION` (`rm`).
    Rm,
    /// X/Open `SCREEN SECTION` (`xopen`).
    Xopen,
}

impl ScreenSectionRules {
    /// Parses a case-insensitive `-fscreen-section-rules=` token.
    ///
    /// # Arguments
    ///
    /// * `raw` - Trimmed option value.
    #[must_use]
    pub fn parse_token(raw: &str) -> Option<Self> {
        let lower = raw
            .trim()
            .trim_matches('"')
            .trim_matches(SQ)
            .to_ascii_lowercase();
        match lower.as_str() {
            "gc" | "gnu" | "gnucobol" => Some(Self::Gc),
            "std" | "standard" | "iso" => Some(Self::Std),
            "acu" | "acucobol" => Some(Self::Acu),
            "mf" | "microfocus" => Some(Self::Mf),
            "rm" | "rmcobol" => Some(Self::Rm),
            "xopen" => Some(Self::Xopen),
            _ => None,
        }
    }
}

/// Severity policy for conflicting `SCREEN` / extended `DISPLAY` / `ACCEPT` clauses (`-frelax-syntax-checks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScreenConflictPolicy {
    /// Emit errors on conflicting clauses (default).
    #[default]
    StrictError,
    /// Downgrade conflicting screen clauses to warnings (`-frelax-syntax-checks`).
    RelaxedWarning,
}

/// Complete strongly-typed compiler dialect configuration profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialectConfig {
    /// Base [`Dialect`] preset.
    pub base_dialect: Dialect,
    /// Human-readable configuration profile name (`name:` in `.conf`).
    pub profile_name: String,
    /// Tab stop width in fixed/free source normalization (`tab-width`, default `8`).
    pub tab_width: u8,
    /// Rightmost code column in fixed format (`text-column`, default `72`).
    pub text_column: u16,
    /// Maximum identifier length (`word-length`, `31` in COBOL-85/IBM, `63` in ISO/Gnu).
    pub max_word_length: u16,
    /// Maximum numeric `PICTURE` digit positions (`18` in strict COBOL-85, `38` otherwise).
    pub max_pic_digits: u8,
    /// `USAGE BINARY` byte order.
    pub binary_byteorder: BinaryByteOrder,
    /// `USAGE BINARY` storage width policy.
    pub binary_size: BinarySizePolicy,
    /// `USAGE BINARY` decimal truncation mode.
    pub binary_truncate: TruncationMode,
    /// Numeric `DISPLAY` formatting mode.
    pub pretty_display: DisplayFormatMode,
    /// Zoned decimal embedded overpunch sign convention (`-fsign=ascii|ebcdic`).
    pub display_sign: DisplaySignMode,
    /// Complex `OCCURS DEPENDING ON` policy.
    pub complex_odo: OdoComplexity,
    /// Variably-located sibling `ODOSLIDE` policy (`odoslide`).
    pub odo_slide: OdoSlidePolicy,
    /// Default non-numeric collating sequence (`-fdefault-colseq=`).
    pub default_colseq: DefaultColSeqMode,
    /// Runtime internal `USAGE DISPLAY` character set (`Ascii` vs `-finternal-ebcdic`).
    pub internal_charset: crate::ebcdic_table::InternalCharsetMode,
    /// Active paired 256-byte EBCDIC<->ASCII translation tables (`-febcdic-table=`).
    pub ebcdic_tables: crate::ebcdic_table::EbcdicTranslationTables,
    /// Initial `EC-BOUND-*` runtime checking toggles (`-debug` / `-fec=` / `-fno-ec=`).
    pub bounds_checks: InitialBoundsChecks,
    /// Chained `REDEFINES` policy.
    pub indirect_redefines: RedefinesChainPolicy,
    /// Disposition of `REDEFINES` entries larger than the redefined target (`larger-redefines`).
    pub larger_redefines: FeatureDisposition,
    /// Relaxed syntax check mode.
    pub relaxed_syntax: RelaxedSyntaxMode,
    /// Sticky linkage mode.
    pub sticky_linkage: StickyLinkageMode,
    /// Out-of-line `PERFORM` continuation style.
    pub perform_style: PerformReturnStyle,
    /// Default `ASSIGN` mapping convention.
    pub assign_clause: AssignClauseDefault,
    /// Optional `-fdefaultbyte` / `defaultbyte:` fill byte (`0..=255`) for uninitialized storage and `ALLOCATE`.
    pub default_byte: Option<u8>,
    /// `DECIMAL-POINT IS COMMA` policy inside `XML`/`JSON` generation (`-fdpc-in-data=`).
    pub dpc_in_data: DpcInDataMode,
    /// `XML PARSE` event stream compatibility mode (`-fxml-parse-xmlss` / `-fno-xml-parse-xmlss`).
    pub xml_parse_mode: XmlParseMode,
    /// CLI bare intrinsic import mode (`-fintrinsics=ALL` or `-fintrinsics=name1,name2`).
    pub cli_intrinsics_mode: CliIntrinsicImportMode,
    /// Uppercase intrinsic names imported without `FUNCTION` when `cli_intrinsics_mode == Listed`.
    pub bare_intrinsics_set: BTreeSet<String>,
    /// Uppercase intrinsic names explicitly disabled via `-fnot-intrinsic=` or `.conf`.
    pub disabled_intrinsics: BTreeSet<String>,
    /// Active `SCREEN SECTION` vendor compatibility rule set (`-fscreen-section-rules=`).
    pub screen_section_rules: ScreenSectionRules,
    /// Severity policy for conflicting `SCREEN` clauses (`-frelax-syntax-checks`).
    pub screen_conflict_policy: ScreenConflictPolicy,
    /// Optional dialect-enabled special registers (`-fregister=LIN,COL`).
    pub custom_registers: BTreeSet<String>,
    /// Uppercase reserved words active in this dialect.
    pub reserved_words: BTreeSet<String>,
    /// Additional `.conf` feature dispositions keyed by canonical `.conf` option name.
    pub feature_rules: BTreeMap<String, FeatureDisposition>,
}

impl DialectConfig {
    /// Maps a built-in standard `.conf` filename (e.g. `"cobol2014.conf"`) to its [`Dialect`].
    ///
    /// # Arguments
    ///
    /// * `filename` - Basename or relative path passed to `-conf=` or `include`.
    #[must_use]
    pub fn builtin_from_conf_filename(filename: &str) -> Option<Dialect> {
        match filename {
            "default.conf" | "lax.conf-inc" => Some(Dialect::Default),
            "cobol85.conf" => Some(Dialect::Cobol85),
            "cobol2002.conf" => Some(Dialect::Cobol2002),
            "cobol2014.conf" | "strict.conf" => Some(Dialect::Cobol2014),
            "cobol2023.conf" => Some(Dialect::Cobol2023),
            "gnu.conf" => Some(Dialect::Gnu),
            "ibm.conf" | "ibm-strict.conf" | "mvs.conf" | "mvs-strict.conf" => Some(Dialect::Ibm),
            "mf.conf" | "mf-strict.conf" => Some(Dialect::MicroFocus),
            "bs2000.conf" | "bs2000-strict.conf" => Some(Dialect::Bs2000),
            "acu.conf" | "acu-strict.conf" => Some(Dialect::Acu),
            "xopen.conf" => Some(Dialect::XOpen),
            _ => None,
        }
    }

    /// Constructs the built-in [`DialectConfig`] preset for `dialect`.
    ///
    /// # Arguments
    ///
    /// * `dialect` - Target COBOL [`Dialect`] preset.
    #[must_use]
    pub fn for_dialect(dialect: Dialect) -> Self {
        let mut cfg = Self {
            base_dialect: dialect,
            profile_name: String::from(dialect.as_str()),
            tab_width: 8,
            text_column: 72,
            max_word_length: 63,
            max_pic_digits: 38,
            binary_byteorder: BinaryByteOrder::BigEndian,
            binary_size: BinarySizePolicy::OneTwoFourEight,
            binary_truncate: TruncationMode::TruncateToPic,
            pretty_display: DisplayFormatMode::GnuPretty,
            display_sign: DisplaySignMode::Ascii,
            complex_odo: OdoComplexity::SimpleIsoOnly,
            odo_slide: OdoSlidePolicy::FixedMaxOffsets,
            default_colseq: DefaultColSeqMode::AsciiNative,
            internal_charset: crate::ebcdic_table::InternalCharsetMode::Ascii,
            ebcdic_tables: crate::ebcdic_table::EbcdicTranslationTables::default(),
            bounds_checks: InitialBoundsChecks::default(),
            indirect_redefines: RedefinesChainPolicy::DirectOnly,
            larger_redefines: FeatureDisposition::Error,
            relaxed_syntax: RelaxedSyntaxMode::Strict,
            sticky_linkage: StickyLinkageMode::Disabled,
            perform_style: PerformReturnStyle::IsoStack,
            assign_clause: AssignClauseDefault::Dynamic,
            default_byte: None,
            dpc_in_data: DpcInDataMode::None,
            xml_parse_mode: XmlParseMode::Xmlss,
            cli_intrinsics_mode: CliIntrinsicImportMode::None,
            bare_intrinsics_set: BTreeSet::new(),
            disabled_intrinsics: BTreeSet::new(),
            screen_section_rules: ScreenSectionRules::Gc,
            screen_conflict_policy: ScreenConflictPolicy::StrictError,
            custom_registers: BTreeSet::new(),
            reserved_words: default_core_reserved_words(),
            feature_rules: BTreeMap::new(),
        };
        apply_dialect_overrides(&mut cfg, dialect);
        cfg
    }

    /// Loads and applies a `.conf` configuration text buffer, resolving `include "..."` directives.
    ///
    /// # Arguments
    ///
    /// * `text` - Raw `.conf` file text.
    /// * `path` - Logical or physical path of `text` (used for cycle detection and diagnostics).
    /// * `include_loader` - Callback returning the text of an `include "name"` target.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ConfigLoad`] on syntax errors, unknown values, or `include` cycles.
    pub fn load_conf_text(
        &mut self,
        text: &str,
        path: &str,
        include_loader: &mut dyn FnMut(&str) -> Result<String>,
    ) -> Result<()> {
        let mut active_stack = BTreeSet::new();
        self.load_conf_inner(text, path, include_loader, &mut active_stack)
    }

    /// Recursive `.conf` parser carrying `active_stack` to detect circular `include` directives.
    ///
    /// # Arguments
    ///
    /// * `text` - Raw `.conf` text.
    /// * `path` - File identifier for `text`.
    /// * `include_loader` - Callback resolving included `.conf` files.
    /// * `active_stack` - Set of currently open `.conf` paths.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ConfigLoad`] on circular includes or malformed entries.
    fn load_conf_inner(
        &mut self,
        text: &str,
        path: &str,
        include_loader: &mut dyn FnMut(&str) -> Result<String>,
        active_stack: &mut BTreeSet<String>,
    ) -> Result<()> {
        if !active_stack.insert(String::from(path)) {
            return Err(CoblangError::ConfigLoad {
                path: String::from(path),
                line: 1,
                message: format!("recursive configuration include of `{path}`"),
            });
        }

        for (idx, raw_line) in text.lines().enumerate() {
            let line_no = idx + 1;
            let trimmed = strip_conf_comment(raw_line).trim();
            if trimmed.is_empty() {
                continue;
            }
            let inc_parsed = if let Some(r) = trimmed
                .strip_prefix("includeif ")
                .or_else(|| trimmed.strip_prefix(INCLUDEIF_TAB))
            {
                Some((true, r.trim()))
            } else if let Some(r) = trimmed
                .strip_prefix("include ")
                .or_else(|| trimmed.strip_prefix(INCLUDE_TAB))
            {
                match r.trim_start().strip_prefix("if-exists") {
                    Some(after) => Some((true, after.trim())),
                    None => Some((false, r.trim())),
                }
            } else {
                None
            };
            if let Some((optional, raw_target)) = inc_parsed {
                let target = raw_target.trim_matches('"').trim_matches(SQ);
                if target.is_empty() {
                    return Err(CoblangError::ConfigLoad {
                        path: String::from(path),
                        line: line_no,
                        message: String::from("empty `include` target in configuration"),
                    });
                }
                match include_loader(target) {
                    Ok(child_text) => {
                        self.load_conf_inner(&child_text, target, include_loader, active_stack)?;
                    }
                    Err(err) => {
                        if !optional {
                            return Err(err);
                        }
                    }
                }
                continue;
            }
            let Some((raw_key, raw_val)) = trimmed.split_once(':') else {
                return Err(CoblangError::ConfigLoad {
                    path: String::from(path),
                    line: line_no,
                    message: format!("expected `key: value` entry, found `{trimmed}`"),
                });
            };
            self.apply_conf_key_value(raw_key.trim(), raw_val.trim(), path, line_no)?;
        }

        active_stack.remove(path);
        Ok(())
    }

    /// Applies a single `key: value` configuration rule or `-cb_conf=key=value` CLI override.
    ///
    /// # Arguments
    ///
    /// * `key` - Configuration key name.
    /// * `value` - Configuration value text.
    /// * `source_path` - File path or `"<cli>"` for error reporting.
    /// * `line` - 1-based line number inside `source_path`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ConfigLoad`] if `key` has an invalid typed value.
    pub fn apply_conf_key_value(
        &mut self,
        key: &str,
        value: &str,
        source_path: &str,
        line: usize,
    ) -> Result<()> {
        let lower_val = value
            .trim_matches('"')
            .trim_matches(SQ)
            .to_ascii_lowercase();
        match key {
            "name" => {
                self.profile_name = String::from(value.trim_matches('"').trim_matches(SQ));
                Ok(())
            }
            "tab-width" => {
                let parsed = lower_val
                    .parse::<u8>()
                    .map_err(|_| conf_err(source_path, line, key, value))?;
                if parsed == 0 {
                    return Err(conf_err(source_path, line, key, value));
                }
                self.tab_width = parsed;
                Ok(())
            }
            "text-column" => {
                let parsed = lower_val
                    .parse::<u16>()
                    .map_err(|_| conf_err(source_path, line, key, value))?;
                if parsed < 12 {
                    return Err(conf_err(source_path, line, key, value));
                }
                self.text_column = parsed;
                Ok(())
            }
            "word-length" => {
                let parsed = lower_val
                    .parse::<u16>()
                    .map_err(|_| conf_err(source_path, line, key, value))?;
                if parsed == 0 {
                    return Err(conf_err(source_path, line, key, value));
                }
                self.max_word_length = parsed;
                Ok(())
            }
            "pic-length" | "numeric-literal-length" => {
                let parsed = lower_val
                    .parse::<u8>()
                    .map_err(|_| conf_err(source_path, line, key, value))?;
                if parsed == 0 {
                    return Err(conf_err(source_path, line, key, value));
                }
                self.max_pic_digits = parsed;
                Ok(())
            }
            "reserved" => {
                let word = value
                    .split('=')
                    .next()
                    .unwrap_or(value)
                    .trim()
                    .to_ascii_uppercase();
                let clean = word.trim_end_matches('*');
                if !clean.is_empty() {
                    self.reserved_words.insert(String::from(clean));
                }
                Ok(())
            }
            "not-reserved" => {
                self.reserved_words
                    .remove(&value.trim().to_ascii_uppercase());
                Ok(())
            }
            _ => self.apply_policy_or_feature_key(key, &lower_val, value, source_path, line),
        }
    }

    /// Applies strongly-typed enum keys (`binary-*`, `assign-clause`, etc.) or generic feature rules.
    ///
    /// # Arguments
    ///
    /// * `key` - Configuration key name.
    /// * `lower_val` - Lowercased unquoted value.
    /// * `raw_val` - Original raw value for diagnostics.
    /// * `source_path` - Source file path.
    /// * `line` - 1-based line number.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ConfigLoad`] if the value is invalid for `key`.
    fn apply_policy_or_feature_key(
        &mut self,
        key: &str,
        lower_val: &str,
        raw_val: &str,
        source_path: &str,
        line: usize,
    ) -> Result<()> {
        match key {
            "binary-byteorder" => match lower_val {
                "native" => self.binary_byteorder = BinaryByteOrder::Native,
                "big-endian" => self.binary_byteorder = BinaryByteOrder::BigEndian,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "binary-size" => match lower_val {
                "1-2-4-8" => self.binary_size = BinarySizePolicy::OneTwoFourEight,
                "2-4-8" => self.binary_size = BinarySizePolicy::TwoFourEight,
                "1--8" => self.binary_size = BinarySizePolicy::OneToEight,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "binary-truncate" => match lower_val {
                "yes" | "true" => self.binary_truncate = TruncationMode::TruncateToPic,
                "no" | "false" => self.binary_truncate = TruncationMode::NoTrunc,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "pretty-display" => match lower_val {
                "yes" | "true" => self.pretty_display = DisplayFormatMode::GnuPretty,
                "no" | "false" => self.pretty_display = DisplayFormatMode::IsoStandard,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "sign-ebcdic" | "display-sign" => match lower_val {
                "yes" | "true" | "ebcdic" => self.display_sign = DisplaySignMode::Ebcdic,
                "no" | "false" | "ascii" => self.display_sign = DisplaySignMode::Ascii,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "complex-odo" => match lower_val {
                "yes" | "true" => self.complex_odo = OdoComplexity::ComplexAllowed,
                "no" | "false" => self.complex_odo = OdoComplexity::SimpleIsoOnly,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "odoslide" => match lower_val {
                "yes" | "true" => self.odo_slide = OdoSlidePolicy::SlideVariablyLocated,
                "no" | "false" => self.odo_slide = OdoSlidePolicy::FixedMaxOffsets,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "default-colseq" => match lower_val {
                "ebcdic" => self.default_colseq = DefaultColSeqMode::Ebcdic,
                "ascii" | "native" => self.default_colseq = DefaultColSeqMode::AsciiNative,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "internal-ebcdic" => match lower_val {
                "yes" | "true" => {
                    self.internal_charset = crate::ebcdic_table::InternalCharsetMode::Ebcdic;
                    self.display_sign = DisplaySignMode::Ebcdic;
                }
                "no" | "false" => {
                    self.internal_charset = crate::ebcdic_table::InternalCharsetMode::Ascii;
                }
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "ebcdic-table" => {
                let tbl =
                    crate::ebcdic_table::EbcdicTranslationTables::from_builtin_name(lower_val)
                        .ok_or_else(|| conf_err(source_path, line, key, raw_val))?;
                self.ebcdic_tables = tbl;
            }
            "indirect-redefines" => match lower_val {
                "yes" | "true" => self.indirect_redefines = RedefinesChainPolicy::IndirectAllowed,
                "no" | "false" => self.indirect_redefines = RedefinesChainPolicy::DirectOnly,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "larger-redefines" | "larger-redefines-ok" => {
                let disp = FeatureDisposition::parse_value(lower_val)
                    .ok_or_else(|| conf_err(source_path, line, key, raw_val))?;
                self.larger_redefines = disp;
            }
            "relax-syntax-checks" => match lower_val {
                "yes" | "true" => {
                    self.relaxed_syntax = RelaxedSyntaxMode::Relaxed;
                    self.screen_conflict_policy = ScreenConflictPolicy::RelaxedWarning;
                }
                "no" | "false" => {
                    self.relaxed_syntax = RelaxedSyntaxMode::Strict;
                    self.screen_conflict_policy = ScreenConflictPolicy::StrictError;
                }
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "sticky-linkage" => match lower_val {
                "yes" | "true" => self.sticky_linkage = StickyLinkageMode::Sticky,
                "no" | "false" => self.sticky_linkage = StickyLinkageMode::Disabled,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "perform-osvs" => match lower_val {
                "yes" | "true" => self.perform_style = PerformReturnStyle::OsVsContinuation,
                "no" | "false" => self.perform_style = PerformReturnStyle::IsoStack,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            _ => {
                self.apply_ext_policy_or_feature_key(key, lower_val, raw_val, source_path, line)?;
            }
        }
        Ok(())
    }

    /// Applies XML/JSON, intrinsic, and generic feature-disposition `.conf` keys.
    ///
    /// # Arguments
    ///
    /// * `key` - Configuration key name.
    /// * `lower_val` - Lowercased unquoted value.
    /// * `raw_val` - Original value text for diagnostics.
    /// * `source_path` - Origin path for diagnostics.
    /// * `line` - 1-based line number.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ConfigLoad`] if the value is invalid for `key`.
    fn apply_ext_policy_or_feature_key(
        &mut self,
        key: &str,
        lower_val: &str,
        raw_val: &str,
        source_path: &str,
        line: usize,
    ) -> Result<()> {
        match key {
            "defaultbyte" => {
                self.default_byte =
                    parse_default_byte_value(raw_val, lower_val, source_path, line, key)?;
            }
            "assign-clause" => match lower_val {
                "dynamic" => self.assign_clause = AssignClauseDefault::Dynamic,
                "external" | "ibm" => self.assign_clause = AssignClauseDefault::External,
                "mf" => self.assign_clause = AssignClauseDefault::MicroFocus,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "dpc-in-data" => {
                self.dpc_in_data = DpcInDataMode::parse_token(lower_val)
                    .ok_or_else(|| conf_err(source_path, line, key, raw_val))?;
            }
            "xml-parse-xmlss" => match lower_val {
                "yes" | "true" => self.xml_parse_mode = XmlParseMode::Xmlss,
                "no" | "false" => self.xml_parse_mode = XmlParseMode::Compat,
                _ => return Err(conf_err(source_path, line, key, raw_val)),
            },
            "intrinsics" => {
                self.apply_intrinsics_cli_flag(raw_val);
            }
            "not-intrinsic" => {
                self.apply_not_intrinsic_cli_flag(raw_val);
            }
            "intrinsic-function" => {
                let upper = raw_val
                    .trim()
                    .trim_matches('"')
                    .trim_matches(SQ)
                    .to_ascii_uppercase();
                if !upper.is_empty() {
                    let _ = self.disabled_intrinsics.remove(&upper);
                }
            }
            "screen-section-rules" => {
                self.screen_section_rules = ScreenSectionRules::parse_token(lower_val)
                    .ok_or_else(|| conf_err(source_path, line, key, raw_val))?;
            }
            "register" => {
                self.apply_register_cli_flag(raw_val);
            }
            _ => {
                let disp = FeatureDisposition::parse_value(lower_val)
                    .ok_or_else(|| conf_err(source_path, line, key, raw_val))?;
                self.feature_rules.insert(String::from(key), disp);
            }
        }
        Ok(())
    }

    /// Applies `-fregister=name1,name2` (e.g. `LIN,COL`) onto `self`.
    ///
    /// # Arguments
    ///
    /// * `spec` - Raw comma-separated register specification.
    pub fn apply_register_cli_flag(&mut self, spec: &str) {
        for tok in spec.trim().trim_matches('"').trim_matches(SQ).split(',') {
            let up = tok
                .split('=')
                .next()
                .unwrap_or(tok)
                .trim()
                .trim_matches('"')
                .trim_matches(SQ)
                .to_ascii_uppercase();
            if !up.is_empty() {
                self.custom_registers.insert(up);
            }
        }
    }

    /// Applies `-fintrinsics=ALL` or `-fintrinsics=name1,name2` onto `self`.
    ///
    /// # Arguments
    ///
    /// * `spec` - Raw comma-separated intrinsic specification.
    pub fn apply_intrinsics_cli_flag(&mut self, spec: &str) {
        let trimmed = spec.trim().trim_matches('"').trim_matches(SQ);
        if trimmed.eq_ignore_ascii_case("ALL") {
            self.cli_intrinsics_mode = CliIntrinsicImportMode::All;
            return;
        }
        if self.cli_intrinsics_mode == CliIntrinsicImportMode::None {
            self.cli_intrinsics_mode = CliIntrinsicImportMode::Listed;
        }
        for tok in trimmed.split(',') {
            let up = tok.trim().to_ascii_uppercase();
            if !up.is_empty() {
                let _ = self.disabled_intrinsics.remove(&up);
                self.bare_intrinsics_set.insert(up);
            }
        }
    }

    /// Applies `-fnot-intrinsic=name1,name2` onto `self`, disabling matching intrinsic names.
    ///
    /// # Arguments
    ///
    /// * `spec` - Raw comma-separated intrinsic names to disable.
    pub fn apply_not_intrinsic_cli_flag(&mut self, spec: &str) {
        for tok in spec.trim().trim_matches('"').trim_matches(SQ).split(',') {
            let up = tok.trim().to_ascii_uppercase();
            if !up.is_empty() {
                let _ = self.bare_intrinsics_set.remove(&up);
                self.disabled_intrinsics.insert(up);
            }
        }
    }

    /// Returns `true` when canonical uppercase `intrinsic_name` is active in this dialect configuration.
    ///
    /// # Arguments
    ///
    /// * `intrinsic_name` - Canonical uppercase intrinsic name (e.g. `"ABS"` or `"SUBSTITUTE"`).
    #[must_use]
    pub fn is_intrinsic_enabled(&self, intrinsic_name: &str) -> bool {
        if self.disabled_intrinsics.contains(intrinsic_name) {
            return false;
        }
        if self.base_dialect == Dialect::Acu && !is_acu_dialect_intrinsic(intrinsic_name) {
            return self.bare_intrinsics_set.contains(intrinsic_name);
        }
        true
    }

    /// Returns `true` when `intrinsic_name` was imported for bare (no-`FUNCTION`) calls via CLI/config.
    ///
    /// # Arguments
    ///
    /// * `intrinsic_name` - Canonical uppercase intrinsic name.
    #[must_use]
    pub fn is_bare_intrinsic_imported(&self, intrinsic_name: &str) -> bool {
        if !self.is_intrinsic_enabled(intrinsic_name) {
            return false;
        }
        match self.cli_intrinsics_mode {
            CliIntrinsicImportMode::None => false,
            CliIntrinsicImportMode::All => true,
            CliIntrinsicImportMode::Listed => self.bare_intrinsics_set.contains(intrinsic_name),
        }
    }
}

/// Returns `true` when `name` belongs to the ACUCOBOL-GT (`-std=acu`) intrinsic set.
///
/// # Arguments
///
/// * `name` - Canonical uppercase intrinsic name.
#[must_use]
fn is_acu_dialect_intrinsic(name: &str) -> bool {
    matches!(
        name,
        "ABS"
            | "ACOS"
            | "ANNUITY"
            | "ASIN"
            | "ATAN"
            | "CHAR"
            | "COS"
            | "CURRENT-DATE"
            | "DATE-OF-INTEGER"
            | "DAY-OF-INTEGER"
            | "FACTORIAL"
            | "INTEGER"
            | "INTEGER-OF-DATE"
            | "INTEGER-OF-DAY"
            | "INTEGER-PART"
            | "LENGTH"
            | "LOG"
            | "LOG10"
            | "LOWER-CASE"
            | "MAX"
            | "MEAN"
            | "MEDIAN"
            | "MIDRANGE"
            | "MIN"
            | "MOD"
            | "NUMVAL"
            | "NUMVAL-C"
            | "ORD"
            | "ORD-MAX"
            | "ORD-MIN"
            | "PRESENT-VALUE"
            | "RANDOM"
            | "RANGE"
            | "REM"
            | "REVERSE"
            | "SIN"
            | "SQRT"
            | "STANDARD-DEVIATION"
            | "SUM"
            | "TAN"
            | "UPPER-CASE"
            | "VARIANCE"
            | "WHEN-COMPILED"
    )
}

/// Applies preset differences for each [`Dialect`] variant.
///
/// # Arguments
///
/// * `cfg` - Mutable [`DialectConfig`] initialized with ISO/default values.
/// * `dialect` - Requested [`Dialect`].
fn apply_dialect_overrides(cfg: &mut DialectConfig, dialect: Dialect) {
    match dialect {
        Dialect::Cobol85 => {
            cfg.max_word_length = 30;
            cfg.max_pic_digits = 18;
            cfg.pretty_display = DisplayFormatMode::IsoStandard;
            cfg.larger_redefines = FeatureDisposition::Unconformable;
        }
        Dialect::Cobol2002 | Dialect::Cobol2014 => {
            cfg.max_word_length = 63;
            cfg.max_pic_digits = 38;
            cfg.pretty_display = DisplayFormatMode::IsoStandard;
            cfg.larger_redefines = FeatureDisposition::Error;
            cfg.screen_section_rules = ScreenSectionRules::Std;
        }
        Dialect::Cobol2023 => {
            cfg.max_word_length = 63;
            cfg.max_pic_digits = 38;
            cfg.binary_byteorder = BinaryByteOrder::Native;
            cfg.pretty_display = DisplayFormatMode::IsoStandard;
            cfg.larger_redefines = FeatureDisposition::Error;
            cfg.screen_section_rules = ScreenSectionRules::Std;
        }
        Dialect::Gnu | Dialect::Default => {
            cfg.complex_odo = OdoComplexity::ComplexAllowed;
            cfg.indirect_redefines = RedefinesChainPolicy::IndirectAllowed;
            cfg.larger_redefines = FeatureDisposition::Warning;
            cfg.relaxed_syntax = RelaxedSyntaxMode::Relaxed;
        }
        Dialect::Ibm => {
            cfg.max_word_length = 30;
            cfg.binary_size = BinarySizePolicy::TwoFourEight;
            cfg.display_sign = DisplaySignMode::Ebcdic;
            cfg.complex_odo = OdoComplexity::ComplexAllowed;
            cfg.odo_slide = OdoSlidePolicy::SlideVariablyLocated;
            cfg.indirect_redefines = RedefinesChainPolicy::IndirectAllowed;
            cfg.larger_redefines = FeatureDisposition::Warning;
            cfg.sticky_linkage = StickyLinkageMode::Sticky;
            cfg.perform_style = PerformReturnStyle::OsVsContinuation;
            cfg.assign_clause = AssignClauseDefault::External;
        }
        Dialect::MicroFocus => {
            cfg.binary_size = BinarySizePolicy::OneToEight;
            cfg.binary_truncate = TruncationMode::NoTrunc;
            cfg.complex_odo = OdoComplexity::ComplexAllowed;
            cfg.indirect_redefines = RedefinesChainPolicy::IndirectAllowed;
            cfg.larger_redefines = FeatureDisposition::Ok;
            cfg.relaxed_syntax = RelaxedSyntaxMode::Relaxed;
            cfg.assign_clause = AssignClauseDefault::MicroFocus;
            cfg.screen_section_rules = ScreenSectionRules::Mf;
        }
        Dialect::Bs2000 => {
            cfg.complex_odo = OdoComplexity::ComplexAllowed;
            cfg.larger_redefines = FeatureDisposition::Warning;
        }
        Dialect::Acu => {
            cfg.complex_odo = OdoComplexity::ComplexAllowed;
            cfg.larger_redefines = FeatureDisposition::Warning;
            cfg.screen_section_rules = ScreenSectionRules::Acu;
        }
        Dialect::XOpen => {
            cfg.complex_odo = OdoComplexity::ComplexAllowed;
            cfg.larger_redefines = FeatureDisposition::Warning;
            cfg.screen_section_rules = ScreenSectionRules::Xopen;
        }
    }
}

/// Strips `#` comments outside double/single quotes from a `.conf` line.
///
/// # Arguments
///
/// * `line` - Raw `.conf` line.
#[must_use]
fn strip_conf_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (idx, ch) in line.char_indices() {
        if ch == '"' || ch == SQ {
            in_quotes = !in_quotes;
        } else if ch == '#' && !in_quotes {
            return line.get(..idx).unwrap_or(line);
        }
    }
    line
}

/// Builds a [`CoblangError::ConfigLoad`] error for an invalid `key: value` pair.
///
/// # Arguments
///
/// * `path` - Configuration file path.
/// * `line` - 1-based line number.
/// * `key` - Option key.
/// * `val` - Rejected option value.
#[must_use]
fn conf_err(path: &str, line: usize, key: &str, val: &str) -> CoblangError {
    CoblangError::ConfigLoad {
        path: String::from(path),
        line,
        message: format!("invalid value `{val}` for configuration key `{key}`"),
    }
}

/// Baseline uppercase COBOL reserved words shared across dialects.
const CORE_RESERVED_WORDS: &[&str] = &[
    "ACCEPT",
    "ADD",
    "ALLOCATE",
    "ALTER",
    "CALL",
    "CANCEL",
    "CLOSE",
    "COMPUTE",
    "CONFIGURATION",
    "CONTINUE",
    "COPY",
    "DATA",
    "DECLARATIVES",
    "DELETE",
    "DISPLAY",
    "DIVIDE",
    "DIVISION",
    "ELSE",
    "END",
    "END-CALL",
    "END-COMPUTE",
    "END-EVALUATE",
    "END-IF",
    "END-PERFORM",
    "END-READ",
    "END-SEARCH",
    "END-STRING",
    "END-UNSTRING",
    "END-WRITE",
    "ENVIRONMENT",
    "EVALUATE",
    "EXIT",
    "FD",
    "FILE",
    "FILE-CONTROL",
    "FILLER",
    "FREE",
    "FUNCTION",
    "FUNCTION-ID",
    "GENERATE",
    "GIVING",
    "GO",
    "GOBACK",
    "IDENTIFICATION",
    "IF",
    "INITIALIZE",
    "INITIATE",
    "INPUT-OUTPUT",
    "INSPECT",
    "INVOKE",
    "LINKAGE",
    "LOCAL-STORAGE",
    "MERGE",
    "MESSAGE",
    "MOVE",
    "MULTIPLY",
    "NEXT",
    "OCCURS",
    "OPEN",
    "PERFORM",
    "PIC",
    "PICTURE",
    "PROCEDURE",
    "PROGRAM-ID",
    "RAISE",
    "READ",
    "REDEFINES",
    "RELEASE",
    "RENAMES",
    "REPLACE",
    "REPORT",
    "RESUME",
    "RETURN",
    "RETURNING",
    "REWRITE",
    "RD",
    "SCREEN",
    "SD",
    "SEARCH",
    "SECTION",
    "SELECT",
    "SET",
    "SORT",
    "SPECIAL-NAMES",
    "START",
    "STOP",
    "STRING",
    "SUBTRACT",
    "SUPPRESS",
    "TERMINATE",
    "THEN",
    "TO",
    "UNLOCK",
    "UNSTRING",
    "USAGE",
    "USING",
    "VALUE",
    "WHEN",
    "WORKING-STORAGE",
    "WRITE",
];

/// Parses a `defaultbyte` / `-fdefaultbyte` value (`none`, `0..=255`, `0x00..=0xFF`, or single ASCII character).
///
/// # Arguments
///
/// * `raw_val` - Trimmed raw configuration value (preserving case and optional quotes).
/// * `lower_val` - Unquoted lowercase configuration value.
/// * `source_path` - Configuration file path for diagnostics.
/// * `line` - 1-based line number for diagnostics.
/// * `key` - Configuration key name for diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ConfigLoad`] if `raw_val` is not a valid `defaultbyte` specification.
fn parse_default_byte_value(
    raw_val: &str,
    lower_val: &str,
    source_path: &str,
    line: usize,
    key: &str,
) -> Result<Option<u8>> {
    if lower_val == "none" {
        return Ok(None);
    }
    if let Some(hex_str) = lower_val.strip_prefix("0x") {
        let byte = u8::from_str_radix(hex_str, 16)
            .map_err(|_| conf_err(source_path, line, key, raw_val))?;
        return Ok(Some(byte));
    }
    if let Ok(num) = lower_val.parse::<u8>() {
        return Ok(Some(num));
    }
    let trimmed = raw_val.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| trimmed.strip_prefix(SQ).and_then(|s| s.strip_suffix(SQ)))
        .ok_or_else(|| conf_err(source_path, line, key, raw_val))?;
    if let [single] = unquoted.as_bytes() {
        return Ok(Some(*single));
    }
    Err(conf_err(source_path, line, key, raw_val))
}

/// Returns the core uppercase COBOL reserved-word baseline shared across dialects.
#[must_use]
fn default_core_reserved_words() -> BTreeSet<String> {
    CORE_RESERVED_WORDS
        .iter()
        .copied()
        .map(String::from)
        .collect()
}
