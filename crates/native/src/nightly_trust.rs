//! Reviewed public trust inputs only. Provision in source review, never from a release/feed,
//! user environment, debug mode, local mirror, or the running Launcher's own certificate.

#[cfg(target_os = "linux")]
pub(crate) struct LinuxSigner<'a> {
    pub public_key: &'a str,
    pub primary_fingerprint: &'a str,
}

#[cfg(target_os = "linux")]
pub(crate) const LINUX_SIGNER: Option<LinuxSigner<'static>> = None;

/// A Windows publisher, by what stays the same when its certificate renews. Subjects are exact
/// native `X509Certificate2.Subject` strings, case-sensitive, not display CNs. No leaf key,
/// thumbprint or issuing CA is pinned: those rotate.
#[cfg(any(windows, test))]
pub(crate) struct WindowsPublisher<'a> {
    /// The signer certificate's Subject.
    pub subject: &'a str,
    /// The trusted root the signer's chain must end at.
    pub root: &'a str,
    /// Enhanced key usages the signer certificate must all carry.
    pub ekus: &'a [&'a str],
}

/// PhotoCraft's Azure Trusted Signing (Microsoft ID Verified) identity, read from the v0.23.1
/// stable photocraft.exe; evidence: photocraft-stable-signing-identity-20261123/SIGNING-IDENTITY.md.
/// Its leaf lasts three days and is renewed for each signing run, under rotating issuing CAs, so
/// the pin is the verified subject, the identity's own EKU and the identity root. A timestamped
/// signature stays valid after its leaf expires.
#[cfg(windows)]
pub(crate) const WINDOWS_PUBLISHER: Option<WindowsPublisher<'static>> = Some(WindowsPublisher {
    subject: "CN=Learning Machines Inc, O=Learning Machines Inc, L=Atlanta, S=Georgia, C=US",
    root: "CN=Microsoft Identity Verification Root Certificate Authority 2020, O=Microsoft Corporation, C=US",
    ekus: &[CODE_SIGNING_EKU, "1.3.6.1.4.1.311.97.627281783.214399755.66947568.492752054"],
});

/// Code Signing (id-kp-codeSigning).
#[cfg(any(windows, test))]
pub(crate) const CODE_SIGNING_EKU: &str = "1.3.6.1.5.5.7.3.3";
