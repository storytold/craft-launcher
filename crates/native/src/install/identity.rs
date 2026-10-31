//! Which packages the elevated helper may install: ours, and the app it replaces.
//!
//! The helper supplies two identity facts and an independent cached revocation check through
//! [`PackageFacts`]. Its Windows APIs live outside this crate, which forbids `unsafe`:
//! - **The signing root.** Windows trusts any root the person's own account added, and the helper
//!   runs as that account, so a matching subject and issuer name prove nothing alone: a program of
//!   theirs could add its own root, issue itself our publisher's name, and have our helper install
//!   its package for all users behind our genuine prompt. So the chain Windows built must end at
//!   [`SIGNING_ROOT_SHA256`], the root every Trusted Signing package chains to.
//! - **The product.** A package signed by our publisher could be any Craft app, the launcher, or
//!   anything else we ship. Windows Installer's own all-users registration must link the package's
//!   UpgradeCode to every product the downgrade removes ([`linked_upgrade_code`]), the same
//!   identity Windows Installer uses for upgrades.

#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;

/// SHA-256 of Microsoft Identity Verification Root Certificate Authority 2020 (valid until
/// 16/04/2045). Every package signed through Microsoft's Trusted Signing chains to it: leaf, then
/// `Microsoft ID Verified CS AOC CA 0x` (which rotates), then `Microsoft ID Verified Code Signing
/// PCA 2021`, then this root. Pinned at the root so the rotation of the issuing CAs never refuses
/// a real release.
pub const SIGNING_ROOT_SHA256: &str = "5367f20c7ade0e2bca790915056d086b720c33c1fa2a2661acf787e3292e1270";

/// What the elevated helper reads from a package through Windows itself.
pub trait PackageFacts {
    /// SHA-256 (lower-case hex) of the root certificate of the chain Windows built while verifying
    /// `file`'s signature. An error when the signature doesn't verify.
    fn signing_root(&self, file: &Path) -> Result<String, String>;
    /// Reject a known-revoked signing certificate using only Windows' cache. Required on every
    /// provider: a caller starting the helper directly cannot be trusted to have checked it.
    fn check_revocation(&self, file: &Path) -> Result<(), String>;
    /// `file`'s `UpgradeCode` property, as written in the package.
    fn upgrade_code(&self, file: &Path) -> Result<String, String>;
}

/// Checks that `file`, a package for `app` (its name, for messages), is signed through the pinned
/// root and belongs to every product in `removed`, the all-users registrations the downgrade takes
/// away. `linked(upgrade, product)` says whether Windows Installer links the two.
pub(super) fn check(facts: &dyn PackageFacts, file: &Path, app: &str, removed: &[String], linked: impl Fn(&str, &str) -> bool) -> Result<(), String> {
    let shown = file.display();
    let root = facts.signing_root(file)?;
    if !root.eq_ignore_ascii_case(SIGNING_ROOT_SHA256) {
        return Err(format!("{shown} isn't signed through the certificate service ArtCraft's packages use, so nothing was changed"));
    }
    // This decision is inside the helper's admission path, not only before its prompt.
    facts.check_revocation(file)?;
    let upgrade = facts.upgrade_code(file)?;
    let theirs = |product: &String| crate::msi_context::valid_product_code(&upgrade) && linked(&upgrade, product);
    if removed.is_empty() || !removed.iter().all(theirs) {
        return Err(format!("{shown} isn't a package of {app}, so nothing was changed"));
    }
    Ok(())
}

/// Windows Installer's packed form of a braced GUID, the name of its registration keys and values:
/// the first three groups reversed, then each remaining byte's two digits swapped.
fn packed_guid(code: &str) -> Option<String> {
    if !crate::msi_context::valid_product_code(code) {
        return None;
    }
    let hex: Vec<char> = code.chars().filter(char::is_ascii_hexdigit).map(|c| c.to_ascii_uppercase()).collect();
    let (groups, rest) = hex.split_at_checked(16)?;
    let mut packed = String::with_capacity(32);
    for range in [0..8, 8..12, 12..16] {
        packed.extend(groups.get(range)?.iter().rev());
    }
    for pair in rest.chunks(2) {
        packed.extend(pair.iter().rev());
    }
    Some(packed)
}

/// Whether Windows Installer's all-users registration lists `product` under `upgrade`:
/// `HKLM\SOFTWARE\Classes\Installer\UpgradeCodes\<packed upgrade>` has a value named after the
/// packed product code. Only administrators can write there.
#[cfg(windows)]
pub(super) fn linked_upgrade_code(upgrade: &str, product: &str) -> bool {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};

    let (Some(upgrade), Some(product)) = (packed_guid(upgrade), packed_guid(product)) else { return false };
    let path = format!(r"SOFTWARE\Classes\Installer\UpgradeCodes\{upgrade}");
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY)
        .is_ok_and(|key| key.enum_values().flatten().any(|(name, _)| name.eq_ignore_ascii_case(&product)))
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
