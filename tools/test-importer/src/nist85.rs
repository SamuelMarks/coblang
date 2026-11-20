//! Native NIST CCVS-85 (`newcob.val`) extractor, `EXEC85` preprocessor, and 12-module importer.

use std::collections::BTreeMap;
use std::path::Path;

use coblang_common::{
    Capability, CoblangError, CompilationMode, CompileCobolSpec, Dialect, NistModule,
    OptimizationLevel, OutputMatcher, Provenance, Result, RunBinarySpec, SourceFormat, SuiteId,
    TestManifest, TestStep, TextEncoding, WorkspaceFile,
};

use crate::taxonomy::infer_feature_tags;

/// Pinned SHA-256 of `newcob.val.tar.gz` recorded in [`Provenance`].
pub const NIST_ARCHIVE_SHA256: &str =
    "e4513f26a9b38911f7bf882fe3d3339a80b45cabc2caf85eb3055b0f5ce87ee0";

/// Parsed `EXEC85.conf.in` control-card and sequence-update configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec85Config {
    /// Whether unselected optional-switch lines in col 7 are deleted (`*KILL-DELETIONS`).
    pub kill_deletions: bool,
    /// `X-NNN` substitution strings keyed by 1-based card number (`1..=200`).
    pub x_cards: BTreeMap<u16, String>,
    /// Per-program sequence updates (`prog_id -> (6-char seq -> 80-char line)`).
    pub updates: BTreeMap<String, BTreeMap<String, String>>,
}

/// Reference subtest expectation row parsed from `GnuCOBOL`'s `tests/cobol85/<MOD>.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NistBaselineEntry {
    /// Program filename in the module directory (e.g., `"NC101A.CBL"`).
    pub filename: String,
    /// Total subtests expected.
    pub total: u32,
    /// Expected `PASS` count.
    pub pass: u32,
    /// Expected `DELETED` count.
    pub deleted: u32,
    /// Expected `INSPECT` count.
    pub inspect: u32,
}

/// Extracted and `EXEC85`-preprocessed files from `newcob.val`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedNistArchive {
    /// Shared copybooks (`copy/<NAME>` and `copyalt/ALTLB`).
    pub copybooks: Vec<WorkspaceFile>,
    /// Preprocessed main/subprogram sources keyed by `"NC101A.CBL"` or `"IC101SUB.SUB"` -> `(start_line, source_text)`.
    pub programs: BTreeMap<String, (u32, String)>,
    /// Companion `.SUB` files belonging to a parent main program (`"IC101A" -> Vec<(sub_filename, text)>`).
    pub attached_subs: BTreeMap<String, Vec<(String, String)>>,
    /// Module-shared `SUBRTN` dynamic libraries (`NistModule -> Vec<(filename, text)>`).
    pub module_libs: BTreeMap<NistModule, Vec<(String, String)>>,
    /// Input `.DAT` payloads keyed by program stem (`"SQ101M" -> bytes`).
    pub data_files: BTreeMap<String, Vec<u8>>,
}

/// Parses an `EXEC85.conf.in` text into an [`Exec85Config`].
///
/// # Arguments
///
/// * `conf_text` - Contents of `EXEC85.conf.in`.
#[must_use]
pub fn parse_exec85_config(conf_text: &str) -> Exec85Config {
    let mut x_cards = BTreeMap::new();
    x_cards.insert(84, String::from("    OMITTED"));

    let mut kill_deletions = false;
    let mut in_updates = false;
    let mut active_prog: Option<String> = None;
    let mut updates: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    for raw in conf_text.lines() {
        let line = raw.trim_end_matches('\u{000d}');
        if line.starts_with("*KILL-DELETIONS") {
            kill_deletions = true;
        } else if line.starts_with("*BEGIN-UPDATE") {
            in_updates = true;
        } else if line.starts_with("*END-UPDATE") {
            in_updates = false;
            active_prog = None;
        } else if in_updates {
            if let Some(rest) = line.strip_prefix("*START ") {
                let p = rest.trim();
                if !p.is_empty() {
                    active_prog = Some(String::from(p));
                }
            } else if line.len() >= 6
                && line[..6].bytes().all(|b| b.is_ascii_digit())
                && let Some(prog) = &active_prog
            {
                updates
                    .entry(prog.clone())
                    .or_default()
                    .insert(String::from(&line[..6]), String::from(line));
            }
        } else if let Some(rest) = line.strip_prefix("X-")
            && rest.len() >= 6
            && let Ok(card_no) = rest[..3].parse::<u16>()
        {
            let mut val = String::from(rest[6..].trim_end());
            if card_no == 48 && val == r#""../copyalt""# {
                val = String::from(r#""copyalt""#);
            }
            x_cards.insert(card_no, val);
        }
    }

    Exec85Config {
        kill_deletions,
        x_cards,
        updates,
    }
}

/// Applies `EXEC85` sequence updates, col-7 optional-letter filtering, and `XXXX` substitutions.
///
/// # Arguments
///
/// * `raw_lines` - Raw 80-column COBOL lines between `*HEADER` and `*END-OF`.
/// * `prog_id` - Six-character NIST program identifier (e.g., `"NC101A"`).
/// * `cfg` - Parsed [`Exec85Config`].
#[must_use]
pub fn preprocess_exec85_lines(raw_lines: &[String], prog_id: &str, cfg: &Exec85Config) -> String {
    let prog_updates = cfg.updates.get(prog_id);
    let mut merged: Vec<String> = Vec::with_capacity(raw_lines.len() + 4);
    let mut pending: BTreeMap<String, String> = prog_updates.cloned().unwrap_or_default();

    for line in raw_lines {
        if line.len() >= 6 && line[..6].bytes().all(|b| b.is_ascii_digit()) {
            let seq = &line[..6];
            let older: Vec<(String, String)> = pending
                .range(..seq.to_string())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (k, ins) in older {
                pending.remove(&k);
                merged.push(ins);
            }
            if let Some(repl) = pending.remove(seq) {
                merged.push(repl);
                continue;
            }
        }
        merged.push(line.clone());
    }
    for (_, tail_ins) in pending {
        merged.push(tail_ins);
    }

    let mut out = String::with_capacity(merged.len() * 80);
    for line in &merged {
        if let Some(transformed) = transform_single_exec85_line(line, cfg) {
            out.push_str(&transformed);
            out.push('\u{000a}');
        }
    }
    out
}

/// Transforms a single 80-column NIST source card according to `EXEC85` col-7 and `XXXX` rules.
///
/// # Arguments
///
/// * `line` - Single COBOL card line.
/// * `cfg` - Parsed [`Exec85Config`].
#[must_use]
fn transform_single_exec85_line(line: &str, cfg: &Exec85Config) -> Option<String> {
    let mut chars: Vec<char> = line.chars().collect();
    let Some(&col7) = chars.get(6) else {
        return Some(String::from(line));
    };

    if col7.is_ascii_alphabetic() && col7 != 'D' {
        if matches!(col7, 'A' | 'E' | 'H' | 'L' | 'Y' | 'T') {
            chars.get_mut(6).into_iter().for_each(|c| *c = ' ');
        } else if cfg.kill_deletions {
            return None;
        } else {
            if let Some(col8) = chars.get_mut(7) {
                *col8 = col7;
            }
            chars.get_mut(6).into_iter().for_each(|c| *c = '*');
            return Some(chars.into_iter().collect());
        }
    }

    if chars.len() >= 19 {
        let tag_12_15: String = chars.get(11..15).unwrap_or(&[]).iter().collect();
        let num_17_19: String = chars.get(16..19).unwrap_or(&[]).iter().collect();
        if tag_12_15 == "XXXX"
            && let Ok(card_no) = num_17_19.parse::<u16>()
            && let Some(raw_sub) = cfg.x_cards.get(&card_no)
        {
            let wants_dot = chars.get(19) == Some(&'.');
            let mut sub = raw_sub.trim_end().to_string();
            if wants_dot && !sub.ends_with('.') {
                sub.push('.');
            } else if !wants_dot && sub.ends_with('.') {
                sub.pop();
            }
            let prefix: String = chars.get(..11).unwrap_or(&[]).iter().collect();
            let suffix: String = chars.get(72..).unwrap_or(&[]).iter().collect();
            return Some(format!("{prefix}{sub:<61}{suffix}"));
        }
    }

    Some(chars.into_iter().collect())
}

/// Parses a `GnuCOBOL` `tests/cobol85/<MOD>.txt` baseline file into [`NistBaselineEntry`] rows.
///
/// # Arguments
///
/// * `text` - Full UTF-8 contents of `<MOD>.txt`.
/// * `module_file` - Filename used for error diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if a baseline row has non-numeric subtest counts.
pub fn parse_module_baseline(text: &str, module_file: &str) -> Result<Vec<NistBaselineEntry>> {
    let mut rows = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if !trimmed.ends_with(" OK") {
            continue;
        }
        let cols: Vec<&str> = trimmed.split_whitespace().collect();
        let [filename, total_s, pass_s, _, deleted_s, inspect_s, _] = cols.as_slice() else {
            return Err(CoblangError::ImportParse {
                source_file: String::from(module_file),
                line: idx + 1,
                message: format!("expected 7 columns in baseline row `{trimmed}`"),
            });
        };
        let parse_col = |s: &str| -> Result<u32> {
            s.parse::<u32>().map_err(|_| CoblangError::ImportParse {
                source_file: String::from(module_file),
                line: idx + 1,
                message: format!("invalid count `{s}` in `{trimmed}`"),
            })
        };
        rows.push(NistBaselineEntry {
            filename: String::from(*filename),
            total: parse_col(total_s)?,
            pass: parse_col(pass_s)?,
            deleted: parse_col(deleted_s)?,
            inspect: parse_col(inspect_s)?,
        });
    }
    Ok(rows)
}

/// Extracts and preprocesses all records from `newcob.val`.
///
/// # Arguments
///
/// * `newcob_val_path` - Path to `newcob.val` on disk.
/// * `cfg` - Parsed [`Exec85Config`].
///
/// # Errors
///
/// Returns [`CoblangError::Io`] or [`CoblangError::ImportParse`] on malformed archive headers.
pub fn extract_newcob_archive(
    newcob_val_path: &Path,
    cfg: &Exec85Config,
) -> Result<ExtractedNistArchive> {
    let raw_bytes = std::fs::read(newcob_val_path)?;
    let text: String = raw_bytes.iter().map(|&b| char::from(b)).collect();
    extract_newcob_from_str(&text, cfg)
}

/// Extracts and preprocesses all records from an in-memory `newcob.val` string.
///
/// # Arguments
///
/// * `text` - Decoded Latin-1 / ASCII contents of `newcob.val`.
/// * `cfg` - Parsed [`Exec85Config`].
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if no `*HEADER` sections exist.
pub fn extract_newcob_from_str(text: &str, cfg: &Exec85Config) -> Result<ExtractedNistArchive> {
    let mut archive = ExtractedNistArchive {
        copybooks: Vec::new(),
        programs: BTreeMap::new(),
        attached_subs: BTreeMap::new(),
        module_libs: BTreeMap::new(),
        data_files: BTreeMap::new(),
    };
    let mut seen_copybooks = std::collections::BTreeSet::new();
    let all_lines: Vec<&str> = text.lines().collect();
    let mut idx = 0;
    let mut found_any = false;

    while let Some(&raw_line) = all_lines.get(idx) {
        let line = raw_line.trim_end_matches('\u{000d}');
        if let Some(hdr) = line.strip_prefix("*HEADER,") {
            found_any = true;
            let hdr_line = u32::try_from(idx + 1).unwrap_or(1);
            idx += 1;
            let mut body = Vec::new();
            while let Some(&raw_cur) = all_lines.get(idx) {
                let cur = raw_cur.trim_end_matches('\u{000d}');
                if cur.starts_with("*END") || cur.starts_with("*HEADER,") {
                    break;
                }
                body.push(String::from(cur));
                idx += 1;
            }
            store_nist_section(&mut archive, &mut seen_copybooks, hdr, hdr_line, &body, cfg);
            continue;
        }
        idx += 1;
    }

    if !found_any {
        return Err(CoblangError::ImportParse {
            source_file: String::from("newcob.val"),
            line: 1,
            message: String::from("no *HEADER records found in newcob.val"),
        });
    }
    Ok(archive)
}

/// Stores one `*HEADER` block into [`ExtractedNistArchive`].
///
/// # Arguments
///
/// * `arch` - Mutable archive accumulator.
/// * `seen_copy` - Set of already-materialized copybook paths.
/// * `hdr` - Comma-separated header fields following `*HEADER,`.
/// * `hdr_line` - 1-based starting line number in `newcob.val`.
/// * `body` - Raw 80-column body lines.
/// * `cfg` - `EXEC85` configuration.
fn store_nist_section(
    arch: &mut ExtractedNistArchive,
    seen_copy: &mut std::collections::BTreeSet<String>,
    hdr: &str,
    hdr_line: u32,
    body: &[String],
    cfg: &Exec85Config,
) {
    let fields: Vec<&str> = hdr
        .split(',')
        .map(|f| f.split_whitespace().next().unwrap_or(""))
        .collect();
    let kind = fields.first().copied().unwrap_or("");
    let prog = fields.get(1).copied().unwrap_or("");
    let sub_kind = fields.get(2).copied().unwrap_or("");
    let sub_name = fields.get(3).copied().unwrap_or("");

    if prog == "EXEC85" || prog.is_empty() {
        return;
    }

    if kind == "CLBRY" {
        let rel = if prog == "ALTL1" {
            String::from("copyalt/ALTLB")
        } else {
            format!("copy/{prog}")
        };
        if seen_copy.insert(rel.clone()) {
            let processed = preprocess_exec85_lines(body, prog, cfg);
            arch.copybooks.push(WorkspaceFile {
                relative_path: rel,
                contents: processed.into_bytes(),
            });
        }
    } else if kind == "DATA*" {
        let joined = format!("{}\u{000a}", body.join("\u{000a}"));
        arch.data_files
            .insert(String::from(prog), joined.into_bytes());
    } else if kind == "COBOL" {
        if sub_kind == "SUBRTN" {
            if let Some(code) = prog.get(..2)
                && let Ok(m) = NistModule::from_code(code, hdr_line as usize)
            {
                let processed = preprocess_exec85_lines(body, sub_name, cfg);
                arch.module_libs
                    .entry(m)
                    .or_default()
                    .push((format!("{sub_name}.CBL"), processed));
            }
        } else if sub_kind == "SUBPRG" {
            let processed = preprocess_exec85_lines(body, sub_name, cfg);
            let fname = format!("{sub_name}.SUB");
            arch.programs
                .insert(fname.clone(), (hdr_line, processed.clone()));
            arch.attached_subs
                .entry(String::from(prog))
                .or_default()
                .push((fname, processed));
        } else {
            let processed = preprocess_exec85_lines(body, prog, cfg);
            arch.programs
                .insert(format!("{prog}.CBL"), (hdr_line, processed));
        }
    }
}

/// Imports all 12 NIST CCVS-85 validation modules (`NC`..`DB`) into [`TestManifest`] bundles.
///
/// # Arguments
///
/// * `newcob_val_path` - Path to `.upstream-cache/nist85/newcob.val`.
/// * `cobol85_dir` - Path to `.upstream-cache/gnucobol/tests/cobol85/` (`EXEC85.conf.in` + `*.txt`).
///
/// # Errors
///
/// Propagates I/O, `newcob.val`, or baseline `.txt` parse errors.
pub fn import_nist85_suite(
    newcob_val_path: &Path,
    cobol85_dir: &Path,
) -> Result<BTreeMap<NistModule, Vec<TestManifest>>> {
    let conf_text = std::fs::read_to_string(cobol85_dir.join("EXEC85.conf.in"))?;
    let cfg = parse_exec85_config(&conf_text);
    let archive = extract_newcob_archive(newcob_val_path, &cfg)?;
    build_nist_manifests_from_archive(&archive, cobol85_dir)
}

/// Builds per-module [`TestManifest`] vectors from an [`ExtractedNistArchive`] and `<MOD>.txt` baselines.
///
/// # Arguments
///
/// * `archive` - Extracted NIST programs, copybooks, libs, and data files.
/// * `cobol85_dir` - Directory containing `<MOD>.txt` expected subtest tables.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if a program listed in `<MOD>.txt` is missing from `newcob.val`.
pub fn build_nist_manifests_from_archive(
    archive: &ExtractedNistArchive,
    cobol85_dir: &Path,
) -> Result<BTreeMap<NistModule, Vec<TestManifest>>> {
    let mut by_module = BTreeMap::new();
    for &module in NistModule::ALL {
        let base_name = format!("{}.txt", module.code());
        let base_text = std::fs::read_to_string(cobol85_dir.join(&base_name))?;
        let rows = parse_module_baseline(&base_text, &base_name)?;
        let mut manifests = Vec::with_capacity(rows.len());
        let mut seen_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut prev_cumulative: BTreeMap<String, NistBaselineEntry> = BTreeMap::new();

        for row in rows {
            let occ = seen_counts.entry(row.filename.clone()).or_insert(0);
            *occ += 1;
            let effective_row = if let Some(prev) = prev_cumulative.get(&row.filename) {
                NistBaselineEntry {
                    filename: row.filename.clone(),
                    total: row.total.saturating_sub(prev.total),
                    pass: row.pass.saturating_sub(prev.pass),
                    deleted: row.deleted.saturating_sub(prev.deleted),
                    inspect: row.inspect.saturating_sub(prev.inspect),
                }
            } else {
                row.clone()
            };
            prev_cumulative.insert(row.filename.clone(), row);
            let manifest = build_single_nist_manifest(module, &effective_row, *occ, archive)?;
            manifests.push(manifest);
        }
        by_module.insert(module, manifests);
    }
    Ok(by_module)
}

/// Constructs a single NIST CCVS-85 [`TestManifest`] for a row in `<MOD>.txt`.
///
/// # Arguments
///
/// * `module` - Owning [`NistModule`].
/// * `row` - Expected `(total, pass, deleted, inspect)` baseline row.
/// * `occurrence` - `1` for normal run, `2` for `DB103M`'s second `COB_SET_DEBUG=N` run.
/// * `archive` - Extracted `newcob.val` archive.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if `row.filename` was not found in `archive.programs`.
fn build_single_nist_manifest(
    module: NistModule,
    row: &NistBaselineEntry,
    occurrence: usize,
    archive: &ExtractedNistArchive,
) -> Result<TestManifest> {
    let Some((src_line, src_text)) = archive.programs.get(&row.filename) else {
        return Err(CoblangError::ImportParse {
            source_file: String::from("newcob.val"),
            line: 1,
            message: format!("NIST program `{}` missing from newcob.val", row.filename),
        });
    };

    let stem = row
        .filename
        .split('.')
        .next()
        .unwrap_or(row.filename.as_str());
    let id = if occurrence > 1 {
        format!("nist85/{}/{stem}_run{occurrence}", module.code())
    } else {
        format!("nist85/{}/{stem}", module.code())
    };

    let mut files = Vec::new();
    if module == NistModule::Sm {
        files.extend(archive.copybooks.iter().cloned());
    }
    files.push(WorkspaceFile {
        relative_path: row.filename.clone(),
        contents: src_text.as_bytes().to_vec(),
    });

    let mut steps = Vec::new();
    append_nist_companion_modules(module, stem, archive, &mut files, &mut steps);

    let extra_flags = nist_program_extra_flags(stem);
    let comp_only = row.total == 0;
    let mode = if comp_only {
        CompilationMode::SyntaxOnly
    } else {
        CompilationMode::BuildExecutable
    };
    let out_bin = if comp_only { "" } else { stem };
    steps.push(build_nist_compile_step(
        mode,
        &row.filename,
        out_bin,
        module,
        &extra_flags,
    ));

    if !comp_only {
        steps.push(build_nist_run_step(module, stem, row, occurrence, archive));
    }

    let mut caps = Vec::new();
    if module == NistModule::Ix || stem == "DB203A" {
        caps.push(Capability::Isam);
    }

    let tags = infer_feature_tags(
        &src_text.to_uppercase(),
        mode,
        Dialect::Cobol85,
        &TextEncoding::Ascii,
    );

    Ok(TestManifest {
        id,
        suite: SuiteId::Nist85,
        provenance: Provenance {
            upstream_repo: String::from("NIST-CCVS-85/newcob.val"),
            upstream_commit: String::from(NIST_ARCHIVE_SHA256),
            source_file: format!("newcob.val:{}", row.filename),
            source_line: *src_line,
            original_test_name: String::from(stem),
            spdx_license: String::from("NIST-PD"),
        },
        files,
        steps,
        torture_optimizations: Vec::new(),
        required_capabilities: caps,
        xfail_reason: if stem == "OBNC1M" {
            Some(String::from("STOP literal interactive operator kill"))
        } else {
            None
        },
        skip_reason: None,
        feature_tags: tags,
        equivalent_tests: Vec::new(),
    })
}

/// Appends module-level `SUBRTN` libraries and program-attached `SUBPRG` subprograms to `files` and `steps`.
///
/// # Arguments
///
/// * `module` - Owning [`NistModule`].
/// * `stem` - Six-character parent program stem.
/// * `archive` - Extracted NIST archive.
/// * `files` - Mutable workspace file list.
/// * `steps` - Mutable pipeline step list.
fn append_nist_companion_modules(
    module: NistModule,
    stem: &str,
    archive: &ExtractedNistArchive,
    files: &mut Vec<WorkspaceFile>,
    steps: &mut Vec<TestStep>,
) {
    if let Some(libs) = archive.module_libs.get(&module) {
        for (lib_name, lib_src) in libs {
            let rel = format!("lib/{lib_name}");
            files.push(WorkspaceFile {
                relative_path: rel.clone(),
                contents: lib_src.as_bytes().to_vec(),
            });
            steps.push(build_nist_compile_step(
                CompilationMode::CompileModule,
                &rel,
                "",
                module,
                &[],
            ));
        }
    }
    if let Some(subs) = archive.attached_subs.get(stem) {
        for (sub_fname, sub_src) in subs {
            let sub_stem = sub_fname.split('.').next().unwrap_or("");
            let sub_flags = nist_program_extra_flags(sub_stem);
            let cbl_name = sub_fname.replace(".SUB", ".CBL");
            files.push(WorkspaceFile {
                relative_path: cbl_name.clone(),
                contents: sub_src.as_bytes().to_vec(),
            });
            steps.push(build_nist_compile_step(
                CompilationMode::CompileModule,
                &cbl_name,
                "",
                module,
                &sub_flags,
            ));
        }
    }
}

/// Builds a COBOL compile step for a NIST CCVS-85 module library or test program.
///
/// # Arguments
///
/// * `mode` - `CompileModule`, `SyntaxOnly`, or `BuildExecutable`.
/// * `source` - Relative source filename.
/// * `output` - Output binary path (`""` when default module/syntax output applies).
/// * `module` - Owning [`NistModule`].
/// * `extra_flags` - Program-specific flags (`-fdebugging-line`, `-Wobsolete`, etc.).
#[must_use]
fn build_nist_compile_step(
    mode: CompilationMode,
    source: &str,
    output: &str,
    module: NistModule,
    extra_flags: &[String],
) -> TestStep {
    let include_dirs = if module == NistModule::Sm {
        vec![String::from("copy")]
    } else {
        Vec::new()
    };
    let mut raw_flags = vec![String::from("-debug")];
    raw_flags.extend(extra_flags.iter().cloned());
    TestStep::CompileCobol(CompileCobolSpec {
        mode,
        format: SourceFormat::Fixed,
        dialect: Dialect::Cobol85,
        optimization: OptimizationLevel::O0,
        encoding: TextEncoding::Ascii,
        sources: vec![String::from(source)],
        include_dirs,
        copy_exts: Vec::new(),
        library_dirs: Vec::new(),
        libraries: Vec::new(),
        raw_flags,
        output: String::from(output),
        expected_success: true,
        expected_diagnostics: Vec::new(),
        bogus_patterns: Vec::new(),
        allow_excess_errors: false,
        prune_patterns: Vec::new(),
        env: vec![(String::from("COB_DISABLE_WARNINGS"), String::from("Y"))],
    })
}

/// Returns per-program compiler flags matching `report.pl`.
///
/// # Arguments
///
/// * `stem` - Six-character NIST program stem.
#[must_use]
pub fn nist_program_extra_flags(stem: &str) -> Vec<String> {
    match stem {
        "SM206A" => vec![String::from("-fdebugging-line")],
        "NC302M" | "DB304M" => vec![String::from("-Wobsolete")],
        "IC115A" => vec![String::from("-Wno-goto-section")],
        "DB201A" => vec![String::from("-fno-ec=data-incompatible")],
        _ => Vec::new(),
    }
}

/// Builds the [`TestStep::RunBinary`] for an executable NIST CCVS-85 validation program.
///
/// # Arguments
///
/// * `module` - Owning [`NistModule`].
/// * `stem` - Six-character program stem.
/// * `row` - Baseline subtest counts.
/// * `occurrence` - Execution index (`2` disables runtime debug on `DB103M`).
/// * `archive` - Extracted archive containing optional `.DAT` stdin data.
#[must_use]
fn build_nist_run_step(
    module: NistModule,
    stem: &str,
    row: &NistBaselineEntry,
    occurrence: usize,
    archive: &ExtractedNistArchive,
) -> TestStep {
    let debug_val = if module == NistModule::Db && stem != "DB102A" && occurrence == 1 {
        "Y"
    } else {
        "N"
    };
    let report_file = format!("{stem}.log");
    let mut env = vec![
        (String::from("COB_SWITCH_1"), String::from("ON")),
        (String::from("COB_SWITCH_2"), String::from("OFF")),
        (String::from("COB_SET_DEBUG"), String::from(debug_val)),
        (String::from("COB_LIBRARY_PATH"), String::from(".")),
        (String::from("REPORT"), report_file.clone()),
    ];
    if module == NistModule::Rw {
        env.push((String::from("DD_XXXXX049"), format!("{stem}.rep")));
    }

    let stdin = archive.data_files.get(stem).cloned().unwrap_or_default();

    let no_report_file = matches!(
        stem,
        "NC110M"
            | "NC214M"
            | "OBSQ3A"
            | "ST102A"
            | "ST109A"
            | "ST110A"
            | "ST112M"
            | "ST113M"
            | "ST115A"
            | "ST116A"
            | "ST120A"
            | "ST122A"
            | "ST123A"
            | "DB301M"
            | "DB302M"
            | "DB303M"
            | "DB305M"
            | "IF402M"
            | "NC107A"
            | "NC113M"
            | "NC121M"
            | "NC220M"
            | "NC135A"
    );
    let expected_files = if no_report_file {
        Vec::new()
    } else {
        vec![(
            report_file,
            OutputMatcher::Ccvs85Report {
                expected_pass: row.pass,
                expected_deleted: row.deleted,
                expected_inspect: row.inspect,
            },
        )]
    };

    TestStep::RunBinary(RunBinarySpec {
        target: format!("./{stem}"),
        via_cobcrun: false,
        args: Vec::new(),
        stdin,
        env,
        expected_exit_code: 0,
        stdout: OutputMatcher::Ignore,
        stderr: OutputMatcher::Ignore,
        expected_files,
        timeout_ms: 20_000,
    })
}
