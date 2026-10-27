//! GitHub releases: craft-update's parsing (re-exported, shared with every Craft app), plus the
//! launcher's own "Choose version" list.
//!
//! Everything here is untrusted input. Fix the parsing itself in craft-update
//! (storytold/craft-libs, `crates/update/src/release.rs`); only the version choice is the
//! launcher's.

pub use craft_update::release::*;

use crate::protocol::VersionTrack;
use crate::version;

/// Published releases offered for manual selection, in GitHub's order, at most 100.
/// Drafts and unparseable tags cannot be installed.
pub fn parse_github_versions(json: &str, repo: &str) -> Result<Vec<Release>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("unreadable answer from GitHub ({e})"))?;
    let list = v.as_array().ok_or("unexpected answer from GitHub")?;
    Ok(list
        .iter()
        .filter(|item| !item["draft"].as_bool().unwrap_or(false))
        // One element of the list is what `parse_github_release` reads.
        .filter_map(|item| parse_github_release(&item.to_string(), repo).ok())
        .filter(|r| r.version.is_some())
        .take(100)
        .collect())
}

/// A published, explicitly marked nightly build, not merely a beta or release candidate.
pub fn is_nightly(release: &Release) -> bool {
    release.prerelease && release.version.as_ref().is_some_and(|v| v.pre.as_str().split('.').any(|part| part.eq_ignore_ascii_case("nightly")))
}

/// Whether the release publishes a checksum to verify against: a `SHA256SUMS.txt` or an asset
/// digest.
pub fn has_published_checksum(r: &Release) -> bool {
    r.sums_asset().is_some() || r.assets.iter().any(|a| a.sha256.is_some())
}

/// Selects a published release. A nightly must be explicitly marked and carry a checksum; a pin
/// never falls back.
pub fn choose_version(releases: &[Release], track: &VersionTrack) -> Option<Release> {
    match track {
        VersionTrack::Stable => releases
            .iter()
            .filter(|r| !r.prerelease && r.version.as_ref().is_some_and(version::is_stable))
            .max_by(|a, b| a.version.cmp(&b.version))
            .cloned(),
        VersionTrack::Nightly => releases.iter().filter(|r| is_nightly(r) && has_published_checksum(r)).max_by_key(|r| r.published_at).cloned(),
        VersionTrack::Pinned(tag) => releases.iter().find(|r| r.tag == *tag).cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "storytold/photocraft";
    const HASH: &str = "8a5edab282632443219e051e4ade2d1d5bbc671c781051bf1437897cbdfea0f1";

    /// Drafts and tags that aren't versions are left out; only a pre-release marked `nightly`
    /// counts as one.
    #[test]
    fn version_choices_exclude_drafts_and_distinguish_nightly_from_betas() {
        let json = r#"[
            {"tag_name":"v0.8.0-nightly.20261028","prerelease":true,"draft":false,"assets":[]},
            {"tag_name":"v0.8.0-beta.1","prerelease":true,"draft":false,"assets":[]},
            {"tag_name":"v0.7.0","prerelease":false,"draft":false,"assets":[]},
            {"tag_name":"v0.9.0-nightly.20261029","prerelease":true,"draft":true,"assets":[]},
            {"tag_name":"not-a-version","prerelease":false,"draft":false,"assets":[]}]"#;
        let choices = parse_github_versions(json, REPO).unwrap();
        assert_eq!(choices.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(), ["v0.8.0-nightly.20261028", "v0.8.0-beta.1", "v0.7.0"]);
        assert!(is_nightly(&choices[0]));
        assert!(!is_nightly(&choices[1]));
        assert!(!is_nightly(&choices[2]));
        // Each element is read as `parse_github_release` reads it, so it carries the repo too.
        assert!(choices.iter().all(|r| r.repo == REPO));
    }

    /// A nightly with nothing to verify against is never chosen.
    #[test]
    fn a_nightly_without_published_checksums_is_unavailable() {
        let mut nightly = Release {
            tag: "v1.1.0-nightly.20261028".into(),
            version: crate::version::parse("v1.1.0-nightly.20261028"),
            prerelease: true,
            ..Default::default()
        };
        nightly.assets.push(Asset { name: "app.zip".into(), size: 3, url: "https://example.invalid/app.zip".into(), sha256: None });
        assert!(choose_version(&[nightly], &VersionTrack::Nightly).is_none());
    }

    /// Stable takes the highest stable release, nightly the newest nightly, and a pin its own tag
    /// or nothing.
    #[test]
    fn selection_follows_nightly_or_pins_an_older_release_without_beta_fallback() {
        let json = r#"[
            {"tag_name":"v0.8.0-nightly.20261028","prerelease":true,"published_at":"2026-10-28T10:00:00Z"},
            {"tag_name":"v0.8.0-beta.1","prerelease":true,"published_at":"2026-10-28T12:00:00Z"},
            {"tag_name":"v0.8.0-nightly.20261027","prerelease":true,"published_at":"2026-10-27T10:00:00Z"},
            {"tag_name":"v0.7.0","prerelease":false},
            {"tag_name":"v0.6.0","prerelease":false}]"#;
        let mut list = parse_github_versions(json, REPO).unwrap();
        for r in list.iter_mut().filter(|r| is_nightly(r)) {
            r.assets.push(Asset { name: "app.zip".into(), size: 3, url: "https://example.invalid/app.zip".into(), sha256: Some(HASH.into()) });
        }
        assert_eq!(choose_version(&list, &VersionTrack::Stable).unwrap().tag, "v0.7.0");
        assert_eq!(choose_version(&list, &VersionTrack::Nightly).unwrap().tag, "v0.8.0-nightly.20261028");
        assert_eq!(choose_version(&list, &VersionTrack::Pinned("v0.6.0".into())).unwrap().tag, "v0.6.0");
        assert!(choose_version(&list, &VersionTrack::Pinned("v9.0.0".into())).is_none());
        assert!(choose_version(&list[3..], &VersionTrack::Nightly).is_none());
    }
}
