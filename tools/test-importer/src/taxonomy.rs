//! Automated compiler capability taxonomy tagger and cross-suite equivalence linker.

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{CompilationMode, Dialect, FeatureTag, SuiteId, TestManifest, TextEncoding};

/// Infers sorted, deduplicated [`FeatureTag`] labels from COBOL source text and compile settings.
///
/// # Arguments
///
/// * `source_upper` - Uppercased COBOL source text across the test's primary files.
/// * `mode` - Compilation pipeline stage of the test.
/// * `dialect` - Configured COBOL dialect.
/// * `encoding` - Configured runtime text encoding.
#[must_use]
pub fn infer_feature_tags(
    source_upper: &str,
    mode: CompilationMode,
    dialect: Dialect,
    encoding: &TextEncoding,
) -> Vec<FeatureTag> {
    let mut tags = BTreeSet::new();
    if mode == CompilationMode::PreprocessOnly
        || contains_any(source_upper, &["COPY ", "REPLACE ", ">>DEFINE", ">>IF"])
    {
        tags.insert(FeatureTag::StagePreprocess);
    }
    if mode == CompilationMode::SyntaxOnly {
        tags.insert(FeatureTag::StageParseSema);
    }
    if !matches!(encoding, TextEncoding::Ascii) || source_upper.contains("EBCDIC") {
        tags.insert(FeatureTag::FeatureEbcdic);
    }
    if dialect == Dialect::Ibm && source_upper.contains("COMP") {
        tags.insert(FeatureTag::TypeBinaryComp5);
    }
    infer_type_tags(source_upper, &mut tags);
    infer_language_feature_tags(source_upper, &mut tags);
    if tags.is_empty() {
        tags.insert(FeatureTag::FeaturePerformEvalIf);
    }
    if let Some(dtag) = map_dialect_tag(dialect) {
        tags.insert(dtag);
    }
    tags.into_iter().collect()
}

/// Maps a [`Dialect`] to its Phase 5.1 `dialect:*` [`FeatureTag`].
///
/// # Arguments
///
/// * `dialect` - Configured COBOL dialect.
#[must_use]
pub const fn map_dialect_tag(dialect: Dialect) -> Option<FeatureTag> {
    match dialect {
        Dialect::Cobol85 => Some(FeatureTag::DialectIso85),
        Dialect::Cobol2002 => Some(FeatureTag::DialectIso2002),
        Dialect::Cobol2014 => Some(FeatureTag::DialectIso2014),
        Dialect::Cobol2023 => Some(FeatureTag::DialectIso2023),
        Dialect::Ibm => Some(FeatureTag::DialectIbm),
        Dialect::MicroFocus => Some(FeatureTag::DialectMf),
        Dialect::Gnu => Some(FeatureTag::DialectGnucobol),
        Dialect::Default | Dialect::Bs2000 | Dialect::Acu | Dialect::XOpen => None,
    }
}

/// Populates `type:*` taxonomy tags based on `PICTURE` and `USAGE` keywords in `src`.
///
/// # Arguments
///
/// * `src` - Uppercased COBOL source text.
/// * `tags` - Destination tag set.
fn infer_type_tags(src: &str, tags: &mut BTreeSet<FeatureTag>) {
    let rules: &[(&[&str], FeatureTag)] = &[
        (
            &["PIC X", "PICTURE X", "PIC A", "ALPHABETIC"],
            FeatureTag::TypeAlphanumeric,
        ),
        (
            &["PIC 9", "PIC S9", "PICTURE 9", "PICTURE S9"],
            FeatureTag::TypeNumericDisplay,
        ),
        (
            &["PIC Z", "PIC $", "PIC +", "PIC -", "BLANK WHEN ZERO"],
            FeatureTag::TypeNumericEdited,
        ),
        (
            &["COMP-5", "BINARY", "COMP-4", "BINARY-LONG", "BINARY-DOUBLE"],
            FeatureTag::TypeBinaryComp5,
        ),
        (
            &["COMP-3", "PACKED-DECIMAL", "COMP-6"],
            FeatureTag::TypePackedComp3,
        ),
        (
            &[
                "COMP-1",
                "COMP-2",
                "FLOAT-SHORT",
                "FLOAT-LONG",
                "FLOAT-EXTENDED",
            ],
            FeatureTag::TypeFloat,
        ),
        (&["POINTER", "ADDRESS OF"], FeatureTag::TypePointer),
        (
            &["NATIONAL", "PIC N", "UTF-8", "PIC U"],
            FeatureTag::TypeNationalUtf8,
        ),
        (&["9(37)", "9(38)", "37-DIGIT"], FeatureTag::TypeDecimal128),
    ];
    for (keywords, tag) in rules {
        if contains_any(src, keywords) {
            tags.insert(*tag);
        }
    }
}

/// Populates `feature:*` taxonomy tags based on COBOL division, clause, and verb keywords in `src`.
///
/// # Arguments
///
/// * `src` - Uppercased COBOL source text.
/// * `tags` - Destination tag set.
fn infer_language_feature_tags(src: &str, tags: &mut BTreeSet<FeatureTag>) {
    let rules: &[(&[&str], FeatureTag)] = &[
        (&[" 88 ", "\u{000a}88 "], FeatureTag::FeatureLevel88),
        (
            &["REDEFINES ", " 66 ", "RENAMES "],
            FeatureTag::FeatureRedefinesRenames,
        ),
        (&["OCCURS ", "DEPENDING ON"], FeatureTag::FeatureOccursOdo),
        (&["REFERENCE-MOD", "(1:", "(2:"], FeatureTag::FeatureRefmod),
        (
            &["ADD ", "SUBTRACT ", "MULTIPLY ", "DIVIDE ", "COMPUTE "],
            FeatureTag::FeatureArith,
        ),
        (&["MOVE ", "INITIALIZE "], FeatureTag::FeatureMoveInit),
        (
            &["STRING ", "UNSTRING ", "INSPECT ", "TRANSFORM "],
            FeatureTag::FeatureStringInspect,
        ),
        (
            &["PERFORM ", "EVALUATE ", "IF ", "GO TO ", "ALTER "],
            FeatureTag::FeaturePerformEvalIf,
        ),
        (&["SEARCH ", "SORT "], FeatureTag::FeatureSearchSortTable),
        (
            &["CALL ", "CANCEL ", "LINKAGE SECTION", "GOBACK"],
            FeatureTag::FeatureCallSubprog,
        ),
        (
            &["GLOBAL", "EXTERNAL", "COMMON"],
            FeatureTag::FeatureNestedGlobalExternal,
        ),
        (
            &["ALLOCATE ", "FREE ", "BASED"],
            FeatureTag::FeatureAllocateBased,
        ),
        (
            &["DECLARATIVES", "USE AFTER", "EC-"],
            FeatureTag::FeatureDeclarativesExceptions,
        ),
        (&["FUNCTION "], FeatureTag::FeatureIntrinsics),
        (&["FUNCTION-ID"], FeatureTag::FeatureUdf),
        (
            &["SEQUENTIAL", "OPEN ", "READ ", "WRITE "],
            FeatureTag::FeatureFileSeq,
        ),
        (&["RELATIVE "], FeatureTag::FeatureFileRel),
        (&["INDEXED ", "RECORD KEY"], FeatureTag::FeatureFileIdx),
        (&["MERGE ", "SD "], FeatureTag::FeatureSortMerge),
        (
            &["REPORT SECTION", "INITIATE ", "GENERATE "],
            FeatureTag::FeatureReportWriter,
        ),
        (
            &["SCREEN SECTION", "BELL", "HIGHLIGHT"],
            FeatureTag::FeatureScreen,
        ),
        (
            &["XML GENERATE", "XML PARSE", "JSON GENERATE", "JSON PARSE"],
            FeatureTag::FeatureXmlJson,
        ),
        (&["CLASS-ID", "METHOD-ID", "INVOKE "], FeatureTag::FeatureOo),
        (&["CBL_", "C$"], FeatureTag::FeatureCInterop),
    ];
    for (keywords, tag) in rules {
        if contains_any(src, keywords) {
            tags.insert(*tag);
        }
    }
}

/// Checks whether `haystack` contains at least one substring in `needles`.
///
/// # Arguments
///
/// * `haystack` - Text to search.
/// * `needles` - Candidate substrings.
#[must_use]
fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Normalizes a test title into lowercase alphanumeric words for cross-suite equivalence matching.
///
/// # Arguments
///
/// * `title` - Raw `original_test_name` from `gcobol` or `gnucobol`.
#[must_use]
pub fn normalize_equivalence_key(title: &str) -> String {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            cur.push(ch.to_ascii_lowercase());
        } else if !cur.is_empty() {
            tokens.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens.join("_")
}

/// Cross-links equivalent tests across `gcobol` and `gnucobol` manifests in place.
///
/// # Arguments
///
/// * `manifests` - Combined slice of imported manifests across suites.
pub fn link_cross_suite_equivalents(manifests: &mut [TestManifest]) {
    let mut groups: BTreeMap<String, Vec<(usize, SuiteId, String)>> = BTreeMap::new();
    for (idx, m) in manifests.iter().enumerate() {
        let key = normalize_equivalence_key(&m.provenance.original_test_name);
        if key.len() >= 4 {
            groups
                .entry(key)
                .or_default()
                .push((idx, m.suite, m.id.clone()));
        }
    }

    for members in groups.values() {
        for &(idx, suite, _) in members {
            let mut peers: Vec<String> = members
                .iter()
                .filter(|(_, other_suite, _)| *other_suite != suite)
                .map(|(_, _, other_id)| other_id.clone())
                .collect();
            peers.sort();
            peers.dedup();
            if !peers.is_empty() {
                manifests[idx].equivalent_tests = peers;
            }
        }
    }
}
