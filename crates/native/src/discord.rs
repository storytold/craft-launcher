//! Whether Discord is installed, so the community link opens the app instead of the browser.

#[cfg(any(target_os = "linux", target_os = "macos", test))]
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use crate::process::{capture, in_sandbox};

static DISCORD_HANDLER_DETECTED: AtomicBool = AtomicBool::new(false);
static DETECTION_STARTED: AtomicBool = AtomicBool::new(false);

/// Starts asynchronous detection of a registered Discord URL scheme handler in the background.
pub fn ensure_discord_detection_started() {
    if !DETECTION_STARTED.swap(true, Ordering::Relaxed) {
        let _ = std::thread::Builder::new().name("discord-detect".into()).spawn(|| {
            let detected = detect_discord_scheme_handler();
            DISCORD_HANDLER_DETECTED.store(detected, Ordering::Relaxed);
        });
    }
}

/// Whether a registered Discord scheme handler has been detected on this machine.
///
/// Triggers background detection if not yet started, and returns immediately without
/// blocking the calling thread.
pub fn is_discord_installed() -> bool {
    ensure_discord_detection_started();
    DISCORD_HANDLER_DETECTED.load(Ordering::Relaxed)
}

/// The URL to use for Discord links: deep-links to the app if a handler is registered,
/// else the web invite. Non-blocking (detection runs in the background).
pub fn discord_url() -> &'static str {
    if is_discord_installed() { artcraft_launcher_core::catalog::DISCORD_APP_URL } else { artcraft_launcher_core::catalog::DISCORD_URL }
}

#[cfg(windows)]
fn detect_discord_scheme_handler() -> bool {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};

    for hive in [HKEY_CURRENT_USER, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE] {
        let subkey = if hive == HKEY_CLASSES_ROOT { r"discord\shell\open\command" } else { r"Software\Classes\discord\shell\open\command" };
        if let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(subkey, KEY_READ)
            && let Ok(val) = key.get_value::<String, _>("")
            && !val.trim().is_empty()
        {
            return true;
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn detect_discord_scheme_handler() -> bool {
    macos_discord_candidates(dirs::home_dir().as_deref()).iter().any(|app_dir| {
        let plist_path = app_dir.join("Contents/Info.plist");
        if let Ok(content) = std::fs::read_to_string(&plist_path) { content.contains("<string>discord</string>") } else { false }
    })
}

#[cfg(any(target_os = "macos", test))]
fn macos_discord_candidates(home: Option<&Path>) -> Vec<std::path::PathBuf> {
    let apps = ["Discord.app", "Discord Canary.app", "Discord PTB.app", "Discord Development.app"];
    let mut dirs = vec![std::path::PathBuf::from("/Applications")];
    if let Some(home) = home {
        dirs.push(home.join("Applications"));
    }
    let mut out = Vec::new();
    for dir in dirs {
        for app in apps {
            out.push(dir.join(app));
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn detect_discord_scheme_handler() -> bool {
    // 1. Query xdg-mime for the registered scheme handler (on host or in flatpak sandbox).
    let mime_query = if in_sandbox() {
        capture("flatpak-spawn", &["--host", "xdg-mime", "query", "default", "x-scheme-handler/discord"], Duration::from_millis(800))
    } else {
        capture("xdg-mime", &["query", "default", "x-scheme-handler/discord"], Duration::from_millis(800))
    };
    if let Some(out) = mime_query {
        let trimmed = out.trim();
        if !trimmed.is_empty() && trimmed.ends_with(".desktop") {
            return true;
        }
    }

    // 2. Fall back to inspecting installed desktop files to ensure MimeType includes
    // x-scheme-handler/discord (preventing silent failures as in Arch FS#76847).
    for path in linux_discord_candidates(dirs::home_dir().as_deref()) {
        if path.extension().is_some_and(|ext| ext == "desktop")
            && let Ok(content) = std::fs::read_to_string(&path)
            && content.lines().any(|line| {
                let line = line.trim();
                line.starts_with("MimeType=") && line.contains("x-scheme-handler/discord")
            })
        {
            return true;
        }
    }

    false
}

#[cfg(any(target_os = "linux", test))]
fn linux_discord_candidates(home: Option<&Path>) -> Vec<std::path::PathBuf> {
    let mut files = vec![
        std::path::PathBuf::from("/usr/share/applications/discord.desktop"),
        std::path::PathBuf::from("/usr/share/applications/discord-canary.desktop"),
        std::path::PathBuf::from("/usr/share/applications/discord-ptb.desktop"),
        std::path::PathBuf::from("/var/lib/flatpak/exports/share/applications/com.discordapp.Discord.desktop"),
        std::path::PathBuf::from("/var/lib/flatpak/exports/share/applications/com.discordapp.DiscordCanary.desktop"),
        std::path::PathBuf::from("/var/lib/snapd/desktop/applications/discord_discord.desktop"),
        std::path::PathBuf::from("/snap/bin/discord"),
    ];
    if let Some(home) = home {
        files.push(home.join(".local/share/applications/discord.desktop"));
        files.push(home.join(".local/share/applications/discord-canary.desktop"));
        files.push(home.join(".local/share/applications/discord-ptb.desktop"));
        files.push(home.join(".local/share/flatpak/exports/share/applications/com.discordapp.Discord.desktop"));
        files.push(home.join(".local/share/flatpak/exports/share/applications/com.discordapp.DiscordCanary.desktop"));
    }
    files
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn detect_discord_scheme_handler() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_candidates_include_flatpak_and_system_paths() {
        let fake_home = Path::new("/home/testuser");
        let candidates = linux_discord_candidates(Some(fake_home));
        assert!(candidates.iter().any(|p| p.ends_with("com.discordapp.Discord.desktop")));
        assert!(candidates.iter().any(|p| p == Path::new("/home/testuser/.local/share/applications/discord.desktop")));
        assert!(candidates.iter().any(|p| p == Path::new("/usr/share/applications/discord.desktop")));
        assert!(candidates.iter().any(|p| p == Path::new("/snap/bin/discord")));
    }

    #[test]
    fn macos_candidates_include_standard_apps() {
        let fake_home = Path::new("/Users/testuser");
        let candidates = macos_discord_candidates(Some(fake_home));
        assert!(candidates.iter().any(|p| p == Path::new("/Applications/Discord.app")));
        assert!(candidates.iter().any(|p| p == Path::new("/Users/testuser/Applications/Discord.app")));
        assert!(candidates.iter().any(|p| p == Path::new("/Applications/Discord Canary.app")));
    }

    #[test]
    fn discord_url_gives_app_or_web_url() {
        use artcraft_launcher_core::catalog::{DISCORD_APP_URL, DISCORD_URL};
        let url = discord_url();
        assert!(url == DISCORD_URL || url == DISCORD_APP_URL);
    }
}
