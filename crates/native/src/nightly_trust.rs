//! Reviewed public trust inputs only. Provision in source review, never from a release/feed,
//! user environment, debug mode, local mirror, or the running Launcher's own certificate.

/// Developer ID Application team (leaf certificate OU) that signs and notarizes the macOS
/// Nightly. A reviewed literal, deliberately not the catalog's team constant: admission also
/// requires stable PhotoCraft's catalog team to equal it, so a change on either side refuses
/// new Nightly installs until both are reviewed again.
pub(crate) const MAC_TEAM: Option<&str> = Some("DJ6XS33FX8");

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
