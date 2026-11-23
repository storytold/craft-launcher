//! Reviewed public trust inputs only. Provision in source review, never from a release/feed,
//! user environment, debug mode, local mirror, or the running Launcher's own certificate.

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
