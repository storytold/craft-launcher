//! Signed website catalog. The bundled catalog remains available without any network.
//!
//! One bounded, immutable snapshot is activated per process. Its strings are interned only
//! inside `OnceLock::get_or_init`, preserving the existing static app handles used by jobs.
//! No repeated refresh can leak another snapshot or change a running install's identity.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey, pkcs8::DecodePublicKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::catalog::{ARTCRAFT_TEAM_ID, AppKind, CraftApp, SUITE};
use crate::details::{AppDetails, Feature, Shot};

pub const CATALOG_URL: &str = "https://getartcraft.com/launcher-catalog.toml";
pub const SIGNATURE_URL: &str = "https://getartcraft.com/launcher-catalog.toml.sig";
pub const PUBLIC_KEY_PEM: &str = include_str!("../assets/launcher-catalog-public-key.pem");
pub const MAX_CATALOG_BYTES: usize = 1 << 20;
pub const MAX_SIGNATURE_BYTES: usize = 128;
pub const MAX_ICON_BYTES: usize = 256 << 10;
const MAX_APPS: usize = 64;

static ACTIVE: OnceLock<Snapshot> = OnceLock::new();

struct Snapshot {
    apps: Vec<CraftApp>,
    details: Vec<AppDetails>,
    metadata: HashMap<String, Metadata>,
    icons: HashMap<String, Vec<u8>>,
}

/// Only desktop metadata consumed by existing integration. Package identities stay pinned.
pub struct Metadata {
    pub desktop_categories: String,
    pub startup_wm_class: String,
    pub icon_sha256: String,
}

#[derive(Deserialize)]
struct Document {
    apps: Vec<toml::Value>,
}

#[derive(Deserialize)]
struct Record {
    id: String,
    name: String,
    tagline: String,
    color: [u8; 3],
    ink_light: [u8; 3],
    ink_dark: [u8; 3],
    repo: String,
    #[serde(default)]
    former_ids: Vec<String>,
    #[serde(default)]
    former_names: Vec<String>,
    #[serde(default)]
    former_bundles: Vec<String>,
    kind: String,
    publisher: String,
    msi_publisher: Option<String>,
    bundle: Option<String>,
    site: Option<String>,
    mac_team: Option<String>,
    mac_bundle_name: Option<String>,
    icon_url: String,
    icon_sha256: String,
    desktop_categories: Vec<String>,
    startup_wm_class: String,
    details: Option<Page>,
}

#[derive(Deserialize)]
struct Page {
    headline: String,
    about: String,
    platforms: String,
    #[serde(default)]
    features: Vec<PageFeature>,
    #[serde(default)]
    shots: Vec<PageShot>,
}

#[derive(Deserialize)]
struct PageFeature {
    title: String,
    text: String,
}

#[derive(Deserialize)]
struct PageShot {
    key: String,
    caption: String,
    url: String,
}

/// A verified, policy-checked catalog, not yet made visible to running jobs.
pub struct VerifiedCatalog {
    records: Vec<Record>,
    pub rejected_entries: usize,
}

impl VerifiedCatalog {
    /// Signature verification always precedes parsing; even cached bytes are reverified.
    pub fn verify(bytes: &[u8], signature: &[u8]) -> Result<Self, String> {
        let key = VerifyingKey::from_public_key_pem(PUBLIC_KEY_PEM).map_err(|_| "invalid compiled catalog key")?;
        Self::verify_with_key(bytes, signature, &key)
    }

    fn verify_with_key(bytes: &[u8], signature: &[u8], key: &VerifyingKey) -> Result<Self, String> {
        if bytes.len() > MAX_CATALOG_BYTES || signature.len() > MAX_SIGNATURE_BYTES {
            return Err("catalog or signature exceeds its byte limit".into());
        }
        let encoded = std::str::from_utf8(signature).map_err(|_| "signature is not UTF-8")?.trim();
        let decoded = STANDARD.decode(encoded).map_err(|_| "invalid base64 catalog signature")?;
        let signature = Signature::from_slice(&decoded).map_err(|_| "invalid Ed25519 signature length")?;
        key.verify_strict(bytes, &signature).map_err(|_| "catalog signature does not match the pinned key")?;
        Self::parse(bytes)
    }

    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "catalog is not UTF-8")?;
        let document: Document = toml::from_str(text).map_err(|e| format!("invalid catalog TOML: {e}"))?;
        if document.apps.is_empty() || document.apps.len() > MAX_APPS {
            return Err("catalog app count is outside the limit".into());
        }
        let mut records = Vec::new();
        let mut seen = HashSet::new();
        let mut rejected_entries = 0;
        for value in document.apps {
            let record: Record = match value.try_into() {
                Ok(record) => record,
                Err(_) => {
                    rejected_entries += 1;
                    continue;
                }
            };
            if !valid_record(&record) || !seen.insert(record.id.clone()) {
                rejected_entries += 1;
                continue;
            }
            records.push(record);
        }
        if records.is_empty() {
            return Err("catalog has no acceptable apps".into());
        }
        Ok(Self { records, rejected_entries })
    }

    /// URLs/hashes are already validated. Fetchers may use a cached or bundled matching icon.
    pub fn icons(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.records.iter().map(|r| (r.id.as_str(), r.icon_url.as_str(), r.icon_sha256.as_str()))
    }

    /// Publishes once per process; omitted bundled apps stay manageable after a remote removal.
    pub fn activate(self, icons: HashMap<String, Vec<u8>>) {
        ACTIVE.get_or_init(|| self.into_snapshot(icons));
    }

    fn into_snapshot(self, icons: HashMap<String, Vec<u8>>) -> Snapshot {
        let mut apps = Vec::new();
        let mut details = Vec::new();
        let mut metadata = HashMap::new();
        let mut kept_icons = HashMap::new();
        for record in self.records {
            if let Some(icon) = icons.get(&record.id).filter(|b| valid_icon(b, &record.icon_sha256)) {
                kept_icons.insert(record.id.clone(), icon.clone());
            }
            metadata.insert(
                record.id.clone(),
                Metadata {
                    desktop_categories: record.desktop_categories.join(";") + ";",
                    startup_wm_class: record.startup_wm_class.clone(),
                    icon_sha256: record.icon_sha256.clone(),
                },
            );
            if let Some(page) = record.details {
                details.push(AppDetails {
                    id: intern(record.id.clone()),
                    headline: intern(page.headline),
                    about: intern(page.about),
                    platforms: intern(page.platforms),
                    features: Box::leak(
                        page.features
                            .into_iter()
                            .map(|f| Feature { title: intern(f.title), text: intern(f.text) })
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
                    ),
                    shots: Box::leak(
                        page.shots
                            .into_iter()
                            .map(|s| Shot { key: intern(s.key), caption: intern(s.caption) })
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
                    ),
                });
            }
            apps.push(CraftApp {
                id: intern(record.id),
                name: intern(record.name),
                tagline: intern(record.tagline),
                color: record.color,
                ink_light: record.ink_light,
                ink_dark: record.ink_dark,
                repo: intern(record.repo),
                former_ids: intern_list(record.former_ids),
                former_names: intern_list(record.former_names),
                former_bundles: intern_list(record.former_bundles),
                mac_bundle_name: record.mac_bundle_name.map(intern),
                kind: if record.kind == "official" { AppKind::Official } else { AppKind::Community },
                publisher: intern(record.publisher),
                msi_publisher: record.msi_publisher.map(intern),
                bundle: record.bundle.map(intern),
                site: record.site.map(intern),
                mac_team: record.mac_team.map(intern),
            });
        }
        for app in &SUITE {
            if !apps.iter().any(|a| a.id == app.id) {
                apps.push(*app);
            }
        }
        Snapshot { apps, details, metadata, icons: kept_icons }
    }
}

/// Runtime catalog, with the compiled suite as the offline/invalid-response fallback.
pub fn apps() -> &'static [CraftApp] {
    ACTIVE.get().map_or(&SUITE, |s| s.apps.as_slice())
}
pub fn details(id: &str) -> Option<&'static AppDetails> {
    ACTIVE.get()?.details.iter().find(|p| p.id == id)
}
pub fn metadata(id: &str) -> Option<&'static Metadata> {
    ACTIVE.get()?.metadata.get(id)
}
pub fn icon(id: &str) -> Option<&'static [u8]> {
    ACTIVE.get()?.icons.get(id).map(Vec::as_slice)
}

/// Bound decoded allocation before handing an authenticated PNG to an image loader.
pub fn valid_icon(bytes: &[u8], digest: &str) -> bool {
    if bytes.len() > MAX_ICON_BYTES || bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16) != Some(b"IHDR") {
        return false;
    }
    let Some(size) = bytes.get(16..24) else { return false };
    let Ok(size) = <&[u8; 8]>::try_from(size) else { return false };
    let [a, b, c, d, e, f, g, h] = *size;
    let (w, h) = (u32::from_be_bytes([a, b, c, d]), u32::from_be_bytes([e, f, g, h]));
    w > 0 && h > 0 && w <= 512 && h <= 512 && format!("{:x}", Sha256::digest(bytes)) == digest
}

fn valid_record(r: &Record) -> bool {
    if !identifier(&r.id)
        || r.id.as_bytes().first().is_none_or(|c| !c.is_ascii_lowercase())
        || r.id == "artcraft-launcher"
        || r.name.is_empty()
        || r.name.len() > 128
        || r.name.chars().any(char::is_control)
        || !r.id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || r.name.contains(['/', '\\'])
        || !matches!(r.kind.as_str(), "official" | "community")
    {
        return false;
    }
    let bundle = r.bundle.clone().unwrap_or_else(|| format!("ai.storyteller.{}", r.id));
    if !bundle.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) || bundle.is_empty() {
        return false;
    }
    // A signature cannot expand the compiled community allowlist or change existing install
    // identities. These checks keep native detection/uninstall and publisher verification pinned.
    if let Some(pinned) = SUITE.iter().find(|a| a.id == r.id) {
        if r.repo != pinned.repo
            || (r.kind == "official") != (pinned.kind == AppKind::Official)
            || r.publisher != pinned.publisher
            || r.msi_publisher.as_deref() != pinned.msi_publisher
            || bundle != pinned.bundle_id()
            || r.former_ids != pinned.former_ids
            || r.former_names != pinned.former_names
            || r.former_bundles != pinned.former_bundles
            || r.mac_team.as_deref() != pinned.mac_team
            || r.mac_bundle_name.as_deref().unwrap_or(&r.name) != pinned.mac_bundle_name()
            || r.name != pinned.name
        {
            return false;
        }
    } else if r.kind != "official"
        || r.repo != format!("storytold/{}", r.id)
        || r.publisher != "ArtCraft"
        || r.msi_publisher.is_some()
        || r.bundle.is_some()
        || r.mac_team.as_deref() != Some(ARTCRAFT_TEAM_ID)
        || !r.former_ids.is_empty()
        || !r.former_names.is_empty()
        || !r.former_bundles.is_empty()
        || r.mac_bundle_name.as_deref().is_some_and(|name| name != r.name)
    {
        return false;
    }
    if r.desktop_categories.len() > 16
        || !r.desktop_categories.iter().all(|c| identifier(c))
        || r.startup_wm_class.is_empty()
        || r.startup_wm_class.chars().any(char::is_control)
    {
        return false;
    }
    if r.site.as_ref().is_some_and(|s| !s.starts_with("https://") || s.chars().any(char::is_control)) {
        return false;
    }
    if r.icon_sha256.len() != 64
        || !r.icon_sha256.bytes().all(|c| c.is_ascii_hexdigit())
        || r.icon_url != format!("https://getartcraft.com/launcher-app-icons/{}-{}.png", r.id, r.icon_sha256)
    {
        return false;
    }
    r.details.as_ref().is_none_or(|p| {
        p.features.len() <= 16
            && p.shots.len() <= 16
            && p.shots.iter().all(|s| {
                identifier(&s.key)
                    && s.url == format!("{}/{}/{}/{}.jpg", crate::details::SCREENSHOTS_BASE, crate::details::SCREENSHOTS_REV, r.id, s.key)
            })
    })
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 80 && value.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}
fn intern(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}
fn intern_list(values: Vec<String>) -> &'static [&'static str] {
    Box::leak(values.into_iter().map(intern).collect::<Vec<_>>().into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const BUNDLED: &[u8] = include_bytes!("../catalog.toml");
    const WEBSITE: &[u8] = include_bytes!("../tests/fixtures/hosted-catalog.toml");
    const SIGNATURE: &[u8] = include_bytes!("../tests/fixtures/hosted-catalog.toml.sig");

    fn signed(bytes: &[u8]) -> Result<VerifiedCatalog, String> {
        let key = SigningKey::from_bytes(&[7; 32]);
        let signature = STANDARD.encode(key.sign(bytes).to_bytes());
        VerifiedCatalog::verify_with_key(bytes, signature.as_bytes(), &key.verifying_key())
    }

    fn first_record() -> Record {
        let document: Document = toml::from_str(std::str::from_utf8(BUNDLED).unwrap()).unwrap();
        document.apps.into_iter().next().unwrap().try_into().unwrap()
    }

    #[test]
    fn website_fixture_verifies_with_the_actual_pinned_public_key() {
        let catalog = VerifiedCatalog::verify(WEBSITE, SIGNATURE).unwrap();
        assert_eq!(catalog.records.len(), SUITE.len());
        assert_eq!(catalog.rejected_entries, 0);
        for record in &catalog.records {
            let bundled = SUITE.iter().find(|app| app.id == record.id).unwrap();
            assert_eq!(record.repo, bundled.repo);
        }
    }

    #[test]
    fn tampered_wrong_key_missing_and_oversized_signatures_are_rejected() {
        let mut tampered = WEBSITE.to_vec();
        tampered.push(b'\n');
        assert!(VerifiedCatalog::verify(&tampered, SIGNATURE).is_err());
        let wrong = STANDARD.encode(SigningKey::from_bytes(&[7; 32]).sign(WEBSITE).to_bytes());
        assert!(VerifiedCatalog::verify(WEBSITE, wrong.as_bytes()).is_err());
        for signature in [b"".as_slice(), b"not-base64", &[b'a'; MAX_SIGNATURE_BYTES + 1]] {
            assert!(VerifiedCatalog::verify(BUNDLED, signature).is_err());
        }
        assert!(VerifiedCatalog::verify(&vec![0; MAX_CATALOG_BYTES + 1], SIGNATURE).is_err());
    }

    #[test]
    fn signed_fixture_keeps_lf_bytes_on_every_platform() {
        assert!(!WEBSITE.contains(&b'\r'));
        assert!(VerifiedCatalog::verify(WEBSITE, SIGNATURE).is_ok());
        let crlf = std::str::from_utf8(WEBSITE).unwrap().replace("\n", "\r\n");
        assert!(VerifiedCatalog::verify(crlf.as_bytes(), SIGNATURE).is_err());
    }

    #[test]
    fn malformed_entries_and_unknown_fields_do_not_spoil_valid_apps() {
        let text = std::str::from_utf8(BUNDLED).unwrap();
        let text = format!("unknown_top_level = true\n{text}\n[[apps]]\nid = 42\n");
        let verified = signed(text.as_bytes()).unwrap();
        assert_eq!(verified.records.len(), SUITE.len());
        assert_eq!(verified.rejected_entries, 1);
        assert!(signed(b"not = [valid").is_err());
        assert!(signed(b"apps = []").is_err());
    }

    #[test]
    fn signature_cannot_expand_community_or_change_pinned_install_identity() {
        let mut r = first_record();
        r.repo = "attacker/photocraft".into();
        assert!(!valid_record(&r));
        r = first_record();
        r.mac_team = Some("OTHERTEAM1".into());
        assert!(!valid_record(&r));
        r = first_record();
        r.bundle = Some("other.bundle".into());
        assert!(!valid_record(&r));
        r = first_record();
        r.msi_publisher = Some("Other Publisher".into());
        assert!(!valid_record(&r));
        r = first_record();
        r.former_ids.push("unrelated".into());
        assert!(!valid_record(&r));
        r = first_record();
        r.name = "../other".into();
        assert!(!valid_record(&r));
        r = future_app();
        r.kind = "community".into();
        r.repo = "unapproved/examplecraft".into();
        assert!(!valid_record(&r));
    }

    fn future_app() -> Record {
        let mut r = first_record();
        r.id = "examplecraft".into();
        r.name = "ExampleCraft".into();
        r.repo = "storytold/examplecraft".into();
        r.mac_bundle_name = Some(r.name.clone());
        r.startup_wm_class = "ai.storyteller.examplecraft".into();
        r.icon_url = format!("https://getartcraft.com/launcher-app-icons/{}-{}.png", r.id, r.icon_sha256);
        r.details = None;
        r
    }

    #[test]
    fn future_official_app_needs_no_launcher_release_but_stays_within_policy() {
        let mut r = future_app();
        assert!(valid_record(&r));
        r.id = "../escape".into();
        assert!(!valid_record(&r));
        r = future_app();
        r.id = "-option".into();
        r.repo = "storytold/-option".into();
        r.icon_url = format!("https://getartcraft.com/launcher-app-icons/{}-{}.png", r.id, r.icon_sha256);
        assert!(!valid_record(&r));
        r = future_app();
        r.icon_url = "https://other.example/icon.png".into();
        assert!(!valid_record(&r));
        r = future_app();
        r.startup_wm_class = "example\nExec=other".into();
        assert!(!valid_record(&r));
        r = future_app();
        r.desktop_categories.push("Graphics;\nExec=other".into());
        assert!(!valid_record(&r));
        r = first_record();
        r.details.as_mut().unwrap().shots[0].url = "https://other.example/shot.jpg".into();
        assert!(!valid_record(&r));
    }

    #[test]
    fn icon_hashes_and_decode_dimensions_are_checked() {
        let bytes = crate::catalog::app_icon_png("photocraft").unwrap();
        let digest = format!("{:x}", Sha256::digest(bytes));
        assert!(valid_icon(bytes, &digest));
        assert!(!valid_icon(bytes, &"0".repeat(64)));
        assert!(!valid_icon(&bytes[..20], &digest));
        let mut huge = bytes.to_vec();
        huge[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(!valid_icon(&huge, &format!("{:x}", Sha256::digest(&huge))));
    }
    #[test]
    fn runtime_snapshot_adds_apps_details_icons_and_retains_omitted_bundled_apps() {
        let mut record = future_app();
        record.details = Some(Page {
            headline: "New app".into(),
            about: "Signed copy".into(),
            platforms: "Linux".into(),
            features: vec![PageFeature { title: "Feature".into(), text: "Description".into() }],
            shots: vec![],
        });
        let icon = crate::catalog::app_icon_png("photocraft").unwrap().to_vec();
        let snapshot =
            VerifiedCatalog { records: vec![record], rejected_entries: 0 }.into_snapshot(HashMap::from([("examplecraft".into(), icon.clone())]));
        assert_eq!(snapshot.apps.len(), SUITE.len() + 1);
        assert_eq!(snapshot.apps[0].id, "examplecraft");
        assert_eq!(snapshot.apps[0].repo, "storytold/examplecraft");
        assert_eq!(snapshot.details[0].headline, "New app");
        assert_eq!(snapshot.metadata["examplecraft"].startup_wm_class, "ai.storyteller.examplecraft");
        assert_eq!(snapshot.icons["examplecraft"], icon);
        assert!(SUITE.iter().all(|app| snapshot.apps.iter().any(|a| a.id == app.id)));
    }
}
