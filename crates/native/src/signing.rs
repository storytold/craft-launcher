//! Authenticode: who signed a file, and whether it is ArtCraft's own publisher.
//!
//! One place for these checks, shared by the launcher's self-update and the Windows MSI batch
//! helper, so the rules can't drift apart. Checks never go online (no revocation lists, no
//! fetching missing certificates): they answer from what is on the machine, so being offline
//! never leaves an install waiting on them.

/// Who ArtCraft Launcher's and the Craft Apps' Windows packages come from: the publisher their
/// MSIs register (`Publisher` in the uninstall entry), and the organization on the certificate
/// they are signed with.
pub const PUBLISHER: &str = "Learning Machines LLC";

/// Whether a certificate's organization is [`PUBLISHER`], exactly.
pub fn is_publisher(organization: Option<&str>) -> bool {
    organization == Some(PUBLISHER)
}

#[cfg(windows)]
pub use windows::{own_publisher, publisher_of, signed_like};

#[cfg(windows)]
mod windows {
    use std::path::Path;
    use std::sync::OnceLock;

    use verifysign::{CodeSignVerifier, Name, VerifyPolicy};

    /// The one verifier: the system's trust in the signature, from local data only.
    fn verifier() -> Option<&'static CodeSignVerifier> {
        static VERIFIER: OnceLock<Option<CodeSignVerifier>> = OnceLock::new();
        VERIFIER.get_or_init(|| CodeSignVerifier::new(VerifyPolicy::new().allow_network(false)).ok()).as_ref()
    }

    /// The publisher on `file`'s valid Authenticode signature; `None` when it has none, it isn't
    /// valid, or it names no one.
    pub fn publisher_of(file: &Path) -> Option<Name> {
        let name = verifier()?.verify_file(file).ok()?.subject_name();
        (name.organization.is_some() || name.common_name.is_some()).then_some(name)
    }

    /// The publisher that signed this program, when it is [`super::PUBLISHER`]; `None` for an
    /// unsigned build or anyone else's. Looked up once per run.
    pub fn own_publisher() -> Option<Name> {
        static OWN: OnceLock<Option<Name>> = OnceLock::new();
        OWN.get_or_init(|| {
            let name = publisher_of(&std::env::current_exe().ok()?)?;
            if super::is_publisher(name.organization.as_deref()) {
                Some(name)
            } else {
                log::warn!("this program is signed by {name:?}, not {}", super::PUBLISHER);
                None
            }
        })
        .clone()
    }

    /// Accepts `file` when it carries a valid Authenticode signature from the same publisher as
    /// `reference`. Refuses an unsigned file, another publisher's, and every file when
    /// `reference` itself isn't signed.
    pub fn signed_like(reference: &Path, file: &Path) -> Result<(), String> {
        let verify = |path: &Path| match verifier() {
            Some(verifier) => verifier.verify_file(path).map(|signature| signature.subject_name()).map_err(|e| e.to_string()),
            None => Err("signatures can't be checked on this system".to_string()),
        };
        let ours = verify(reference).map_err(|e| format!("this copy of ArtCraft Launcher isn't signed ({e}), so it doesn't install updates"))?;
        let theirs = verify(file).map_err(|e| format!("{} isn't signed ({e})", file.display()))?;
        if ours == theirs { Ok(()) } else { Err(format!("{} isn't signed by ArtCraft Launcher's publisher", file.display())) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_publisher_itself_counts() {
        assert!(is_publisher(Some("Learning Machines LLC")));
        assert!(!is_publisher(Some("Learning Machines LLC Evil")));
        assert!(!is_publisher(Some("learning machines llc")), "the certificate's name exactly");
        assert!(!is_publisher(None));
    }
}
