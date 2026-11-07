use std::path::Path;

use super::*;

const GRIDCRAFT_UPGRADE: &str = "{5511327B-F305-41BE-92D0-EB84944E7C4D}";
const GRIDCRAFT_PRODUCT: &str = "{FCC27CE8-9613-4A6D-9FCE-282D055B404A}";
const LAUNCHER_UPGRADE: &str = "{1AC08DBF-D4CF-4BA4-A81E-CAA5C1004974}";

/// Facts a test chooses: the root a signature chains to, and the package's UpgradeCode.
struct Facts {
    root: Result<String, String>,
    revocation: Result<(), String>,
    upgrade: Result<String, String>,
}

impl PackageFacts for Facts {
    fn signing_root(&self, _: &Path) -> Result<String, String> {
        self.root.clone()
    }
    fn check_revocation(&self, _: &Path) -> Result<(), String> {
        self.revocation.clone()
    }
    fn upgrade_code(&self, _: &Path) -> Result<String, String> {
        self.upgrade.clone()
    }
}

fn facts(root: &str, upgrade: &str) -> Facts {
    Facts { root: Ok(root.into()), revocation: Ok(()), upgrade: Ok(upgrade.into()) }
}

/// Windows Installer links GridCraft's UpgradeCode to its installed product, nothing else.
fn registry(upgrade: &str, product: &str) -> bool {
    upgrade.eq_ignore_ascii_case(GRIDCRAFT_UPGRADE) && product.eq_ignore_ascii_case(GRIDCRAFT_PRODUCT)
}

fn run(facts: &Facts, removed: &[&str]) -> Result<(), String> {
    let removed: Vec<String> = removed.iter().map(|s| s.to_string()).collect();
    check(facts, Path::new(r"C:\runs\chosen.msi"), "GridCraft", &removed, registry)
}

#[test]
fn a_package_signed_through_the_pinned_root_for_the_installed_product_is_accepted() {
    assert_eq!(run(&facts(SIGNING_ROOT_SHA256, GRIDCRAFT_UPGRADE), &[GRIDCRAFT_PRODUCT]), Ok(()));
    let upper = SIGNING_ROOT_SHA256.to_ascii_uppercase();
    assert_eq!(run(&facts(&upper, &GRIDCRAFT_UPGRADE.to_ascii_lowercase()), &[GRIDCRAFT_PRODUCT]), Ok(()));
}

/// A root the person's own account added, issuing itself our publisher's name: the subject and
/// issuer match, the root doesn't (the sandbox run's T10).
#[test]
fn any_other_signing_root_is_refused_before_the_product_is_read() {
    let other = "a".repeat(64);
    let read = Facts { root: Ok(other), revocation: Err("must not check an untrusted root".into()), upgrade: Err("must not be read".into()) };
    let error = run(&read, &[GRIDCRAFT_PRODUCT]).unwrap_err();
    assert!(error.contains("isn't signed through the certificate service"), "{error}");
    let unverified = Facts { root: Err("not signed".into()), revocation: Ok(()), upgrade: Ok(GRIDCRAFT_UPGRADE.into()) };
    assert_eq!(run(&unverified, &[GRIDCRAFT_PRODUCT]), Err("not signed".into()));
}

/// This is the admission path used even when someone starts the helper without the launcher.
/// A trusted root and matching product cannot bypass a cached revocation refusal. The unreadable
/// UpgradeCode also proves that nothing after this refusal is attempted.
#[test]
fn direct_helper_admission_refuses_revocation_before_reading_the_product() {
    let revoked = Facts {
        root: Ok(SIGNING_ROOT_SHA256.into()),
        revocation: Err("cached signing certificate was revoked".into()),
        upgrade: Err("must not be read".into()),
    };
    assert_eq!(run(&revoked, &[GRIDCRAFT_PRODUCT]), Err("cached signing certificate was revoked".into()));
}

/// Another package our publisher signed, such as the launcher's own (the sandbox run's T9).
#[test]
fn a_package_of_another_product_is_refused() {
    let error = run(&facts(SIGNING_ROOT_SHA256, LAUNCHER_UPGRADE), &[GRIDCRAFT_PRODUCT]).unwrap_err();
    assert!(error.contains("isn't a package of GridCraft"), "{error}");
}

#[test]
fn every_removed_registration_must_belong_to_the_package() {
    let other_product = "{00000000-1111-2222-3333-444444444444}";
    assert!(run(&facts(SIGNING_ROOT_SHA256, GRIDCRAFT_UPGRADE), &[GRIDCRAFT_PRODUCT, other_product]).is_err());
    assert!(run(&facts(SIGNING_ROOT_SHA256, GRIDCRAFT_UPGRADE), &[]).is_err(), "nothing to replace, nothing proven");
}

#[test]
fn an_upgrade_code_that_is_not_a_guid_is_refused_without_asking_the_registry() {
    for upgrade in ["", "GridCraft", "{5511327B-F305-41BE-92D0-EB84944E7C4D", "{5511327B-F305-41BE-92D0-EB84944E7C4D}\n"] {
        let removed = vec![GRIDCRAFT_PRODUCT.to_string()];
        let result = check(&facts(SIGNING_ROOT_SHA256, upgrade), Path::new("x.msi"), "GridCraft", &removed, |_, _| panic!("asked"));
        assert!(result.is_err(), "{upgrade:?}");
    }
}

/// The packed form Windows Installer used for GridCraft's UpgradeCode on a real machine
/// (`HKLM\SOFTWARE\Classes\Installer\UpgradeCodes\B7231155503FEB14290DBE4849E4C7D4`).
#[test]
fn a_guid_packs_the_way_windows_installer_names_its_keys() {
    assert_eq!(packed_guid(GRIDCRAFT_UPGRADE).as_deref(), Some("B7231155503FEB14290DBE4849E4C7D4"));
    assert_eq!(packed_guid(&GRIDCRAFT_UPGRADE.to_ascii_lowercase()).as_deref(), Some("B7231155503FEB14290DBE4849E4C7D4"));
    for code in ["", "5511327B-F305-41BE-92D0-EB84944E7C4D", "{5511327B-F305-41BE-92D0-EB84944E7C4Z}"] {
        assert_eq!(packed_guid(code), None, "{code:?}");
    }
}

#[test]
fn the_pinned_root_is_a_sha256() {
    assert_eq!(SIGNING_ROOT_SHA256.len(), 64);
    assert!(SIGNING_ROOT_SHA256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
}

/// A product nobody installed has no all-users UpgradeCode link.
#[cfg(windows)]
#[test]
fn an_unregistered_product_is_not_linked() {
    assert!(!linked_upgrade_code(GRIDCRAFT_UPGRADE, "{00000000-1111-2222-3333-444444444444}"));
    assert!(!linked_upgrade_code("not a guid", GRIDCRAFT_PRODUCT));
}
