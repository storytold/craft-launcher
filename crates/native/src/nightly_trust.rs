//! Reviewed public trust inputs only. Provision in source review, never from a release/feed,
//! user environment, debug mode, local mirror, or the running Launcher's own certificate.

/// Developer ID Application team (leaf certificate OU) that signs and notarizes the macOS
/// Nightly. The team is shared by every official app, so it alone doesn't name PhotoCraft: the
/// Nightly bundle identifier, checked with it, is what tells the apps apart. A reviewed literal,
/// deliberately not the catalog's team constant: admission also requires stable PhotoCraft's
/// catalog team to equal it, so a change on either side refuses new Nightly installs until both
/// are reviewed again.
pub(crate) const MAC_TEAM: Option<&str> = Some("DJ6XS33FX8");

#[cfg(target_os = "linux")]
pub(crate) struct LinuxSigner<'a> {
    pub public_key: &'a str,
    pub primary_fingerprint: &'a str,
}

/// The ArtCraft Linux Release Signing Key, the same key that signs stable Linux releases' `.asc`
/// files (craft-common-actions' shared release workflow, `sign-linux.sh`) and the ArtCraft OCI
/// Flatpak images. Provenance: Infra, who hold the key, handed over the public key and the
/// primary fingerprint below on 2026-11-23, and the same key is the `GPGKey` in getartcraft.com's
/// artcraft-oci.flatpakrepo, published independently of any release. (A copy also ships with
/// PhotoCraft releases, but that copy is vouched for only by its own signature, so it is not
/// evidence.) Signatures come from its signing subkey 568DC1F68F8FAECC797F3C7B006DAF6BB0EF334E;
/// the pin is the primary, as `VALIDSIG` reports it.
#[cfg(target_os = "linux")]
pub(crate) const LINUX_SIGNER: Option<LinuxSigner<'static>> = Some(LinuxSigner {
    public_key: include_str!("../assets/linux-release-signing-key.asc"),
    primary_fingerprint: "2B76741B414AAD6CD33E631213A005C0B9D7C3ED",
});

/// A Windows publisher, by what stays the same when its certificate renews. Subjects are exact
/// native `X509Certificate2.Subject` strings, case-sensitive, not display CNs. No leaf key,
/// thumbprint or issuing CA is pinned: those rotate.
#[cfg(any(windows, test))]
pub(crate) struct WindowsPublisher<'a> {
    /// The signer certificate's Subject.
    pub subject: &'a str,
    /// The trusted root the signer's chain must end at.
    pub root: &'a str,
    /// That root certificate's SHA-256, so a different certificate with the same name can't pass.
    pub root_sha256: &'a str,
    /// Enhanced key usages the signer certificate must all carry.
    pub ekus: &'a [&'a str],
}

/// PhotoCraft's Azure Trusted Signing (Microsoft ID Verified) identity, read from the v0.23.1
/// stable photocraft.exe (provenance in docs/nightly-signing.md). Its leaf lasts three days and is
/// renewed for each signing run, under rotating issuing CAs, so the pin is the verified subject,
/// the identity's own EKU and the identity root. A timestamped signature stays valid after its
/// leaf expires. Compiled into tests on every platform, so CI checks it everywhere; only Windows
/// uses it to admit anything.
#[cfg(any(windows, test))]
pub(crate) const WINDOWS_PUBLISHER: Option<WindowsPublisher<'static>> = Some(WindowsPublisher {
    subject: "CN=Learning Machines Inc, O=Learning Machines Inc, L=Atlanta, S=Georgia, C=US",
    root: "CN=Microsoft Identity Verification Root Certificate Authority 2020, O=Microsoft Corporation, C=US",
    root_sha256: "5367f20c7ade0e2bca790915056d086b720c33c1fa2a2661acf787e3292e1270",
    ekus: &[CODE_SIGNING_EKU, "1.3.6.1.4.1.311.97.627281783.214399755.66947568.492752054"],
});

/// Code Signing (id-kp-codeSigning).
#[cfg(any(windows, test))]
pub(crate) const CODE_SIGNING_EKU: &str = "1.3.6.1.5.5.7.3.3";

/// Whether new Nightly payloads for `os` can be authenticated here: its reviewed trust input is
/// set. On macOS that is the Nightly team pin, still matching stable PhotoCraft's team.
pub(crate) fn configured_for(os: artcraft_launcher_core::Os) -> bool {
    match os {
        #[cfg(windows)]
        artcraft_launcher_core::Os::Windows => WINDOWS_PUBLISHER.is_some(),
        #[cfg(target_os = "linux")]
        artcraft_launcher_core::Os::Linux => LINUX_SIGNER.is_some(),
        #[cfg(target_os = "macos")]
        artcraft_launcher_core::Os::MacOs => crate::nightly_profile::mac_team().is_ok(),
        _ => false,
    }
}
