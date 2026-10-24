mod common;

use std::fs;
use std::path::{Path, PathBuf};

use fontcore::{AddOptions, AddOutcome, AppId, Error, FontFormat, Issue, License, Manifest, Vault, inspect};
use tempfile::TempDir;

fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, data).unwrap();
    path
}

fn app(id: &str) -> AppId {
    AppId::new(id).unwrap()
}

fn setup() -> (TempDir, Vault) {
    let tmp = TempDir::new().unwrap();
    let vault = Vault::init(tmp.path().join("vault")).unwrap();
    (tmp, vault)
}

#[test]
fn inspect_reads_names() {
    let (format, faces) = inspect(&common::ttf("Story Sans", "Bold", "a")).unwrap();
    assert_eq!(format, FontFormat::Ttf);
    assert_eq!(faces.len(), 1);
    assert_eq!(faces[0].family, "Story Sans");
    assert_eq!(faces[0].subfamily, "Bold");
    assert_eq!(faces[0].full_name.as_deref(), Some("Story Sans Bold"));
    assert_eq!(faces[0].postscript_name.as_deref(), Some("StorySans-Bold"));
}

#[test]
fn inspect_reads_collections() {
    let data = common::ttc(&[common::ttf("Craft", "Regular", "a"), common::ttf("Craft", "Italic", "b")]);
    let (format, faces) = inspect(&data).unwrap();
    assert_eq!(format, FontFormat::Ttc);
    let styles: Vec<_> = faces.iter().map(|f| (f.index, f.subfamily.as_str(), f.enabled)).collect();
    assert_eq!(styles, [(0, "Regular", true), (1, "Italic", true)]);
}

#[test]
fn inspect_rejects_non_fonts() {
    assert!(matches!(inspect(b"wOF2...."), Err(Error::UnsupportedFormat(_))));
    assert!(matches!(inspect(b"hello world"), Err(Error::UnsupportedFormat(_))));
    assert!(matches!(inspect(&[0, 1, 0, 0, 0, 0]), Err(Error::InvalidFont(_))));
}

#[test]
fn open_requires_init() {
    let tmp = TempDir::new().unwrap();
    assert!(matches!(Vault::open(tmp.path()), Err(Error::NotInitialized(_))));
    Vault::init(tmp.path()).unwrap();
    Vault::init(tmp.path()).unwrap(); // idempotent
    assert!(Vault::open(tmp.path()).unwrap().manifest().unwrap().fonts.is_empty());
}

#[test]
fn add_copies_and_dedupes() {
    let (tmp, vault) = setup();
    let src = write(tmp.path(), "Story.ttf", &common::ttf("Story", "Regular", "a"));
    let options = AddOptions { tags: vec!["display".into()], license: License::parse("OFL-1.1"), ..Default::default() };

    let AddOutcome::Added(entry) = vault.add(&src, &options).unwrap() else {
        panic!("expected a new font");
    };
    assert_eq!(entry.id.len(), 16);
    assert_eq!(entry.file, format!("fonts/{}.ttf", entry.id));
    assert_eq!(entry.source.as_deref(), Some("Story.ttf"));
    assert_eq!(fs::read(vault.root().join(&entry.file)).unwrap(), fs::read(&src).unwrap());

    // Same bytes under another name are not imported twice.
    let copy = write(tmp.path(), "copy.ttf", &fs::read(&src).unwrap());
    assert!(matches!(vault.add(&copy, &options).unwrap(), AddOutcome::AlreadyPresent(e) if e.id == entry.id));

    let manifest = vault.manifest().unwrap();
    assert_eq!(manifest.fonts, vec![entry]);
    // The lock was released: another handle can take it straight away.
    let lock = fs::OpenOptions::new().write(true).open(vault.root().join(".fontcore.lock")).unwrap();
    lock.try_lock().unwrap();
}

#[test]
fn manifest_round_trips_through_json() {
    let (tmp, vault) = setup();
    let src = write(tmp.path(), "a.ttf", &common::ttf("A", "Regular", "a"));
    vault.add(&src, &AddOptions::default()).unwrap();

    let text = fs::read_to_string(vault.manifest_path()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["fonts"][0]["faces"][0]["family"], "A");
    assert_eq!(json["fonts"][0]["faces"][0]["enabled"], true);
    let added_at = json["fonts"][0]["added_at"].as_str().unwrap();
    assert!(added_at.ends_with('Z') && added_at.len() == 20, "{added_at}");
    // Empty optional fields are omitted to keep the file readable.
    assert!(json["fonts"][0].get("apps").is_none());
}

#[test]
fn other_schema_versions_are_rejected() {
    let (_tmp, vault) = setup();
    for version in [0, 2, 99] {
        fs::write(vault.manifest_path(), format!(r#"{{"schema_version": {version}, "fonts": []}}"#)).unwrap();
        assert!(matches!(vault.manifest(), Err(Error::UnsupportedSchema { found, .. }) if found == version));
    }
}

#[test]
fn ids_resolve_by_prefix() {
    let (tmp, vault) = setup();
    let a = write(tmp.path(), "a.ttf", &common::ttf("A", "Regular", "a"));
    let id = vault.add(&a, &AddOptions::default()).unwrap().entry().id.clone();
    assert_eq!(vault.get(&id[..4]).unwrap().id, id);
    assert!(matches!(vault.get("zzzz"), Err(Error::NotFound(_))));
    assert!(matches!(vault.get(""), Err(Error::NotFound(_))));

    let mut manifest = vault.manifest().unwrap();
    let mut twin = manifest.fonts[0].clone();
    twin.id = format!("{}0", &id[..4]);
    manifest.fonts.push(twin);
    manifest.save(&vault.manifest_path()).unwrap();
    assert!(matches!(vault.get(&id[..4]), Err(Error::Ambiguous(_, 2))));
}

#[test]
fn remove_deletes_file() {
    let (tmp, vault) = setup();
    let src = write(tmp.path(), "a.ttf", &common::ttf("A", "Regular", "a"));
    let entry = vault.add(&src, &AddOptions::default()).unwrap().entry().clone();
    vault.remove(&entry.id).unwrap();
    assert!(vault.manifest().unwrap().fonts.is_empty());
    assert!(!vault.root().join(&entry.file).exists());
    assert!(matches!(vault.remove(&entry.id), Err(Error::NotFound(_))));
}

#[test]
fn fonts_for_app_respects_scope_and_state() {
    let (tmp, vault) = setup();
    let photo = "ai.storytold.photocraft";
    let add = |name: &str, apps: &[&str]| {
        let src = write(tmp.path(), &format!("{name}.ttf"), &common::ttf(name, "Regular", name));
        let options = AddOptions {
            // Duplicates are dropped on import.
            apps: apps.iter().chain(apps).map(|s| app(s)).collect(),
            ..Default::default()
        };
        vault.add(&src, &options).unwrap().entry().clone()
    };
    add("Global", &[]);
    assert_eq!(add("PhotoOnly", &[photo]).apps, [app(photo)]);
    let hidden = add("Hidden", &[]).id;
    vault.set_enabled(&hidden, false).unwrap();

    let families = |id: &str| -> Vec<String> {
        let set = vault.fonts_for_app(&app(id)).unwrap();
        assert!(set.unavailable.is_empty());
        set.fonts.iter().map(|f| f.entry.family().to_owned()).collect()
    };
    assert_eq!(families(photo), ["Global", "PhotoOnly"]);
    assert_eq!(families("ai.storytold.wordcraft"), ["Global"]);

    vault.set_enabled(&hidden, true).unwrap();
    assert_eq!(families("ai.storytold.wordcraft"), ["Global", "Hidden"]);

    let set = vault.fonts_for_app(&app("ai.storytold.pdfcraft")).unwrap();
    assert_eq!(set.fonts[0].read().unwrap(), fs::read(&set.fonts[0].path).unwrap());
}

/// Variable fonts are stored like any other TTF/OTF: the file is copied
/// byte for byte, so its axes and named instances reach apps intact, but the
/// manifest records only the default face.
#[test]
fn variable_fonts_import_as_ordinary_files() {
    let (tmp, vault) = setup();
    let data = common::variable_ttf("Story Flex");
    let parsed = ttf_parser::Face::parse(&data, 0).unwrap();
    assert!(parsed.is_variable());
    assert_eq!(parsed.variation_axes().len(), 1);

    let src = write(tmp.path(), "StoryFlex[wght].ttf", &data);
    let entry = vault.add(&src, &AddOptions::default()).unwrap().entry().clone();
    assert_eq!(entry.format, FontFormat::Ttf);
    assert_eq!(entry.faces.len(), 1);
    assert_eq!(entry.faces[0].family, "Story Flex");

    let set = vault.fonts_for_app(&app("ai.storytold.photocraft")).unwrap();
    let stored = set.fonts[0].read().unwrap();
    assert_eq!(stored, data);
    let axes = ttf_parser::Face::parse(&stored, 0).unwrap().variation_axes();
    let axis = axes.get(0).unwrap();
    assert_eq!((axis.tag, axis.min_value, axis.max_value), (ttf_parser::Tag::from_bytes(b"wght"), 100.0, 900.0));
}

#[test]
fn faces_toggle_individually() {
    let (tmp, vault) = setup();
    let data = common::ttc(&[common::ttf("Craft", "Regular", "a"), common::ttf("Craft", "Italic", "b")]);
    let src = write(tmp.path(), "craft.ttc", &data);
    let id = vault.add(&src, &AddOptions::default()).unwrap().entry().id.clone();
    let wordcraft = app("ai.storytold.wordcraft");
    let active = || -> Vec<u32> {
        let set = vault.fonts_for_app(&wordcraft).unwrap();
        set.fonts.iter().flat_map(|f| f.faces().map(|face| face.index)).collect()
    };
    assert_eq!(active(), [0, 1]);

    let entry = vault.set_face_enabled(&id, 1, false).unwrap();
    assert!(entry.enabled && !entry.faces[1].enabled);
    assert_eq!(active(), [0]);

    // Disabling every face hides the file, as does disabling the entry.
    vault.set_face_enabled(&id, 0, false).unwrap();
    assert!(active().is_empty());
    vault.set_face_enabled(&id, 0, true).unwrap();
    vault.set_enabled(&id, false).unwrap();
    assert!(active().is_empty());
    vault.set_enabled(&id, true).unwrap();
    assert_eq!(active(), [0]);

    assert!(matches!(vault.set_face_enabled(&id, 2, true), Err(Error::NoSuchFace { index: 2, .. })));
}

#[test]
fn unsafe_paths_are_rejected() {
    let (tmp, vault) = setup();
    let src = write(tmp.path(), "a.ttf", &common::ttf("A", "Regular", "a"));
    let mut entry = vault.add(&src, &AddOptions::default()).unwrap().entry().clone();
    for bad in ["", "/etc/x.ttf", "fonts//x.ttf", "./x.ttf", "fonts/../x.ttf", "fonts\\x.ttf", "C:/x.ttf"] {
        entry.file = bad.into();
        assert_eq!(entry.path_in(vault.root()), None, "{bad:?}");
    }
    entry.file = "fonts/x.ttf".into();
    assert_eq!(entry.path_in(vault.root()), Some(vault.root().join("fonts").join("x.ttf")));
}

#[test]
fn loader_reports_missing_and_unsafe_entries() {
    let (tmp, vault) = setup();
    let a = write(tmp.path(), "a.ttf", &common::ttf("A", "Regular", "a"));
    let b = write(tmp.path(), "b.ttf", &common::ttf("B", "Regular", "b"));
    let a = vault.add(&a, &AddOptions::default()).unwrap().entry().clone();
    vault.add(&b, &AddOptions::default()).unwrap();
    fs::remove_file(vault.root().join(&a.file)).unwrap();

    let mut manifest = vault.manifest().unwrap();
    manifest.fonts[1].file = "../outside.ttf".into();
    manifest.save(&vault.manifest_path()).unwrap();
    fs::write(vault.root().join("../outside.ttf"), b"x").unwrap();

    let set = vault.fonts_for_app(&app("ai.storytold.wordcraft")).unwrap();
    assert!(set.fonts.is_empty());
    assert_eq!(set.unavailable.len(), 2);
}

#[test]
fn verify_finds_problems() {
    let (tmp, vault) = setup();
    let a = write(tmp.path(), "a.ttf", &common::ttf("A", "Regular", "a"));
    let b = write(tmp.path(), "b.ttf", &common::ttf("B", "Regular", "b"));
    let a = vault.add(&a, &AddOptions::default()).unwrap().entry().clone();
    let b = vault.add(&b, &AddOptions::default()).unwrap().entry().clone();
    assert!(vault.verify().unwrap().is_empty());

    fs::remove_file(vault.root().join(&a.file)).unwrap();
    fs::write(vault.root().join(&b.file), b"tampered").unwrap();
    let orphan = write(&vault.root().join("fonts"), "stray.ttf", b"x");

    let issues = vault.verify().unwrap();
    assert_eq!(issues.len(), 3);
    assert!(issues.contains(&Issue::MissingFile { id: a.id, path: vault.root().join(&a.file) }));
    assert!(issues.contains(&Issue::HashMismatch { id: b.id, path: vault.root().join(&b.file) }));
    assert!(issues.contains(&Issue::Orphan { path: orphan }));
}

#[test]
fn init_waits_for_a_writer_and_keeps_its_manifest() {
    // Another process is creating the vault: it holds the lock while it writes a manifest with a
    // font in it. An init that starts meanwhile must wait, then leave that manifest alone.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("vault");
    fs::create_dir_all(&root).unwrap();
    let holder = fs::File::create(root.join(".fontcore.lock")).unwrap();
    holder.lock().unwrap();
    let writer_root = root.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut manifest = Manifest::default();
        manifest.extra.insert("written-by".into(), "other writer".into());
        manifest.save(&writer_root.join("fonts.json")).unwrap();
        drop(holder);
    });
    let started = std::time::Instant::now();
    let vault = Vault::init(&root).unwrap();
    // Without the lock, init would have checked and written straight away, before the writer.
    assert!(started.elapsed() >= std::time::Duration::from_millis(250), "init did not wait for the lock");
    writer.join().unwrap();
    assert_eq!(vault.manifest().unwrap().extra["written-by"], "other writer");
}

#[test]
fn a_lock_file_nobody_holds_does_not_block() {
    // What a crashed writer leaves behind: the file, but no lock on it.
    let (tmp, vault) = setup();
    fs::File::create(vault.root().join(".fontcore.lock")).unwrap();
    let src = write(tmp.path(), "a.ttf", &common::ttf("A", "Regular", "a"));
    vault.add(&src, &AddOptions::default()).unwrap();
}

#[test]
fn a_held_lock_makes_writers_wait_however_old_it_is() {
    let (tmp, vault) = setup();
    let lock = vault.root().join(".fontcore.lock");
    let holder = fs::File::create(&lock).unwrap();
    holder.lock().unwrap();
    // Old enough that the previous age-based rule would have taken it over.
    holder.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120)).unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(holder);
    });
    let src = write(tmp.path(), "a.ttf", &common::ttf("A", "Regular", "a"));
    let started = std::time::Instant::now();
    vault.add(&src, &AddOptions::default()).unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(250), "the add went ahead while the lock was held");
    release.join().unwrap();
    assert_eq!(vault.manifest().unwrap().fonts.len(), 1);
}

#[test]
fn removing_then_adding_the_same_font_keeps_its_file() {
    // The path is content-addressed, so the re-add recreates the very file remove deleted.
    let (tmp, vault) = setup();
    let src = write(tmp.path(), "a.ttf", &common::ttf("A", "Regular", "a"));
    let entry = vault.add(&src, &AddOptions::default()).unwrap().entry().clone();
    vault.remove(&entry.id).unwrap();
    let again = vault.add(&src, &AddOptions::default()).unwrap();
    assert!(matches!(again, AddOutcome::Added(_)));
    assert!(vault.root().join(&entry.file).is_file());
    assert!(vault.verify().unwrap().is_empty());
}

#[test]
fn default_manifest_is_current_schema() {
    assert_eq!(Manifest::default().schema_version, fontcore::SCHEMA_VERSION);
}
