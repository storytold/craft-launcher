//! MSI update context is authoritative, not inferred from a display registry entry.
//! An existing copy must never fall back to a fresh installation when discovery fails.

use crate::install::MsiScope;

const UNKNOWN_CONTEXT: &str = "Windows could not confirm who this app is installed for. No changes were made. Try the app's installer instead.";
const CONFLICTING_CONTEXT: &str = "This app is installed for both your account and everyone. Use its installer to choose which copy to update.";

/// Why the Windows Installer lookup gave no context.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProbeError {
    /// The lookup ran but gave no usable answer. The context is never guessed from that.
    Unknown,
    /// The same product is registered for this account and for everyone.
    Conflicting,
    /// The lookup could not run (a registration key it may not read, no account identity). Carries the real
    /// error so it can be logged.
    Unavailable(String),
}

/// The context an update must keep. Windows Installer's own registration decides. Only when that
/// lookup cannot run at all does the context come from the uninstall record the copy was found by
/// (`record_scope`): the machine's records are per-machine and the account's own are per-user. A
/// lookup that ran and gave no answer, or a copy registered twice, still stops the update.
pub(crate) fn update_scope(
    is_msi: bool,
    product_code: Option<&str>,
    probe: impl FnOnce(&str) -> Result<MsiScope, ProbeError>,
    record_scope: impl FnOnce(&str) -> Option<MsiScope>,
) -> Result<Option<MsiScope>, String> {
    if !is_msi {
        return Ok(None);
    }
    let code = product_code.filter(|code| valid_product_code(code)).ok_or(UNKNOWN_CONTEXT)?;
    match probe(code) {
        Ok(scope) => Ok(Some(scope)),
        Err(ProbeError::Conflicting) => Err(CONFLICTING_CONTEXT.into()),
        Err(ProbeError::Unknown) => Err(UNKNOWN_CONTEXT.into()),
        Err(ProbeError::Unavailable(reason)) => {
            log::warn!("Windows Installer context lookup for {code} could not run: {reason}");
            let scope = record_scope(code).ok_or(UNKNOWN_CONTEXT)?;
            log::info!("{code}: keeping the {scope:?} context of its uninstall record");
            Ok(Some(scope))
        }
    }
}

/// The one context every uninstall record of a product agrees on. Records that disagree (the copy is
/// registered for this account and for everyone) or no records at all give `None`, so the update stops.
pub(crate) fn single_scope(scopes: impl IntoIterator<Item = MsiScope>) -> Option<MsiScope> {
    let mut scopes = scopes.into_iter();
    let first = scopes.next()?;
    scopes.all(|scope| scope == first).then_some(first)
}

/// The context to install in. An update keeps `updated`, the context of the copy it replaces. A fresh
/// install (no copy at all) is machine-wide, so every app lands in `C:\Program Files\ArtCraft\<App>`
/// (or the folder the person chose) and not in one account's profile; a dual-scope package would
/// otherwise take its own default, which is per-user. Windows asks for permission once, for that
/// install. Anything else (a copy that is not an MSI) takes the package's own default.
pub(crate) fn install_scope(updated: Option<MsiScope>, fresh: bool) -> Option<MsiScope> {
    updated.or_else(|| fresh.then_some(MsiScope::PerMachine))
}

pub(crate) fn valid_product_code(code: &str) -> bool {
    code.len() == 38
        && code.bytes().enumerate().all(|(index, byte)| match index {
            0 => byte == b'{',
            37 => byte == b'}',
            9 | 14 | 19 | 24 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// The one context the product is registered in: 1 and 2 (managed and unmanaged, this account) are
/// per-user, 4 is the machine. None, any other value, or both kinds is never guessed from.
fn scope_of(contexts: &[u32]) -> Result<MsiScope, ProbeError> {
    let mut scope = None;
    for context in contexts {
        let context = match context {
            1 | 2 => MsiScope::PerUser,
            4 => MsiScope::PerMachine,
            _ => return Err(ProbeError::Unknown),
        };
        if scope.is_some_and(|previous| previous != context) {
            return Err(ProbeError::Conflicting);
        }
        scope = Some(context);
    }
    scope.ok_or(ProbeError::Unknown)
}

/// Windows Installer's packed form of a validated product code, the name of its registration keys:
/// the first three groups reversed, then each remaining byte's two digits swapped.
fn packed_code(code: &str) -> Option<String> {
    if !valid_product_code(code) {
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

/// Reads the product's registration for this account and the machine, as `ProductsEx` does, from
/// Windows Installer's own keys: no other account, and never `Win32_Product` (which repairs).
#[cfg(windows)]
pub(crate) fn probe(product_code: &str) -> Result<MsiScope, ProbeError> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};

    let packed = packed_code(product_code).ok_or(ProbeError::Unknown)?;
    let sid = current_user_sid().map_err(ProbeError::Unavailable)?;
    let keys = [
        (1, HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Installer\Managed\{sid}\Installer\Products\{packed}")),
        (2, HKEY_CURRENT_USER, format!(r"Software\Microsoft\Installer\Products\{packed}")),
        (4, HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\Classes\Installer\Products\{packed}")),
    ];
    let mut contexts = Vec::new();
    for (context, hive, path) in keys {
        match RegKey::predef(hive).open_subkey_with_flags(&path, KEY_READ | KEY_WOW64_64KEY) {
            Ok(_) => contexts.push(context),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // A key we may not read is a lookup that didn't run, not a product that isn't there.
            Err(e) => return Err(ProbeError::Unavailable(format!("couldn't read the Windows Installer registration ({e})"))),
        }
    }
    scope_of(&contexts)
}

/// This account's SID, which names its managed (policy-installed) registrations.
#[cfg(windows)]
fn current_user_sid() -> Result<String, String> {
    use winsafe::{HACCESSTOKEN, TokenInfo, co};
    let failed = |e: winsafe::co::ERROR| format!("couldn't identify this account ({e})");
    match HACCESSTOKEN::GetCurrentProcessToken().GetTokenInformation(co::TOKEN_INFORMATION_CLASS::User).map_err(failed)? {
        TokenInfo::User(user) => user.User.Sid().map(|sid| sid.to_string()).ok_or_else(|| "couldn't identify this account".into()),
        _ => Err("couldn't identify this account".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "{12345678-1234-1234-1234-123456789ABC}";

    #[test]
    fn authoritative_machine_context_is_preserved() {
        assert_eq!(scope_of(&[4]), Ok(MsiScope::PerMachine));
    }

    #[test]
    fn managed_and_unmanaged_accounts_are_both_per_user() {
        assert_eq!(scope_of(&[1]), Ok(MsiScope::PerUser));
        assert_eq!(scope_of(&[2]), Ok(MsiScope::PerUser));
        assert_eq!(scope_of(&[1, 2]), Ok(MsiScope::PerUser));
    }

    #[test]
    fn missing_unknown_or_conflicting_contexts_are_not_guessed() {
        for contexts in [&[][..], &[0], &[7], &[2, 4], &[4, 1], &[4, 3]] {
            assert!(scope_of(contexts).is_err(), "{contexts:?}");
        }
        assert_eq!(scope_of(&[4, 1]), Err(ProbeError::Conflicting));
        assert_eq!(scope_of(&[7]), Err(ProbeError::Unknown));
    }

    #[test]
    fn a_product_code_packs_the_way_windows_installer_names_its_keys() {
        assert_eq!(packed_code("{12345678-ABCD-EF01-2345-6789ABCDEF01}").as_deref(), Some("87654321DCBA10FE32547698BADCFE10"));
        assert_eq!(packed_code("{12345678-abcd-ef01-2345-6789abcdef01}").as_deref(), Some("87654321DCBA10FE32547698BADCFE10"));
        for code in ["", "12345678-ABCD-EF01-2345-6789ABCDEF01", "{12345678-ABCD-EF01-2345-6789ABCDEF0Z}", r"{1234567\-ABCD-EF01-2345-6789ABCDEF01}"]
        {
            assert_eq!(packed_code(code), None, "{code:?}");
        }
    }

    /// A product nobody installed is registered nowhere: the lookup runs and finds no context.
    #[cfg(windows)]
    #[test]
    fn a_product_that_is_not_installed_has_no_context() {
        assert_eq!(probe("{00000000-1111-2222-3333-444444444444}"), Err(ProbeError::Unknown));
    }

    #[cfg(windows)]
    #[test]
    fn this_accounts_sid_is_found() {
        // Local and domain accounts are S-1-5-..., Microsoft Entra ID accounts S-1-12-1-...: only the
        // shape is the same, `S-1-` and then numbers.
        let sid = current_user_sid().unwrap();
        let parts: Vec<&str> = sid.split('-').collect();
        let numbers = parts.get(1..).unwrap_or_default();
        assert!(parts.first() == Some(&"S") && numbers.len() >= 3, "{sid}");
        assert!(numbers.iter().all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())), "{sid}");
        assert_eq!(numbers.first(), Some(&"1"), "{sid}");
    }

    #[test]
    fn a_fresh_install_is_not_looked_up() {
        assert_eq!(update_scope(false, None, |_| Err(ProbeError::Unknown), |_| None), Ok(None));
    }

    #[test]
    fn known_updates_keep_the_authoritative_context() {
        for scope in [MsiScope::PerUser, MsiScope::PerMachine] {
            assert_eq!(update_scope(true, Some(CODE), |_| Ok(scope), |_| None), Ok(Some(scope)));
        }
    }

    #[test]
    fn an_update_without_its_product_code_stops_before_probing() {
        let called = std::cell::Cell::new(false);
        assert!(
            update_scope(
                true,
                None,
                |_| {
                    called.set(true);
                    Ok(MsiScope::PerMachine)
                },
                |_| None
            )
            .is_err()
        );
        assert!(!called.get());
    }

    #[test]
    fn malformed_product_codes_stop_before_any_lookup() {
        for code in ["", "not-a-guid", "{12345678-1234-1234-1234-123456789ABZ}", "';exit 0;#", "{12345678-1234-1234-1234-123456789ABC}\n"] {
            let called = std::cell::Cell::new(false);
            assert!(
                update_scope(
                    true,
                    Some(code),
                    |_| {
                        called.set(true);
                        Ok(MsiScope::PerMachine)
                    },
                    |_| None
                )
                .is_err()
            );
            assert!(!called.get(), "{code:?}");
        }
    }

    #[test]
    fn unavailable_context_never_becomes_a_fresh_install() {
        let unavailable = || Err(ProbeError::Unavailable("query failed".into()));
        assert!(update_scope(true, Some(CODE), |_| unavailable(), |_| None).is_err());
    }

    #[test]
    fn a_lookup_that_cannot_run_falls_back_to_the_uninstall_record() {
        for scope in [MsiScope::PerUser, MsiScope::PerMachine] {
            let seen = std::cell::RefCell::new(String::new());
            let result = update_scope(
                true,
                Some(CODE),
                |_| Err(ProbeError::Unavailable("the registry could not be read".into())),
                |code| {
                    *seen.borrow_mut() = code.to_owned();
                    Some(scope)
                },
            );
            assert_eq!(result, Ok(Some(scope)));
            assert_eq!(*seen.borrow(), CODE);
        }
    }

    #[test]
    fn an_answer_that_is_unclear_or_doubled_never_uses_the_uninstall_record() {
        for failure in [ProbeError::Unknown, ProbeError::Conflicting] {
            let result = update_scope(true, Some(CODE), |_| Err(failure), |_| panic!("must not consult the record"));
            assert!(result.is_err());
        }
    }

    #[test]
    fn a_blocked_lookup_without_a_record_stops_the_update() {
        let result = update_scope(true, Some(CODE), |_| Err(ProbeError::Unavailable("timed out".into())), |_| None);
        assert_eq!(result, Err(UNKNOWN_CONTEXT.to_string()));
    }

    #[test]
    fn records_in_two_contexts_give_no_fallback_scope() {
        assert_eq!(single_scope([]), None);
        assert_eq!(single_scope([MsiScope::PerUser]), Some(MsiScope::PerUser));
        assert_eq!(single_scope([MsiScope::PerMachine, MsiScope::PerMachine]), Some(MsiScope::PerMachine));
        assert_eq!(single_scope([MsiScope::PerMachine, MsiScope::PerUser]), None);
        assert_eq!(single_scope([MsiScope::PerUser, MsiScope::PerMachine, MsiScope::PerMachine]), None);
    }

    #[test]
    fn a_fresh_install_is_machine_wide() {
        assert_eq!(install_scope(None, true), Some(MsiScope::PerMachine));
    }

    #[test]
    fn an_update_keeps_the_context_of_the_copy_it_replaces() {
        for scope in [MsiScope::PerUser, MsiScope::PerMachine] {
            assert_eq!(install_scope(Some(scope), false), Some(scope));
        }
    }

    #[test]
    fn a_copy_that_is_not_an_msi_takes_the_package_default() {
        assert_eq!(install_scope(None, false), None);
    }
}
