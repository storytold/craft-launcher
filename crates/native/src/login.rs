//! "Start ArtCraft Launcher when I log in".
//!
//! - macOS: a LaunchAgent in `~/Library/LaunchAgents`.
//! - Windows: a value under `HKCU\…\CurrentVersion\Run`.
//! - Linux and FreeBSD: an XDG autostart entry in `~/.config/autostart`.
//!
//! The launcher starts hidden in the tray when it's started this way (`--hidden`).

use std::path::PathBuf;

#[cfg_attr(windows, allow(dead_code))]
const ID: &str = "ai.storyteller.artcraft-launcher";
/// The command-line flag a login start passes.
pub const HIDDEN_FLAG: &str = "--hidden";

/// The program to start at login: the AppImage when running from one, else this executable.
fn program() -> Result<PathBuf, String> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Ok(PathBuf::from(appimage));
    }
    std::env::current_exe().map_err(|e| format!("couldn't find this program ({e})"))
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
    return autostart().is_ok_and(|p| p.exists());
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
        let path = autostart()?;
        if !on {
            return remove(&path);
        }
        let exe = program()?.display().to_string().replace('"', "\\\"");
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=ArtCraft Launcher\nComment=Install and update the Craft App Suite\nExec=\"{exe}\" {HIDDEN_FLAG}\nIcon={ID}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        );
        crate::paths::write_atomic(&path, entry.as_bytes())
    }
}

/// The `Run` value for a launcher at `exe`.
#[cfg_attr(not(windows), allow(dead_code))]
fn run_value(exe: &std::path::Path) -> String {
    format!("\"{}\" {HIDDEN_FLAG}", exe.display())
}

/// The program a `Run` value starts: the quoted path, or the text before `--hidden` when it isn't quoted.
#[cfg_attr(not(windows), allow(dead_code))]
fn stored_program(stored: &str) -> &str {
    let stored = stored.trim();
    match stored.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => stored.strip_suffix(HIDDEN_FLAG).unwrap_or(stored).trim(),
    }
}

/// The `Run` value to write instead of `stored` for a launcher at `exe`, if `stored` starts a program that
/// no longer exists. A value whose program is still there is left alone, so a second copy (portable, dev
/// build) can't take over the entry.
#[cfg_attr(not(windows), allow(dead_code))]
fn repointed(stored: &str, exe: &std::path::Path, exists: impl Fn(&std::path::Path) -> bool) -> Option<String> {
    let value = run_value(exe);
    (value != stored && !exists(std::path::Path::new(stored_program(stored)))).then_some(value)
}

/// Windows: if start at login is on but the `Run` value names a program that no longer exists (the
/// launcher was moved or reinstalled into another folder), rewrite it to this executable, so the next
/// login starts it instead of a missing one. An entry whose program still exists is not touched.
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
        if let Some(value) = repointed(&stored, &exe, |p| p.exists()) {
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
fn autostart() -> Result<PathBuf, String> {
    Ok(dirs::config_dir().ok_or("no config folder")?.join("autostart").join(format!("{ID}.desktop")))
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
    use std::path::Path;

    #[test]
    fn run_value_quotes_the_path_and_starts_hidden() {
        assert_eq!(run_value(Path::new("C:/Program Files/ArtCraft/launcher.exe")), "\"C:/Program Files/ArtCraft/launcher.exe\" --hidden");
    }

    #[test]
    fn a_run_value_for_a_missing_program_is_repointed_at_this_one() {
        let new = Path::new("C:/New/launcher.exe");
        let old = run_value(Path::new("C:/Old/launcher.exe"));
        assert_eq!(repointed(&old, new, |_| false), Some("\"C:/New/launcher.exe\" --hidden".to_string()));
        assert_eq!(repointed(&run_value(new), new, |_| false), None);
    }

    #[test]
    fn a_run_value_for_another_program_that_still_exists_is_left_alone() {
        let other = run_value(Path::new("C:/Program Files/ArtCraft/launcher.exe"));
        let existing = Path::new("C:/Program Files/ArtCraft/launcher.exe");
        assert_eq!(repointed(&other, Path::new("D:/Portable/launcher.exe"), |p| p == existing), None);
    }

    #[test]
    fn stored_program_reads_quoted_and_unquoted_values() {
        assert_eq!(stored_program("\"C:/Program Files/A/launcher.exe\" --hidden"), "C:/Program Files/A/launcher.exe");
        assert_eq!(stored_program("C:/A/launcher.exe --hidden"), "C:/A/launcher.exe");
        assert_eq!(stored_program("C:/A/launcher.exe"), "C:/A/launcher.exe");
    }
}
