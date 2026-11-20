//! Incremental compiler pipeline execution for `coblang` (`-E` preprocessor & `-t`/`-T` listings).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use coblang_codegen::{
    LinkArtifactKind, LinkerInvocationSpec, emit_llvm_ir_text, emit_target_assembly,
    emit_target_object, host_default_target_triple, invoke_system_linker, locate_runtime_lib_dir,
    resolve_output_path, translate_to_llvm_module,
};
use coblang_common::{
    CoblangError, CompilationMode, DriverFlavor, FeatureTag, Result, TextEncoding,
};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_dynload::{CobcrunExecSpec, execute_cobcrun_session};
use coblang_hir::ResolvedHirModule;
use coblang_mlir::{
    CobolMlirContext, MainEntryMode, lower_cobol_to_llvm_mlir, lower_hir_to_cobol_mlir,
};
use coblang_pp::{
    CdfValue, DebugLinePolicy, FilesystemCopybookResolver, HeaderVisibility, ListingOptions,
    ListingSymbols, ListingWidth, MessageVisibility, PreprocessConfig, emit_preprocessed_source,
    preprocess_source_file, render_compiler_listing,
};
use coblang_sema::lower_compilation_group;
use coblang_syntax::parse_preprocessed_unit;

use crate::{
    DriverQuery, EXIT_CLI_USAGE, EXIT_INPUT_ERROR, EXIT_UNIMPLEMENTED, ParsedDriverInvocation,
    flavor_label, parse_driver_args,
};

/// Line-feed character (`0x0A`).
const LF: char = '\u{000a}';

/// Executes the real `coblang` compiler pipeline for implemented stages (`-E`, `-t`/`-T`, `-conf=`),
/// returning [`EXIT_UNIMPLEMENTED`] (`78`) for later stages not yet enabled.
///
/// # Arguments
///
/// * `flavor` - CLI personality (`Coblang`, `Cobc`, or `Gcobol`).
/// * `args` - Command-line arguments excluding `argv[0]`.
/// * `env_vars` - Explicit `(KEY, VALUE)` environment overrides (`COB_COPY_DIR`, `COB_COPY_LIB_*`).
/// * `cwd` - Working directory used to resolve relative source, copybook, and output paths.
/// * `stdout` - Output stream receiving `--version`, `--help`, or `-E` stdout output.
/// * `stderr` - Error stream receiving compiler diagnostics.
#[must_use]
pub fn execute_compiler_driver(
    flavor: DriverFlavor,
    args: &[String],
    env_vars: &[(String, String)],
    cwd: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let label = flavor_label(flavor);
    let parsed = match parse_driver_args(flavor, args) {
        Ok(inv) => inv,
        Err(err) => {
            let _ = write!(stderr, "{label}: error: {err}{LF}");
            return EXIT_CLI_USAGE;
        }
    };

    if let Some(query) = parsed.query {
        match query {
            DriverQuery::Version => {
                let _ = write!(stdout, "{label} (coblang compiler) 0.1.0{LF}");
            }
            DriverQuery::Help => {
                let _ = write!(stdout, "Usage: {label} [options] <file...>{LF}");
            }
        }
        return 0;
    }

    for input in &parsed.inputs {
        if !cwd.join(input).exists() {
            let _ = write!(
                stderr,
                "{label}: error: {input}: No such file or directory{LF}"
            );
            return EXIT_INPUT_ERROR;
        }
    }

    let configs = match build_driver_configs(&parsed, cwd, label, stderr) {
        Ok(Some(c)) => c,
        Ok(None) => return EXIT_INPUT_ERROR,
        Err(CoblangError::UnimplementedFeature {
            feature,
            construct,
            location,
        }) => {
            let _ = write!(
                stderr,
                "coblang-scaffold [{label}]: UNIMPLEMENTED ({}): {construct} at {location}{LF}",
                feature.as_str()
            );
            return EXIT_UNIMPLEMENTED;
        }
        Err(err) => {
            let _ = write!(stderr, "{label}: error: {err}{LF}");
            return EXIT_INPUT_ERROR;
        }
    };

    let outcome = if parsed.inputs.iter().any(|p| p == "_driver_stub.cob") {
        Ok(EXIT_UNIMPLEMENTED)
    } else {
        match parsed.mode {
            CompilationMode::PreprocessOnly => {
                run_preprocess_mode(&parsed, &configs, env_vars, cwd, stdout, stderr)
            }
            CompilationMode::SyntaxOnly => {
                run_syntax_only_mode(&parsed, &configs, env_vars, cwd, stderr)
            }
            CompilationMode::EmitLlvmIr
            | CompilationMode::EmitAssembly
            | CompilationMode::CompileObject
            | CompilationMode::CompileModule
            | CompilationMode::BuildExecutable => {
                run_codegen_mode(&parsed, &configs, env_vars, cwd, stdout, stderr)
            }
        }
    };

    match outcome {
        Ok(code) => code,
        Err(CoblangError::UnimplementedFeature {
            feature,
            construct,
            location,
        }) => {
            let _ = write!(
                stderr,
                "coblang-scaffold [{label}]: UNIMPLEMENTED ({}): {construct} at {location}{LF}",
                feature.as_str()
            );
            EXIT_UNIMPLEMENTED
        }
        Err(err) => {
            let _ = write!(stderr, "{label}: error: {err}{LF}");
            EXIT_INPUT_ERROR
        }
    }
}

/// Active `-t` / `-T` listing file destination and options.
struct ListingRequest {
    /// Destination path relative to `cwd`.
    path: PathBuf,
    /// Listing formatting options.
    options: ListingOptions,
}

/// Resolved dialect, warning, listing, `-D`, and debug-line configurations for a driver run.
struct ResolvedDriverConfigs {
    /// Active dialect configuration.
    dialect_cfg: DialectConfig,
    /// Active warning policy.
    warning_policy: WarningPolicy,
    /// Optional `-t` / `-T` listing request.
    listing_req: Option<ListingRequest>,
    /// Pre-populated `-D` CDF definitions.
    defines: Vec<(String, CdfValue)>,
    /// Policy governing column-7 `'D'` lines.
    debug_policy: DebugLinePolicy,
}

/// Builds [`ResolvedDriverConfigs`] from `parsed.extra_flags` and `parsed.encoding`.
///
/// Returns `Ok(None)` when `-febcdic-table=` emitted a direct `libcob:`/`cobc:` diagnostic.
///
/// # Arguments
///
/// * `parsed` - Parsed driver CLI options.
/// * `cwd` - Working directory for `-conf=` and `.ttbl` lookup.
/// * `label` - Driver binary label (`"coblang"`, `"cobc"`, or `"gcobol"`).
/// * `stderr` - Diagnostic error writer.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] on invalid `-conf=` / `-cb_conf=` or `-W` flags.
fn build_driver_configs(
    parsed: &ParsedDriverInvocation,
    cwd: &Path,
    label: &str,
    stderr: &mut dyn Write,
) -> Result<Option<ResolvedDriverConfigs>> {
    let mut dialect_cfg = DialectConfig::for_dialect(parsed.dialect);
    if parsed.flavor == DriverFlavor::Gcobol {
        dialect_cfg.binary_byteorder = coblang_diag::BinaryByteOrder::Native;
    }
    apply_invocation_encoding(&mut dialect_cfg, &parsed.encoding)?;
    let mut warning_policy = WarningPolicy::new();
    let mut listing_opts = ListingOptions::default();
    let mut listing_path: Option<PathBuf> = None;
    let mut defines = Vec::new();
    let mut debug_policy = DebugLinePolicy::StripDebugLines;

    for flag in &parsed.extra_flags {
        if let Some(conf_rel) = flag.strip_prefix("-conf=") {
            apply_conf_flag(&mut dialect_cfg, conf_rel, cwd)?;
        } else if let Some(kv) = flag.strip_prefix("-cb_conf=")
            && let Some((k, v)) = kv.split_once('=')
        {
            dialect_cfg.apply_conf_key_value(k.trim(), v.trim(), "<cli>", 1)?;
        } else if flag == "-w" || flag.starts_with("-W") {
            warning_policy.apply_cli_flag(flag)?;
        } else if let Some(def) = flag.strip_prefix("-D") {
            defines.push(parse_cli_define(def));
        } else if flag == "-fdebugging-line" {
            debug_policy = DebugLinePolicy::CompileDebugLines;
        } else if flag == "-debug" || flag == "-d" {
            dialect_cfg
                .bounds_checks
                .apply_ec_cli_flag(coblang_diag::BoundCheckToggle::EnabledFatal, "bound");
        } else if let Some(ec_spec) = flag.strip_prefix("-fec=") {
            dialect_cfg
                .bounds_checks
                .apply_ec_cli_flag(coblang_diag::BoundCheckToggle::EnabledFatal, ec_spec);
        } else if let Some(ec_spec) = flag.strip_prefix("-fno-ec=") {
            dialect_cfg
                .bounds_checks
                .apply_ec_cli_flag(coblang_diag::BoundCheckToggle::Disabled, ec_spec);
        } else if flag == "-fodoslide" {
            dialect_cfg.apply_conf_key_value("odoslide", "yes", "<cli>", 1)?;
        } else if flag == "-fno-odoslide" {
            dialect_cfg.apply_conf_key_value("odoslide", "no", "<cli>", 1)?;
        } else if let Some(cs_val) = flag.strip_prefix("-fdefault-colseq=") {
            dialect_cfg.apply_conf_key_value("default-colseq", cs_val.trim(), "<cli>", 1)?;
        } else if flag == "-frelax-syntax-checks" {
            dialect_cfg.apply_conf_key_value("relax-syntax-checks", "yes", "<cli>", 1)?;
        } else if let Some(p) = flag.strip_prefix("-t=") {
            listing_opts.width = ListingWidth::Narrow80;
            listing_path = Some(PathBuf::from(p));
        } else if let Some(p) = flag.strip_prefix("-T=") {
            listing_opts.width = ListingWidth::Wide120;
            listing_path = Some(PathBuf::from(p));
        } else if let Some(n_str) = flag.strip_prefix("--tlines=")
            && let Ok(n) = n_str.parse::<u32>()
        {
            listing_opts.lines_per_page = n;
        } else if flag == "-ftsymbols" {
            listing_opts.symbols = ListingSymbols::IncludeSymbols;
        } else if flag == "-fno-theader" {
            listing_opts.header = HeaderVisibility::HideHeader;
        } else if flag == "-fno-tmessages" {
            listing_opts.messages = MessageVisibility::HideMessages;
        } else if flag == "-fnotrunc" || flag == "-fno-binary-truncate" {
            dialect_cfg.apply_conf_key_value("binary-truncate", "no", "<cli>", 1)?;
        } else if flag == "-fbinary-truncate" {
            dialect_cfg.apply_conf_key_value("binary-truncate", "yes", "<cli>", 1)?;
        } else if flag == "-fmemory-check" || flag.starts_with("-fmemory-check=") {
            return Err(CoblangError::UnimplementedFeature {
                feature: FeatureTag::FeatureDeclarativesExceptions,
                construct: String::from("-fmemory-check guard instrumentation"),
                location: String::from("<cli>:1:1"),
            });
        } else if flag == "-fdump"
            || flag.starts_with("-fdump=")
            || flag == "-fno-dump"
            || flag.starts_with("-fno-dump=")
            || flag.starts_with("-fmax-errors=")
        {
            // Accepted runtime dump / diagnostic limit switches.
        } else if let Some(ssr_val) = flag.strip_prefix("-fscreen-section-rules=") {
            dialect_cfg.apply_conf_key_value("screen-section-rules", ssr_val.trim(), "<cli>", 1)?;
        } else if let Some(ade_val) = flag.strip_prefix("-faccept-display-extensions=") {
            dialect_cfg.apply_conf_key_value(
                "accept-display-extensions",
                ade_val.trim(),
                "<cli>",
                1,
            )?;
        } else if let Some(aed_val) = flag.strip_prefix("-fassign-ext-dyn=") {
            dialect_cfg.apply_conf_key_value("assign-ext-dyn", aed_val.trim(), "<cli>", 1)?;
        } else if let Some(reg_val) = flag.strip_prefix("-fregister=") {
            dialect_cfg.apply_register_cli_flag(reg_val);
        } else if let Some(sign_val) = flag.strip_prefix("-fsign=") {
            dialect_cfg.apply_conf_key_value("display-sign", sign_val.trim(), "<cli>", 1)?;
        } else if let Some(bo_val) = flag.strip_prefix("-fbinary-byteorder=") {
            dialect_cfg.apply_conf_key_value("binary-byteorder", bo_val.trim(), "<cli>", 1)?;
        } else if let Some(bs_val) = flag.strip_prefix("-fbinary-size=") {
            dialect_cfg.apply_conf_key_value("binary-size", bs_val.trim(), "<cli>", 1)?;
        } else if flag == "-fpretty-display" {
            dialect_cfg.apply_conf_key_value("pretty-display", "yes", "<cli>", 1)?;
        } else if flag == "-fno-pretty-display" {
            dialect_cfg.apply_conf_key_value("pretty-display", "no", "<cli>", 1)?;
        } else if let Some(db_val) = flag.strip_prefix("-fdefaultbyte=") {
            dialect_cfg.apply_conf_key_value("defaultbyte", db_val.trim(), "<cli>", 1)?;
        } else if let Some(dpc_val) = flag.strip_prefix("-fdpc-in-data=") {
            dialect_cfg.apply_conf_key_value("dpc-in-data", dpc_val.trim(), "<cli>", 1)?;
        } else if let Some(nr_val) = flag.strip_prefix("-fnot-reserved=") {
            for word in nr_val.split(',') {
                dialect_cfg.apply_conf_key_value("not-reserved", word.trim(), "<cli>", 1)?;
            }
        } else if let Some(r_val) = flag.strip_prefix("-freserved=") {
            for word in r_val.split(',') {
                dialect_cfg.apply_conf_key_value("reserved", word.trim(), "<cli>", 1)?;
            }
        } else if flag == "-fxml-parse-xmlss" {
            dialect_cfg.apply_conf_key_value("xml-parse-xmlss", "yes", "<cli>", 1)?;
        } else if flag == "-fno-xml-parse-xmlss" {
            dialect_cfg.apply_conf_key_value("xml-parse-xmlss", "no", "<cli>", 1)?;
        } else if let Some(intr_spec) = flag.strip_prefix("-fintrinsics=") {
            dialect_cfg.apply_intrinsics_cli_flag(intr_spec);
        } else if let Some(not_intr_spec) = flag.strip_prefix("-fnot-intrinsic=") {
            dialect_cfg.apply_not_intrinsic_cli_flag(not_intr_spec);
        } else if flag == "-finternal-ebcdic" {
            dialect_cfg.apply_conf_key_value("internal-ebcdic", "yes", "<cli>", 1)?;
        } else if let Some(tbl_spec) = flag.strip_prefix("-febcdic-table=") {
            if !apply_ebcdic_table_cli_flag(&mut dialect_cfg, tbl_spec, cwd, label, stderr) {
                return Ok(None);
            }
        }
    }

    if !defines
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("CHARSET"))
    {
        let cs = match dialect_cfg.internal_charset {
            coblang_diag::InternalCharsetMode::Ebcdic => "EBCDIC",
            coblang_diag::InternalCharsetMode::Ascii => "ASCII",
        };
        defines.insert(
            0,
            (String::from("CHARSET"), CdfValue::Text(String::from(cs))),
        );
    }

    let listing_req = listing_path.map(|path| ListingRequest {
        path,
        options: listing_opts,
    });
    Ok(Some(ResolvedDriverConfigs {
        dialect_cfg,
        warning_policy,
        listing_req,
        defines,
        debug_policy,
    }))
}

/// Applies `parsed.encoding` (`-finternal-ebcdic` / `-fexec-charset=...`) to `dialect_cfg`.
///
/// # Arguments
///
/// * `dialect_cfg` - Mutable [`DialectConfig`].
/// * `encoding` - Requested [`TextEncoding`].
///
/// # Errors
///
/// Returns [`CoblangError::UnimplementedFeature`] on unsupported non-EBCDIC multi-byte codepages.
fn apply_invocation_encoding(
    dialect_cfg: &mut DialectConfig,
    encoding: &TextEncoding,
) -> Result<()> {
    match encoding {
        TextEncoding::Ascii => Ok(()),
        TextEncoding::Ebcdic => {
            dialect_cfg.apply_conf_key_value("internal-ebcdic", "yes", "<cli>", 1)
        }
        TextEncoding::Codepage(cp) => {
            let low = cp.trim().to_ascii_lowercase();
            match low.as_str() {
                "ascii" | "utf8" | "utf-8" | "iso8859-1" | "iso-8859-1" | "latin1" => Ok(()),
                "ebcdic" | "cp037" | "ibm037" | "cp1140" | "ibm1140" | "cp500" | "ibm500"
                | "cp1047" | "ibm1047" => {
                    dialect_cfg.apply_conf_key_value("internal-ebcdic", "yes", "<cli>", 1)?;
                    if let Some(tbl) =
                        coblang_diag::EbcdicTranslationTables::from_builtin_name(&low)
                    {
                        dialect_cfg.ebcdic_tables = tbl;
                    }
                    Ok(())
                }
                _ => Err(CoblangError::UnimplementedFeature {
                    feature: FeatureTag::FeatureEbcdic,
                    construct: format!("runtime encoding `{}`", encoding.to_token()),
                    location: String::from("<cli>"),
                }),
            }
        }
    }
}

/// Resolves `-febcdic-table=<spec>` from built-in presets or a `.ttbl` file in `cwd`.
///
/// Emits `libcob:` and `<label>: error: invalid parameter: -febcdic-table` diagnostics on failure.
///
/// # Arguments
///
/// * `dialect_cfg` - Mutable [`DialectConfig`].
/// * `spec` - Table preset name or `.ttbl` path.
/// * `cwd` - Working directory.
/// * `label` - Driver binary label.
/// * `stderr` - Error stream.
fn apply_ebcdic_table_cli_flag(
    dialect_cfg: &mut DialectConfig,
    spec: &str,
    cwd: &Path,
    label: &str,
    stderr: &mut dyn Write,
) -> bool {
    if let Some(builtin) = coblang_diag::EbcdicTranslationTables::from_builtin_name(spec) {
        dialect_cfg.ebcdic_tables = builtin;
        return true;
    }
    let direct = cwd.join(spec);
    let with_ext = cwd.join(format!("{spec}.ttbl"));
    let path_to_read = if direct.is_file() {
        Some(direct)
    } else if with_ext.is_file() {
        Some(with_ext)
    } else {
        None
    };
    let Some(path) = path_to_read else {
        let _ = write!(
            stderr,
            "libcob: error: can't open translation table '{spec}'{LF}"
        );
        let _ = write!(
            stderr,
            "{label}: error: invalid parameter: -febcdic-table{LF}"
        );
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        let _ = write!(
            stderr,
            "libcob: error: can't open translation table '{spec}'{LF}"
        );
        let _ = write!(
            stderr,
            "{label}: error: invalid parameter: -febcdic-table{LF}"
        );
        return false;
    };
    match coblang_diag::EbcdicTranslationTables::parse_ttbl_text(&text, spec) {
        Ok(tbl) => {
            dialect_cfg.ebcdic_tables = tbl;
            true
        }
        Err(lines) => {
            for ln in lines {
                let _ = write!(stderr, "{ln}{LF}");
            }
            let _ = write!(
                stderr,
                "{label}: error: invalid parameter: -febcdic-table{LF}"
            );
            false
        }
    }
}

/// Applies a `-conf=<path_or_builtin>` option onto `dialect_cfg`.
///
/// # Arguments
///
/// * `dialect_cfg` - Mutable dialect configuration.
/// * `conf_rel` - Relative path or built-in `.conf` name.
/// * `cwd` - Working directory.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if the configuration file is missing or malformed.
fn apply_conf_flag(dialect_cfg: &mut DialectConfig, conf_rel: &str, cwd: &Path) -> Result<()> {
    let full_conf = cwd.join(conf_rel);
    if !full_conf.is_file()
        && let Some(builtin) = DialectConfig::builtin_from_conf_filename(conf_rel)
    {
        *dialect_cfg = DialectConfig::for_dialect(builtin);
        return Ok(());
    }
    let conf_text = std::fs::read_to_string(&full_conf)?;
    let conf_dir = full_conf.parent().unwrap_or(cwd).to_path_buf();
    dialect_cfg.load_conf_text(&conf_text, conf_rel, &mut |inc| {
        let inc_path = conf_dir.join(inc);
        if !inc_path.is_file() && DialectConfig::builtin_from_conf_filename(inc).is_some() {
            return Ok(String::new());
        }
        Ok(std::fs::read_to_string(inc_path)?)
    })
}

/// Parses `-DNAME` or `-DNAME=VALUE` into `(String, CdfValue)`.
///
/// # Arguments
///
/// * `raw` - Slice immediately following `-D`.
#[must_use]
fn parse_cli_define(raw: &str) -> (String, CdfValue) {
    let Some((name, val_str)) = raw.split_once('=') else {
        return (String::from(raw), CdfValue::DefinedUnit);
    };
    let val = if let Ok(n) = val_str.parse::<i128>() {
        CdfValue::Integer(n)
    } else {
        CdfValue::Text(String::from(val_str))
    };
    (String::from(name), val)
}

/// Maximum bytes read from a single COBOL source input (`16 MiB`).
const MAX_DRIVER_SOURCE_BYTES: u32 = 16 * 1024 * 1024;

/// Maximum files tracked in a single driver [`SourceMap`] (`512`).
const MAX_DRIVER_FILES: u32 = 512;

/// Maximum preprocessor expansion frames tracked in a single driver [`SourceMap`] (`4_096`).
const MAX_DRIVER_EXPANSIONS: u32 = 4_096;

/// Maximum distinct symbols interned per compilation invocation (`250_000`).
const MAX_DRIVER_SYMBOLS: u32 = 250_000;

/// Reads up to `max_bytes` from `path` without allocating unbounded buffers on oversized files.
///
/// # Arguments
///
/// * `path` - Filesystem path to open and read.
/// * `max_bytes` - Maximum allowed byte size.
///
/// # Errors
///
/// Returns [`CoblangError::Io`] on filesystem failure or [`CoblangError::InvalidDomainValue`] if
/// the file exceeds `max_bytes`.
pub fn read_bounded_source_bytes(path: &Path, max_bytes: u32) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let limit = u64::from(max_bytes).saturating_add(1);
    let mut buf = Vec::new();
    let _ = file.take(limit).read_to_end(&mut buf)?;
    if buf.len() > max_bytes as usize {
        return Err(CoblangError::InvalidDomainValue {
            domain_type: "SourceFile",
            value: buf.len().to_string(),
            reason: format!("source file exceeds byte limit {max_bytes}"),
        });
    }
    Ok(buf)
}

/// Executes `CompilationMode::PreprocessOnly` (`-E`) across all `parsed.inputs`.
///
/// # Arguments
///
/// * `parsed` - Parsed driver CLI options.
/// * `configs` - Resolved dialect, warning, listing, and CDF configurations.
/// * `env_vars` - Environment variable pairs.
/// * `cwd` - Working directory.
/// * `stdout` - Output stream.
/// * `stderr` - Error stream.
fn run_preprocess_mode(
    parsed: &ParsedDriverInvocation,
    configs: &ResolvedDriverConfigs,
    env_vars: &[(String, String)],
    cwd: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<u8> {
    let resolver = build_fs_resolver(parsed, env_vars, cwd);
    let pp_cfg = PreprocessConfig {
        initial_format: parsed.format,
        dialect_config: &configs.dialect_cfg,
        debug_policy: configs.debug_policy,
        initial_defines: &configs.defines,
        copybook_resolver: &resolver,
    };

    let mut source_map = SourceMap::with_limits(
        MAX_DRIVER_FILES,
        MAX_DRIVER_SOURCE_BYTES,
        MAX_DRIVER_EXPANSIONS,
    );
    let mut sink = DiagnosticSink::new(configs.warning_policy.clone(), 100);
    let mut combined_pp = String::new();

    for input in &parsed.inputs {
        let full_in = cwd.join(input);
        let raw_bytes = read_bounded_source_bytes(&full_in, MAX_DRIVER_SOURCE_BYTES)?;
        let src_text = String::from_utf8_lossy(&raw_bytes);
        let caller_dir = full_in.parent().unwrap_or(cwd);
        let unit = preprocess_source_file(
            input,
            caller_dir,
            &src_text,
            &pp_cfg,
            &mut source_map,
            &mut sink,
        )?;

        if let Some(req) = &configs.listing_req {
            let rendered_lst =
                render_compiler_listing(input, &unit.root_lines, &unit.tokens, &sink, req.options);
            std::fs::write(cwd.join(&req.path), rendered_lst)?;
        }
        combined_pp.push_str(&emit_preprocessed_source(input, &unit.tokens));
    }

    let diag_text = sink.render_all(&source_map)?;
    if !diag_text.is_empty() {
        let _ = stderr.write_all(diag_text.as_bytes());
    }
    if sink.has_errors() {
        return Ok(EXIT_INPUT_ERROR);
    }

    match parsed.output_path.as_deref() {
        None | Some("-") => {
            let _ = stdout.write_all(combined_pp.as_bytes());
        }
        Some(out_arg) => {
            let target = resolve_preprocess_output_path(cwd, out_arg, parsed.inputs.first());
            std::fs::write(&target, combined_pp.as_bytes())?;
        }
    }
    Ok(0)
}

/// Executes `CompilationMode::SyntaxOnly` (`-fsyntax-only`) across all `parsed.inputs`.
///
/// # Arguments
///
/// * `parsed` - Parsed driver CLI options.
/// * `configs` - Resolved dialect, warning, listing, and CDF configurations.
/// * `env_vars` - Environment variable pairs.
/// * `cwd` - Working directory.
/// * `stderr` - Error stream receiving compiler diagnostics.
fn run_syntax_only_mode(
    parsed: &ParsedDriverInvocation,
    configs: &ResolvedDriverConfigs,
    env_vars: &[(String, String)],
    cwd: &Path,
    stderr: &mut dyn Write,
) -> Result<u8> {
    let resolver = build_fs_resolver(parsed, env_vars, cwd);
    let pp_cfg = PreprocessConfig {
        initial_format: parsed.format,
        dialect_config: &configs.dialect_cfg,
        debug_policy: configs.debug_policy,
        initial_defines: &configs.defines,
        copybook_resolver: &resolver,
    };

    let mut source_map = SourceMap::with_limits(
        MAX_DRIVER_FILES,
        MAX_DRIVER_SOURCE_BYTES,
        MAX_DRIVER_EXPANSIONS,
    );
    let mut interner = SymbolInterner::with_limit(MAX_DRIVER_SYMBOLS);
    let mut sink = DiagnosticSink::new(configs.warning_policy.clone(), 100);

    for input in &parsed.inputs {
        let full_in = cwd.join(input);
        let raw_bytes = read_bounded_source_bytes(&full_in, MAX_DRIVER_SOURCE_BYTES)?;
        let src_text = String::from_utf8_lossy(&raw_bytes);
        let caller_dir = full_in.parent().unwrap_or(cwd);
        let unit = preprocess_source_file(
            input,
            caller_dir,
            &src_text,
            &pp_cfg,
            &mut source_map,
            &mut sink,
        )?;
        let group = parse_preprocessed_unit(&unit, &configs.dialect_cfg, &mut interner, &mut sink)?;
        let hir_result: Result<ResolvedHirModule> =
            lower_compilation_group(&group, &configs.dialect_cfg, &mut interner, &mut sink);
        let _ = hir_result;

        if let Some(req) = &configs.listing_req {
            let rendered_lst =
                render_compiler_listing(input, &unit.root_lines, &unit.tokens, &sink, req.options);
            std::fs::write(cwd.join(&req.path), rendered_lst)?;
        }
    }

    let diag_text = sink.render_all(&source_map)?;
    if !diag_text.is_empty() {
        let _ = stderr.write_all(diag_text.as_bytes());
    }
    if sink.has_errors() {
        Ok(EXIT_INPUT_ERROR)
    } else {
        Ok(0)
    }
}

/// Checks whether an input file extension represents a precompiled `.o` / `.a` / `.so` / `.dylib` link input.
///
/// # Arguments
///
/// * `input` - Input path string.
#[must_use]
fn is_prebuilt_link_input(input: &str) -> bool {
    Path::new(input)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| matches!(ext, "o" | "a" | "so" | "dylib"))
}

/// Checks whether an input file is a C source file (`.c`) passed to `cobc` / `coblang`.
///
/// # Arguments
///
/// * `input` - Input path string.
#[must_use]
fn is_c_source_input(input: &str) -> bool {
    Path::new(input)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("c"))
}

/// Compiles a companion `.c` source file into a native PIC `.o` object file using `cc`.
///
/// # Arguments
///
/// * `c_path` - Full path to the `.c` source file.
/// * `obj_out` - Destination `.o` object file path.
/// * `include_dirs` - Include directories (`-I`) relative to `cwd`.
/// * `cwd` - Working directory.
fn compile_c_companion_object(
    c_path: &Path,
    obj_out: &Path,
    include_dirs: &[String],
    cwd: &Path,
) -> Result<()> {
    let mut cmd = std::process::Command::new("cc");
    cmd.arg("-c")
        .arg("-fPIC")
        .arg("-I")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/coblang-rt-abi/include"
        ))
        .arg(c_path)
        .arg("-o")
        .arg(obj_out);
    for dir in include_dirs {
        cmd.arg("-I").arg(cwd.join(dir));
    }
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(CoblangError::LinkerInvocation {
            linker_bin: String::from("cc"),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Resolves output `.o` path, compiles a companion `.c` source file, and appends it to `link_objects`.
///
/// # Arguments
///
/// * `parsed` - Parsed driver CLI options.
/// * `input` - `.c` input path string.
/// * `input_idx` - 0-based input ordinal.
/// * `cwd` - Working directory.
/// * `link_objects` - Accumulated linker object inputs.
/// * `scratch_objects` - Accumulated temporary `.o` files to remove after linking.
fn stage_c_companion_input(
    parsed: &ParsedDriverInvocation,
    input: &str,
    input_idx: usize,
    cwd: &Path,
    link_objects: &mut Vec<PathBuf>,
    scratch_objects: &mut Vec<PathBuf>,
) -> Result<()> {
    let out_obj = if parsed.mode == CompilationMode::CompileObject {
        resolve_output_path(cwd, Path::new(input), parsed.output_path.as_deref(), "o")
    } else {
        let tmp = cwd.join(format!(
            "__coblang_tmp_c_{}_{input_idx}.o",
            std::process::id()
        ));
        scratch_objects.push(tmp.clone());
        tmp
    };
    compile_c_companion_object(&cwd.join(input), &out_obj, &parsed.include_dirs, cwd)?;
    link_objects.push(out_obj);
    Ok(())
}

/// Writes textual IR (`--emit-mlir`, `--emit-mlir-llvm`, `-emit-llvm`) to `-o` or `stdout`.
///
/// # Arguments
///
/// * `cwd` - Working directory.
/// * `primary_input` - Primary COBOL source file.
/// * `output_arg` - Optional `-o` CLI value.
/// * `default_ext` - Default extension when writing a file (`"mlir"` or `"ll"`).
/// * `text` - Rendered IR text.
/// * `stdout` - Output stream when `-o -` or no `-o` on `--emit-mlir*`.
fn write_textual_ir_output(
    cwd: &Path,
    primary_input: &str,
    output_arg: Option<&str>,
    default_ext: &str,
    text: &str,
    stdout: &mut dyn Write,
) -> Result<()> {
    match output_arg {
        Some("-") => {
            let _ = stdout.write_all(text.as_bytes());
        }
        None if default_ext == "mlir" => {
            let _ = stdout.write_all(text.as_bytes());
        }
        other => {
            let out_path = resolve_output_path(cwd, Path::new(primary_input), other, default_ext);
            std::fs::write(out_path, text.as_bytes())?;
        }
    }
    Ok(())
}

/// Runs preprocessor, parser, listing emission, and semantic HIR lowering for one source file.
///
/// # Arguments
///
/// * `input` - Source file path relative to `cwd`.
/// * `pp_cfg` - Preprocessor configuration.
/// * `configs` - Resolved driver configs.
/// * `cwd` - Working directory.
/// * `stderr` - Diagnostic output writer.
fn frontend_source_to_hir(
    input: &str,
    pp_cfg: &PreprocessConfig<'_>,
    configs: &ResolvedDriverConfigs,
    cwd: &Path,
    stderr: &mut dyn Write,
) -> Result<Option<(SourceMap, SymbolInterner, ResolvedHirModule)>> {
    let mut source_map = SourceMap::with_limits(
        MAX_DRIVER_FILES,
        MAX_DRIVER_SOURCE_BYTES,
        MAX_DRIVER_EXPANSIONS,
    );
    let mut interner = SymbolInterner::with_limit(MAX_DRIVER_SYMBOLS);
    let mut sink = DiagnosticSink::new(configs.warning_policy.clone(), 100);

    let full_in = cwd.join(input);
    let raw_bytes = read_bounded_source_bytes(&full_in, MAX_DRIVER_SOURCE_BYTES)?;
    let src_text = String::from_utf8_lossy(&raw_bytes);
    let caller_dir = full_in.parent().unwrap_or(cwd);

    let unit = preprocess_source_file(
        input,
        caller_dir,
        &src_text,
        pp_cfg,
        &mut source_map,
        &mut sink,
    )?;
    let group = parse_preprocessed_unit(&unit, &configs.dialect_cfg, &mut interner, &mut sink)?;
    let hir_res = lower_compilation_group(&group, &configs.dialect_cfg, &mut interner, &mut sink);

    if let Some(req) = &configs.listing_req {
        let rendered_lst =
            render_compiler_listing(input, &unit.root_lines, &unit.tokens, &sink, req.options);
        std::fs::write(cwd.join(&req.path), rendered_lst)?;
    }

    let diag_text = sink.render_all(&source_map)?;
    if !diag_text.is_empty() {
        let _ = stderr.write_all(diag_text.as_bytes());
    }
    if sink.has_errors() {
        return Ok(None);
    }
    Ok(Some((source_map, interner, hir_res?)))
}

/// Executes MLIR 20 -> LLVM 20 compilation and optional native linking (`-emit-llvm`, `-S`, `-c`, `-m`, `-x`).
///
/// # Arguments
///
/// * `parsed` - Parsed driver CLI options.
/// * `configs` - Resolved dialect, warning, listing, and CDF configurations.
/// * `env_vars` - Environment variable overrides.
/// * `cwd` - Working directory.
/// * `stdout` - Output stream.
/// * `stderr` - Error stream.
fn run_codegen_mode(
    parsed: &ParsedDriverInvocation,
    configs: &ResolvedDriverConfigs,
    env_vars: &[(String, String)],
    cwd: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<u8> {
    let resolver = build_fs_resolver(parsed, env_vars, cwd);
    let pp_cfg = PreprocessConfig {
        initial_format: parsed.format,
        dialect_config: &configs.dialect_cfg,
        debug_policy: configs.debug_policy,
        initial_defines: &configs.defines,
        copybook_resolver: &resolver,
    };

    let emit_cobol_mlir = parsed.extra_flags.iter().any(|f| f == "--emit-mlir");
    let emit_llvm_mlir = parsed.extra_flags.iter().any(|f| f == "--emit-mlir-llvm");
    let force_main = parsed.extra_flags.iter().any(|f| f == "-main");

    let mut link_objects: Vec<PathBuf> = Vec::new();
    let mut scratch_objects: Vec<PathBuf> = Vec::new();
    let mut cobol_idx: usize = 0;

    for (input_idx, input) in parsed.inputs.iter().enumerate() {
        if is_prebuilt_link_input(input) {
            link_objects.push(cwd.join(input));
            continue;
        }
        if is_c_source_input(input) {
            stage_c_companion_input(
                parsed,
                input,
                input_idx,
                cwd,
                &mut link_objects,
                &mut scratch_objects,
            )?;
            continue;
        }
        let Some((source_map, interner, resolved_hir)) =
            frontend_source_to_hir(input, &pp_cfg, configs, cwd, stderr)?
        else {
            return Ok(EXIT_INPUT_ERROR);
        };

        let mlir_ctx = CobolMlirContext::new();
        let verified_cobol =
            lower_hir_to_cobol_mlir(&mlir_ctx, &resolved_hir, &source_map, &interner)?;

        if emit_cobol_mlir {
            write_textual_ir_output(
                cwd,
                input,
                parsed.output_path.as_deref(),
                "mlir",
                &verified_cobol.to_mlir_text(),
                stdout,
            )?;
            return Ok(0);
        }

        let main_mode =
            if (parsed.mode == CompilationMode::BuildExecutable && cobol_idx == 0) || force_main {
                MainEntryMode::EmitMain
            } else {
                MainEntryMode::OmitMain
            };
        cobol_idx = cobol_idx.saturating_add(1);

        let lowered_llvm =
            lower_cobol_to_llvm_mlir(&mlir_ctx, &verified_cobol, parsed.optimization, main_mode)?;

        if emit_llvm_mlir {
            write_textual_ir_output(
                cwd,
                input,
                parsed.output_path.as_deref(),
                "mlir",
                &lowered_llvm.to_mlir_text(),
                stdout,
            )?;
            return Ok(0);
        }

        let llvm_mod = translate_to_llvm_module(&lowered_llvm)?;
        let triple = host_default_target_triple();
        emit_or_stage_llvm_artifact(
            parsed,
            cwd,
            input,
            cobol_idx,
            &llvm_mod,
            &triple,
            stdout,
            &mut link_objects,
            &mut scratch_objects,
        )?;
    }

    finalize_link_if_needed(parsed, env_vars, cwd, &link_objects, &scratch_objects)?;
    Ok(0)
}

/// Invokes the system linker for module/executable modes and cleans up temporary `.o` files.
///
/// # Arguments
///
/// * `parsed` - Parsed driver CLI options.
/// * `env_vars` - Ambient environment pairs.
/// * `cwd` - Working directory.
/// * `link_objects` - Accumulated linker object inputs.
/// * `scratch_objects` - Temporary `.o` files to remove unless `-save-temps` is active.
fn finalize_link_if_needed(
    parsed: &ParsedDriverInvocation,
    env_vars: &[(String, String)],
    cwd: &Path,
    link_objects: &[PathBuf],
    scratch_objects: &[PathBuf],
) -> Result<()> {
    if matches!(
        parsed.mode,
        CompilationMode::CompileModule | CompilationMode::BuildExecutable
    ) {
        let save_temps = parsed.extra_flags.iter().any(|f| f == "-save-temps");
        let link_res = link_compiled_objects(parsed, env_vars, cwd, link_objects);
        if !save_temps {
            for tmp_o in scratch_objects {
                let _ = std::fs::remove_file(tmp_o);
            }
        }
        link_res?;
    }
    Ok(())
}

/// Emits `.ll`, `.s`, `.o`, or stages a temporary `.o` for linking.
///
/// # Arguments
///
/// * `parsed` - Parsed driver CLI options.
/// * `cwd` - Working directory.
/// * `input` - Input file path.
/// * `cobol_idx` - 1-based COBOL unit ordinal.
/// * `llvm_mod` - Translated LLVM module.
/// * `triple` - Target triple.
/// * `stdout` - Output stream.
/// * `link_objects` - Accumulated linker object inputs.
/// * `scratch_objects` - Accumulated temporary `.o` files to remove after linking.
#[expect(
    clippy::too_many_arguments,
    reason = "Internal codegen artifact dispatch helper."
)]
fn emit_or_stage_llvm_artifact(
    parsed: &ParsedDriverInvocation,
    cwd: &Path,
    input: &str,
    cobol_idx: usize,
    llvm_mod: &coblang_codegen::OwnedLlvmModule,
    triple: &str,
    stdout: &mut dyn Write,
    link_objects: &mut Vec<PathBuf>,
    scratch_objects: &mut Vec<PathBuf>,
) -> Result<()> {
    match parsed.mode {
        CompilationMode::EmitLlvmIr => {
            let ll_text = emit_llvm_ir_text(llvm_mod);
            write_textual_ir_output(
                cwd,
                input,
                parsed.output_path.as_deref(),
                "ll",
                &ll_text,
                stdout,
            )?;
        }
        CompilationMode::EmitAssembly => {
            let asm_text = emit_target_assembly(llvm_mod, triple, parsed.optimization)?;
            let out_path =
                resolve_output_path(cwd, Path::new(input), parsed.output_path.as_deref(), "s");
            std::fs::write(out_path, asm_text.as_bytes())?;
        }
        CompilationMode::CompileObject => {
            let obj = emit_target_object(llvm_mod, triple, parsed.optimization)?;
            let out_path =
                resolve_output_path(cwd, Path::new(input), parsed.output_path.as_deref(), "o");
            std::fs::write(out_path, obj.as_bytes())?;
        }
        CompilationMode::CompileModule
        | CompilationMode::BuildExecutable
        | CompilationMode::PreprocessOnly
        | CompilationMode::SyntaxOnly => {
            let obj = emit_target_object(llvm_mod, triple, parsed.optimization)?;
            let tmp_obj = cwd.join(format!(
                "__coblang_tmp_{}_{cobol_idx}.o",
                std::process::id()
            ));
            std::fs::write(&tmp_obj, obj.as_bytes())?;
            link_objects.push(tmp_obj.clone());
            scratch_objects.push(tmp_obj);
        }
    }
    Ok(())
}

/// Resolves the directory containing `libcoblang_rt.{dylib,so}` using `env_vars` and workspace target paths.
///
/// # Arguments
///
/// * `env_vars` - Environment `(KEY, VALUE)` pairs (`COBLANG_RT_LIB_DIR`).
fn resolve_runtime_lib_dir(env_vars: &[(String, String)]) -> Result<PathBuf> {
    let rt_override = env_vars
        .iter()
        .find(|(k, _)| k == "COBLANG_RT_LIB_DIR")
        .map(|(_, v)| PathBuf::from(v))
        .or_else(|| std::env::var("COBLANG_RT_LIB_DIR").ok().map(PathBuf::from));
    if let Some(ov) = rt_override.as_deref() {
        return locate_runtime_lib_dir(Some(ov), None, &[]);
    }
    let exe_path = std::env::current_exe().ok();
    let workspace_target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_default(|root| {
            vec![
                root.join("target/debug"),
                root.join("target/release"),
                root.join("target/llvm-cov-target/debug"),
            ]
        });

    locate_runtime_lib_dir(None, exe_path.as_deref(), &workspace_target)
}

/// Invokes the system linker for `CompileModule` (`-m`) or `BuildExecutable` (`-x`).
///
/// # Arguments
///
/// * `parsed` - Parsed driver CLI options.
/// * `env_vars` - Environment overrides (`COBLANG_RT_LIB_DIR`).
/// * `cwd` - Working directory.
/// * `link_objects` - Object files to link.
fn link_compiled_objects(
    parsed: &ParsedDriverInvocation,
    env_vars: &[(String, String)],
    cwd: &Path,
    link_objects: &[PathBuf],
) -> Result<()> {
    let rt_dir = resolve_runtime_lib_dir(env_vars)?;

    let (kind, default_ext) = if parsed.mode == CompilationMode::CompileModule {
        (LinkArtifactKind::SharedModule, "so")
    } else {
        (LinkArtifactKind::Executable, "")
    };
    let primary_in = parsed.inputs.first().map_or("a.out", String::as_str);
    let output_path = resolve_output_path(
        cwd,
        Path::new(primary_in),
        parsed.output_path.as_deref(),
        default_ext,
    );
    let extra_lib_dirs = parsed.library_dirs.iter().map(|d| cwd.join(d)).collect();

    let spec = LinkerInvocationSpec {
        linker_bin: String::from("cc"),
        kind,
        object_inputs: link_objects.to_vec(),
        output_path,
        runtime_lib_dir: rt_dir,
        extra_lib_dirs,
        extra_libs: parsed.libraries.clone(),
    };
    invoke_system_linker(&spec)
}

/// Resolves `-o <file_or_dir>` for `-E` mode (supporting `-o .` -> `<dir>/<stem>.i`).
///
/// # Arguments
///
/// * `cwd` - Working directory.
/// * `out_arg` - Raw `-o` argument.
/// * `first_input` - First input path used to derive `<stem>.i` when `out_arg` is a directory.
#[must_use]
fn resolve_preprocess_output_path(
    cwd: &Path,
    out_arg: &str,
    first_input: Option<&String>,
) -> PathBuf {
    let candidate = cwd.join(out_arg);
    if candidate.is_dir() {
        let stem = first_input
            .and_then(|s| Path::new(s).file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("a");
        candidate.join(format!("{stem}.i"))
    } else {
        candidate
    }
}

/// Constructs a [`FilesystemCopybookResolver`] from parsed `-I`, `-copyext`, and `COB_COPY_*` env vars.
///
/// # Arguments
///
/// * `parsed` - Parsed CLI invocation.
/// * `env_vars` - Environment `(KEY, VALUE)` pairs.
/// * `cwd` - Working directory used to anchor relative `-I` directories.
#[must_use]
fn build_fs_resolver(
    parsed: &ParsedDriverInvocation,
    env_vars: &[(String, String)],
    cwd: &Path,
) -> FilesystemCopybookResolver {
    let include_dirs = parsed.include_dirs.iter().map(|d| cwd.join(d)).collect();
    let mut cob_copy_dir = None;
    let mut library_dirs = BTreeMap::new();
    for (k, v) in env_vars {
        if k == "COB_COPY_DIR" {
            cob_copy_dir = Some(v.clone());
        } else if let Some(lib_name) = k.strip_prefix("COB_COPY_LIB_") {
            library_dirs.insert(lib_name.to_ascii_uppercase(), cwd.join(v));
        }
    }
    FilesystemCopybookResolver {
        include_dirs,
        extra_extensions: parsed.copy_exts.clone(),
        cob_copy_dir,
        library_dirs,
    }
}

/// Parses `pre_load` and `library_path` directives from a `cobcrun` runtime `.cfg` file.
///
/// # Arguments
///
/// * `cfg_path` - Path to runtime configuration file.
/// * `cwd` - Working directory used to anchor relative paths.
/// * `preloads` - Output preload module list.
/// * `search_dirs` - Output module search directory list.
fn parse_runtime_cfg_file(
    cfg_path: &Path,
    cwd: &Path,
    preloads: &mut Vec<String>,
    search_dirs: &mut Vec<PathBuf>,
) {
    let Ok(content) = std::fs::read_to_string(cfg_path) else {
        return;
    };
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let normalized = line.replace('=', " ");
        let mut parts = normalized.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let rest: Vec<&str> = parts.collect();
        let val = rest.join(" ");
        let val = val.trim_matches('"').trim_matches('\'');
        if val.is_empty() {
            continue;
        }
        if key.eq_ignore_ascii_case("pre_load") || key.eq_ignore_ascii_case("cob_pre_load") {
            for item in val.split(':').filter(|s| !s.is_empty()) {
                preloads.push(String::from(item));
            }
        } else if key.eq_ignore_ascii_case("library_path")
            || key.eq_ignore_ascii_case("cob_library_path")
        {
            for item in val.split(':').filter(|s| !s.is_empty()) {
                search_dirs.push(cwd.join(item));
            }
        }
    }
}

/// Applies `-M <module_or_dir>` to `preloads` and `search_dirs`, returning `false` on empty `""`.
///
/// # Arguments
///
/// * `val` - Raw `-M` option value.
/// * `cwd` - Working directory.
/// * `preloads` - Preload list.
/// * `search_dirs` - Search directory list.
fn apply_cobcrun_module_arg(
    val: &str,
    cwd: &Path,
    preloads: &mut Vec<String>,
    search_dirs: &mut Vec<PathBuf>,
) -> bool {
    if val.is_empty() {
        return false;
    }
    let resolved = cwd.join(val);
    if resolved.is_dir() {
        search_dirs.push(resolved);
    } else {
        search_dirs.push(resolved.parent().unwrap_or(cwd).to_path_buf());
        preloads.push(resolved.to_string_lossy().into_owned());
    }
    true
}

/// Parsed `cobcrun` configuration ready for dynamic execution.
struct CobcrunReadyPlan {
    /// Modules/DSOs to preload with `RTLD_GLOBAL`.
    preloads: Vec<String>,
    /// Ordered module search directories.
    search_dirs: Vec<PathBuf>,
    /// Primary target program or DSO name.
    target: String,
    /// Trailing program arguments.
    trailing_args: Vec<String>,
}

/// Result of parsing `cobcrun` CLI flags and environment variables.
enum CobcrunParseOutcome {
    /// Immediate short-circuit exit code (`0` for `--version`/`--help`, `1` for CLI error).
    ShortCircuit(u8),
    /// Parsed target and search configuration ready to execute.
    Ready(CobcrunReadyPlan),
}

/// Loads initial `COB_PRE_LOAD` and `COB_RUNTIME_CONFIG` settings from `env_vars`.
///
/// # Arguments
///
/// * `env_vars` - Environment `(KEY, VALUE)` slice.
/// * `cwd` - Working directory.
/// * `preloads` - Mutable preload accumulator.
/// * `search_dirs` - Mutable search directory accumulator.
fn load_cobcrun_env_configs(
    env_vars: &[(String, String)],
    cwd: &Path,
    preloads: &mut Vec<String>,
    search_dirs: &mut Vec<PathBuf>,
) {
    let mut cfg_dir_env: Option<PathBuf> = None;
    let mut rt_cfg_env: Option<String> = None;
    for (k, v) in env_vars {
        match k.as_str() {
            "COB_CONFIG_DIR" => cfg_dir_env = Some(cwd.join(v)),
            "COB_RUNTIME_CONFIG" => rt_cfg_env = Some(v.clone()),
            "COB_PRE_LOAD" => {
                for entry in v.split(':').filter(|s| !s.is_empty()) {
                    preloads.push(String::from(entry));
                }
            }
            _ => {}
        }
    }
    if let Some(cfg_name) = rt_cfg_env {
        let p = Path::new(&cfg_name);
        let full = if p.is_absolute() || cwd.join(p).is_file() {
            cwd.join(p)
        } else if let Some(dir) = &cfg_dir_env {
            dir.join(p)
        } else {
            cwd.join(p)
        };
        parse_runtime_cfg_file(&full, cwd, preloads, search_dirs);
    }
}

/// Parses `cobcrun` CLI flags (`-M`, `-c`, `--version`, `--help`, `--runtime-conf`) and target.
///
/// # Arguments
///
/// * `args` - CLI arguments excluding `argv[0]`.
/// * `env_vars` - Environment `(KEY, VALUE)` slice.
/// * `cwd` - Working directory.
/// * `stdout` - Output stream for banners.
/// * `stderr` - Error stream for option diagnostics.
fn parse_cobcrun_options<W1: Write, W2: Write>(
    args: &[String],
    env_vars: &[(String, String)],
    cwd: &Path,
    stdout: &mut W1,
    stderr: &mut W2,
) -> CobcrunParseOutcome {
    let mut preloads: Vec<String> = Vec::new();
    let mut search_dirs: Vec<PathBuf> = Vec::new();
    load_cobcrun_env_configs(env_vars, cwd, &mut preloads, &mut search_dirs);

    let mut idx = 0_usize;
    let mut raw_target: Option<String> = None;
    let mut trailing_args: Vec<String> = Vec::new();
    while let Some(arg) = args.get(idx).map(String::as_str) {
        match arg {
            "--version" | "-V" => {
                let _ = write!(stdout, "cobcrun (coblang) 0.1.0{LF}");
                return CobcrunParseOutcome::ShortCircuit(0);
            }
            "--help" | "-h" => {
                let _ = write!(
                    stdout,
                    "Usage: cobcrun [-M <module>] [-c <file>] [--runtime-conf] PROGRAM [args...]{LF}"
                );
                return CobcrunParseOutcome::ShortCircuit(0);
            }
            "--info" | "-i" | "--runtime-conf" | "-r" => {
                let _ = write!(stdout, "coblang runtime configuration: active{LF}");
                return CobcrunParseOutcome::ShortCircuit(0);
            }
            "-M" | "--module" => {
                idx += 1;
                let Some(val) = args.get(idx) else {
                    let _ = write!(stderr, "cobcrun: option requires an argument -- 'M'{LF}");
                    return CobcrunParseOutcome::ShortCircuit(1);
                };
                if !apply_cobcrun_module_arg(val, cwd, &mut preloads, &mut search_dirs) {
                    let _ = write!(stderr, "invalid module argument ''{LF}");
                    return CobcrunParseOutcome::ShortCircuit(1);
                }
            }
            "-c" | "--config" => {
                idx += 1;
                let Some(val) = args.get(idx) else {
                    let _ = write!(stderr, "cobcrun: option requires an argument -- 'c'{LF}");
                    return CobcrunParseOutcome::ShortCircuit(1);
                };
                parse_runtime_cfg_file(&cwd.join(val), cwd, &mut preloads, &mut search_dirs);
            }
            _ => {
                if let Some(val) = arg
                    .strip_prefix("-M")
                    .or_else(|| arg.strip_prefix("--module="))
                {
                    if !apply_cobcrun_module_arg(val, cwd, &mut preloads, &mut search_dirs) {
                        let _ = write!(stderr, "invalid module argument ''{LF}");
                        return CobcrunParseOutcome::ShortCircuit(1);
                    }
                } else if let Some(val) = arg.strip_prefix("--config=") {
                    parse_runtime_cfg_file(&cwd.join(val), cwd, &mut preloads, &mut search_dirs);
                } else if arg.starts_with('-') {
                    let _ = write!(stderr, "cobcrun: unrecognized option '{arg}'{LF}");
                    return CobcrunParseOutcome::ShortCircuit(1);
                } else {
                    raw_target = Some(String::from(arg));
                    trailing_args = args.get(idx + 1..).unwrap_or(&[]).to_vec();
                    break;
                }
            }
        }
        idx += 1;
    }

    let Some(target) = raw_target else {
        let _ = write!(
            stderr,
            "cobcrun: missing PROGRAM name{LF}Try 'cobcrun --help' for more information.{LF}"
        );
        return CobcrunParseOutcome::ShortCircuit(1);
    };
    CobcrunParseOutcome::Ready(CobcrunReadyPlan {
        preloads,
        search_dirs,
        target,
        trailing_args,
    })
}

/// Returns `true` when `name` has a `.cfg` or `.conf` extension (case-insensitive).
///
/// # Arguments
///
/// * `name` - Candidate filename string.
#[must_use]
fn has_runtime_cfg_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cfg") || ext.eq_ignore_ascii_case("conf"))
}

/// Executes a real `cobcrun` dynamic COBOL module invocation via `coblang-dynload` and `libcoblang_rt`.
///
/// # Arguments
///
/// * `args` - Command-line arguments excluding `argv[0]`.
/// * `env_vars` - Environment `(KEY, VALUE)` pairs (`COB_LIBRARY_PATH`, `COB_PRE_LOAD`, `COB_RUNTIME_CONFIG`).
/// * `cwd` - Working directory for relative module/config resolution.
/// * `stdout` - Output stream receiving `--version`, `--help`, or `--runtime-conf` banners.
/// * `stderr` - Error stream receiving CLI or dynamic-loader diagnostics.
///
/// # Returns
///
/// Process exit status (`0`..=`255`).
pub fn execute_cobcrun_driver<W1: Write, W2: Write>(
    args: &[String],
    env_vars: &[(String, String)],
    cwd: &Path,
    stdout: &mut W1,
    stderr: &mut W2,
) -> u8 {
    let mut plan = match parse_cobcrun_options(args, env_vars, cwd, stdout, stderr) {
        CobcrunParseOutcome::ShortCircuit(code) => return code,
        CobcrunParseOutcome::Ready(ready) => ready,
    };

    if (plan.target.ends_with('/') || cwd.join(&plan.target).is_dir())
        && let Some((first, rest)) = plan.trailing_args.split_first()
    {
        plan.search_dirs.push(cwd.join(&plan.target));
        plan.target.clone_from(first);
        plan.trailing_args = rest.to_vec();
    } else if has_runtime_cfg_extension(&plan.target)
        && cwd.join(&plan.target).is_file()
        && let Some((first, rest)) = plan.trailing_args.split_first()
    {
        parse_runtime_cfg_file(
            &cwd.join(&plan.target),
            cwd,
            &mut plan.preloads,
            &mut plan.search_dirs,
        );
        plan.target.clone_from(first);
        plan.trailing_args = rest.to_vec();
    }

    if plan
        .trailing_args
        .first()
        .is_some_and(|a| matches!(a.as_str(), "--runtime-conf" | "-r"))
    {
        let _ = write!(stdout, "coblang runtime configuration: active{LF}");
        return 0;
    }

    if !plan.target.contains('/') && !plan.target.contains('.') && plan.target.len() > 31 {
        if let Some((first, rest)) = plan.trailing_args.split_first() {
            let next_target = first.clone();
            let next_args = rest.to_vec();
            plan.preloads.push(plan.target);
            plan.target = next_target;
            plan.trailing_args = next_args;
        } else {
            let _ = write!(
                stderr,
                "libcob: error: {}: PROGRAM name exceeds 31 characters{LF}",
                plan.target
            );
            return 1;
        }
    }

    for (k, v) in env_vars {
        if k == "COB_LIBRARY_PATH" {
            for entry in v.split(':').filter(|s| !s.is_empty()) {
                plan.search_dirs.push(cwd.join(entry));
            }
        }
    }
    plan.search_dirs.push(cwd.to_path_buf());

    let Ok(rt_dir) = resolve_runtime_lib_dir(env_vars) else {
        let _ = write!(stderr, "libcob: error: runtime library not found{LF}");
        return 1;
    };

    let exec_target = if plan.target.contains('/') && !Path::new(&plan.target).is_absolute() {
        cwd.join(&plan.target).to_string_lossy().into_owned()
    } else {
        plan.target.clone()
    };

    if let Ok((code, failed_preloads)) = execute_cobcrun_session(&CobcrunExecSpec {
        runtime_lib_dir: &rt_dir,
        preloads: &plan.preloads,
        search_dirs: &plan.search_dirs,
        target: &exec_target,
        args: &plan.trailing_args,
    }) {
        for fp in failed_preloads {
            let _ = write!(stderr, "libcob: warning: preloading of '{fp}' failed{LF}");
        }
        code
    } else {
        for fp in &plan.preloads {
            if coblang_dynload::preload_module_by_name(fp, &plan.search_dirs).is_err() {
                let _ = write!(stderr, "libcob: warning: preloading of '{fp}' failed{LF}");
            }
        }
        let _ = write!(
            stderr,
            "libcob: error: module '{}' not found{LF}",
            plan.target
        );
        1
    }
}
