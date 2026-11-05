//! Fixed-ID child AppImage integration. The manifest is an ownership ledger, not permission to
//! overwrite a path: both the recorded path and the complete, unchanged contents must agree.

use std::borrow::Cow;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use artcraft_launcher_core::{CraftApp, PackageKind};
use sha2::{Digest, Sha256};

use crate::manifest::{AppImageDesktopOwnership, Record};

/// Resource paths and producer expectations that must be persisted as one ownership result.
#[derive(Default)]
pub(crate) struct Integration {
    pub files: Vec<PathBuf>,
    pub ownership: Option<AppImageDesktopOwnership>,
}

impl Integration {
    /// Replace a record's desktop ledger and expectations without changing its binary metadata.
    pub fn apply(self, record: &mut Record) {
        record.files = self.files;
        record.appimage_desktop = self.ownership;
    }

    /// Preserve installation metadata while returning a record with this integration result.
    pub fn into_record(self, previous: &Record) -> Record {
        Record {
            version: previous.version.clone(),
            kind: previous.kind,
            path: previous.path.clone(),
            installed_at: previous.installed_at,
            files: self.files,
            appimage_desktop: self.ownership,
            desktop_cleanup_pending: previous.desktop_cleanup_pending,
            needs_root: previous.needs_root,
        }
    }

    /// Track a desktop resource together with the producer text expected on disk.
    fn desktop(&mut self, path: PathBuf, text: Cow<'_, str>) {
        self.files.push(path);
        self.ownership.get_or_insert_with(AppImageDesktopOwnership::default).desktop_text = Some(text.into_owned());
    }

    /// Track a native icon together with its producer fingerprint.
    fn icon(&mut self, path: PathBuf, hash: Cow<'_, str>) {
        self.files.push(path);
        self.ownership.get_or_insert_with(AppImageDesktopOwnership::default).icon_sha256 = Some(hash.into_owned());
    }
}

/// Encode the lowercase digest stored in an AppImage icon ownership snapshot.
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Failed manifest persistence must not leave unrecorded native resources behind. Roll back
/// only new acquisitions; existing resources may have been refreshed and keep producer snapshots.
pub(crate) fn rollback_new(data: &Path, app: &CraftApp, previous: &Record, candidate: &Record) -> Integration {
    let mut acquired = candidate.clone();
    acquired.files.retain(|file| !previous.files.contains(file));
    let retained = if acquired.files.is_empty() { Integration::default() } else { remove_owned_from(data, app, Some(&acquired)) };
    let files: Vec<PathBuf> = candidate.files.iter().filter(|file| previous.files.contains(file) || retained.files.contains(file)).cloned().collect();
    let mut ownership = candidate.appimage_desktop.clone();
    if let Some(expected) = &mut ownership {
        if !files.contains(&desktop_path(data, app)) {
            expected.desktop_text = None;
        }
        if !files.contains(&icon_path(data, app)) {
            expected.icon_sha256 = None;
        }
        if expected.desktop_text.is_none() && expected.icon_sha256.is_none() {
            ownership = None;
        }
    }
    Integration { files, ownership }
}

/// Locate the child's fixed bundle-ID desktop entry in the configured data root.
fn desktop_path(data: &Path, app: &CraftApp) -> PathBuf {
    data.join("applications").join(format!("{}.desktop", app.bundle_id()))
}

/// Locate the native icon associated with the same fixed desktop identity.
fn icon_path(data: &Path, app: &CraftApp) -> PathBuf {
    data.join("icons/hicolor/128x128/apps").join(format!("{}.png", app.bundle_id()))
}

/// The explicitly configured data root and its ancestors may legitimately be symlinks. Refuse
/// links strictly below that boundary, including dangling resource links, without canonicalizing
/// the lexical paths recorded in the manifest.
fn safe_path(data: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(data) else { return false };
    data.is_absolute()
        && !relative.as_os_str().is_empty()
        && relative.components().all(|part| matches!(part, Component::Normal(_)))
        && path.ancestors().take_while(|part| *part != data).all(|part| match std::fs::symlink_metadata(part) {
            Ok(meta) => !meta.file_type().is_symlink(),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        })
}

/// Treat a resource as vacant only when it is safely inside the root and genuinely missing.
fn absent(data: &Path, path: &Path) -> bool {
    safe_path(data, path) && std::fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

/// Require a safe regular file whose complete contents match the expected producer bytes.
fn matches_bytes(data: &Path, path: &Path, bytes: &[u8]) -> bool {
    safe_path(data, path)
        && std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() == bytes.len() as u64)
        && std::fs::read(path).is_ok_and(|actual| actual == bytes)
}

/// Limit ownership candidates to AppImage resources already named by the installation ledger.
fn recorded(record: Option<&Record>, path: &Path) -> bool {
    record.is_some_and(|record| record.kind == PackageKind::AppImage && record.files.iter().any(|file| file == path))
}

/// Recover expected desktop text only when its ledger entry and current bytes agree.
/// Legacy records acquire expectations only from an exact known native render.
fn owned_desktop<'a>(data: &Path, path: &Path, app: &CraftApp, record: Option<&'a Record>) -> Option<Cow<'a, str>> {
    let record = record.filter(|record| recorded(Some(record), path))?;
    if let Some(ownership) = &record.appimage_desktop {
        let text = ownership.desktop_text.as_deref()?;
        return matches_bytes(data, path, text.as_bytes()).then_some(Cow::Borrowed(text));
    }
    // Legacy records have no snapshot. Only an exact known render may initialize ownership.
    [false, true].into_iter().find_map(|icon| {
        let text = crate::desktop_entry::render(app, &record.path, icon)?;
        matches_bytes(data, path, text.as_bytes()).then_some(Cow::Owned(text))
    })
}

/// Recover a producer fingerprint only for an unchanged recorded icon, with bounded reads.
/// Legacy ownership is initialized only from the bundled PNG bytes.
fn owned_icon<'a>(data: &Path, path: &Path, app: &CraftApp, record: Option<&'a Record>) -> Option<Cow<'a, str>> {
    let record = record.filter(|record| recorded(Some(record), path))?;
    if let Some(ownership) = &record.appimage_desktop {
        let expected = ownership.icon_sha256.as_deref()?;
        // Bound reads of a replaced local resource as well as refusing links.
        const MAX_ICON: u64 = 4 * 1024 * 1024;
        if !safe_path(data, path) {
            return None;
        }
        let metadata = std::fs::symlink_metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_ICON {
            return None;
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        std::fs::File::open(path).ok()?.take(MAX_ICON + 1).read_to_end(&mut bytes).ok()?;
        return (bytes.len() as u64 <= MAX_ICON && sha256(&bytes) == expected).then_some(Cow::Borrowed(expected));
    }
    let png = artcraft_launcher_core::catalog::app_icon_png(app.id)?;
    matches_bytes(data, path, png).then(|| Cow::Owned(sha256(png)))
}

/// Reconcile one install, returning only resources still proven to belong to the launcher.
/// Unrecognized ledger paths are never visited: an old manifest is not authority over arbitrary
/// icons or another XDG data directory. Customized resources are retained and disowned.
pub(crate) fn integrate_into(data: &Path, app: &CraftApp, appimage: &Path, previous: Option<&Record>, external: bool) -> Integration {
    integrate_into_with(data, app, appimage, previous, external, crate::paths::write_atomic, |path| std::fs::remove_file(path))
}

/// Apply native or external ownership while preserving customized and foreign resources.
/// Injected filesystem operations exercise the same state transitions under deterministic failures.
fn integrate_into_with(
    data: &Path,
    app: &CraftApp,
    appimage: &Path,
    previous: Option<&Record>,
    external: bool,
    write: impl Fn(&Path, &[u8]) -> Result<(), String>,
    remove: impl Fn(&Path) -> std::io::Result<()>,
) -> Integration {
    if external {
        return remove_owned_from_with(data, app, previous, remove);
    }
    let desktop = desktop_path(data, app);
    let icon = icon_path(data, app);
    report_unrecognized(app, previous, &desktop, &icon);
    let desktop_expected = owned_desktop(data, &desktop, app, previous);
    let icon_expected = owned_icon(data, &icon, app, previous);
    let mut result = Integration::default();
    if !absent(data, &desktop) && desktop_expected.is_none() {
        log::info!("{}: retaining custom or external menu entry {}", app.name, desktop.display());
        // A retained entry may depend on an unchanged native icon. Do not rewrite that icon.
        if let Some(hash) = icon_expected {
            result.icon(icon, hash);
        }
        return result;
    }
    // Render before creating an icon, so an invalid executable path cannot orphan a new icon.
    if crate::desktop_entry::render(app, appimage, false).is_none() {
        log::warn!("{}: {} can't be written into a menu entry; retaining existing integration", app.name, appimage.display());
        if let Some(hash) = icon_expected {
            result.icon(icon, hash);
        }
        if let Some(text) = desktop_expected {
            result.desktop(desktop, text);
        }
        return result;
    }
    let has_icon = if icon_expected.is_some() || absent(data, &icon) {
        match artcraft_launcher_core::catalog::app_icon_png(app.id) {
            Some(png) => {
                // Capture producer bytes, never a potentially replaced file after writing it.
                let hash = sha256(png);
                if icon_expected.as_deref() == Some(hash.as_str()) {
                    result.icon(icon, Cow::Owned(hash));
                    true
                } else {
                    match write(&icon, png) {
                        Ok(()) => {
                            result.icon(icon, Cow::Owned(hash));
                            true
                        }
                        Err(error) => {
                            log::warn!("{}: couldn't install the menu icon: {error}", app.name);
                            if let Some(hash) = icon_expected {
                                result.icon(icon, hash);
                                true
                            } else {
                                false
                            }
                        }
                    }
                }
            }
            None => {
                if let Some(hash) = icon_expected {
                    result.icon(icon, hash);
                    true
                } else {
                    false
                }
            }
        }
    } else {
        log::info!("{}: retaining custom or external menu icon {}", app.name, icon.display());
        safe_path(data, &icon) && std::fs::symlink_metadata(&icon).is_ok_and(|meta| meta.is_file())
    };
    if let Some(text) = crate::desktop_entry::render(app, appimage, has_icon) {
        if matches_bytes(data, &desktop, text.as_bytes()) {
            result.desktop(desktop, Cow::Owned(text));
        } else {
            match write(&desktop, text.as_bytes()) {
                Ok(()) => result.desktop(desktop, Cow::Owned(text)),
                Err(error) => {
                    log::warn!("{}: couldn't add the menu entry: {error}", app.name);
                    if let Some(text) = desktop_expected {
                        result.desktop(desktop, text);
                    }
                }
            }
        }
    }
    result
}

/// Report stale ledger paths without treating them as authority over unrelated resources.
fn report_unrecognized(app: &CraftApp, record: Option<&Record>, desktop: &Path, icon: &Path) {
    if let Some(record) = record {
        for path in &record.files {
            if path != desktop && path != icon {
                log::info!("{}: retaining unrecognized previously recorded resource {}", app.name, path.display());
            }
        }
    }
}

/// Enabling external ownership and uninstall share exactly the same conservative cleanup.
/// Failed removals remain in the ledger. An unchanged icon is retained while any custom,
/// foreign, unreadable or symlinked desktop entry remains, since that entry may use the icon.
pub(crate) fn remove_owned_from(data: &Path, app: &CraftApp, record: Option<&Record>) -> Integration {
    remove_owned_from_with(data, app, record, |path| std::fs::remove_file(path))
}

/// Remove proven native resources while retaining expectations for failed removals.
/// Keep an owned icon while a preserved desktop entry may still depend on it.
fn remove_owned_from_with(data: &Path, app: &CraftApp, record: Option<&Record>, remove: impl Fn(&Path) -> std::io::Result<()>) -> Integration {
    let desktop = desktop_path(data, app);
    let icon = icon_path(data, app);
    report_unrecognized(app, record, &desktop, &icon);
    let mut retained = Integration::default();
    if let Some(text) = owned_desktop(data, &desktop, app, record) {
        if let Err(error) = remove(&desktop) {
            log::warn!("{}: couldn't remove owned menu entry {}: {error}", app.name, desktop.display());
            retained.desktop(desktop.clone(), text);
        }
    } else if !absent(data, &desktop) {
        log::info!("{}: retaining custom or external menu entry {}", app.name, desktop.display());
    }
    if let Some(hash) = owned_icon(data, &icon, app, record) {
        if absent(data, &desktop) {
            if let Err(error) = remove(&icon) {
                log::warn!("{}: couldn't remove owned menu icon {}: {error}", app.name, icon.display());
                retained.icon(icon, hash);
            }
        } else {
            log::info!("{}: retaining menu icon {} for the retained entry", app.name, icon.display());
            retained.icon(icon, hash);
        }
    } else if !absent(data, &icon) {
        log::info!("{}: retaining custom or external menu icon {}", app.name, icon.display());
    }
    retained
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Use a catalog-backed app so resource identities and bundled icon bytes match production.
    fn app() -> &'static CraftApp {
        CraftApp::by_id("wordcraft").unwrap()
    }

    /// Pair an AppImage path with the ownership result that an installation would persist.
    fn record(path: &Path, integration: Integration) -> Record {
        Record {
            version: "0.3.0".into(),
            kind: PackageKind::AppImage,
            path: path.into(),
            installed_at: 0,
            files: integration.files,
            appimage_desktop: integration.ownership,
            desktop_cleanup_pending: false,
            needs_root: false,
        }
    }

    /// Native upgrades, external handoff, restoration, and uninstall preserve the resource ledger.
    #[test]
    fn native_install_upgrade_handoff_restore_and_removal() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("share");
        let apps = root.path().join(r#"My Apps 100% \" $HOME"#);
        std::fs::create_dir(&apps).unwrap();
        let download = root.path().join("download");
        std::fs::write(&download, b"first").unwrap();
        let (old, _) = super::super::linux::install_appimage(&download, "wordcraft-0.2.0.AppImage", Some(&apps), None, false).unwrap();
        let files = integrate_into(&data, app(), &old, None, false);
        let desktop = desktop_path(&data, app());
        let icon = icon_path(&data, app());
        assert_eq!(files.files, [icon.clone(), desktop.clone()]);
        assert_eq!(std::fs::read(&icon).unwrap(), artcraft_launcher_core::catalog::app_icon_png(app().id).unwrap());
        let previous = record(&old, files);
        std::fs::write(&download, b"second").unwrap();
        let (new, removed) =
            super::super::linux::install_appimage(&download, "wordcraft-0.3.0.AppImage", Some(&apps), Some(&previous), false).unwrap();
        assert_eq!(removed, std::slice::from_ref(&old));
        assert!(!old.exists());
        let files = integrate_into(&data, app(), &new, Some(&previous), false);
        let mut current = record(&new, files);
        let text = std::fs::read_to_string(&desktop).unwrap();
        assert_eq!(text, crate::desktop_entry::render(app(), &new, true).unwrap());
        integrate_into(&data, app(), &new, Some(&current), true).apply(&mut current);
        assert!(current.files.is_empty());
        assert!(!desktop.exists() && !icon.exists());
        assert_eq!(std::fs::read(&new).unwrap(), b"second");
        integrate_into(&data, app(), &new, Some(&current), false).apply(&mut current);
        assert_eq!(current.files, [icon.clone(), desktop.clone()]);
        super::super::linux::unintegrate_from(&data, app(), &new, Some(&current));
        assert!(!desktop.exists() && !icon.exists());
    }

    /// Configured root and HOME links keep lexical ownership paths usable across the lifecycle.
    #[test]
    fn configured_root_and_home_symlinks_preserve_native_lifecycle_and_lexical_ownership() {
        use std::os::unix::fs::symlink;

        for root_is_link in [false, true] {
            for legacy in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let physical_home = root.path().join("physical-home");
                let physical_data = physical_home.join("share");
                std::fs::create_dir_all(&physical_data).unwrap();
                let link = root.path().join(if root_is_link { "configured-data" } else { "configured-home" });
                let target = if root_is_link { &physical_data } else { &physical_home };
                symlink(target, &link).unwrap();
                let data = if root_is_link { link.clone() } else { link.join("share") };
                let desktop = desktop_path(&data, app());
                let icon = icon_path(&data, app());
                let physical_desktop = desktop_path(&physical_data, app());
                let physical_icon = icon_path(&physical_data, app());
                let apps = root.path().join("Apps");
                std::fs::create_dir(&apps).unwrap();
                let download = root.path().join("download");
                std::fs::write(&download, b"first AppImage").unwrap();
                let (old, _) = super::super::linux::install_appimage(&download, "wordcraft-0.2.0.AppImage", Some(&apps), None, false).unwrap();
                let mut current = record(&old, integrate_into(&data, app(), &old, None, false));
                assert_eq!(current.files, [icon.clone(), desktop.clone()]);
                assert_eq!(std::fs::read_to_string(&physical_desktop).unwrap(), crate::desktop_entry::render(app(), &old, true).unwrap());
                assert_eq!(std::fs::read(&physical_icon).unwrap(), artcraft_launcher_core::catalog::app_icon_png(app().id).unwrap());
                if legacy {
                    current.appimage_desktop = None;
                }
                current = persist(root.path(), current);
                assert_eq!(current.files, [icon.clone(), desktop.clone()]);

                std::fs::write(&download, b"second AppImage").unwrap();
                let (new, removed) =
                    super::super::linux::install_appimage(&download, "wordcraft-0.3.0.AppImage", Some(&apps), Some(&current), false).unwrap();
                assert_eq!(removed, std::slice::from_ref(&old));
                current = record(&new, integrate_into(&data, app(), &new, Some(&current), false));
                current = persist(root.path(), current);
                assert_eq!(current.files, [icon.clone(), desktop.clone()]);
                assert_eq!(std::fs::read_to_string(&physical_desktop).unwrap(), crate::desktop_entry::render(app(), &new, true).unwrap());
                assert!(!old.exists());

                integrate_into(&data, app(), &new, Some(&current), true).apply(&mut current);
                current = persist(root.path(), current);
                assert!(current.files.is_empty() && current.appimage_desktop.is_none());
                assert!(!physical_desktop.exists() && !physical_icon.exists());
                assert_eq!(std::fs::read(&new).unwrap(), b"second AppImage");

                integrate_into(&data, app(), &new, Some(&current), false).apply(&mut current);
                current = persist(root.path(), current);
                assert_eq!(current.files, [icon, desktop]);
                assert_eq!(std::fs::read_to_string(&physical_desktop).unwrap(), crate::desktop_entry::render(app(), &new, true).unwrap());
                assert!(physical_icon.is_file());
                std::fs::remove_file(&new).unwrap();
                super::super::linux::unintegrate_from(&data, app(), &new, Some(&current));
                assert!(!physical_desktop.exists() && !physical_icon.exists());
                assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
                assert_eq!(std::fs::read_link(&link).unwrap(), *target);
                assert!(physical_data.is_dir());
            }
        }
    }

    /// Matching executables and stale ledger paths never authorize adopting external resources.
    #[test]
    fn foreign_same_target_metadata_and_stale_icon_ledgers_survive() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("share");
        let image = root.path().join("wordcraft.AppImage");
        let desktop = desktop_path(&data, app());
        let icon = icon_path(&data, app());
        let files = integrate_into(&data, app(), &image, None, false);
        let mut previous = record(&image, files);
        let foreign = format!(
            "[Desktop Entry]\nName=My WordCraft\nExec=\"{}\" %F\nMimeType=text/plain;\nX-AppImage-Version=0.3.0\nX-AppImage-UpdateInformation=keep\n",
            image.display()
        );
        std::fs::write(&desktop, &foreign).unwrap();
        std::fs::write(&icon, b"external icon").unwrap();
        let other_icon = root.path().join("external-manager-icon.png");
        std::fs::write(&other_icon, b"external recorded icon").unwrap();
        previous.files.push(other_icon.clone());
        for external in [true, false] {
            assert!(integrate_into(&data, app(), &image, Some(&previous), external).files.is_empty());
            assert_eq!(std::fs::read_to_string(&desktop).unwrap(), foreign);
            assert_eq!(std::fs::read(&icon).unwrap(), b"external icon");
        }
        super::super::linux::unintegrate_from(&data, app(), &image, Some(&previous));
        assert_eq!(std::fs::read_to_string(&desktop).unwrap(), foreign);
        assert_eq!(std::fs::read(&icon).unwrap(), b"external icon");
        assert_eq!(std::fs::read(&other_icon).unwrap(), b"external recorded icon");
    }

    /// Handoff and uninstall preserve a customized entry and the unchanged icon it still uses.
    #[test]
    fn a_customized_marked_entry_keeps_its_icon_without_being_reclaimed() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("share");
        let image = root.path().join("wordcraft.AppImage");
        let desktop = desktop_path(&data, app());
        let icon = icon_path(&data, app());
        let mut previous = record(&image, integrate_into(&data, app(), &image, None, false));
        let custom = format!("{}Actions=Personal;\n", std::fs::read_to_string(&desktop).unwrap());
        std::fs::write(&desktop, &custom).unwrap();
        integrate_into(&data, app(), &image, Some(&previous), true).apply(&mut previous);
        assert_eq!(previous.files, std::slice::from_ref(&icon));
        assert_eq!(std::fs::read_to_string(&desktop).unwrap(), custom);
        assert!(icon.is_file());
        integrate_into(&data, app(), &image, Some(&previous), false).apply(&mut previous);
        assert_eq!(previous.files, std::slice::from_ref(&icon));
        let retained = super::super::linux::unintegrate_from(&data, app(), &image, Some(&previous));
        assert_eq!(retained.files, std::slice::from_ref(&icon));
        assert_eq!(retained.ownership, previous.appimage_desktop);
        assert_eq!(std::fs::read_to_string(&desktop).unwrap(), custom);
        assert!(icon.is_file());
        // After handoff, even an exact marked render is not reclaimed without its ledger entry.
        let external = crate::desktop_entry::render(app(), &image, true).unwrap();
        std::fs::write(&desktop, &external).unwrap();
        super::super::linux::unintegrate_from(&data, app(), &image, Some(&previous));
        assert_eq!(std::fs::read_to_string(&desktop).unwrap(), external);
        assert!(icon.is_file());
    }

    /// A marker or matching bytes cannot establish ownership without a recorded resource path.
    #[test]
    fn marker_or_matching_bytes_without_a_record_do_not_establish_ownership() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("share");
        let image = root.path().join("wordcraft.AppImage");
        let desktop = desktop_path(&data, app());
        let icon = icon_path(&data, app());
        integrate_into(&data, app(), &image, None, false);
        let unowned = record(&image, Integration::default());
        for external in [true, false] {
            assert!(integrate_into(&data, app(), &image, Some(&unowned), external).files.is_empty());
            assert!(desktop.is_file() && icon.is_file());
        }
        super::super::linux::unintegrate_from(&data, app(), &image, None);
        assert!(desktop.is_file() && icon.is_file());
    }

    /// Resource and internal-directory links cannot redirect writes or removals outside the root.
    #[test]
    fn links_in_resources_or_their_parents_are_never_followed_or_replaced() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("share");
        let image = root.path().join("wordcraft.AppImage");
        let desktop = desktop_path(&data, app());
        let icon = icon_path(&data, app());
        let previous = record(&image, integrate_into(&data, app(), &image, None, false));
        let desktop_target = root.path().join("external.desktop");
        let icon_target = root.path().join("external.png");
        std::fs::rename(&desktop, &desktop_target).unwrap();
        std::fs::rename(&icon, &icon_target).unwrap();
        symlink(&desktop_target, &desktop).unwrap();
        symlink(&icon_target, &icon).unwrap();
        for external in [true, false] {
            assert!(integrate_into(&data, app(), &image, Some(&previous), external).files.is_empty());
        }
        super::super::linux::unintegrate_from(&data, app(), &image, Some(&previous));
        assert!(std::fs::symlink_metadata(&desktop).unwrap().file_type().is_symlink());
        assert!(std::fs::symlink_metadata(&icon).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&desktop_target).unwrap(), crate::desktop_entry::render(app(), &image, true).unwrap());
        assert_eq!(std::fs::read(&icon_target).unwrap(), artcraft_launcher_core::catalog::app_icon_png(app().id).unwrap());

        std::fs::remove_file(&desktop).unwrap();
        symlink(root.path().join("missing.desktop"), &desktop).unwrap();
        assert!(integrate_into(&data, app(), &image, Some(&previous), false).files.is_empty());
        assert!(std::fs::symlink_metadata(&desktop).unwrap().file_type().is_symlink());

        let linked_data = root.path().join("linked-share");
        symlink(&data, &linked_data).unwrap();
        assert!(integrate_into(&linked_data, app(), &image, Some(&previous), false).files.is_empty());
        let parent_data = root.path().join("parent-share");
        std::fs::create_dir(&parent_data).unwrap();
        symlink(data.join("applications"), parent_data.join("applications")).unwrap();
        assert!(integrate_into(&parent_data, app(), &image, None, false).files.is_empty());
        assert!(!parent_data.join("icons").exists());

        let icon_parent_data = root.path().join("icon-parent-share");
        std::fs::create_dir(&icon_parent_data).unwrap();
        symlink(data.join("icons"), icon_parent_data.join("icons")).unwrap();
        let own_desktop = desktop_path(&icon_parent_data, app());
        let files = integrate_into(&icon_parent_data, app(), &image, None, false);
        assert_eq!(files.files, std::slice::from_ref(&own_desktop));
        let text = std::fs::read_to_string(&own_desktop).unwrap();
        assert!(!text.contains("Icon="));
        super::super::linux::unintegrate_from(&icon_parent_data, app(), &image, Some(&record(&image, files)));
        assert!(!own_desktop.exists());
        assert!(std::fs::symlink_metadata(icon_parent_data.join("icons")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&icon_target).unwrap(), artcraft_launcher_core::catalog::app_icon_png(app().id).unwrap());
    }

    /// An unrenderable executable path cannot leave an orphaned icon or desktop entry.
    #[test]
    fn invalid_exec_paths_do_not_leave_orphaned_icons() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("share");
        for image in [root.path().join("line\nbreak.AppImage"), root.path().join("a=b.AppImage")] {
            assert!(integrate_into(&data, app(), &image, None, false).files.is_empty());
            assert!(!data.exists());
        }
    }

    /// External-mode installs and changed-path updates leave desktop and MIME resources intact.
    #[test]
    fn external_fresh_install_and_changed_path_upgrade_do_not_write_desktop_resources() {
        let root = tempfile::tempdir().unwrap();
        let apps = root.path().join("Apps");
        std::fs::create_dir(&apps).unwrap();
        let download = root.path().join("download");
        std::fs::write(&download, b"first external AppImage").unwrap();
        let data = root.path().join("share");
        let (old, _) = super::super::linux::install_appimage(&download, "wordcraft-0.2.0.AppImage", Some(&apps), None, false).unwrap();
        let mut previous = record(&old, integrate_into(&data, app(), &old, None, true));
        assert!(previous.files.is_empty());
        assert!(!data.exists(), "a fresh external install must not create even a native icon");
        let desktop = desktop_path(&data, app());
        let icon = icon_path(&data, app());
        let mime = data.join("mime/packages/manager-wordcraft.xml");
        let entry = format!("[Desktop Entry]\nExec=\"{}\" %F\nMimeType=text/plain;\nX-AppImage-UpdateInformation=preserve\n", old.display());
        crate::paths::write_atomic(&desktop, entry.as_bytes()).unwrap();
        crate::paths::write_atomic(&icon, b"external icon").unwrap();
        crate::paths::write_atomic(&mime, b"external MIME registration").unwrap();
        std::fs::write(&download, b"second external AppImage").unwrap();
        let (new, removed) =
            super::super::linux::install_appimage(&download, "wordcraft-0.3.0.AppImage", Some(&apps), Some(&previous), false).unwrap();
        assert_eq!(removed, std::slice::from_ref(&old));
        previous = record(&new, integrate_into(&data, app(), &new, Some(&previous), true));
        assert!(previous.files.is_empty());
        assert!(!old.exists());
        assert_eq!(std::fs::read(&new).unwrap(), b"second external AppImage");
        super::super::linux::unintegrate_from(&data, app(), &new, Some(&previous));
        assert_eq!(std::fs::read_to_string(&desktop).unwrap(), entry);
        assert_eq!(std::fs::read(&icon).unwrap(), b"external icon");
        assert_eq!(std::fs::read(&mime).unwrap(), b"external MIME registration");
    }

    /// A replaced icon stays external through handoff, native restoration, and uninstall.
    #[test]
    fn an_externally_replaced_icon_survives_handoff_restore_and_uninstall() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("share");
        let image = root.path().join("wordcraft.AppImage");
        let desktop = desktop_path(&data, app());
        let icon = icon_path(&data, app());
        let mut previous = record(&image, integrate_into(&data, app(), &image, None, false));
        std::fs::write(&icon, b"personal icon").unwrap();
        integrate_into(&data, app(), &image, Some(&previous), true).apply(&mut previous);
        assert!(previous.files.is_empty());
        assert!(!desktop.exists());
        assert_eq!(std::fs::read(&icon).unwrap(), b"personal icon");
        integrate_into(&data, app(), &image, Some(&previous), false).apply(&mut previous);
        assert_eq!(previous.files, std::slice::from_ref(&desktop));
        assert!(std::fs::read_to_string(&desktop).unwrap().contains("Icon=ai.storyteller.wordcraft\n"));
        super::super::linux::unintegrate_from(&data, app(), &image, Some(&previous));
        assert!(!desktop.exists());
        assert_eq!(std::fs::read(&icon).unwrap(), b"personal icon");
    }

    /// Stage a changed-path upgrade with native resources for deterministic failure scenarios.
    fn upgrade_fixture(root: &Path) -> (PathBuf, Record, PathBuf) {
        let apps = root.join("Apps");
        let data = root.join("share");
        let download = root.join("download");
        std::fs::create_dir(&apps).unwrap();
        std::fs::write(&download, b"old AppImage").unwrap();
        let (old, _) = super::super::linux::install_appimage(&download, "wordcraft-0.2.0.AppImage", Some(&apps), None, false).unwrap();
        let mut previous = record(&old, integrate_into(&data, app(), &old, None, false));
        previous.version = "0.2.0".into();
        std::fs::write(&download, b"new AppImage").unwrap();
        let (new, _) = super::super::linux::install_appimage(&download, "wordcraft-0.3.0.AppImage", Some(&apps), Some(&previous), false).unwrap();
        (data, previous, new)
    }

    /// Exercise a real atomic-rename failure without relying on the effective user's permissions.
    fn fail_desktop_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
        if path.extension().is_none_or(|extension| extension != "desktop") {
            return crate::paths::write_atomic(path, bytes);
        }
        let saved = path.with_extension("saved-desktop");
        std::fs::rename(path, &saved).unwrap();
        std::fs::create_dir(path).unwrap();
        let result = crate::paths::write_atomic(path, bytes);
        std::fs::remove_dir(path).unwrap();
        std::fs::rename(saved, path).unwrap();
        assert!(result.is_err());
        result
    }

    /// Force an unlink error with a directory, then restore the original resource for recovery.
    fn fail_removal(path: &Path) -> std::io::Result<()> {
        let saved = path.with_extension("saved-resource");
        std::fs::rename(path, &saved).unwrap();
        std::fs::create_dir(path).unwrap();
        let result = std::fs::remove_file(path);
        std::fs::remove_dir(path).unwrap();
        std::fs::rename(saved, path).unwrap();
        assert!(result.is_err());
        result
    }

    /// Round-trip ownership through the JSON manifest to exercise recovery after a reload.
    fn persist(root: &Path, current: Record) -> Record {
        let path = root.join("installed.json");
        let mut manifest = crate::manifest::Manifest::default();
        manifest.apps.insert(app().id.into(), current);
        manifest.save(&path).unwrap();
        crate::manifest::Manifest::load(&path).apps.remove(app().id).unwrap()
    }

    /// Failed rewrites retain the old expected text until a later update or handoff succeeds.
    #[test]
    fn failed_upgrade_desktop_rewrite_keeps_the_old_expectation_until_retry_or_handoff() {
        for legacy in [false, true] {
            for handoff_first in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let (data, mut previous, new) = upgrade_fixture(root.path());
                let desktop = desktop_path(&data, app());
                let icon = icon_path(&data, app());
                let old_text = std::fs::read_to_string(&desktop).unwrap();
                if legacy {
                    previous.appimage_desktop = None;
                }
                let integration =
                    integrate_into_with(&data, app(), &new, Some(&previous), false, fail_desktop_write, |path| std::fs::remove_file(path));
                let mut current = persist(root.path(), record(&new, integration));
                assert_eq!(current.path, new);
                assert_eq!(current.appimage_desktop.as_ref().unwrap().desktop_text.as_deref(), Some(old_text.as_str()));
                assert_eq!(std::fs::read_to_string(&desktop).unwrap(), old_text);
                if !handoff_first {
                    integrate_into(&data, app(), &new, Some(&current), false).apply(&mut current);
                    current = persist(root.path(), current);
                    let expected = crate::desktop_entry::render(app(), &new, true).unwrap();
                    assert_eq!(std::fs::read_to_string(&desktop).unwrap(), expected);
                    assert_eq!(current.appimage_desktop.as_ref().unwrap().desktop_text.as_deref(), Some(expected.as_str()));
                }
                integrate_into(&data, app(), &new, Some(&current), true).apply(&mut current);
                assert!(current.files.is_empty());
                assert!(current.appimage_desktop.is_none());
                assert!(!desktop.exists() && !icon.exists());
                assert_eq!(std::fs::read(&new).unwrap(), b"new AppImage");
            }
        }
    }

    /// Failed upgrade cleanup keeps old entries recognizable after the installed path changes.
    #[test]
    fn failed_external_cleanup_during_upgrade_keeps_removable_old_expectations() {
        for legacy in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (data, mut previous, new) = upgrade_fixture(root.path());
            let desktop = desktop_path(&data, app());
            let icon = icon_path(&data, app());
            let old_text = std::fs::read_to_string(&desktop).unwrap();
            if legacy {
                previous.appimage_desktop = None;
            }
            let integration = integrate_into_with(&data, app(), &new, Some(&previous), true, crate::paths::write_atomic, fail_removal);
            let current = persist(root.path(), record(&new, integration));
            assert_eq!(current.files.len(), 2);
            assert_eq!(current.appimage_desktop.as_ref().unwrap().desktop_text.as_deref(), Some(old_text.as_str()));
            assert_eq!(std::fs::read_to_string(&desktop).unwrap(), old_text);
            super::super::linux::unintegrate_from(&data, app(), &new, Some(&current));
            assert!(!desktop.exists() && !icon.exists());
            assert_eq!(std::fs::read(&new).unwrap(), b"new AppImage");
        }
    }

    /// Historical producer snapshots, not current catalog metadata, determine resource ownership.
    #[test]
    fn saved_expectations_survive_native_catalog_text_and_icon_changes() {
        for external in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let data = root.path().join("share");
            let image = root.path().join("wordcraft.AppImage");
            let desktop = desktop_path(&data, app());
            let icon = icon_path(&data, app());
            let text = crate::desktop_entry::render(app(), &image, true).unwrap().replace(app().tagline, "An earlier native catalog description");
            let mut png = artcraft_launcher_core::catalog::app_icon_png(app().id).unwrap().to_vec();
            png.extend_from_slice(b"earlier native PNG metadata");
            let expected = AppImageDesktopOwnership { desktop_text: Some(text.clone()), icon_sha256: Some(sha256(&png)) };
            crate::paths::write_atomic(&desktop, text.as_bytes()).unwrap();
            crate::paths::write_atomic(&icon, &png).unwrap();
            let mut current =
                persist(root.path(), record(&image, Integration { files: vec![icon.clone(), desktop.clone()], ownership: Some(expected) }));
            integrate_into(&data, app(), &image, Some(&current), external).apply(&mut current);
            if !external {
                assert_eq!(std::fs::read_to_string(&desktop).unwrap(), crate::desktop_entry::render(app(), &image, true).unwrap());
                assert_eq!(std::fs::read(&icon).unwrap(), artcraft_launcher_core::catalog::app_icon_png(app().id).unwrap());
                current = persist(root.path(), current);
                integrate_into(&data, app(), &image, Some(&current), true).apply(&mut current);
            }
            assert!(current.files.is_empty());
            assert!(!desktop.exists() && !icon.exists());
        }
    }

    /// Post-write external replacements are never adopted as the producer's expected contents.
    #[test]
    fn resources_replaced_after_a_write_are_not_adopted_as_native_expectations() {
        for replace_icon in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let data = root.path().join("share");
            let image = root.path().join("wordcraft.AppImage");
            let desktop = desktop_path(&data, app());
            let icon = icon_path(&data, app());
            let integration = integrate_into_with(
                &data,
                app(),
                &image,
                None,
                false,
                |path, bytes| {
                    crate::paths::write_atomic(path, bytes)?;
                    if (path == icon.as_path() && replace_icon) || (path == desktop.as_path() && !replace_icon) {
                        crate::paths::write_atomic(path, b"external manager replacement")?;
                    }
                    Ok(())
                },
                |path| std::fs::remove_file(path),
            );
            let mut current = persist(root.path(), record(&image, integration));
            let expected = current.appimage_desktop.as_ref().unwrap();
            assert_eq!(expected.desktop_text, crate::desktop_entry::render(app(), &image, true));
            let native_hash = sha256(artcraft_launcher_core::catalog::app_icon_png(app().id).unwrap());
            assert_eq!(expected.icon_sha256.as_deref(), Some(native_hash.as_str()));
            integrate_into(&data, app(), &image, Some(&current), true).apply(&mut current);
            let replaced = if replace_icon { &icon } else { &desktop };
            assert_eq!(std::fs::read(replaced).unwrap(), b"external manager replacement");
        }
    }

    /// A cleanup-only record cannot authorize replacing or removing another AppImage at its old path.
    #[test]
    fn pending_cleanup_does_not_claim_a_reoccupied_binary_path() {
        let root = tempfile::tempdir().unwrap();
        let apps = root.path().join("Apps");
        let data = root.path().join("share");
        std::fs::create_dir(&apps).unwrap();
        let image = apps.join("wordcraft-0.3.0.AppImage");
        std::fs::write(&image, b"formerly installed AppImage").unwrap();
        let mut previous = record(&image, integrate_into(&data, app(), &image, None, false));
        previous.desktop_cleanup_pending = true;
        std::fs::write(&image, b"unrelated replacement").unwrap();
        let locations = artcraft_launcher_core::protocol::InstallLocations { root: Some(apps.display().to_string()), ..Default::default() };
        let asset = artcraft_launcher_core::release::Asset { name: "wordcraft-0.3.0.AppImage".into(), ..Default::default() };
        assert_eq!(
            super::super::occupied(app(), &asset, PackageKind::AppImage, None, Some(&previous), &locations, &crate::paths::Paths::under(root.path()))
                .unwrap(),
            Some(image.clone())
        );
        let download = root.path().join("download");
        std::fs::write(&download, b"new downloaded AppImage").unwrap();
        assert!(super::super::linux::install_appimage(&download, &asset.name, Some(&apps), Some(&previous), false).is_err());
        assert!(super::super::remove_appimage(app(), &image, Some(&previous)).is_err());
        assert_eq!(std::fs::read(&image).unwrap(), b"unrelated replacement");
        let (new, removed) =
            super::super::linux::install_appimage(&download, "wordcraft-0.4.0.AppImage", Some(&apps), Some(&previous), false).unwrap();
        assert!(removed.is_empty());
        assert_eq!(std::fs::read(&new).unwrap(), b"new downloaded AppImage");
        assert_eq!(std::fs::read(&image).unwrap(), b"unrelated replacement");
    }
}
