//! Going back from an installed copy to an older release the person chose: which published
//! release can be put back if going back fails. The desktop backend asks this before it changes
//! anything, and the UI asks it to say beforehand when going back can't start.

use crate::protocol::VersionTrack;
use crate::release::{Release, choose_version, is_nightly};
use crate::status::{InstallSource, Installed};

/// What going back from an installed copy can put back if it fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestorePoint<'a> {
    /// The installed version's own release.
    Exact(&'a Release),
    /// A pre-release with the installed copy's numbers, for a Windows Installer copy whose
    /// version has numbers only (`0.24.0`) and no release of its own. Windows Installer drops the
    /// pre-release label (`0.24.0-rc.1` registers as `0.24.0`), so that pre-release is most likely
    /// the copy installed, but nothing proves it: it is used only to put something back, only when
    /// going back to the newest stable release, and only once the person has agreed. The copy is
    /// never shown or compared as that pre-release.
    SameNumbers(&'a Release),
    /// Nothing published can be put back.
    None,
}

/// Where going back from `installed` to `chosen` finds the release to put back, in `releases`
/// (the app's published releases).
pub fn restore_point<'a>(releases: &'a [Release], installed: &Installed, chosen: &Release) -> RestorePoint<'a> {
    let Some(have) = installed.version.as_ref() else { return RestorePoint::None };
    if let Some(exact) = releases.iter().find(|r| r.version.as_ref() == Some(have)) {
        return RestorePoint::Exact(exact);
    }
    let to_newest_stable = choose_version(releases, &VersionTrack::Stable).is_some_and(|newest| newest.tag == chosen.tag);
    if installed.source != InstallSource::Msi || !have.pre.is_empty() || !to_newest_stable {
        return RestorePoint::None;
    }
    releases
        .iter()
        .filter(|r| !is_nightly(r))
        .filter(|r| r.version.as_ref().is_some_and(|v| !v.pre.is_empty() && (v.major, v.minor, v.patch) == (have.major, have.minor, have.patch)))
        .max_by(|a, b| a.version.cmp(&b.version))
        .map_or(RestorePoint::None, RestorePoint::SameNumbers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version;

    fn release(v: &str) -> Release {
        Release { tag: format!("v{v}"), version: version::parse(v), prerelease: v.contains('-'), ..Default::default() }
    }

    fn copy(v: &str, source: InstallSource) -> Installed {
        Installed {
            version: version::parse(v),
            source,
            location: String::new(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        }
    }

    #[test]
    fn the_installed_release_is_put_back_when_it_is_published() {
        let releases = [release("0.24.0-rc.1"), release("0.23.1")];
        let rc = copy("0.24.0-rc.1", InstallSource::Msi);
        assert_eq!(restore_point(&releases, &rc, &releases[1]), RestorePoint::Exact(&releases[0]));
    }

    #[test]
    fn a_numbers_only_installer_copy_falls_back_to_the_matching_pre_release_only_for_the_newest_stable() {
        let releases = [release("0.24.0-rc.2"), release("0.24.0-rc.1"), release("0.23.1"), release("0.23.0")];
        let stuck = copy("0.24.0", InstallSource::Msi);
        // The newest pre-release with those numbers, and only for going back to the latest release.
        assert_eq!(restore_point(&releases, &stuck, &releases[2]), RestorePoint::SameNumbers(&releases[0]));
        assert_eq!(restore_point(&releases, &stuck, &releases[3]), RestorePoint::None);
        assert_eq!(restore_point(&releases, &stuck, &releases[1]), RestorePoint::None);
        // Only Windows Installer drops the label; another copy with no release of its own stays guarded.
        assert_eq!(restore_point(&releases, &copy("0.24.0", InstallSource::MacApp), &releases[2]), RestorePoint::None);
        // Nothing published with its numbers: guarded.
        assert_eq!(restore_point(&releases, &copy("0.25.0", InstallSource::Msi), &releases[2]), RestorePoint::None);
        assert_eq!(restore_point(&releases, &copy("", InstallSource::Msi), &releases[2]), RestorePoint::None);
    }
}
