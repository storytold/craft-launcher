#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, fs};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey, pkcs8::DecodePublicKey};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const CATALOG_URL: &str = "https://getartcraft.com/launcher-catalog.toml";
const SIGNATURE_URL: &str = "https://getartcraft.com/launcher-catalog.toml.sig";
const PUBLIC_KEY_PEM: &str = include_str!("assets/launcher-catalog-public-key.pem");
const MAX_CATALOG_BYTES: usize = 1 << 20;
const MAX_SIGNATURE_BYTES: usize = 128;
const MAX_ICON_BYTES: usize = 256 << 10;

#[derive(Deserialize)]
struct Manifest {
    apps: Vec<App>,
}

#[derive(Deserialize)]
struct App {
    id: String,
    name: String,
    tagline: String,
    color: [u8; 3],
    ink_light: [u8; 3],
    ink_dark: [u8; 3],
    repo: String,
    former_repo: Option<String>,
    #[serde(default)]
    former_ids: Vec<String>,
    #[serde(default)]
    former_names: Vec<String>,
    kind: String,
    publisher: String,
    msi_publisher: Option<String>,
    bundle: Option<String>,
    #[serde(default)]
    former_bundles: Vec<String>,
    site: Option<String>,
    mac_team: Option<String>,
    mac_bundle_name: Option<String>,
    icon_url: String,
    icon_sha256: String,
    details: Option<Details>,
}

#[derive(Deserialize)]
struct Details {
    headline: String,
    about: String,
    platforms: String,
    #[serde(default)]
    features: Vec<Feature>,
    #[serde(default)]
    shots: Vec<Shot>,
}

#[derive(Deserialize)]
struct Feature {
    title: String,
    text: String,
}

#[derive(Deserialize)]
struct Shot {
    key: String,
    caption: String,
}

fn optional(s: &Option<String>) -> String {
    s.as_ref().map_or_else(|| "None".to_owned(), |v| format!("Some({v:?})"))
}

fn slice(items: &[String]) -> String {
    format!("&{items:?}")
}

fn verify_signature(bytes: &[u8], signature: &[u8], key: &VerifyingKey) -> Result<(), String> {
    if bytes.len() > MAX_CATALOG_BYTES || signature.len() > MAX_SIGNATURE_BYTES {
        return Err("catalog or signature exceeds byte limit".into());
    }
    let encoded = std::str::from_utf8(signature).map_err(|_| "signature is not UTF-8")?.trim();
    let decoded = STANDARD.decode(encoded).map_err(|_| "invalid base64 signature")?;
    let sig = Signature::from_slice(&decoded).map_err(|_| "invalid Ed25519 signature length")?;
    key.verify_strict(bytes, &sig).map_err(|_| "catalog signature does not match the pinned key".into())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn build_agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    let config = ureq::Agent::config_builder()
        .tls_config(tls)
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(20)))
        .max_redirects(0)
        .user_agent("ArtCraft-Launcher-Build/1.0")
        .build();
    ureq::Agent::new_with_config(config)
}

fn fetch_bytes(agent: &ureq::Agent, url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut response = agent.get(url).call().map_err(|e| format!("GET {url}: {e}"))?;
    let mut reader = response.body_mut().as_reader();
    let mut bytes = Vec::new();
    let mut limited = std::io::Read::take(&mut reader, (max_bytes + 1) as u64);
    std::io::Read::read_to_end(&mut limited, &mut bytes).map_err(|e| format!("read {url}: {e}"))?;
    if bytes.len() > max_bytes {
        return Err(format!("{url} exceeded {max_bytes} bytes"));
    }
    Ok(bytes)
}

fn cache_dir(out_dir: &Path) -> PathBuf {
    if let Ok(explicit) = env::var("ARTCRAFT_CATALOG_CACHE_DIR")
        && !explicit.trim().is_empty()
    {
        return PathBuf::from(explicit);
    }
    // Share the build cache across OUT_DIR hashes under Cargo's target directory.
    let mut ancestor = out_dir.to_path_buf();
    while let Some(parent) = ancestor.parent() {
        if ancestor.file_name().is_some_and(|n| n == "build")
            && let Some(profile_dir) = ancestor.parent()
            && let Some(target_dir) = profile_dir.parent()
        {
            return target_dir.join("artcraft-catalog-cache");
        }
        ancestor = parent.to_path_buf();
    }
    out_dir.join("catalog-cache")
}

fn load_verified_catalog(agent: &ureq::Agent, cache: &Path, key: &VerifyingKey) -> Result<(Vec<u8>, Vec<u8>), Box<dyn Error>> {
    let cached_toml = cache.join("launcher-catalog.toml");
    let cached_sig = cache.join("launcher-catalog.toml.sig");

    if let Ok(override_dir) = env::var("ARTCRAFT_CATALOG_DIR")
        && !override_dir.trim().is_empty()
    {
        let dir = PathBuf::from(override_dir);
        let toml = fs::read(dir.join("launcher-catalog.toml"))?;
        let sig = fs::read(dir.join("launcher-catalog.toml.sig"))?;
        verify_signature(&toml, &sig, key)?;
        return Ok((toml, sig));
    }

    let remote = (|| -> Result<(Vec<u8>, Vec<u8>), String> {
        let toml = fetch_bytes(agent, CATALOG_URL, MAX_CATALOG_BYTES)?;
        let sig = fetch_bytes(agent, SIGNATURE_URL, MAX_SIGNATURE_BYTES)?;
        verify_signature(&toml, &sig, key)?;
        Ok((toml, sig))
    })();

    match remote {
        Ok((toml, sig)) => {
            let _ = fs::create_dir_all(cache);
            let _ = fs::write(&cached_toml, &toml);
            let _ = fs::write(&cached_sig, &sig);
            Ok((toml, sig))
        }
        Err(remote_err) => {
            if let (Ok(toml), Ok(sig)) = (fs::read(&cached_toml), fs::read(&cached_sig))
                && verify_signature(&toml, &sig, key).is_ok()
            {
                println!("cargo:warning=Could not fetch {CATALOG_URL} ({remote_err}); using verified local cache.");
                return Ok((toml, sig));
            }
            Err(format!("Failed to fetch signed catalog from {CATALOG_URL} ({remote_err}) and no verified local cache exists").into())
        }
    }
}

fn load_verified_icon(agent: &ureq::Agent, cache: &Path, app: &App) -> Result<Vec<u8>, Box<dyn Error>> {
    let icons_cache = cache.join("icons");
    let cached_icon = icons_cache.join(format!("{}.png", app.icon_sha256));
    if let Ok(bytes) = fs::read(&cached_icon)
        && sha256_hex(&bytes) == app.icon_sha256
    {
        return Ok(bytes);
    }

    if let Ok(override_dir) = env::var("ARTCRAFT_CATALOG_DIR")
        && !override_dir.trim().is_empty()
    {
        let candidate = PathBuf::from(override_dir).join("launcher-app-icons").join(format!("{}-{}.png", app.id, app.icon_sha256));
        if let Ok(bytes) = fs::read(&candidate)
            && sha256_hex(&bytes) == app.icon_sha256
        {
            return Ok(bytes);
        }
    }

    let bytes = fetch_bytes(agent, &app.icon_url, MAX_ICON_BYTES)?;
    if sha256_hex(&bytes) != app.icon_sha256 {
        return Err(format!("icon SHA-256 mismatch for {} from {}", app.id, app.icon_url).into());
    }
    let _ = fs::create_dir_all(&icons_cache);
    let _ = fs::write(&cached_icon, &bytes);
    Ok(bytes)
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=assets/launcher-catalog-public-key.pem");
    println!("cargo:rerun-if-env-changed=ARTCRAFT_CATALOG_DIR");
    println!("cargo:rerun-if-env-changed=ARTCRAFT_CATALOG_CACHE_DIR");
    // The override directory's catalog, signature and icons can change under an unchanged value.
    if let Ok(override_dir) = env::var("ARTCRAFT_CATALOG_DIR")
        && !override_dir.trim().is_empty()
    {
        println!("cargo:rerun-if-changed={override_dir}");
    }

    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR missing")?);
    let cache = cache_dir(&out);
    let key = VerifyingKey::from_public_key_pem(PUBLIC_KEY_PEM)?;
    let agent = build_agent();

    let (catalog_bytes, sig_bytes) = load_verified_catalog(&agent, &cache, &key)?;
    let catalog_text = std::str::from_utf8(&catalog_bytes)?;
    let manifest: Manifest = toml::from_str(catalog_text)?;

    fs::write(out.join("catalog.toml"), &catalog_bytes)?;
    fs::write(out.join("catalog.toml.sig"), &sig_bytes)?;

    let out_icons = out.join("app-icons");
    fs::create_dir_all(&out_icons)?;

    // Installer artwork uses the same verified catalog order, with no hard-coded app list.
    let installer_apps = manifest.apps.iter().filter(|app| app.kind == "official").map(|app| app.id.as_str()).collect::<Vec<_>>().join("\n");
    fs::write(out.join("installer-apps.txt"), installer_apps)?;
    let mut apps = format!("pub const SUITE: [CraftApp; {}] = [\n", manifest.apps.len());
    let count = manifest.apps.iter().filter(|a| a.details.is_some()).count();
    let mut details = format!("pub const DETAILS: [AppDetails; {count}] = [\n");
    let mut icons = String::from("fn bundled_app_icon_png(id: &str) -> Option<&'static [u8]> {\n    Some(match id {\n");

    for app in &manifest.apps {
        let kind = match app.kind.as_str() {
            "official" => "Official",
            "community" => "Community",
            _ => return Err(format!("unknown app kind for {}", app.id).into()),
        };
        let icon_bytes = load_verified_icon(&agent, &cache, app)?;
        let icon_file = format!("{}.png", app.id);
        fs::write(out_icons.join(&icon_file), &icon_bytes)?;
        writeln!(icons, "        {:?} => include_bytes!(concat!(env!(\"OUT_DIR\"), \"/app-icons/{icon_file}\")),", app.id)?;

        writeln!(
            apps,
            "CraftApp {{ id: {:?}, name: {:?}, tagline: {:?}, color: {:?}, ink_light: {:?}, ink_dark: {:?}, repo: {:?}, former_repo: {}, former_ids: {}, former_names: {}, kind: AppKind::{kind}, publisher: {:?}, msi_publisher: {}, bundle: {}, former_bundles: {}, site: {}, mac_team: {}, mac_bundle_name: {} }},",
            app.id,
            app.name,
            app.tagline,
            app.color,
            app.ink_light,
            app.ink_dark,
            app.repo,
            optional(&app.former_repo),
            slice(&app.former_ids),
            slice(&app.former_names),
            app.publisher,
            optional(&app.msi_publisher),
            optional(&app.bundle),
            slice(&app.former_bundles),
            optional(&app.site),
            optional(&app.mac_team),
            optional(&app.mac_bundle_name)
        )?;
        if let Some(d) = &app.details {
            write!(
                details,
                "AppDetails {{ id: {:?}, headline: {:?}, about: {:?}, platforms: {:?}, features: &[",
                app.id, d.headline, d.about, d.platforms
            )?;
            for f in &d.features {
                write!(details, "feature({:?}, {:?}),", f.title, f.text)?;
            }
            write!(details, "], shots: &[")?;
            for s in &d.shots {
                write!(details, "shot({:?}, {:?}),", s.key, s.caption)?;
            }
            writeln!(details, "] }},")?;
        }
    }
    apps.push_str("];\n");
    details.push_str("];\n");
    icons.push_str("        _ => return None,\n    })\n}\n");

    fs::write(out.join("catalog.rs"), apps)?;
    fs::write(out.join("details.rs"), details)?;
    fs::write(out.join("app_icons.rs"), icons)?;
    Ok(())
}
