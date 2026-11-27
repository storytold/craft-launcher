//! The launcher's own Windows installer: the fast install.
//!
//! A fast install is what a Windows Installer package gives a person — a program folder, a Start
//! menu entry, a listing in Apps & features the system can uninstall, and an App Paths
//! registration so Win+R finds the exe — without what makes that package slow:
//!
//! - No `msiexec` run. The Windows Installer service, its registration journal and its wizard UI
//!   are replaced by one bounded file transaction in this process (the same hardened transaction
//!   a portable install uses: every path checked, nothing unrecorded touched, a failure puts the
//!   previous files back).
//! - No elevation. Everything a fast install writes belongs to the account: the program folder
//!   under `%LOCALAPPDATA%\Programs\ArtCraft`, the Start menu entry, and the Apps & features
//!   record under `HKCU`. A first install therefore asks no permission prompt, where the
//!   launcher's MSI route installs per-machine into Program Files and asks for an administrator.
//! - No second download. The payload is the release's portable ZIP, the smallest published
//!   package of the app, so nothing but the app's own files crosses the network.
//!
//! The price of that speed is honesty about what is not done: there is no Windows Installer
//! registration, so the copy updates and uninstalls through this launcher alone. The Apps &
//! features entry says as much by pointing its Uninstall command at the launcher
//! ([`uninstall_app`]), which removes the recorded files, the Start menu entry and both registry
//! keys without a window.

use std::path::{Path, PathBuf};
use std::sync::PoisonError;
use std::sync::atomic::{AtomicU32, Ordering};

use artcraft_launcher_core::{CraftApp, InstallSource, PackageKind};

use crate::manifest::Record;
use crate::paths::Paths;

/// Where the suite's per-user programs live: `%LOCALAPPDATA%\Programs\ArtCraft`, the folder the
/// MSI packages' own per-user route uses, so a fast-installed app sits beside its installed
/// siblings. The app's own folder inside it is named by its catalog id, the same spelling the
/// launcher's records and portable installs use.
pub fn programs_folder() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("Programs").join("ArtCraft"))
}

/// The per-user Start menu's Programs folder: `%APPDATA%\Microsoft\Windows\Start Menu\Programs`,
/// where the suite's MSI packages put their shortcuts.
fn start_menu_programs() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"))
}

/// The Start menu shortcut of `app`: `<Programs>\<App>.lnk`, the name its MSI package also uses.
fn shortcut_path(menu: &Path, app: &CraftApp) -> PathBuf {
    menu.join(format!("{}.lnk", app.name))
}

/// The Apps & features key a fast install of `app` records itself under. Not a product-code GUID,
/// so Windows Installer detection never mistakes it for an MSI registration.
fn uninstall_key(id: &str) -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\ArtCraftLauncher.{id}")
}

/// The App Paths key that lets Win+R and the Run dialog find the exe by name.
fn app_paths_key(id: &str) -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{id}.exe")
}

/// One app the launcher fast-installed, as Apps & features shows it.
pub(crate) struct Package<'a> {
    pub app: &'a CraftApp,
    pub file: &'a Path,
    pub version: &'a str,
    pub previous: Option<&'a Record>,
    pub paths: &'a Paths,
    /// The parent folder for a fresh install; `None` takes the default Programs folder. An update
    /// ignores it and keeps the recorded folder.
    pub directory: Option<&'a Path>,
    pub replace: bool,
}

/// Unpacks `package` and integrates it with Windows, as one transaction.
///
/// The file transaction is the portable install's own ([`crate::portable`]): the ZIP is unpacked
/// into a staging folder, checked, and copied over the recorded files, which are backed up first
/// and put back if anything fails. The Windows integration — Start menu entry, Apps & features
/// record, App Paths — happens inside the transaction's commit, so a registry or shortcut that
/// cannot be written rolls the files back rather than leaving an integrated half-copy; a commit
/// the caller refuses (the version check, a manifest that won't save) takes the integration back
/// with it. A failed update puts the previous copy's integration back instead.
pub(crate) fn install(package: Package<'_>, commit: &mut dyn FnMut(&Record) -> Result<(), String>) -> Result<Record, String> {
    let Package { app, file, version, previous, paths, directory, replace } = package;
    // A fresh install lands in the per-user Programs folder unless another parent was chosen; an
    // update keeps the recorded folder, which the transaction reads from `previous` itself.
    let directory = match previous {
        Some(_) => None,
        None => Some(directory.map_or_else(default_parent, |parent| Ok(parent.to_path_buf()))?),
    };
    let mut written = Integrated::default();
    let mut commit_with_integration = |record: &Record| {
        let menu = start_menu_programs().ok_or("couldn't find the Start menu's Programs folder")?;
        let (integration, failure) = integrate(app, record, &menu, installed_kb(record));
        written = integration;
        match failure {
            Some(error) => Err(error),
            None => commit(record),
        }
    };
    let result = crate::portable::install_checked(
        crate::portable::Package { app, file, version, kind: PackageKind::FastInstall, previous, paths, directory: directory.as_deref(), replace },
        &|_, _| Ok(()),
        &mut commit_with_integration,
    );
    if result.is_ok() {
        // The install stands: the saved copy of the Start menu entry it replaced is of no use now.
        drop_shortcut_backup(&written.shortcut);
    } else {
        // The transaction is putting the previous files back, or removing a fresh copy's; what
        // Windows was told must describe what is on disk again. A failed update gets the previous
        // copy's listing back and the Start menu entry it had before the install — the copy the
        // install saved of the entry it replaced, which is the previous version's own or one the
        // person (or an MSI package) had put there, never a new one in its place — and a failed
        // first install gets everything the install managed to write taken back.
        match previous {
            Some(previous) => {
                if written.registration
                    && let Err(error) = write_registration(app, previous, installed_kb(previous))
                {
                    log::warn!("{}: couldn't restore the previous version's Apps & features record: {error}", app.name);
                }
                if let Some(menu) = start_menu_programs()
                    && let Some(shortcut) = written.shortcut.take()
                    && let Err(error) = undo_shortcut(&menu, app, shortcut)
                {
                    log::warn!("{}: {error}", app.name);
                }
            }
            None => take_back(app, written),
        }
    }
    result
}

/// The parent folder a fresh fast install with no chosen folder goes into: the per-user Programs
/// folder every such install belongs in, or an honest refusal when the account has none.
fn default_parent() -> Result<PathBuf, String> {
    programs_folder().ok_or_else(|| "couldn't find the per-user Programs folder".to_string())
}

/// Removes a fast install: the recorded files, then the Start menu entry and both registry keys.
/// Personal settings and documents remain, as with every removal the launcher does.
pub(crate) fn remove(app: &CraftApp, record: &Record) -> Result<(), String> {
    crate::portable::remove(app, record)?;
    remove_integration(app);
    Ok(())
}

/// What the launcher told Windows about an install, so a failed transaction can take back exactly
/// that and nothing the previous version (or the person) had put there.
#[derive(Default)]
struct Integrated {
    shortcut: Option<Shortcut>,
    registration: bool,
}

/// What writing the Start menu entry did to the entry that was in its place, so a rollback
/// undoes exactly that and nothing the person (or an MSI package) had put there.
#[derive(Debug)]
enum Shortcut {
    /// The entry's place was empty: the install created the entry.
    Created,
    /// A shortcut file was in the entry's place and is saved at the path, to be put back when
    /// the install is taken back and dropped once the install stands.
    Replaced(PathBuf),
}

/// Writes the Start menu entry and the two registry keys, stopping at the first failure and
/// reporting both what got written and what went wrong, so the caller can undo the former.
fn integrate(app: &CraftApp, record: &Record, menu: &Path, size_kb: u32) -> (Integrated, Option<String>) {
    let mut written = Integrated::default();
    match write_shortcut(menu, app, &record.path) {
        Ok(shortcut) => written.shortcut = Some(shortcut),
        Err(error) => return (written, Some(error)),
    }
    match write_registration(app, record, size_kb) {
        Ok(()) => written.registration = true,
        Err(error) => return (written, Some(error)),
    }
    (written, None)
}

/// Takes back what [`integrate`] wrote. Best effort: a removal that fails is logged, not fatal —
/// the caller is already reporting the failure that caused the undo.
fn take_back(app: &CraftApp, written: Integrated) {
    let Integrated { shortcut, registration } = written;
    if let Some(shortcut) = shortcut
        && let Some(menu) = start_menu_programs()
        && let Err(error) = undo_shortcut(&menu, app, shortcut)
    {
        log::warn!("{}: couldn't undo the Start menu entry it had written: {error}", app.name);
    }
    if registration && let Err(error) = remove_registration(app) {
        log::warn!("{}: couldn't remove the Apps & features record it had written: {error}", app.name);
    }
}

/// Puts the Start menu entry back the way [`write_shortcut`] found it: one the install created is
/// removed, one it replaced is restored from the saved copy, so an entry the person (or an MSI
/// package) had put there is not lost to a failed install. A copy that cannot be moved back stays
/// where it is, and the error says where, so the person can put it back by hand.
fn undo_shortcut(menu: &Path, app: &CraftApp, shortcut: Shortcut) -> Result<(), String> {
    match shortcut {
        Shortcut::Created => remove_shortcut(menu, app),
        Shortcut::Replaced(backup) => std::fs::rename(&backup, shortcut_path(menu, app))
            .map_err(|e| format!("couldn't put the Start menu entry back ({e}); the entry that was there is saved at {}", backup.display())),
    }
}

/// The saved copy of an entry the install replaced is of no use once the install stands, or once
/// its undo has happened: drop it, best effort.
fn drop_shortcut_backup(shortcut: &Option<Shortcut>) {
    if let Some(Shortcut::Replaced(backup)) = shortcut
        && let Err(error) = std::fs::remove_file(backup)
    {
        log::warn!("couldn't remove the saved copy of the Start menu entry it replaced: {error}");
    }
}

/// One value of the Apps & features record: a string (`REG_SZ`) or a number (`REG_DWORD`).
#[derive(Debug, PartialEq)]
enum Registered {
    Sz(String),
    Dword(u32),
}

impl Registered {
    /// The value as the registry stores it.
    fn reg_value(&self) -> winreg::RegValue {
        use winreg::types::ToRegValue;
        match self {
            Registered::Sz(text) => text.as_str().to_reg_value(),
            Registered::Dword(number) => number.to_reg_value(),
        }
    }
}

/// The values a fast install's Apps & features record carries. Pure data, so its layout — what
/// the Settings app shows and what uninstalling needs — is checked by tests.
fn registration_values(app: &CraftApp, record: &Record, size_kb: u32, uninstall: &str) -> Vec<(&'static str, Registered)> {
    let folder = record.path.parent().map(|path| path.display().to_string()).unwrap_or_default();
    let display_icon = record.path.display().to_string();
    vec![
        ("DisplayName", Registered::Sz(app.name.to_string())),
        ("DisplayVersion", Registered::Sz(record.version.clone())),
        ("Publisher", Registered::Sz(app.msi_publisher.unwrap_or(crate::detect::PUBLISHER).to_string())),
        ("InstallLocation", Registered::Sz(folder)),
        ("DisplayIcon", Registered::Sz(display_icon)),
        ("UninstallString", Registered::Sz(uninstall.to_string())),
        ("URLInfoAbout", Registered::Sz(app.website())),
        ("NoModify", Registered::Dword(1)),
        ("NoRepair", Registered::Dword(1)),
        ("EstimatedSize", Registered::Dword(size_kb)),
    ]
}

/// The command the Apps & features record runs to uninstall: the launcher, asked to remove this
/// app without a window. The exe is quoted, as the Settings app and `Run` expect of a path with
/// spaces; the app id names the copy to remove.
fn uninstall_command(launcher: &Path, id: &str) -> String {
    format!("\"{}\" --uninstall {}", launcher.display(), id)
}

/// The size of the installed files, in kilobytes, as Apps & features shows it. Files that cannot
/// be read are skipped, not guessed at: the number is a display hint, never a record.
fn installed_kb(record: &Record) -> u32 {
    let bytes = record.files.iter().filter_map(|file| std::fs::metadata(file).ok()).map(|meta| meta.len()).sum::<u64>();
    u32::try_from(bytes / 1024).unwrap_or(u32::MAX)
}

/// The values a registry key carried before the install rewrote it, so a failed write leaves the
/// key exactly as it was: a key the install created is taken away again, and one that was there
/// (the previous version's, or a same-named one an MSI package wrote) gets its values back.
struct KeySnapshot {
    existed: bool,
    values: Vec<(String, winreg::RegValue)>,
}

impl KeySnapshot {
    /// What `path` carries now. A key that cannot be read refuses the write before anything is
    /// written: the install only writes a registry key it can put back.
    fn take(hkcu: &winreg::RegKey, path: &str) -> std::io::Result<Self> {
        match hkcu.open_subkey(path) {
            Ok(key) => {
                let values = key.enum_values().collect::<Result<Vec<_>, _>>()?;
                Ok(Self { existed: true, values })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self { existed: false, values: Vec::new() }),
            Err(error) => Err(error),
        }
    }

    /// Puts back what [`write_values`] put into the key: every name in `written` gets the value
    /// it carried before, or is deleted when the key had none, and a key the install created is
    /// removed. Best effort: a value that cannot be put back is left as the failed write left it.
    fn undo(&self, hkcu: &winreg::RegKey, path: &str, written: &[String]) {
        if !self.existed {
            let _ = hkcu.delete_subkey(path);
            return;
        }
        if let Ok(key) = hkcu.open_subkey(path) {
            for name in written {
                match self.values.iter().find(|(had, _)| had == name) {
                    Some((_, value)) => {
                        let _ = key.set_raw_value(name, value);
                    }
                    None => {
                        let _ = key.delete_value(name);
                    }
                }
            }
        }
    }
}

/// Writes `values` into `key` in order, stopping at the first failure and reporting both the
/// names that got written and what went wrong, so the caller can undo exactly those.
fn write_values(
    key: &winreg::RegKey,
    values: impl IntoIterator<Item = (&'static str, winreg::RegValue)>,
    what: &str,
) -> (Vec<String>, Option<String>) {
    let mut written = Vec::new();
    for (name, value) in values {
        if let Err(error) = key.set_raw_value(name, &value) {
            return (written, Some(format!("{what} ({error})")));
        }
        written.push(name.to_string());
    }
    (written, None)
}

/// Writes the Apps & features record and the App Paths key under `HKCU`. Each key is written so a
/// failure anywhere leaves it exactly as it was, and the record's key is undone again when the
/// Run registration fails, so the install leaves no half listing behind: [`take_back`] sees
/// either a finished write or nothing at all.
fn write_registration(app: &CraftApp, record: &Record, size_kb: u32) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;

    let launcher = std::env::current_exe().map_err(|e| format!("couldn't find the launcher's own program ({e})"))?;
    let uninstall = uninstall_command(&launcher, app.id);
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);

    // The Apps & features record first: the Run registration failing must take its values back
    // too, not only the failing key's own.
    let uninstall_path = uninstall_key(app.id);
    let undo_record = KeySnapshot::take(&hkcu, &uninstall_path).map_err(|e| format!("couldn't read the app's Apps & features record ({e})"))?;
    let (entry, _) = hkcu.create_subkey(&uninstall_path).map_err(|e| format!("couldn't list the app in Apps & features ({e})"))?;
    let values = registration_values(app, record, size_kb, &uninstall);
    let (written, failure) =
        write_values(&entry, values.iter().map(|(name, value)| (*name, value.reg_value())), "couldn't write the Apps & features record");
    if let Some(error) = failure {
        undo_record.undo(&hkcu, &uninstall_path, &written);
        return Err(error);
    }

    let app_paths_path = app_paths_key(app.id);
    let undo_run = KeySnapshot::take(&hkcu, &app_paths_path).map_err(|e| {
        undo_record.undo(&hkcu, &uninstall_path, &written);
        format!("couldn't read the app's Run registration ({e})")
    })?;
    let (paths, _) = hkcu.create_subkey(&app_paths_path).map_err(|e| {
        undo_record.undo(&hkcu, &uninstall_path, &written);
        format!("couldn't register the app for the Run dialog ({e})")
    })?;
    let target = Registered::Sz(record.path.display().to_string()).reg_value();
    let (written_run, failure) = write_values(&paths, [("", target)], "couldn't register the app for the Run dialog");
    if let Some(error) = failure {
        undo_run.undo(&hkcu, &app_paths_path, &written_run);
        undo_record.undo(&hkcu, &uninstall_path, &written);
        return Err(error);
    }
    Ok(())
}

/// Removes the Apps & features record and the App Paths key. A key that is already gone is fine:
/// removal must work whatever an earlier failure left behind.
fn remove_registration(app: &CraftApp) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for key in [uninstall_key(app.id), app_paths_key(app.id)] {
        match hkcu.delete_subkey(&key) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("couldn't remove the app's registration ({error})")),
        }
    }
    Ok(())
}

/// Whether the Apps & features key a fast install of `id` writes is there, whatever it carries.
fn registered(id: &str) -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    RegKey::predef(HKEY_CURRENT_USER).open_subkey(uninstall_key(id)).is_ok()
}

/// The folder the Apps & features record names, when it is still there.
fn registered_folder(id: &str) -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let entry = RegKey::predef(HKEY_CURRENT_USER).open_subkey(uninstall_key(id)).ok()?;
    entry.get_value::<String, _>("InstallLocation").ok()
}

/// Counts the shortcut writes this process has made, so their side files (the staging copy and
/// the saved copy of a replaced entry) never take each other's names.
static WRITE_SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// Writes the Start menu shortcut pointing at `target`, replacing the one a previous fast install
/// of the same app left, whose target is the same exe. A folder or a link in the shortcut's place
/// is refused rather than replaced: only a shortcut file may be swapped for a shortcut. What was
/// in the entry's place is reported ([`Shortcut`]), so a rollback puts it back instead of
/// deleting an entry the person or an MSI package had put there.
fn write_shortcut(menu: &Path, app: &CraftApp, target: &Path) -> Result<Shortcut, String> {
    if !target.is_file() {
        return Err(format!("{} isn't where it was installed", target.display()));
    }
    std::fs::create_dir_all(menu).map_err(|e| format!("couldn't open the Start menu's Programs folder ({e})"))?;
    let lnk = shortcut_path(menu, app);
    if let Ok(meta) = std::fs::symlink_metadata(&lnk)
        && !meta.is_file()
    {
        return Err(format!("The Start menu entry {} isn't a shortcut the installer can replace.", app.name));
    }
    // Prefer a normal path without the `\\?\` prefix — .lnk targets work better that way.
    let target = dunce::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    // mslnk panics on non-UTF-8 paths; fail with a message instead of killing the worker thread.
    if target.to_str().is_none() {
        return Err("path isn't valid Unicode".into());
    }
    let mut link = mslnk::ShellLink::new(&target).map_err(|e| e.to_string())?;
    if let Some(dir) = target.parent().and_then(|p| p.to_str()) {
        link.set_working_dir(Some(dir.to_string()));
    }
    // Write beside the entry, then publish with a rename, so a crash never leaves a half shortcut
    // and the swap with the previous version's entry is one step.
    let serial = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = menu.join(format!(".{}.{}.{}.lnk.tmp", app.id, std::process::id(), serial));
    let _ = std::fs::remove_file(&temp);
    if let Err(e) = link.create_lnk(&temp) {
        let _ = std::fs::remove_file(&temp);
        return Err(e.to_string());
    }
    // A shortcut file already in the entry's place is moved aside first: on Windows, a rename
    // does not replace the file it lands on, so publishing over it fails on the second install
    // and on every update. The moved copy goes with the answer ([`Shortcut::Replaced`]) — a
    // rollback puts it back — and a move that cannot happen (the file is held open, say) refuses
    // the write rather than publish half of it.
    let saved = match std::fs::symlink_metadata(&lnk) {
        Ok(_) => {
            let backup = menu.join(format!(".{}.{}.{}.lnk.prev", app.id, std::process::id(), serial));
            match std::fs::rename(&lnk, &backup) {
                Ok(()) => Some(backup),
                Err(e) => {
                    let _ = std::fs::remove_file(&temp);
                    return Err(format!("couldn't replace the Start menu entry ({e})"));
                }
            }
        }
        Err(_) => None,
    };
    match std::fs::rename(&temp, &lnk) {
        Ok(()) => Ok(saved.map_or(Shortcut::Created, Shortcut::Replaced)),
        Err(e) => {
            let mut error = format!("couldn't publish the Start menu entry ({e})");
            if let Some(backup) = &saved
                && std::fs::rename(backup, &lnk).is_err()
            {
                error.push_str(&format!(", and the entry it replaced is saved at {}", backup.display()));
            }
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

/// Removes the Start menu shortcut, if it is still a shortcut file.
fn remove_shortcut(menu: &Path, app: &CraftApp) -> Result<(), String> {
    let lnk = shortcut_path(menu, app);
    match std::fs::symlink_metadata(&lnk) {
        Ok(meta) if meta.is_file() => std::fs::remove_file(&lnk).map_err(|e| format!("couldn't remove the Start menu entry ({e})")),
        _ => Ok(()),
    }
}

/// Removes everything the install told Windows about the app: the Start menu entry, the Apps &
/// features record and the App Paths key.
fn remove_integration(app: &CraftApp) {
    if let Some(menu) = start_menu_programs()
        && let Err(error) = remove_shortcut(&menu, app)
    {
        log::warn!("{}: {error}", app.name);
    }
    if let Err(error) = remove_registration(app) {
        log::warn!("{}: {error}", app.name);
    }
}

/// The manifest is written by one removal at a time in this process: the owner runs its own
/// removal and, on other threads, the ones other processes hand it.
static REMOVALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Removes a fast-installed app without a window: what the Apps & features record's Uninstall
/// command runs (`<launcher> --uninstall <app>`).
///
/// The manifest that says what may be removed is one file for the whole account, and the running
/// launcher commits to it as it installs and removes other apps: this helper never writes the
/// file beside a launcher that is running. When one holds the account's instance and proves it,
/// the removal is handed to it and its answer is this one's
/// ([`crate::single_instance::forward_uninstall`]); when none does, this process holds the
/// instance for the duration ([`crate::single_instance::Held`]), so a launcher starting meanwhile
/// defers to it instead of racing the save, and the removal is its own ([`remove_owned`]).
///
/// With the launcher's own record of the copy, the removal is the launcher's ordinary one: the
/// recorded files, the Start menu entry and both registry keys, with the record dropped from the
/// manifest. What is removed without that record, and what is left alone, is
/// [`remove_recorded`]'s to say.
pub fn uninstall_app(id: &str) -> Result<(), String> {
    let app = CraftApp::by_id(id).ok_or_else(|| format!("{id} isn't one of the Craft apps this launcher installs."))?;
    // This process may be the instance's owner already, or become it: either way, removals other
    // processes hand over are served here too (two headless removals starting together).
    crate::single_instance::set_uninstall_handler(std::sync::Arc::new(remove_owned));
    for _ in 0..3 {
        match crate::single_instance::forward_uninstall(id) {
            crate::single_instance::Forwarded::Answered(result) => return result,
            crate::single_instance::Forwarded::Free => {
                if let Some(held) = crate::single_instance::Held::take() {
                    let result = remove_owned(id);
                    drop(held);
                    return result;
                }
            }
            crate::single_instance::Forwarded::Held => {
                return Err(format!(
                    "{} couldn't be removed: something holds the launcher's instance and didn't take the removal, so its record of installed apps was left alone. Quit it and try again.",
                    app.name
                ));
            }
        }
        // The instance changed hands while this removal was starting (another removal, or a
        // launcher being replaced): one more look, and then an honest refusal rather than a
        // write beside whoever holds it.
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(format!("{} couldn't be removed: the launcher's instance kept changing hands while the removal started. Try again.", app.name))
}

/// The removal the instance's owner performs: its own `--uninstall`, or a removal another process
/// handed over. One at a time ([`REMOVALS`]), and the commit reads the manifest again before it
/// saves, so the records of apps removed or installed alongside survive.
fn remove_owned(id: &str) -> Result<(), String> {
    let _removal = REMOVALS.lock().unwrap_or_else(PoisonError::into_inner);
    let paths = Paths::new()?;
    let app = CraftApp::by_id(id).ok_or_else(|| format!("{id} isn't one of the Craft apps this launcher installs."))?;
    let manifest = paths.manifest();
    let read = || -> Result<crate::manifest::Manifest, String> { Ok(crate::manifest::Manifest::load(&manifest)) };
    let mut commit = || {
        let mut current = crate::manifest::Manifest::load(&manifest);
        let changed = current.apps.remove(id).is_some() || current.releases.remove(id).is_some();
        if changed { current.save(&manifest) } else { Ok(()) }
    };
    remove_recorded(app, &paths, &crate::http::Stop::default(), &read, &mut commit)
}

/// The removal itself, stopping when `stop` says so, reading the manifest through `read` for what
/// it decides from and committing the record's removal through `commit`, so the headless helper
/// and the launcher's own uninstall each write the file through their own coordination.
///
/// With the launcher's own record of the copy, the removal is the launcher's ordinary one: the
/// recorded files, the Start menu entry and both registry keys, with the record dropped from the
/// manifest. Without it — the files were deleted by hand, the manifest was reset — the program
/// folder cannot be told from the person's own files, so only the registration is taken back and
/// the folder named in it is left alone, with the reason reported. With no registration either,
/// nothing is touched: the Start menu entry may be an MSI package's or the person's own.
pub(crate) fn remove_recorded(
    app: &CraftApp,
    paths: &Paths,
    stop: &crate::http::Stop,
    read: &dyn Fn() -> Result<crate::manifest::Manifest, String>,
    commit: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let manifest = read()?;
    let record = manifest.apps.get(app.id).cloned();
    let installed = crate::detect::detect(app, &manifest, &crate::detect::Snapshot::take());
    let ours = |record: &Record, installed: &artcraft_launcher_core::Installed| {
        record.kind == PackageKind::FastInstall && installed.source == InstallSource::Launcher
    };
    match (record.as_ref(), installed.as_ref()) {
        (Some(record), Some(installed)) if ours(record, installed) => {
            let mut phase = |_: artcraft_launcher_core::protocol::JobPhase| ();
            crate::install::uninstall(app, installed, Some(record), paths, stop, &mut phase)?;
            commit()
        }
        // Nothing of a fast install is registered: nothing is removed. The files are not the
        // launcher's to vouch for, and the Start menu entry under the app's name may well be an
        // MSI package's or the person's own.
        _ if !registered(app.id) => Err(format!(
            "{} has no fast install to remove: the launcher has no record of its files and no Apps & features listing of its own, so nothing was touched.",
            app.name
        )),
        // A fast install's listing without its record: take the registration back, and say where
        // the folder was, rather than deleting a folder it cannot vouch for.
        _ => {
            let folder = registered_folder(app.id);
            remove_integration(app);
            Err(match folder {
                Some(folder) if !folder.is_empty() => format!(
                    "The launcher no longer has a record of {}'s files, so the folder {} was left alone; its Start menu entry and Apps & features listing were removed.",
                    app.name, folder
                ),
                _ => format!(
                    "{} isn't installed by the launcher's fast install any more, so only its Start menu entry and Apps & features listing were removed.",
                    app.name
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Apps & features record names the app, its version, the publisher its MSI also names,
    /// where it is, how to uninstall it, and that Windows offers no modify or repair; the size is
    /// the one measured from the installed files.
    #[test]
    fn the_registration_names_the_app_and_how_to_remove_it() {
        let app = CraftApp::by_id("photocraft").unwrap();
        let folder = Path::new(r"C:\Users\a\AppData\Local\Programs\ArtCraft\photocraft");
        let record = Record {
            version: "0.12.0-rc.1".into(),
            kind: PackageKind::FastInstall,
            path: folder.join("photocraft.exe"),
            installed_at: 0,
            files: vec![folder.join("photocraft.exe"), folder.join("LICENSE-MIT")],
            needs_root: false,
        };
        let values = registration_values(app, &record, 4096, r#""C:\Program Files\ArtCraft Launcher\artcraft-launcher.exe" --uninstall photocraft"#);
        let value = |name: &str| values.iter().find(|(key, _)| *key == name).map(|(_, value)| value);
        assert_eq!(value("DisplayName"), Some(&Registered::Sz("PhotoCraft".into())));
        // The whole version, pre-release label included, which Windows Installer itself drops.
        assert_eq!(value("DisplayVersion"), Some(&Registered::Sz("0.12.0-rc.1".into())));
        assert_eq!(value("Publisher"), Some(&Registered::Sz("Learning Machines LLC".into())));
        assert_eq!(value("InstallLocation"), Some(&Registered::Sz(folder.display().to_string())));
        assert_eq!(value("DisplayIcon"), Some(&Registered::Sz(folder.join("photocraft.exe").display().to_string())));
        assert_eq!(
            value("UninstallString"),
            Some(&Registered::Sz(r#""C:\Program Files\ArtCraft Launcher\artcraft-launcher.exe" --uninstall photocraft"#.into()))
        );
        assert_eq!(value("URLInfoAbout"), Some(&Registered::Sz("https://getartcraft.com/apps/photocraft".into())));
        assert_eq!(value("NoModify"), Some(&Registered::Dword(1)));
        assert_eq!(value("NoRepair"), Some(&Registered::Dword(1)));
        assert_eq!(value("EstimatedSize"), Some(&Registered::Dword(4096)));
        // A community app's registration names its own publisher.
        let community = CraftApp::by_id("solidcraft").unwrap();
        let values = registration_values(community, &record, 0, "uninstall");
        assert_eq!(
            values.iter().find(|(key, _)| *key == "Publisher").map(|(_, value)| value),
            Some(&Registered::Sz("SolidCraft contributors".into()))
        );
    }

    /// The Uninstall command quotes the launcher's path — it can hold spaces — and names the app.
    #[test]
    fn the_uninstall_command_quotes_the_launcher_and_names_the_app() {
        assert_eq!(
            uninstall_command(Path::new(r"C:\Program Files\ArtCraft Launcher\artcraft-launcher.exe"), "photocraft"),
            r#""C:\Program Files\ArtCraft Launcher\artcraft-launcher.exe" --uninstall photocraft"#
        );
    }

    /// The Apps & features key is the launcher's own name, not a product-code GUID, so Windows
    /// Installer detection never mistakes the record for an MSI registration, and one app's record
    /// never collides with another's.
    #[test]
    fn the_apps_and_features_key_is_never_a_product_code() {
        assert_eq!(uninstall_key("photocraft"), r"Software\Microsoft\Windows\CurrentVersion\Uninstall\ArtCraftLauncher.photocraft");
        for id in ["photocraft", "artcraft-launcher", "a"] {
            let key = uninstall_key(id);
            let name = key.rsplit('\\').next().unwrap_or("");
            assert!(!crate::msi_context::valid_product_code(name), "{name}");
            assert_ne!(name, uninstall_key("other").rsplit('\\').next().unwrap_or("other"), "one app's record per key");
        }
    }

    /// The shortcut is the app's display name in the Programs folder, where the suite's MSI
    /// packages also put theirs.
    #[test]
    fn the_shortcut_is_the_apps_name_in_the_programs_folder() {
        let app = CraftApp::by_id("photocraft").unwrap();
        let menu = Path::new(r"C:\Users\a\AppData\Roaming\Microsoft\Windows\Start Menu\Programs");
        assert_eq!(shortcut_path(menu, app), menu.join("PhotoCraft.lnk"));
    }

    /// The measured size is the files' bytes in kilobytes; a file that cannot be read is skipped
    /// rather than guessed at, and a size too large for the record's number saturates.
    #[test]
    fn the_measured_size_counts_the_files_in_kilobytes() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("photocraft.exe");
        let two = dir.path().join("runtime.dll");
        std::fs::write(&one, vec![0; 2048]).unwrap();
        std::fs::write(&two, vec![0; 1025]).unwrap();
        let record = Record {
            version: "1.0.0".into(),
            kind: PackageKind::FastInstall,
            path: one.clone(),
            installed_at: 0,
            files: vec![one, two, dir.path().join("missing.dll")],
            needs_root: false,
        };
        assert_eq!(installed_kb(&record), 3);
    }

    /// A shortcut is written for a real executable, replaces the previous version's entry (the
    /// same target) and reports the copy it saved, refuses to replace a folder, and is removed by
    /// name. A rollback puts a replaced entry back instead of deleting it, and one the install
    /// created is removed.
    #[test]
    fn the_start_menu_shortcut_is_written_replaced_and_removed() {
        let menu = tempfile::tempdir().unwrap();
        let app = CraftApp::by_id("photocraft").unwrap();
        let program = menu.path().join("program.exe");
        std::fs::write(&program, b"MZ").unwrap();

        let shortcut = write_shortcut(menu.path(), app, &program).unwrap();
        let lnk = shortcut_path(menu.path(), app);
        assert!(matches!(shortcut, Shortcut::Created), "an empty entry's place makes the entry the install's own");
        assert!(lnk.is_file(), "the shortcut is published under the app's name");

        // The previous version's entry is replaced, not left beside a second one, and what it
        // replaced is saved for a rollback rather than dropped.
        std::fs::write(&lnk, b"the previous version's shortcut").unwrap();
        let replaced = write_shortcut(menu.path(), app, &program).unwrap();
        assert!(lnk.is_file());
        let Shortcut::Replaced(backup) = replaced else {
            panic!("a replaced entry is reported with its saved copy");
        };
        let left: Vec<_> = std::fs::read_dir(menu.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(left.len(), 3, "the program, the entry and its saved copy are there, no temporary shortcut: {left:?}");

        // The shortcut points at the program as UTF-16, like any .lnk.
        let data = std::fs::read(&lnk).unwrap();
        assert!(data.len() >= 0x4c, "too small for a .lnk: {} bytes", data.len());
        let needle: Vec<u8> = "program.exe".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert!(data.windows(needle.len()).any(|w| w == needle.as_slice()), "shortcut missing its target");

        // A rollback of an install that replaced an entry puts it back, not deletes it.
        std::fs::write(&backup, b"a shortcut the person made").unwrap();
        undo_shortcut(menu.path(), app, Shortcut::Replaced(backup.clone())).unwrap();
        assert_eq!(std::fs::read(&lnk).unwrap(), b"a shortcut the person made");
        assert!(!backup.exists(), "the saved copy moved back into the entry's place");

        // A rollback of an install that created the entry removes it.
        undo_shortcut(menu.path(), app, Shortcut::Created).unwrap();
        assert!(!lnk.exists());

        // A folder in the shortcut's place is refused, not replaced.
        std::fs::create_dir(&lnk).unwrap();
        let error = write_shortcut(menu.path(), app, &program).unwrap_err();
        assert!(error.contains("isn't a shortcut"), "{error}");
        assert!(lnk.is_dir());

        // Removal takes a shortcut file and leaves anything else alone.
        std::fs::remove_dir(&lnk).unwrap();
        std::fs::write(&lnk, b"a shortcut the person made").unwrap();
        remove_shortcut(menu.path(), app).unwrap();
        assert!(!lnk.exists());
        std::fs::create_dir(&lnk).unwrap();
        remove_shortcut(menu.path(), app).unwrap();
        assert!(lnk.is_dir(), "only a shortcut file is removed");
    }

    /// A saved copy of a replaced entry that is of no more use is dropped, best effort, so a
    /// committed install leaves none of its side files behind.
    #[test]
    fn a_committed_install_drops_the_copy_of_the_entry_it_replaced() {
        let menu = tempfile::tempdir().unwrap();
        let backup = menu.path().join(".photocraft.0.1.lnk.prev");
        std::fs::write(&backup, b"the previous version's shortcut").unwrap();
        drop_shortcut_backup(&Some(Shortcut::Replaced(backup.clone())));
        assert!(!backup.exists(), "the copy of a committed install's entry is gone");
        // An entry the install created, or wrote nothing about, has no copy to drop.
        drop_shortcut_backup(&Some(Shortcut::Created));
        drop_shortcut_backup(&None);
    }

    /// A missing target is never linked to.
    #[test]
    fn the_shortcut_refuses_a_missing_program() {
        let menu = tempfile::tempdir().unwrap();
        let app = CraftApp::by_id("photocraft").unwrap();
        let error = write_shortcut(menu.path(), app, &menu.path().join("gone.exe")).unwrap_err();
        assert!(error.contains("isn't where it was installed"), "{error}");
        assert!(!shortcut_path(menu.path(), app).exists());
    }

    /// The installed folder is named by the catalog id, beside the suite's other programs.
    #[test]
    fn fresh_installs_default_to_the_per_user_programs_folder() {
        // The Programs folder every account has; a machine without %LOCALAPPDATA% has none, and
        // the install refuses rather than guessing a folder.
        match (programs_folder(), default_parent()) {
            (Some(programs), Ok(parent)) => {
                assert!(programs.ends_with(r"Programs\ArtCraft"), "{}", programs.display());
                assert_eq!(parent, programs);
            }
            (None, Err(error)) => assert_eq!(error, "couldn't find the per-user Programs folder"),
            other => panic!("the default folder and its error disagree: {other:?}"),
        }
    }
}
