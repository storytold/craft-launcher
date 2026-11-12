//! "Set as default for its file types" on Linux: makes an installed app the default for the file
//! types its menu entry lists, by writing them under `[Default Applications]` in the user's
//! `mimeapps.list` (freedesktop's MIME Applications Associations spec).
//!
//! Installing an app only offers it for those types. The default stays whatever the distribution
//! or another app chose: Fedora's own `/usr/share/applications/mimeapps.list` sends spreadsheets to
//! LibreOffice Calc, so a newly installed GridCraft never opens a `.csv` on a double-click until
//! the user's own list says so. Only the user's list outranks the system's.
//!
//! The text is edited by pure functions, tested on every platform; only Linux touches files.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::{Path, PathBuf};

use artcraft_launcher_core::CraftApp;

/// The section the spec reads defaults from.
const DEFAULTS: &str = "[Default Applications]";
/// The section that hides an app from a type; a default it lists there would be ignored.
const REMOVED: &str = "[Removed Associations]";

/// The `MimeType=` list of a desktop entry's `[Desktop Entry]` group (not its actions'), keeping
/// only well-formed `type/subtype` names, in order and without repeats.
pub fn mime_types(entry: &str) -> Vec<String> {
    let mut in_main = false;
    let mut out: Vec<String> = Vec::new();
    for line in entry.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some(value) = line.strip_prefix("MimeType").map(str::trim_start).and_then(|r| r.strip_prefix('=')) else { continue };
        for t in value.split(';').map(str::trim) {
            if is_mime_type(t) && !out.iter().any(|o| o == t) {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// `type/subtype` made of the characters MIME names use: nothing that could break a
/// `key=value` line (`=`, `;`, `[`, spaces, line breaks).
fn is_mime_type(t: &str) -> bool {
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_'));
    matches!(t.split_once('/'), Some((a, b)) if ok(a) && ok(b))
}

/// `list` (a `mimeapps.list`, possibly empty) with `desktop_id` as the default for every type in
/// `types`, and dropped from those types' `[Removed Associations]`. Every other line is kept as
/// it was. Types already defaulting to `desktop_id` alone are left untouched.
pub fn set_defaults(list: &str, desktop_id: &str, types: &[String]) -> String {
    let mut lines: Vec<String> = list.lines().map(str::to_string).collect();
    let wanted = format!("{desktop_id};");

    // Removed Associations: take the app out of each type's list; drop a key left empty.
    if let Some((start, end)) = section(&lines, REMOVED) {
        let mut kept = Vec::with_capacity(end - start);
        for line in lines.drain(start..end) {
            match key_value(&line) {
                Some((key, value)) if types.iter().any(|t| t == key) => {
                    let rest: Vec<&str> = value.split(';').map(str::trim).filter(|v| !v.is_empty() && *v != desktop_id).collect();
                    if rest.len() == value.split(';').map(str::trim).filter(|v| !v.is_empty()).count() {
                        kept.push(line);
                    } else if !rest.is_empty() {
                        kept.push(format!("{key}={};", rest.join(";")));
                    }
                }
                _ => kept.push(line),
            }
        }
        for (i, line) in kept.into_iter().enumerate() {
            lines.insert(start + i, line);
        }
    }

    // Default Applications: replace each type's line, add the missing ones at the section's end.
    let (start, end) = match section(&lines, DEFAULTS) {
        Some(range) => range,
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(DEFAULTS.to_string());
            (lines.len(), lines.len())
        }
    };
    let mut missing: Vec<&String> = Vec::new();
    for t in types {
        match (start..end).find(|&i| key_value(&lines[i]).is_some_and(|(k, _)| k == t)) {
            Some(i) => {
                let same = key_value(&lines[i]).is_some_and(|(_, v)| v.trim().trim_end_matches(';') == desktop_id);
                if !same {
                    lines[i] = format!("{t}={wanted}");
                }
            }
            None => missing.push(t),
        }
    }
    // After the section's last entry, before any blank lines that separate it from the next one.
    let mut at = end;
    while at > start && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    for (i, t) in missing.into_iter().enumerate() {
        lines.insert(at + i, format!("{t}={wanted}"));
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The body of `header`'s group: the line after the header up to the next group (or the end).
fn section(lines: &[String], header: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| l.trim() == header)? + 1;
    let end = lines.iter().skip(start).position(|l| l.trim_start().starts_with('[')).map_or(lines.len(), |n| start + n);
    Some((start, end))
}

/// A `key=value` line's parts, trimmed; `None` for comments, blanks and group headers.
fn key_value(line: &str) -> Option<(&str, &str)> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') || t.starts_with('[') {
        return None;
    }
    let (k, v) = t.split_once('=')?;
    Some((k.trim(), v.trim()))
}

/// The user's `mimeapps.list`. Inside the launcher's own Flatpak `XDG_CONFIG_HOME` points into
/// the sandbox, where nothing on the desktop reads it, so the host's is used.
pub fn user_list_path() -> Option<PathBuf> {
    let config = if crate::process::in_sandbox() {
        match std::env::var_os("HOST_XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
            Some(dir) => Some(dir),
            None => dirs::home_dir().map(|h| h.join(".config")),
        }
    } else {
        dirs::config_dir()
    };
    config.map(|c| c.join("mimeapps.list"))
}

/// Where an installed app's menu entry can be, the user's own folders first: the launcher's
/// AppImage entries, user and system Flatpak exports, then the system's data folders (inside the
/// sandbox those include the host's Flatpak exports, `/run/host/…-share`).
#[cfg(target_os = "linux")]
fn entry_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(data) = crate::install::linux::host_data_dir() {
        dirs.push(data.join("flatpak/exports/share"));
        dirs.push(data);
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    if let Some(list) = std::env::var_os("XDG_DATA_DIRS") {
        dirs.extend(std::env::split_paths(&list).filter(|p| p.is_absolute()));
    }
    dirs.push(PathBuf::from("/usr/local/share"));
    dirs.push(PathBuf::from("/usr/share"));
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let new = !seen.contains(d);
        seen.push(d.clone());
        new
    });
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

/// Inside the launcher's own Flatpak the installed apps' entries are out of sight: Flatpak hides
/// `~/.local/share/flatpak` (an empty folder in the sandbox, whatever `--filesystem=home` says),
/// and `/var/lib/flatpak` and the host's `/usr/share` aren't mounted. They are read on the host
/// through `flatpak-spawn --host`, which the launcher already uses for `flatpak` itself.
#[cfg(target_os = "linux")]
fn host_entry(file: &str) -> Option<(PathBuf, Vec<String>)> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/share/flatpak/exports/share/applications"));
        candidates.push(home.join(".local/share/applications"));
    }
    candidates.extend(["/var/lib/flatpak/exports/share/applications", "/usr/local/share/applications", "/usr/share/applications"].map(PathBuf::from));
    candidates.into_iter().map(|d| d.join(file)).find_map(|path| {
        let text = crate::process::capture("flatpak-spawn", &["--host", "cat", "--", path.to_str()?], std::time::Duration::from_secs(5))?;
        let types = mime_types(&text);
        (!types.is_empty()).then_some((path, types))
    })
}

/// The first `<id>.desktop` among `dirs` that lists file types, with them.
fn find_entry(dirs: &[PathBuf], file: &str) -> Option<(PathBuf, Vec<String>)> {
    dirs.iter().map(|d| d.join(file)).find_map(|path| {
        let text = std::fs::read_to_string(&path).ok()?;
        let types = mime_types(&text);
        (!types.is_empty()).then_some((path, types))
    })
}

/// Makes `app` the default for the file types its installed menu entry lists. `Ok` says how many.
#[cfg(not(target_os = "linux"))]
pub fn set_default(app: &CraftApp) -> Result<String, String> {
    Err(format!("Setting {} as the default isn't available on this system yet", app.name))
}

/// Makes `app` the default for the file types its installed menu entry lists. `Ok` says how many.
#[cfg(target_os = "linux")]
pub fn set_default(app: &CraftApp) -> Result<String, String> {
    let desktop_id = format!("{}.desktop", app.bundle_id());
    let found = find_entry(&entry_dirs(), &desktop_id).or_else(|| if crate::process::in_sandbox() { host_entry(&desktop_id) } else { None });
    let Some((entry, types)) = found else {
        return Err(format!("Couldn't set {} as the default: its menu entry ({desktop_id}) lists no file types", app.name));
    };
    let list = user_list_path().ok_or_else(|| format!("Couldn't set {} as the default: no home folder", app.name))?;
    write_defaults(&list, &desktop_id, &types).map_err(|e| format!("Couldn't set {} as the default: {e}", app.name))?;
    log::info!("{}: default for {} in {} (from {})", app.id, types.join(";"), list.display(), entry.display());
    Ok(match types.len() {
        1 => format!("{} now opens its file type by default", app.name),
        n => format!("{} now opens its {n} file types by default", app.name),
    })
}

/// Serializes the launcher's own read-modify-write of `mimeapps.list`, so two requests at once don't drop each other's types.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Rewrites `list` through a temporary file beside it, so a crash never leaves it half written.
fn write_defaults(list: &Path, desktop_id: &str, types: &[String]) -> Result<(), String> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let old = match std::fs::read_to_string(list) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("couldn't read {} ({e})", list.display())),
    };
    let new = set_defaults(&old, desktop_id, types);
    if new == old {
        return Ok(());
    }
    let dir = list.parent().ok_or_else(|| format!("{} has no folder", list.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("couldn't write in {} ({e})", dir.display()))?;
    std::io::Write::write_all(&mut tmp, new.as_bytes()).map_err(|e| format!("couldn't write {} ({e})", list.display()))?;
    // Keep the old file's permissions rather than the temporary file's 0600.
    if let Ok(meta) = std::fs::metadata(list) {
        let _ = std::fs::set_permissions(tmp.path(), meta.permissions());
    }
    tmp.persist(list).map_err(|e| format!("couldn't replace {} ({})", list.display(), e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "ai.storyteller.gridcraft.desktop";

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reads_the_main_groups_file_types_only() {
        let entry = "[Desktop Entry]\nName=GridCraft\nMimeType=text/csv;application/vnd.oasis.opendocument.spreadsheet;text/csv;bad type;x=y/z;\n\
                     [Desktop Action new]\nMimeType=image/png;\n";
        assert_eq!(mime_types(entry), types(&["text/csv", "application/vnd.oasis.opendocument.spreadsheet"]));
        assert!(mime_types("[Desktop Entry]\nName=x\n").is_empty());
        assert_eq!(mime_types("[Desktop Entry]\nMimeType = text/csv\n"), types(&["text/csv"]));
    }

    #[test]
    fn adds_a_defaults_section_to_an_empty_or_missing_list() {
        assert_eq!(set_defaults("", ID, &types(&["text/csv"])), format!("[Default Applications]\ntext/csv={ID};\n"));
        let list = "[Added Associations]\nx-scheme-handler/http=brave.desktop;\n\n";
        assert_eq!(
            set_defaults(list, ID, &types(&["text/csv"])),
            format!("[Added Associations]\nx-scheme-handler/http=brave.desktop;\n\n[Default Applications]\ntext/csv={ID};\n")
        );
    }

    #[test]
    fn replaces_and_appends_inside_the_existing_section_keeping_everything_else() {
        let list = "# mine\n[Added Associations]\ntext/csv=calc.desktop;\n\n[Default Applications]\n\
                    x-scheme-handler/https=brave.desktop;\ntext/csv=calc.desktop;\n\n[Removed Associations]\nimage/png=gimp.desktop;\n";
        let out = set_defaults(list, ID, &types(&["text/csv", "text/tab-separated-values"]));
        assert_eq!(
            out,
            format!(
                "# mine\n[Added Associations]\ntext/csv=calc.desktop;\n\n[Default Applications]\n\
                 x-scheme-handler/https=brave.desktop;\ntext/csv={ID};\ntext/tab-separated-values={ID};\n\n\
                 [Removed Associations]\nimage/png=gimp.desktop;\n"
            )
        );
        // Running it again changes nothing.
        assert_eq!(set_defaults(&out, ID, &types(&["text/csv", "text/tab-separated-values"])), out);
    }

    #[test]
    fn takes_the_app_out_of_removed_associations_for_those_types_only() {
        let list = format!("[Removed Associations]\ntext/csv={ID};calc.desktop;\ntext/tab-separated-values={ID};\nimage/png={ID};\n");
        let out = set_defaults(&list, ID, &types(&["text/csv", "text/tab-separated-values"]));
        assert_eq!(
            out,
            format!(
                "[Removed Associations]\ntext/csv=calc.desktop;\nimage/png={ID};\n\n\
                 [Default Applications]\ntext/csv={ID};\ntext/tab-separated-values={ID};\n"
            )
        );
    }

    #[test]
    fn finds_the_first_entry_that_lists_file_types() {
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (root.path().join("a"), root.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        // The launcher's own AppImage entry without MimeType= doesn't hide the Flatpak's.
        std::fs::write(a.join(ID), "[Desktop Entry]\nName=GridCraft\n").unwrap();
        std::fs::write(b.join(ID), "[Desktop Entry]\nMimeType=text/csv;\n").unwrap();
        let (path, found) = find_entry(&[a, b.clone()], ID).unwrap();
        assert_eq!((path, found), (b.join(ID), types(&["text/csv"])));
        assert!(find_entry(&[root.path().join("none")], ID).is_none());
    }

    #[test]
    fn writes_the_list_creating_its_folder_and_keeping_its_mode() {
        let root = tempfile::tempdir().unwrap();
        let list = root.path().join("config/mimeapps.list");
        write_defaults(&list, ID, &types(&["text/csv"])).unwrap();
        assert_eq!(std::fs::read_to_string(&list).unwrap(), format!("[Default Applications]\ntext/csv={ID};\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&list, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_defaults(&list, ID, &types(&["text/csv", "text/tab-separated-values"])).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&list).unwrap().permissions().mode() & 0o777, 0o644);
        }
        assert!(std::fs::read_to_string(&list).unwrap().contains("text/tab-separated-values="));
    }
}
