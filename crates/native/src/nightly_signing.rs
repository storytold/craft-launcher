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

    // Parse the documented --status-fd protocol, not OpenPGP packets or human-readable output.
    // GnuPG checks crypto/key validity; we independently require one exact primary identity,
    // a binary detached signature, a strong digest, and no revocation/expiry/error status.
    fn status(output: &crate::process::RunOutput, expected: &str, now: i64) -> Result<(), String> {
        if !output.success || output.stdout.len() > 64 * 1024 {
            return Err("The Nightly OpenPGP verifier failed or returned excessive status output.".into());
        }
        let mut valid = 0;
        let mut good = 0;
        let mut started = 0;
        for line in output.stdout.lines() {
            let fields: Vec<_> = line.strip_prefix("[GNUPG:] ").ok_or("Unexpected Nightly OpenPGP status output.")?.split_whitespace().collect();
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
                        return Err("The Nightly detached signature does not have the pinned primary identity and validity.".into());
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
                _ => return Err("The Nightly OpenPGP verifier reported an unsupported, expired, revoked or error status.".into()),
            }
        }
        if valid != 1 || good != 1 || started != 1 {
            return Err("The Nightly archive requires exactly one valid signature from the pinned signer.".into());
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

    pub(crate) fn prepare(
        file: &Path,
        asset: &Asset,
        signature: &Download,
        http: &crate::http::Http,
        paths: &Paths,
    ) -> Result<VerifiedArchive, String> {
        let signer = crate::nightly_trust::LINUX_SIGNER
            .as_ref()
            .ok_or("No independently trusted Linux Nightly signer is configured; the installed copy and data were left unchanged.")?;
        let bytes = http.get_bytes(&signature.url, MAX_SIGNATURE_BYTES)?.ok_or("The Nightly archive signature is missing.")?;
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
    ) -> Result<VerifiedArchive, String> {
        use sha2::{Digest, Sha256};
        if !fingerprint(signer.primary_fingerprint) {
            return Err("The pinned Linux Nightly primary fingerprint is unavailable or malformed.".into());
        }
        armor(signer.public_key.as_bytes(), "PUBLIC KEY BLOCK", MAX_PUBLIC_KEY)?;
        armor(bytes, "SIGNATURE", MAX_SIGNATURE_BYTES)?;
        if bytes.len() as u64 != signature.size || !format!("{:x}", Sha256::digest(bytes)).eq_ignore_ascii_case(&signature.sha256) {
            return Err("The Nightly detached signature does not match its published size and checksum.".into());
        }
        crate::nightly_profile::validate_paths(paths)?;
        std::fs::create_dir_all(&paths.cache).map_err(|error| error.to_string())?;
        let home = tempfile::Builder::new().prefix("nightly-auth-").tempdir_in(&paths.cache).map_err(|error| error.to_string())?;
        let snapshot = home.path().join("archive.tar.gz");
        let mut input = std::fs::File::open(file).map_err(|error| error.to_string())?.take(asset.size.saturating_add(1));
        if asset.size == 0 || asset.size > MAX_ARCHIVE {
            return Err("The Nightly archive exceeds its authentication limit.".into());
        }
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(&snapshot).map_err(|error| error.to_string())?;
        let copied = std::io::copy(&mut input, &mut out).map_err(|error| error.to_string())?;
        if copied != asset.size
            || asset.sha256.as_ref().is_none_or(|hash| crate::http::sha256_file(&snapshot).map_or(true, |actual| !actual.eq_ignore_ascii_case(hash)))
        {
            return Err("The exact Nightly archive bytes do not match the verified release.".into());
        }
        drop(out);
        let public_key = home.path().join("pinned-public.asc");
        let detached = home.path().join("archive.tar.gz.asc");
        std::fs::write(&public_key, signer.public_key).map_err(|error| error.to_string())?;
        std::fs::write(&detached, bytes).map_err(|error| error.to_string())?;
        let imported = gpg(home.path(), &["--import".as_ref(), public_key.as_os_str()], program)?;
        if !imported.success {
            return Err("The pinned Linux Nightly public key could not be imported.".into());
        }
        let verified = gpg(home.path(), &["--verify".as_ref(), detached.as_os_str(), snapshot.as_os_str()], program)?;
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
pub(crate) use linux::prepare as prepare_linux;

#[cfg(any(windows, test))]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsEvidence {
    subject: String,
    valid: bool,
    timestamp: bool,
    embedded: bool,
    thumbprint: String,
}

#[cfg(any(windows, test))]
fn match_windows(evidence: &WindowsEvidence, expected: Option<&str>, native_thumbprint: &str) -> Result<(), String> {
    let expected = expected.filter(|subject| !subject.is_empty()).ok_or("No independently trusted Windows Nightly publisher is configured.")?;
    if evidence.valid
        && evidence.timestamp
        && evidence.embedded
        && evidence.subject == expected
        && native_thumbprint.len() == 64
        && native_thumbprint.bytes().all(|byte| byte.is_ascii_hexdigit())
        && evidence.thumbprint.eq_ignore_ascii_case(native_thumbprint)
    {
        Ok(())
    } else {
        Err("The Nightly executable requires a valid timestamped signature from the exact pinned publisher.".into())
    }
}

#[cfg(any(windows, test))]
fn windows_metadata_with(
    path: &std::path::Path,
    expected: Option<&str>,
    native_thumbprint: &str,
    query: impl FnOnce(&std::path::Path) -> Result<String, String>,
) -> Result<(), String> {
    if expected.is_none_or(str::is_empty) {
        return Err("No independently trusted Windows Nightly publisher is configured.".into());
    }
    let text = query(path)?;
    if text.len() > 16 * 1024 {
        return Err("The Nightly Authenticode evidence exceeds its limit.".into());
    }
    let evidence: WindowsEvidence = serde_json::from_str(&text).map_err(|_| "The Nightly Authenticode evidence is malformed.")?;
    match_windows(&evidence, expected, native_thumbprint)
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
@{ subject = $s.SignerCertificate.Subject; valid = ($s.Status.ToString() -ceq 'Valid');
   timestamp = ($null -ne $s.TimeStamperCertificate); embedded = ($s.SignatureType.ToString() -ceq 'Authenticode');
   thumbprint = $thumb } | Microsoft.PowerShell.Utility\ConvertTo-Json -Compress
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
    let expected = crate::nightly_trust::WINDOWS_SUBJECT
        .ok_or("No independently trusted Windows Nightly publisher is configured; the installed copy and data were left unchanged.")?;
    authenticate_windows_as(root, files, expected)
}

// This private helper receives only source trust from the production wrapper. A cfg(test)
// native OS fixture supplies its own independently verified Microsoft reference identity.
#[cfg(windows)]
fn authenticate_windows_as(root: &std::path::Path, files: &[std::path::PathBuf], expected: &str) -> Result<std::fs::File, String> {
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
    windows_metadata_with(&executable, Some(expected), &native_thumbprint, read_windows_metadata)?;
    Ok(held)
}

#[cfg(windows)]
fn read_windows_metadata(path: &std::path::Path) -> Result<String, String> {
    let command = windows_command(path, || winsafe::GetSystemDirectory().map(std::path::PathBuf::from).map_err(|error| error.to_string()))?;
    let output = crate::process::run_command_with_output(command, "Nightly Authenticode metadata", std::time::Duration::from_secs(60))?;
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
        let native = "a".repeat(64);
        let evidence = json!({"subject":expected,"valid":true,"timestamp":true,"embedded":true,"thumbprint":native});
        let path = Path::new("owned-photocraft.exe");
        let check = |value: &serde_json::Value, pin| windows_metadata_with(path, pin, &native, |_| Ok(value.to_string()));
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
            assert!(windows_metadata_with(path, Some(expected), &native, |_| Ok(malformed)).is_err());
        }
        for failure in ["tool unavailable", "native query timed out", "native trust error"] {
            assert!(windows_metadata_with(path, Some(expected), &native, |_| Err(failure.into())).is_err());
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
        let expected = artifact["subject"].as_str().unwrap();
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
        windows_metadata_with(&reference, Some(expected), &native.sha256_thumbprint(), read_windows_metadata)
            .expect("configured fixture requires exact reviewed subject, real embedded timestamped metadata and same leaf");
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("photocraft.exe");
        std::fs::copy(&reference, &executable).unwrap();
        std::fs::OpenOptions::new().write(true).open(&executable).expect("fixture must be writable before authentication");
        let before = crate::http::sha256_file(&executable).unwrap();
        let held = authenticate_windows_as(directory.path(), &[std::path::PathBuf::from("photocraft.exe")], expected)
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
