//! Exposes installed Craft Apps as stdio MCP servers in LM Studio's Bionic config.
//!
//! The launcher already knows which Craft Apps are on this machine (see [`detect`](crate::detect)).
//! This module turns that into `ng-mcp.json` entries so LM Studio / Bionic can drive each app's
//! built-in tools over MCP stdio. Every path comes from detection, which reads the user's own
//! install location (`InstallLocation` on Windows, the App bundle or package dir elsewhere). Nothing
//! here hardcodes a folder; [`mcp_config_path`] derives LM Studio's config file from the user
//! profile and is overridable via `ARTCRAFT_LAUNCHER_MCP_CONFIG` for tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use artcraft_launcher_core::CraftApp;
use serde::{Deserialize, Serialize};

use crate::detect;
use crate::manifest::Manifest;

/// A Craft App that has a usable CLI on this machine and can be exposed as an MCP server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalService {
    /// The app id (`wordcraft`), stable across versions.
    pub id: String,
    /// Display name (`WordCraft`).
    pub name: String,
    /// The detected main executable path; the CLI lives beside it.
    pub location: String,
    /// The `-cli` binary that speaks MCP stdio. `None` when detection found the app but no CLI
    /// sits next to it.
    #[serde(default)]
    pub cli_path: Option<PathBuf>,
}

/// One entry in LM Studio's `ng-mcp.json` `servers` array, matching the file's shape exactly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEntry {
    /// Stable id for this server; reused across toggles so Bionic keeps its own state keyed to it.
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub connection: Connection,
}

/// The transport a host uses to reach the server. Mirrors `ng-mcp.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Connection {
    /// A local process the host spawns. `command` is the CLI path; `args` are its arguments.
    #[serde(rename = "stdio")]
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    /// A network endpoint the host connects to (reserved for HTTP-based services like the bridge).
    #[serde(rename = "streamableHttp")]
    StreamableHttp {
        url: String,
        headers: HashMap<String, String>,
        auth: Option<Auth>,
    },
}

/// Credentials a streamable-HTTP host sends. `None` means no auth; Bionic writes this field only
/// when present.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Auth {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "bearerToken")]
    BearerToken { token: String },
}

/// The path to LM Studio / Bionic's MCP server config on this machine.
///
/// Derived from the user profile, never hardcoded. Overridable via `ARTCRAFT_LAUNCHER_MCP_CONFIG`
/// (tests point it at a temp file). Returns `None` when the home directory can't be resolved.
pub fn mcp_config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ARTCRAFT_LAUNCHER_MCP_CONFIG") {
        return Some(PathBuf::from(&p));
    }
    Some(
        dirs::home_dir()?
            .join(".lmstudio")
            .join("apps")
            .join("bionic")
            .join(".internal")
            .join("ng-mcp.json"),
    )
}

/// Load the `servers` array from a config file. A missing, unreadable, or malformed file yields an
/// empty list rather than an error, so a fresh install starts with no servers and never crashes.
pub fn load_servers(path: &Path) -> Vec<ServerEntry> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return vec![],
    };
    let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    match value.get("servers").and_then(|s| s.as_array()) {
        Some(arr) => arr
            .iter()
            .filter_map(|e| serde_json::from_value(e.clone()).ok())
            .collect(),
        None => vec![],
    }
}

/// Save `servers` back to the config file, preserving any top-level keys other than `servers`.
/// Writes atomically (temp file + rename) so a crash never leaves a half-written config.
pub fn save_servers(path: &Path, servers: &[ServerEntry]) -> Result<(), String> {
    let existing: Option<serde_json::Value> = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let array = match servers.iter().map(serde_json::to_value).collect::<Result<Vec<serde_json::Value>, _>>() {
        Ok(a) => a,
        Err(e) => return Err(e.to_string()),
    };
    // Preserve any top-level keys other than `servers`; replace the array. If the file was not
    // a JSON object, it is not an `ng-mcp.json` shape and starts fresh.
    let mut map = serde_json::Map::new();
    if let Some(existing_value) = existing {
        if let Some(obj) = existing_value.as_object() {
            for (k, v) in obj {
                if k != "servers" {
                    map.insert(k.clone(), v.clone());
                }
            }
        }
    }
    map.insert("servers".to_string(), serde_json::json!(array));
    let out = serde_json::to_string_pretty(&serde_json::Value::Object(map))
        .map_err(|e| e.to_string())?;
    crate::paths::write_atomic(path, out.as_bytes())
}

/// Every Craft App that has a usable CLI on this machine, in suite order. Detection is the single
/// source of truth for "installed"; this just adds the CLI path each app would expose to MCP.
pub fn installed_services(manifest: &Manifest, snapshot: &detect::Snapshot) -> Vec<LocalService> {
    artcraft_launcher_core::SUITE
        .iter()
        .filter_map(|app| {
            let installed = detect::detect(app, manifest, snapshot)?;
            Some(LocalService {
                id: app.id.to_string(),
                name: app.name.to_string(),
                location: installed.location.clone(),
                cli_path: cli_path_for(&installed.location, app),
            })
        })
        .collect()
}

/// The `-cli` binary beside a detected main executable. Tries every stem the app has shipped as,
/// current first, and both the bare name (Unix) and `.exe` (Windows). `None` when no CLI exists
/// next to it.
fn cli_path_for(location: &str, app: &CraftApp) -> Option<PathBuf> {
    let dir = Path::new(location).parent()?;
    for stem in app.stems() {
        for ext in ["", ".exe"] {
            let candidate = dir.join(format!("{stem}-cli{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The stable id this module uses for a given app, so enable/disable/remove agree.
pub fn entry_id(service_id: &str) -> String {
    format!("craft-ai-mcp-{}", service_id)
}

/// The stdio `ServerEntry` for a service, ready to drop into `ng-mcp.json`. Returns `None` when
/// the service has no usable CLI.
pub fn entry_for(service: &LocalService) -> Option<ServerEntry> {
    let cli = service.cli_path.clone()?;
    Some(ServerEntry {
        id: entry_id(&service.id),
        name: service.name.clone(),
        description: format!("{} built-in tools over MCP.", service.name),
        enabled: true,
        connection: Connection::Stdio {
            command: cli.display().to_string(),
            args: vec!["mcp".into()],
            env: HashMap::new(),
        },
    })
}

/// Add or update the entry for `service`, enabling it. Returns false when there was no usable CLI.
pub fn enable_service(servers: &mut Vec<ServerEntry>, service: &LocalService) -> bool {
    let Some(entry) = entry_for(service) else { return false };
    match servers.iter_mut().find(|e| e.id == entry.id) {
        Some(existing) => {
            *existing = entry;
            existing.enabled = true;
        }
        None => servers.push(entry),
    }
    true
}

/// Disable the entry for `service_id`, leaving it in the list (Bionic keeps a disabled row).
/// Returns false when there was nothing to disable.
pub fn disable_service(servers: &mut Vec<ServerEntry>, service_id: &str) -> bool {
    let id = entry_id(service_id);
    match servers.iter_mut().find(|e| e.id == id) {
        Some(e) => {
            if e.enabled {
                e.enabled = false;
                true
            } else {
                false
            }
        }
        None => false,
    }
}

/// Remove the entry for `service_id` entirely. Returns false when there was nothing to remove.
pub fn remove_service(servers: &mut Vec<ServerEntry>, service_id: &str) -> bool {
    let id = entry_id(service_id);
    let before = servers.len();
    servers.retain(|e| e.id != id);
    servers.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use artcraft_launcher_core::catalog::AppKind;

    /// A minimal Craft App for CLI-path tests. Only `id` and `former_ids` matter (they feed
    /// [`CraftApp::stems`]); the rest are inert literals. String literals carry `'static`, which
    /// every field here requires.
    fn fake_app(id: &'static str, former_ids: &'static [&'static str]) -> CraftApp {
        CraftApp {
            id,
            name: "TestCraft",
            tagline: "A test app.",
            color: [0; 3],
            ink_light: [0; 3],
            ink_dark: [0; 3],
            repo: "storyteller/example",
            former_ids,
            former_names: &[],
            kind: AppKind::Official,
            publisher: "ArtCraft",
            msi_publisher: None,
            bundle: None,
            former_bundles: &[],
            site: None,
            mac_team: None,
        }
    }

    #[test]
    fn cli_path_prefers_the_current_stem_and_windows_extension() {
        let dir = tempfile::tempdir().unwrap();
        // Main exe (what detection returns) plus a CLI under the current stem.
        std::fs::write(dir.path().join("wordcraft.exe"), b"").unwrap();
        std::fs::write(dir.path().join("wordcraft-cli.exe"), b"cli").unwrap();
        let app = fake_app("wordcraft", &[]);
        assert_eq!(
            cli_path_for(&dir.path().join("wordcraft.exe").display().to_string(), &app),
            Some(dir.path().join("wordcraft-cli.exe"))
        );
    }

    #[test]
    fn cli_path_falls_back_to_a_former_stem() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pdfcraft.exe"), b"").unwrap();
        // Only the former stem's CLI exists (e.g. an older install renamed to printcraft).
        std::fs::write(dir.path().join("printcraft-cli.exe"), b"cli").unwrap();
        let app = fake_app("pdfcraft", &["printcraft"]);
        assert_eq!(
            cli_path_for(&dir.path().join("pdfcraft.exe").display().to_string(), &app),
            Some(dir.path().join("printcraft-cli.exe"))
        );
    }

    #[test]
    fn cli_path_is_none_when_no_cli_sits_beside_the_exe() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wordcraft.exe"), b"").unwrap();
        let app = fake_app("wordcraft", &[]);
        assert_eq!(cli_path_for(&dir.path().join("wordcraft.exe").display().to_string(), &app), None);
    }

    #[test]
    fn cli_path_is_none_when_the_location_has_no_parent() {
        // A bare file name with no directory component has no parent to look in.
        let app = fake_app("wordcraft", &[]);
        assert_eq!(cli_path_for("wordcraft.exe", &app), None);
    }

    #[test]
    fn save_then_load_round_trips_and_preserves_other_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ng-mcp.json");
        // Seed a top-level key that is not `servers` to prove it survives.
        std::fs::write(&path, r#"{"meta":{"note":"keep me"},"servers":[]}"#).unwrap();

        let servers = vec![
            ServerEntry {
                id: entry_id("wordcraft"),
                name: "WordCraft".into(),
                description: "WordCraft built-in tools over MCP.".into(),
                enabled: true,
                connection: Connection::Stdio {
                    command: dir.path().join("wordcraft-cli.exe").display().to_string(),
                    args: vec!["mcp".into()],
                    env: HashMap::new(),
                },
            },
        ];
        assert!(save_servers(&path, &servers).is_ok());

        let loaded = load_servers(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, entry_id("wordcraft"));
        assert!(loaded[0].enabled);
        match &loaded[0].connection {
            Connection::Stdio { args, .. } => {
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], "mcp");
            }
            _ => panic!("expected a stdio connection"),
        };

        // The non-`servers` top-level key must survive the round trip.
        let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(raw["meta"]["note"], serde_json::json!("keep me"));
    }

    #[test]
    fn load_returns_empty_for_missing_malformed_and_serverless_files() {
        let dir = tempfile::tempdir().unwrap();

        // Missing file.
        assert_eq!(load_servers(&dir.path().join("nope.json")), vec![]);

        // Malformed JSON.
        std::fs::write(dir.path().join("bad.json"), b"{ not json").unwrap();
        assert_eq!(load_servers(&dir.path().join("bad.json")), vec![]);

        // Valid JSON with no `servers` key.
        std::fs::write(dir.path().join("plain.json"), r#"{"other": 1}"#).unwrap();
        assert_eq!(load_servers(&dir.path().join("plain.json")), vec![]);
    }

    #[test]
    fn save_creates_the_file_when_it_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ng-mcp.json");
        assert!(save_servers(&path, &[]).is_ok());
        assert_eq!(load_servers(&path), vec![]);
    }

    #[test]
    fn enable_disable_remove_manage_the_server_list() {
        let mut servers: Vec<ServerEntry> = Vec::new();
        let service = LocalService {
            id: "wordcraft".into(),
            name: "WordCraft".into(),
            location: "/x/wordcraft.exe".into(),
            cli_path: Some("/x/wordcraft-cli.exe".into()),
        };

        // Enable adds it.
        assert!(enable_service(&mut servers, &service));
        assert_eq!(servers.len(), 1);
        assert!(servers[0].enabled);
        assert_eq!(servers[0].id, entry_id("wordcraft"));

        // Enabling again is idempotent (no duplicate row).
        assert!(enable_service(&mut servers, &service));
        assert_eq!(servers.len(), 1);

        // Disable keeps the row but turns it off.
        assert!(disable_service(&mut servers, "wordcraft"));
        assert!(!servers[0].enabled);
        assert_eq!(servers.len(), 1);

        // Disabling again is a no-op that reports false.
        assert!(!disable_service(&mut servers, "wordcraft"));
        assert!(!servers[0].enabled);

        // Remove takes the row out.
        assert!(remove_service(&mut servers, "wordcraft"));
        assert_eq!(servers.len(), 0);
        assert!(!remove_service(&mut servers, "wordcraft"));
    }

    #[test]
    fn entry_for_is_none_without_a_cli() {
        let service = LocalService {
            id: "wordcraft".into(),
            name: "WordCraft".into(),
            location: "/x/wordcraft.exe".into(),
            cli_path: None,
        };
        assert!(entry_for(&service).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn detects_the_apps_installed_on_this_machine_when_requested() {
        // Gated on an env var so it only runs where the suite is actually installed (a dev box),
        // never in CI. Set ARTCRAFT_LAUNCHER_VERIFY_INSTALLED=1 to run it.
        if std::env::var("ARTCRAFT_LAUNCHER_VERIFY_INSTALLED").is_err() {
            return;
        }
        let paths = crate::paths::Paths::new().unwrap();
        let manifest = Manifest::load(&paths.manifest());
        let services = installed_services(&manifest, &detect::Snapshot::take());
        for s in &services {
            assert!(s.cli_path.is_some(), "service {} has no CLI path: {:?}", s.id, s.location);
        }
        // WordCraft and PdfCraft are installed on the machine this test runs against.
        assert!(services.iter().any(|s| s.id == "wordcraft"), "expected wordcraft to be detected");
        assert!(services.iter().any(|s| s.id == "pdfcraft"), "expected pdfcraft to be detected");
    }
}
