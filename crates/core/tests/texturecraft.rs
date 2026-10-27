//! TextureCraft's first release, as GitHub published it, picks the right asset on each desktop.
//! The parsing and the choice are craft-update's; the fixture checks the launcher's newest app
//! against them.
#![allow(clippy::unwrap_used)]

use artcraft_launcher_core::platform::{Arch, Os, PackageKind, Platform, pick_asset};
use artcraft_launcher_core::release::parse_github_release;

/// The Windows MSI, the universal DMG and the x86_64 Flatpak are each found by their kind.
#[test]
fn texturecraft_selects_published_desktop_assets() {
    let release = parse_github_release(include_str!("fixtures/texturecraft-release.json"), "storytold/texturecraft").unwrap();
    for (os, kind, expected) in [
        (Os::Windows, PackageKind::Msi, "texturecraft-0.1.0-windows-x64.msi"),
        (Os::MacOs, PackageKind::Dmg, "texturecraft-0.1.0-macos-universal.dmg"),
        (Os::Linux, PackageKind::Flatpak, "texturecraft-0.1.0-linux-x86_64.flatpak"),
    ] {
        let platform = Platform { os, arch: Arch::X64 };
        let (asset, selected) = pick_asset(&release, &["texturecraft"], platform, &[kind]).unwrap();
        assert_eq!(asset.name, expected);
        assert_eq!(selected, kind);
    }
}
