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

/// Index of an entry whose display name matches `name`, or None. Matching by name (rather than
/// id) lets us recognize and manage entries created with a different id, e.g. UUIDs from an
/// earlier launcher version.
fn find_by_name(servers: &[ServerEntry], name: &str) -> Option<usize> {
    servers.iter().position(|e| e.name == name)
}

/// Add or update the entry for `service`, enabling it. Manages any existing entry with the same
/// display name in place (keeping its id, refreshing the command); otherwise creates one under
/// the deterministic id. Returns false when there was no usable CLI.
pub fn enable_service(servers: &mut Vec<ServerEntry>, service: &LocalService) -> bool {
    let Some(cli) = service.cli_path.as_deref() else { return false };
    match find_by_name(servers, &service.name) {
        Some(i) => {
            servers[i].enabled = true;
            if let Connection::Stdio { command: c, .. } = &mut servers[i].connection {
                *c = cli.display().to_string();
            }
        }
        None => servers.push(ServerEntry {
            id: entry_id(&service.id),
            name: service.name.clone(),
            description: format!("{} built-in tools over MCP.", service.name),
            enabled: true,
            connection: Connection::Stdio {
                command: cli.display().to_string(),
                args: vec!["mcp".into()],
                env: HashMap::new(),
            },
        }),
    }
    true
}

/// Disable the entry for `service`, leaving it in the list (Bionic keeps a disabled row). Matches
/// by display name first, then the deterministic id. Returns false when there was nothing to disable.
pub fn disable_service(servers: &mut Vec<ServerEntry>, service: &LocalService) -> bool {
    if let Some(i) = find_by_name(servers, &service.name) {
        if servers[i].enabled {
            servers[i].enabled = false;
            return true;
        }
        return false;
    }
    let id = entry_id(&service.id);
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

/// Remove the entry for `service`. Matches by display name first, then the deterministic id.
/// Returns false when there was nothing to remove.
pub fn remove_service(servers: &mut Vec<ServerEntry>, service: &LocalService) -> bool {
    if let Some(i) = find_by_name(servers, &service.name) {
        servers.remove(i);
        return true;
    }
    let id = entry_id(&service.id);
    let before = servers.len();
    servers.retain(|e| e.id != id);
    servers.len() != before
}

/// Whether an entry with display name `name` is currently enabled.
pub fn service_enabled(servers: &[ServerEntry], name: &str) -> bool {
    find_by_name(servers, name).map_or(false, |i| servers[i].enabled)
}

/// Port the unified `craft-ai-mcp` bridge listens on by default.
pub const BRIDGE_PORT: u16 = 12345;

/// Stable id for the single unified-bridge entry in `ng-mcp.json`.
pub const BRIDGE_ID: &str = "craft-ai-mcp";

/// Locate the unified `craft-ai-mcp` bridge binary, if present.
///
/// Checks, in order:
/// - `ARTCRAFT_LAUNCHER_BRIDGE_PATH` (dev/test override);
/// - a small set of derived install locations.
/// Returns `None` when no bridge is found so callers can fall back to per-app stdio entries.
pub fn locate_bridge() -> Option<PathBuf> {
    let override_path = std::env::var("ARTCRAFT_LAUNCHER_BRIDGE_PATH").ok().map(PathBuf::from);
    first_existing(override_path.as_deref(), &candidate_bridge_paths())
}

/// First existing path among `override` (checked first) then `candidates`.
fn first_existing(override_path: Option<&Path>, candidates: &[PathBuf]) -> Option<PathBuf> {
    if let Some(p) = override_path {
        return p.is_file().then_some(PathBuf::from(p));
    }
    candidates.iter().find(|p| p.is_file()).cloned()
}

/// Candidate locations for the bridge binary. Derived from this launcher's own install, not
/// hardcoded to one machine's layout; extend as the ship story settles.
fn candidate_bridge_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(exe) = std::env::current_exe().ok()
        && let Some(dir) = exe.parent() {
        for name in ["craft-ai-mcp.exe", "craft-ai-mcp"] {
            out.push(dir.join(name));
        }
    }
    out
}

/// The bridge binary's version, when readable. Reuses the launcher's existing PE-version reader so
/// the two branches agree on how a version is read; `None` off-Windows or without version info.
#[cfg(windows)]
pub fn bridge_version(path: &Path) -> Option<artcraft_launcher_core::version::Version> {
    crate::detect::windows::executable_version(path)
}

#[cfg(not(windows))]
pub fn bridge_version(_path: &Path) -> Option<artcraft_launcher_core::version::Version> {
    None
}

/// The single `streamableHttp` entry for the unified bridge at `port`. Named "Craft AI" so it is
/// recognized (and managed in place) even when an earlier launcher version created it under a UUID.
pub fn bridge_entry(port: u16) -> ServerEntry {
    ServerEntry {
        id: BRIDGE_ID.to_string(),
        name: "Craft AI".to_string(),
        description: "All Craft AI tools over one MCP endpoint.".to_string(),
        enabled: true,
        connection: Connection::StreamableHttp {
            url: format!("http://127.0.0.1:{port}/mcp"),
            headers: HashMap::new(),
            auth: Some(Auth::None),
        },
    }
}

/// Index of the unified-bridge entry, or None: one whose id is the deterministic bridge id, or a
/// `streamableHttp` entry named "Craft AI" (created by an earlier launcher version under a UUID).
fn find_bridge(servers: &[ServerEntry]) -> Option<usize> {
    servers.iter().position(|e| e.id == BRIDGE_ID)
        .or_else(|| {
            servers.iter().position(|e| {
                matches!(&e.connection, Connection::StreamableHttp { .. }) && e.name == "Craft AI"
            })
        })
}

/// The current port of the unified-bridge entry (parsed from its `streamableHttp` URL), or None
/// when there is no bridge entry.
pub fn bridge_port_current(servers: &[ServerEntry]) -> Option<u16> {
    find_bridge(servers).and_then(|i| match &servers[i].connection {
        Connection::StreamableHttp { url, .. } => parse_port(url),
        _ => None,
    })
}

/// Whether the unified-bridge entry is currently present and enabled.
pub fn bridge_enabled(servers: &[ServerEntry]) -> bool {
    find_bridge(servers).map_or(false, |i| servers[i].enabled)
}

/// Port from a `streamableHttp` URL like `http://127.0.0.1:12345/mcp`, or None when absent.
fn parse_port(url: &str) -> Option<u16> {
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() < 3 {
        return None;
    }
    // parts[0] is the scheme, parts[2] is `host[:port]`.
    let host_port = parts[2];
    let port_str = host_port.rsplit_once(':').map(|(_, p)| p)?;
    port_str.parse::<u16>().ok()
}

/// Add or update the unified-bridge entry, enabling it. Manages any existing bridge entry in place
/// (keeping its id, refreshing the URL); otherwise creates one under the deterministic id. Returns
/// false for a zero port.
pub fn enable_bridge(servers: &mut Vec<ServerEntry>, port: u16) -> bool {
    if port == 0 {
        return false;
    }
    match find_bridge(servers) {
        Some(i) => {
            servers[i].enabled = true;
            if let Connection::StreamableHttp { url, .. } = &mut servers[i].connection {
                *url = format!("http://127.0.0.1:{port}/mcp");
            }
        }
        None => servers.push(bridge_entry(port)),
    }
    true
}

/// Disable the unified-bridge entry (leaving it in the list). Returns false when absent or already off.
pub fn disable_bridge(servers: &mut Vec<ServerEntry>) -> bool {
    match find_bridge(servers) {
        Some(i) => {
            if servers[i].enabled {
                servers[i].enabled = false;
                true
            } else {
                false
            }
        }
        None => false,
    }
}

/// Remove the unified-bridge entry entirely. Returns false when absent.
pub fn remove_bridge(servers: &mut Vec<ServerEntry>) -> bool {
    match find_bridge(servers) {
        Some(i) => {
            servers.remove(i);
            true
        }
        None => false,
    }
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

        // Disable keeps the row but turns it off (matched by display name).
        assert!(disable_service(&mut servers, &service));
        assert!(!servers[0].enabled);
        assert_eq!(servers.len(), 1);

        // Disabling again is a no-op that reports false.
        assert!(!disable_service(&mut servers, &service));
        assert!(!servers[0].enabled);

        // Remove takes the row out.
        assert!(remove_service(&mut servers, &service));
        assert_eq!(servers.len(), 0);
        assert!(!remove_service(&mut servers, &service));
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

    #[test]
    fn bridge_entry_matches_the_streamable_http_shape() {
        let entry = bridge_entry(BRIDGE_PORT);
        assert_eq!(entry.id, BRIDGE_ID);
        assert_eq!(entry.name, "Craft AI");
        assert!(entry.enabled);
        match &entry.connection {
            Connection::StreamableHttp { url, auth, .. } => {
                assert_eq!(url, "http://127.0.0.1:12345/mcp");
                assert_eq!(*auth, Some(Auth::None));
            }
            _ => panic!("expected a streamableHttp connection"),
        };
    }

    #[test]
    fn bridge_entry_uses_the_requested_port() {
        let entry = bridge_entry(9000);
        match &entry.connection {
            Connection::StreamableHttp { url, .. } => assert_eq!(url, "http://127.0.0.1:9000/mcp"),
            _ => panic!("expected a streamableHttp connection"),
        };
    }

    #[test]
    fn enable_disable_remove_bridge_manage_the_entry() {
        let mut servers: Vec<ServerEntry> = Vec::new();
        assert!(!enable_bridge(&mut servers, 0)); // zero port is refused
        assert_eq!(servers.len(), 0);

        assert!(enable_bridge(&mut servers, BRIDGE_PORT));
        assert_eq!(servers.len(), 1);
        assert!(servers[0].enabled);
        assert_eq!(servers[0].id, BRIDGE_ID);

        // Enabling again is idempotent (no duplicate row).
        assert!(enable_bridge(&mut servers, BRIDGE_PORT));
        assert_eq!(servers.len(), 1);

        // Disabling turns it off.
        assert!(disable_bridge(&mut servers));
        assert!(!servers[0].enabled);
        // Disabling again is a no-op that reports false.
        assert!(!disable_bridge(&mut servers));

        // Remove takes the row out.
        assert!(remove_bridge(&mut servers));
        assert_eq!(servers.len(), 0);
        assert!(!remove_bridge(&mut servers));
    }

    #[test]
    fn first_existing_prefers_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("craft-ai-mcp.exe");
        std::fs::write(&present, b"").unwrap();
        let missing = dir.path().join("nope.exe");
        // Override wins even when a candidate also exists.
        assert_eq!(
            first_existing(Some(present.as_ref()), &[missing.clone(), present.clone()]),
            Some(present),
        );
    }

    #[test]
    fn first_existing_falls_back_to_candidates_and_reports_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.exe");
        let present = dir.path().join("craft-ai-mcp.exe");
        std::fs::write(&present, b"").unwrap();
        // No override: the first existing candidate wins.
        assert_eq!(first_existing(None, &[missing.clone(), present.clone()]), Some(present));
        // Nothing exists -> None.
        assert_eq!(first_existing(None, &[missing]), None);
    }

    /// Enabling a service manages an existing entry by display name (keeping its id) rather than
    /// creating a duplicate under the deterministic id.
    #[test]
    fn enable_service_manages_an_existing_entry_by_name() {
        let mut servers = vec![ServerEntry {
            id: "uuid-1".into(),
            name: "WordCraft".into(),
            description: "old".into(),
            enabled: false,
            connection: Connection::Stdio {
                command: "/old/wordcraft-cli.exe".into(),
                args: vec!["mcp".into()],
                env: HashMap::new(),
            },
        }];
        let service = LocalService {
            id: "wordcraft".into(),
            name: "WordCraft".into(),
            location: "/x/wordcraft.exe".into(),
            cli_path: Some("/x/wordcraft-cli.exe".into()),
        };
        assert!(enable_service(&mut servers, &service));
        // Same single entry, original id preserved, command refreshed.
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "uuid-1");
        assert!(servers[0].enabled);
        match &servers[0].connection {
            Connection::Stdio { command, .. } => assert_eq!(command, "/x/wordcraft-cli.exe"),
            _ => panic!("expected stdio"),
        }
    }

    /// Disabling matches by display name first (a UUID entry is found via its name).
    #[test]
    fn disable_service_matches_by_name_first() {
        let mut servers = vec![ServerEntry {
            id: "uuid-1".into(),
            name: "WordCraft".into(),
            description: String::new(),
            enabled: true,
            connection: Connection::Stdio {
                command: "/x/wordcraft-cli.exe".into(),
                args: vec!["mcp".into()],
                env: HashMap::new(),
            },
        }];
        let service = LocalService {
            id: "wordcraft".into(),
            name: "WordCraft".into(),
            location: "/x/wordcraft.exe".into(),
            cli_path: Some("/x/wordcraft-cli.exe".into()),
        };
        assert!(disable_service(&mut servers, &service));
        assert!(!servers[0].enabled);
        // The deterministic-id entry is not created.
        assert_eq!(servers.len(), 1);
    }

    /// Removing matches by display name first.
    #[test]
    fn remove_service_matches_by_name_first() {
        let mut servers = vec![ServerEntry {
            id: "uuid-1".into(),
            name: "WordCraft".into(),
            description: String::new(),
            enabled: true,
            connection: Connection::Stdio {
                command: "/x/wordcraft-cli.exe".into(),
                args: vec!["mcp".into()],
                env: HashMap::new(),
            },
        }];
        let service = LocalService {
            id: "wordcraft".into(),
            name: "WordCraft".into(),
            location: "/x/wordcraft.exe".into(),
            cli_path: Some("/x/wordcraft-cli.exe".into()),
        };
        assert!(remove_service(&mut servers, &service));
        assert_eq!(servers.len(), 0);
    }

    /// Enabling the bridge manages an existing "Craft AI" entry by name (keeping its id, refreshing
    /// the URL) rather than creating a duplicate.
    #[test]
    fn enable_bridge_manages_an_existing_entry_by_name() {
        let mut servers = vec![ServerEntry {
            id: "uuid-bridge".into(),
            name: "Craft AI".into(),
            description: String::new(),
            enabled: false,
            connection: Connection::StreamableHttp {
                url: "http://127.0.0.1:9999/mcp".into(),
                headers: HashMap::new(),
                auth: Some(Auth::None),
            },
        }];
        assert!(enable_bridge(&mut servers, BRIDGE_PORT));
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id, "uuid-bridge");
        assert!(servers[0].enabled);
        match &servers[0].connection {
            Connection::StreamableHttp { url, .. } => assert_eq!(url, "http://127.0.0.1:12345/mcp"),
            _ => panic!("expected streamableHttp"),
        }
    }

    /// bridge_port_current parses the port from an existing entry's URL.
    #[test]
    fn bridge_port_current_parses_the_url() {
        let servers = vec![ServerEntry {
            id: "uuid-bridge".into(),
            name: "Craft AI".into(),
            description: String::new(),
            enabled: true,
            connection: Connection::StreamableHttp {
                url: "http://127.0.0.1:9876/mcp".into(),
                headers: HashMap::new(),
                auth: Some(Auth::None),
            },
        }];
        assert_eq!(bridge_port_current(&servers), Some(9876));
    }

    /// bridge_enabled reports whether the existing entry is on.
    #[test]
    fn bridge_enabled_reports_the_existing_entry() {
        let mut servers = vec![ServerEntry {
            id: "uuid-bridge".into(),
            name: "Craft AI".into(),
            description: String::new(),
            enabled: true,
            connection: Connection::StreamableHttp {
                url: "http://127.0.0.1:12345/mcp".into(),
                headers: HashMap::new(),
                auth: Some(Auth::None),
            },
        }];
        assert!(bridge_enabled(&servers));
        servers[0].enabled = false;
        assert!(!bridge_enabled(&servers));
    }
}
