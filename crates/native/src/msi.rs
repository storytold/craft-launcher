//! What a Windows Installer package must prove before it's run with nobody watching.
//!
//! When the launcher installs an MSI, Windows shows the user a UAC prompt naming the package's
//! publisher, and the user decides. The update service (`service`) runs packages as LocalSystem
//! with no prompt, so it checks for itself first, as the macOS install checks the bundle's
//! Developer ID team and version (`install::macos::Expect`):
//!
//! - the package's Authenticode signature is valid and was made by the Craft Apps' publisher, as
//!   Windows itself judges it (`Get-AuthenticodeSignature`): the certificate's whole subject
//!   ([`SIGNER_SUBJECT`]) must match, not only its name, which a company elsewhere could share;
//! - the package is the app's own (its `ProductName` is one of the app's names, its
//!   `Manufacturer` is [`MANUFACTURER`]);
//! - it is the version the release promises (`ProductVersion`, which MSI keeps numeric).
//!
//! That is on top of the published SHA-256 every download is checked against.

use std::path::Path;

use artcraft_launcher_core::version::{self, Version};
use serde::Deserialize;

/// The name on the certificate every Craft App's and ArtCraft Launcher's Windows packages are
/// signed with (Azure Trusted Signing), for messages.
pub const SIGNER: &str = "Learning Machines Inc";
/// That certificate's whole subject, as Windows writes it, which a package's signer must match.
/// The certificate itself is renewed every few days and its issuing CA changes from time to time;
/// the subject is the publisher's verified identity and stays.
pub const SIGNER_SUBJECT: &str = "CN=Learning Machines Inc, O=Learning Machines Inc, L=Atlanta, S=Georgia, C=US";
/// The `Manufacturer` of those packages (also the `Publisher` of their uninstall entries).
pub const MANUFACTURER: &str = "Learning Machines LLC";

/// What a package says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Facts {
    /// Windows' verdict on the signature: `Valid`, `NotSigned`, `HashMismatch`, `NotTrusted`, ….
    pub status: String,
    /// The signing certificate's name, when there is a signature.
    pub signer: Option<String>,
    /// The signing certificate's whole subject (`CN=…, O=…, L=…, S=…, C=…`).
    pub subject: Option<String>,
    pub product_name: Option<String>,
    pub manufacturer: Option<String>,
    pub product_version: Option<String>,
}

/// What a package must be.
#[derive(Clone, Debug)]
pub struct Expect<'a> {
    /// The product names it may carry (an app's current and former names).
    pub names: Vec<&'a str>,
    /// The version it must be; `None` when the release's tag isn't a version.
    pub version: Option<&'a Version>,
    /// The whole subject of the certificate it must be signed with ([`SIGNER_SUBJECT`]); `None`
    /// only in tests.
    pub signer: Option<&'a str>,
}

/// Checks `facts` against `expect`; the error says what didn't match, for the log and the user.
pub fn check(facts: &Facts, expect: &Expect<'_>) -> Result<(), String> {
    if let Some(subject) = expect.signer {
        if facts.status != "Valid" {
            let status = if facts.status.is_empty() { "unknown" } else { facts.status.as_str() };
            return Err(format!("the installer's signature isn't valid ({status}), so it wasn't installed"));
        }
        if facts.subject.as_deref() != Some(subject) {
            // The same name somewhere else: the whole subject shows where.
            let who = match (facts.signer.as_deref(), facts.subject.as_deref()) {
                (Some(SIGNER), Some(whole)) | (None, Some(whole)) => whole,
                (Some(name), _) => name,
                (None, None) => "nobody known",
            };
            return Err(format!("the installer is signed by {who}, not {SIGNER}, so it wasn't installed"));
        }
    }
    let name = facts.product_name.as_deref().unwrap_or_default();
    if !expect.names.contains(&name) {
        return Err(format!("the installer is for \"{name}\", not {}, so it wasn't installed", expect.names.first().copied().unwrap_or("this app")));
    }
    if facts.manufacturer.as_deref() != Some(MANUFACTURER) {
        return Err("the installer doesn't come from the Craft Apps' publisher, so it wasn't installed".into());
    }
    if let Some(want) = expect.version {
        let found = facts.product_version.as_deref().and_then(version::parse);
        // MSI versions are numeric: a pre-release installs as its numbers.
        if !found.as_ref().is_some_and(|f| (f.major, f.minor, f.patch) == (want.major, want.minor, want.patch)) {
            let found = facts.product_version.as_deref().unwrap_or("unknown");
            return Err(format!("the installer is version {found}, not {want}, so it wasn't installed"));
        }
    }
    Ok(())
}

/// Reads what `msi` says about itself (signature and identity), and checks it.
pub fn verify(msi: &Path, expect: &Expect<'_>) -> Result<(), String> {
    check(&inspect(msi)?, expect)
}

/// Asks Windows about `msi`: its Authenticode signature (`Get-AuthenticodeSignature`) and three
/// properties from its database (through the Windows Installer's own COM object), in one
/// PowerShell run. The path reaches the script through the environment, never its text.
#[cfg(windows)]
pub fn inspect(msi: &Path) -> Result<Facts, String> {
    let powershell = crate::install::windows::system32().join(r"WindowsPowerShell\v1.0\powershell.exe");
    let mut cmd = crate::process::command(&powershell);
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encode_command(INSPECT)]).env(PATH_VAR, msi);
    let out = crate::process::run_command(cmd, "PowerShell", std::time::Duration::from_secs(180))
        .map_err(|e| format!("couldn't check the installer's signature ({e})"))?;
    parse_facts(&out)
}

#[cfg(not(windows))]
pub fn inspect(_msi: &Path) -> Result<Facts, String> {
    Err("Windows Installer packages can only be checked on Windows".into())
}

/// Where [`INSPECT`] finds the package's path.
#[cfg_attr(not(windows), allow(dead_code))]
const PATH_VAR: &str = "ARTCRAFT_LAUNCHER_MSI";

/// The PowerShell behind [`inspect`]. Prints one line of JSON (see [`Facts`]).
#[cfg_attr(not(windows), allow(dead_code))]
const INSPECT: &str = r#"
$ErrorActionPreference = 'Stop'
$path = $env:ARTCRAFT_LAUNCHER_MSI
$sig = Get-AuthenticodeSignature -LiteralPath $path
$signer = $null
$subject = $null
if ($sig.SignerCertificate) {
  $signer = $sig.SignerCertificate.GetNameInfo([System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
  $subject = $sig.SignerCertificate.Subject
}
$props = @{}
$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($path, 0))
foreach ($name in 'ProductName', 'Manufacturer', 'ProductVersion') {
  $query = 'SELECT `Value` FROM `Property` WHERE `Property` = ''' + $name + ''''
  $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($query))
  [void]$view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null)
  $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
  if ($record) { $props[$name] = $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, 1) }
  [void]$view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null)
}
[pscustomobject]@{
  status = "$($sig.Status)"
  signer = $signer
  subject = $subject
  productName = $props['ProductName']
  manufacturer = $props['Manufacturer']
  productVersion = $props['ProductVersion']
} | ConvertTo-Json -Compress
"#;

/// Reads [`INSPECT`]'s answer: the last line that is a JSON object.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_facts(out: &str) -> Result<Facts, String> {
    out.lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
        .ok_or_else(|| "couldn't read the installer's signature".to_string())
}

/// A script for `powershell -EncodedCommand`: UTF-16LE, then base64. Nothing in it can be taken
/// apart by the command line's quoting.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

/// Standard base64 with padding.
#[cfg_attr(not(windows), allow(dead_code))]
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol = |i: u32| char::from(ALPHABET.get((i & 63) as usize).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (b(0) << 16) | (b(1) << 8) | b(2);
        out.push(symbol(n >> 18));
        out.push(symbol(n >> 12));
        out.push(if chunk.len() > 1 { symbol(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { symbol(n) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photocraft_0_8_2() -> Facts {
        Facts {
            status: "Valid".into(),
            signer: Some(SIGNER.into()),
            subject: Some(SIGNER_SUBJECT.into()),
            product_name: Some("PhotoCraft".into()),
            manufacturer: Some(MANUFACTURER.into()),
            product_version: Some("0.8.2".into()),
        }
    }

    fn expect(version: &Version) -> Expect<'_> {
        Expect { names: vec!["PhotoCraft"], version: Some(version), signer: Some(SIGNER_SUBJECT) }
    }

    #[test]
    fn the_real_thing_passes() {
        let v = version::parse("0.8.2").unwrap();
        assert_eq!(check(&photocraft_0_8_2(), &expect(&v)), Ok(()));
        // A pre-release installs as its numbers.
        let rc = version::parse("0.8.2-rc.1").unwrap();
        assert_eq!(check(&photocraft_0_8_2(), &expect(&rc)), Ok(()));
        // A former name still counts (PdfCraft's MSI said PrintCraft until 0.2.1).
        let facts = Facts { product_name: Some("PrintCraft".into()), ..photocraft_0_8_2() };
        let names = Expect { names: vec!["PdfCraft", "PrintCraft"], ..expect(&v) };
        assert_eq!(check(&facts, &names), Ok(()));
    }

    #[test]
    fn a_bad_or_foreign_signature_is_refused() {
        let v = version::parse("0.8.2").unwrap();
        for status in ["NotSigned", "HashMismatch", "NotTrusted", "UnknownError", ""] {
            let facts = Facts { status: status.into(), ..photocraft_0_8_2() };
            let err = check(&facts, &expect(&v)).unwrap_err();
            assert!(err.contains("signature isn't valid"), "{status}: {err}");
        }
        let facts = Facts { signer: Some("Someone Else Ltd".into()), subject: Some("CN=Someone Else Ltd".into()), ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).unwrap_err().contains("signed by Someone Else Ltd"));
        let facts = Facts { signer: None, subject: None, ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).is_err());
        // Close isn't good enough.
        let close = "CN=Learning Machines Inc., O=Learning Machines Inc., L=Atlanta, S=Georgia, C=US";
        let facts = Facts { signer: Some("Learning Machines Inc.".into()), subject: Some(close.into()), ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).is_err());
    }

    #[test]
    fn the_same_name_somewhere_else_is_refused() {
        let v = version::parse("0.8.2").unwrap();
        let elsewhere = "CN=Learning Machines Inc, O=Learning Machines Inc, L=Springfield, S=Oregon, C=US";
        let facts = Facts { subject: Some(elsewhere.into()), ..photocraft_0_8_2() };
        let err = check(&facts, &expect(&v)).unwrap_err();
        assert!(err.contains("L=Springfield, S=Oregon"), "{err}");
        // A subject with more in it (a department, say) isn't the same certificate either.
        let more = format!("CN=Learning Machines Inc, OU=Marketing, {}", SIGNER_SUBJECT.trim_start_matches("CN=Learning Machines Inc, "));
        let facts = Facts { subject: Some(more), ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).is_err());
        // Nor is the right name with no subject to go with it.
        let facts = Facts { subject: None, ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).is_err());
    }

    #[test]
    fn another_product_or_version_is_refused() {
        let v = version::parse("0.8.2").unwrap();
        let facts = Facts { product_name: Some("WordCraft".into()), ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).unwrap_err().contains("\"WordCraft\", not PhotoCraft"));
        let facts = Facts { manufacturer: Some("Contoso".into()), ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).unwrap_err().contains("publisher"));
        // An older package dressed up as the latest release (a downgrade) never installs.
        let facts = Facts { product_version: Some("0.1.0".into()), ..photocraft_0_8_2() };
        assert_eq!(check(&facts, &expect(&v)), Err("the installer is version 0.1.0, not 0.8.2, so it wasn't installed".into()));
        let facts = Facts { product_version: None, ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).is_err());
        assert!(check(&Facts::default(), &expect(&v)).is_err());
    }

    #[test]
    fn reads_what_powershell_printed() {
        let out = "WARNING: something chatty\r\n{\"status\":\"Valid\",\"signer\":\"Learning Machines Inc\",\
                   \"subject\":\"CN=Learning Machines Inc, O=Learning Machines Inc, L=Atlanta, S=Georgia, C=US\",\"productName\":\"PhotoCraft\",\
                   \"manufacturer\":\"Learning Machines LLC\",\"productVersion\":\"0.8.2\"}\r\n";
        assert_eq!(parse_facts(out).unwrap(), photocraft_0_8_2());
        let unsigned = r#"{"status":"NotSigned","signer":null,"subject":null,"productName":"PhotoCraft","manufacturer":"Learning Machines LLC","productVersion":"0.8.2"}"#;
        let unsigned = parse_facts(unsigned).unwrap();
        assert_eq!((unsigned.signer, unsigned.subject), (None, None));
        assert!(parse_facts("Get-AuthenticodeSignature : Cannot find path").is_err());
        assert!(parse_facts("").is_err());
    }

    #[test]
    fn base64_matches_the_standard() {
        for (raw, encoded) in
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(base64(raw.as_bytes()), encoded, "{raw}");
        }
        // `powershell -EncodedCommand` takes UTF-16LE.
        assert_eq!(encode_command("dir"), "ZABpAHIA");
    }

    /// Windows reads any package: run with `ANY_MSI=C:\Windows\Installer\<some>.msi cargo test -p
    /// artcraft-launcher-native -- --ignored windows_reads_any_msi`.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs an MSI on this machine"]
    fn windows_reads_any_msi() {
        let msi = std::env::var("ANY_MSI").expect("set ANY_MSI to an MSI");
        let facts = inspect(Path::new(&msi)).unwrap();
        assert!(!facts.status.is_empty() && facts.product_name.is_some() && facts.product_version.is_some(), "{facts:?}");
        eprintln!("{facts:?}");
    }

    /// The real check against a real release package. Run with `CRAFT_MSI=C:\path\to\photocraft-0.8.2-windows-x64.msi
    /// CRAFT_MSI_VERSION=0.8.2 cargo test -p artcraft-launcher-native -- --ignored a_real_craft_app_msi`.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a Craft App MSI from a real release"]
    fn a_real_craft_app_msi_passes() {
        let msi = std::env::var("CRAFT_MSI").expect("set CRAFT_MSI to a Craft App MSI");
        let want = version::parse(&std::env::var("CRAFT_MSI_VERSION").expect("set CRAFT_MSI_VERSION")).unwrap();
        let facts = inspect(Path::new(&msi)).unwrap();
        let names: Vec<&str> = artcraft_launcher_core::SUITE.iter().map(|a| a.name).collect();
        assert_eq!(check(&facts, &Expect { names, version: Some(&want), signer: Some(SIGNER_SUBJECT) }), Ok(()), "{facts:?}");
    }
}
