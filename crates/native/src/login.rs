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
            let value = format!("\"{}\" {HIDDEN_FLAG}", program()?.display());
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
        // Quote the executable path per the Desktop Entry spec, rejecting one that can't be
        // represented as a single argument (e.g. a path with a line break, which could otherwise
        // introduce a second Exec= key that the desktop runs at login instead of us) (CWE-74).
        let exec = crate::desktop_entry::exec_quote(&program()?).ok_or("this program's path can't be used in a startup entry")?;
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=ArtCraft Launcher\nComment=Install and update the Craft App Suite\nExec={exec} {HIDDEN_FLAG}\nIcon={ID}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        );
        crate::paths::write_atomic(&path, entry.as_bytes())
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
