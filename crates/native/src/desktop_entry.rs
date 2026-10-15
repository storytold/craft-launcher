//! Menu entries (freedesktop `.desktop` files) for AppImages the launcher installs.
//!
//! An AppImage installs nothing by itself, so without an entry the app is missing from the
//! desktop's menu and, on Wayland, the dock can't match the window's app id to an icon. The
//! launcher writes `<data>/applications/ai.storyteller.<app>.desktop` (the id each app's window
//! reports) and marks it as its own, so it only ever replaces or removes entries it wrote. The
//! entry also carries the file types the app opens (`MimeType=`), which is what makes the desktop
//! offer the app for a file at all; `install::linux` refreshes the desktop's MIME database after
//! writing or removing one, since GLib-based desktops read `mimeinfo.cache` beside the entries
//! rather than the entries themselves.
//!
//! The text is built by pure functions, tested on every platform; only Linux writes the files.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::Path;

use artcraft_launcher_core::CraftApp;

/// The key that marks an entry as written by the launcher.
pub const MARKER: &str = "X-ArtCraft-Launcher=true";

/// The menu categories and the window class each app's own `.desktop` file declares.
pub fn meta(app: &CraftApp) -> (&'static str, Option<&'static str>) {
    // Keep in step with [`file_types`], the other half of what the apps declare there.
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

/// The file types (`MimeType=`) each app's own `.desktop` file declares, copied verbatim from
/// `<app>/packaging/linux/ai.storyteller.<app>.desktop`. They are how the desktop knows the app
/// opens a file at all: an entry without them shows up as a launch button and nothing else, so
/// double-clicking an `.xlsx` can't offer GridCraft and "Open with" never lists it.
///
/// The list is the app's, not a choice the launcher makes: where two apps claim the same type
/// (PhotoCraft and LightCraft both take `image/jpeg`), which one a click reaches is the user's
/// default, and the desktop decides it. DesignCraft and EffectCraft declare none yet.
pub fn file_types(app: &CraftApp) -> Option<&'static str> {
    let mimes = match app.id {
        "photocraft" => {
            "application/x-photocraft;image/x-paintnet;image/openraster;application/x-krita;\
             image/vnd.adobe.photoshop;image/x-psd;image/x-psb;image/png;image/jpeg;image/tiff;\
             image/webp;image/gif;image/bmp;image/x-tga;image/x-icon;image/vnd.microsoft.icon;\
             image/x-portable-anymap;image/x-portable-bitmap;image/x-portable-graymap;\
             image/x-portable-pixmap;image/x-exr;image/vnd.radiance;image/avif;image/qoi;"
        }
        "pdfcraft" => "application/pdf;",
        "lightcraft" => {
            "image/jpeg;image/png;image/tiff;image/webp;image/x-adobe-dng;image/x-sony-arw;\
             image/x-canon-cr2;image/x-canon-cr3;image/x-nikon-nef;image/x-fuji-raf;\
             image/x-olympus-orf;image/x-panasonic-rw2;image/x-panasonic-rw;image/x-pentax-pef;"
        }
        "filmcraft" => {
            "application/x-filmcraft;video/mp4;video/x-m4v;video/quicktime;video/x-matroska;\
             video/webm;video/vnd.avi;video/x-msvideo;application/mxf;video/mp2t;video/mpeg;video/3gpp;"
        }
        "vectorcraft" => "image/svg+xml;application/pdf;",
        "wordcraft" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document;\
             application/vnd.ms-word.document.macroEnabled.12;\
             application/vnd.openxmlformats-officedocument.wordprocessingml.template;\
             application/vnd.ms-word.template.macroEnabled.12;application/msword;\
             application/msword-template;application/vnd.oasis.opendocument.text;application/rtf;\
             text/rtf;text/markdown;text/x-tex;text/html;text/plain;"
        }
        "deckcraft" => {
            "application/x-deckcraft;\
             application/vnd.openxmlformats-officedocument.presentationml.presentation;\
             application/vnd.openxmlformats-officedocument.presentationml.template;\
             application/vnd.openxmlformats-officedocument.presentationml.slideshow;"
        }
        "gridcraft" => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet;\
             application/vnd.ms-excel.sheet.macroEnabled.12;\
             application/vnd.ms-excel.sheet.binary.macroEnabled.12;\
             application/vnd.oasis.opendocument.spreadsheet;text/csv;text/tab-separated-values;"
        }
        "cadcraft" => "image/vnd.dxf;image/vnd.dwg;application/x-dxf;application/acad;",
        "soundcraft" => {
            "application/x-soundcraft-session;audio/x-wav;audio/wav;audio/vnd.wave;audio/x-aiff;\
             audio/aiff;audio/flac;audio/x-flac;audio/mpeg;audio/midi;audio/x-midi;"
        }
        "figcraft" => "application/x-figcraft;image/svg+xml;image/png;image/jpeg;x-scheme-handler/figcraft;",
        _ => return None,
    };
    Some(mimes)
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
    // What the app opens, as its own entry declares it. An empty `MimeType=` is worse than none:
    // the desktop reads it as a claim on nothing, so leave the key out.
    if let Some(file_types) = file_types(app) {
        text.push_str(&format!("MimeType={file_types}\n"));
    }
    text.push_str(&format!("StartupWMClass={}\n", wm_class.map_or_else(|| app.bundle_id(), str::to_string)));
    text.push_str(MARKER);
    text.push('\n');
    Some(text)
}

/// Whether the launcher wrote this entry.
pub fn is_ours(text: &str) -> bool {
    text.lines().any(|l| l.trim() == MARKER)
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
             MimeType={}\nStartupWMClass=photocraft\nX-ArtCraft-Launcher=true\n",
            app("photocraft").tagline,
            file_types(app("photocraft")).unwrap()
        );
        assert_eq!(text, expected);
        assert!(is_ours(&text));
        let no_icon = render(app("wordcraft"), Path::new("/x/w.AppImage"), false).unwrap();
        assert!(!no_icon.contains("Icon="));
        assert!(no_icon.contains("StartupWMClass=ai.storyteller.wordcraft\n"));
        assert!(no_icon.contains("Categories=Office;WordProcessor;\n"));
    }

    #[test]
    fn lists_the_file_types_each_app_opens() {
        let wordcraft = render(app("wordcraft"), Path::new("/x/w.AppImage"), false).unwrap();
        assert!(wordcraft.contains("MimeType=application/vnd.openxmlformats-officedocument.wordprocessingml.document;"));
        assert!(wordcraft.contains(";text/plain;\n"), "{wordcraft}");
        let figcraft = render(app("figcraft"), Path::new("/x/f.AppImage"), false).unwrap();
        assert!(figcraft.contains(";x-scheme-handler/figcraft;\n"), "{figcraft}");
        // DesignCraft declares no file types, so its entry must claim none either: an app that
        // has never opened a `.psd` shouldn't be offered for one.
        let designcraft = render(app("designcraft"), Path::new("/x/d.AppImage"), false).unwrap();
        assert!(!designcraft.contains("MimeType="), "{designcraft}");
        // Every list is a well-formed `MimeType=`: `type/subtype;`, no spaces, closed with `;`.
        for app in &artcraft_launcher_core::SUITE {
            let Some(file_types) = file_types(app) else {
                continue;
            };
            assert!(file_types.ends_with(';'), "{}: {file_types}", app.id);
            for one in file_types.split(';').filter(|t| !t.is_empty()) {
                assert!(one.contains('/') && !one.contains(char::is_whitespace), "{}: {one}", app.id);
            }
        }
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
}
