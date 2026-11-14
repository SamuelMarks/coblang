//! Host and target capability detection for conditional test `SKIP` evaluation.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};

use coblang_common::Capability;

/// Snapshot of host/target capabilities used to evaluate `required_capabilities` on tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCapabilities {
    /// Explicitly enabled discrete capabilities.
    enabled: BTreeSet<Capability>,
    /// Whether unknown `iconv` codepages should be probed dynamically via the system `iconv` CLI.
    probe_system_iconv: bool,
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
        insert_endian_capability(&mut enabled, cfg!(target_endian = "big"));
        Self {
            enabled,
            probe_system_iconv: true,
        }
    }

    /// Constructs a deterministic capability set from an explicit slice without OS probing.
    ///
    /// # Arguments
    ///
    /// * `caps` - Slice of capabilities to mark available.
    /// * `probe_system_iconv` - Whether `IconvCodepage` may fall back to invoking `iconv`.
    #[must_use]
    pub fn from_explicit(caps: &[Capability], probe_system_iconv: bool) -> Self {
        Self {
            enabled: caps.iter().cloned().collect(),
            probe_system_iconv,
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
            && self.probe_system_iconv
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
/// * `is_big_endian` - `true` for big-endian targets, `false` for little-endian targets.
pub fn insert_endian_capability(enabled: &mut BTreeSet<Capability>, is_big_endian: bool) {
    if is_big_endian {
        enabled.insert(Capability::BigEndian);
    } else {
        enabled.insert(Capability::LittleEndian);
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
