//! Verified nightly discovery and the sole source-owned per-target admission token.
//! A raw release or descriptor cannot construct this token or redirect the stable installer.

use artcraft_launcher_core::{Asset, CraftApp, InstallSource, Installed, Platform, Release};

use crate::http::Http;
use crate::manifest::Record;
use crate::nightly_profile::{self, Adapter};
use crate::paths::Paths;
#[cfg(windows)]
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct VerifiedRelease {
    release: Release,
    asset: Asset,
    signature: Option<artcraft_launcher_core::nightly::Download>,
    adapter: Adapter,
}

impl VerifiedRelease {
    pub(crate) fn release(&self) -> &Release {
        &self.release
    }
    pub(crate) fn asset(&self) -> &Asset {
        &self.asset
    }
    pub(crate) fn signature(&self) -> Option<&artcraft_launcher_core::nightly::Download> {
        self.signature.as_ref()
    }
    pub(crate) fn adapter(&self) -> Adapter {
        self.adapter
    }
}

/// Only this function can create an installable release. Both independently published
/// checksums, the exact logical GitHub release and the typed profile row must agree.
pub(crate) fn verify(app: &CraftApp, mut release: Release, http: &Http, platform: Platform) -> Result<VerifiedRelease, String> {
    use artcraft_launcher_core::nightly::{MANIFEST_NAME, MAX_MANIFEST_BYTES};
    nightly_profile::admit_new(app, platform)?;
    if let Some(sums) = release.sums_asset() {
        let expected = format!("https://github.com/{}/releases/download/{}/{}", app.repo, release.tag, sums.name);
        if sums.url != expected || !artcraft_launcher_core::release::valid_file_name(&sums.name) {
            return Err("The nightly checksums are outside the selected catalog release.".into());
        }
        let text = http.get_text(&sums.url)?.ok_or("The nightly checksums are not published.")?;
        release.apply_sums(&artcraft_launcher_core::release::parse_sha256sums(&text));
    }
    let descriptor = release
        .assets
        .iter()
        .find(|asset| asset.name == MANIFEST_NAME)
        .ok_or("This nightly has no verified isolation descriptor; the stable copy was left alone.")?;
    if descriptor.size == 0 || descriptor.size > MAX_MANIFEST_BYTES as u64 || descriptor.sha256.is_none() {
        return Err("This nightly has no bounded, checksum-verifiable isolation descriptor.".into());
    }
    let expected = format!("https://github.com/{}/releases/download/{}/NIGHTLY.json", app.repo, release.tag);
    if descriptor.url != expected
        || !artcraft_launcher_core::release::download_url_under(&descriptor.url, &artcraft_launcher_core::release::download_prefix(app.repo))
    {
        return Err("The nightly descriptor is outside the selected catalog release.".into());
    }
    let body = http.get_bytes(&descriptor.url, MAX_MANIFEST_BYTES)?.ok_or("The nightly descriptor is not published.")?;
    let descriptor = artcraft_launcher_core::nightly::parse(&body, app, &release)?;
    let (artifact, adapter) = descriptor
        .artifacts
        .iter()
        .find_map(|artifact| nightly_profile::adapter(app, &release, artifact, platform).ok().map(|adapter| (artifact, adapter)))
        .ok_or("No verified separate-profile installation adapter is available for this nightly target.")?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == artifact.asset.name)
        .ok_or("The verified nightly payload is missing from the selected release.")?
        .clone();
    Ok(VerifiedRelease { release, asset, signature: artifact.signature.clone(), adapter })
}

/// Both discovery and the legacy verifier use the current descriptor validator. A valid
/// descriptor without a source-owned matching row is unsupported, not an invalid descriptor.
pub(crate) fn verified_descriptor(app: &CraftApp, release: Release, body: &[u8], platform: Platform) -> Result<Option<VerifiedRelease>, String> {
    let descriptor = artcraft_launcher_core::nightly::parse(body, app, &release)?;
    let Some((artifact, adapter)) = descriptor
        .artifacts
        .iter()
        .find_map(|artifact| nightly_profile::adapter(app, &release, artifact, platform).ok().map(|adapter| (artifact, adapter)))
    else {
        return Ok(None);
    };
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == artifact.asset.name)
        .ok_or("The verified nightly payload is missing from the selected release.")?
        .clone();
    Ok(Some(VerifiedRelease { release, asset, signature: artifact.signature.clone(), adapter }))
}

/// Retained copies come only from the private manifest and the known profile row. This never
/// asks the host package manager, searches PATH or adopts an OS-detected stable installation.
pub(crate) fn owned_installation(app: &CraftApp, record: &Record, paths: &Paths, platform: Platform) -> Result<Installed, String> {
    let adapter = nightly_profile::for_app(app, platform)?;
    adapter.validate_record(paths, record)?;
    if !record.path.is_file() {
        return Err("The recorded nightly executable is missing.".into());
    }
    #[cfg(windows)]
    if crate::detect::windows::executable_version(&record.path) != artcraft_launcher_core::version::parse(&record.version) {
        return Err("The retained program no longer matches its recorded nightly version; it was not opened.".into());
    }
    let location = record.path.to_str().ok_or("The nightly program path cannot be represented without loss.")?.to_owned();
    Ok(Installed {
        version: artcraft_launcher_core::version::parse(&record.version),
        source: InstallSource::Launcher,
        location,
        package: None,
        externally_managed: false,
        origin: None,
        installation: None,
    })
}

/// OFF preserves this operation: only an owned program and the fixed source-owned profile
/// selector are used. Caller filenames follow the literal option delimiter.
pub(crate) fn launch(
    app: &CraftApp,
    record: &Record,
    paths: &Paths,
    platform: Platform,
    files: &[String],
    stop: &crate::http::Stop,
) -> Result<(), String> {
    owned_installation(app, record, paths, platform)?;
    let adapter = nightly_profile::for_app(app, platform)?;
    adapter.validate_profile(paths)?;
    let _program = crate::nightly_fs::Dir::open(&paths.apps().join(app.id))?;
    #[cfg(unix)]
    crate::nightly_unix::approve(adapter, paths, record)?;
    #[cfg(windows)]
    let _authenticated = {
        let root = crate::nightly_transaction::record_root(record, &paths.apps().join(app.id), adapter.executable())?.to_path_buf();
        let files = record
            .files
            .iter()
            .map(|file| file.strip_prefix(&root).map(Path::to_path_buf).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        // Kept alive through CreateProcess: deny writes, replacement and deletion after approval.
        crate::nightly_signing::authenticate_windows(&root, &files)?
    };
    stop.check()?;
    let profile = adapter.profile_root(paths)?;
    let _profile = crate::nightly_fs::Dir::ensure(&profile)?;
    adapter.validate_profile(paths)?;
    crate::process::launch_binary(&record.path, &adapter.launch_arguments(paths, files)?)
}

/// Removal is program-only. Persistent profiles live outside this directory and are never
/// included in the recorded portable files; there is no stable/OS uninstall fallback.
pub(crate) fn remove(
    app: &CraftApp,
    record: &Record,
    paths: &Paths,
    platform: Platform,
    stop: &crate::http::Stop,
    lease: &crate::nightly_transaction::Lease,
    commit: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    owned_installation(app, record, paths, platform)?;
    stop.check()?;
    if crate::process::is_running(&record.path) {
        return Err(format!("{} is running. Quit it, then try again.", app.name));
    }
    crate::nightly_transaction::remove(lease, record, commit, &mut |_| Ok(()))
}

#[cfg(test)]
mod tests {
    use artcraft_launcher_core::CraftApp;
    use artcraft_launcher_core::protocol::VersionTrack;
    use artcraft_launcher_core::release::ordinary_track_allowed;

    #[test]
    fn old_nightly_and_pinned_nightly_settings_cannot_redirect_separately_channelled_stable_updaters() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let legacy = CraftApp::by_id("solidcraft").unwrap();
        for track in [VersionTrack::Nightly, VersionTrack::Pinned("v1.0.0-nightly.20261120.1.1.gabcdefabcdef".into())] {
            assert!(!ordinary_track_allowed(photo, &track));
            assert!(ordinary_track_allowed(legacy, &track), "apps without a separate channel keep main's Nightly choices");
        }
        for track in [VersionTrack::Stable, VersionTrack::Pinned("v1.0.0-beta.1".into())] {
            assert!(ordinary_track_allowed(photo, &track));
        }
    }
}
