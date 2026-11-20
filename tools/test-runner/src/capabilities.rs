//! Host and target capability detection for conditional test `SKIP` evaluation.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};

use coblang_common::Capability;

/// Policy controlling whether unknown `iconv` codepages may probe the host `iconv` CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconvProbePolicy {
    /// Satisfy capabilities strictly from the explicit set without spawning `iconv`.
    ExplicitOnly,
    /// Dynamically probe unknown `IconvCodepage` entries via the system `iconv` binary.
    ProbeSystemIconv,
}

/// Target byte-order classification for capability registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetEndianness {
    /// Little-endian target architecture.
    LittleEndian,
    /// Big-endian target architecture.
    BigEndian,
}

/// Compile-time endianness of the current host target.
#[cfg(target_endian = "big")]
const HOST_ENDIANNESS: TargetEndianness = TargetEndianness::BigEndian;

/// Compile-time endianness of the current host target.
#[cfg(target_endian = "little")]
const HOST_ENDIANNESS: TargetEndianness = TargetEndianness::LittleEndian;

/// Snapshot of host/target capabilities used to evaluate `required_capabilities` on tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCapabilities {
    /// Explicitly enabled discrete capabilities.
    enabled: BTreeSet<Capability>,
    /// Policy governing dynamic `iconv` codepage probing.
    iconv_policy: IconvProbePolicy,
}

impl HostCapabilities {
    /// Detects capabilities of the current host environment (`darwin` / `linux`).
    #[must_use]
    pub fn detect_host() -> Self {
        let mut enabled = BTreeSet::new();
        enabled.insert(Capability::SharedLibraries);
        enabled.insert(Capability::CCompiler);
        enabled.insert(Capability::PosixTools);
        enabled.insert(Capability::Int128);
        enabled.insert(Capability::CursesPty);
        enabled.insert(Capability::Isam);
        enabled.insert(Capability::Xml);
        enabled.insert(Capability::Json);
        insert_endian_capability(&mut enabled, HOST_ENDIANNESS);
        Self {
            enabled,
            iconv_policy: IconvProbePolicy::ProbeSystemIconv,
        }
    }

    /// Constructs a deterministic capability set from an explicit slice without OS probing unless requested.
    ///
    /// # Arguments
    ///
    /// * `caps` - Slice of capabilities to mark available.
    /// * `iconv_policy` - Policy governing whether `IconvCodepage` may invoke `iconv`.
    #[must_use]
    pub fn from_explicit(caps: &[Capability], iconv_policy: IconvProbePolicy) -> Self {
        Self {
            enabled: caps.iter().cloned().collect(),
            iconv_policy,
        }
    }

    /// Checks whether `cap` is satisfied by this capability set.
    ///
    /// # Arguments
    ///
    /// * `cap` - Required capability to test.
    #[must_use]
    pub fn supports(&self, cap: &Capability) -> bool {
        if self.enabled.contains(cap) {
            return true;
        }
        if let Capability::IconvCodepage(codepage) = cap
            && self.iconv_policy == IconvProbePolicy::ProbeSystemIconv
        {
            return probe_iconv_with_binary(Path::new("iconv"), codepage);
        }
        false
    }

    /// Returns the first missing capability in `required`, if any.
    ///
    /// # Arguments
    ///
    /// * `required` - Slice of capabilities required by a [`coblang_common::TestManifest`].
    #[must_use]
    pub fn first_missing<'a>(&self, required: &'a [Capability]) -> Option<&'a Capability> {
        required.iter().find(|cap| !self.supports(cap))
    }
}

/// Inserts [`Capability::BigEndian`] or [`Capability::LittleEndian`] into `enabled`.
///
/// # Arguments
///
/// * `enabled` - Mutable capability set.
/// * `endianness` - Target byte-order classification.
pub fn insert_endian_capability(enabled: &mut BTreeSet<Capability>, endianness: TargetEndianness) {
    match endianness {
        TargetEndianness::BigEndian => {
            enabled.insert(Capability::BigEndian);
        }
        TargetEndianness::LittleEndian => {
            enabled.insert(Capability::LittleEndian);
        }
    }
}

/// Probes whether `iconv_bin` supports converting from `ASCII` to `codepage`.
///
/// # Arguments
///
/// * `iconv_bin` - Path or binary name for the `iconv` command.
/// * `codepage` - Target character encoding name (e.g., `"CP1140"`).
#[must_use]
pub fn probe_iconv_with_binary(iconv_bin: &Path, codepage: &str) -> bool {
    Command::new(iconv_bin)
        .args(["-f", "ASCII", "-t", codepage, "/dev/null"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
