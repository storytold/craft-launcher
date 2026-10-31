//! A CJK font from the operating system, for the CJK interface languages. The bundled faces have
//! no CJK glyphs, and a CJK font is too large to ship (10 MB or more), so the desktop build
//! borrows the one every system that can show them already has. Desktop only.
//!
//! Georgian, which the bundled faces don't draw either, borrows its system face the same way.

use std::path::{Path, PathBuf};

use egui::epaint::text::VariationCoords;
use egui::{FontData, FontTweak};
use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, MetadataProvider, string::StringId};

use crate::i18n::Script;

/// Larger files aren't read: PingFang, the largest candidate, is about 80 MB.
const MAX_FONT_BYTES: u64 = 100 * 1024 * 1024;
/// Faces looked at in one collection file (PingFang has 24).
const MAX_FACES: u32 = 64;

/// The bundled body font (Inter), whose baseline a fallback face is nudged onto.
const BODY_FONT: &[u8] = include_bytes!("../../../assets/fonts/Inter-Variable.ttf");

/// `(ascent, ascent + |descent| + leading) / units_per_em` of face `index` in `data`.
///
/// egui puts a fallback glyph's baseline at that face's own ascent and only centres the two line
/// boxes (`galley_from_rows`), so a face whose metrics differ from the body font's rides off the
/// baseline: a face with a half-em line gap, for one, sat about a quarter em high. [`load`]
/// cancels the difference with a `FontTweak`.
fn face_metrics(data: &[u8], index: u32) -> (f32, f32) {
    let Ok(font) = FontRef::from_index(data, index) else { return (0.0, 0.0) };
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let upem = f32::from(metrics.units_per_em);
    if upem <= 0.0 {
        return (0.0, 0.0);
    }
    (metrics.ascent / upem, (metrics.ascent - metrics.descent + metrics.leading) / upem)
}

/// The script's name, for the log lines.
fn script_name(script: Script) -> &'static str {
    match script {
        Script::Chinese => "Chinese",
        Script::Japanese => "Japanese",
        Script::Georgian => "Georgian",
    }
}

/// The families looked for, for the "none found" log line.
fn script_families(script: Script) -> &'static str {
    match script {
        Script::Chinese => "PingFang SC, Hiragino Sans GB, Heiti SC, Microsoft YaHei, SimSun, Noto Sans CJK SC, WenQuanYi and MiSans",
        Script::Japanese => "Hiragino Sans, Yu Gothic, Meiryo, MS Gothic, Noto Sans CJK JP, Noto Sans JP, IPAGothic and TakaoGothic",
        Script::Georgian => "SF Georgian, Arial Unicode MS, Sylfaen, Noto Sans Georgian and DejaVu Sans",
    }
}

/// A font file that may be installed, and the family of its face for this script.
struct Candidate {
    path: PathBuf,
    family: &'static str,
}

/// The system's font for `script`, or `None` (logged) when it has none this knows about.
/// Reads the file (once per process: the caller caches the answer).
pub(crate) fn load(script: Script) -> Option<FontData> {
    for Candidate { path, family } in candidates(script) {
        let Some(bytes) = read(&path) else { continue };
        let Some(index) = face_index(&bytes, family) else {
            log::debug!("no {family} face in {}", path.display());
            continue;
        };
        log::info!("{} text uses {family} from {} (face {index})", script_name(script), path.display());
        // Regular weight, in case the face is variable (Noto Sans CJK VF), and a nudge onto the
        // body font's baseline (see `face_metrics`).
        let (body_ascent, body_height) = face_metrics(BODY_FONT, 0);
        let (face_ascent, face_height) = face_metrics(&bytes, index);
        let tweak = FontTweak {
            coords: VariationCoords::new([(*b"wght", 400.0)]),
            y_offset_factor: (body_ascent - face_ascent) + 0.5 * (face_height - body_height),
            ..Default::default()
        };
        if matches!(script, Script::Georgian) {
            // Upper-case labels use Mtavruli only where this face draws it.
            let caps = FontRef::from_index(&bytes, index).is_ok_and(|f| ('\u{1c90}'..='\u{1cba}').all(|c| f.charmap().map(c).is_some()));
            crate::widgets::set_georgian_caps(caps);
        }
        let mut font = FontData::from_owned(bytes).tweak(tweak);
        font.index = index;
        return Some(font);
    }
    log::warn!("no {} font found on this system (looked for {})", script_name(script), script_families(script));
    None
}

fn read(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_FONT_BYTES {
        log::warn!("{} is too large to load as a font ({} bytes)", path.display(), meta.len());
        return None;
    }
    std::fs::read(path).map_err(|e| log::warn!("couldn't read {}: {e}", path.display())).ok()
}

/// The face of `family` in a font file or collection: its Regular face, else the first face of
/// that family. Matches the (typographic) family name in any language the file gives it in.
pub(crate) fn face_index(data: &[u8], family: &str) -> Option<u32> {
    let names = |font: &FontRef<'_>, ids: [StringId; 2]| -> Vec<String> {
        ids.into_iter().flat_map(|id| font.localized_strings(id).map(|s| s.to_string())).collect()
    };
    let mut first = None;
    for index in 0..MAX_FACES {
        let Ok(font) = FontRef::from_index(data, index) else { break };
        let families = names(&font, [StringId::TYPOGRAPHIC_FAMILY_NAME, StringId::FAMILY_NAME]);
        if !families.iter().any(|f| f.eq_ignore_ascii_case(family)) {
            continue;
        }
        let styles = names(&font, [StringId::TYPOGRAPHIC_SUBFAMILY_NAME, StringId::SUBFAMILY_NAME]);
        if styles.iter().any(|s| s.eq_ignore_ascii_case("Regular")) {
            return Some(index);
        }
        first.get_or_insert(index);
    }
    first
}

/// Where each system keeps a font for `script`, best first.
fn candidates(script: Script) -> Vec<Candidate> {
    match script {
        Script::Chinese => chinese_candidates(),
        Script::Japanese => japanese_candidates(),
        Script::Georgian => georgian_candidates(),
    }
}

/// The MiSans files a user installs by hand (Xiaomi's free Chinese face: the one some systems
/// have in place of any of the above), by weight preference. Its family name is `MiSans`.
const MISANS_FILES: [&str; 3] = ["MiSans-Regular.ttf", "MiSans-Normal.ttf", "MiSans-Medium.ttf"];

/// Where fonts a user installs by hand go on this system: the user's font folders, then the
/// system-wide ones, each as the font managers lay them out.
fn user_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join("Library/Fonts"));
        }
        dirs.push("/Library/Fonts".into());
    }
    #[cfg(target_os = "windows")]
    {
        // Fonts installed "for me" (the default since Windows 10 1809) go under the user's profile.
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
        }
        let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        dirs.push(root.join("Fonts"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            for sub in [".local/share/fonts", ".local/share/fonts/MiSans", ".local/share/fonts/misans", ".fonts"] {
                dirs.push(home.join(sub));
            }
        }
        for dir in [
            "/usr/share/fonts/MiSans",
            "/usr/share/fonts/misans",
            "/usr/share/fonts/truetype/misans",
            "/usr/share/fonts/TTF",
            "/usr/local/share/fonts/MiSans",
        ] {
            dirs.push(dir.into());
        }
    }
    dirs
}

/// Where each system keeps a Simplified Chinese font, best first.
fn chinese_candidates() -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut add = |path: PathBuf, family: &'static str| out.push(Candidate { path, family });
    #[cfg(target_os = "macos")]
    {
        // PingFang moved from the system fonts into a downloaded asset in later macOS versions.
        add("/System/Library/Fonts/PingFang.ttc".into(), "PingFang SC");
        for path in mac_font_assets("PingFang.ttc") {
            add(path, "PingFang SC");
        }
        add("/System/Library/Fonts/Hiragino Sans GB.ttc".into(), "Hiragino Sans GB W3");
        add("/System/Library/Fonts/STHeiti Medium.ttc".into(), "Heiti SC");
    }
    #[cfg(target_os = "windows")]
    {
        let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        add(root.join("Fonts").join("msyh.ttc"), "Microsoft YaHei");
        add(root.join("Fonts").join("simsun.ttc"), "SimSun");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Debian/Ubuntu, Arch, Fedora (static and variable) and FreeBSD's ports.
        const NOTO_DIRS: [&str; 8] = [
            "/usr/share/fonts/opentype/noto",
            "/usr/share/fonts/noto-cjk",
            "/usr/share/fonts/google-noto-cjk",
            "/usr/share/fonts/google-noto-sans-cjk-fonts",
            "/usr/share/fonts/google-noto-sans-cjk-vf-fonts",
            "/usr/share/fonts/noto",
            "/usr/local/share/fonts/noto",
            "/usr/local/share/fonts/noto-cjk",
        ];
        for file in ["NotoSansCJK-Regular.ttc", "NotoSansCJKsc-Regular.otf", "NotoSansCJK-VF.ttc"] {
            for dir in NOTO_DIRS {
                add(Path::new(dir).join(file), "Noto Sans CJK SC");
            }
        }
        const WQY_DIRS: [&str; 5] = [
            "/usr/share/fonts/truetype/wqy",
            "/usr/share/fonts/wenquanyi/wqy-microhei",
            "/usr/share/fonts/wqy-microhei",
            "/usr/share/fonts/wqy-zenhei",
            "/usr/local/share/fonts/wqy",
        ];
        for (file, family) in [("wqy-microhei.ttc", "WenQuanYi Micro Hei"), ("wqy-zenhei.ttc", "WenQuanYi Zen Hei")] {
            for dir in WQY_DIRS {
                add(Path::new(dir).join(file), family);
            }
        }
    }
    // Last, a MiSans the user installed themselves, on any system (#378).
    for file in MISANS_FILES {
        for dir in user_font_dirs() {
            add(dir.join(file), "MiSans");
        }
    }
    out
}

/// Where each system keeps a Japanese font, best first.
fn japanese_candidates() -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut add = |path: PathBuf, family: &'static str| out.push(Candidate { path, family });
    #[cfg(target_os = "macos")]
    {
        // Hiragino Sans (its file keeps the Japanese name); it may be a downloaded font asset.
        for file in ["ヒラギノ角ゴシック W3.ttc", "ヒラギノ角ゴシック W4.ttc", "ヒラギノ角ゴシック W6.ttc"] {
            add(Path::new("/System/Library/Fonts").join(file), "Hiragino Sans");
            for path in mac_font_assets(file) {
                add(path, "Hiragino Sans");
            }
        }
        add("/System/Library/Fonts/ヒラギノ丸ゴ ProN W4.ttc".into(), "Hiragino Maru Gothic ProN");
    }
    #[cfg(target_os = "windows")]
    {
        let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        add(root.join("Fonts").join("YuGothR.ttc"), "Yu Gothic");
        add(root.join("Fonts").join("meiryo.ttc"), "Meiryo");
        add(root.join("Fonts").join("msgothic.ttc"), "MS Gothic");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Debian/Ubuntu, Arch, Fedora (static and variable) and FreeBSD's ports.
        const NOTO_DIRS: [&str; 8] = [
            "/usr/share/fonts/opentype/noto",
            "/usr/share/fonts/noto-cjk",
            "/usr/share/fonts/google-noto-cjk",
            "/usr/share/fonts/google-noto-sans-cjk-fonts",
            "/usr/share/fonts/google-noto-sans-cjk-vf-fonts",
            "/usr/share/fonts/noto",
            "/usr/local/share/fonts/noto",
            "/usr/local/share/fonts/noto-cjk",
        ];
        for file in ["NotoSansCJK-Regular.ttc", "NotoSansCJKjp-Regular.otf", "NotoSansCJK-VF.ttc"] {
            for dir in NOTO_DIRS {
                add(Path::new(dir).join(file), "Noto Sans CJK JP");
            }
        }
        // Standalone Noto Sans JP, and the classic Japanese faces (ipafont-gothic, Takao, VL).
        const JP_DIRS: [&str; 8] = [
            "/usr/share/fonts/opentype/noto",
            "/usr/share/fonts/truetype/noto",
            "/usr/share/fonts/noto",
            "/usr/share/fonts/truetype/ipafont-gothic",
            "/usr/share/fonts/truetype/ipafont",
            "/usr/share/fonts/truetype/takao-gothic",
            "/usr/share/fonts/truetype/vlgothic",
            "/usr/local/share/fonts",
        ];
        for file in ["NotoSansJP-Regular.otf", "NotoSansJP-VF.ttf"] {
            for dir in JP_DIRS {
                add(Path::new(dir).join(file), "Noto Sans JP");
            }
        }
        for (file, family) in [("ipag.ttf", "IPAGothic"), ("ipagp.ttf", "IPAPGothic")] {
            for dir in JP_DIRS {
                add(Path::new(dir).join(file), family);
            }
        }
        for (file, family) in [("TakaoGothic.ttf", "TakaoGothic"), ("TakaoPGothic.ttf", "TakaoPGothic")] {
            for dir in JP_DIRS {
                add(Path::new(dir).join(file), family);
            }
        }
        for (file, family) in [("VL-Gothic-Regular.ttf", "VL Gothic"), ("VL-PGothic-Regular.ttf", "VL PGothic")] {
            for dir in JP_DIRS {
                add(Path::new(dir).join(file), family);
            }
        }
    }
    out
}

/// Where each system keeps a Georgian font, best first. These faces are small (about 0.1–1 MB),
/// except Arial Unicode MS, which macOS ships too.
fn georgian_candidates() -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut add = |path: PathBuf, family: &'static str| out.push(Candidate { path, family });
    #[cfg(target_os = "macos")]
    {
        // SF Georgian is the system UI face for Georgian; its family name starts with a dot, as
        // macOS's hidden UI faces do.
        add("/System/Library/Fonts/SFGeorgian.ttf".into(), ".SF Georgian");
        add("/System/Library/Fonts/Supplemental/Arial Unicode.ttf".into(), "Arial Unicode MS");
        add("/Library/Fonts/Arial Unicode.ttf".into(), "Arial Unicode MS");
    }
    #[cfg(target_os = "windows")]
    {
        let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        add(root.join("Fonts").join("sylfaen.ttf"), "Sylfaen");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Debian/Ubuntu, Arch, Fedora (static and variable) and FreeBSD's ports.
        for path in [
            "/usr/share/fonts/truetype/noto/NotoSansGeorgian-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansGeorgian-Regular.ttf",
            "/usr/share/fonts/google-noto/NotoSansGeorgian-Regular.ttf",
            "/usr/share/fonts/google-noto-vf/NotoSansGeorgian[wght].ttf",
            "/usr/local/share/fonts/noto/NotoSansGeorgian-Regular.ttf",
        ] {
            add(path.into(), "Noto Sans Georgian");
        }
        // DejaVu Sans draws Georgian too, and most desktops have it.
        for path in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/local/share/fonts/dejavu/DejaVuSans.ttf",
        ] {
            add(path.into(), "DejaVu Sans");
        }
    }
    out
}

/// `file` in macOS's downloaded font assets:
/// `/System/Library/AssetsV2/com_apple_MobileAsset_Font*/*.asset/AssetData/<file>`.
#[cfg(target_os = "macos")]
fn mac_font_assets(file: &str) -> Vec<PathBuf> {
    let dirs = |path: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(path).map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect()).unwrap_or_default()
    };
    let mut out = Vec::new();
    for catalog in dirs(Path::new("/System/Library/AssetsV2")) {
        if !catalog.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("com_apple_MobileAsset_Font")) {
            continue;
        }
        for asset in dirs(&catalog) {
            let path = asset.join("AssetData").join(file);
            if asset.extension().is_some_and(|e| e == "asset") && path.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_index_rejects_what_isnt_a_font() {
        assert_eq!(face_index(b"", "PingFang SC"), None);
        assert_eq!(face_index(b"ttcf\0\0\0\0garbage", "PingFang SC"), None);
        assert_eq!(face_index(&[0u8; 64], "PingFang SC"), None);
    }

    /// The baseline nudge reads a face's ascent and height; the reference is Inter's.
    #[test]
    fn face_metrics_reads_font_metrics() {
        let (ascent, height) = face_metrics(BODY_FONT, 0);
        assert!((ascent - 0.9688).abs() < 0.005, "{ascent}");
        assert!((height - 1.21).abs() < 0.005, "{height}");
        assert_eq!(face_metrics(b"not a font", 0), (0.0, 0.0));
    }

    /// A system CJK face sits on the Latin baseline, thanks to the nudge in [`load`]. Without a
    /// CJK font (CI, the web) there is nothing to check.
    #[test]
    fn cjk_sits_on_the_latin_baseline() {
        use egui::{Color32, FontFamily, FontId};
        for info in crate::i18n::LANGUAGES.iter().filter(|l| l.script.is_some()) {
            let Some(lang) = crate::i18n::Lang::from_code(info.code) else { continue };
            let Some(font) = crate::theme::cjk_font(lang) else { continue };
            let y_offset_factor = font.tweak.y_offset_factor;
            // A character the language's own catalog uses, so the sample fits its script: the face
            // must draw it, on the Latin baseline.
            let Some(sample) =
                info.source.lines().filter_map(|line| line.split('\t').nth(2)).flat_map(str::chars).find(|c| !c.is_ascii() && c.is_alphabetic())
            else {
                continue;
            };
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx, Some(lang));
            let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
            output.textures_delta.clear();
            let size = 20.0;
            let font_id = FontId::new(size, FontFamily::Proportional);
            let galley = ctx.fonts_mut(|f| f.layout_no_wrap(format!("A{sample}"), font_id.clone(), Color32::WHITE));
            assert!(ctx.fonts_mut(|f| f.has_glyphs(&font_id, &sample.to_string())), "{}: no glyph for {sample:?}", lang.code());
            let glyphs = &galley.rows[0].row.glyphs;
            let pos = |c: char| glyphs.iter().find(|g| g.chr == c).map(|g| g.pos.y);
            // `pos.y` is the baseline before the nudge (which moves the ink, not `pos`).
            if let (Some(latin), Some(cjk)) = (pos('A'), pos(sample)) {
                let rendered = cjk + y_offset_factor * size;
                assert!((rendered - latin).abs() < 1.0, "{}: latin {latin}, cjk {rendered}, nudge {y_offset_factor}", lang.code());
            }
        }
    }

    /// A hand-installed MiSans is looked for in the user's font folders on every system (#378).
    #[test]
    fn misans_is_looked_for_where_users_install_fonts() {
        let names: Vec<String> = chinese_candidates().iter().filter_map(|c| c.path.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        assert!(names.iter().any(|n| n == "MiSans-Regular.ttf"), "{names:?}");
        assert!(chinese_candidates().iter().filter(|c| c.family == "MiSans").all(|c| c.path.is_absolute() || c.path.starts_with("/")));
        // After the system's own faces: those are complete families with hinting the system tuned.
        let first_misans = chinese_candidates().iter().position(|c| c.family == "MiSans").unwrap();
        assert!(chinese_candidates()[..first_misans].iter().all(|c| c.family != "MiSans"));
        assert!(first_misans > 0, "the system's own fonts come first");
    }

    #[test]
    fn face_index_finds_families_in_real_fonts() {
        // The bundled Latin faces stand in for system fonts the test machine may not have.
        let inter: &[u8] = include_bytes!("../../../assets/fonts/Inter-Variable.ttf");
        assert_eq!(face_index(inter, "Inter"), Some(0));
        assert_eq!(face_index(inter, "PingFang SC"), None);
    }

    /// Whatever this machine has loads without panicking, and a found face really is the family's.
    #[test]
    fn system_font_loads_when_present() {
        for script in Script::ALL {
            if let Some(font) = load(script) {
                let family = candidates(script).into_iter().find(|c| c.path.is_file()).map(|c| c.family);
                assert!(family.is_some());
                assert!(FontRef::from_index(font.bytes(), font.index).is_ok());
            }
        }
    }
}
