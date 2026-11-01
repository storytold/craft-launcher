//! What a Windows Installer package must prove before it's run with nobody watching.
//!
//! When the launcher installs an MSI, Windows shows the user a UAC prompt naming the package's
//! publisher, and the user decides. The update service (`service`) runs packages as LocalSystem
//! with no prompt, so it checks for itself first, as the macOS install checks the bundle's
//! Developer ID team and version (`install::macos::Expect`):
//!
//! - the package's Authenticode signature is valid and chains to a root Windows trusts, as
//!   WinTrust judges it (through `verifysign`), and its certificate is the Craft Apps'
//!   publisher's ([`PUBLISHER`]): that name, organization and country, no department, issued
//!   by Microsoft's code signing service;
//! - the package is the app's own (its `ProductName` is one of the app's names, its
//!   `Manufacturer` is [`MANUFACTURER`]);
//! - it is the version the release promises (`ProductVersion`, which MSI keeps numeric).
//!
//! That is on top of the published SHA-256 every download is checked against. No script runs:
//! the properties come from the Windows Installer's own automation object, so the package is read
//! by the same `msi.dll` that `msiexec` installs it with, and only once its signature has passed.

use std::path::Path;

use artcraft_launcher_core::version::{self, Version};

/// The name on the certificate every Craft App's and ArtCraft Launcher's Windows packages are
/// signed with (Azure Trusted Signing).
pub const SIGNER: &str = "Learning Machines Inc";
/// The `Manufacturer` of those packages (also the `Publisher` of their uninstall entries).
pub const MANUFACTURER: &str = "Learning Machines LLC";

/// A signing certificate as a package must carry it.
#[derive(Clone, Copy, Debug)]
pub struct Publisher {
    pub common_name: &'static str,
    pub organization: &'static str,
    pub country: &'static str,
    /// The organization of the certificate authority that issued it.
    pub issuer: &'static str,
}

/// The publisher's certificate. The certificate itself is renewed every few days and its issuing
/// CA changes from time to time (`Microsoft ID Verified CS EOC CA 03`, …), so neither is pinned;
/// the verified identity and the issuing organization stay.
pub const PUBLISHER: Publisher = Publisher { common_name: SIGNER, organization: SIGNER, country: "US", issuer: "Microsoft Corporation" };

/// Who signed a package, as Windows reads its certificate once the signature is valid and trusted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signer {
    pub common_name: Option<String>,
    pub organization: Option<String>,
    /// A department within the organization (`OU`), which the publisher's certificate doesn't have.
    pub organization_unit: Option<String>,
    pub country: Option<String>,
    /// The organization of the certificate authority that issued it.
    pub issuer: Option<String>,
}

impl Signer {
    /// The certificate's names, for messages: `CN=…, O=…, OU=…, C=…, issued by …`.
    fn describe(&self) -> String {
        let parts: Vec<String> = [("CN", &self.common_name), ("O", &self.organization), ("OU", &self.organization_unit), ("C", &self.country)]
            .into_iter()
            .filter_map(|(key, value)| value.as_ref().map(|v| format!("{key}={v}")))
            .collect();
        let subject = if parts.is_empty() { "nobody known".to_string() } else { parts.join(", ") };
        match &self.issuer {
            Some(issuer) => format!("{subject}, issued by {issuer}"),
            None => subject,
        }
    }
}

impl Publisher {
    /// Whether `signer` is this publisher's certificate: every part the same, and nothing more.
    fn signed(&self, signer: &Signer) -> bool {
        signer.common_name.as_deref() == Some(self.common_name)
            && signer.organization.as_deref() == Some(self.organization)
            && signer.organization_unit.is_none()
            && signer.country.as_deref() == Some(self.country)
            && signer.issuer.as_deref() == Some(self.issuer)
    }
}

/// What a package says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// Who signed it, or why Windows doesn't accept its signature.
    pub signature: Result<Signer, String>,
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
    /// The certificate it must be signed with ([`PUBLISHER`]); `None` only in tests.
    pub signer: Option<&'a Publisher>,
}

/// Checks `facts`' signature against `expect`.
fn check_signature(facts: &Facts, expect: &Expect<'_>) -> Result<(), String> {
    let Some(publisher) = expect.signer else { return Ok(()) };
    let signer = facts.signature.as_ref().map_err(|why| format!("the installer's signature isn't valid ({why}), so it wasn't installed"))?;
    if publisher.signed(signer) {
        Ok(())
    } else {
        Err(format!("the installer is signed by {}, not {SIGNER}, so it wasn't installed", signer.describe()))
    }
}

/// Checks `facts` against `expect`; the error says what didn't match, for the log and the user.
pub fn check(facts: &Facts, expect: &Expect<'_>) -> Result<(), String> {
    check_signature(facts, expect)?;
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

/// Checks `msi`'s signature, then reads what it says about itself and checks that. A package
/// whose signature fails is never opened.
pub fn verify(msi: &Path, expect: &Expect<'_>) -> Result<(), String> {
    let unread = Facts { signature: signature(msi), product_name: None, manufacturer: None, product_version: None };
    check_signature(&unread, expect)?;
    let [product_name, manufacturer, product_version] = properties(msi)?;
    check(&Facts { product_name, manufacturer, product_version, ..unread }, expect)
}

/// Who signed `msi`, through WinTrust: the signature must be valid and chain to a root Windows
/// trusts before its certificate is read.
#[cfg(windows)]
pub fn signature(msi: &Path) -> Result<Signer, String> {
    let signed = verifysign::verify_file(msi).map_err(|e| e.to_string())?;
    let (subject, issuer) = (signed.subject_name(), signed.issuer_name());
    Ok(Signer {
        common_name: subject.common_name,
        organization: subject.organization,
        organization_unit: subject.organization_unit,
        country: subject.country,
        issuer: issuer.organization,
    })
}

#[cfg(not(windows))]
pub fn signature(_msi: &Path) -> Result<Signer, String> {
    Err("Windows Installer packages can only be checked on Windows".into())
}

/// The properties [`properties`] reads, in its answer's order.
#[cfg_attr(not(windows), allow(dead_code))]
const PROPERTIES: [&str; 3] = ["ProductName", "Manufacturer", "ProductVersion"];

/// `msi`'s [`PROPERTIES`], from its database, read through the Windows Installer's automation
/// object (`WindowsInstaller.Installer`) on a thread of its own, so COM starts there in the
/// apartment this asks for, whatever the caller's thread did.
#[cfg(windows)]
pub fn properties(msi: &Path) -> Result<[Option<String>; 3], String> {
    let path = msi.to_str().ok_or("the installer's path can't be read")?;
    std::thread::scope(|scope| scope.spawn(|| read_properties(path)).join()).map_err(|_| "couldn't read the installer".to_string())?
}

#[cfg(not(windows))]
pub fn properties(_msi: &Path) -> Result<[Option<String>; 3], String> {
    Err("Windows Installer packages can only be checked on Windows".into())
}

#[cfg(windows)]
fn read_properties(path: &str) -> Result<[Option<String>; 3], String> {
    use winsafe::{self as w, co, prelude::*};
    let failed = |e: &dyn std::fmt::Display| format!("couldn't read the installer ({e})");
    let object = |v: w::Variant| match v {
        w::Variant::Dispatch(d) => Ok(d),
        _ => Err("couldn't read the installer (the Windows Installer gave no answer)".to_string()),
    };
    // Declared first, so COM is shut down last, after every object below is released.
    let _com = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE).map_err(|e| failed(&e))?;
    let class = w::CLSIDFromProgID("WindowsInstaller.Installer").map_err(|e| failed(&e))?;
    let installer = w::CoCreateInstance::<w::IDispatch>(&class, None::<&w::IUnknown>, co::CLSCTX::INPROC_SERVER).map_err(|e| failed(&e))?;
    // 0: msiOpenDatabaseModeReadOnly.
    let database = object(installer.invoke_method("OpenDatabase", &[&w::Variant::from_str(path), &w::Variant::I4(0)]).map_err(|e| failed(&e))?)?;
    let mut values = [None, None, None];
    for (value, name) in values.iter_mut().zip(PROPERTIES) {
        let query = format!("SELECT `Value` FROM `Property` WHERE `Property` = '{name}'");
        let view = object(database.invoke_method("OpenView", &[&w::Variant::from_str(query)]).map_err(|e| failed(&e))?)?;
        view.invoke_method("Execute", &[]).map_err(|e| failed(&e))?;
        // `Fetch` answers a missing row with a null object, which winsafe can't take. Every
        // package has these three (Windows Installer's required properties), and only one signed
        // by the publisher gets this far.
        let record = object(view.invoke_method("Fetch", &[]).map_err(|e| failed(&e))?)?;
        if let w::Variant::Bstr(text) = record.invoke_get("StringData", &[&w::Variant::I4(1)]).map_err(|e| failed(&e))? {
            *value = Some(text);
        }
        view.invoke_method("Close", &[]).map_err(|e| failed(&e))?;
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publisher() -> Signer {
        Signer {
            common_name: Some(SIGNER.into()),
            organization: Some(SIGNER.into()),
            organization_unit: None,
            country: Some("US".into()),
            issuer: Some("Microsoft Corporation".into()),
        }
    }

    fn photocraft_0_8_2() -> Facts {
        Facts {
            signature: Ok(publisher()),
            product_name: Some("PhotoCraft".into()),
            manufacturer: Some(MANUFACTURER.into()),
            product_version: Some("0.8.2".into()),
        }
    }

    fn expect(version: &Version) -> Expect<'_> {
        Expect { names: vec!["PhotoCraft"], version: Some(version), signer: Some(&PUBLISHER) }
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
        for why in ["the file is not signed", "OS error 0x80096010", "OS error 0x800b0109", ""] {
            let facts = Facts { signature: Err(why.into()), ..photocraft_0_8_2() };
            let err = check(&facts, &expect(&v)).unwrap_err();
            assert!(err.contains("signature isn't valid"), "{why}: {err}");
        }
        let someone = Signer { common_name: Some("Someone Else Ltd".into()), organization: Some("Someone Else Ltd".into()), ..publisher() };
        let facts = Facts { signature: Ok(someone), ..photocraft_0_8_2() };
        let err = check(&facts, &expect(&v)).unwrap_err();
        assert!(err.contains("signed by CN=Someone Else Ltd, O=Someone Else Ltd, C=US, issued by Microsoft Corporation"), "{err}");
        let facts = Facts { signature: Ok(Signer::default()), ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).unwrap_err().contains("signed by nobody known"));
        // Close isn't good enough.
        let close = Signer { common_name: Some("Learning Machines Inc.".into()), organization: Some("Learning Machines Inc.".into()), ..publisher() };
        let facts = Facts { signature: Ok(close), ..photocraft_0_8_2() };
        assert!(check(&facts, &expect(&v)).is_err());
    }

    #[test]
    fn the_same_name_with_anything_else_is_refused() {
        let v = version::parse("0.8.2").unwrap();
        let refused = |signer: Signer| check(&Facts { signature: Ok(signer), ..photocraft_0_8_2() }, &expect(&v)).unwrap_err();
        // A department isn't the same certificate.
        assert!(refused(Signer { organization_unit: Some("Marketing".into()), ..publisher() }).contains("OU=Marketing"));
        // Nor is another country's company of that name, or one whose certificate another CA issued.
        assert!(refused(Signer { country: Some("GB".into()), ..publisher() }).contains("C=GB"));
        assert!(refused(Signer { issuer: Some("Contoso CA".into()), ..publisher() }).contains("issued by Contoso CA"));
        // Nor the right name with no organization, or no issuer, to go with it.
        assert!(!refused(Signer { organization: None, ..publisher() }).is_empty());
        assert!(!refused(Signer { issuer: None, ..publisher() }).is_empty());
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
        let nothing = Facts { signature: Ok(publisher()), product_name: None, manufacturer: None, product_version: None };
        assert!(check(&nothing, &expect(&v)).is_err());
    }

    /// An unsigned file is refused before anything reads it as a package.
    #[test]
    fn an_unsigned_file_is_never_opened() {
        let dir = tempfile::tempdir().unwrap();
        let msi = dir.path().join("photocraft-0.8.2-windows-x64.msi");
        std::fs::write(&msi, b"not a package").unwrap();
        let v = version::parse("0.8.2").unwrap();
        let err = verify(&msi, &expect(&v)).unwrap_err();
        assert!(err.contains("signature isn't valid"), "{err}");
    }

    /// Windows reads any package: run with `ANY_MSI=C:\path\to\some.msi cargo test -p
    /// artcraft-launcher-native -- --ignored windows_reads_any_msi`.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs an MSI on this machine"]
    fn windows_reads_any_msi() {
        let msi = std::env::var("ANY_MSI").expect("set ANY_MSI to an MSI");
        let [name, manufacturer, version] = properties(Path::new(&msi)).unwrap();
        assert!(name.is_some() && manufacturer.is_some() && version.is_some(), "{name:?} {manufacturer:?} {version:?}");
        eprintln!("{name:?} {manufacturer:?} {version:?} {:?}", signature(Path::new(&msi)));
    }

    /// The real check against a real release package. Run with `CRAFT_MSI=C:\path\to\photocraft-0.8.2-windows-x64.msi
    /// CRAFT_MSI_VERSION=0.8.2 cargo test -p artcraft-launcher-native -- --ignored a_real_craft_app_msi`.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a Craft App MSI from a real release"]
    fn a_real_craft_app_msi_passes() {
        let msi = std::env::var("CRAFT_MSI").expect("set CRAFT_MSI to a Craft App MSI");
        let want = version::parse(&std::env::var("CRAFT_MSI_VERSION").expect("set CRAFT_MSI_VERSION")).unwrap();
        let names: Vec<&str> = artcraft_launcher_core::SUITE.iter().map(|a| a.name).collect();
        assert_eq!(verify(Path::new(&msi), &Expect { names, version: Some(&want), signer: Some(&PUBLISHER) }), Ok(()));
    }
}
