//! ArtCraft Launcher updating itself.
//!
//! The new version is downloaded and verified like any Craft App, then swapped in the way this
//! copy was installed. A small helper process waits for this launcher to exit, finishes anything
//! that can't happen while it runs (Windows locks a running exe), and starts the new one. The
//! launcher has to exit first anyway: it holds the single-instance lock, and a new copy started
//! while it runs would only ask it to show its window.
//!
//! | Installed as | Swap |
//! |---|---|
//! | macOS `.app` | copy the new bundle from the DMG over this one (fine while running), reopen it |
//! | Windows MSI (per-user, any folder) | after exit, run the new MSI (it reinstalls into the same folder), then start the exe |
//! | Windows portable zip | after exit, unzip over this folder (settings in `ArtCraftLauncherData` stay) |
//! | Linux AppImage | rename the new AppImage over this one (fine while running), start it |
//! | Linux `.deb` / `.rpm` | the system package manager, with the password prompt; then restart |
//!
//! Anything else (a Flatpak, FreeBSD, a development build) gets the download link instead.

use std::path::{Path, PathBuf};

use artcraft_launcher_core::PackageKind;
use artcraft_launcher_core::catalog::LAUNCHER;
use artcraft_launcher_core::platform::{Platform, pick_asset};
use artcraft_launcher_core::protocol::JobPhase;
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::version::{self, Version};
#[cfg(target_os = "macos")]
use artcraft_launcher_core::{InstallSource, Installed};

use crate::http::{Http, Stop};
use crate::install::{self, Job};
use crate::paths::Paths;
use crate::process;

/// How this running launcher was installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// The `ArtCraft Launcher.app` bundle.
    MacApp(PathBuf),
    /// The per-user Windows Installer package; the exe.
    Msi(PathBuf),
    /// The Windows portable zip; the exe (with `portable.txt` beside it).
    Portable(PathBuf),
    /// The AppImage file.
    AppImage(PathBuf),
    /// A Debian or RPM package; the binary.
    Package(PackageKind, PathBuf),
}

impl Install {
    /// Refuse unsupported version changes before downloading or starting an exit helper.
    pub(crate) fn validate_release(&self, release: &Release, current: Option<&Version>) -> Result<(), String> {
        let target = release.version.as_ref().ok_or("This release's version cannot be verified.")?;
        let current = current.ok_or("This release's version cannot be verified.")?;
        if target < current {
            match self {
                Self::Msi(_) => return Err("Launcher MSI downgrades require a portable copy. This installation can upgrade only.".into()),
                Self::Package(PackageKind::Rpm, _) => return Err("This package cannot downgrade here. Use a portable installation instead.".into()),
                _ => {}
            }
        }
        Ok(())
    }

    fn kind(&self) -> PackageKind {
        match self {
            Self::MacApp(_) => PackageKind::Dmg,
            Self::Msi(_) => PackageKind::Msi,
            Self::Portable(_) => PackageKind::PortableZip,
            Self::AppImage(_) => PackageKind::AppImage,
            Self::Package(kind, _) => *kind,
        }
    }
}

/// Makes an installed copy believe it's this version, to try the self-update against the current
/// release. The restart helper drops it, so it lasts one run.
const PRETEND_VAR: &str = "ARTCRAFT_LAUNCHER_PRETEND_VERSION";

/// The version this launcher counts as (see [`PRETEND_VAR`]).
pub fn current_version() -> Option<Version> {
    std::env::var(PRETEND_VAR).ok().and_then(|v| version::parse(&v)).or_else(|| version::parse(env!("CARGO_PKG_VERSION")))
}

/// How long a just-installed version stays exempt from being offered again.
const JUST_UPDATED_FOR: i64 = 24 * 3600;
const FAILED_UPDATE_RETRY_FOR: i64 = 10 * 60;

/// Whether `release` is the version this launcher just updated itself to. Offering it again would
/// loop: update, restart, "update available", … That happens if a release's binary reports a
/// different version from its tag, or while a pretend version is set.
pub fn just_updated_to(paths: &Paths, release: &Release) -> bool {
    let Ok(text) = std::fs::read_to_string(marker(paths)) else { return false };
    let mut lines = text.lines();
    let (Some(tag), Some(at)) = (lines.next(), lines.next().and_then(|a| a.parse::<i64>().ok())) else { return false };
    let delay = if lines.next() == Some("failed") { FAILED_UPDATE_RETRY_FOR } else { JUST_UPDATED_FOR };
    tag == release.tag && crate::now() - at < delay
}

fn marker(paths: &Paths) -> PathBuf {
    paths.data.join("self-update.txt")
}

/// How this copy is installed, if it's one the launcher knows how to replace.
pub fn current() -> Option<Install> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    detect(&exe)
}

#[cfg(target_os = "macos")]
fn detect(exe: &Path) -> Option<Install> {
    let bundle = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))?;
    let plist = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    let id = plist.as_dictionary()?.get("CFBundleIdentifier")?.as_string()?;
    (id == LAUNCHER.bundle_id()).then(|| Install::MacApp(bundle.to_path_buf()))
}

#[cfg(windows)]
fn detect(exe: &Path) -> Option<Install> {
    let dir = exe.parent()?;
    if dir.join("portable.txt").is_file() {
        return Some(Install::Portable(exe.to_path_buf()));
    }
    // The MSI installs per user, into the folder chosen in its wizard (which it records in the
    // registry) or by default %LOCALAPPDATA%\Programs\ArtCraft Launcher.
    let canonical = |p: PathBuf| p.canonicalize().unwrap_or(p);
    let recorded = msi_install_dir().map(canonical);
    let default = dirs::data_local_dir().map(|d| canonical(d.join("Programs").join(LAUNCHER.name)));
    [recorded, default].into_iter().flatten().any(|d| d == dir).then(|| Install::Msi(exe.to_path_buf()))
}

/// The folder the MSI installed into, as it records it (`packaging/windows/artcraft-launcher.wxs`).
#[cfg(windows)]
fn msi_install_dir() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Learning Machines LLC\ArtCraft Launcher").ok()?;
    let dir: String = key.get_value("InstallDir").ok()?;
    let dir = dir.trim();
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

#[cfg(target_os = "linux")]
fn detect(exe: &Path) -> Option<Install> {
    if process::in_sandbox() {
        return None;
    }
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Some(Install::AppImage(PathBuf::from(appimage)));
    }
    if exe != Path::new("/usr/bin").join(LAUNCHER.id) {
        return None;
    }
    let t = std::time::Duration::from_secs(10);
    if process::capture("dpkg-query", &["-W", "-f", "${db:Status-Status}", LAUNCHER.id], t).is_some_and(|s| s.trim() == "installed") {
        return Some(Install::Package(PackageKind::Deb, exe.to_path_buf()));
    }
    if process::capture("rpm", &["-q", LAUNCHER.id], t).is_some() {
        return Some(Install::Package(PackageKind::Rpm, exe.to_path_buf()));
    }
    None
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
fn detect(_exe: &Path) -> Option<Install> {
    None
}

/// Downloads `release` for `platform`, puts it in place and starts the helper that relaunches
/// it once this process has exited. On success the caller must quit promptly. `last` is called
/// once the download is ready and returns when the launcher may replace itself and restart.
#[allow(clippy::too_many_arguments)]
pub fn update(
    install: &Install,
    release: &Release,
    platform: Platform,
    http: &Http,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
    last: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    install.validate_release(release, current_version().as_ref())?;
    let (asset, _) = pick_asset(release, &[LAUNCHER.id], platform, &[install.kind()])
        .ok_or_else(|| format!("ArtCraft Launcher {} has no {} for {}", release.version_label(), install.kind().label(), platform.label()))?;
    let asset = asset.clone();
    let mut job = Job { http, paths, stop, phase };
    let (file, verified) = install::download(release, &asset, &mut job)?;
    if verified != crate::http::Verified::Checksum {
        return Err("This release has no published checksum for this download.".into());
    }
    // The restart ends this process: nothing else may be under way.
    last()?;
    phase(JobPhase::Installing);
    crate::paths::write_atomic(&marker(paths), format!("{}\n{}\n", release.tag, crate::now()).as_bytes()).map_err(|e| {
        log::warn!("couldn't record the self-update: {e}");
        "Could not save the launcher update state. Update was not started.".to_string()
    })?;
    let pid = std::process::id();
    let result = match install {
        #[cfg(target_os = "macos")]
        Install::MacApp(bundle) => {
            let existing = Installed { version: None, source: InstallSource::MacApp, location: bundle.display().to_string(), package: None };
            let placed = install::macos::install_dmg(&LAUNCHER, &file, Some(&existing), paths, &install::macos::Expect::release(release));
            let _ = std::fs::remove_file(&file);
            let placed = placed?;
            relaunch_after_exit(pid, &[Path::new("/usr/bin/open").as_os_str(), placed.as_os_str()])
        }
        #[cfg(target_os = "linux")]
        Install::AppImage(path) => {
            replace_appimage(&file, path)?;
            let _ = std::fs::remove_file(&file);
            relaunch_after_exit(pid, &[path.as_os_str()])
        }
        #[cfg(target_os = "linux")]
        Install::Package(kind, exe) => {
            let result = install::linux::install_package(*kind, &file, stop, phase);
            let _ = std::fs::remove_file(&file);
            result?;
            relaunch_after_exit(pid, &[exe.as_os_str()])
        }
        #[cfg(windows)]
        Install::Msi(exe) => msi_step(&file, &paths.cache.join("launcher-self-update.log")).and_then(|step| windows_helper(pid, &step, exe, paths)),
        #[cfg(windows)]
        Install::Portable(exe) => {
            let dir = exe.parent().ok_or("the launcher isn't in a folder")?;
            let tmp = paths.cache.join("launcher-update");
            windows_helper(
                pid,
                &format!(
                    "$t = {tmp}; Remove-Item -LiteralPath $t -Recurse -Force -ErrorAction SilentlyContinue; Expand-Archive -LiteralPath {zip} -DestinationPath $t -Force; \
                     $top = Get-ChildItem -LiteralPath $t | Select-Object -First 1; Copy-Item -Path (Join-Path $top.FullName '*') -Destination {dir} -Recurse -Force; \
                     Remove-Item -LiteralPath $t -Recurse -Force -ErrorAction SilentlyContinue; Remove-Item -LiteralPath {zip} -ErrorAction SilentlyContinue",
                    tmp = ps_quote(&tmp),
                    zip = ps_quote(&file),
                    dir = ps_quote(dir),
                ),
                exe,
                paths,
            )
        }
        #[allow(unreachable_patterns)]
        _ => {
            let _ = (pid, &file);
            Err("this copy of ArtCraft Launcher can't update itself; download the new version instead".into())
        }
    };
    if result.is_err() {
        let _ = std::fs::remove_file(marker(paths));
    }
    result
}

/// Copies the new AppImage beside the running one and renames it over it. The running copy is
/// mounted from its own (now unlinked) file, so it keeps working until it exits.
#[cfg(target_os = "linux")]
fn replace_appimage(new: &Path, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().ok_or("the AppImage isn't in a folder")?;
    let staged = dir.join(format!(".{}.update", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    std::fs::copy(new, &staged).map_err(|e| format!("couldn't write next to {} ({e})", path.display()))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    std::fs::rename(&staged, path).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("couldn't replace {} ({e})", path.display())
    })
}

/// Starts `sh`, which waits for `pid` to exit and then runs `command`. Detached, so it outlives us.
#[cfg(unix)]
fn relaunch_after_exit(pid: u32, command: &[&std::ffi::OsStr]) -> Result<(), String> {
    process::spawn_detached(helper_command(pid, command)).map_err(|e| format!("couldn't schedule the restart ({e})"))
}

#[cfg(unix)]
fn helper_command(pid: u32, command: &[&std::ffi::OsStr]) -> std::process::Command {
    let mut cmd = process::command("/bin/sh");
    cmd.arg("-c")
        .arg(r#"pid=$1; shift; i=0; while kill -0 "$pid" 2>/dev/null && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done; exec "$@""#)
        .arg("sh")
        .arg(pid.to_string())
        .args(command)
        // The new launcher reports its real version; a pretend one was for this run only.
        .env_remove(PRETEND_VAR);
    cmd
}

/// Starts a hidden PowerShell that waits for `pid` to exit, runs `step`, then starts `exe`.
#[cfg(windows)]
fn windows_helper(pid: u32, step: &str, exe: &Path, paths: &Paths) -> Result<(), String> {
    let script = windows_script(pid, step, exe, &marker(paths), &paths.cache.join("launcher-self-update.log"), 60_000);
    let mut cmd = process::command("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-Command", &script]).env_remove(PRETEND_VAR);
    process::spawn_detached(cmd).map_err(|e| format!("couldn't schedule the update ({e})"))
}

#[cfg(windows)]
fn msi_step(file: &Path, log: &Path) -> Result<String, String> {
    let arguments = crate::windows_msi::command_line("/i", file.as_os_str(), log, None)?;
    Ok(format!(
        "$msi = Start-Process -FilePath {msiexec} -ArgumentList {arguments} -WindowStyle Hidden -Wait -PassThru; \
         if ($msi.ExitCode -notin @(0, 3010, 1641)) {{ throw ('Windows Installer failed with exit code ' + $msi.ExitCode) }}; \
         Remove-Item -LiteralPath {file} -ErrorAction SilentlyContinue",
        msiexec = ps_quote(&install::windows::msiexec()),
        arguments = ps_literal(&arguments),
        file = ps_quote(file),
    ))
}

#[cfg(windows)]
fn windows_script(pid: u32, step: &str, exe: &Path, marker: &Path, log: &Path, wait_ms: u32) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'; \
         $old = Get-Process -Id {pid} -ErrorAction SilentlyContinue; \
         if ($null -ne $old -and -not $old.WaitForExit({wait_ms})) {{ \
             Add-Content -LiteralPath {marker} -Value 'failed' -ErrorAction SilentlyContinue; \
             Add-Content -LiteralPath {log} -Value 'The old launcher did not exit; self-update was not started.' -ErrorAction SilentlyContinue; exit 1 \
         }}; \
         try {{ {step} }} catch {{ \
             Add-Content -LiteralPath {marker} -Value 'failed' -ErrorAction SilentlyContinue; \
             Add-Content -LiteralPath {log} -Value $_.Exception.Message -ErrorAction SilentlyContinue \
         }}; Start-Process -FilePath {exe} -WindowStyle Hidden",
        marker = ps_quote(marker),
        log = ps_quote(log),
        exe = ps_quote(exe),
    )
}

/// A PowerShell single-quoted literal.
#[cfg_attr(not(windows), allow(dead_code))]
fn ps_quote(path: &Path) -> String {
    ps_literal(path.as_os_str())
}

#[cfg_attr(not(windows), allow(dead_code))]
fn ps_literal(value: &std::ffi::OsStr) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_version_switches_respect_installation_format_before_downloading() {
        let current = version::parse("0.2.1").unwrap();
        let older = Release { tag: "v0.1.0".into(), version: version::parse("0.1.0"), ..Release::default() };
        let newer = Release { tag: "v0.3.0".into(), version: version::parse("0.3.0"), ..Release::default() };
        for install in [Install::Msi("launcher.exe".into()), Install::Package(PackageKind::Rpm, "launcher".into())] {
            assert!(install.validate_release(&older, Some(&current)).is_err());
            assert!(install.validate_release(&newer, Some(&current)).is_ok());
        }
        for install in [
            Install::Portable("launcher.exe".into()),
            Install::MacApp("launcher.app".into()),
            Install::AppImage("launcher.AppImage".into()),
            Install::Package(PackageKind::Deb, "launcher".into()),
        ] {
            assert!(install.validate_release(&older, Some(&current)).is_ok());
            assert!(install.validate_release(&newer, Some(&current)).is_ok());
            assert!(install.validate_release(&Release::default(), Some(&current)).is_err());
            assert!(install.validate_release(&older, None).is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn quiet_msi_self_update_restarts_and_retains_recovery_data_on_failure() {
        // Run the real PowerShell orchestration with a fake installer/relauncher. No installed
        // software or running launcher is changed. The MSI parser has its own Windows test.
        const FAKE: &str = r#"
function Start-Process {
    param($FilePath, $ArgumentList, $WindowStyle, [switch]$Wait, [switch]$PassThru)
    if ($FilePath -like '*msiexec.exe') {
        if ($WindowStyle -ne 'Hidden' -or -not $Wait -or -not $PassThru) { throw 'unexpected installer options' }
        Set-Content -LiteralPath $env:TEST_MSI_ARGUMENTS -Value $ArgumentList
        if ($env:TEST_MSI_CODE -eq 'throw') { throw 'installer could not start' }
        return [pscustomobject]@{ExitCode = [int]$env:TEST_MSI_CODE}
    }
    Set-Content -LiteralPath $env:TEST_RESTARTED -Value $FilePath
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("O'Brien's tools");
        std::fs::create_dir(&root).unwrap();
        for (code, success) in [("0", true), ("3010", true), ("1603", false), ("1602", false), ("throw", false)] {
            let file = root.join("launcher package.msi");
            let marker = root.join("self-update.txt");
            let log = root.join(format!("result-{code}.log"));
            let args = root.join(format!("args-{code}.txt"));
            let restarted = root.join(format!("restart-{code}.txt"));
            std::fs::write(&file, b"not a real installer").unwrap();
            std::fs::write(&marker, b"v0.2.1\n123456\n").unwrap();
            let step = msi_step(&file, &log).unwrap();
            let script = format!("{FAKE}\n{}", windows_script(2_000_000_000, &step, &root.join("launcher.exe"), &marker, &log, 100));
            let mut command = process::command("powershell.exe");
            command
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .env("TEST_MSI_CODE", code)
                .env("TEST_MSI_ARGUMENTS", &args)
                .env("TEST_RESTARTED", &restarted);
            assert!(process::capture_command(command, std::time::Duration::from_secs(10)).is_some(), "helper failed for {code}");
            let arguments = std::fs::read_to_string(args).unwrap();
            assert!(arguments.contains("/qn /norestart"));
            assert!(arguments.contains(&format!("\"{}\"", file.display())));
            assert!(restarted.exists(), "did not relaunch after {code}");
            assert_eq!(file.exists(), !success, "installer retention for {code}");
            let guard = std::fs::read_to_string(&marker).unwrap();
            assert_eq!(guard.lines().nth(2), (!success).then_some("failed"), "update guard for {code}");
            if !success {
                assert!(std::fs::metadata(log).unwrap().len() > 0);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn self_update_never_installs_or_restarts_while_old_launcher_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let changed = dir.path().join("changed.txt");
        let marker = dir.path().join("self-update.txt");
        let log = dir.path().join("self-update.log");
        std::fs::write(&marker, b"v0.2.1\n123456\n").unwrap();
        let mut old =
            process::command("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 10"]).spawn().unwrap();
        let script = windows_script(
            old.id(),
            &format!("Set-Content -LiteralPath {} -Value changed", ps_quote(&changed)),
            &dir.path().join("missing.exe"),
            &marker,
            &log,
            100,
        );
        let result = process::capture("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", &script], std::time::Duration::from_secs(5));
        let still_running = old.try_wait().unwrap().is_none();
        let _ = old.kill();
        let _ = old.wait();
        assert!(result.is_none());
        assert!(still_running);
        assert!(!changed.exists());
        assert_eq!(std::fs::read_to_string(marker).unwrap().lines().nth(2), Some("failed"));
        assert!(std::fs::metadata(log).unwrap().len() > 0);
    }

    #[test]
    fn a_version_just_installed_is_not_offered_again() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let r = Release { tag: "v0.2.0".into(), ..Release::default() };
        assert!(!just_updated_to(&paths, &r));
        crate::paths::write_atomic(&marker(&paths), format!("v0.2.0\n{}\n", crate::now()).as_bytes()).unwrap();
        assert!(just_updated_to(&paths, &r));
        assert!(!just_updated_to(&paths, &Release { tag: "v0.3.0".into(), ..Release::default() }));
        // A day later the guard lapses.
        crate::paths::write_atomic(&marker(&paths), format!("v0.2.0\n{}\n", crate::now() - JUST_UPDATED_FOR - 1).as_bytes()).unwrap();
        assert!(!just_updated_to(&paths, &r));
    }

    #[test]
    fn a_failed_self_update_does_not_restart_in_a_loop_and_can_retry_after_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let release = Release { tag: "v0.2.1".into(), ..Release::default() };
        crate::paths::write_atomic(&marker(&paths), format!("v0.2.1\n{}\nfailed\n", crate::now()).as_bytes()).unwrap();
        assert!(just_updated_to(&paths, &release));
        crate::paths::write_atomic(&marker(&paths), format!("v0.2.1\n{}\nfailed\n", crate::now() - FAILED_UPDATE_RETRY_FOR - 1).as_bytes()).unwrap();
        assert!(!just_updated_to(&paths, &release));
    }

    /// The restart helper doesn't pass a pretend version on to the new launcher.
    #[cfg(unix)]
    #[test]
    fn the_restarted_launcher_does_not_inherit_a_pretend_version() {
        let cmd = helper_command(1, &[std::ffi::OsStr::new("/usr/bin/true")]);
        assert!(cmd.get_envs().any(|(k, v)| k == PRETEND_VAR && v.is_none()), "the helper must remove {PRETEND_VAR}");
    }

    /// The helper waits for the old launcher to exit before it starts the new one.
    #[cfg(unix)]
    #[test]
    fn the_restart_waits_for_the_old_process_to_exit() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started");
        let mut old = std::process::Command::new("/bin/sleep").arg("1").spawn().unwrap();
        relaunch_after_exit(old.id(), &[std::ffi::OsStr::new("/usr/bin/touch"), marker.as_os_str()]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(!marker.exists(), "started while the old launcher was still running");
        old.wait().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(marker.exists(), "never started after the old launcher exited");
    }

    /// A real disk image: the new `ArtCraft Launcher.app` replaces the old one in place.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_new_bundle_replaces_the_running_one() {
        fn bundle(at: &Path, version: &str) {
            let contents = at.join("ArtCraft Launcher.app/Contents");
            std::fs::create_dir_all(contents.join("MacOS")).unwrap();
            std::fs::write(
                contents.join("Info.plist"),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>\
                     <key>CFBundleIdentifier</key><string>ai.storyteller.artcraft-launcher</string>\
                     <key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"
                ),
            )
            .unwrap();
            std::fs::write(contents.join("MacOS/ArtCraft Launcher"), version).unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        bundle(&apps, "0.1.0");
        let src = dir.path().join("src");
        bundle(&src, "0.2.0");
        let dmg = dir.path().join("artcraft-launcher-0.2.0-macos-universal.dmg");
        let one_at_a_time = install::macos::DISK_IMAGES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ok = std::process::Command::new("hdiutil")
            .args(["create", "-quiet", "-format", "UDZO", "-volname", "ArtCraft Launcher 0.2.0", "-srcfolder"])
            .arg(&src)
            .arg(&dmg)
            .status()
            .unwrap()
            .success();
        assert!(ok, "hdiutil create failed");
        drop(one_at_a_time);
        let current = apps.join("ArtCraft Launcher.app");
        let existing = Installed { version: None, source: InstallSource::MacApp, location: current.display().to_string(), package: None };
        // An unsigned test bundle: the signature check has its own tests in `install`.
        let expect = install::macos::Expect { team: None, version: None };
        let placed = install::macos::install_dmg(&LAUNCHER, &dmg, Some(&existing), &Paths::under(dir.path()), &expect).unwrap();
        assert_eq!(placed, current);
        assert_eq!(std::fs::read_to_string(current.join("Contents/MacOS/ArtCraft Launcher")).unwrap(), "0.2.0");
        assert!(!dir.path().join("Applications/.ArtCraft Launcher.app.partial").exists());
    }

    #[test]
    fn powershell_quoting_survives_apostrophes() {
        let p = Path::new(r"C:\Users\O'Brien\AppData\Local\ArtCraft Launcher\x.msi");
        assert_eq!(ps_quote(p), r"'C:\Users\O''Brien\AppData\Local\ArtCraft Launcher\x.msi'");
        assert_eq!(
            ps_literal(std::ffi::OsStr::new(r#""C:\Users\O'Brien\AppData\Local\ArtCraft Launcher\x.msi""#)),
            r#"'"C:\Users\O''Brien\AppData\Local\ArtCraft Launcher\x.msi"'"#
        );
    }

    #[test]
    fn a_development_build_does_not_update_itself() {
        // `cargo test` runs from target/, which no installer puts anywhere.
        assert_eq!(current(), None);
    }

    #[test]
    fn the_launcher_picks_its_own_release_files() {
        let names = [
            "artcraft-launcher-0.2.0-macos-universal.dmg",
            "artcraft-launcher-0.2.0-windows-x64.msi",
            "artcraft-launcher-0.2.0-windows-x64-portable.zip",
            "artcraft-launcher-0.2.0-linux-x86_64.AppImage",
            "artcraft-launcher-0.2.0-linux-x86_64.deb",
            "artcraft-launcher-web-0.2.0.zip",
        ];
        let release = Release {
            assets: names
                .iter()
                .map(|n| artcraft_launcher_core::release::Asset { name: (*n).into(), size: 1, url: String::new(), sha256: None })
                .collect(),
            ..Release::default()
        };
        use artcraft_launcher_core::{Arch, Os};
        let pick = |os, kind| pick_asset(&release, &[LAUNCHER.id], Platform { os, arch: Arch::X64 }, &[kind]).map(|(a, _)| a.name.clone());
        assert_eq!(pick(Os::MacOs, Install::MacApp(PathBuf::new()).kind()).as_deref(), Some(names[0]));
        assert_eq!(pick(Os::Windows, Install::Msi(PathBuf::new()).kind()).as_deref(), Some(names[1]));
        assert_eq!(pick(Os::Windows, Install::Portable(PathBuf::new()).kind()).as_deref(), Some(names[2]));
        assert_eq!(pick(Os::Linux, Install::AppImage(PathBuf::new()).kind()).as_deref(), Some(names[3]));
        assert_eq!(pick(Os::Linux, Install::Package(PackageKind::Deb, PathBuf::new()).kind()).as_deref(), Some(names[4]));
    }
}
