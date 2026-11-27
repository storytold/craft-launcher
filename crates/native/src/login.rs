//! "Start ArtCraft Launcher when I log in".
//!
//! - macOS: a LaunchAgent in `~/Library/LaunchAgents`.
//! - Windows: a value under `HKCU\…\CurrentVersion\Run`.
//! - Linux and FreeBSD: an XDG autostart entry in `~/.config/autostart` (the host's, from the
//!   Flatpak, starting the app through `flatpak run`).
//!
//! The launcher starts hidden in the tray when it's started this way (`--hidden`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg_attr(windows, allow(dead_code))]
const ID: &str = "ai.storyteller.artcraft-launcher";
/// The command-line flag a login start passes.
pub const HIDDEN_FLAG: &str = "--hidden";
/// The command the Flatpak exports (`command:` in `packaging/linux/flatpak/*.yml`).
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
const FLATPAK_COMMAND: &str = "artcraft-launcher";

/// The program to start at login: the AppImage when running from one, else this executable.
/// Windows has no AppImage, so a leftover `$APPIMAGE` is ignored there and the running
/// executable is used instead.
fn program() -> Result<PathBuf, String> {
    program_from(std::env::var_os("APPIMAGE"), std::env::current_exe)
}

fn program_from(appimage: Option<OsString>, current_exe: impl FnOnce() -> std::io::Result<PathBuf>) -> Result<PathBuf, String> {
    #[cfg(not(windows))]
    if let Some(appimage) = appimage {
        return Ok(PathBuf::from(appimage));
    }
    let _ = appimage;
    current_exe().map_err(|e| format!("couldn't find this program ({e})"))
}

#[cfg(target_os = "macos")]
fn agent() -> Result<PathBuf, String> {
    Ok(dirs::home_dir().ok_or("no home folder")?.join("Library/LaunchAgents").join(format!("{ID}.plist")))
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn is_enabled() -> bool {
    #[cfg(target_os = "macos")]
    return agent().is_ok_and(|p| p.exists());
    #[cfg(windows)]
    return windows_run().and_then(|k| k.get_value::<String, _>("ArtCraft Launcher").ok()).is_some();
    #[cfg(not(any(target_os = "macos", windows)))]
    return {
        let flatpak = this_flatpak();
        autostart(flatpak.as_deref()).is_ok_and(|p| is_enabled_at(&p, flatpak.as_deref()))
    };
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let path = agent()?;
        if !on {
            return remove(&path);
        }
        // Inside a bundle, `open -a` starts the app the way Finder does (one instance, Dock icon).
        let exe = program()?;
        let bundle = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(PathBuf::from);
        let args: Vec<String> = match bundle {
            Some(b) => vec!["/usr/bin/open".into(), "-a".into(), b.display().to_string(), "--args".into(), HIDDEN_FLAG.into()],
            None => vec![exe.display().to_string(), HIDDEN_FLAG.into()],
        };
        let args_xml: String = args.iter().map(|a| format!("    <string>{}</string>\n", xml_escape(a))).collect();
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{ID}</string>\n  <key>ProgramArguments</key>\n  <array>\n{args_xml}  </array>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n"
        );
        crate::paths::write_atomic(&path, plist.as_bytes())
    }
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| format!("couldn't change the startup list ({e})"))?;
        if on {
            let value = run_value(&program()?);
            key.set_value("ArtCraft Launcher", &value).map_err(|e| format!("couldn't change the startup list ({e})"))
        } else {
            match key.delete_value("ArtCraft Launcher") {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("couldn't change the startup list ({e})")),
            }
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let flatpak = this_flatpak();
        let path = autostart(flatpak.as_deref())?;
        if !on {
            return remove(&path);
        }
        enable_at(&path, flatpak.as_deref(), program)
    }
}

/// Write the entry at `path`, replacing whatever is there: the host build's entry has the same
/// file name, so a user who moved to the Flatpak gets the Flatpak form.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn enable_at(path: &Path, flatpak: Option<&str>, program: impl FnOnce() -> Result<PathBuf, String>) -> Result<(), String> {
    let entry = match flatpak {
        Some(app_id) => flatpak_autostart_entry(app_id),
        None => autostart_entry(&program()?).ok_or("this program's path can't be used in a startup entry")?,
    };
    crate::paths::write_atomic(path, entry.as_bytes())
}

/// Whether the entry at `path` starts this launcher. In the Flatpak only its own form counts: an
/// entry left by the host build (same file name) starts the host program, which may be gone.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn is_enabled_at(path: &Path, flatpak: Option<&str>) -> bool {
    let Some(app_id) = flatpak else {
        return path.exists();
    };
    use std::io::Read as _;
    // Ours is a few hundred bytes; don't read a large file someone else put there.
    let mut text = String::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(64 * 1024).read_to_string(&mut text));
    read.is_ok() && is_flatpak_entry(&text, app_id)
}

/// Whether `entry` is the Flatpak form for `app_id` (see `flatpak_autostart_entry`) and the
/// desktop will run it: not hidden (a startup manager's "off"), and its Exec starts this app.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn is_flatpak_entry(entry: &str, app_id: &str) -> bool {
    let key = |name| desktop_entry_key(entry, name);
    key("Hidden") != Some("true")
        && key("X-GNOME-Autostart-enabled") != Some("false")
        && key("X-Flatpak") == Some(app_id)
        && key("Exec") == Some(format!("{} {HIDDEN_FLAG}", flatpak_exec(app_id)).as_str())
}

/// The value of `key` in the entry's `[Desktop Entry]` group, as desktop-entry parsers read it:
/// other groups and comments don't count, space around `=` is ignored, and the last value wins.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn desktop_entry_key<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
    let mut in_group = false;
    let mut value = None;
    for line in entry.lines().map(str::trim) {
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
        } else if in_group
            && !line.starts_with('#')
            && let Some((name, v)) = line.split_once('=')
            && name.trim_end() == key
        {
            value = Some(v.trim_start());
        }
    }
    value
}

/// The `Run` value for a launcher at `exe`.
#[cfg_attr(not(windows), allow(dead_code))]
fn run_value(exe: &std::path::Path) -> String {
    format!("\"{}\" {HIDDEN_FLAG}", exe.display())
}

/// The program a `Run` value starts. A quoted path is taken as written. An unquoted value is
/// resolved the way Windows resolves a Run command line: each whitespace-separated prefix is
/// tried in order, and the first path that exists (or cannot be checked) is the program. That
/// keeps `C:\Portable\launcher.exe minimized` and `C:\Builds\ArtCraft - Portable\launcher.exe`
/// intact, and still accepts a path with spaces followed by `--hidden` or `/min`.
#[cfg_attr(not(windows), allow(dead_code))]
fn stored_program(stored: &str, exists: impl Fn(&Path) -> std::io::Result<bool>) -> &str {
    let stored = stored.trim();
    match stored.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => unquoted_program(stored, exists),
    }
}

/// First existing (or uncheckable) prefix of an unquoted command line, or the whole line if none exist.
#[cfg_attr(not(windows), allow(dead_code))]
fn unquoted_program(command: &str, exists: impl Fn(&Path) -> std::io::Result<bool>) -> &str {
    let mut offset = 0;
    for token in command.split_inclusive(|c: char| c.is_ascii_whitespace()) {
        offset += token.len();
        let candidate = command[..offset].trim();
        if candidate.is_empty() {
            continue;
        }
        match exists(Path::new(candidate)) {
            Ok(true) | Err(_) => return candidate,
            Ok(false) => {}
        }
    }
    command
}

/// `text` with each `%NAME%` replaced by `lookup(NAME)`, as Windows does for an expandable (`REG_EXPAND_SZ`) `Run` value.
/// A name `lookup` doesn't know stays as written.
#[cfg_attr(not(windows), allow(dead_code))]
fn expand_env(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => match lookup(&after[..end]).filter(|_| end > 0) {
                Some(value) => {
                    out.push_str(&value);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('%');
                    rest = after;
                }
            },
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `Run` value to write instead of `stored` for a launcher at `exe`, if `stored` starts a program that
/// no longer exists. A value whose program is still there, or whose path cannot be checked, is left
/// alone, so a second copy (portable, dev build) can't take over the entry, and an inaccessible
/// program is not treated as missing.
#[cfg_attr(not(windows), allow(dead_code))]
fn repointed(stored: &str, exe: &std::path::Path, exists: impl Fn(&std::path::Path) -> std::io::Result<bool>) -> Option<String> {
    let value = run_value(exe);
    if value == stored {
        return None;
    }
    match exists(std::path::Path::new(stored_program(stored, &exists))) {
        Ok(false) => Some(value),
        Ok(true) | Err(_) => None,
    }
}

/// Windows: if start at login is on but the `Run` value names a program that no longer exists (the
/// launcher was moved or reinstalled into another folder), rewrite it to this executable, so the next
/// login starts it instead of a missing one. An entry whose program still exists, or that cannot be
/// checked, is not touched. Environment variables in the stored path are expanded first, as for a
/// `REG_EXPAND_SZ` value.
pub fn refresh() {
    #[cfg(windows)]
    {
        let Some(stored) = windows_run().and_then(|k| k.get_value::<String, _>("ArtCraft Launcher").ok()) else {
            return;
        };
        let exe = match program() {
            Ok(exe) => exe,
            Err(e) => {
                log::warn!("start at login not refreshed: {e}");
                return;
            }
        };
        let exists = |p: &std::path::Path| std::path::Path::new(&expand_env(&p.to_string_lossy(), |name| std::env::var(name).ok())).try_exists();
        if let Some(value) = repointed(&stored, &exe, exists) {
            match set_enabled(true) {
                Ok(()) => log::info!("start at login now points at {value} (was {stored})"),
                Err(e) => log::warn!("start at login not refreshed: {e}"),
            }
        }
    }
}

#[cfg(windows)]
fn windows_run() -> Option<winreg::RegKey> {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run").ok()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn autostart(flatpak: Option<&str>) -> Result<PathBuf, String> {
    autostart_path(flatpak, |name| std::env::var_os(name), dirs::home_dir).ok_or_else(|| "no config folder".into())
}

/// The Flatpak app id this process runs as, if it runs in one.
#[cfg(not(any(target_os = "macos", windows)))]
fn this_flatpak() -> Option<String> {
    let env_id = std::env::var("FLATPAK_ID").ok();
    flatpak_app(Path::new("/.flatpak-info").exists(), env_id.as_deref()).map(str::to_owned)
}

/// The Flatpak app id when this runs inside a Flatpak (`flatpak_info`: /.flatpak-info exists),
/// else `None`, whatever $FLATPAK_ID says: a terminal in a Flatpak IDE hands its own id on to
/// programs it starts on the host.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn flatpak_app(flatpak_info: bool, env_id: Option<&str>) -> Option<&str> {
    flatpak_info.then(|| flatpak_id(env_id))
}

/// Where the XDG autostart entry goes: `autostart/` in the user's config folder. Inside the Flatpak
/// (`flatpak` is its app id) that is the host's ($HOST_XDG_CONFIG_HOME, else ~/.config; the
/// manifests' `--filesystem=xdg-config/autostart:create` exposes its `autostart/` at that host
/// path, wherever it is), not the sandbox's own $XDG_CONFIG_HOME (~/.var/app/<id>/config),
/// which the host session never reads (#1204); the file is named after the app id, as the XDG
/// Background portal names it, so a renamed build (….Devel) beside the stable one has its own.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn autostart_path(flatpak: Option<&str>, env: impl Fn(&str) -> Option<OsString>, home: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    let Some(app_id) = flatpak else {
        return Some(dirs::config_dir()?.join("autostart").join(format!("{ID}.desktop")));
    };
    // The XDG spec ignores relative values (`has_root` is `is_absolute` on Unix).
    let config = env("HOST_XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.has_root())
        .or_else(|| home().filter(|p| p.has_root()).map(|h| h.join(".config")))?;
    Some(config.join("autostart").join(format!("{app_id}.desktop")))
}

/// The autostart entry that starts `program` hidden; `None` when its path can't be quoted.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn autostart_entry(program: &Path) -> Option<String> {
    // Quote the executable path per the Desktop Entry spec, rejecting one that can't be
    // represented as a single argument (e.g. a path with a line break, which could otherwise
    // introduce a second Exec= key that the desktop runs at login instead of us) (CWE-74).
    Some(entry_text(&crate::desktop_entry::exec_quote(program)?, ""))
}

/// Inside the Flatpak this program's path (/app/bin/…) exists only in the sandbox, so the entry
/// starts the app through `flatpak run` on the host, as Flatpak's own exported entries do.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn flatpak_autostart_entry(app_id: &str) -> String {
    entry_text(&flatpak_exec(app_id), &format!("X-Flatpak={app_id}\n"))
}

/// The command that starts the Flatpak `app_id` from the host (the entry adds `--hidden`).
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn flatpak_exec(app_id: &str) -> String {
    format!("flatpak run --command={FLATPAK_COMMAND} {app_id}")
}

#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn entry_text(exec: &str, extra: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=ArtCraft Launcher\nComment=Install and update the Craft App Suite\nExec={exec} {HIDDEN_FLAG}\nIcon={ID}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n{extra}"
    )
}

/// The sandbox's app id ($FLATPAK_ID) when it is a valid one (dot-separated letters, digits, `_`
/// and `-`, so it is one plain word of the Exec line), else the launcher's own.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn flatpak_id(from_env: Option<&str>) -> &str {
    let valid = |id: &str| {
        id.len() <= 255
            && id.split('.').count() >= 2
            && id.split('.').all(|part| {
                part.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            })
    };
    from_env.filter(|id| valid(id)).unwrap_or(ID)
}

#[cfg(not(windows))]
fn remove(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("couldn't remove {} ({e})", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRY: &str = "/home/u/.config/autostart/ai.storyteller.artcraft-launcher.desktop";
    const SANDBOX_CONFIG: &str = "/home/u/.var/app/ai.storyteller.artcraft-launcher/config";

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| OsString::from(v))
    }

    fn home() -> Option<PathBuf> {
        Some(PathBuf::from("/home/u"))
    }

    fn exec_line(entry: &str) -> &str {
        entry.lines().find_map(|l| l.strip_prefix("Exec=")).unwrap_or_default()
    }

    /// #1204: the host session never reads the sandbox's own config folder.
    #[test]
    fn flatpak_entry_goes_in_the_hosts_config_folder() {
        let vars = [("HOME", "/home/u"), ("XDG_CONFIG_HOME", SANDBOX_CONFIG)];
        assert_eq!(autostart_path(Some(ID), env(&vars), home), Some(PathBuf::from(ENTRY)));
        // No usable home folder: no entry rather than a relative path.
        assert_eq!(autostart_path(Some(ID), env(&vars), || Some(PathBuf::from("u"))), None);
        assert_eq!(autostart_path(Some(ID), env(&vars), || None), None);
    }

    #[test]
    fn flatpak_entry_follows_host_xdg_config_home() {
        let vars = [("HOME", "/home/u"), ("XDG_CONFIG_HOME", SANDBOX_CONFIG), ("HOST_XDG_CONFIG_HOME", "/home/u/cfg")];
        assert_eq!(autostart_path(Some(ID), env(&vars), home), Some(PathBuf::from("/home/u/cfg/autostart/ai.storyteller.artcraft-launcher.desktop")));
        // A relative one isn't allowed by the XDG spec: ~/.config.
        let vars = [("HOME", "/home/u"), ("XDG_CONFIG_HOME", SANDBOX_CONFIG), ("HOST_XDG_CONFIG_HOME", "cfg")];
        assert_eq!(autostart_path(Some(ID), env(&vars), home), Some(PathBuf::from(ENTRY)));
    }

    /// A renamed build beside the stable one keeps its own entry; outside a Flatpak the name stays.
    #[test]
    fn entry_file_is_named_after_the_flatpak_app_id() {
        let devel = "ai.storyteller.artcraft-launcher.Devel";
        assert_eq!(
            autostart_path(Some(devel), env(&[]), home),
            Some(PathBuf::from("/home/u/.config/autostart/ai.storyteller.artcraft-launcher.Devel.desktop"))
        );
        let host = autostart_path(None, env(&[("FLATPAK_ID", devel)]), home);
        assert_eq!(host, dirs::config_dir().map(|c| c.join("autostart").join("ai.storyteller.artcraft-launcher.desktop")));
    }

    /// An entry left by the host build doesn't count in the Flatpak, and turning the setting on
    /// replaces it with the Flatpak form.
    #[test]
    fn flatpak_replaces_the_host_builds_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autostart").join(format!("{ID}.desktop"));
        assert!(!is_enabled_at(&path, None));
        assert!(!is_enabled_at(&path, Some(ID)));

        enable_at(&path, None, || Ok(PathBuf::from("/usr/bin/artcraft-launcher"))).unwrap();
        assert!(is_enabled_at(&path, None));
        assert!(!is_enabled_at(&path, Some(ID)));

        enable_at(&path, Some(ID), || Err("the Flatpak form doesn't name this program".into())).unwrap();
        assert!(is_enabled_at(&path, Some(ID)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), flatpak_autostart_entry(ID));
    }

    #[test]
    fn flatpak_entry_is_recognised_only_for_its_own_app_id() {
        let entry = flatpak_autostart_entry(ID);
        assert!(is_flatpak_entry(&entry, ID));
        assert!(is_flatpak_entry(&entry.replace('\n', " \r\n"), ID));
        assert!(!is_flatpak_entry(&entry, "ai.storyteller.artcraft-launcher.Devel"));
        assert!(!is_flatpak_entry(&autostart_entry(Path::new("/usr/bin/artcraft-launcher")).unwrap(), ID));
        // Flatpak-looking id but a host Exec line: not ours.
        let host_exec = entry.replace("Exec=flatpak run --command=artcraft-launcher", "Exec=/usr/bin/artcraft-launcher");
        assert!(!is_flatpak_entry(&host_exec, ID));
    }

    /// A startup manager turns an entry off by hiding it; the desktop then skips it.
    #[test]
    fn hidden_flatpak_entry_is_off() {
        let entry = flatpak_autostart_entry(ID);
        assert!(!is_flatpak_entry(&format!("{entry}Hidden=true\n"), ID));
        assert!(!is_flatpak_entry(&format!("{entry}Hidden = true\n"), ID));
        assert!(!is_flatpak_entry(&entry.replace("X-GNOME-Autostart-enabled=true", "X-GNOME-Autostart-enabled=false"), ID));
        assert!(is_flatpak_entry(&format!("{entry}Hidden=false\n"), ID));
        assert!(is_flatpak_entry(&format!("{entry}#Hidden=true\n"), ID));
        // The last value is the one that counts.
        assert!(is_flatpak_entry(&format!("{entry}Hidden=true\nHidden=false\n"), ID));
    }

    /// Our X-Flatpak isn't enough: the Exec has to start this app, hidden.
    #[test]
    fn flatpak_entry_must_start_this_app() {
        let entry = flatpak_autostart_entry(ID);
        let other = entry.replace(
            "Exec=flatpak run --command=artcraft-launcher ai.storyteller.artcraft-launcher --hidden",
            "Exec=flatpak run --command=other com.example.Other --hidden",
        );
        assert!(!is_flatpak_entry(&other, ID));
        assert!(!is_flatpak_entry(&entry.replace(" --hidden", ""), ID));
        assert!(!is_flatpak_entry(&format!("{entry}Exec=flatpak run com.example.Other --hidden\n"), ID));
    }

    /// Only the [Desktop Entry] group's keys count.
    #[test]
    fn keys_in_other_groups_dont_count() {
        let flatpak = flatpak_autostart_entry(ID);
        let in_action = format!(
            "{}[Desktop Action flatpak]\n{}",
            autostart_entry(Path::new("/usr/bin/artcraft-launcher")).unwrap(),
            flatpak.replace("[Desktop Entry]\n", "")
        );
        assert!(!is_flatpak_entry(&in_action, ID));
        assert!(is_flatpak_entry(&format!("{flatpak}[X-Other]\nHidden=true\nExec=/usr/bin/other\n"), ID));
        assert!(!is_flatpak_entry(&format!("[X-Other]\n{}", flatpak.replace("[Desktop Entry]\n", "")), ID));
        assert_eq!(desktop_entry_key("[A]\nExec=a\n[Desktop Entry]\n# Exec=b\nExec[de]=c\n Exec = d \n", "Exec"), Some("d"));
    }

    /// Only a real sandbox (/.flatpak-info) counts: a $FLATPAK_ID inherited on the host, e.g. from a
    /// terminal in a Flatpak IDE, must not give `flatpak run com.example.Ide`.
    #[test]
    fn flatpak_form_needs_flatpak_info() {
        assert_eq!(flatpak_app(false, Some("com.example.Ide")), None);
        assert_eq!(flatpak_app(false, None), None);
        assert_eq!(flatpak_app(true, Some("ai.storyteller.artcraft-launcher.Devel")), Some("ai.storyteller.artcraft-launcher.Devel"));
        assert_eq!(flatpak_app(true, None), Some(ID));
    }

    /// #1204: /app/bin/artcraft-launcher exists only inside the sandbox.
    #[test]
    fn flatpak_entry_starts_the_app_through_flatpak_run() {
        let entry = flatpak_autostart_entry("ai.storyteller.artcraft-launcher");
        let exec = exec_line(&entry);
        assert!(exec.starts_with("flatpak run "), "{exec}");
        assert!(exec.ends_with(" --hidden"), "{exec}");
        assert_eq!(exec, "flatpak run --command=artcraft-launcher ai.storyteller.artcraft-launcher --hidden");
        assert!(entry.contains("\nX-Flatpak=ai.storyteller.artcraft-launcher\n"), "{entry}");
        assert!(!entry.contains("/app/"), "{entry}");
    }

    #[test]
    fn flatpak_id_is_the_sandboxes_when_it_is_a_valid_one() {
        assert_eq!(flatpak_id(Some("ai.storyteller.artcraft-launcher.Devel")), "ai.storyteller.artcraft-launcher.Devel");
        for bad in [None, Some(""), Some("-a.b"), Some("a b.c"), Some("a.b\nExec=evil"), Some("a.b%u"), Some("nodot")] {
            assert_eq!(flatpak_id(bad), ID, "{bad:?}");
        }
    }

    #[test]
    fn host_entry_is_unchanged() {
        assert_eq!(
            autostart_entry(Path::new("/usr/bin/artcraft-launcher")).as_deref(),
            Some(
                "[Desktop Entry]\nType=Application\nName=ArtCraft Launcher\nComment=Install and update the Craft App Suite\nExec=\"/usr/bin/artcraft-launcher\" --hidden\nIcon=ai.storyteller.artcraft-launcher\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
            )
        );
    }

    #[test]
    fn run_value_quotes_the_path_and_starts_hidden() {
        assert_eq!(run_value(Path::new("C:/Program Files/ArtCraft/launcher.exe")), "\"C:/Program Files/ArtCraft/launcher.exe\" --hidden");
    }

    #[test]
    fn a_run_value_for_a_missing_program_is_repointed_at_this_one() {
        let new = Path::new("C:/New/launcher.exe");
        let old = run_value(Path::new("C:/Old/launcher.exe"));
        assert_eq!(repointed(&old, new, |_| Ok(false)), Some("\"C:/New/launcher.exe\" --hidden".to_string()));
        assert_eq!(repointed(&run_value(new), new, |_| Ok(false)), None);
    }

    #[test]
    fn a_run_value_for_another_program_that_still_exists_is_left_alone() {
        let other = run_value(Path::new("C:/Program Files/ArtCraft/launcher.exe"));
        let existing = Path::new("C:/Program Files/ArtCraft/launcher.exe");
        assert_eq!(repointed(&other, Path::new("D:/Portable/launcher.exe"), |p| Ok(p == existing)), None);
    }

    #[test]
    fn a_run_value_with_other_arguments_is_left_alone_when_the_program_exists() {
        let existing = Path::new("C:/Portable/launcher.exe");
        let exists = |p: &Path| Ok(p == existing);
        let other = Path::new("D:/Dev/launcher.exe");
        assert_eq!(repointed("C:/Portable/launcher.exe --minimized", other, exists), None);
        assert_eq!(repointed("\"C:/Portable/launcher.exe\" --minimized", other, exists), None);
    }

    #[test]
    fn a_run_value_with_other_arguments_is_repointed_when_the_program_is_gone() {
        let new = Path::new("C:/New/launcher.exe");
        assert_eq!(repointed("C:/Portable/launcher.exe --minimized", new, |_| Ok(false)), Some("\"C:/New/launcher.exe\" --hidden".to_string()));
    }

    #[test]
    fn a_run_value_with_a_positional_argument_is_left_alone_when_the_program_exists() {
        let existing = Path::new("C:/Portable/launcher.exe");
        let exists = |p: &Path| Ok(p == existing);
        let other = Path::new("D:/Dev/launcher.exe");
        assert_eq!(repointed("C:/Portable/launcher.exe minimized", other, exists), None);
    }

    #[test]
    fn a_run_value_with_a_positional_argument_is_repointed_when_the_program_is_gone() {
        let new = Path::new("C:/New/launcher.exe");
        assert_eq!(repointed("C:/Portable/launcher.exe minimized", new, |_| Ok(false)), Some("\"C:/New/launcher.exe\" --hidden".to_string()));
    }

    #[test]
    fn a_run_value_whose_folder_name_contains_a_spaced_dash_is_left_alone() {
        let existing = Path::new("C:/Builds/ArtCraft - Portable/launcher.exe");
        let exists = |p: &Path| Ok(p == existing);
        let other = Path::new("D:/Dev/launcher.exe");
        assert_eq!(repointed("C:/Builds/ArtCraft - Portable/launcher.exe", other, exists), None);
        assert_eq!(repointed("C:/Builds/ArtCraft - Portable/launcher.exe --hidden", other, exists), None);
    }

    #[test]
    fn a_run_value_whose_program_cannot_be_checked_is_left_alone() {
        let denied = |_: &Path| Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"));
        let stored = run_value(Path::new("C:/Protected/launcher.exe"));
        assert_eq!(repointed(&stored, Path::new("D:/Portable/launcher.exe"), denied), None);
    }

    #[test]
    fn expand_env_replaces_known_names_and_keeps_unknown_ones() {
        let lookup = |name: &str| (name == "LOCALAPPDATA").then(|| "C:/Users/a/AppData/Local".to_string());
        assert_eq!(expand_env("%LOCALAPPDATA%/ArtCraft/launcher.exe", lookup), "C:/Users/a/AppData/Local/ArtCraft/launcher.exe");
        assert_eq!(expand_env("%NOPE%/launcher.exe", lookup), "%NOPE%/launcher.exe");
        assert_eq!(expand_env("100%/x", lookup), "100%/x");
    }

    #[test]
    fn an_expandable_run_value_whose_program_exists_is_left_alone() {
        let lookup = |name: &str| (name == "LOCALAPPDATA").then(|| "C:/Users/a/AppData/Local".to_string());
        let existing = Path::new("C:/Users/a/AppData/Local/ArtCraft/launcher.exe");
        let exists = |p: &Path| Ok(p.to_str().is_some_and(|t| Path::new(&expand_env(t, lookup)) == existing));
        let stored = "\"%LOCALAPPDATA%/ArtCraft/launcher.exe\" --hidden";
        assert_eq!(repointed(stored, Path::new("D:/Portable/launcher.exe"), exists), None);
    }

    /// If we forgot to expand, the unexpanded `%LOCALAPPDATA%...` path would be treated as existing
    /// here and the stale entry would be kept.
    #[test]
    fn an_expandable_run_value_whose_program_is_gone_is_repointed() {
        let lookup = |name: &str| (name == "LOCALAPPDATA").then(|| "C:/Users/a/AppData/Local".to_string());
        let unexpanded = Path::new("%LOCALAPPDATA%/ArtCraft/launcher.exe");
        let exists = |p: &Path| {
            let expanded = PathBuf::from(expand_env(&p.to_string_lossy(), lookup));
            Ok(expanded.as_path() == unexpanded)
        };
        let stored = "\"%LOCALAPPDATA%/ArtCraft/launcher.exe\" --hidden";
        assert_eq!(repointed(stored, Path::new("D:/Portable/launcher.exe"), exists), Some("\"D:/Portable/launcher.exe\" --hidden".to_string()));
    }

    #[test]
    fn an_expandable_run_value_with_other_arguments_whose_program_exists_is_left_alone() {
        let lookup = |name: &str| (name == "LOCALAPPDATA").then(|| "C:/Users/a/AppData/Local".to_string());
        let existing = Path::new("C:/Users/a/AppData/Local/ArtCraft/launcher.exe");
        let exists = |p: &Path| Ok(p.to_str().is_some_and(|t| Path::new(&expand_env(t, lookup)) == existing));
        let stored = "\"%LOCALAPPDATA%/ArtCraft/launcher.exe\" --minimized";
        assert_eq!(repointed(stored, Path::new("D:/Portable/launcher.exe"), exists), None);
    }

    #[test]
    fn stored_program_reads_quoted_and_unquoted_values() {
        let exists = |want: &'static str| move |p: &Path| Ok(p == Path::new(want));
        assert_eq!(
            stored_program("\"C:/Program Files/A/launcher.exe\" --hidden", exists("C:/Program Files/A/launcher.exe")),
            "C:/Program Files/A/launcher.exe"
        );
        assert_eq!(stored_program("C:/A/launcher.exe --hidden", exists("C:/A/launcher.exe")), "C:/A/launcher.exe");
        assert_eq!(stored_program("C:/A/launcher.exe", exists("C:/A/launcher.exe")), "C:/A/launcher.exe");
        assert_eq!(stored_program("C:/Portable/launcher.exe --minimized", exists("C:/Portable/launcher.exe")), "C:/Portable/launcher.exe");
        assert_eq!(stored_program("\"C:/Portable/launcher.exe\" --minimized", exists("C:/Portable/launcher.exe")), "C:/Portable/launcher.exe");
        assert_eq!(
            stored_program("C:/Program Files/A/launcher.exe --hidden", exists("C:/Program Files/A/launcher.exe")),
            "C:/Program Files/A/launcher.exe"
        );
        assert_eq!(stored_program("C:/Portable/launcher.exe /min", exists("C:/Portable/launcher.exe")), "C:/Portable/launcher.exe");
        assert_eq!(stored_program("C:/Portable/launcher.exe minimized", exists("C:/Portable/launcher.exe")), "C:/Portable/launcher.exe");
        assert_eq!(
            stored_program("C:/Builds/ArtCraft - Portable/launcher.exe", exists("C:/Builds/ArtCraft - Portable/launcher.exe")),
            "C:/Builds/ArtCraft - Portable/launcher.exe"
        );
        assert_eq!(
            stored_program("C:/Builds/ArtCraft - Portable/launcher.exe --hidden", exists("C:/Builds/ArtCraft - Portable/launcher.exe")),
            "C:/Builds/ArtCraft - Portable/launcher.exe"
        );
    }

    #[test]
    fn program_from_ignores_appimage_on_windows_only() {
        let app = OsString::from("/tmp/ArtCraft.AppImage");
        let exe = PathBuf::from("C:/ArtCraft/launcher.exe");
        let got = program_from(Some(app.clone()), || Ok(exe.clone())).unwrap();
        if cfg!(windows) {
            assert_eq!(got, exe);
        } else {
            assert_eq!(got, PathBuf::from(app));
        }
    }

    #[test]
    fn program_from_uses_the_running_executable_when_appimage_is_absent() {
        let exe = PathBuf::from("C:/ArtCraft/launcher.exe");
        assert_eq!(program_from(None, || Ok(exe.clone())).unwrap(), exe);
    }
}
