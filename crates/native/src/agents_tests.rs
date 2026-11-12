use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;

fn service(id: &str, name: &str, cli: Option<PathBuf>) -> InstalledApp {
    InstalledApp { id: id.into(), name: name.into(), location: String::new(), cli_path: cli }
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
fn apps_get_their_extra_mcp_arguments_only_where_the_cli_takes_them() {
    let dir = tempfile::tempdir().unwrap();
    let at = |name: &str| dir.path().join(name);
    let services = [
        service("photocraft", "PhotoCraft", Some(at("photocraft-cli"))),
        service("vectorcraft", "VectorCraft", Some(at("vectorcraft-cli"))),
        service("filmcraft", "FilmCraft", Some(at("filmcraft-cli"))),
    ];
    let asked = std::cell::RefCell::new(Vec::new());
    let new = apps_json_with(&services, &|cli: &Path, extra: &[&str]| {
        asked.borrow_mut().push((cli.file_name().unwrap().to_string_lossy().into_owned(), extra.join(" ")));
        true
    });
    let args = |list: &Value, i: usize| list["apps"][i]["args"].clone();
    assert_eq!(args(&new, 0), json!(["mcp", "--live"]), "PhotoCraft: the open window");
    assert_eq!(args(&new, 1), json!(["mcp", "--headless"]), "VectorCraft: off its unauthenticated port");
    assert_eq!(args(&new, 2), json!(["mcp"]), "other apps as they are");
    assert_eq!(
        *asked.borrow(),
        [("photocraft-cli".into(), "--live".into()), ("vectorcraft-cli".into(), "--headless".into())],
        "only apps with extras are asked"
    );
    // An older release that doesn't know the argument gets plain `mcp`, not a usage error.
    let old = apps_json_with(&services, &|_: &Path, _: &[&str]| false);
    for i in 0..3 {
        assert_eq!(args(&old, i), json!(["mcp"]));
    }
}

#[test]
fn claude_desktop_from_the_microsoft_store_is_found_in_its_package() {
    let dir = tempfile::tempdir().unwrap();
    let (config, local) = (dir.path().join("Roaming"), dir.path().join("Local"));
    assert_eq!(claude_desktop_dir(config.clone(), Some(&local)), config.join("Claude"), "nothing installed: the usual place");
    let packaged = local.join("Packages").join("Claude_pzs8sxrjxfjjc").join("LocalCache").join("Roaming").join("Claude");
    std::fs::create_dir_all(&packaged).unwrap();
    std::fs::create_dir_all(local.join("Packages").join("Other_123")).unwrap();
    assert_eq!(claude_desktop_dir(config.clone(), Some(&local)), packaged, "only an older Store package");
    std::fs::create_dir_all(config.join("Claude")).unwrap();
    assert_eq!(claude_desktop_dir(config.clone(), Some(&local)), config.join("Claude"), "the classic install wins over the package");
    assert_eq!(claude_desktop_dir(config.clone(), None), config.join("Claude"));
}

/// Current Windows versions keep their data in `%LOCALAPPDATA%\Claude-Data\<install>` and leave a
/// marker in the package folder they moved out of: the config there is no longer read.
#[test]
fn claude_desktop_data_moved_to_claude_data_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let (config, local) = (dir.path().join("Roaming"), dir.path().join("Local"));
    let packaged = local.join("Packages").join("Claude_pzs8sxrjxfjjc").join("LocalCache").join("Roaming").join("Claude");
    std::fs::create_dir_all(&packaged).unwrap();
    std::fs::write(packaged.join(".user-data-left.claude_pzs8sxrjxfjjc"), "{}").unwrap();
    assert_eq!(claude_desktop_dir(config.clone(), Some(&local)), config.join("Claude"), "a package folder the data left is skipped");
    std::fs::create_dir_all(config.join("Claude")).unwrap();
    let data = local.join("Claude-Data").join("claude_pzs8sxrjxfjjc");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(local.join("Claude-Data").join("README.txt"), "x").unwrap();
    assert_eq!(claude_desktop_dir(config.clone(), Some(&local)), data, "Claude-Data wins, its README is no install");
}

#[test]
fn the_help_text_must_name_the_argument_as_a_word() {
    let help = "Usage: photocraft-cli mcp [OPTIONS]
      --live
          Reach the open window
  vectorcraft-cli mcp [--connect ADDR | --headless]";
    assert!(help_names(help, &["--live"]));
    assert!(help_names(help, &["--headless"]), "inside brackets");
    assert!(!help_names("      --lively", &["--live"]));
    assert!(!help_names("bridge to a running app, or live", &["--live"]));
    assert!(!help_names(help, &["--live", "--when-closed"]), "every argument");
}

#[test]
fn a_cli_that_cannot_run_takes_no_extra_arguments() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!cli_accepts(&dir.path().join("missing-cli"), &["--live"]));
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

#[test]
fn the_gateway_is_copied_once_and_refreshed_when_it_changes() {
    let src_dir = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let src = src_dir.path().join("craft-ai-mcp");
    std::fs::write(&src, b"v1").unwrap();
    let copy = install_gateway(&src, dir.path()).unwrap();
    assert_eq!(copy, gateway_copy(dir.path()));
    assert!(copy.starts_with(dir.path()), "clients get a path in the launcher's folder, not the shipped one");
    assert_eq!(std::fs::read(&copy).unwrap(), b"v1");
    // Unchanged: kept as is.
    let before = std::fs::metadata(&copy).unwrap().modified().unwrap();
    install_gateway(&src, dir.path()).unwrap();
    assert_eq!(std::fs::metadata(&copy).unwrap().modified().unwrap(), before);
    // An update of another size replaces it.
    std::fs::write(&src, b"version 2").unwrap();
    install_gateway(&src, dir.path()).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"version 2");
    // No source: an error, not a panic.
    assert!(install_gateway(&src_dir.path().join("missing"), dir.path()).is_err());
}

#[cfg(unix)]
#[test]
fn the_gateway_copy_is_executable() {
    use std::os::unix::fs::PermissionsExt;
    let src_dir = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let src = src_dir.path().join("craft-ai-mcp");
    std::fs::write(&src, b"#!/bin/sh\n").unwrap();
    let copy = install_gateway(&src, dir.path()).unwrap();
    assert_eq!(std::fs::metadata(copy).unwrap().permissions().mode() & 0o111, 0o111);
}

#[test]
fn a_gateway_of_the_same_size_but_other_bytes_replaces_the_copy() {
    let src_dir = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let src = src_dir.path().join("craft-ai-mcp");
    std::fs::write(&src, b"aaaa").unwrap();
    let copy = install_gateway(&src, dir.path()).unwrap();
    // A new build of the same size whose timestamp is older than the copy.
    std::fs::write(&src, b"bbbb").unwrap();
    let old = std::fs::File::options().write(true).open(&src).unwrap();
    old.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000)).unwrap();
    drop(old);
    install_gateway(&src, dir.path()).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"bbbb");
}
