//! Which package a fresh install would use, so the install-folder fields show the folder that
//! install really goes into.
//!
//! It mirrors the backend's choice (`native::install::choose_kinds`, then `pick_asset`): the
//! preferred formats first, then every other format this platform installs, each tried against
//! every asset token. Choosing only the preferred format would show the portable `apps` folder
//! for a release that ships only an MSI, which the backend then installs into Program Files.
//! The UI cannot see a Linux machine's package managers, so there a fresh install answers nothing
//! and the field keeps saying "System default".

use artcraft_launcher_core::platform::{parse_asset_name, pick_asset};
use artcraft_launcher_core::protocol::LatestRelease;
use artcraft_launcher_core::{Arch, Asset, CraftApp, InstallSource, Installed, Os, PackageKind, Release};

use crate::app::{Host, LauncherUi};
use crate::model::AppEntry;

/// The formats a fresh install can use on `os`, best first: native `available_kinds`. Linux's
/// depend on its package managers, so none are certain.
fn platform_kinds(os: Os) -> &'static [PackageKind] {
    match os {
        Os::Windows => &[PackageKind::Msi, PackageKind::PortableZip],
        Os::MacOs => &[PackageKind::Dmg],
        Os::FreeBsd => &[PackageKind::TarGz],
        Os::Linux => &[],
    }
}

/// The formats native `available_kinds` offers on `os` whatever the machine has: on Linux the
/// AppImage, whichever package managers are there.
fn certain_kinds(os: Os) -> &'static [PackageKind] {
    match os {
        Os::Linux => &[PackageKind::AppImage],
        _ => platform_kinds(os),
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

/// The format the copy that's there was installed as, where the UI can tell: on Windows an MSI is
/// found in the registry and anything the launcher records is its portable ZIP (MSI installs
/// leave no record); on macOS every copy is the DMG's app; an AppImage found on Linux is one.
/// What the launcher put on Linux (an AppImage or a tarball) it alone knows.
fn installed_kind(os: Os, source: InstallSource) -> Option<PackageKind> {
    match (os, source) {
        (Os::Windows, InstallSource::Msi) => Some(PackageKind::Msi),
        (Os::Windows, InstallSource::Launcher) => Some(PackageKind::PortableZip),
        (Os::MacOs, InstallSource::MacApp | InstallSource::Launcher) => Some(PackageKind::Dmg),
        (Os::Linux, InstallSource::AppImage) => Some(PackageKind::AppImage),
        _ => None,
    }
}

/// The formats an install or update of `app` tries, best first, as `native::install::choose_kinds`
/// orders them: the installed copy's format alone when the backend surely installs it here, else
/// [`fresh_kinds`]. Empty where the UI can't tell (a fresh install on Linux).
fn install_kinds(app: &CraftApp, os: Os, installed: Option<&Installed>, preferred: &[PackageKind]) -> Vec<PackageKind> {
    let kept = installed.and_then(|inst| installed_kind(os, inst.source)).filter(|kind| certain_kinds(os).contains(kind) && app.installs(*kind));
    match kept {
        Some(kind) => vec![kind],
        None => fresh_kinds(app, os, preferred),
    }
}

impl LauncherUi {
    /// The asset of `release` that installing or updating `entry` here would download. `None`
    /// when none would be, or the UI can't tell which (see [`install_kinds`]).
    pub(crate) fn install_asset<'r>(&self, entry: &AppEntry, release: &'r Release) -> Option<&'r Asset> {
        let platform = self.platform?;
        let kinds = install_kinds(entry.app, platform.os, entry.installed.as_ref(), &self.settings.preferred_kinds);
        let stems: Vec<&str> = entry.app.stems().collect();
        pick_asset(release, &stems, platform, &kinds).map(|(asset, _)| asset)
    }

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

    fn installed(source: InstallSource) -> Installed {
        Installed { version: None, source, location: String::new(), package: None, externally_managed: false, origin: None, installation: None }
    }

    #[test]
    fn an_update_keeps_the_installed_copys_format_as_the_backend_does() {
        use PackageKind::{Dmg, Msi, PortableZip};
        let app = &artcraft_launcher_core::SUITE[0];
        let kinds = |os, source: Option<InstallSource>, preferred: &[PackageKind]| install_kinds(app, os, source.map(installed).as_ref(), preferred);
        // The launcher's own copy on Windows is its portable ZIP, even with MSI preferred.
        assert_eq!(kinds(Os::Windows, Some(InstallSource::Launcher), &[Msi]), [PortableZip]);
        // An MSI install updates as an MSI, even with the portable ZIP preferred.
        assert_eq!(kinds(Os::Windows, Some(InstallSource::Msi), &[PortableZip]), [Msi]);
        // Nothing installed: the preference, then the rest.
        assert_eq!(kinds(Os::Windows, None, &[PortableZip]), [PortableZip, Msi]);
        assert_eq!(kinds(Os::MacOs, Some(InstallSource::MacApp), &[]), [Dmg]);
        // FreeBSD installs only the tarball.
        assert_eq!(kinds(Os::FreeBsd, None, &[]), [PackageKind::TarGz]);
        assert_eq!(kinds(Os::FreeBsd, Some(InstallSource::Binary), &[]), [PackageKind::TarGz]);
        // An AppImage is always installable on Linux, so an AppImage copy updates as one.
        assert_eq!(kinds(Os::Linux, Some(InstallSource::AppImage), &[PackageKind::Deb]), [PackageKind::AppImage]);
        // Other Linux formats depend on package managers the UI can't see: no answer.
        assert!(kinds(Os::Linux, Some(InstallSource::Deb), &[]).is_empty());
        assert!(kinds(Os::Linux, Some(InstallSource::Launcher), &[]).is_empty());
        assert!(kinds(Os::Linux, None, &[PackageKind::AppImage]).is_empty());
    }
}
