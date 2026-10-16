//! Explicitly added GitHub sources. They never replace an entry in the official suite.

use serde::{Deserialize, Serialize};

use crate::{Os, PackageKind, Platform, Release, platform::pick_asset};

pub const MAX_SOURCES: usize = 32;
pub const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunitySource {
    pub repo: String,
    pub name: String,
    /// Release asset stem and executable name; may differ from a fork's repository name.
    pub package: String,
    pub enabled: bool,
}

impl CommunitySource {
    pub fn new(repo: &str, name: &str, package: &str) -> Result<Self, String> {
        let repo = repo.trim().strip_prefix("https://github.com/").unwrap_or(repo.trim()).trim_end_matches('/');
        let repo = repo.strip_suffix(".git").unwrap_or(repo).to_ascii_lowercase();
        let source = Self { repo, name: name.trim().to_string(), package: package.trim().to_string(), enabled: true };
        source.validate()?;
        Ok(source)
    }

    pub fn validate(&self) -> Result<(), String> {
        let (owner, repo) = self.repo.split_once('/').ok_or("Use a GitHub owner/repository or repository URL.")?;
        let owner_ok = !owner.is_empty() && owner.len() <= 39 && owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !owner_ok || !component(repo) || self.repo.len() > 140 || owner.eq_ignore_ascii_case("storytold") {
            return Err("Use a community GitHub repository; official apps already have their own catalog.".into());
        }
        if self.name.is_empty() || self.name.len() > 80 || self.name.chars().any(char::is_control) || !component(&self.package) {
            return Err("Enter a display name and a simple package name, such as pdfcraft.".into());
        }
        Ok(())
    }

    pub fn key(&self) -> String {
        self.repo.to_ascii_lowercase()
    }

    pub fn url(&self) -> String {
        format!("https://github.com/{}", self.repo)
    }

    /// GitHub repository names are case-insensitive, unlike release tags and asset names.
    pub fn parse_release(&self, json: &str) -> Result<Release, String> {
        self.validate()?;
        let mut value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let prefix = format!("{}/", self.url());
        let normalize = |value: &mut serde_json::Value| {
            if let Some(url) = value.as_str()
                && url.get(..prefix.len()).is_some_and(|s| s.eq_ignore_ascii_case(&prefix))
                && let Some(rest) = url.get(prefix.len()..)
            {
                *value = serde_json::Value::String(format!("{prefix}{rest}"));
            }
        };
        if let Some(url) = value.get_mut("html_url") {
            normalize(url);
        }
        if let Some(assets) = value.get_mut("assets").and_then(|v| v.as_array_mut()) {
            if assets.len() > 256 {
                return Err("The release contains too many assets.".into());
            }
            for asset in assets {
                if let Some(url) = asset.get_mut("browser_download_url") {
                    normalize(url);
                }
            }
        }
        let release = crate::release::parse_github_release(&value.to_string(), &self.repo)?;
        if release.tag.is_empty() || !release.tag.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b)) {
            return Err("The release tag is not supported.".into());
        }
        Ok(release)
    }

    /// Only formats which can be kept inside an isolated per-source directory.
    pub fn package_asset<'a>(&self, release: &'a Release, platform: Platform) -> Option<&'a crate::Asset> {
        let kinds: &[PackageKind] = match platform.os {
            Os::Windows => &[PackageKind::PortableZip],
            Os::Linux => &[PackageKind::AppImage],
            _ => return None,
        };
        let (asset, _) = pick_asset(release, &[&self.package], platform, kinds)?;
        let expected = format!("{}/releases/download/{}/{}", self.url(), release.tag, asset.name);
        (asset.url == expected && (1..=MAX_PACKAGE_BYTES).contains(&asset.size)).then_some(asset)
    }
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommunityAction {
    Check,
    Install,
    Launch,
    Cancel,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommunityStatus {
    pub release: Option<Release>,
    pub installed: Option<String>,
    pub error: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_scoped_and_canonical() {
        let source = CommunitySource::new("https://github.com/Example/PdfCraft.git/", "My PDF", "pdfcraft").unwrap();
        assert_eq!(source.repo, "example/pdfcraft");
        for repo in ["storytold/pdfcraft", "StoryTold/pdfcraft", "a/../b", "a/b?x", "a/b#x", "a/b/c", "a%2fb/c", "https://evil.test/a/b", "a/."] {
            assert!(CommunitySource::new(repo, "App", "app").is_err(), "{repo}");
        }
        for package in ["../app", "app.exe:stream", "/app", "a\\b", ""] {
            assert!(CommunitySource::new("example/app", "App", package).is_err());
        }
    }

    #[test]
    fn fork_of_real_craft_release_keeps_package_names_and_platform_selection() {
        // Reduced public GitHub metadata, with only the publisher changed to model a fork.
        let json = include_str!("../tests/fixtures/pdfcraft-release.json").replace("storytold/pdfcraft", "Example/pdfcraft-fork");
        let source = CommunitySource::new("example/pdfcraft-fork", "PDF fork", "pdfcraft").unwrap();
        let release = source.parse_release(&json).unwrap();
        for (os, arch) in [(Os::Windows, crate::Arch::X64), (Os::Windows, crate::Arch::Arm64), (Os::Linux, crate::Arch::X64)] {
            let asset = source.package_asset(&release, Platform { os, arch }).unwrap();
            assert!(asset.url.starts_with("https://github.com/example/pdfcraft-fork/"));
        }
        assert!(source.package_asset(&release, Platform { os: Os::MacOs, arch: crate::Arch::Arm64 }).is_none());
    }

    #[test]
    fn release_keeps_case_sensitive_assets_and_rejects_foreign_downloads() {
        let source = CommunitySource::new("example/fork", "PDF fork", "pdfcraft").unwrap();
        let json = r#"{"tag_name":"v1.0.0","assets":[
          {"name":"pdfcraft-1.0.0-windows-x64-portable.zip","size":10,"browser_download_url":"https://github.com/Example/Fork/releases/download/v1.0.0/pdfcraft-1.0.0-windows-x64-portable.zip"},
          {"name":"evil.zip","browser_download_url":"https://github.com/elsewhere/fork/releases/download/v1.0.0/evil.zip"}] }"#;
        let release = source.parse_release(json).unwrap();
        assert_eq!(release.assets.len(), 1);
        let platform = Platform { os: Os::Windows, arch: crate::Arch::X64 };
        assert!(source.package_asset(&release, platform).is_some());
        let mut changed = release.clone();
        changed.assets[0].url = changed.assets[0].url.replace("v1.0.0", "v2.0.0");
        assert!(source.package_asset(&changed, platform).is_none());
        changed = release;
        changed.assets[0].size = MAX_PACKAGE_BYTES + 1;
        assert!(source.package_asset(&changed, platform).is_none());
    }
}
