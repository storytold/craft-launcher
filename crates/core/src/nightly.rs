//! Launcher policy for the official nightly descriptor, never an executable install recipe.
//!
//! A descriptor is not proof of profile isolation. The native adapter must separately support
//! its exact app, package identity and profile contract before offering an install. Unknown
//! capabilities fail closed. Stable release parsing and the stable feed are unchanged.

use std::collections::BTreeSet;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::catalog::{AppKind, CraftApp};
use crate::release::{self, Release};

pub const MANIFEST_NAME: &str = "NIGHTLY.json";
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_ARTIFACTS: usize = 64;
pub const MAX_SIGNATURE_BYTES: usize = 16 * 1024;

/// An immutable backend/install namespace, not a preference supplied by a feed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InstallChannel {
    #[default]
    Stable,
    Nightly,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub schema: String,
    pub channel: String,
    pub app: String,
    pub repository: String,
    pub tag: String,
    pub source_commit: String,
    /// UTC build-identity seconds, fixed before building; publication can happen later.
    pub published_at: i64,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub target: Target,
    pub asset: Download,
    /// Required only for Linux tar archives; this binds bytes, never supplies signer trust.
    pub signature: Option<Download>,
    pub isolation: Isolation,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub os: String,
    pub arch: String,
    pub format: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Download {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Isolation {
    pub package_identity: String,
    pub profile_contract: String,
    pub profile_version: u32,
    pub associations: String,
}

/// Validate a bounded, checksum-verified descriptor against a published nightly release.
///
/// `release` must come from the existing non-draft GitHub versions parser and have its published
/// SHA256SUMS applied, or API digests present. No descriptor-provided URL, command, environment,
/// path, platform signature or claimed profile capability is accepted on its own authority.
///
/// # Errors
/// Refuses missing checksums, mismatched identity, unknown schema/fields, duplicate targets or
/// names and assets outside the exact selected catalog repository and tag.
pub fn parse(body: &[u8], app: &CraftApp, release: &Release) -> Result<Descriptor, String> {
    if body.is_empty() || body.len() > MAX_MANIFEST_BYTES {
        return Err("the nightly descriptor is empty or exceeds its size limit".into());
    }
    if app.kind != AppKind::Official || !app.repo.starts_with("storytold/") || !release::is_nightly(release) {
        return Err("the selected release is not an official published nightly".into());
    }
    let manifests: Vec<_> = release.assets.iter().filter(|a| a.name == MANIFEST_NAME).collect();
    let [manifest] = manifests.as_slice() else {
        return Err("the nightly must publish exactly one NIGHTLY.json descriptor".into());
    };
    if manifest.size == 0 || manifest.size > MAX_MANIFEST_BYTES as u64 || manifest.size != body.len() as u64 {
        return Err("the nightly descriptor's published size does not match".into());
    }
    let expected = manifest.sha256.as_deref().filter(|s| hex(s, 64)).ok_or("the nightly descriptor has no published SHA-256")?;
    if !expected.eq_ignore_ascii_case(&format!("{:x}", Sha256::digest(body))) {
        return Err("the nightly descriptor does not match its published SHA-256".into());
    }
    if !exact_asset_url(app.repo, &release.tag, MANIFEST_NAME, &manifest.url) {
        return Err("the nightly descriptor is outside the selected release".into());
    }
    let descriptor: Descriptor = serde_json::from_slice(body).map_err(|e| format!("unreadable nightly descriptor ({e})"))?;
    if descriptor.schema != "craft-nightly/v2"
        || descriptor.channel != "nightly"
        || descriptor.app != app.id
        || descriptor.repository != app.repo
        || descriptor.tag != release.tag
        || !hex(&descriptor.source_commit, 40)
        || !immutable_tag(&descriptor.tag, &descriptor.source_commit)
        || descriptor.source_commit.bytes().all(|b| b == b'0')
        || release.published_at.is_none_or(|published| published < descriptor.published_at)
        || descriptor.published_at <= 0
    {
        return Err("the nightly descriptor does not match the selected app, channel or release".into());
    }
    if descriptor.artifacts.is_empty() || descriptor.artifacts.len() > MAX_ARTIFACTS {
        return Err("the nightly descriptor's artifact count is outside its limit".into());
    }
    let mut targets = BTreeSet::new();
    let mut names = BTreeSet::new();
    for artifact in &descriptor.artifacts {
        if !targets.insert(&artifact.target) || !names.insert(&artifact.asset.name) {
            return Err("the nightly descriptor repeats a target or artifact name".into());
        }
        if !matches!(artifact.target.os.as_str(), "windows" | "macos" | "linux" | "freebsd")
            || !matches!(artifact.target.arch.as_str(), "x86" | "x86_64" | "aarch64")
            || !matches!(
                artifact.target.format.as_str(),
                "portable_zip" | "appimage" | "dmg" | "tar_gz" | "msi" | "deb" | "rpm" | "flatpak" | "arch_pkg"
            )
            || !name(&artifact.isolation.package_identity)
            || !artifact.isolation.package_identity.ends_with(".nightly")
            || !name(&artifact.isolation.profile_contract)
            || artifact.isolation.profile_version == 0
            || artifact.isolation.associations != "none"
        {
            return Err("the nightly descriptor has an unsupported target or isolation declaration".into());
        }
        match (&artifact.signature, artifact.target.os.as_str(), artifact.target.format.as_str()) {
            (Some(signature), "linux", "tar_gz") => {
                if signature.name != format!("{}.asc", artifact.asset.name)
                    || !names.insert(&signature.name)
                    || signature.size == 0
                    || signature.size > MAX_SIGNATURE_BYTES as u64
                    || !published_download(signature, app, release)
                {
                    return Err("the nightly detached signature does not match its bounded published archive identity".into());
                }
            }
            (None, "linux", _) | (Some(_), _, _) => return Err("the nightly has a missing or unsupported detached signature declaration".into()),
            (None, _, _) => {}
        }
        let asset = &artifact.asset;
        let published: Vec<_> = release.assets.iter().filter(|a| a.name == asset.name).collect();
        let [published] = published.as_slice() else {
            return Err("the nightly artifact is not uniquely published in the selected release".into());
        };
        if asset.name == MANIFEST_NAME
            || asset.name == "SHA256SUMS.txt"
            || asset.size == 0
            || asset.size != published.size
            || asset.url != published.url
            || !exact_asset_url(app.repo, &release.tag, &asset.name, &asset.url)
            || !hex(&asset.sha256, 64)
            || published.sha256.as_deref().is_none_or(|sha| !sha.eq_ignore_ascii_case(&asset.sha256))
        {
            return Err("the nightly artifact does not match its published identity and checksum".into());
        }
    }
    Ok(descriptor)
}

fn published_download(download: &Download, app: &CraftApp, release: &Release) -> bool {
    let assets: Vec<_> = release.assets.iter().filter(|asset| asset.name == download.name).collect();
    let [asset] = assets.as_slice() else { return false };
    download.size == asset.size
        && download.url == asset.url
        && exact_asset_url(app.repo, &release.tag, &download.name, &download.url)
        && hex(&download.sha256, 64)
        && asset.sha256.as_ref().is_some_and(|sha| sha.eq_ignore_ascii_case(&download.sha256))
}

fn immutable_tag(tag: &str, source: &str) -> bool {
    let Some(version) = crate::version::parse(tag) else {
        return false;
    };
    let parts: Vec<_> = version.pre.as_str().split('.').collect();
    let [channel, date, run, attempt, revision] = parts.as_slice() else {
        return false;
    };
    *channel == "nightly"
        && date.len() == 8
        && date.bytes().all(|b| b.is_ascii_digit())
        && run.parse::<u64>().is_ok_and(|n| n > 0)
        && attempt.parse::<u64>().is_ok_and(|n| n > 0)
        && source.len() == 40
        && *revision == format!("g{}", &source[..12])
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn name(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn exact_asset_url(repo: &str, tag: &str, name: &str, url: &str) -> bool {
    release::download_url_under(url, &release::download_prefix(repo)) && url == format!("{}{tag}/{name}", release::download_prefix(repo))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::Asset;
    use serde_json::{Value, json};

    fn fixture() -> (Value, Release, &'static CraftApp) {
        let app = crate::catalog::SUITE.iter().find(|a| a.id == "photocraft").unwrap();
        let tag = "v1.0.0-nightly.20261120.123456.1.gabcdefabcdef";
        let url = format!("https://github.com/{}/releases/download/{tag}/photocraft-nightly.zip", app.repo);
        let sha = "a".repeat(64);
        let value = json!({
            "schema":"craft-nightly/v2", "channel":"nightly", "app":app.id,
            "repository":app.repo, "tag":tag, "source_commit":"abcdefabcdefabcdefabcdefabcdefabcdefabcd",
            "published_at":1795190400_i64, "artifacts":[{
                "target":{"os":"windows","arch":"x86_64","format":"portable_zip"},
                "asset":{"name":"photocraft-nightly.zip","url":url,"size":123,"sha256":sha},
                "isolation":{"package_identity":"ai.storyteller.photocraft.nightly","profile_contract":"unproven-profile","profile_version":1,"associations":"none"}
            }]
        });
        let release = Release {
            tag: tag.into(),
            version: crate::version::parse(tag),
            prerelease: true,
            published_at: Some(1795190400),
            assets: vec![Asset { name: "photocraft-nightly.zip".into(), url, size: 123, sha256: Some(sha) }],
            ..Default::default()
        };
        (value, release, app)
    }

    fn bind(value: &Value, release: &mut Release) -> Vec<u8> {
        let body = serde_json::to_vec(value).unwrap();
        release.assets.retain(|a| a.name != MANIFEST_NAME);
        release.assets.push(Asset {
            name: MANIFEST_NAME.into(),
            size: body.len() as u64,
            url: format!("https://github.com/storytold/photocraft/releases/download/{}/NIGHTLY.json", release.tag),
            sha256: Some(format!("{:x}", Sha256::digest(&body))),
        });
        body
    }

    #[test]
    fn build_identity_time_can_precede_but_not_follow_actual_publication() {
        let (value, mut release, app) = fixture();
        release.published_at = Some(1795190400 + 3600);
        let body = bind(&value, &mut release);
        assert!(parse(&body, app, &release).is_ok());
        release.published_at = Some(1795190400 - 1);
        assert!(parse(&body, app, &release).is_err());
    }

    #[test]
    fn valid_descriptor_is_not_a_profile_installation_capability() {
        let (value, mut release, app) = fixture();
        let body = bind(&value, &mut release);
        let descriptor = parse(&body, app, &release).unwrap();
        // Parsing accepts the declaration as data, not an executable adapter or a permission.
        assert_eq!(descriptor.artifacts[0].isolation.profile_contract, "unproven-profile");
    }

    #[test]
    fn descriptor_and_payload_need_separate_published_checksums() {
        let (value, mut release, app) = fixture();
        let mut body = bind(&value, &mut release);
        body[0] ^= 1;
        assert!(parse(&body, app, &release).is_err());
        let body = bind(&value, &mut release);
        release.assets[0].sha256 = None;
        assert!(parse(&body, app, &release).is_err());
        let body = bind(&value, &mut release);
        release.assets.last_mut().unwrap().sha256 = None;
        assert!(parse(&body, app, &release).is_err());
    }

    #[test]
    fn identity_changes_cannot_borrow_another_release_or_its_trust() {
        for (field, replacement) in [
            ("app", "wordcraft"),
            ("repository", "attacker/photocraft"),
            ("tag", "nightly"),
            ("channel", "stable"),
            ("schema", "craft-nightly/v1"),
            ("source_commit", "bad"),
        ] {
            let (mut value, mut release, app) = fixture();
            value[field] = replacement.into();
            let body = bind(&value, &mut release);
            assert!(parse(&body, app, &release).is_err(), "{field}");
        }
        let (value, mut release, app) = fixture();
        let body = bind(&value, &mut release);
        release.prerelease = false;
        assert!(parse(&body, app, &release).is_err());
    }

    #[test]
    fn duplicate_targets_names_and_recipe_injection_are_refused() {
        let (mut value, mut release, app) = fixture();
        let copy = value["artifacts"][0].clone();
        value["artifacts"].as_array_mut().unwrap().push(copy);
        let body = bind(&value, &mut release);
        assert!(parse(&body, app, &release).is_err());
        for (field, injected) in [("profile_contract", "../../stable"), ("package_identity", "bad/identity"), ("associations", "stable")] {
            let (mut value, mut release, app) = fixture();
            value["artifacts"][0]["isolation"][field] = injected.into();
            let body = bind(&value, &mut release);
            assert!(parse(&body, app, &release).is_err(), "{field}");
        }
        let (mut value, mut release, app) = fixture();
        value["artifacts"][0]["isolation"]["command"] = "powershell".into();
        let body = bind(&value, &mut release);
        assert!(parse(&body, app, &release).is_err());
    }

    #[test]
    fn payload_url_size_and_digest_must_match_the_exact_release_asset() {
        for (field, replacement) in
            [("url", json!("https://github.com/attacker/app/releases/download/x/a.zip")), ("size", json!(124)), ("sha256", json!("b".repeat(64)))]
        {
            let (mut value, mut release, app) = fixture();
            value["artifacts"][0]["asset"][field] = replacement;
            let body = bind(&value, &mut release);
            assert!(parse(&body, app, &release).is_err(), "{field}");
        }
    }

    #[test]
    fn bounded_inputs_refuse_oversized_empty_and_missing_descriptors() {
        let (_, release, app) = fixture();
        assert!(parse(&[], app, &release).is_err());
        assert!(parse(&vec![b' '; MAX_MANIFEST_BYTES + 1], app, &release).is_err());
        assert!(parse(b"{}", app, &release).is_err());
        let (mut value, mut release, app) = fixture();
        value["artifacts"] = json!([]);
        let body = bind(&value, &mut release);
        assert!(parse(&body, app, &release).is_err());
    }
    fn signed_linux() -> (Value, Release, &'static CraftApp) {
        let (mut value, mut release, app) = fixture();
        value["artifacts"][0]["target"] = json!({"os":"linux","arch":"x86_64","format":"tar_gz"});
        let name = "photocraft-nightly.tar.gz";
        let signature_name = format!("{name}.asc");
        let url = format!("https://github.com/{}/releases/download/{}/{name}", app.repo, release.tag);
        value["artifacts"][0]["asset"]["name"] = name.into();
        value["artifacts"][0]["asset"]["url"] = url.clone().into();
        release.assets[0].name = name.into();
        release.assets[0].url = url;
        let signature_url = format!("https://github.com/{}/releases/download/{}/{signature_name}", app.repo, release.tag);
        value["artifacts"][0]["signature"] = json!({"name":signature_name,"url":signature_url,"size":128,"sha256":"b".repeat(64)});
        release.assets.push(Asset { name: signature_name, url: signature_url, size: 128, sha256: Some("b".repeat(64)) });
        (value, release, app)
    }

    #[test]
    fn signed_v2_requires_exact_bounded_unique_linux_signature_binding() {
        let (value, mut release, app) = signed_linux();
        let body = bind(&value, &mut release);
        assert!(parse(&body, app, &release).is_ok());
        for (field, invalid) in [
            ("name", json!("foreign.asc")),
            ("url", json!("https://github.com/attacker/repo/releases/download/tag/a.asc")),
            ("size", json!(0)),
            ("size", json!(MAX_SIGNATURE_BYTES + 1)),
            ("sha256", json!("c".repeat(64))),
            ("command", json!("untrusted")),
        ] {
            let (mut value, mut release, app) = signed_linux();
            value["artifacts"][0]["signature"][field] = invalid;
            let body = bind(&value, &mut release);
            assert!(parse(&body, app, &release).is_err(), "{field}");
        }
        for variant in 0..5 {
            let (mut value, mut release, app) = signed_linux();
            match variant {
                0 => {
                    value["artifacts"][0].as_object_mut().unwrap().remove("signature");
                }
                1 => {
                    release.assets.push(release.assets[1].clone());
                }
                2 => {
                    release.assets[1].sha256 = None;
                }
                3 => {
                    value["schema"] = "craft-nightly/v1".into();
                }
                _ => {
                    value["artifacts"][0]["target"]["os"] = "windows".into();
                }
            }
            let body = bind(&value, &mut release);
            assert!(parse(&body, app, &release).is_err(), "variant {variant}");
        }
    }
}
