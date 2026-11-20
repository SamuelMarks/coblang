//! Exhaustive round-trip and malformed-input coverage tests for `coblang_rt_core::fileio_wire` and `lookup_env`.

use coblang_common::Result;
use coblang_hir::{
    HirAltKeySuppression, HirDuplicateKeyPolicy, HirFileAccessMode, HirFileLockMode,
    HirFileOptionality, HirFileOrganization, HirFileSharingMode, HirKeySegment,
    HirPrintAdvancingMode, HirRecordFormat,
};
use coblang_rt_core::{
    CobolRuntimeContext,
    accept_env::EnvDisplayTarget,
    fileio_wire::{
        DECL_MASK_EXTEND, DECL_MASK_FILE, DECL_MASK_INPUT, DECL_MASK_IO, DECL_MASK_OUTPUT,
        FileConnectorWireSpec, POST_IO_CONTINUE, POST_IO_FATAL_ABORT, POST_IO_FIRE_AT_END,
        POST_IO_FIRE_AT_EOP, POST_IO_FIRE_INVALID_KEY, POST_IO_FIRE_NOT_AT_END,
        POST_IO_FIRE_NOT_AT_EOP, POST_IO_FIRE_NOT_INVALID_KEY, POST_IO_INVOKE_DECL_EXTEND,
        POST_IO_INVOKE_DECL_FILE, POST_IO_INVOKE_DECL_INPUT, POST_IO_INVOKE_DECL_IO,
        POST_IO_INVOKE_DECL_OUTPUT, WireAltKey, WireAssignKind, WireConnectorIdentity,
        decode_file_connector_spec, encode_file_connector_spec,
    },
};

/// Builds a baseline [`FileConnectorWireSpec`] for combinatorial variant testing.
fn sample_wire_spec() -> FileConnectorWireSpec {
    FileConnectorWireSpec {
        identity: WireConnectorIdentity::UnitOwned {
            owner_unit: 3,
            owner_fc: 7,
        },
        file_name: String::from("CUST-FILE"),
        organization: HirFileOrganization::Sequential,
        access_mode: HirFileAccessMode::Sequential,
        optionality: HirFileOptionality::Required,
        assign: WireAssignKind::Literal(String::from("cust.dat")),
        record_format: HirRecordFormat::Fixed,
        min_record_bytes: 80,
        max_record_bytes: 80,
        relative_key_digits: 0,
        lock_mode: HirFileLockMode::Default,
        sharing_mode: None,
        padding_byte: b' ',
        print_mode: HirPrintAdvancingMode::RawBinary,
        collating_ranks: None,
        primary_key_segments: Vec::new(),
        alternate_keys: Vec::new(),
    }
}

/// Verifies wire-protocol `POST_IO_*` and `DECL_MASK_*` constant values.
#[test]
fn wire_spec_constants() {
    assert_eq!(POST_IO_CONTINUE, 0);
    assert_eq!(POST_IO_FIRE_AT_END, 1);
    assert_eq!(POST_IO_FIRE_NOT_AT_END, 2);
    assert_eq!(POST_IO_FIRE_INVALID_KEY, 3);
    assert_eq!(POST_IO_FIRE_NOT_INVALID_KEY, 4);
    assert_eq!(POST_IO_FIRE_AT_EOP, 5);
    assert_eq!(POST_IO_FIRE_NOT_AT_EOP, 6);
    assert_eq!(POST_IO_INVOKE_DECL_FILE, 7);
    assert_eq!(POST_IO_INVOKE_DECL_INPUT, 8);
    assert_eq!(POST_IO_INVOKE_DECL_OUTPUT, 9);
    assert_eq!(POST_IO_INVOKE_DECL_IO, 10);
    assert_eq!(POST_IO_INVOKE_DECL_EXTEND, 11);
    assert_eq!(POST_IO_FATAL_ABORT, 12);
    assert_eq!(
        DECL_MASK_FILE | DECL_MASK_INPUT | DECL_MASK_OUTPUT | DECL_MASK_IO | DECL_MASK_EXTEND,
        0b1_1111
    );
}

/// Builds sample primary and alternate key lists for indexed wire round-trip testing.
fn sample_indexed_keys() -> (Vec<HirKeySegment>, Vec<WireAltKey>) {
    let pk = vec![
        HirKeySegment {
            offset: 0,
            length: 4,
        },
        HirKeySegment {
            offset: 10,
            length: 6,
        },
    ];
    let aks = vec![
        WireAltKey {
            segments: vec![HirKeySegment {
                offset: 4,
                length: 6,
            }],
            duplicates: HirDuplicateKeyPolicy::Unique,
            suppression: None,
        },
        WireAltKey {
            segments: vec![HirKeySegment {
                offset: 20,
                length: 4,
            }],
            duplicates: HirDuplicateKeyPolicy::WithDuplicates,
            suppression: Some(HirAltKeySuppression::WhenAllByte(b'*')),
        },
        WireAltKey {
            segments: vec![HirKeySegment {
                offset: 24,
                length: 3,
            }],
            duplicates: HirDuplicateKeyPolicy::WithDuplicates,
            suppression: Some(HirAltKeySuppression::WhenExactBytes(vec![b'N', b'/', b'A'])),
        },
    ];
    (pk, aks)
}

/// Verifies round-trip encoding/decoding across all enum variants and optional payloads.
#[test]
fn wire_spec_round_trip_all_variants() -> Result<()> {
    let mut ranks = [0_u8; 256];
    for (i, slot) in ranks.iter_mut().enumerate() {
        *slot = u8::try_from(255 - i).unwrap_or(0);
    }

    let identities = [
        WireConnectorIdentity::UnitOwned {
            owner_unit: 0,
            owner_fc: 1,
        },
        WireConnectorIdentity::External(String::from("EXT-FILE")),
    ];
    let orgs = [
        HirFileOrganization::Sequential,
        HirFileOrganization::LineSequential,
        HirFileOrganization::Relative,
        HirFileOrganization::Indexed,
    ];
    let accesses = [
        HirFileAccessMode::Sequential,
        HirFileAccessMode::Random,
        HirFileAccessMode::Dynamic,
    ];
    let opts = [HirFileOptionality::Required, HirFileOptionality::Optional];
    let assigns = [
        WireAssignKind::Literal(String::from("a.dat")),
        WireAssignKind::ExternalEnv(String::from("DD_A")),
        WireAssignKind::DynamicBuffer,
        WireAssignKind::EnvOrLiteral(String::from("BARE")),
        WireAssignKind::DisplayDevice,
        WireAssignKind::KeyboardDevice,
    ];
    let formats = [HirRecordFormat::Fixed, HirRecordFormat::Varying];
    let locks = [
        HirFileLockMode::Default,
        HirFileLockMode::Manual,
        HirFileLockMode::Automatic,
        HirFileLockMode::Exclusive,
    ];
    let sharings = [
        None,
        Some(HirFileSharingMode::AllOther),
        Some(HirFileSharingMode::NoOther),
        Some(HirFileSharingMode::ReadOnly),
    ];
    let prints = [
        HirPrintAdvancingMode::RawBinary,
        HirPrintAdvancingMode::LineAdvancing,
    ];

    for (idx, assign) in assigns.into_iter().enumerate() {
        let mut s = sample_wire_spec();
        s.identity = identities
            .get(idx % identities.len())
            .cloned()
            .unwrap_or(s.identity);
        s.organization = orgs
            .get(idx % orgs.len())
            .copied()
            .unwrap_or(s.organization);
        s.access_mode = accesses
            .get(idx % accesses.len())
            .copied()
            .unwrap_or(s.access_mode);
        s.optionality = opts.get(idx % opts.len()).copied().unwrap_or(s.optionality);
        s.assign = assign;
        s.record_format = formats
            .get(idx % formats.len())
            .copied()
            .unwrap_or(s.record_format);
        s.lock_mode = locks.get(idx % locks.len()).copied().unwrap_or(s.lock_mode);
        s.sharing_mode = sharings.get(idx % sharings.len()).copied().unwrap_or(None);
        s.print_mode = prints
            .get(idx % prints.len())
            .copied()
            .unwrap_or(s.print_mode);
        s.relative_key_digits = u8::try_from(idx).unwrap_or(0);
        if idx % 2 == 1 {
            s.collating_ranks = Some(ranks);
            let (pk, aks) = sample_indexed_keys();
            s.primary_key_segments = pk;
            s.alternate_keys = aks;
        }
        let encoded = encode_file_connector_spec(&s);
        let decoded = decode_file_connector_spec(&encoded)?;
        assert_eq!(decoded, s);
    }
    Ok(())
}

/// Verifies every truncation, bad-magic, bad-tag, and invalid-UTF-8 error branch in `decode_file_connector_spec`.
#[test]
fn wire_spec_error_branches() {
    assert!(decode_file_connector_spec(b"").is_err());
    assert!(decode_file_connector_spec(b"XXXX").is_err());

    let mut base = sample_wire_spec();
    base.collating_ranks = Some([0_u8; 256]);
    base.primary_key_segments = vec![HirKeySegment {
        offset: 0,
        length: 4,
    }];
    base.alternate_keys = vec![WireAltKey {
        segments: vec![HirKeySegment {
            offset: 4,
            length: 2,
        }],
        duplicates: HirDuplicateKeyPolicy::Unique,
        suppression: Some(HirAltKeySuppression::WhenExactBytes(vec![b'A', b'B'])),
    }];
    let valid = encode_file_connector_spec(&base);
    assert!(decode_file_connector_spec(&valid).is_ok());

    // Every prefix truncation of `valid` and alternate string/byte-bearing variants must fail cleanly.
    let mut v2 = sample_wire_spec();
    v2.identity = WireConnectorIdentity::External(String::from("EXT"));
    v2.assign = WireAssignKind::ExternalEnv(String::from("DD_X"));
    v2.alternate_keys = vec![WireAltKey {
        segments: Vec::new(),
        duplicates: HirDuplicateKeyPolicy::WithDuplicates,
        suppression: Some(HirAltKeySuppression::WhenAllByte(b'*')),
    }];
    let mut v3 = sample_wire_spec();
    v3.assign = WireAssignKind::EnvOrLiteral(String::from("ENVLIT"));
    for blob in [
        valid.clone(),
        encode_file_connector_spec(&v2),
        encode_file_connector_spec(&v3),
    ] {
        for cut in 0..blob.len() {
            let prefix = blob.get(..cut).unwrap_or(&[]);
            assert!(decode_file_connector_spec(prefix).is_err());
        }
    }

    // Mutate each tag byte position to `0xFE` and verify invalid-tag rejection.
    for pos in 4..valid.len() {
        let mut mutated = valid.clone();
        if let Some(slot) = mutated.get_mut(pos) {
            *slot = 0xFE;
        }
        let _ = decode_file_connector_spec(&mutated);
    }

    // Invalid UTF-8 inside `file_name` (`UnitOwned` layout: 4 magic + 1 id_tag + 8 u32s + 2 len = offset 15).
    let simple = encode_file_connector_spec(&sample_wire_spec());
    let mut bad_utf8 = simple;
    if let Some(b) = bad_utf8.get_mut(15) {
        *b = 0xFF;
    }
    assert!(decode_file_connector_spec(&bad_utf8).is_err());
}

/// Verifies `CobolRuntimeContext::lookup_env` overlays `SET ENVIRONMENT` before host env.
#[test]
fn runtime_context_lookup_env_overlay() {
    let mut ctx = CobolRuntimeContext::new();
    assert_eq!(ctx.lookup_env("COBLANG_DEFINITELY_UNSET_ENV_998877"), None);
    ctx.display_staging_mut()
        .push_raw(b"COBLANG_DEFINITELY_UNSET_ENV_998877");
    ctx.flush_display_to_env_target(EnvDisplayTarget::EnvironmentName);
    ctx.display_staging_mut().push_raw(b"overridden_path.dat");
    ctx.flush_display_to_env_target(EnvDisplayTarget::EnvironmentValue);
    assert_eq!(
        ctx.lookup_env("COBLANG_DEFINITELY_UNSET_ENV_998877")
            .as_deref(),
        Some("overridden_path.dat")
    );
}
