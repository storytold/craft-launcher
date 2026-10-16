//! The desktop store against a real folder: install a file, choose it, and see an app's `Reader`
//! (the code every Craft app uses) agree. The packages are built here from test colours.

use std::io::{Cursor, Write};

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::*;

fn app(s: &str) -> AppId {
    AppId::new(s).unwrap()
}

fn theme_json() -> Vec<u8> {
    serde_json::json!({
        "schema": 1,
        "palette": {
            "dark": true, "background": "#0f1720", "surface": "#18222e", "text": "#e8eef4", "muted_text": "#a9b7c6",
            "line": "#2b3a4a", "accent": "#4da3ff", "good": "#4cc38a", "warn": "#e0b04a", "bad": "#ff6b6b"
        }
    })
    .to_string()
    .into_bytes()
}

fn package(id: &str, name: &str, apps: &[&str], theme: &[u8]) -> Vec<u8> {
    let hex: String = Sha256::digest(theme).iter().map(|b| format!("{b:02x}")).collect();
    let compat: serde_json::Map<String, serde_json::Value> = apps.iter().map(|a| (a.to_string(), serde_json::json!({}))).collect();
    let manifest = serde_json::json!({
        "schema": 1, "id": id, "name": name, "version": "1.0.0", "kind": "theme", "publisher": {"name": "Example"},
        "compat": compat, "files": [{"path": "theme.json", "sha256": hex, "size": theme.len()}]
    })
    .to_string();
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("craft-extension.json", options).unwrap();
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.start_file("theme.json", options).unwrap();
    zip.write_all(theme).unwrap();
    zip.finish().unwrap().into_inner()
}

fn write_package(dir: &Path, file: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(file);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn a_file_added_in_the_launcher_is_listed_chosen_and_seen_by_an_app() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ext");
    let mut launcher = DesktopStore::at(root.clone());
    let file = write_package(dir.path(), "ocean.zip", &package("org.example.ocean", "Ocean", &["photocraft", "lightcraft"], &theme_json()));

    assert_eq!(launcher.rows().unwrap(), Vec::new(), "an empty store lists nothing");
    assert_eq!(launcher.add_file(&file).unwrap(), "Ocean");
    let rows = launcher.rows().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.tier, Tier::FromFile, "a file the person picked is recorded as such");
    assert_eq!(row.enabled.len(), 2, "installing switches it on for the apps it is made for");
    assert!(row.worn.is_empty(), "installing changes nobody's look");
    assert!(row.palette.as_ref().is_some_and(|p| p.dark));

    // An app, with no launcher, reads the same folder.
    let photocraft = Reader::open(&root);
    let before = photocraft.generation();
    assert!(photocraft.active_theme(&app("photocraft")).is_none());
    launcher.wear(&app("photocraft"), Some(&row.id)).unwrap();
    assert!(photocraft.generation() > before, "the running app can tell something changed");
    assert_eq!(photocraft.active_theme(&app("photocraft")).unwrap().name, "Ocean");
    assert!(photocraft.active_theme(&app("lightcraft")).is_none(), "another app keeps its own look until it chooses");
    assert_eq!(launcher.rows().unwrap()[0].worn.iter().map(|a| a.as_str()).collect::<Vec<_>>(), ["photocraft"]);

    launcher.set_enabled(&app("photocraft"), &row.id, false).unwrap();
    assert!(photocraft.active_theme(&app("photocraft")).is_none(), "switching it off gives the app its look back");
    assert!(launcher.rows().unwrap()[0].worn.is_empty());

    launcher.remove(&row.id).unwrap();
    assert!(launcher.rows().unwrap().is_empty());
}

#[test]
fn a_hostile_or_broken_file_is_refused_with_a_sentence_and_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut launcher = DesktopStore::at(dir.path().join("ext"));
    let not_a_package = write_package(dir.path(), "nope.zip", b"this is not a zip file");
    let low_contrast = serde_json::json!({
        "schema": 1,
        "palette": {
            "dark": true, "background": "#0f1720", "surface": "#18222e", "text": "#101820", "muted_text": "#a9b7c6",
            "line": "#2b3a4a", "accent": "#4da3ff", "good": "#4cc38a", "warn": "#e0b04a", "bad": "#ff6b6b"
        }
    })
    .to_string()
    .into_bytes();
    let unreadable = write_package(dir.path(), "dim.zip", &package("org.example.dim", "Dim", &["photocraft"], &low_contrast));
    let claims_first_party = write_package(dir.path(), "fake.zip", &package("org.storytold.fake", "Fake", &["photocraft"], &theme_json()));
    for file in [not_a_package, unreadable, claims_first_party, dir.path().join("missing.zip")] {
        let reason = launcher.add_file(&file).unwrap_err();
        assert!(!reason.is_empty() && reason.chars().next().is_some_and(char::is_lowercase), "{reason:?}");
        for word in ["zip", "manifest", "sha", "namespace", "unwrap", "panic"] {
            assert!(!reason.to_lowercase().contains(word), "{reason:?} mentions {word:?}");
        }
    }
    assert!(launcher.rows().unwrap().is_empty());
}

#[test]
fn a_theme_whose_file_goes_bad_after_install_loses_its_preview_without_failing_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ext");
    let mut launcher = DesktopStore::at(root.clone());
    let file = write_package(dir.path(), "ocean.zip", &package("org.example.ocean", "Ocean", &["photocraft"], &theme_json()));
    launcher.add_file(&file).unwrap();
    let id = launcher.rows().unwrap()[0].id.clone();
    // A second, fresh view of the same folder (no cached colours) after the file is damaged.
    std::fs::write(root.join("packages").join(id.as_str()).join("1.0.0").join("theme.json"), b"garbage").unwrap();
    let mut fresh = DesktopStore::at(root);
    let rows = fresh.rows().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].palette.is_none());
}

/// Writes three sample theme packages for hand testing, and proves each installs. Ignored by
/// default; run with `CRAFT_EXT_SAMPLES_OUT=<folder> cargo test -p artcraft-launcher --bin
/// artcraft-launcher -- --ignored write_sample_packages`.
#[test]
#[ignore = "writes files for hand testing"]
fn write_sample_packages() {
    let Some(out) = std::env::var_os("CRAFT_EXT_SAMPLES_OUT").map(PathBuf::from) else { return };
    std::fs::create_dir_all(&out).unwrap();
    let theme = |dark: bool, c: [&str; 9]| {
        serde_json::json!({
            "schema": 1,
            "palette": {
                "dark": dark, "background": c[0], "surface": c[1], "text": c[2], "muted_text": c[3], "line": c[4],
                "accent": c[5], "good": c[6], "warn": c[7], "bad": c[8]
            }
        })
        .to_string()
        .into_bytes()
    };
    let apps = ["photocraft", "lightcraft", "pdfcraft"];
    let samples = [
        (
            "sample-midnight.zip",
            "org.example.midnight",
            "Midnight",
            &apps[..],
            theme(true, ["#101820", "#18222d", "#e8eef4", "#a9b6c2", "#35424f", "#4aa3ff", "#3fd6b8", "#ffc14d", "#ff6259"]),
        ),
        (
            "sample-paper.zip",
            "org.example.paper",
            "Paper",
            &apps[..],
            theme(false, ["#f5f1e8", "#ece6d8", "#1d1b16", "#55504a", "#cdc5b3", "#1a5fb4", "#0a7563", "#8a5800", "#b3261e"]),
        ),
        (
            "sample-ocean.zip",
            "org.example.ocean",
            "Ocean",
            &apps[..1],
            theme(true, ["#0b2530", "#12384a", "#e6f4f7", "#a6c8d2", "#2c5b6e", "#3fd6b8", "#4cc38a", "#e0b04a", "#ff6b6b"]),
        ),
    ];
    let scratch = tempfile::tempdir().unwrap();
    let mut proof = DesktopStore::at(scratch.path().join("ext"));
    for (file, id, name, apps, colours) in samples {
        let path = write_package(&out, file, &package(id, name, apps, &colours));
        assert_eq!(proof.add_file(&path).unwrap(), name, "{file} must install");
    }
    assert_eq!(proof.rows().unwrap().iter().filter(|r| r.palette.is_some()).count(), 3);
}
