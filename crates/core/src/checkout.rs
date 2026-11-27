//! A Craft App's source checkout: the version its `Cargo.toml` declares.
//!
//! Developer builds of the launcher (the `local-checkouts` feature) open apps built from a folder
//! of checkouts. The version shown for such a build is the one the checkout declares, read here
//! from the manifest text the native side loads.

use serde::Deserialize;

use crate::version::{self, Version};

#[derive(Deserialize)]
struct Manifest {
    package: Option<Package>,
    workspace: Option<Workspace>,
}

#[derive(Deserialize)]
struct Workspace {
    package: Option<Package>,
}

/// `version` is a string in `[package]` and `[workspace.package]`, but a member's
/// `version.workspace = true` is a table, which this leaves unread instead of failing on.
#[derive(Deserialize)]
struct Package {
    version: Option<toml::Value>,
}

impl Package {
    fn version(&self) -> Option<Version> {
        self.version.as_ref()?.as_str().and_then(version::parse)
    }
}

/// The version a checkout's root `Cargo.toml` declares: `[workspace.package] version` (the Craft
/// Apps' layout) or else `[package] version` (a single-crate repository). `None` when the text
/// isn't a manifest or names no version.
pub fn version(manifest: &str) -> Option<Version> {
    let manifest: Manifest = toml::from_str(manifest).ok()?;
    let workspace = manifest.workspace.and_then(|w| w.package).and_then(|p| p.version());
    workspace.or_else(|| manifest.package.and_then(|p| p.version()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_version_wins_over_the_root_package_inheriting_it() {
        let text = "[package]\nname = \"anicraft\"\nversion.workspace = true\n\n[workspace.package]\nversion = \"0.3.0\"\n";
        assert_eq!(version(text), version::parse("0.3.0"));
    }

    #[test]
    fn a_single_crate_repository_gives_its_package_version() {
        assert_eq!(version("[package]\nname = \"x\"\nversion = \"1.2.0-rc.1\"\n"), version::parse("1.2.0-rc.1"));
    }

    #[test]
    fn text_that_is_not_a_versioned_manifest_gives_none() {
        assert_eq!(version("[workspace]\nmembers = [\"crates/*\"]\n"), None);
        assert_eq!(version("not = [toml"), None);
        assert_eq!(version("[package]\nversion = \"soon\"\n"), None);
    }
}
