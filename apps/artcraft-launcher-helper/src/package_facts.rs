//! The installer helper's package checks for `install::identity`: the signing root, cached
//! revocation, and UpgradeCode. The root and UpgradeCode have no safe wrapper (`verifysign`
//! exposes only the signer's own certificate, and `winsafe` has no Windows Installer API), so
//! their Windows calls live here, in the only crate that allows reviewed `unsafe`. Cached
//! revocation delegates to the shared, safe verification policy in the native crate.
//!
//! Each call's result is checked, every pointer is null-checked before it is read, and every
//! handle is closed by a guard, on every path. Nothing here decides what is allowed: that is
//! `identity::check`, which has no `unsafe` and is unit-tested.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use artcraft_launcher_native::install::identity::PackageFacts;
use sha2::{Digest, Sha256};

/// Reads a package's facts through WinTrust and Windows Installer.
pub struct WindowsFacts;

impl PackageFacts for WindowsFacts {
    fn signing_root(&self, file: &Path) -> Result<String, String> {
        signing_root(file).map_err(|e| format!("couldn't check the signature of {} ({e})", file.display()))
    }

    fn check_revocation(&self, file: &Path) -> Result<(), String> {
        artcraft_launcher_native::install::revocation::check_cached(file)
    }

    fn upgrade_code(&self, file: &Path) -> Result<String, String> {
        upgrade_code(file).map_err(|e| format!("couldn't read which app {} is ({e})", file.display()))
    }
}

/// `path` as a NUL-terminated UTF-16 string.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// Verifies `file`'s signature with WinTrust, as `verifysign` does (no revocation check, no UI,
/// MD2 and MD4 refused), keeps the state, and hashes the root certificate of the chain WinTrust
/// built. The state is closed however this returns.
///
/// Chain building is cache-only: `WTD_REVOKE_NONE` alone does not prevent network retrieval.
/// Revocation is checked independently by `WindowsFacts::check_revocation`, also cache-only, in
/// `identity::check` before any product is removed. A direct invocation cannot skip that guard.
#[allow(unsafe_code)]
fn signing_root(file: &Path) -> Result<String, String> {
    use windows_sys::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_STATEACTION_CLOSE, WTHelperGetProvSignerFromChain,
        WTHelperProvDataFromStateData, WinVerifyTrust,
    };

    /// Closes the WinTrust state when dropped.
    struct State<'a>(&'a mut WINTRUST_DATA);
    impl Drop for State<'_> {
        fn drop(&mut self) {
            let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
            self.0.dwStateAction = WTD_STATEACTION_CLOSE;
            // SAFETY: closes the state the verify call opened, with the same live structures.
            unsafe { WinVerifyTrust(std::ptr::null_mut(), &raw mut action, std::ptr::from_mut(self.0).cast()) };
        }
    }

    let path = wide(file);
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: path.as_ptr(),
        hFile: std::ptr::null_mut(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = signature_data(&mut info);
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: `data` and `info` point at live locals (`path` outlives the call), sized as WinTrust
    // expects; no window handle. The state it opens is closed by `State` below.
    let verdict = unsafe { WinVerifyTrust(std::ptr::null_mut(), &raw mut action, (&raw mut data).cast()) };
    let data = State(&mut data);
    if verdict != 0 {
        return Err(format!("Windows error {verdict:#010x}"));
    }
    // SAFETY: the state handle came from the successful verify above and is still open. Each
    // pointer read below is null-checked, and each count bounds the array it indexes; all of it
    // belongs to that state, which stays open until `data` drops at the end of this function.
    unsafe {
        let provider = WTHelperProvDataFromStateData(data.0.hWVTStateData);
        if provider.is_null() {
            return Err("no signature data".into());
        }
        let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
        let chain = signer.as_ref().map(|s| s.pChainContext).filter(|c| !c.is_null()).ok_or("no certificate chain")?;
        let chain = &*chain;
        if chain.cChain == 0 || chain.rgpChain.is_null() {
            return Err("no certificate chain".into());
        }
        let simple = (*chain.rgpChain).as_ref().ok_or("no certificate chain")?;
        if simple.cElement == 0 || simple.rgpElement.is_null() {
            return Err("an empty certificate chain".into());
        }
        let root = (*simple.rgpElement.add(simple.cElement as usize - 1)).as_ref().ok_or("no root certificate")?;
        let cert = root.pCertContext.as_ref().ok_or("no root certificate")?;
        if cert.pbCertEncoded.is_null() || cert.cbCertEncoded == 0 {
            return Err("an empty root certificate".into());
        }
        let encoded = std::slice::from_raw_parts(cert.pbCertEncoded, cert.cbCertEncoded as usize);
        Ok(Sha256::digest(encoded).iter().map(|b| format!("{b:02x}")).collect())
    }
}

/// The exact WinTrust settings used for the helper's signature/root check. Network retrieval is
/// forbidden even with an empty certificate cache, not merely avoided on a warm test machine.
fn signature_data(info: &mut windows_sys::Win32::Security::WinTrust::WINTRUST_FILE_INFO) -> windows_sys::Win32::Security::WinTrust::WINTRUST_DATA {
    use windows_sys::Win32::Security::WinTrust::{
        WINTRUST_DATA, WINTRUST_DATA_0, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_DISABLE_MD2_MD4, WTD_REVOKE_NONE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE, WTD_UICONTEXT_EXECUTE,
    };
    WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_DISABLE_MD2_MD4 | WTD_CACHE_ONLY_URL_RETRIEVAL,
        dwUIContext: WTD_UICONTEXT_EXECUTE,
        ..Default::default()
    }
}

/// Reads the `UpgradeCode` property from `file` with Windows Installer, read-only. The database is
/// closed before this returns, so it never holds the package open while `msiexec` runs.
#[allow(unsafe_code)]
fn upgrade_code(file: &Path) -> Result<String, String> {
    use windows_sys::Win32::System::ApplicationInstallationAndServicing::{
        MSIDBOPEN_READONLY, MSIHANDLE, MsiCloseHandle, MsiDatabaseOpenViewW, MsiOpenDatabaseW, MsiRecordGetStringW, MsiViewExecute, MsiViewFetch,
    };

    /// Closes a Windows Installer handle when dropped (zero is no handle).
    struct Handle(MSIHANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if self.0 != 0 {
                // SAFETY: a handle this function opened and nothing else closes.
                unsafe { MsiCloseHandle(self.0) };
            }
        }
    }
    let failed = |step: &str, code: u32| format!("{step} failed with Windows Installer error {code}");

    let path = wide(file);
    let query: Vec<u16> = "SELECT `Value` FROM `Property` WHERE `Property` = 'UpgradeCode'".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: every string is NUL-terminated and outlives its call; each out-handle is a live local
    // owned by a `Handle`, so it is closed on every path, in reverse order of opening.
    unsafe {
        let mut database = Handle(0);
        let code = MsiOpenDatabaseW(path.as_ptr(), MSIDBOPEN_READONLY, &raw mut database.0);
        if code != 0 {
            return Err(failed("opening the package", code));
        }
        let mut view = Handle(0);
        let code = MsiDatabaseOpenViewW(database.0, query.as_ptr(), &raw mut view.0);
        if code != 0 {
            return Err(failed("reading the package", code));
        }
        let code = MsiViewExecute(view.0, 0);
        if code != 0 {
            return Err(failed("reading the package", code));
        }
        let mut record = Handle(0);
        let code = MsiViewFetch(view.0, &raw mut record.0);
        if code != 0 {
            return Err(failed("finding its UpgradeCode", code));
        }
        // A braced GUID is 38 characters; anything longer is refused rather than read.
        let mut buffer = [0u16; 40];
        let mut length = buffer.len() as u32;
        let code = MsiRecordGetStringW(record.0, 1, buffer.as_mut_ptr(), &raw mut length);
        if code != 0 {
            return Err(failed("reading its UpgradeCode", code));
        }
        let text = buffer.get(..length as usize).ok_or("an UpgradeCode longer than its buffer")?;
        String::from_utf16(text).map_err(|_| "an UpgradeCode that isn't text".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_signature_verification_cannot_fetch_over_the_network() {
        use windows_sys::Win32::Security::WinTrust::{WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_DISABLE_MD2_MD4, WTD_UI_NONE};
        let mut info = WINTRUST_FILE_INFO::default();
        let data = signature_data(&mut info);
        assert_ne!(data.dwProvFlags & WTD_CACHE_ONLY_URL_RETRIEVAL, 0);
        assert_ne!(data.dwProvFlags & WTD_DISABLE_MD2_MD4, 0);
        assert_eq!(data.dwUIChoice, WTD_UI_NONE);
    }

    /// A file with no signature fails, rather than giving some root.
    #[test]
    fn an_unsigned_file_has_no_signing_root() {
        let file = std::env::temp_dir().join(format!("artcraft-helper-unsigned-{}.msi", std::process::id()));
        let _ = std::fs::write(&file, b"not a package");
        assert!(signing_root(&file).is_err());
        assert!(WindowsFacts.check_revocation(&file).is_err(), "the provider must actually verify the cached signature");
        assert!(upgrade_code(&file).is_err(), "not a Windows Installer database");
        let _ = std::fs::remove_file(&file);
    }

    /// Real packages, when given (`ARTCRAFT_HELPER_REAL_MSI` = a Trusted Signing-signed Craft MSI,
    /// `ARTCRAFT_HELPER_REAL_UPGRADE` = its UpgradeCode). Not in CI, which has no such package.
    #[test]
    #[ignore = "needs a real signed package: set ARTCRAFT_HELPER_REAL_MSI and ARTCRAFT_HELPER_REAL_UPGRADE"]
    fn a_real_package_chains_to_the_pinned_root_and_names_its_product() {
        let file = std::path::PathBuf::from(std::env::var_os("ARTCRAFT_HELPER_REAL_MSI").unwrap_or_default());
        let upgrade = std::env::var("ARTCRAFT_HELPER_REAL_UPGRADE").unwrap_or_default();
        assert_eq!(signing_root(&file).as_deref(), Ok(artcraft_launcher_native::install::identity::SIGNING_ROOT_SHA256));
        assert_eq!(WindowsFacts.check_revocation(&file), Ok(()));
        assert_eq!(upgrade_code(&file).map(|u| u.to_ascii_uppercase()), Ok(upgrade.to_ascii_uppercase()));
    }
}
