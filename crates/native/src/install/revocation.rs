//! Whether a package's signing certificate was withdrawn (revoked).
//!
//! Timestamped signatures outlive their certificates. The unelevated launcher may fetch revocation
//! information before a prompt; the elevated helper must independently reject known revocations,
//! even when started directly. It uses only Windows' cache, never the network. Both entry points
//! share the same decision for good, revoked, unknown and unexpected results.

#![cfg_attr(not(windows), allow(dead_code))]

/// `CRYPT_E_REVOKED`: a certificate in the chain was revoked.
const REVOKED: i32 = 0x8009_2010_u32.cast_signed();
/// `CERT_E_REVOKED`: the trust provider reports a revoked certificate.
const CERT_REVOKED: i32 = 0x800B_010C_u32.cast_signed();
/// `CRYPT_E_NO_REVOCATION_CHECK`: a certificate names no way to check it.
const NO_CHECK: i32 = 0x8009_2012_u32.cast_signed();
/// `CRYPT_E_REVOCATION_OFFLINE`: the revocation server couldn't be reached.
const OFFLINE: i32 = 0x8009_2013_u32.cast_signed();
/// `CERT_E_REVOCATION_FAILURE`: the check couldn't be completed.
const FAILED: i32 = 0x800B_010E_u32.cast_signed();

/// What a signature check with revocation on found.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Status {
    /// Valid, and no certificate in the chain is revoked.
    Good,
    Revoked,
    /// Valid as far as it went, but nobody could say whether a certificate was revoked.
    Unknown,
    /// Any other failure. The same file already passed the check without revocation, so this
    /// is unexpected, and it refuses.
    Failed(i32),
}

/// Sorts the HRESULT of a verification with revocation on (`None` when it passed).
pub(super) fn classify(error: Option<i32>) -> Status {
    match error {
        None => Status::Good,
        Some(REVOKED | CERT_REVOKED) => Status::Revoked,
        Some(NO_CHECK | OFFLINE | FAILED) => Status::Unknown,
        Some(code) => Status::Failed(code),
    }
}

/// Checks that `file`'s signing certificates aren't revoked. Call it only on a file whose
/// signature and publisher already passed.
///
/// shortcut: a check nobody can answer (offline, or no revocation server reachable) continues,
/// with a warning in the log, so all-users updates and downgrades still work offline. Upgrade to
/// a refusal if the publisher ever needs a withdrawn package refused on a machine that is offline.
#[cfg(windows)]
pub(super) fn check(file: &std::path::Path) -> Result<(), String> {
    check_status(file, status(file, true)?)
}

/// Repeats revocation verification inside the helper without network retrieval. An untrusted
/// caller can bypass the launcher's online check, so this must run before any installed copy is
/// removed. An unknown cached answer follows the same documented offline policy as `check`.
#[cfg(windows)]
pub fn check_cached(file: &std::path::Path) -> Result<(), String> {
    check_status(file, status(file, false)?)
}

fn check_status(file: &std::path::Path, status: Status) -> Result<(), String> {
    let shown = file.display();
    match status {
        Status::Good => Ok(()),
        Status::Revoked => {
            Err(format!("{shown} was signed with a certificate its publisher has withdrawn, so it wasn't installed. Nothing was changed."))
        }
        Status::Unknown => {
            log::warn!("couldn't find out whether {shown}'s signing certificate was withdrawn; continuing without a confirmed revocation result");
            Ok(())
        }
        Status::Failed(code) => Err(format!("couldn't check whether {shown}'s signature was withdrawn (error {code:#010x}), so it wasn't installed")),
    }
}

/// `file`'s signature checked with revocation on. `network` lets Windows fetch revocation lists;
/// without it, only what Windows has cached is used.
#[cfg(windows)]
fn status(file: &std::path::Path, network: bool) -> Result<Status, String> {
    use verifysign::{CodeSignVerifier, Revocation, VerifyPolicy};

    let shown = file.display();
    let verifier = CodeSignVerifier::new(VerifyPolicy::new().revocation(Revocation::Chain).allow_network(network))
        .map_err(|e| format!("couldn't check whether {shown}'s signature was withdrawn ({e})"))?;
    match verifier.verify_file(file) {
        Ok(_) => Ok(classify(None)),
        Err(verifysign::Error::OsError(code)) => Ok(classify(Some(code))),
        Err(e) => Err(format!("couldn't check whether {shown}'s signature was withdrawn ({e}), so it wasn't installed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revoked_certificate_is_refused() {
        let file = std::path::Path::new("cached.msi");
        for error in [0x8009_2010_u32, 0x800B_010C] {
            let status = classify(Some(error.cast_signed()));
            assert_eq!(status, Status::Revoked);
            let refused = check_status(file, status).unwrap_err();
            assert!(refused.contains("withdrawn") && refused.contains("Nothing was changed"));
        }
    }

    #[test]
    fn a_check_nobody_can_answer_is_unknown_not_revoked() {
        for code in [0x8009_2012_u32, 0x8009_2013, 0x800B_010E] {
            let status = classify(Some(code.cast_signed()));
            assert_eq!(status, Status::Unknown, "{code:#x}");
            assert_eq!(check_status(std::path::Path::new("offline.msi"), status), Ok(()));
        }
    }

    #[test]
    fn a_passed_check_is_good_and_any_other_error_refuses() {
        assert_eq!(classify(None), Status::Good);
        assert_eq!(check_status(std::path::Path::new("good.msi"), classify(None)), Ok(()));
        assert!(check_status(std::path::Path::new("changed.msi"), classify(Some(0x8009_6010_u32.cast_signed()))).is_err());
        // TRUST_E_BAD_DIGEST: the file changed since it was signed.
        assert_eq!(classify(Some(0x8009_6010_u32.cast_signed())), Status::Failed(0x8009_6010_u32.cast_signed()));
    }

    /// A real signed package, when given (`ARTCRAFT_REVOCATION_REAL_MSI`): online, Windows must
    /// answer (not merely fail to); from its cache alone, it must never refuse.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real signed package and the network: set ARTCRAFT_REVOCATION_REAL_MSI"]
    fn a_real_package_is_not_revoked() {
        let file = std::path::PathBuf::from(std::env::var_os("ARTCRAFT_REVOCATION_REAL_MSI").unwrap_or_default());
        assert_eq!(status(&file, true), Ok(Status::Good));
        assert!(matches!(status(&file, false), Ok(Status::Good | Status::Unknown)), "{:?}", status(&file, false));
        assert_eq!(check(&file), Ok(()));
        assert_eq!(check_cached(&file), Ok(()));
    }
}
