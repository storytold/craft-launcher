//! Which package a fresh install would use, so the install-folder fields show the folder that
//! install really goes into.
//!
//! It mirrors the backend's choice (`native::install::choose_kinds`, then `pick_asset`): the
//! preferred formats first, then every other format this platform installs, each tried against
//! every asset token. Choosing only the preferred format would show the portable `apps` folder
//! for a release that ships only an MSI, which the backend then installs into Program Files.
//! The UI cannot see a Linux machine's package managers, so there it answers nothing and the
//! field keeps saying "System default".

use artcraft_launcher_core::platform::{parse_asset_name, pick_asset};
use artcraft_launcher_core::protocol::LatestRelease;
use artcraft_launcher_core::{Arch, CraftApp, Os, PackageKind};

use crate::app::{Host, LauncherUi};
use crate::model::AppEntry;

/// The formats a fresh install can use on `os`, best first: native `available_kinds`.
fn platform_kinds(os: Os) -> &'static [PackageKind] {
    match os {
        Os::Windows => &[PackageKind::Msi, PackageKind::PortableZip],
        Os::MacOs => &[PackageKind::Dmg],
        _ => &[],
    }
}

/// The order a fresh install of `app` tries formats in: `preferred` first, then the rest.
fn fresh_kinds(app: &CraftApp, os: Os, preferred: &[PackageKind]) -> Vec<PackageKind> {
    let available: Vec<PackageKind> = platform_kinds(os).iter().copied().filter(|kind| app.installs(*kind)).collect();
    let mut kinds: Vec<PackageKind> = preferred.iter().copied().filter(|kind| available.contains(kind)).collect();
    for kind in available {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

impl LauncherUi {
    /// The format and architecture of the package a fresh install of `entry` would use. Before
    /// the release is known, the first format with no architecture (an MSI then answers
    /// nothing, as its Program Files depends on it). `None` when nothing installs it here.
    fn fresh_package(&self, entry: &AppEntry) -> Option<(PackageKind, Option<Arch>)> {
        let platform = self.platform?;
        let kinds = fresh_kinds(entry.app, platform.os, &self.settings.preferred_kinds);
        let Some(LatestRelease::Found(release)) = entry.latest.as_ref() else {
            return kinds.first().map(|kind| (*kind, None));
        };
        let stems: Vec<&str> = entry.app.stems().collect();
        let (asset, kind) = pick_asset(release, &stems, platform, &kinds)?;
        Some((kind, parse_asset_name(&asset.name, &stems).and_then(|name| name.arch())))
    }

    /// The folder a fresh install of `id` goes inside when no folder is chosen.
    pub(crate) fn default_folder_for(&self, id: &str, host: &mut dyn Host) -> Option<String> {
        let (kind, arch) = self.fresh_package(self.entry(id)?)?;
        host.default_install_folder(kind, arch)
    }

    /// The default for Settings, which covers every app: the folder when every app that can be
    /// installed here shares one, else `None` ("System default"). A 32-bit-only app goes to
    /// `Program Files (x86)`, so the launcher's own architecture is not the answer. An app
    /// whose release is not known yet cannot be installed, so it does not count.
    pub(crate) fn suite_default_folder(&self, host: &mut dyn Host) -> Option<String> {
        let mut shared: Option<String> = None;
        for entry in self.entries.iter().filter(|entry| matches!(entry.latest, Some(LatestRelease::Found(_)))) {
            let Some((kind, arch)) = self.fresh_package(entry) else { continue };
            let folder = host.default_install_folder(kind, arch)?;
            match &shared {
                Some(seen) if *seen != folder => return None,
                Some(_) => {}
                None => shared = Some(folder),
            }
        }
        shared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_install_tries_the_preferred_format_then_the_rest() {
        let app = &artcraft_launcher_core::SUITE[0];
        assert_eq!(fresh_kinds(app, Os::Windows, &[]), [PackageKind::Msi, PackageKind::PortableZip]);
        assert_eq!(fresh_kinds(app, Os::Windows, &[PackageKind::PortableZip]), [PackageKind::PortableZip, PackageKind::Msi]);
        // A preference this platform cannot install is skipped, as the backend does.
        assert_eq!(fresh_kinds(app, Os::Windows, &[PackageKind::Deb]), [PackageKind::Msi, PackageKind::PortableZip]);
        assert!(fresh_kinds(app, Os::Linux, &[PackageKind::AppImage]).is_empty());
    }
}
