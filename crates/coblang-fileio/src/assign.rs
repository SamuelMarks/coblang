//! `ASSIGN TO` path and environment resolver (`DD_<NAME>`, `dd_<name>`, `<NAME>`, `COB_FILE_PATH`,
//! literal vs dynamic `WORKING-STORAGE` data items, and `COB_LS_FIXED` / `COB_LS_NULLS`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use coblang_common::{CoblangError, FileStatusKind, Result};

use crate::types::{ASCII_NUL, NullEscapePolicy, TrailingSpacePolicy};

/// ASCII backslash byte (`0x5C`) used to detect Windows-style directory separators in literals.
const ASCII_BACKSLASH: u8 = 0x5C;

/// Read-only environment variable lookup abstraction for deterministic testing and process execution.
pub trait EnvLookup {
    /// Looks up the value of `key` in the active environment, if set.
    ///
    /// # Arguments
    ///
    /// * `key` - Exact environment variable name (case-sensitive on POSIX).
    fn get_var(&self, key: &str) -> Option<String>;
}

/// Host process environment lookup backed by [`std::env::var`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    /// Queries [`std::env::var`] for `key`.
    ///
    /// # Arguments
    ///
    /// * `key` - Environment variable identifier.
    fn get_var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Deterministic in-memory environment map for unit and conformance tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapEnv {
    /// Stored `(name, value)` environment bindings.
    vars: BTreeMap<String, String>,
}

impl MapEnv {
    /// Creates an empty [`MapEnv`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            vars: BTreeMap::new(),
        }
    }

    /// Inserts or replaces an environment variable binding, returning `self` for chaining.
    ///
    /// # Arguments
    ///
    /// * `key` - Environment variable name.
    /// * `value` - Environment variable value.
    #[must_use]
    pub fn with_var(mut self, key: &str, value: &str) -> Self {
        self.vars.insert(String::from(key), String::from(value));
        self
    }

    /// Sets or updates an environment variable binding in place.
    ///
    /// # Arguments
    ///
    /// * `key` - Environment variable name.
    /// * `value` - Environment variable value.
    pub fn insert(&mut self, key: &str, value: &str) {
        self.vars.insert(String::from(key), String::from(value));
    }
}

impl EnvLookup for MapEnv {
    /// Looks up `key` inside the in-memory `BTreeMap`.
    ///
    /// # Arguments
    ///
    /// * `key` - Environment variable identifier.
    fn get_var(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }
}

/// Standard console/stream device target in `ASSIGN TO DISPLAY | KEYBOARD | SYSERR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardDevice {
    /// Standard output (`DISPLAY` / `STDOUT`).
    DisplayStdout,
    /// Standard input (`KEYBOARD` / `STDIN`).
    KeyboardStdin,
    /// Standard error (`SYSERR` / `STDERR`).
    SyserrStderr,
}

/// Controls whether bare string literals (`ASSIGN TO "MYFILE"`) check `DD_*` environment overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiteralEnvPolicy {
    /// Treat string literals strictly as filesystem paths without `DD_*` environment indirection.
    LiteralOnly,
    /// Check `DD_<NAME>`, `dd_<name>`, `<NAME>` when the literal contains no path separator.
    CheckEnvWhenBareName,
}

/// Controls whether mainframe organization prefixes (`S-`, `AS-`, `UT-S-`) are stripped from external names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignPrefixPolicy {
    /// Preserve the full external name verbatim.
    PreserveFull,
    /// Strip leading mainframe JCL prefixes (`UT-S-`, `AS-`, `S-`) before `DD_*` lookup and fallback filename.
    StripMainframePrefix,
}

/// Dialect configuration governing `ASSIGN TO` resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssignResolutionConfig {
    /// Environment lookup policy for string literals.
    pub literal_env_policy: LiteralEnvPolicy,
    /// Mainframe prefix stripping policy for external DD names.
    pub prefix_policy: AssignPrefixPolicy,
}

impl Default for AssignResolutionConfig {
    /// Returns the default `GnuCOBOL` / `gcobol` compatible resolution policy.
    fn default() -> Self {
        Self {
            literal_env_policy: LiteralEnvPolicy::CheckEnvWhenBareName,
            prefix_policy: AssignPrefixPolicy::StripMainframePrefix,
        }
    }
}

/// Source specification of a COBOL `SELECT ... ASSIGN TO` clause at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignSource {
    /// Quoted string literal (`ASSIGN TO "orders.dat"`).
    Literal(String),
    /// External DD/environment identifier (`ASSIGN TO [EXTERNAL] INFILE` or `"S-INFILE"`).
    ExternalName(String),
    /// Dynamic `WORKING-STORAGE` / `LOCAL-STORAGE` / `LINKAGE` alphanumeric field bytes.
    DynamicVariable(Vec<u8>),
    /// Standard stream device (`DISPLAY`, `KEYBOARD`, `SYSERR`).
    Device(StandardDevice),
}

/// Provenance indicating how [`resolve_assign_target`] obtained the final target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignResolutionOrigin {
    /// Matched `DD_<UPPER_NAME>` environment variable.
    DdUpperEnv,
    /// Matched `dd_<lower_name>` environment variable.
    DdLowerEnv,
    /// Matched `<UPPER_NAME>` or exact `<name>` environment variable.
    DirectEnv,
    /// Extracted from a dynamic `WORKING-STORAGE` data item.
    DynamicVariable,
    /// Taken directly from a string literal path.
    LiteralPath,
    /// Defaulted to the normalized external name when no `DD_*` / `<NAME>` env var was set.
    DefaultExternalFilename,
    /// Bound to a standard stream device (`STDOUT` / `STDIN` / `STDERR`).
    StandardDevice,
}

/// Concrete resolved destination of an `ASSIGN TO` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAssignTarget {
    /// Concrete filesystem path (with `COB_FILE_PATH` applied when relative).
    FilesystemPath(PathBuf),
    /// Standard process stream device.
    StandardDevice(StandardDevice),
}

/// Complete output of resolving a COBOL `ASSIGN TO` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAssign {
    /// Resolved filesystem path or standard stream device.
    pub target: ResolvedAssignTarget,
    /// Resolution step that produced `target`.
    pub origin: AssignResolutionOrigin,
}

/// Strips leading/trailing ASCII spaces (`0x20`) and NUL (`0x00`) bytes from raw path text.
///
/// # Arguments
///
/// * `raw` - Raw string slice from literal, env var, or COBOL data item.
#[must_use]
fn trim_cobol_path_str(raw: &str) -> &str {
    let nul_char = char::from(ASCII_NUL);
    raw.trim_matches(|c: char| c == ' ' || c == nul_char)
}

/// Strips mainframe organization prefixes (`UT-S-`, `AS-`, `S-`) case-insensitively when enabled.
///
/// # Arguments
///
/// * `name` - Trimmed external DD name.
/// * `policy` - Prefix stripping policy.
#[must_use]
fn apply_prefix_policy(name: &str, policy: AssignPrefixPolicy) -> &str {
    match policy {
        AssignPrefixPolicy::PreserveFull => name,
        AssignPrefixPolicy::StripMainframePrefix => {
            let upper = name.to_ascii_uppercase();
            let prefix_len = if upper.starts_with("UT-S-") && name.len() > 5 {
                5
            } else if upper.starts_with("AS-") && name.len() > 3 {
                3
            } else if upper.starts_with("S-") && name.len() > 2 {
                2
            } else {
                0
            };
            name.get(prefix_len..).unwrap_or(name)
        }
    }
}

/// Sanitizes a COBOL identifier for environment lookup by replacing `-` with `_`.
///
/// # Arguments
///
/// * `name` - External DD name.
#[must_use]
fn sanitize_env_stem(name: &str) -> String {
    name.chars()
        .map(|c| if c == '-' { '_' } else { c })
        .collect()
}

/// Looks up `DD_<NAME>`, `dd_<name>`, `<UPPER_NAME>`, and `<raw_name>` in `env`.
///
/// # Arguments
///
/// * `stem` - Normalized external identifier (after optional mainframe prefix stripping).
/// * `env` - Environment lookup provider.
#[must_use]
fn lookup_dd_environment(
    stem: &str,
    env: &impl EnvLookup,
) -> Option<(String, AssignResolutionOrigin)> {
    let sanitized = sanitize_env_stem(stem);
    let upper = sanitized.to_ascii_uppercase();
    let lower = sanitized.to_ascii_lowercase();

    let dd_upper_key = format!("DD_{upper}");
    if let Some(val) = env.get_var(&dd_upper_key) {
        let trimmed = trim_cobol_path_str(&val);
        if !trimmed.is_empty() {
            return Some((String::from(trimmed), AssignResolutionOrigin::DdUpperEnv));
        }
    }

    let dd_lower_key = format!("dd_{lower}");
    if let Some(val) = env.get_var(&dd_lower_key) {
        let trimmed = trim_cobol_path_str(&val);
        if !trimmed.is_empty() {
            return Some((String::from(trimmed), AssignResolutionOrigin::DdLowerEnv));
        }
    }

    if let Some(val) = env.get_var(&upper) {
        let trimmed = trim_cobol_path_str(&val);
        if !trimmed.is_empty() {
            return Some((String::from(trimmed), AssignResolutionOrigin::DirectEnv));
        }
    }

    if stem != upper
        && let Some(val) = env.get_var(stem)
    {
        let trimmed = trim_cobol_path_str(&val);
        if !trimmed.is_empty() {
            return Some((String::from(trimmed), AssignResolutionOrigin::DirectEnv));
        }
    }

    None
}

/// Applies `COB_FILE_PATH` base-directory prepending when `raw_path` is relative.
///
/// # Arguments
///
/// * `raw_path` - Non-empty resolved filename or path string.
/// * `env` - Environment lookup provider queried for `COB_FILE_PATH`.
#[must_use]
fn apply_cob_file_path(raw_path: &str, env: &impl EnvLookup) -> PathBuf {
    let candidate = Path::new(raw_path);
    if candidate.is_absolute() {
        return PathBuf::from(candidate);
    }
    if let Some(base) = env.get_var("COB_FILE_PATH") {
        let trimmed_base = trim_cobol_path_str(&base);
        if !trimmed_base.is_empty() {
            return Path::new(trimmed_base).join(candidate);
        }
    }
    PathBuf::from(candidate)
}

/// Checks whether `path_str` contains no forward or backward directory separator.
///
/// # Arguments
///
/// * `path_str` - Trimmed literal path candidate.
#[must_use]
fn is_bare_filename(path_str: &str) -> bool {
    !path_str.bytes().any(|b| b == b'/' || b == ASCII_BACKSLASH)
}

/// Resolves a COBOL `SELECT ... ASSIGN TO` clause into a concrete path or standard device.
///
/// Resolution order:
/// 1. [`AssignSource::Device`] resolves immediately to [`ResolvedAssignTarget::StandardDevice`].
/// 2. [`AssignSource::DynamicVariable`] decodes UTF-8/ASCII bytes, trims spaces/NULs, rejects
///    all-blank filenames with [`CoblangError::RuntimeFileIo`] (`"30"`), and prepends `COB_FILE_PATH`
///    if relative.
/// 3. [`AssignSource::ExternalName`] strips optional mainframe prefixes (`UT-S-`, `AS-`, `S-`),
///    queries `DD_<NAME>`, `dd_<name>`, `<UPPER_NAME>`, `<name>`, falls back to the normalized stem,
///    and prepends `COB_FILE_PATH` if relative.
/// 4. [`AssignSource::Literal`] optionally checks `DD_*` when bare (no path separator), otherwise
///    uses the literal path directly and prepends `COB_FILE_PATH` if relative.
///
/// # Arguments
///
/// * `file_name` - Logical COBOL file connector name (for error reporting).
/// * `source` - Runtime `ASSIGN TO` source specification.
/// * `config` - Dialect resolution policy.
/// * `env` - Environment variable lookup provider.
///
/// # Errors
///
/// Returns [`CoblangError::RuntimeFileIo`] (`PermanentError30`) if the resolved filename is empty
/// or contains invalid UTF-8 bytes in a dynamic path variable.
pub fn resolve_assign_target(
    file_name: &str,
    source: &AssignSource,
    config: AssignResolutionConfig,
    env: &impl EnvLookup,
) -> Result<ResolvedAssign> {
    match source {
        AssignSource::Device(dev) => Ok(ResolvedAssign {
            target: ResolvedAssignTarget::StandardDevice(*dev),
            origin: AssignResolutionOrigin::StandardDevice,
        }),
        AssignSource::DynamicVariable(bytes) => {
            let raw_str = core::str::from_utf8(bytes).map_err(|_| CoblangError::RuntimeFileIo {
                file_name: String::from(file_name),
                status: FileStatusKind::PermanentError30.code(),
                message: String::from(
                    "dynamic ASSIGN TO variable contains invalid UTF-8 path bytes",
                ),
            })?;
            let trimmed = trim_cobol_path_str(raw_str);
            if trimmed.is_empty() {
                return Err(CoblangError::RuntimeFileIo {
                    file_name: String::from(file_name),
                    status: FileStatusKind::PermanentError30.code(),
                    message: String::from("dynamic ASSIGN TO variable resolved to an empty path"),
                });
            }
            Ok(ResolvedAssign {
                target: ResolvedAssignTarget::FilesystemPath(apply_cob_file_path(trimmed, env)),
                origin: AssignResolutionOrigin::DynamicVariable,
            })
        }
        AssignSource::ExternalName(raw_ext) => {
            let trimmed = trim_cobol_path_str(raw_ext);
            if trimmed.is_empty() {
                return Err(CoblangError::RuntimeFileIo {
                    file_name: String::from(file_name),
                    status: FileStatusKind::PermanentError30.code(),
                    message: String::from("EXTERNAL ASSIGN TO name is empty"),
                });
            }
            let stem = apply_prefix_policy(trimmed, config.prefix_policy);
            let (chosen, origin) = lookup_dd_environment(stem, env).unwrap_or_else(|| {
                (
                    String::from(stem),
                    AssignResolutionOrigin::DefaultExternalFilename,
                )
            });
            Ok(ResolvedAssign {
                target: ResolvedAssignTarget::FilesystemPath(apply_cob_file_path(&chosen, env)),
                origin,
            })
        }
        AssignSource::Literal(raw_lit) => {
            let trimmed = trim_cobol_path_str(raw_lit);
            if trimmed.is_empty() {
                return Err(CoblangError::RuntimeFileIo {
                    file_name: String::from(file_name),
                    status: FileStatusKind::PermanentError30.code(),
                    message: String::from("literal ASSIGN TO path is empty"),
                });
            }
            if config.literal_env_policy == LiteralEnvPolicy::CheckEnvWhenBareName
                && is_bare_filename(trimmed)
            {
                let stem = apply_prefix_policy(trimmed, config.prefix_policy);
                if let Some((env_path, origin)) = lookup_dd_environment(stem, env) {
                    return Ok(ResolvedAssign {
                        target: ResolvedAssignTarget::FilesystemPath(apply_cob_file_path(
                            &env_path, env,
                        )),
                        origin,
                    });
                }
            }
            Ok(ResolvedAssign {
                target: ResolvedAssignTarget::FilesystemPath(apply_cob_file_path(trimmed, env)),
                origin: AssignResolutionOrigin::LiteralPath,
            })
        }
    }
}

/// Runtime options for `LINE SEQUENTIAL` parsed from `COB_LS_FIXED` and `COB_LS_NULLS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineSequentialEnvOptions {
    /// Trailing-space handling derived from `COB_LS_FIXED`.
    pub trailing_spaces: TrailingSpacePolicy,
    /// Control-character NUL escaping derived from `COB_LS_NULLS`.
    pub null_escapes: NullEscapePolicy,
}

impl LineSequentialEnvOptions {
    /// Parses `COB_LS_FIXED` and `COB_LS_NULLS` from `env`.
    ///
    /// Recognized truthy values (case-insensitive): `"1"`, `"Y"`, `"YES"`, `"ON"`, `"TRUE"`.
    ///
    /// # Arguments
    ///
    /// * `env` - Environment lookup provider.
    #[must_use]
    pub fn from_env(env: &impl EnvLookup) -> Self {
        let trailing_spaces = if env_flag_enabled(env, "COB_LS_FIXED") {
            TrailingSpacePolicy::PreserveFixedSpaces
        } else {
            TrailingSpacePolicy::StripTrailingSpaces
        };
        let null_escapes = if env_flag_enabled(env, "COB_LS_NULLS") {
            NullEscapePolicy::NullPrefixControlBytes
        } else {
            NullEscapePolicy::RawBytes
        };
        Self {
            trailing_spaces,
            null_escapes,
        }
    }
}

/// Evaluates whether an environment flag variable is set to a truthy COBOL runtime token.
///
/// # Arguments
///
/// * `env` - Environment lookup provider.
/// * `key` - Environment variable name.
#[must_use]
fn env_flag_enabled(env: &impl EnvLookup, key: &str) -> bool {
    let Some(raw) = env.get_var(key) else {
        return false;
    };
    matches!(
        raw.trim().to_ascii_uppercase().as_str(),
        "1" | "Y" | "YES" | "ON" | "TRUE"
    )
}
