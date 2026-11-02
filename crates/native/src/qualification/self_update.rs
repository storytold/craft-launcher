//! Run the candidate's production updater against an isolated older launcher installation.
//! The candidate artifact is seeded into the ordinary verified-download cache because it is
//! not published yet. Hash, signature, helper ownership and version checks remain unchanged.

use std::path::Path;

use artcraft_launcher_core::{Platform, catalog, release, version};

use crate::http::{Http, Stop};
use crate::paths::Paths;
use crate::self_update::{self, Install};

pub fn run(args: &[String]) -> Result<(), String> {
    let [root, kind, candidate, target] = args else {
        return Err("usage: --qualify-self-update <qualification-root> <dmg|portable|appimage> <candidate-artifact> <older-installation>".into());
    };
    super::require_runner()?;
    let root = super::checked_root(Path::new(root))?;
    let _ = super::ui_report(&root)?;
    let parent = root.parent().ok_or("no qualification parent")?;
    let target = super::canonical(Path::new(target))?;
    if !target.starts_with(parent) {
        return Err("self-update target must be an isolated installation beside the qualification root".into());
    }
    // This existing developer switch models the old target's version while executing the NEW
    // updater code. The runner also executes the target --version before requesting the update.
    if std::env::var(self_update::PRETEND_VAR).as_deref() != Ok("0.9.0") {
        return Err("self-update qualification requires the pinned previous version".into());
    }
    let current = version::parse(env!("CARGO_PKG_VERSION")).ok_or("invalid candidate version")?;
    let previous = version::parse("0.9.0").ok_or("invalid fixture version")?;
    if !version::is_newer(&current, &previous) {
        return Err("candidate must be newer than the previous-version fixture".into());
    }
    let install = match kind.as_str() {
        "dmg" if cfg!(target_os = "macos") && target.extension().is_some_and(|e| e == "app") => Install::MacApp(target.clone()),
        "portable" if cfg!(windows) && target.file_name().is_some_and(|n| n == "artcraft-launcher.exe") => Install::Portable(target.clone()),
        "appimage" if cfg!(target_os = "linux") && target.extension().is_some_and(|e| e == "AppImage") => Install::AppImage(target.clone()),
        _ => return Err("unsupported self-update qualification target".into()),
    };
    let candidate = super::canonical(Path::new(candidate))?;
    let name = candidate.file_name().and_then(|n| n.to_str()).filter(|n| release::valid_file_name(n)).ok_or("invalid candidate filename")?;
    let sha = crate::http::sha256_file(&candidate)?;
    let tag = format!("v{current}");
    let asset = release::Asset {
        name: name.into(),
        size: std::fs::metadata(&candidate).map_err(|e| e.to_string())?.len(),
        sha256: Some(sha.clone()),
        url: format!("{}{tag}/{name}", release::download_prefix(catalog::LAUNCHER_REPO)),
    };
    let release = release::Release { tag, version: Some(current), assets: vec![asset.clone()], ..Default::default() };
    let paths = if cfg!(windows) {
        let helper_paths = craft_update::Paths::for_exe(&craft_update::suite::LAUNCHER, &target)?;
        // A portable target's helper and all its data must remain inside the test installation.
        if !helper_paths.data.starts_with(parent) || !helper_paths.cache.starts_with(parent) {
            return Err("self-update helper paths escaped the qualification folder".into());
        }
        Paths { data: helper_paths.data, cache: helper_paths.cache }
    } else {
        Paths::under(&root.join("self-update"))
    };
    std::fs::create_dir_all(paths.downloads()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&paths.data).map_err(|e| e.to_string())?;
    let preferences = paths.data.join("qualification-preferences.txt");
    std::fs::write(&preferences, b"keep launcher preferences\n").map_err(|e| e.to_string())?;
    let cached = paths.downloads().join(name);
    std::fs::copy(&candidate, &cached).map_err(|e| e.to_string())?;
    if crate::http::sha256_file(&cached)? != sha {
        return Err("candidate changed while staging the update".into());
    }
    self_update::update(
        &install,
        &release,
        Platform::current().ok_or("unknown platform")?,
        &Http::new()?,
        &paths,
        &Stop::default(),
        &mut |_| {},
        &|| Ok(()),
    )?;
    let result = serde_json::json!({
        "schema": 1, "scheduled": true, "candidate_sha256": sha, "from_version": "0.9.0",
        "to_version": env!("CARGO_PKG_VERSION"), "source_sha": option_env!("ARTCRAFT_LAUNCHER_BUILD_SHA"),
        "target": target, "failure_log": paths.logs().join("self-update-error.txt"), "preferences": preferences
    });
    crate::paths::write_atomic(&root.join("self-update.json"), &serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?)
}
