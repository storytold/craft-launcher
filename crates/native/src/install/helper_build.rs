//! Which program the launcher may elevate as its installer helper: our helper, of this build.
//!
//! The helper sits beside the launcher in the person's own folder, which their programs can write.
//! The publisher check (`elevated::signed_like`) admits any program we ever signed under that
//! name: a genuine older ArtCraft Launcher renamed `artcraft-launcher-helper.exe` passes it, and
//! elevated, that launcher loads DLLs planted beside it (v0.11.1 failed to start with a planted
//! `glu32.dll`), which is the hole the separate helper exists to close. So the helper's
//! VERSIONINFO must name the helper and this launcher's own version. Both sit inside the signed
//! part of the file, so a genuine signed program can't be relabelled without breaking its
//! signature. The launcher and its helper are built from one workspace version and shipped
//! together (one MSI, one portable zip), so a matching pair always agrees.
//!
//! Not covered: a program signed through a root the person's own account added, carrying our
//! publisher's name and a copied VERSIONINFO. Telling it apart needs the chain's root, which this
//! crate can't read without `unsafe`; the helper checks the root of every package it installs
//! (`identity`). Such a program could ask Windows for the same prompt without the launcher.

#![cfg_attr(not(windows), allow(dead_code))]

/// The helper's own file name in its VERSIONINFO (`OriginalFilename`, set in its `build.rs`).
const ORIGINAL_NAME: &str = "artcraft-launcher-helper.exe";

/// Whether a program whose VERSIONINFO gives `original_name` and `version` is the helper built
/// with this launcher (version `ours`). Missing fields refuse: every release build embeds them.
pub(super) fn is_this_helper(original_name: Option<&str>, version: Option<&str>, ours: &str) -> Result<(), String> {
    let Some(name) = original_name.map(str::trim) else {
        return Err("the installer helper doesn't say which program it is".into());
    };
    if !name.eq_ignore_ascii_case(ORIGINAL_NAME) {
        return Err(format!("the installer helper is another program ({name})"));
    }
    match version.map(str::trim) {
        Some(v) if v == ours => Ok(()),
        Some(v) => Err(format!("the installer helper is version {v}, not {ours} like ArtCraft Launcher")),
        None => Err("the installer helper doesn't say which version it is".into()),
    }
}

/// Checks that the helper at `path` (resolved and held by the caller) is this launcher's own.
#[cfg(windows)]
pub(super) fn check(path: &std::path::Path) -> Result<(), String> {
    let (name, version) = version_strings(path);
    is_this_helper(name.as_deref(), version.as_deref(), env!("CARGO_PKG_VERSION"))
}

/// `OriginalFilename` and `ProductVersion` from `path`'s VERSIONINFO, read without running it.
#[cfg(windows)]
fn version_strings(path: &std::path::Path) -> (Option<String>, Option<String>) {
    let Some(info) = path.to_str().and_then(|p| winsafe::HVERSIONINFO::GetFileVersionInfo(p).ok()) else {
        return (None, None);
    };
    let listed = info.langs_and_cps().map(<[_]>::to_vec).unwrap_or_default();
    let tables = crate::detect::windows::string_tables(&listed);
    let read = |key: &str| tables.iter().find_map(|&table| info.str_val(table, key).ok());
    (read("OriginalFilename"), read("ProductVersion"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helper_of_this_version_passes() {
        assert_eq!(is_this_helper(Some("artcraft-launcher-helper.exe"), Some("0.11.1"), "0.11.1"), Ok(()));
        assert_eq!(is_this_helper(Some("ArtCraft-Launcher-Helper.EXE "), Some(" 0.11.1"), "0.11.1"), Ok(()));
    }

    /// The case from review: a genuine, signed older launcher renamed as the helper.
    #[test]
    fn a_renamed_launcher_is_refused() {
        let error = is_this_helper(Some("artcraft-launcher.exe"), Some("0.11.1"), "0.11.1").unwrap_err();
        assert!(error.contains("another program (artcraft-launcher.exe)"), "{error}");
    }

    #[test]
    fn a_helper_of_another_version_is_refused() {
        let error = is_this_helper(Some("artcraft-launcher-helper.exe"), Some("0.11.0"), "0.11.1").unwrap_err();
        assert!(error.contains("version 0.11.0"), "{error}");
    }

    #[test]
    fn a_program_that_says_nothing_is_refused() {
        assert!(is_this_helper(None, Some("0.11.1"), "0.11.1").is_err());
        assert!(is_this_helper(Some("artcraft-launcher-helper.exe"), None, "0.11.1").is_err());
    }

    /// A real signed program put in the helper's place: its VERSIONINFO still names it.
    #[cfg(windows)]
    #[test]
    fn a_signed_windows_program_under_the_helpers_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap().join(r"System32\hostname.exe");
        let fake = dir.path().join(ORIGINAL_NAME);
        std::fs::copy(system, &fake).unwrap();
        let error = check(&fake).unwrap_err();
        assert!(error.contains("another program (hostname.exe)"), "{error}");
    }

    /// A real built helper, when given (`ARTCRAFT_HELPER_REAL_EXE`, built from this workspace).
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a built helper: set ARTCRAFT_HELPER_REAL_EXE"]
    fn a_real_helper_of_this_build_passes() {
        let file = std::path::PathBuf::from(std::env::var_os("ARTCRAFT_HELPER_REAL_EXE").unwrap_or_default());
        assert_eq!(check(&file), Ok(()));
    }
}
