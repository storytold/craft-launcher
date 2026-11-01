//! Reading system package lists (dpkg, rpm, pacman, Flatpak) and matching them to Craft Apps.
//!
//! Pure functions over the tools' text output, so they're tested on every platform; only Linux
//! runs the tools (`detect::linux`).

use artcraft_launcher_core::{CraftApp, InstallSource, version};

/// One installed package that belongs to a Craft App.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub manager: InstallSource,
    /// The package's name (`photocraft-bin`) or Flatpak app id.
    pub name: String,
    /// As the package manager reports it (`0.5.0-1`); empty when it doesn't say.
    pub version: String,
    /// A per-user Flatpak install (`flatpak --user`), which the launcher installs and updates.
    pub user: bool,
}

/// `dpkg-query -W -f '${Package}|${db:Status-Status}|${Version}\n'`: installed packages only.
pub fn parse_dpkg(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('|');
            match (f.next(), f.next(), f.next()) {
                (Some(name), Some("installed"), Some(ver)) => {
                    let name = name.split_once(':').map(|(n, _)| n).unwrap_or(name);
                    Some((name.to_string(), ver.to_string()))
                }
                _ => None,
            }
        })
        .collect()
}

/// `rpm -qa --qf '%{NAME}|%{VERSION}\n'`.
pub fn parse_rpm(text: &str) -> Vec<(String, String)> {
    text.lines().filter_map(|line| line.split_once('|').map(|(n, v)| (n.to_string(), v.to_string()))).collect()
}

/// `pacman -Q`: `name version` per line.
pub fn parse_pacman(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            Some((f.next()?.to_string(), f.next()?.to_string()))
        })
        .collect()
}

/// `flatpak list --app --columns=application,version`: the version column is often empty for
/// bundles built from a release tarball.
pub fn parse_flatpak_list(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let id = f.next()?.trim();
            (!id.is_empty()).then(|| (id.to_string(), f.next().unwrap_or("").trim().to_string()))
        })
        .collect()
}

/// The `Version:` line of `flatpak info <id>`.
pub fn parse_flatpak_info(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim().strip_prefix("Version:").map(|v| v.trim().to_string())).filter(|v| !v.is_empty())
}

/// The package names a Craft App may be installed under with `manager`. On Arch the AUR carries
/// each app three ways: built from source (`photocraft`), the release binary (`photocraft-bin`)
/// and the latest commit (`photocraft-git`). Debian and RPM packages may use either the short stem
/// (`photocraft`) or the reverse-DNS bundle id (`ai.storyteller.photocraft`).
pub fn candidate_names(app: &CraftApp, manager: InstallSource) -> Vec<String> {
    match manager {
        InstallSource::Flatpak => app.bundle_ids().collect(),
        InstallSource::Pacman => {
            app.stems().flat_map(|s| [s.to_string(), format!("{s}-bin"), format!("{s}-git")]).chain(std::iter::once(app.bundle_id())).collect()
        }
        InstallSource::Deb | InstallSource::Rpm => app.stems().map(str::to_string).chain(std::iter::once(app.bundle_id())).collect(),
        _ => app.stems().map(str::to_string).collect(),
    }
}

/// The package of `app` among `installed`, preferring the manager listed first in `order`. A user
/// install beats a system one under any of the app's ids (#881); within a scope the current id wins.
pub fn find<'a>(app: &CraftApp, installed: &'a [Package], order: &[InstallSource]) -> Option<&'a Package> {
    order.iter().find_map(|manager| {
        let names = candidate_names(app, *manager);
        [true, false]
            .into_iter()
            .find_map(|user| names.iter().find_map(|n| installed.iter().find(|p| p.manager == *manager && p.user == user && p.name == *n)))
    })
}

/// A package's version, read the package-manager way (`0.5.0-1` is 0.5.0).
pub fn version_of(p: &Package) -> Option<version::Version> {
    version::parse_package(&p.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkgs(manager: InstallSource, list: Vec<(String, String)>) -> Vec<Package> {
        list.into_iter().map(|(name, version)| Package { manager, name, version, user: false }).collect()
    }

    #[test]
    fn reads_each_package_managers_listing() {
        assert_eq!(
            parse_dpkg("photocraft|installed|0.5.0-1\nphotocraft:amd64|installed|0.6.0-1\nwordcraft|config-files|0.3.0-1\n"),
            [("photocraft".into(), "0.5.0-1".into()), ("photocraft".into(), "0.6.0-1".into())]
        );
        assert_eq!(parse_rpm("gridcraft|0.3.0\n"), [("gridcraft".into(), "0.3.0".into())]);
        assert_eq!(parse_pacman("photocraft-bin 0.5.0-1\nlinux 6.11.1.arch1-1\n").len(), 2);
        assert_eq!(
            parse_flatpak_list("ai.storyteller.wordcraft\t\nai.storyteller.cadcraft\t0.3.0\n"),
            [("ai.storyteller.wordcraft".into(), String::new()), ("ai.storyteller.cadcraft".into(), "0.3.0".into())]
        );
        let info = "\nWordCraft - Word processor\n\n          ID: ai.storyteller.wordcraft\n     Version: 0.3.0\n      Branch: master\n";
        assert_eq!(parse_flatpak_info(info).as_deref(), Some("0.3.0"));
        assert_eq!(parse_flatpak_info("Version: \n"), None);
    }

    #[test]
    fn solidcraft_flatpak_is_found_under_its_older_ids_current_first() {
        let sc = CraftApp::by_id("solidcraft").unwrap();
        let old = pkgs(InstallSource::Flatpak, parse_flatpak_list("ai.storyteller.solvecraft\t0.7.0\n"));
        assert_eq!(find(sc, &old, &[InstallSource::Flatpak]).map(|p| p.name.as_str()), Some("ai.storyteller.solvecraft"));
        let both = pkgs(InstallSource::Flatpak, parse_flatpak_list("ai.storyteller.solidcraft\t0.8.0\nio.tangencylabs.solidcraft\t0.25.1\n"));
        assert_eq!(find(sc, &both, &[InstallSource::Flatpak]).map(|p| p.name.as_str()), Some("io.tangencylabs.solidcraft"));
        // Another app's old id never matches SolidCraft.
        let other = pkgs(InstallSource::Flatpak, parse_flatpak_list("ai.storyteller.cadcraft\t0.3.0\n"));
        assert!(find(sc, &other, &[InstallSource::Flatpak]).is_none());
    }

    #[test]
    fn a_user_flatpak_beats_a_system_one_under_any_id() {
        let sc = CraftApp::by_id("solidcraft").unwrap();
        let mut installed = pkgs(InstallSource::Flatpak, parse_flatpak_list("io.tangencylabs.solidcraft\t0.25.1\n"));
        installed.push(Package { manager: InstallSource::Flatpak, name: "ai.storyteller.solidcraft".into(), version: "0.26.0".into(), user: true });
        assert_eq!(find(sc, &installed, &[InstallSource::Flatpak]).map(|p| p.name.as_str()), Some("ai.storyteller.solidcraft"));
        // Within the user scope the current id still wins.
        installed.push(Package { manager: InstallSource::Flatpak, name: "io.tangencylabs.solidcraft".into(), version: "0.26.1".into(), user: true });
        assert_eq!(find(sc, &installed, &[InstallSource::Flatpak]).map(|p| (p.name.as_str(), p.user)), Some(("io.tangencylabs.solidcraft", true)));
    }

    #[test]
    fn candidate_names_cover_bundle_ids_for_deb_rpm_and_pacman() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let deb_names = candidate_names(photo, InstallSource::Deb);
        assert!(deb_names.contains(&"photocraft".to_string()));
        assert!(deb_names.contains(&"ai.storyteller.photocraft".to_string()));

        let rpm_names = candidate_names(photo, InstallSource::Rpm);
        assert!(rpm_names.contains(&"photocraft".to_string()));
        assert!(rpm_names.contains(&"ai.storyteller.photocraft".to_string()));

        let pacman_names = candidate_names(photo, InstallSource::Pacman);
        assert!(pacman_names.contains(&"photocraft".to_string()));
        assert!(pacman_names.contains(&"photocraft-bin".to_string()));
        assert!(pacman_names.contains(&"ai.storyteller.photocraft".to_string()));
    }

    #[test]
    fn finds_aur_packages_in_every_flavour() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let pdf = CraftApp::by_id("pdfcraft").unwrap();
        let installed =
            pkgs(InstallSource::Pacman, parse_pacman("linux 6.11.1.arch1-1\nphotocraft-git 0.3.0.r47.g47f9306-1\nprintcraft-bin 0.2.1-1\n"));
        let hit = find(photo, &installed, &[InstallSource::Pacman]).unwrap();
        assert_eq!(hit.name, "photocraft-git");
        assert_eq!(version_of(hit).unwrap().to_string(), "0.3.0");
        // PdfCraft under its old name.
        assert_eq!(find(pdf, &installed, &[InstallSource::Pacman]).unwrap().name, "printcraft-bin");
        assert!(find(CraftApp::by_id("wordcraft").unwrap(), &installed, &[InstallSource::Pacman]).is_none());
    }

    #[test]
    fn prefers_managers_in_order_and_reads_revisions() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let mut installed = pkgs(InstallSource::Deb, parse_dpkg("photocraft|installed|0.5.0-1\n"));
        installed.extend(pkgs(InstallSource::Flatpak, parse_flatpak_list("ai.storyteller.photocraft\t0.4.0\n")));
        let hit = find(photo, &installed, &[InstallSource::Deb, InstallSource::Flatpak]).unwrap();
        assert_eq!(hit.manager, InstallSource::Deb);
        // 0.5.0-1 is the 0.5.0 release, not a pre-release of it.
        assert_eq!(version_of(hit).unwrap().to_string(), "0.5.0");
        assert_eq!(find(photo, &installed, &[InstallSource::Flatpak]).unwrap().version, "0.4.0");
    }
}
