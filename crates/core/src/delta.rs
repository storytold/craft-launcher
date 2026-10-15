//! Choosing how to fetch an update: the whole file, or only what changed.
//!
//! The decision is pure data (OS, package format, what's already on disk), so it lives in core
//! next to [`crate::Platform::default_kinds`]: no I/O, no network, wasm-safe. Fetching itself
//! stays in `native`, which must keep today's guarantee: a file nothing vouches for is never
//! installed (#90). Anything this decision can't vouch for answers [`UpdateMethod::Full`].

use crate::platform::{Os, PackageKind};

/// How an update's bytes get onto the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateMethod {
    /// Reuse the installed copy's blocks and fetch only what changed (zsync over HTTP range
    /// requests). Linux AppImage only: its `.zsync` file is published next to every release.
    DeltaZsync,
    /// Today's behaviour: fetch the release asset whole and check its SHA-256.
    Full,
}

/// Which fetch to use for an update. Delta only when every precondition holds; anything else is
/// [`UpdateMethod::Full`], so a half-ready release can never strand an update.
pub fn update_method(os: Os, kind: PackageKind, have_old_file: bool, have_zsync_asset: bool) -> UpdateMethod {
    match (os, kind) {
        (Os::Linux, PackageKind::AppImage) if have_old_file && have_zsync_asset => UpdateMethod::DeltaZsync,
        _ => UpdateMethod::Full,
    }
}

/// The `.zsync` file published next to an AppImage asset (`foo.AppImage` → `foo.AppImage.zsync`);
/// `None` for anything else. `platform::parse_asset_name` deliberately ignores these files, so
/// this is the only place that names them.
pub fn zsync_asset_name(asset_name: &str) -> Option<String> {
    if asset_name.ends_with(".AppImage") { Some(format!("{asset_name}.zsync")) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_only_for_linux_appimage_with_old_file_and_zsync() {
        assert_eq!(update_method(Os::Linux, PackageKind::AppImage, true, true), UpdateMethod::DeltaZsync);
    }

    #[test]
    fn anything_missing_falls_back_to_full() {
        // No installed copy to reuse, or no .zsync published: today's full download.
        assert_eq!(update_method(Os::Linux, PackageKind::AppImage, false, true), UpdateMethod::Full);
        assert_eq!(update_method(Os::Linux, PackageKind::AppImage, true, false), UpdateMethod::Full);
        assert_eq!(update_method(Os::Linux, PackageKind::AppImage, false, false), UpdateMethod::Full);
        // Other systems never take the delta path, even with everything present.
        assert_eq!(update_method(Os::Windows, PackageKind::Msi, true, true), UpdateMethod::Full);
        assert_eq!(update_method(Os::Windows, PackageKind::PortableZip, true, true), UpdateMethod::Full);
        assert_eq!(update_method(Os::MacOs, PackageKind::Dmg, true, true), UpdateMethod::Full);
        assert_eq!(update_method(Os::Linux, PackageKind::Deb, true, true), UpdateMethod::Full);
        assert_eq!(update_method(Os::Linux, PackageKind::Rpm, true, true), UpdateMethod::Full);
        assert_eq!(update_method(Os::Linux, PackageKind::Flatpak, true, true), UpdateMethod::Full);
        assert_eq!(update_method(Os::Linux, PackageKind::TarGz, true, true), UpdateMethod::Full);
        assert_eq!(update_method(Os::FreeBsd, PackageKind::TarGz, true, true), UpdateMethod::Full);
    }

    #[test]
    fn zsync_name_is_the_appimage_name_plus_zsync() {
        assert_eq!(zsync_asset_name("photocraft-0.5.0-linux-x86_64.AppImage").as_deref(), Some("photocraft-0.5.0-linux-x86_64.AppImage.zsync"));
        assert_eq!(zsync_asset_name("photocraft-0.5.0-windows-x64.msi"), None);
        assert_eq!(zsync_asset_name("photocraft-0.5.0-macos-universal.dmg"), None);
        assert_eq!(zsync_asset_name("SHA256SUMS.txt"), None);
        // The .zsync file itself is not an AppImage to delta from.
        assert_eq!(zsync_asset_name("photocraft-0.5.0-linux-x86_64.AppImage.zsync"), None);
    }
}
