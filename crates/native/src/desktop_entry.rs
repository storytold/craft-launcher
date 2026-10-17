//! Menu entries (freedesktop `.desktop` files) for AppImages the launcher installs.
//!
//! An AppImage installs nothing by itself, so without an entry the app is missing from the
//! desktop's menu and, on Wayland, the dock can't match the window's app id to an icon. The
//! launcher writes `<data>/applications/ai.storyteller.<app>.desktop` (the id each app's window
//! reports) and marks it as its own, so it only ever replaces or removes entries it wrote.
//!
//! The text is built by pure functions, tested on every platform; only Linux writes the files.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::Path;

use artcraft_launcher_core::CraftApp;

/// The key that marks an entry as written by the launcher.
pub const MARKER: &str = "X-ArtCraft-Launcher=true";

/// The menu categories and the window class each app's own `.desktop` file declares.
pub fn meta(app: &CraftApp) -> (&'static str, Option<&'static str>) {
    match app.id {
        "photocraft" => ("Graphics;2DGraphics;RasterGraphics;Photography;", Some("photocraft")),
        "pdfcraft" => ("Office;Viewer;", Some("pdfcraft")),
        "lightcraft" => ("Graphics;2DGraphics;RasterGraphics;Photography;", None),
        "filmcraft" => ("AudioVideo;Video;AudioVideoEditing;", None),
        "designcraft" => ("Graphics;Publishing;", None),
        "vectorcraft" => ("Graphics;2DGraphics;VectorGraphics;", None),
        "effectcraft" => ("AudioVideo;Video;Graphics;2DGraphics;", None),
        "wordcraft" => ("Office;WordProcessor;", None),
        "deckcraft" => ("Office;Presentation;", None),
        "gridcraft" => ("Office;Spreadsheet;", None),
        "cadcraft" => ("Graphics;Engineering;VectorGraphics;", None),
        "soundcraft" => ("AudioVideo;Audio;Midi;Mixer;Sequencer;Recorder;", None),
        "figcraft" => ("Graphics;2DGraphics;VectorGraphics;X-Design;", Some("figcraft")),
        _ => ("Graphics;", None),
    }
}

/// `path` as one quoted argument of an `Exec=` key, per the Desktop Entry spec: inside double
/// quotes `"`, `` ` ``, `$` and `\` take a backslash, then the string-value rule doubles every
/// backslash, and `%` (field codes) is written `%%`. `None` for paths that can't be written
/// safely (not UTF-8, or holding a control character such as a line break).
pub fn exec_quote(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    if s.chars().any(char::is_control) {
        return None;
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

/// A localestring/string value: backslashes and line breaks escaped.
fn value(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

/// The entry for `app` installed as the AppImage `appimage`; `icon` when its icon is installed
/// under the app's id. `None` when the path can't be put in an `Exec=` line.
pub fn render(app: &CraftApp, appimage: &Path, icon: bool) -> Option<String> {
    let exec = exec_quote(appimage)?;
    let (categories, wm_class) = meta(app);
    let mut text = String::from("[Desktop Entry]\nType=Application\n");
    text.push_str(&format!("Name={}\n", value(app.name)));
    text.push_str(&format!("Comment={}\n", value(app.tagline)));
    text.push_str(&format!("Exec={exec} %F\n"));
    if icon {
        text.push_str(&format!("Icon={}\n", app.bundle_id()));
    }
    text.push_str("Terminal=false\n");
    text.push_str(&format!("Categories={categories}\n"));
    text.push_str(&format!("StartupWMClass={}\n", wm_class.map_or_else(|| app.bundle_id(), str::to_string)));
    text.push_str(MARKER);
    text.push('\n');
    Some(text)
}

/// Whether the launcher wrote this entry.
pub fn is_ours(text: &str) -> bool {
    text.lines().any(|l| l.trim() == MARKER)
}

/// Whether the entry points at a file that no longer exists. An entry whose `Exec=` or
/// `TryExec=` (absolute paths only; a bare command name is resolved by the desktop shell,
/// not a file reference) is dangling once the file it names was removed: GNOME hides the
/// whole app, so the user sees it as gone. Old AppImages wrote unmarked entries like this
/// before the launcher adopted the menu entry, and an upgrade removes the old AppImage
/// without re-pointing them, so the only way back to a visible, working entry is for the
/// launcher to take it over.
pub fn is_dangling(text: &str) -> bool {
    text.lines()
        .filter_map(|l| l.strip_prefix("Exec=").or_else(|| l.strip_prefix("TryExec=")))
        .map(|rest| rest.trim_start())
        .filter(|rest| is_abs_path(rest))
        .any(|rest| !Path::new(abs_path(rest)).exists())
}

/// Whether a bare `Exec=`/`TryExec=` value is an absolute file path (quoted or not), as
/// opposed to a command name the desktop shell resolves from `$PATH`.
fn is_abs_path(rest: &str) -> bool {
    rest.strip_prefix('"').map_or(rest.starts_with('/'), |inner| inner.starts_with('/'))
}

/// The absolute path a quoted-or-bare `Exec=`/`TryExec=` value names (its `%`-field-codes and
/// trailing arguments dropped).
fn abs_path(rest: &str) -> &str {
    match rest.strip_prefix('"') {
        Some(inner) => match inner.rfind('"') {
            Some(end) => &inner[..end],
            None => inner,
        },
        None => match rest.find(|c: char| c.is_whitespace()) {
            Some(end) => &rest[..end],
            None => rest,
        },
    }
}

/// The program an entry's `Exec=` runs: its first argument, unquoted. Reads the launcher's own
/// quoting and the plain `Exec="<path>" %F` the apps' AppImages write for themselves.
pub fn exec_program(text: &str) -> Option<String> {
    let line = text.lines().find_map(|l| l.strip_prefix("Exec="))?;
    // String-value escapes first, then the quoting rule.
    let mut unescaped = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('s') => unescaped.push(' '),
                Some('n') => unescaped.push('\n'),
                Some('t') => unescaped.push('\t'),
                Some('r') => unescaped.push('\r'),
                Some(other) => unescaped.push(other),
                None => unescaped.push('\\'),
            }
        } else {
            unescaped.push(c);
        }
    }
    let mut out = String::new();
    let mut chars = unescaped.trim_start().chars();
    if unescaped.trim_start().starts_with('"') {
        chars.next();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => out.push(chars.next()?),
                c => out.push(c),
            }
        }
    } else {
        out.extend(chars.take_while(|c| !c.is_whitespace()));
    }
    Some(out.replace("%%", "%"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn app(id: &str) -> &'static CraftApp {
        CraftApp::by_id(id).unwrap()
    }

    #[test]
    fn writes_a_complete_entry() {
        let text = render(app("photocraft"), Path::new("/home/ana/Applications/photocraft-0.6.0-linux-x86_64.AppImage"), true).unwrap();
        let expected = format!(
            "[Desktop Entry]\nType=Application\nName=PhotoCraft\nComment={}\n\
             Exec=\"/home/ana/Applications/photocraft-0.6.0-linux-x86_64.AppImage\" %F\n\
             Icon=ai.storyteller.photocraft\nTerminal=false\nCategories=Graphics;2DGraphics;RasterGraphics;Photography;\n\
             StartupWMClass=photocraft\nX-ArtCraft-Launcher=true\n",
            app("photocraft").tagline
        );
        assert_eq!(text, expected);
        assert!(is_ours(&text));
        let no_icon = render(app("wordcraft"), Path::new("/x/w.AppImage"), false).unwrap();
        assert!(!no_icon.contains("Icon="));
        assert!(no_icon.contains("StartupWMClass=ai.storyteller.wordcraft\n"));
        assert!(no_icon.contains("Categories=Office;WordProcessor;\n"));
    }

    #[test]
    fn quotes_exec_paths_per_the_spec() {
        assert_eq!(exec_quote(Path::new("/home/ana/My Apps/p.AppImage")).unwrap(), "\"/home/ana/My Apps/p.AppImage\"");
        assert_eq!(exec_quote(Path::new("/a/\"q\" $HOME `x`.AppImage")).unwrap(), r#""/a/\\"q\\" \\$HOME \\`x\\`.AppImage""#);
        assert_eq!(exec_quote(Path::new(r"/a\b/100%.AppImage")).unwrap(), r#""/a\\\\b/100%%.AppImage""#);
        assert_eq!(exec_quote(Path::new("/a/line\nbreak.AppImage")), None);
        assert!(render(app("photocraft"), Path::new("/a/line\nbreak.AppImage"), true).is_none());
    }

    #[test]
    fn reads_back_the_program_it_wrote_and_the_apps_own_entries() {
        for p in ["/home/ana/My Apps/p.AppImage", "/a/\"q\" $HOME `x`.AppImage", r"/a\b/100%.AppImage", "/plain.AppImage"] {
            let text = render(app("gridcraft"), Path::new(p), false).unwrap();
            assert_eq!(exec_program(&text).map(PathBuf::from), Some(PathBuf::from(p)), "{p}");
        }
        // PhotoCraft's AppRun writes its own entry, unmarked.
        let theirs = "[Desktop Entry]\nName=PhotoCraft\nExec=\"/home/ana/Applications/photocraft-0.5.0-linux-x86_64.AppImage\" %F\n";
        assert_eq!(exec_program(theirs).as_deref(), Some("/home/ana/Applications/photocraft-0.5.0-linux-x86_64.AppImage"));
        assert!(!is_ours(theirs));
        assert_eq!(exec_program("[Desktop Entry]\nExec=photocraft %F\n").as_deref(), Some("photocraft"));
        assert_eq!(exec_program("[Desktop Entry]\nName=x\n"), None);
    }

    #[test]
    fn dangling_detection_matches_what_the_upgrade_leaves_behind() {
        // The real-world shape: an old, unmarked entry whose TryExec points at the removed
        // AppImage and whose Exec is a wrapper the desktop shell resolves by name.
        let stale = "\
[Desktop Entry]\n\
Type=Application\n\
Name=CADCraft\n\
Exec=cadcraft %F\n\
TryExec=/home/ana/Applications/cadcraft-0.6.2-linux-x86_64.AppImage\n\
Icon=ai.storyteller.cadcraft\n\
";
        assert!(!is_ours(stale));
        // GNOME hides the app on the dead TryExec even though the wrapper still works.
        assert!(is_dangling(stale));

        // Only counts dangling when the named file is actually absent.
        let tmp = std::env::temp_dir().join("ac_dangling_test.AppImage");
        std::fs::write(&tmp, "").unwrap();
        assert!(!is_dangling(&format!("[Desktop Entry]\nTryExec={}\n", tmp.display())));
        std::fs::remove_file(&tmp).unwrap();
        assert!(is_dangling(&format!("[Desktop Entry]\nTryExec={}\n", tmp.display())));

        // A missing quoted Exec is dangling too (the shape the apps' own AppRun writes).
        assert!(is_dangling("[Desktop Entry]\nExec=\"/home/ana/Applications/figcraft-0.5.0-linux-x86_64.AppImage\" %F\n"));
        // A bare command name (resolved by the desktop shell) is not a file reference.
        assert!(!is_dangling("[Desktop Entry]\nExec=cadcraft %F\n"));
        assert!(!is_dangling("[Desktop Entry]\nExec=photocraft %F\nTryExec=\n"));
        // An entry pointing at a file that still exists is not dangling.
        let present = std::env::temp_dir().join("ac_present_test.AppImage");
        std::fs::write(&present, "").unwrap();
        assert!(!is_dangling(&format!("[Desktop Entry]\nExec={}\n", present.display())));
        // The launcher's own entry (marked, live path) is never dangling.
        assert!(!is_dangling(&render(app("cadcraft"), &present, true).unwrap()));
        std::fs::remove_file(&present).unwrap();
    }
}
