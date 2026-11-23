//! Reviewed public trust inputs only. Provision in source review, never from a release/feed,
//! user environment, debug mode, local mirror, or the running Launcher's own certificate.

/// Developer ID Application team (leaf certificate OU) that signs and notarizes the macOS
/// Nightly. Pinned here rather than read from catalog data; admission also requires it to equal
/// stable PhotoCraft's team, which the stable Mac install verifies.
pub(crate) const MAC_TEAM: Option<&str> = Some(artcraft_launcher_core::catalog::ARTCRAFT_TEAM_ID);

#[cfg(target_os = "linux")]
pub(crate) struct LinuxSigner<'a> {
    pub public_key: &'a str,
    pub primary_fingerprint: &'a str,
}

#[cfg(target_os = "linux")]
pub(crate) const LINUX_SIGNER: Option<LinuxSigner<'static>> = None;

/// Exact native SignerCertificate.Subject representation, case-sensitive; not a display CN.
#[cfg(windows)]
pub(crate) const WINDOWS_SUBJECT: Option<&str> = None;
