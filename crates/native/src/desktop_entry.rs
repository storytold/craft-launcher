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
/// under the app's id. `runner` optionally supplies its runtime. `None` when a path can't be
/// put in an `Exec=` line.
pub fn render(app: &CraftApp, appimage: &Path, icon: bool, runner: Option<&Path>) -> Option<String> {
    let runner = runner.filter(|p| !p.as_os_str().is_empty());
    let appimage_exec = exec_quote(appimage)?;
    let exec = match runner {
        Some(runner) => format!("env {}_NO_DESKTOP_INTEGRATION=1 {} {appimage_exec}", app.id.to_ascii_uppercase(), exec_quote(runner)?),
        None => appimage_exec,
    };
    let (categories, wm_class) = meta(app);
    let mut text = String::from("[Desktop Entry]\nType=Application\n");
    text.push_str(&format!("Name={}\n", value(app.name)));
    text.push_str(&format!("Comment={}\n", value(app.tagline)));
    text.push_str(&format!("Exec={exec} %F\n"));
    if runner.is_some() {
        // The entry belongs to the AppImage, not the runner; keep its target for removal.
        text.push_str(&format!("TryExec={}\n", value(appimage.to_str()?)));
    }
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

/// Whether an entry targets `appimage`, directly or through the launcher's runner.
pub fn runs_appimage(text: &str, appimage: &Path) -> bool {
    if exec_program(text).is_some_and(|p| Path::new(&p) == appimage) {
        return true;
    }
    let Some(path) = appimage.to_str().filter(|_| is_ours(text)) else {
        return false;
    };
    let target = format!("TryExec={}", value(path));
    text.lines().any(|line| line == target)
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
        let text = render(app("photocraft"), Path::new("/home/ana/Applications/photocraft-0.6.0-linux-x86_64.AppImage"), true, None).unwrap();
        let expected = format!(
            "[Desktop Entry]\nType=Application\nName=PhotoCraft\nComment={}\n\
             Exec=\"/home/ana/Applications/photocraft-0.6.0-linux-x86_64.AppImage\" %F\n\
             Icon=ai.storyteller.photocraft\nTerminal=false\nCategories=Graphics;2DGraphics;RasterGraphics;Photography;\n\
             StartupWMClass=photocraft\nX-ArtCraft-Launcher=true\n",
            app("photocraft").tagline
        );
        assert_eq!(text, expected);
        assert!(is_ours(&text));
        let no_icon = render(app("wordcraft"), Path::new("/x/w.AppImage"), false, None).unwrap();
        assert!(!no_icon.contains("Icon="));
        assert!(no_icon.contains("StartupWMClass=ai.storyteller.wordcraft\n"));
        assert!(no_icon.contains("Categories=Office;WordProcessor;\n"));
    }

    #[test]
    fn appimage_menu_entries_use_the_runner_and_keep_the_target() {
        let image = Path::new("/home/ana/My Apps/100%.AppImage");
        let runner = Path::new("/nix/store/My Runner/appimage-run");
        let text = render(app("photocraft"), image, true, Some(runner)).unwrap();
        assert!(
            text.contains(
                "Exec=env PHOTOCRAFT_NO_DESKTOP_INTEGRATION=1 \"/nix/store/My Runner/appimage-run\" \"/home/ana/My Apps/100%%.AppImage\" %F\n"
            ),
            "{text}"
        );
        assert!(text.contains("TryExec=/home/ana/My Apps/100%.AppImage\n"), "{text}");
        assert_eq!(exec_program(&text).as_deref(), Some("env"));
        assert!(runs_appimage(&text, image));
    }

    #[test]
    fn runner_targets_are_recognized_only_in_owned_entries() {
        let image = Path::new(r#"/home/ana/quote\" $x/100%.AppImage"#);
        let text = render(app("gridcraft"), image, false, Some(Path::new("/nix/store/runner/appimage-run"))).unwrap();
        assert!(runs_appimage(&text, image));
        assert!(!runs_appimage(&text, Path::new("/different.AppImage")));
        assert!(!runs_appimage(&text.replace(MARKER, ""), image));
        let direct = render(app("gridcraft"), image, false, None).unwrap();
        assert!(runs_appimage(&direct, image));
        assert_eq!(render(app("gridcraft"), image, false, Some(Path::new(""))), Some(direct));
        assert!(render(app("gridcraft"), image, false, Some(Path::new("/runner\nunsafe"))).is_none());
    }

    #[test]
    fn quotes_exec_paths_per_the_spec() {
        assert_eq!(exec_quote(Path::new("/home/ana/My Apps/p.AppImage")).unwrap(), "\"/home/ana/My Apps/p.AppImage\"");
        assert_eq!(exec_quote(Path::new("/a/\"q\" $HOME `x`.AppImage")).unwrap(), r#""/a/\\"q\\" \\$HOME \\`x\\`.AppImage""#);
        assert_eq!(exec_quote(Path::new(r"/a\b/100%.AppImage")).unwrap(), r#""/a\\\\b/100%%.AppImage""#);
        assert_eq!(exec_quote(Path::new("/a/line\nbreak.AppImage")), None);
        assert!(render(app("photocraft"), Path::new("/a/line\nbreak.AppImage"), true, None).is_none());
    }

    #[test]
    fn reads_back_the_program_it_wrote_and_the_apps_own_entries() {
        for p in ["/home/ana/My Apps/p.AppImage", "/a/\"q\" $HOME `x`.AppImage", r"/a\b/100%.AppImage", "/plain.AppImage"] {
            let text = render(app("gridcraft"), Path::new(p), false, None).unwrap();
            assert_eq!(exec_program(&text).map(PathBuf::from), Some(PathBuf::from(p)), "{p}");
        }
        // PhotoCraft's AppRun writes its own entry, unmarked.
        let theirs = "[Desktop Entry]\nName=PhotoCraft\nExec=\"/home/ana/Applications/photocraft-0.5.0-linux-x86_64.AppImage\" %F\n";
        assert_eq!(exec_program(theirs).as_deref(), Some("/home/ana/Applications/photocraft-0.5.0-linux-x86_64.AppImage"));
        assert!(!is_ours(theirs));
        assert_eq!(exec_program("[Desktop Entry]\nExec=photocraft %F\n").as_deref(), Some("photocraft"));
        assert_eq!(exec_program("[Desktop Entry]\nName=x\n"), None);
    }
}
