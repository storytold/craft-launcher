//! The update service's settings, which only an administrator can change.
//!
//! They live in the registry under HKLM, where only administrators can write, read first from the
//! Group Policy key and then from the one the service's installer writes (its MSI takes them as
//! properties, `AUTOUPDATE=1`, `CHECKINTERVALHOURS=24`, …, and keeps them on upgrades):
//!
//! - `HKLM\SOFTWARE\Policies\Learning Machines LLC\ArtCraft Launcher Updater`
//! - `HKLM\SOFTWARE\Learning Machines LLC\ArtCraft Launcher Updater`
//!
//! | Value (DWORD, or a string of digits) | Default | Meaning |
//! |---|---|---|
//! | `AutoUpdate` | 0 | 1: install updates in the background as they come out. 0: only when the launcher or the command line asks. |
//! | `AllowRequests` | 1 | Users (through the launcher or the command line) may ask for a check or an update. 0: the service keeps to its schedule. |
//! | `CheckIntervalHours` | 6 | Hours between scheduled checks (1 to 168). |
//! | `Prereleases` | 0 | Offer pre-releases as updates (read when the service starts). |
//!
//! Anything missing, unreadable or out of range falls back to the default. Elsewhere than
//! Windows (where the service doesn't run) the defaults apply.
//!
//! `AutoUpdate` is the computer's own "Automatically download and install updates": ArtCraft
//! Launcher shows it in place of the user's setting while the service is installed. Changing it
//! there runs `artcraft-launcher-updater set auto-update on|off` as an administrator
//! ([`set_auto_update_elevated`]), so Windows asks for permission (UAC) and a user without
//! administrator rights can't turn background installs on or off. Set by Group Policy, it can't be
//! changed from the launcher at all.

use std::path::PathBuf;

use artcraft_launcher_core::protocol::MachineAutoUpdate;

/// The update service's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Install updates in the background as they come out.
    pub auto_update: bool,
    /// `auto_update` comes from Group Policy: it can't be changed from the launcher.
    pub auto_update_managed: bool,
    pub allow_requests: bool,
    pub interval_hours: u32,
    pub prereleases: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self { auto_update: false, auto_update_managed: false, allow_requests: true, interval_hours: 6, prereleases: false }
    }
}

/// The shortest and the longest time between scheduled checks.
const INTERVAL_HOURS: std::ops::RangeInclusive<u32> = 1..=168;

impl Policy {
    /// The settings as they stand now.
    pub fn load() -> Self {
        #[cfg(windows)]
        {
            Self::from_values(windows::value)
        }
        #[cfg(not(windows))]
        {
            Self::default()
        }
    }

    /// The settings from `value`: the number stored under a name, and whether Group Policy set it
    /// (`None` when missing or unreadable).
    pub fn from_values(value: impl Fn(&str) -> Option<(u32, bool)>) -> Self {
        let defaults = Self::default();
        let flag = |name: &str, default: bool| match value(name).map(|(v, _)| v) {
            Some(0) => false,
            Some(1) => true,
            _ => default,
        };
        let auto_update = value("AutoUpdate").filter(|(v, _)| *v <= 1);
        Self {
            auto_update: auto_update.map_or(defaults.auto_update, |(v, _)| v == 1),
            auto_update_managed: auto_update.is_some_and(|(_, by_policy)| by_policy),
            allow_requests: flag("AllowRequests", defaults.allow_requests),
            interval_hours: value("CheckIntervalHours").map(|(v, _)| v).filter(|h| INTERVAL_HOURS.contains(h)).unwrap_or(defaults.interval_hours),
            prereleases: flag("Prereleases", defaults.prereleases),
        }
    }
}

/// The updater's program where the service's installer put it: `None` when the service isn't
/// installed.
pub fn updater_exe() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let dir = windows::text(windows::INSTALLER_KEY, "InstallDir")?;
        let exe = PathBuf::from(dir.trim()).join("artcraft-launcher-updater.exe");
        exe.is_file().then_some(exe)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The service's "install updates automatically", while the service is installed.
pub fn machine_auto_update() -> Option<MachineAutoUpdate> {
    updater_exe()?;
    let policy = Policy::load();
    Some(MachineAutoUpdate { on: policy.auto_update, managed: policy.auto_update_managed })
}

/// Turns the service's background installs on or off, in the installer's key. Only an
/// administrator (an elevated process) can; refused while Group Policy sets it.
pub fn set_auto_update(on: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        if windows::number(windows::POLICY_KEY, "AutoUpdate").is_some() {
            return Err("Group Policy sets this on this computer, so it can't be changed here".into());
        }
        windows::write(windows::INSTALLER_KEY, "AutoUpdate", if on { "1" } else { "0" })
    }
    #[cfg(not(windows))]
    {
        let _ = on;
        Err("the update service runs on Windows only".into())
    }
}

/// Where [`ELEVATE`] finds the program to run and the value to give it.
#[cfg_attr(not(windows), allow(dead_code))]
const EXE_VAR: &str = "ARTCRAFT_LAUNCHER_UPDATER";
#[cfg_attr(not(windows), allow(dead_code))]
const VALUE_VAR: &str = "ARTCRAFT_LAUNCHER_AUTO_UPDATE";
/// `ERROR_CANCELLED`: the permission prompt was dismissed.
#[cfg_attr(not(windows), allow(dead_code))]
const CANCELLED: i32 = 1223;

/// The PowerShell behind [`set_auto_update_elevated`]: starts the updater with the `runas` verb,
/// which shows the UAC prompt, waits for it and exits with its exit code (or `ERROR_CANCELLED`
/// when the prompt was dismissed). The program and the value reach it through the environment,
/// never the script text.
#[cfg_attr(not(windows), allow(dead_code))]
const ELEVATE: &str = r#"
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $env:ARTCRAFT_LAUNCHER_UPDATER
$psi.Arguments = 'set auto-update ' + $env:ARTCRAFT_LAUNCHER_AUTO_UPDATE
$psi.Verb = 'runas'
$psi.UseShellExecute = $true
$psi.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
try {
  $p = [System.Diagnostics.Process]::Start($psi)
  if (-not $p) { exit 3 }
  $p.WaitForExit()
  exit $p.ExitCode
} catch {
  $e = $_.Exception
  while ($e -and -not ($e -is [System.ComponentModel.Win32Exception])) { $e = $e.InnerException }
  if ($e) { exit $e.NativeErrorCode }
  exit 2
}
"#;

/// Asks Windows to run `artcraft-launcher-updater set auto-update on|off` as an administrator:
/// the UAC prompt (with an administrator's name and password for a user who isn't one). Waits for
/// it. `Ok(true)` once it's changed, `Ok(false)` when the prompt was dismissed.
pub fn set_auto_update_elevated(on: bool) -> Result<bool, String> {
    #[cfg(windows)]
    {
        let exe = updater_exe().ok_or("the update service isn't installed")?;
        let powershell = crate::install::windows::system32().join(r"WindowsPowerShell\v1.0\powershell.exe");
        let mut cmd = crate::process::command(&powershell);
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &crate::msi::encode_command(ELEVATE)])
            .env(EXE_VAR, &exe)
            .env(VALUE_VAR, if on { "on" } else { "off" })
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let status = cmd.status().map_err(|e| format!("couldn't ask for administrator permission ({e})"))?;
        match status.code() {
            Some(0) => Ok(true),
            Some(CANCELLED) => Ok(false),
            Some(code) => Err(format!("automatic updates weren't changed (error {code})")),
            None => Err("automatic updates weren't changed".into()),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = on;
        Err("the update service runs on Windows only".into())
    }
}

#[cfg(windows)]
mod windows {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

    pub const POLICY_KEY: &str = r"SOFTWARE\Policies\Learning Machines LLC\ArtCraft Launcher Updater";
    pub const INSTALLER_KEY: &str = r"SOFTWARE\Learning Machines LLC\ArtCraft Launcher Updater";

    /// A number from Group Policy's key, else the installer's, and whether it was Group Policy's.
    pub fn value(name: &str) -> Option<(u32, bool)> {
        number(POLICY_KEY, name).map(|v| (v, true)).or_else(|| number(INSTALLER_KEY, name).map(|v| (v, false)))
    }

    /// A number stored under `name` in `path`, in either registry view (the x86 package writes its
    /// settings to the 32-bit view of a 64-bit Windows, the others to the 64-bit one). Group
    /// Policy writes DWORDs; the installer and `set` write strings, which the installer reads back
    /// on an upgrade.
    pub fn number(path: &str, name: &str) -> Option<u32> {
        views(path).find_map(|key| key.get_value::<u32, _>(name).ok().or_else(|| key.get_value::<String, _>(name).ok()?.trim().parse().ok()))
    }

    /// A string stored under `name` in `path`, in either registry view.
    pub fn text(path: &str, name: &str) -> Option<String> {
        views(path).find_map(|key| key.get_value::<String, _>(name).ok())
    }

    fn views(path: &str) -> impl Iterator<Item = RegKey> + '_ {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        [KEY_WOW64_64KEY, KEY_WOW64_32KEY].into_iter().filter_map(move |view| hklm.open_subkey_with_flags(path, KEY_READ | view).ok())
    }

    /// Stores `value` under `name` in `path`, in this program's own registry view (which is the
    /// view its installer wrote, since both are built for the same processor).
    pub fn write(path: &str, name: &str, value: &str) -> Result<(), String> {
        let denied = |e: std::io::Error| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                "changing this needs administrator rights".to_string()
            } else {
                format!("couldn't change the setting ({e})")
            }
        };
        let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE).create_subkey(path).map_err(denied)?;
        key.set_value(name, &value.to_string()).map_err(denied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings from the installer's key, then Group Policy's on top.
    fn policy(installer: &[(&str, u32)], group_policy: &[(&str, u32)]) -> Policy {
        Policy::from_values(|name| {
            let find = |values: &[(&str, u32)]| values.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
            find(group_policy).map(|v| (v, true)).or_else(|| find(installer).map(|v| (v, false)))
        })
    }

    #[test]
    fn nothing_set_means_the_defaults() {
        assert_eq!(policy(&[], &[]), Policy::default());
        let d = Policy::default();
        assert!(!d.auto_update && !d.auto_update_managed, "background installs wait for an administrator");
        assert_eq!((d.allow_requests, d.interval_hours, d.prereleases), (true, 6, false));
    }

    #[test]
    fn an_administrator_sets_each_part() {
        let p = policy(&[("AutoUpdate", 1), ("AllowRequests", 0), ("CheckIntervalHours", 24), ("Prereleases", 1)], &[]);
        assert_eq!(p, Policy { auto_update: true, auto_update_managed: false, allow_requests: false, interval_hours: 24, prereleases: true });
    }

    #[test]
    fn group_policy_wins_and_locks_the_setting() {
        let p = policy(&[("AutoUpdate", 1)], &[("AutoUpdate", 0)]);
        assert!(!p.auto_update && p.auto_update_managed);
        let p = policy(&[], &[("AutoUpdate", 1)]);
        assert!(p.auto_update && p.auto_update_managed);
        // Group Policy that sets something else leaves this one to the administrator.
        let p = policy(&[("AutoUpdate", 1)], &[("CheckIntervalHours", 12)]);
        assert!(p.auto_update && !p.auto_update_managed && p.interval_hours == 12);
    }

    #[test]
    fn nonsense_falls_back_to_the_defaults() {
        let p = policy(&[("AutoUpdate", 7), ("CheckIntervalHours", 0)], &[]);
        assert_eq!(p, Policy::default());
        assert_eq!(policy(&[("CheckIntervalHours", 169)], &[]).interval_hours, 6);
        assert_eq!(policy(&[("CheckIntervalHours", 168)], &[]).interval_hours, 168);
        assert_eq!(policy(&[("CheckIntervalHours", 1)], &[]).interval_hours, 1);
    }

    #[cfg(not(windows))]
    #[test]
    fn there_is_no_service_to_set_elsewhere() {
        assert_eq!(updater_exe(), None);
        assert_eq!(machine_auto_update(), None);
        assert!(set_auto_update(true).is_err() && set_auto_update_elevated(true).is_err());
    }
}
