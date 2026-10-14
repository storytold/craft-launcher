//! Turns `assets/app-icons/alternate_icons/` into the icon pack registry.
//!
//! The packs are data — a folder per pack, `<app>.svg` per app, one row per pack in `packs.tsv`
//! — so adding one means adding files, not editing Rust. `include_bytes!` and `include_str!`,
//! though, only take a *literal* path, so the table has to exist at compile time: this script
//! reads the folders and writes it out, and `crate::icons` `include!`s the result.
//!
//! It fails the build rather than emit a table that doesn't match the folders: a pack with no
//! `packs.tsv` row would be invisible in the UI, and a `<app>.svg` without the render
//! `packaging/icons.sh` writes beside it would break the build later and more confusingly.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Where the packs are, relative to this crate: what `include_bytes!` in the generated file needs.
const ASSETS: &str = "../../assets/app-icons/alternate_icons";

fn main() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join(ASSETS);
    println!("cargo:rerun-if-changed={}", assets.join("packs.tsv").display());
    println!("cargo:rerun-if-changed={}", assets.display());

    let packs = read_packs(&assets).unwrap_or_else(|e| fail(&e));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("icon_packs.rs");
    if let Err(e) = std::fs::write(&out, render(&packs)) {
        fail(&format!("couldn't write {}: {e}", out.display()));
    }
}

fn fail(why: &str) -> ! {
    eprintln!("error: {why}");
    std::process::exit(1)
}

/// One pack: its folder (also its id), its English label, and the apps it was drawn for.
struct Pack {
    folder: String,
    label: String,
    apps: Vec<String>,
}

/// The `packs.tsv` rows, each checked against the folder it names.
fn read_packs(assets: &Path) -> Result<Vec<Pack>, String> {
    let registry = assets.join("packs.tsv");
    let text = std::fs::read_to_string(&registry).map_err(|e| format!("couldn't read {}: {e}", registry.display()))?;

    let mut packs: Vec<Pack> = Vec::new();
    let mut labels = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [folder, label] = fields.as_slice() else {
            return Err(format!("{} line {}: expected `folder<TAB>label`, got {line:?}", registry.display(), i + 1));
        };
        if folder.is_empty() || label.is_empty() {
            return Err(format!("{} line {}: empty folder or label", registry.display(), i + 1));
        }
        // The folder goes into a Rust string literal in the generated file.
        if folder.contains(['"', '\\']) || label.contains(['"', '\\']) {
            return Err(format!("{} line {}: a folder or label may not hold a quote or backslash", registry.display(), i + 1));
        }
        if !labels.insert(*label) {
            return Err(format!("{} line {}: {label:?} is used by two packs", registry.display(), i + 1));
        }
        let dir = assets.join(folder);
        if !dir.is_dir() {
            return Err(format!("{} line {}: {folder} isn't a folder in {}", registry.display(), i + 1, assets.display()));
        }
        packs.push(Pack { folder: (*folder).to_string(), label: (*label).to_string(), apps: read_apps(&dir, folder)? });
    }

    // A folder of artwork nobody listed would be silently unreachable, so say so.
    for folder in pack_folders(assets) {
        if !packs.iter().any(|p| p.folder == folder) {
            return Err(format!("{folder} holds icon artwork but has no row in {} (see its README.md)", registry.display()));
        }
    }
    if packs.is_empty() {
        return Err(format!("{} lists no packs", registry.display()));
    }
    Ok(packs)
}

/// The `<app>.svg` files of one pack, by stem: the app ids it was drawn for.
fn read_apps(dir: &Path, folder: &str) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("couldn't read {}: {e}", dir.display()))?;
    let mut apps = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        match path.extension().and_then(|e| e.to_str()) {
            Some("svg") => {
                // The render beside it is what actually gets bundled; without it the artwork
                // isn't ready, and `packaging/icons.sh` is what writes it.
                if !path.with_extension("png").is_file() {
                    return Err(format!(
                        "{folder}/{stem}.svg has no {stem}.png beside it; run packaging/icons.sh (it needs resvg) and commit the render"
                    ));
                }
                check_square(&path, folder, &stem)?;
                apps.push(stem);
            }
            Some("png") if !path.with_extension("svg").is_file() => {
                return Err(format!("{folder}/{stem}.png has no {stem}.svg beside it; the supplied artwork is what the render comes from"));
            }
            _ => {}
        }
    }
    if apps.is_empty() {
        return Err(format!("{folder} holds no `<app>.svg` files"));
    }
    apps.sort();
    Ok(apps)
}

/// `packaging/icons.sh` renders at exactly 512 × 512, so artwork whose view box isn't square comes
/// out stretched. Best effort — an SVG with no readable `viewBox` is left to `resvg` — but it
/// catches the usual mistake here, where the message can say what to do about it.
fn check_square(path: &Path, folder: &str, app: &str) -> Result<(), String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(()) };
    let Some(view_box) = text.split("viewBox=\"").nth(1).and_then(|rest| rest.split('"').next()) else { return Ok(()) };
    let numbers: Vec<f32> = view_box.split_whitespace().filter_map(|n| n.parse().ok()).collect();
    match numbers.as_slice() {
        [_, _, width, height] if (width - height).abs() > 0.5 => {
            Err(format!("{folder}/{app}.svg has a {width}×{height} view box; icons are rendered square, so it would come out stretched"))
        }
        _ => Ok(()),
    }
}

/// Every directory under `assets` that holds icon artwork, as a path relative to it. Packs sit
/// either directly under `assets` (`mono`) or one level down (`initial-icons/dark`).
fn pack_folders(assets: &Path) -> Vec<String> {
    let holds_svg = |dir: &Path| dir.read_dir().is_ok_and(|d| d.flatten().any(|e| e.path().extension().is_some_and(|x| x == "svg")));
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(assets) else { return out };
    for entry in entries.flatten().filter(|e| e.path().is_dir()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if holds_svg(&entry.path()) {
            out.push(name);
            continue;
        }
        if let Ok(children) = entry.path().read_dir() {
            for child in children.flatten().filter(|c| c.path().is_dir() && holds_svg(&c.path())) {
                out.push(format!("{name}/{}", child.file_name().to_string_lossy()));
            }
        }
    }
    out
}

/// The generated `icon_packs.rs`.
fn render(packs: &[Pack]) -> String {
    let mut out = String::from("// Generated by crates/core/build.rs from assets/app-icons/alternate_icons/. DO NOT EDIT.\n\n");

    out.push_str("/// The bundled icon packs, in the order `packs.tsv` lists them.\n");
    out.push_str("pub const PACKS: &[IconPack] = &[\n");
    for pack in packs {
        let _ = writeln!(out, "    IconPack {{ id: {:?}, label: {:?} }},", pack.folder, pack.label);
    }
    out.push_str("];\n\n");

    out.push_str("/// (pack id, app id, the 512 px render, the supplied SVG) for every app a pack was drawn for.\n");
    out.push_str("const ARTWORK: &[(&str, &str, &[u8], &str)] = &[\n");
    for pack in packs {
        for app in &pack.apps {
            let file = |ext: &str| format!("concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{ASSETS}/{}/{app}.{ext}\")", pack.folder);
            let _ = writeln!(out, "    ({:?}, {:?}, include_bytes!({}), include_str!({})),", pack.folder, app, file("png"), file("svg"));
        }
    }
    out.push_str("];\n");
    out
}
