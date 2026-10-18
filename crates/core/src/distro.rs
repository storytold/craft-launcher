//! Linux distributions: reading `os-release` and choosing the package formats that suit them.
//!
//! The distro decides the system package format, not which tools happen to be on `PATH`: Fedora
//! can have `apt-get` and `dpkg` installed (for building Debian images), and an `.rpm` system
//! must never be handed to apt.

use crate::platform::PackageKind;

/// The parts of `/etc/os-release` that say which distro family this is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OsRelease {
    /// `ID`, lower-cased (`fedora`, `ubuntu`, `opensuse-tumbleweed`, …). Empty when missing.
    pub id: String,
    /// `ID_LIKE`, lower-cased and split on spaces (`["rhel", "centos", "fedora"]`).
    pub id_like: Vec<String>,
}

/// Reads `os-release` text (`KEY=value` lines, values optionally quoted).
pub fn parse_os_release(text: &str) -> OsRelease {
    let mut out = OsRelease::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = unquote(value.trim()).to_ascii_lowercase();
        match key.trim() {
            "ID" => out.id = value,
            "ID_LIKE" => out.id_like = value.split_whitespace().map(str::to_string).collect(),
            _ => {}
        }
    }
    out
}

fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|v| v.strip_suffix(q)) {
            return inner;
        }
    }
    v
}

/// The distro's own package format: `.deb` for the Debian family, `.rpm` for the Fedora, Red Hat
/// and SUSE families, `.pkg.tar.zst` for the Arch Linux family, `None` for anything else
/// (unknown distros), which gets AppImages.
pub fn native_package(os: &OsRelease) -> Option<PackageKind> {
    std::iter::once(&os.id).chain(&os.id_like).find_map(|id| family(id))
}

fn family(id: &str) -> Option<PackageKind> {
    match id {
        "debian" | "ubuntu" => Some(PackageKind::Deb),
        "fedora" | "rhel" | "centos" | "suse" | "sles" => Some(PackageKind::Rpm),
        _ if id.starts_with("opensuse") => Some(PackageKind::Rpm),
        "arch" | "manjaro" | "endeavouros" | "garuda" | "cachyos" | "artix" => Some(PackageKind::ArchPkg),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEDORA: &str = r#"NAME="Fedora Linux"
VERSION="40 (Workstation Edition)"
ID=fedora
VERSION_ID=40
PRETTY_NAME="Fedora Linux 40 (Workstation Edition)"
"#;
    const UBUNTU: &str = r#"PRETTY_NAME="Ubuntu 24.04.1 LTS"
NAME="Ubuntu"
VERSION_ID="24.04"
ID=ubuntu
ID_LIKE=debian
"#;
    const MINT: &str = "NAME=\"Linux Mint\"\nID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
    const ARCH: &str = "NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\nBUILD_ID=rolling\n";
    const TUMBLEWEED: &str = "NAME=\"openSUSE Tumbleweed\"\n# a comment\nID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n";
    const ROCKY: &str = "NAME=\"Rocky Linux\"\nID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n";
    const UNKNOWN: &str = "NAME=\"Some OS\"\nID=someos\n";

    fn native(text: &str) -> Option<PackageKind> {
        native_package(&parse_os_release(text))
    }

    #[test]
    fn os_release_is_parsed() {
        assert_eq!(parse_os_release(FEDORA), OsRelease { id: "fedora".into(), id_like: vec![] });
        assert_eq!(parse_os_release(MINT), OsRelease { id: "linuxmint".into(), id_like: vec!["ubuntu".into(), "debian".into()] });
        assert_eq!(parse_os_release("ID='opensuse-leap'\n").id, "opensuse-leap");
        assert_eq!(parse_os_release(""), OsRelease::default());
    }

    #[test]
    fn the_distro_family_picks_the_package_format() {
        assert_eq!(native(FEDORA), Some(PackageKind::Rpm));
        assert_eq!(native(ROCKY), Some(PackageKind::Rpm));
        assert_eq!(native(TUMBLEWEED), Some(PackageKind::Rpm));
        assert_eq!(native(UBUNTU), Some(PackageKind::Deb));
        assert_eq!(native(MINT), Some(PackageKind::Deb));
        assert_eq!(native(ARCH), Some(PackageKind::ArchPkg));
        assert_eq!(native(UNKNOWN), None);
        assert_eq!(native(""), None);
    }
}
