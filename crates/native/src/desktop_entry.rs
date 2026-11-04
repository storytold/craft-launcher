//! Menu entries (freedesktop `.desktop` files) for AppImages the launcher installs.
//!
//! An AppImage installs nothing by itself, so without an entry the app is missing from the
//! desktop's menu and, on Wayland, the dock can't match the window's app id to an icon. The
//! launcher writes `<data>/applications/ai.storyteller.<app>.desktop` (the id each app's window
//! reports). Only a manifest ledger entry plus the exact rendered contents proves ownership.
//!
//! The text is built by pure functions, tested on every platform; only Linux writes the files.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::Path;

use artcraft_launcher_core::CraftApp;

/// A descriptive marker, not sufficient proof of ownership for replacement or removal.
const MARKER: &str = "X-ArtCraft-Launcher=true";

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
        "texturecraft" => ("Graphics;3DGraphics;", Some("texturecraft")),
        "materialcraft" => ("Graphics;3DGraphics;", Some("materialcraft")),
        "craft-anything" => ("Development;Debugger;", None),
        _ => ("Graphics;", None),
    }
}

/// `path` as one quoted argument of an `Exec=` key, per the Desktop Entry spec: inside double
/// quotes `"`, `` ` ``, `$` and `\` take a backslash, then the string-value rule doubles every
/// backslash, and `%` (field codes) is written `%%`. `None` for paths that can't be written
/// safely (not UTF-8, holding a control character such as a line break, or holding `=`, which the
/// spec forbids in the executable name or path even when quoted).
pub fn exec_quote(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    if s.chars().any(|c| c.is_control() || c == '=') {
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

#[cfg(test)]
mod tests {
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
        // The spec forbids `=` in the executable name or path, even quoted.
        assert_eq!(exec_quote(Path::new("/opt/artcraft=beta/app")), None);
        assert!(render(app("photocraft"), Path::new("/a/line\nbreak.AppImage"), true).is_none());
    }
}
