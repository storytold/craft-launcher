use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;

fn service(id: &str, name: &str, cli: Option<PathBuf>) -> LocalService {
    LocalService { id: id.into(), name: name.into(), location: String::new(), cli_path: cli }
}

fn read(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

#[test]
fn the_app_list_has_every_app_with_a_cli_in_the_gateways_format() {
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("photocraft-cli.exe");
    let list = apps_json(&[
        service("photocraft", "PhotoCraft", Some(cli.clone())),
        service("wordcraft", "WordCraft", None),
        service("relcraft", "RelCraft", Some(PathBuf::from("relative/cli"))),
    ]);
    assert_eq!(list["version"], 1);
    let apps = list["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 1, "apps without an absolute CLI are left out");
    assert_eq!(apps[0]["id"], "photocraft");
    assert_eq!(apps[0]["name"], "PhotoCraft");
    assert_eq!(apps[0]["command"], cli.to_string_lossy().as_ref());
    assert_eq!(apps[0]["args"], json!(["mcp"]));
}

#[test]
fn switching_on_and_off() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!enabled(dir.path()));
    write_apps(dir.path(), &[]).unwrap();
    assert!(enabled(dir.path()));
    remove_apps(dir.path()).unwrap();
    assert!(!enabled(dir.path()));
    remove_apps(dir.path()).unwrap();
}

#[test]
fn register_creates_a_config_and_keeps_other_servers_and_keys() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = dir.path().join("new").join("mcp.json");
    let gw = dir.path().join("craft-ai-mcp.exe");
    register(&fresh, &gw).unwrap();
    assert_eq!(read(&fresh)["mcpServers"][SERVER_KEY]["args"], json!(["--stdio"]));
    assert!(registered(&fresh));

    let existing = dir.path().join("claude.json");
    std::fs::write(&existing, r#"{"theme":"dark","mcpServers":{"other":{"command":"x"}}}"#).unwrap();
    register(&existing, &gw).unwrap();
    let v = read(&existing);
    assert_eq!(v["theme"], "dark");
    assert_eq!(v["mcpServers"]["other"]["command"], "x");
    assert_eq!(v["mcpServers"][SERVER_KEY]["command"], gw.to_string_lossy().as_ref());

    unregister(&existing).unwrap();
    let v = read(&existing);
    assert!(v["mcpServers"].get(SERVER_KEY).is_none());
    assert_eq!(v["mcpServers"]["other"]["command"], "x");
    assert!(!registered(&existing));
}

#[test]
fn a_broken_config_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let gw = dir.path().join("craft-ai-mcp");
    for (name, text) in [("bad.json", "{ not json"), ("array.json", "[1,2]"), ("servers.json", r#"{"mcpServers":[1]}"#)] {
        let p = dir.path().join(name);
        std::fs::write(&p, text).unwrap();
        assert!(register(&p, &gw).is_err(), "{name}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text, "{name} changed");
    }
    let bad = dir.path().join("bad.json");
    assert!(unregister(&bad).is_err());
    assert!(!registered(&bad));
}

#[test]
fn an_empty_config_counts_as_fresh_and_a_missing_one_needs_no_unregister() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("empty.json");
    std::fs::write(&p, "  \n").unwrap();
    register(&p, &dir.path().join("gw")).unwrap();
    assert!(registered(&p));
    unregister(&dir.path().join("missing.json")).unwrap();
    assert!(!dir.path().join("missing.json").exists());
}

#[test]
fn unregister_leaves_a_file_without_our_entry_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("c.json");
    let text = "{\"mcpServers\": {\"other\": {}}}";
    std::fs::write(&p, text).unwrap();
    unregister(&p).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
}

#[test]
fn a_folder_in_place_of_the_config_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mcp.json");
    std::fs::create_dir(&p).unwrap();
    assert!(register(&p, &dir.path().join("gw")).is_err());
}

#[test]
fn every_client_has_a_name_and_paths() {
    for c in CLIENTS {
        assert!(!client_name(c).is_empty());
        if let Some((config, folder)) = client_paths(c) {
            assert!(config.is_absolute() && folder.is_absolute());
        }
    }
}
