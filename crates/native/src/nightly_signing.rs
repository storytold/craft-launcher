//! Nightly-only authenticity gates. A descriptor/checksum never supplies a trust anchor.

#[cfg(target_os = "linux")]
mod linux {
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use crate::nightly_trust::LinuxSigner;
    use crate::paths::Paths;
    use artcraft_launcher_core::Asset;
    use artcraft_launcher_core::nightly::{Download, MAX_SIGNATURE_BYTES};

    const GPG: &str = "/usr/bin/gpg";
    const NOT_SIGNED_BY_ARTCRAFT: &str = "This Nightly download isn't signed by ArtCraft's release key, so it wasn't installed.";
    const MAX_PUBLIC_KEY: usize = 128 * 1024;
    const MAX_ARCHIVE: u64 = 4 * 1024 * 1024 * 1024;

    pub(crate) struct VerifiedArchive {
        _home: tempfile::TempDir,
        pub file: PathBuf,
    }

    fn fingerprint(text: &str) -> bool {
        matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    fn armor(bytes: &[u8], kind: &str, limit: usize) -> Result<(), String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "The Nightly signature/key is not ASCII armor.")?;
        if bytes.is_empty()
            || bytes.len() > limit
            || !text.is_ascii()
            || !text.starts_with(&format!("-----BEGIN PGP {kind}-----"))
            || !text.trim_end().ends_with(&format!("-----END PGP {kind}-----"))
        {
            return Err("The Nightly signature/key has invalid or oversized ASCII armor.".into());
        }
        Ok(())
    }

    /// Why a Nightly archive wasn't authenticated.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum PrepareError {
        /// The archive itself is refused: its bytes don't match the release, or its genuine
        /// published signature doesn't verify it from the pinned key. It can never install.
        Refused(String),
        /// It couldn't be checked this time: the signature didn't arrive intact, the verifier or
        /// storage failed, or trust isn't configured. The download may still be good.
        Unavailable(String),
    }

    impl From<PrepareError> for String {
        fn from(error: PrepareError) -> Self {
            match error {
                PrepareError::Refused(message) | PrepareError::Unavailable(message) => message,
            }
        }
    }

    fn unavailable(error: impl ToString) -> PrepareError {
        PrepareError::Unavailable(error.to_string())
    }

    fn refused(message: &str) -> PrepareError {
        PrepareError::Refused(message.into())
    }

    // Parse the documented --status-fd protocol, not OpenPGP packets or human-readable output.
    // GnuPG checks crypto/key validity; we independently require one exact primary identity,
    // a binary detached signature, a strong digest, and no revocation/expiry/error status.
    fn status(output: &crate::process::RunOutput, expected: &str, now: i64) -> Result<(), PrepareError> {
        if output.stdout.len() > 64 * 1024 {
            return Err(refused("The Nightly OpenPGP verifier returned excessive status output."));
        }
        if !output.success {
            let statuses: Vec<Vec<&str>> =
                output.stdout.lines().filter_map(|line| line.strip_prefix("[GNUPG:] ")).map(|line| line.split_whitespace().collect()).collect();
            let reported = |kind: &str| statuses.iter().any(|fields| fields.first() == Some(&kind));
            // ERRSIG <keyid> <pkalgo> <hashalgo> <class> <time> <rc>: rc 9 is a missing public key.
            let errsig = |rc: &str| statuses.iter().any(|fields| fields.first() == Some(&"ERRSIG") && fields.get(6) == Some(&rc));
            // Only the pinned key is imported, so a signature by any other key can't be checked:
            // say so plainly rather than as a verifier failure.
            if reported("NO_PUBKEY") || errsig("9") {
                return Err(refused(NOT_SIGNED_BY_ARTCRAFT));
            }
            if reported("BADSIG") {
                return Err(refused("This Nightly download doesn't match ArtCraft's signature for it, so it wasn't installed."));
            }
            // rc 4 is an algorithm this system's gpg doesn't support (an old gpg without EdDSA, say):
            // the system's limit, not a verdict on the archive or its signer.
            if errsig("4") {
                return Err(unavailable("This system's gpg can't check the Nightly signature's algorithm; update GnuPG and try again."));
            }
            // Anything else is the verifier's own trouble, not a verdict on the archive.
            return Err(unavailable("The Nightly OpenPGP verifier failed."));
        }
        let mut valid = 0;
        let mut good = 0;
        let mut started = 0;
        for line in output.stdout.lines() {
            let fields: Vec<_> =
                line.strip_prefix("[GNUPG:] ").ok_or_else(|| refused("Unexpected Nightly OpenPGP status output."))?.split_whitespace().collect();
            match fields.first().copied() {
                Some("NEWSIG") => started += 1,
                Some("GOODSIG") => good += 1,
                Some("VALIDSIG") => {
                    if !(fields.len() == 10 || fields.len() == 11)
                        || !fingerprint(fields[1])
                        || !fields.get(10).unwrap_or(&fields[1]).eq_ignore_ascii_case(expected)
                        || !matches!(fields[8], "8" | "9" | "10" | "11")
                        || fields[9] != "00"
                        || fields[4].parse::<i64>().map_or(true, |expiry| expiry != 0 && expiry <= now)
                    {
                        return Err(refused("The Nightly detached signature does not have the pinned primary identity and validity."));
                    }
                    valid += 1;
                }
                Some(
                    "KEY_CONSIDERED"
                    | "SIG_ID"
                    | "TRUST_UNDEFINED"
                    | "TRUST_MARGINAL"
                    | "TRUST_FULLY"
                    | "TRUST_ULTIMATE"
                    | "VERIFICATION_COMPLIANCE_MODE",
                ) => {}
                _ => return Err(refused("The Nightly OpenPGP verifier reported an unsupported, expired, revoked or error status.")),
            }
        }
        if valid != 1 || good != 1 || started != 1 {
            return Err(refused("The Nightly archive requires exactly one valid signature from the pinned signer."));
        }
        Ok(())
    }

    fn gpg(home: &Path, args: &[&std::ffi::OsStr], program: &str) -> Result<crate::process::RunOutput, String> {
        let mut command = crate::process::command(program);
        command
            .env_clear()
            .env("HOME", home)
            .env("GNUPGHOME", home)
            .env("LC_ALL", "C")
            .env("PATH", "/usr/bin:/bin")
            .args([
                "--no-options",
                "--batch",
                "--no-tty",
                "--no-auto-key-retrieve",
                "--no-auto-key-import",
                "--auto-key-locate",
                "clear",
                "--no-auto-check-trustdb",
                "--status-fd=1",
            ])
            .arg("--homedir")
            .arg(home)
            .args(args);
        crate::process::run_command_with_output(command, program, Duration::from_secs(30))
    }

    /// Whether the private copy is exactly the release's archive. A copy that couldn't be hashed
    /// says nothing about the download, so only an actual difference refuses it.
    fn archive_matches(snapshot: &Path, asset: &Asset, copied: u64, sha256: impl Fn(&Path) -> Result<String, String>) -> Result<(), PrepareError> {
        const MISMATCH: &str = "The exact Nightly archive bytes do not match the verified release.";
        let expected = asset.sha256.as_ref().ok_or_else(|| refused(MISMATCH))?;
        if copied != asset.size {
            return Err(refused(MISMATCH));
        }
        let actual = sha256(snapshot).map_err(unavailable)?;
        if actual.eq_ignore_ascii_case(expected) { Ok(()) } else { Err(refused(MISMATCH)) }
    }

    pub(crate) fn prepare(
        file: &Path,
        asset: &Asset,
        signature: &Download,
        http: &crate::http::Http,
        paths: &Paths,
    ) -> Result<VerifiedArchive, PrepareError> {
        let signer = crate::nightly_trust::LINUX_SIGNER.as_ref().ok_or_else(|| {
            unavailable("No independently trusted Linux Nightly signer is configured; the installed copy and data were left unchanged.")
        })?;
        let bytes = http
            .get_bytes(&signature.url, MAX_SIGNATURE_BYTES)
            .map_err(unavailable)?
            .ok_or_else(|| unavailable("The Nightly archive signature is missing."))?;
        prepare_with(file, asset, signature, &bytes, paths, signer, GPG)
    }

    fn prepare_with(
        file: &Path,
        asset: &Asset,
        signature: &Download,
        bytes: &[u8],
        paths: &Paths,
        signer: &LinuxSigner<'_>,
        program: &str,
    ) -> Result<VerifiedArchive, PrepareError> {
        use sha2::{Digest, Sha256};
        if !fingerprint(signer.primary_fingerprint) {
            return Err(unavailable("The pinned Linux Nightly primary fingerprint is unavailable or malformed."));
        }
        armor(signer.public_key.as_bytes(), "PUBLIC KEY BLOCK", MAX_PUBLIC_KEY).map_err(unavailable)?;
        // A signature that didn't arrive intact says nothing about the archive.
        armor(bytes, "SIGNATURE", MAX_SIGNATURE_BYTES).map_err(unavailable)?;
        if bytes.len() as u64 != signature.size || !format!("{:x}", Sha256::digest(bytes)).eq_ignore_ascii_case(&signature.sha256) {
            return Err(unavailable("The Nightly detached signature does not match its published size and checksum."));
        }
        crate::nightly_profile::validate_paths(paths).map_err(unavailable)?;
        std::fs::create_dir_all(&paths.cache).map_err(unavailable)?;
        let home = tempfile::Builder::new().prefix("nightly-auth-").tempdir_in(&paths.cache).map_err(unavailable)?;
        let snapshot = home.path().join("archive.tar.gz");
        let mut input = std::fs::File::open(file).map_err(unavailable)?.take(asset.size.saturating_add(1));
        if asset.size == 0 || asset.size > MAX_ARCHIVE {
            return Err(refused("The Nightly archive exceeds its authentication limit."));
        }
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(&snapshot).map_err(unavailable)?;
        let copied = std::io::copy(&mut input, &mut out).map_err(unavailable)?;
        drop(out);
        archive_matches(&snapshot, asset, copied, crate::http::sha256_file)?;
        let public_key = home.path().join("pinned-public.asc");
        let detached = home.path().join("archive.tar.gz.asc");
        std::fs::write(&public_key, signer.public_key).map_err(unavailable)?;
        std::fs::write(&detached, bytes).map_err(unavailable)?;
        let imported = gpg(home.path(), &["--import".as_ref(), public_key.as_os_str()], program).map_err(unavailable)?;
        if !imported.success {
            return Err(unavailable("The pinned Linux Nightly public key could not be imported."));
        }
        let verified = gpg(home.path(), &["--verify".as_ref(), detached.as_os_str(), snapshot.as_os_str()], program).map_err(unavailable)?;
        status(&verified, signer.primary_fingerprint, crate::now())?;
        Ok(VerifiedArchive { _home: home, file: snapshot })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        include!("nightly_signing_linux_tests.rs");
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{PrepareError as LinuxPrepareError, prepare as prepare_linux};

#[cfg(any(windows, test))]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsEvidence {
    subject: String,
    valid: bool,
    timestamp: bool,
    embedded: bool,
    thumbprint: String,
    /// The signer's chain built to a trusted root, ignoring only time validity.
    chained: bool,
    /// That chain's root Subject.
    root: String,
    /// The SHA-256 of that root certificate.
    root_sha256: String,
    /// The signer certificate's enhanced key usages.
    ekus: Vec<String>,
}

#[cfg(any(windows, test))]
use crate::nightly_trust::WindowsPublisher;

#[cfg(any(windows, test))]
fn configured<'a>(publisher: Option<&'a WindowsPublisher<'a>>) -> Option<&'a WindowsPublisher<'a>> {
    publisher.filter(|p| {
        !p.subject.is_empty() && !p.root.is_empty() && sha256_hex(p.root_sha256) && !p.ekus.is_empty() && p.ekus.iter().all(|eku| !eku.is_empty())
    })
}

#[cfg(any(windows, test))]
fn sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(any(windows, test))]
fn match_windows(evidence: &WindowsEvidence, publisher: Option<&WindowsPublisher<'_>>, native_thumbprint: &str) -> Result<(), String> {
    let publisher = configured(publisher).ok_or("No independently trusted Windows Nightly publisher is configured.")?;
    if evidence.valid
        && evidence.timestamp
        && evidence.embedded
        && evidence.subject == publisher.subject
        && evidence.chained
        && evidence.root == publisher.root
        && evidence.root_sha256.eq_ignore_ascii_case(publisher.root_sha256)
        && publisher.ekus.iter().all(|eku| evidence.ekus.iter().any(|seen| seen == eku))
        && sha256_hex(native_thumbprint)
        && evidence.thumbprint.eq_ignore_ascii_case(native_thumbprint)
    {
        Ok(())
    } else {
        Err("The Nightly executable requires a valid timestamped signature from the exact pinned publisher, chain root and key usage.".into())
    }
}

#[cfg(any(windows, test))]
fn windows_metadata_with(
    path: &std::path::Path,
    publisher: Option<&WindowsPublisher<'_>>,
    native_thumbprint: &str,
    query: impl FnOnce(&std::path::Path) -> Result<String, String>,
) -> Result<(), String> {
    if configured(publisher).is_none() {
        return Err("No independently trusted Windows Nightly publisher is configured.".into());
    }
    let text = query(path)?;
    if text.len() > 16 * 1024 {
        return Err("The Nightly Authenticode evidence exceeds its limit.".into());
    }
    let evidence: WindowsEvidence = serde_json::from_str(&text).map_err(|_| "The Nightly Authenticode evidence is malformed.")?;
    match_windows(&evidence, publisher, native_thumbprint)
}

// Fixed native metadata query, no script/path interpolation. The trusted built-in module
// is imported from $PSHOME, not PSModulePath. Hash binds this query's leaf to WinVerifyTrust's
// actual leaf; it is not a rotating certificate pin. Native timestamp policy remains native.
#[cfg(any(windows, test))]
const WINDOWS_QUERY: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Import-Module -Name (Join-Path $PSHOME 'Modules\Microsoft.PowerShell.Security\Microsoft.PowerShell.Security.psd1') -ErrorAction Stop
Import-Module -Name (Join-Path $PSHOME 'Modules\Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1') -ErrorAction Stop
$s = Microsoft.PowerShell.Security\Get-AuthenticodeSignature -LiteralPath $env:CRAFT_NIGHTLY_VERIFIED_FILE
if ($null -eq $s.SignerCertificate) { throw 'No signer certificate' }
$hash = [System.Security.Cryptography.SHA256]::Create()
try { $thumb = [BitConverter]::ToString($hash.ComputeHash($s.SignerCertificate.RawData)).Replace('-', '').ToLowerInvariant() }
finally { $hash.Dispose() }
# The chain WinVerifyTrust accepted is rebuilt from the signature's own certificates to name its
# root. Time validity is ignored only here: a timestamped signature outlives its short-lived leaf,
# and WinVerifyTrust has already judged validity at signing time and revocation.
$bag = [System.Security.Cryptography.X509Certificates.X509Certificate2Collection]::new()
$bag.Import($env:CRAFT_NIGHTLY_VERIFIED_FILE)
$chain = [System.Security.Cryptography.X509Certificates.X509Chain]::new()
try {
  $chain.ChainPolicy.ExtraStore.AddRange($bag)
  $chain.ChainPolicy.RevocationMode = [System.Security.Cryptography.X509Certificates.X509RevocationMode]::NoCheck
  $chain.ChainPolicy.VerificationFlags = [System.Security.Cryptography.X509Certificates.X509VerificationFlags]::IgnoreNotTimeValid
  $chained = $chain.Build($s.SignerCertificate)
  $count = $chain.ChainElements.Count
  $root = ''; $rootsha = ''
  if ($count -gt 1) {
    $rootcert = $chain.ChainElements[$count - 1].Certificate
    $root = $rootcert.Subject
    $hash = [System.Security.Cryptography.SHA256]::Create()
    try { $rootsha = [BitConverter]::ToString($hash.ComputeHash($rootcert.RawData)).Replace('-', '').ToLowerInvariant() }
    finally { $hash.Dispose() }
  }
} finally { $chain.Reset() }
$ekus = @()
foreach ($extension in $s.SignerCertificate.Extensions) {
  if ($extension -is [System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]) {
    foreach ($usage in $extension.EnhancedKeyUsages) { $ekus += $usage.Value }
  }
}
@{ subject = $s.SignerCertificate.Subject; valid = ($s.Status.ToString() -ceq 'Valid');
   timestamp = ($null -ne $s.TimeStamperCertificate); embedded = ($s.SignatureType.ToString() -ceq 'Authenticode');
   thumbprint = $thumb; chained = $chained; root = $root; root_sha256 = $rootsha; ekus = [object[]]$ekus } | Microsoft.PowerShell.Utility\ConvertTo-Json -Compress
"#;

#[cfg(any(windows, test))]
fn windows_command(
    path: &std::path::Path,
    system_directory: impl FnOnce() -> Result<std::path::PathBuf, String>,
) -> Result<std::process::Command, String> {
    // Production supplies GetSystemDirectory, never an environment variable or PATH lookup.
    let system = system_directory()?;
    if !system.is_absolute() {
        return Err("The native Windows system directory is not absolute.".into());
    }
    let root = system.parent().ok_or("The native Windows system directory has no root.")?;
    let powershell = system.join(r"WindowsPowerShell\v1.0\powershell.exe");
    let modules = system.join(r"WindowsPowerShell\v1.0\Modules");
    let mut command = crate::process::command(powershell);
    command
        .env_clear()
        .current_dir(&system)
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", WINDOWS_QUERY])
        .env("CRAFT_NIGHTLY_VERIFIED_FILE", path)
        .env("SystemRoot", root)
        .env("windir", root)
        .env("PATH", &system)
        .env("PSModulePath", modules);
    Ok(command)
}

#[cfg(any(windows, test))]
fn textual_notice(bytes: &[u8]) -> bool {
    bytes.len() <= 1024 * 1024
        && std::str::from_utf8(bytes).is_ok()
        && !bytes.iter().any(|byte| byte.is_ascii_control() && !matches!(*byte, b'\n' | b'\r' | b'\t'))
}

#[cfg(windows)]
pub(crate) fn authenticate_windows(root: &std::path::Path, files: &[std::path::PathBuf]) -> Result<std::fs::File, String> {
    let publisher = configured(crate::nightly_trust::WINDOWS_PUBLISHER.as_ref())
        .ok_or("No independently trusted Windows Nightly publisher is configured; the installed copy and data were left unchanged.")?;
    authenticate_windows_as(root, files, publisher)
}

// This private helper receives only source trust from the production wrapper. A cfg(test)
// native OS fixture supplies its own independently verified Microsoft reference identity.
#[cfg(windows)]
fn authenticate_windows_as(root: &std::path::Path, files: &[std::path::PathBuf], publisher: &WindowsPublisher<'_>) -> Result<std::fs::File, String> {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    let executable = dunce::canonicalize(root.join("photocraft.exe")).map_err(|error| error.to_string())?;
    // FILE_SHARE_READ only: deny mutation, deletion or rename from before either check through
    // the installer copying these same bytes and committing the ownership record.
    let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&executable).map_err(|error| error.to_string())?;
    for relative in files {
        if relative == std::path::Path::new("photocraft.exe") {
            continue;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(root.join(relative))
            .and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes))
            .map_err(|error| error.to_string())?;
        if !textual_notice(&bytes) {
            return Err("The Nightly payload contains a non-textual notice; nothing was replaced.".into());
        }
    }
    let policy = verifysign::VerifyPolicy::new().revocation(verifysign::Revocation::Chain).allow_network(true);
    let verifier = verifysign::CodeSignVerifier::new(policy).map_err(|error| error.to_string())?;
    let signature = verifier.verify_file(&executable).map_err(|error| format!("Nightly Authenticode verification failed ({error})."))?;
    let native_thumbprint = signature.sha256_thumbprint();
    windows_metadata_with(&executable, Some(publisher), &native_thumbprint, read_windows_metadata)?;
    Ok(held)
}

/// The slowest run seen: a cold Windows PowerShell start plus hashing a large signed executable
/// on a busy Windows 11 machine. A genuinely signed build must not be refused for being slow.
#[cfg(test)]
const SLOWEST_METADATA_QUERY: std::time::Duration = std::time::Duration::from_secs(81);

/// The same bound serves installs and Open, which checks the installed copy again: the query
/// costs the same either way, and a shorter wait would refuse a good build on a slow machine.
#[cfg(any(windows, test))]
const WINDOWS_METADATA_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);

/// A query that ran out of time says so, distinctly from a signature refusal. The wording fits
/// both an install and an Open, and changes nothing either way.
#[cfg(any(windows, test))]
fn metadata_query_error(error: crate::process::RunError) -> String {
    match error {
        crate::process::RunError::TimedOut(_) => format!(
            "The Nightly signature check took longer than {} seconds and was stopped. This is not a signature refusal; try again.",
            WINDOWS_METADATA_DEADLINE.as_secs()
        ),
        crate::process::RunError::Failed(error) => error,
    }
}

#[cfg(windows)]
fn read_windows_metadata(path: &std::path::Path) -> Result<String, String> {
    let command = windows_command(path, || winsafe::GetSystemDirectory().map(std::path::PathBuf::from).map_err(|error| error.to_string()))?;
    let output =
        crate::process::run_command_bounded(command, "Nightly Authenticode metadata", WINDOWS_METADATA_DEADLINE).map_err(metadata_query_error)?;
    if !output.success || !output.stderr.trim().is_empty() {
        return Err("The native Nightly Authenticode metadata query failed.".into());
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn windows_requires_exact_full_subject_timestamp_native_trust_and_same_signer() {
        let expected = "CN=Test Publisher, O=Disposable Test, L=Test City, C=US";
        let root = "CN=Test Root, O=Disposable Test, C=US";
        let identity = "1.3.6.1.4.1.311.97.1.2.3";
        let root_sha256 = "d".repeat(64);
        let publisher = WindowsPublisher {
            subject: expected,
            root,
            root_sha256: &root_sha256,
            ekus: &[crate::nightly_trust::CODE_SIGNING_EKU, "1.3.6.1.4.1.311.97.1.2.3"],
        };
        let native = "a".repeat(64);
        let evidence = json!({"subject":expected,"valid":true,"timestamp":true,"embedded":true,"thumbprint":native,
            "chained":true,"root":root,"root_sha256":root_sha256,"ekus":["1.3.6.1.5.5.7.3.3","1.3.6.1.4.1.311.97.1.0",identity]});
        let path = Path::new("owned-photocraft.exe");
        let check = |value: &serde_json::Value, pin: Option<&str>| {
            let pinned = pin.map(|subject| WindowsPublisher { subject, ..publisher });
            windows_metadata_with(path, pinned.as_ref(), &native, |_| Ok(value.to_string()))
        };
        assert!(check(&evidence, Some(expected)).is_ok());
        assert!(check(&evidence, None).is_err());
        for field in ["valid", "timestamp", "embedded"] {
            let mut invalid = evidence.clone();
            invalid[field] = false.into();
            assert!(check(&invalid, Some(expected)).is_err(), "{field}");
        }
        for subject in [
            "CN=Test Publisher",
            "cn=Test Publisher, O=Disposable Test, L=Test City, C=US",
            "CN=Test Publisher, O=Disposable Test, L=Other City, C=US",
        ] {
            let mut invalid = evidence.clone();
            invalid["subject"] = subject.into();
            assert!(check(&invalid, Some(expected)).is_err());
        }
        let mut invalid = evidence.clone();
        invalid["thumbprint"] = "b".repeat(64).into();
        assert!(check(&invalid, Some(expected)).is_err());
        let mut invalid = evidence.clone();
        invalid.as_object_mut().unwrap().remove("timestamp");
        assert!(check(&invalid, Some(expected)).is_err());
        for malformed in ["not json".to_string(), "x".repeat(16 * 1024 + 1)] {
            assert!(windows_metadata_with(path, Some(&publisher), &native, |_| Ok(malformed)).is_err());
        }
        for failure in ["tool unavailable", "native query timed out", "native trust error"] {
            assert!(windows_metadata_with(path, Some(&publisher), &native, |_| Err(failure.into())).is_err());
        }
    }

    #[test]
    fn a_slow_signature_query_outlasts_the_slowest_seen_and_says_it_timed_out() {
        assert!(WINDOWS_METADATA_DEADLINE > SLOWEST_METADATA_QUERY * 2, "room beyond the slowest cold run seen");
        let timed_out = metadata_query_error(crate::process::RunError::TimedOut("helper didn't finish".into()));
        assert!(timed_out.contains("was stopped") && timed_out.contains("not a signature refusal"), "{timed_out}");
        assert!(!timed_out.contains("replaced"), "the same text serves an Open, which replaces nothing");
        let failed = "couldn't start Nightly Authenticode metadata (denied)";
        assert_eq!(metadata_query_error(crate::process::RunError::Failed(failed.into())), failed);
    }

    /// The production pin, checked on every platform's CI. Azure Trusted Signing renews the leaf
    /// and its issuing CA, so neither is pinned: what must hold is the publisher's subject, its
    /// identity EKU and the chain's root, by name and certificate hash.
    #[test]
    fn the_production_windows_pin_requires_its_root_and_identity_key_usage_not_a_leaf_or_issuer() {
        let publisher = crate::nightly_trust::WINDOWS_PUBLISHER.as_ref().expect("the reviewed publisher is pinned");
        let (subject, root, root_sha256) = (publisher.subject, publisher.root, publisher.root_sha256);
        let identity = "1.3.6.1.4.1.311.97.627281783.214399755.66947568.492752054";
        assert!(publisher.ekus.contains(&identity) && publisher.ekus.contains(&crate::nightly_trust::CODE_SIGNING_EKU));
        let path = Path::new("owned-photocraft.exe");
        let evidence = |thumbprint: &str, subject: &str, chained: bool, root: &str, root_sha256: &str, ekus: &[&str]| {
            json!({"subject":subject,"valid":true,"timestamp":true,"embedded":true,"thumbprint":thumbprint,
                "chained":chained,"root":root,"root_sha256":root_sha256,"ekus":ekus})
        };
        let check = |value: serde_json::Value, native: &str| windows_metadata_with(path, Some(publisher), native, |_| Ok(value.to_string()));
        // PhotoCraft v0.23.1's photocraft.exe as a Windows 11 guest read it.
        let all = ["1.3.6.1.4.1.311.97.1.0", "1.3.6.1.5.5.7.3.3", identity];
        // Each signing run has a new leaf: any thumbprint is accepted while it is WinVerifyTrust's own.
        for leaf in ["a", "b"] {
            let native = leaf.repeat(64);
            assert!(check(evidence(&native, subject, true, root, root_sha256, &all), &native).is_ok());
            assert!(check(evidence(&native, subject, true, root, &root_sha256.to_uppercase(), &all), &native).is_ok());
        }
        let native = "c".repeat(64);
        // The same subject under another root, a broken chain, the root's name in another case,
        // or a root with the right name but another certificate.
        for (chained, other_root) in [(true, "CN=Attacker Root, O=Learning Machines Inc, C=US"), (false, root), (true, "")] {
            assert!(check(evidence(&native, subject, chained, other_root, root_sha256, &all), &native).is_err());
        }
        assert!(check(evidence(&native, subject, true, &root.to_lowercase(), root_sha256, &all), &native).is_err());
        assert!(check(evidence(&native, subject, true, root, &"e".repeat(64), &all), &native).is_err());
        // The identity EKU or code signing missing, even with every other usage present.
        assert!(check(evidence(&native, subject, true, root, root_sha256, &["1.3.6.1.5.5.7.3.3", "1.3.6.1.4.1.311.97.1.0"]), &native).is_err());
        assert!(check(evidence(&native, subject, true, root, root_sha256, &[identity]), &native).is_err());
        assert!(check(evidence(&native, subject, true, root, root_sha256, &[]), &native).is_err());
        // Another subject, even with this identity's EKU and root.
        for other in ["CN=Learning Machines Inc", "CN=Learning Machines Inc, O=Learning Machines Inc, L=Atlanta, ST=Georgia, C=US"] {
            assert!(check(evidence(&native, other, true, root, root_sha256, &all), &native).is_err());
        }
        // A validly signed Microsoft executable (the CI reference pwsh.exe) is refused by the pin.
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/windows-signed-reference.json")).unwrap();
        let pwsh = &fixture["artifact"];
        let ekus: Vec<&str> = pwsh["ekus"].as_array().unwrap().iter().map(|eku| eku.as_str().unwrap()).collect();
        let foreign =
            evidence(&native, pwsh["subject"].as_str().unwrap(), true, pwsh["root"].as_str().unwrap(), pwsh["root_sha256"].as_str().unwrap(), &ekus);
        assert_eq!(
            check(foreign, &native).unwrap_err(),
            "The Nightly executable requires a valid timestamped signature from the exact pinned publisher, chain root and key usage."
        );
        // A publisher missing any part is not configured, so nothing is queried.
        for incomplete in [
            WindowsPublisher { root: "", ..*publisher },
            WindowsPublisher { root_sha256: "not-a-hash", ..*publisher },
            WindowsPublisher { ekus: &[], ..*publisher },
            WindowsPublisher { ekus: &[""], ..*publisher },
        ] {
            assert!(windows_metadata_with(path, Some(&incomplete), &native, |_| panic!("queried without a configured publisher")).is_err());
        }
    }

    #[test]
    fn windows_notice_names_do_not_authorize_binary_payloads() {
        assert!(textual_notice(b"An authored notice.\nCopyright\tTest\r\n"));
        for bytes in [b"MZ\0binary".to_vec(), vec![0xff], vec![b'x'; 1024 * 1024 + 1]] {
            assert!(!textual_notice(&bytes));
        }
    }

    fn test_system_directory() -> std::path::PathBuf {
        if cfg!(windows) {
            std::path::PathBuf::from(r"C:\os-selected\Windows\System32")
        } else {
            std::path::PathBuf::from("/os-selected/Windows/System32")
        }
    }

    #[test]
    fn windows_helper_is_os_selected_and_inherited_lookup_inputs_are_overridden() {
        let directory = test_system_directory();
        let command = windows_command(Path::new("owned.exe"), || Ok(directory.clone())).unwrap();
        assert!(Path::new(command.get_program()).starts_with(&directory));
        // Explicit child values override any hostile inherited SystemRoot/PATH/PSModulePath;
        // the only helper selection input is the successful native directory result.
        let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(env.get(std::ffi::OsStr::new("SystemRoot")), Some(&directory.parent().map(Path::as_os_str)));
        assert_eq!(env.get(std::ffi::OsStr::new("PATH")), Some(&Some(directory.as_os_str())));
        assert!(Path::new(env[std::ffi::OsStr::new("PSModulePath")].unwrap()).starts_with(&directory));
        assert!(windows_command(Path::new("owned.exe"), || Err("native system directory unavailable".into())).is_err());
        assert!(windows_command(Path::new("owned.exe"), || Ok(std::path::PathBuf::from("relative-attacker-root"))).is_err());
    }

    #[test]
    fn windows_query_keeps_owned_paths_as_data_not_executable_script() {
        let path = Path::new(r"C:\private path\a'; throw 'injected' ; [x].exe");
        let command = windows_command(path, || Ok(test_system_directory())).unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args.last(), Some(&std::ffi::OsStr::new(WINDOWS_QUERY)));
        assert!(!WINDOWS_QUERY.contains(path.to_str().unwrap()));
        let supplied = command.get_envs().find(|(key, _)| *key == "CRAFT_NIGHTLY_VERIFIED_FILE").unwrap().1;
        assert_eq!(supplied, Some(path.as_os_str()));
        let seen = std::cell::Cell::new(false);
        assert!(
            windows_metadata_with(path, None, "", |_| {
                seen.set(true);
                Err("must not query without trust".into())
            })
            .is_err()
        );
        assert!(!seen.get());
    }
    #[derive(Debug, PartialEq, Eq)]
    enum SignedFixtureResource {
        Missing,
        Supplied(std::path::PathBuf),
    }

    fn signed_fixture_resource(path: Option<std::ffi::OsString>, required: Option<std::ffi::OsString>) -> Result<SignedFixtureResource, String> {
        let required = match required {
            None => false,
            Some(value) if value.is_empty() => false,
            Some(value) if value == "1" => true,
            Some(_) => return Err("CRAFT_NIGHTLY_SIGNED_FIXTURE_REQUIRED accepts only 1, unset or empty.".into()),
        };
        match path.filter(|value| !value.is_empty()) {
            Some(path) => {
                let path = std::path::PathBuf::from(path);
                if !path.is_absolute() { return Err("CRAFT_NIGHTLY_SIGNED_FIXTURE must be an absolute path to the pinned external executable.".into()); }
                Ok(SignedFixtureResource::Supplied(path))
            }
            None if required => Err("CRAFT_NIGHTLY_SIGNED_FIXTURE_REQUIRED=1 requires CRAFT_NIGHTLY_SIGNED_FIXTURE pointing to the pinned signed reference; no operational check ran.".into()),
            None => Ok(SignedFixtureResource::Missing),
        }
    }

    #[test]
    fn signed_fixture_required_mode_cannot_hide_missing_invalid_or_configured_inputs() {
        use std::ffi::OsString;
        assert_eq!(signed_fixture_resource(None, None).unwrap(), SignedFixtureResource::Missing);
        assert_eq!(signed_fixture_resource(Some(OsString::new()), Some(OsString::new())).unwrap(), SignedFixtureResource::Missing);
        assert!(signed_fixture_resource(None, Some("1".into())).is_err());
        assert!(signed_fixture_resource(Some(OsString::new()), Some("1".into())).is_err());
        for value in ["0", "true", " 1 "] {
            assert!(signed_fixture_resource(None, Some(value.into())).is_err());
        }
        let root = test_system_directory();
        // A supplied input is never converted into an optional-resource skip, even when its
        // actual file/crypto checks will fail; required=false does not forgive bad fixtures.
        assert_eq!(signed_fixture_resource(Some(root.clone().into_os_string()), None).unwrap(), SignedFixtureResource::Supplied(root));
        assert!(signed_fixture_resource(Some("relative-fixture.exe".into()), None).is_err());
    }

    /// The production publisher check on executables supplied by a Windows guest run:
    /// CRAFT_NIGHTLY_PRODUCTION_EXE (a released PhotoCraft photocraft.exe) must pass, and
    /// CRAFT_NIGHTLY_FOREIGN_EXE must be refused by the publisher pin itself. The foreign file
    /// must therefore be validly signed by someone else (pwsh.exe, say): a test-signed or
    /// unsigned file is refused earlier by WinVerifyTrust and fails this test, as do a timeout
    /// or a query error. Each is checked through `authenticate_windows` itself, as a payload's
    /// photocraft.exe. Without them this lists a skip, which is not coverage.
    #[cfg(windows)]
    #[test]
    fn production_publisher_check_on_supplied_executables() {
        use std::io::Write;
        let check = |supplied: std::ffi::OsString| {
            let directory = tempfile::tempdir().unwrap();
            std::fs::copy(supplied, directory.path().join("photocraft.exe")).expect("supplied executable must be readable");
            authenticate_windows(directory.path(), &[std::path::PathBuf::from("photocraft.exe")]).map(drop)
        };
        let production = std::env::var_os("CRAFT_NIGHTLY_PRODUCTION_EXE");
        let foreign = std::env::var_os("CRAFT_NIGHTLY_FOREIGN_EXE");
        if production.is_none() && foreign.is_none() {
            writeln!(std::io::stdout().lock(), "SKIP production_publisher_check_on_supplied_executables: no supplied executables").unwrap();
            return;
        }
        if let Some(path) = production {
            check(path).expect("the released PhotoCraft executable must pass the production publisher check");
            writeln!(std::io::stdout().lock(), "PASS production executable accepted").unwrap();
        }
        if let Some(path) = foreign {
            let refused = check(path).expect_err("another publisher's executable must be refused");
            assert_eq!(
                refused, "The Nightly executable requires a valid timestamped signature from the exact pinned publisher, chain root and key usage.",
                "the foreign executable must pass native verification and be refused by the pin, not by a timeout or another error"
            );
            writeln!(std::io::stdout().lock(), "PASS foreign executable refused by the publisher pin: {refused}").unwrap();
        }
    }

    /// Released MIT fixture only, never a PhotoCraft production identity. This native integration
    /// case may need OCSP/CRL network access and exceeds unit-test timing because it exercises
    /// the actual OS verifier and fixed subprocess. Required runs must provision the resource.
    #[cfg(windows)]
    #[test]
    fn actual_windows_signed_fixture_verifies_metadata_and_is_held_until_released() {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        let resource =
            signed_fixture_resource(std::env::var_os("CRAFT_NIGHTLY_SIGNED_FIXTURE"), std::env::var_os("CRAFT_NIGHTLY_SIGNED_FIXTURE_REQUIRED"))
                .unwrap();
        let reference = match resource {
            SignedFixtureResource::Supplied(path) => dunce::canonicalize(path).expect("configured signed fixture must exist"),
            SignedFixtureResource::Missing => {
                // Direct I/O bypasses libtest's captured println/eprintln, so this reason is
                // listed even by plain cargo test. Its eventual `ok` is NOT native coverage.
                writeln!(std::io::stdout().lock(),"SKIP actual_windows_signed_fixture_verifies_metadata_and_is_held_until_released: pinned external signed fixture unavailable; set CRAFT_NIGHTLY_SIGNED_FIXTURE and CRAFT_NIGHTLY_SIGNED_FIXTURE_REQUIRED=1 for required operational coverage").unwrap();
                return;
            }
        };
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/windows-signed-reference.json")).unwrap();
        let artifact = &fixture["artifact"];
        let ekus: Vec<&str> = artifact["ekus"].as_array().unwrap().iter().map(|eku| eku.as_str().unwrap()).collect();
        let publisher = WindowsPublisher {
            subject: artifact["subject"].as_str().unwrap(),
            root: artifact["root"].as_str().unwrap(),
            root_sha256: artifact["root_sha256"].as_str().unwrap(),
            ekus: &ekus,
        };
        let _held_reference =
            std::fs::OpenOptions::new().read(true).share_mode(1).open(&reference).expect("configured fixture must be readable and stable");
        assert_eq!(
            std::fs::metadata(&reference).unwrap().len(),
            artifact["size"].as_u64().unwrap(),
            "fixture size must match the independently checked official ZIP entry"
        );
        assert_eq!(
            crate::http::sha256_file(&reference).unwrap(),
            artifact["sha256"].as_str().unwrap(),
            "fixture must be the exact pinned official ZIP entry"
        );
        let verifier =
            verifysign::CodeSignVerifier::new(verifysign::VerifyPolicy::new().revocation(verifysign::Revocation::Chain).allow_network(true)).unwrap();
        let native = verifier.verify_file(&reference).expect("configured fixture requires genuine native chain/revocation trust");
        windows_metadata_with(&reference, Some(&publisher), &native.sha256_thumbprint(), read_windows_metadata)
            .expect("configured fixture requires exact reviewed subject, real embedded timestamped metadata and same leaf");
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("photocraft.exe");
        std::fs::copy(&reference, &executable).unwrap();
        std::fs::OpenOptions::new().write(true).open(&executable).expect("fixture must be writable before authentication");
        let before = crate::http::sha256_file(&executable).unwrap();
        let held = authenticate_windows_as(directory.path(), &[std::path::PathBuf::from("photocraft.exe")], &publisher)
            .expect("real native trust, fixed full-subject/timestamp query and same-leaf binding");
        let renamed = directory.path().join("renamed.exe");
        assert!(std::fs::OpenOptions::new().write(true).open(&executable).is_err(), "held executable admitted a writer");
        assert!(std::fs::remove_file(&executable).is_err(), "held executable admitted deletion");
        assert!(std::fs::rename(&executable, &renamed).is_err(), "held executable admitted rename");
        assert_eq!(crate::http::sha256_file(&executable).unwrap(), before);
        drop(held);
        std::fs::OpenOptions::new().write(true).open(&executable).expect("write sharing must be restored after guard release");
        std::fs::rename(&executable, &renamed).expect("rename must succeed after release");
        std::fs::remove_file(renamed).expect("ordinary fixture cleanup after release");
    }
}
