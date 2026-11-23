//! Reviewed public trust inputs only. Provision in source review, never from a release/feed,
//! user environment, debug mode, local mirror, or the running Launcher's own certificate.

#[cfg(target_os = "linux")]
pub(crate) struct LinuxSigner<'a> {
    pub public_key: &'a str,
    pub primary_fingerprint: &'a str,
}

/// The ArtCraft Linux Release Signing Key, the same key that signs stable Linux releases' `.asc`
/// files (craft-common-actions' shared release workflow, `sign-linux.sh`) and the ArtCraft OCI
/// Flatpak images. Provenance: Infra supplied `release-signing-key.asc` as published with
/// PhotoCraft v0.23.1 (listed in that release's signed `SHA256SUMS.txt`) and the primary
/// fingerprint below, on 2026-11-23; it matches the `GPGKey` in getartcraft.com's
/// artcraft-oci.flatpakrepo. Signatures come from its signing subkey
/// 568DC1F68F8FAECC797F3C7B006DAF6BB0EF334E; the pin is the primary, as `VALIDSIG` reports it.
#[cfg(target_os = "linux")]
pub(crate) const LINUX_SIGNER: Option<LinuxSigner<'static>> = Some(LinuxSigner {
    public_key: include_str!("../assets/linux-release-signing-key.asc"),
    primary_fingerprint: "2B76741B414AAD6CD33E631213A005C0B9D7C3ED",
});

/// Exact native SignerCertificate.Subject representation, case-sensitive; not a display CN.
#[cfg(windows)]
pub(crate) const WINDOWS_SUBJECT: Option<&str> = None;
