//! Parsers and importers for GCC COBOL (`cobol.dg`), `GnuCOBOL` Autotest (`.at`), and NIST CCVS-85 (`newcob.val`).

use std::path::Path;

use coblang_common::{Result, TestManifest, encode_manifest_bundle};

pub mod gcobol;
pub mod gnucobol;
pub mod nist85;
pub mod taxonomy;

/// Summary statistics returned by [`import_and_write_all_ported_suites`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullImportSummary {
    /// Total imported manifests across `gcobol`, `gnucobol`, and `nist85`.
    pub total_manifests: usize,
    /// Number of manifests that received at least one cross-suite equivalence link.
    pub linked_equivalents: usize,
}

/// Validates that the test importer crate links cleanly with [`coblang_common`].
///
/// # Arguments
///
/// * `input` - Sample input slice whose byte length is returned.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError::InvalidCli`] when `input` is empty.
pub fn importer_smoke_check(input: &str) -> Result<usize> {
    if input.is_empty() {
        return Err(coblang_common::CoblangError::InvalidCli {
            message: String::from("empty input"),
        });
    }
    Ok(input.len())
}

/// Imports `gcobol`, `gnucobol`, and `nist85` from `cache_root`, cross-links equivalents, and writes `.bundle` files to `ported_root`.
///
/// # Arguments
///
/// * `cache_root` - Path to `.upstream-cache/`.
/// * `ported_root` - Path to `tests/ported/`.
///
/// # Errors
///
/// Propagates I/O or upstream suite parse errors.
pub fn import_and_write_all_ported_suites(
    cache_root: &Path,
    ported_root: &Path,
) -> Result<FullImportSummary> {
    let gcob = gcobol::import_gcobol_tree(&cache_root.join("gcobol"))?;
    let gnu = gnucobol::import_gnucobol_tree(&cache_root.join("gnucobol/tests"))?;
    let nist = nist85::import_nist85_suite(
        &cache_root.join("nist85/newcob.val"),
        &cache_root.join("gnucobol/tests/cobol85"),
    )?;

    let mut buckets: Vec<(String, Vec<TestManifest>)> = vec![
        (String::from("gcobol/top_level.bundle"), gcob.top_level),
        (String::from("gcobol/group1.bundle"), gcob.group1),
        (String::from("gcobol/group2.bundle"), gcob.group2),
        (String::from("gcobol/libgcobol.bundle"), gcob.libgcobol),
    ];
    for (stem, list) in gnu.by_file {
        buckets.push((format!("gnucobol/{stem}.bundle"), list));
    }
    for (module, list) in nist {
        buckets.push((format!("nist85/{}.bundle", module.code()), list));
    }

    // Flatten temporarily to compute bidirectional cross-suite equivalence links
    let mut flat: Vec<TestManifest> = Vec::new();
    let mut lengths: Vec<usize> = Vec::with_capacity(buckets.len());
    for (_, list) in &buckets {
        lengths.push(list.len());
        flat.extend(list.iter().cloned());
    }
    taxonomy::link_cross_suite_equivalents(&mut flat);

    let total_manifests = flat.len();
    let linked_equivalents = flat
        .iter()
        .filter(|m| !m.equivalent_tests.is_empty())
        .count();

    for sub in ["gcobol", "gnucobol", "nist85"] {
        std::fs::create_dir_all(ported_root.join(sub))?;
    }
    let mut offset = 0;
    for ((rel_bundle, _), len) in buckets.iter().zip(lengths) {
        let dest = ported_root.join(rel_bundle);
        let slice = &flat[offset..offset + len];
        std::fs::write(dest, encode_manifest_bundle(slice))?;
        offset += len;
    }

    Ok(FullImportSummary {
        total_manifests,
        linked_equivalents,
    })
}
