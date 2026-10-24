//! Checks `schema/fonts.v1.json` against the Rust manifest types, so the
//! published schema and the library cannot drift apart.

mod common;

use std::fs;
use std::path::PathBuf;

use fontcore::{AddOptions, AppId, License, Manifest, Vault};
use jsonschema::Validator;
use serde_json::{Value, json};
use tempfile::TempDir;

fn schema() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema/fonts.v1.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn validator() -> Validator {
    jsonschema::draft202012::options().should_validate_formats(true).build(&schema()).unwrap()
}

/// A manifest written by the vault that uses every field: a scoped TTF with
/// an SPDX license and a collection with a disabled face and a custom license.
fn vault_manifest() -> Value {
    let tmp = TempDir::new().unwrap();
    let vault = Vault::init(tmp.path().join("vault")).unwrap();
    let ttf = tmp.path().join("Story-Bold.ttf");
    fs::write(&ttf, common::ttf("Story", "Bold", "a")).unwrap();
    let ttc = tmp.path().join("Craft.ttc");
    fs::write(&ttc, common::ttc(&[common::ttf("Craft", "Regular", "b"), common::ttf("Craft", "Italic", "c")])).unwrap();

    vault
        .add(
            &ttf,
            &AddOptions {
                apps: vec![AppId::new("ai.storytold.photocraft").unwrap()],
                tags: vec!["display".into()],
                license: License::parse("OFL-1.1"),
            },
        )
        .unwrap();
    let craft = vault.add(&ttc, &AddOptions { license: Some(License::custom("Licensed to Acme for internal use")), ..Default::default() }).unwrap();
    vault.set_face_enabled(&craft.entry().id, 1, false).unwrap();
    serde_json::from_str(&fs::read_to_string(vault.manifest_path()).unwrap()).unwrap()
}

fn rust_accepts(doc: &Value) -> bool {
    Manifest::from_json(&doc.to_string()).is_ok()
}

fn keys(v: &Value) -> Vec<String> {
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

#[test]
fn schema_is_valid_draft_2020_12() {
    jsonschema::draft202012::meta::validate(&schema()).unwrap();
}

#[test]
fn vault_output_matches_schema() {
    let doc = vault_manifest();
    let errors: Vec<String> = validator().iter_errors(&doc).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:#?}\n{doc:#}");
}

/// Every field the library writes is described by the schema, and the
/// schema describes no field the library does not know.
#[test]
fn schema_properties_match_rust_fields() {
    let schema = schema();
    let doc = vault_manifest();
    let font = &doc["fonts"][0];
    assert_eq!(keys(&schema["properties"]), keys(&doc));
    assert_eq!(keys(&schema["$defs"]["font"]["properties"]), keys(font));
    assert_eq!(keys(&schema["$defs"]["face"]["properties"]), keys(&font["faces"][0]));
}

/// A field is required by the schema exactly when the library refuses a
/// manifest without it.
#[test]
fn schema_required_fields_match_rust() {
    let schema = schema();
    let base = vault_manifest();
    let cases = [(&schema, vec![]), (&schema["$defs"]["font"], vec!["fonts", "0"]), (&schema["$defs"]["face"], vec!["fonts", "0", "faces", "0"])];
    for (def, at) in cases {
        let required: Vec<&str> = def["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        for key in def["properties"].as_object().unwrap().keys() {
            let mut doc = base.clone();
            let mut obj = &mut doc;
            for step in &at {
                obj = match step.parse::<usize>() {
                    Ok(i) => &mut obj[i],
                    Err(_) => &mut obj[*step],
                };
            }
            obj.as_object_mut().unwrap().remove(key);
            let is_required = required.contains(&key.as_str());
            assert_eq!(rust_accepts(&doc), !is_required, "removing {at:?}.{key}");
            assert_eq!(validator().is_valid(&doc), !is_required, "removing {at:?}.{key}");
        }
    }
}

type Edit = Box<dyn Fn(&mut Value)>;

/// Edits applied to a valid manifest, and whether the result is valid.
fn cases() -> Vec<(&'static str, bool, Edit)> {
    let font = |f: fn(&mut Value)| -> Edit { Box::new(move |d| f(&mut d["fonts"][0])) };
    vec![
        (
            "unknown fields everywhere",
            true,
            Box::new(|d| {
                d["ai.storytold.photocraft:recent"] = json!(["x"]);
                d["fonts"][0]["ai.storytold.photocraft:pinned"] = json!(true);
                d["fonts"][0]["faces"][0]["ai.storytold.wordcraft:alias"] = json!("Story");
                d["fonts"][1]["license"]["url"] = json!("https://example.com/eula");
            }),
        ),
        ("timestamp with offset and fraction", true, font(|f| f["added_at"] = json!("2026-10-22T04:30:00.5+02:00"))),
        ("SPDX expression", true, font(|f| f["license"] = json!("MIT OR Apache-2.0"))),
        ("empty app list", true, font(|f| f["apps"] = json!([]))),
        ("newer schema version", false, Box::new(|d| d["schema_version"] = json!(2))),
        ("schema version 0", false, Box::new(|d| d["schema_version"] = json!(0))),
        ("plain app name", false, font(|f| f["apps"] = json!(["photocraft"]))),
        ("uppercase app id", false, font(|f| f["apps"] = json!(["ai.storytold.PhotoCraft"]))),
        ("two-label app id", false, font(|f| f["apps"] = json!(["storytold.photocraft"]))),
        ("unix seconds timestamp", false, font(|f| f["added_at"] = json!(1_792_636_200))),
        ("timestamp without offset", false, font(|f| f["added_at"] = json!("2026-10-22T02:30:00"))),
        ("timestamp with space", false, font(|f| f["added_at"] = json!("2026-10-22 02:30:00Z"))),
        ("impossible date", false, font(|f| f["added_at"] = json!("2026-02-30T00:00:00Z"))),
        ("numeric license", false, font(|f| f["license"] = json!(42))),
        ("license object without custom", false, font(|f| f["license"] = json!({"text": "EULA"}))),
        ("web font format", false, font(|f| f["format"] = json!("woff2"))),
        ("non-boolean face toggle", false, font(|f| f["faces"][0]["enabled"] = json!("no"))),
        ("non-boolean entry toggle", false, font(|f| f["enabled"] = json!(1))),
    ]
}

#[test]
fn rust_and_schema_agree() {
    let validator = validator();
    let base = vault_manifest();
    assert!(rust_accepts(&base));
    for (name, valid, edit) in cases() {
        let mut doc = base.clone();
        edit(&mut doc);
        assert_eq!(rust_accepts(&doc), valid, "library on '{name}'");
        assert_eq!(validator.is_valid(&doc), valid, "schema on '{name}'");
    }
}

#[test]
fn unknown_fields_survive_rewrites() {
    let tmp = TempDir::new().unwrap();
    let vault = Vault::init(tmp.path()).unwrap();
    let src = tmp.path().join("a.ttf");
    fs::write(&src, common::ttf("A", "Regular", "a")).unwrap();
    let id = vault.add(&src, &AddOptions { license: Some(License::custom("EULA")), ..Default::default() }).unwrap().entry().id.clone();

    let mut doc: Value = serde_json::from_str(&fs::read_to_string(vault.manifest_path()).unwrap()).unwrap();
    doc["x-top"] = json!({"nested": [1, 2]});
    doc["fonts"][0]["x-font"] = json!("kept");
    doc["fonts"][0]["faces"][0]["x-face"] = json!(3.5);
    doc["fonts"][0]["license"]["x-license"] = json!(null);
    fs::write(vault.manifest_path(), doc.to_string()).unwrap();

    vault.set_enabled(&id, false).unwrap();

    let after: Value = serde_json::from_str(&fs::read_to_string(vault.manifest_path()).unwrap()).unwrap();
    doc["fonts"][0]["enabled"] = json!(false);
    assert_eq!(after, doc);
}
