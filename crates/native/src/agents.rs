//! AI agents and the Craft apps: one switch in Settings, and one entry in each MCP client.
//!
//! With the switch on, the launcher writes the installed apps and their CLIs into
//! `ArtCraft/mcp/apps.json` (the list the `craft-ai-mcp` gateway reads, see
//! `docs/CRAFT-AI-MCP.md` in storytold/craft-ai-integrations), and can add one stdio entry,
//! `craft-apps` → `craft-ai-mcp --stdio`, to Claude Desktop, Claude Code, Cursor and LM Studio.
//! Through that one entry an agent reaches every installed app, without one entry per app.
//!
//! Turning the switch off removes the list and the entries this module added. A client
//! configuration that does not parse is never overwritten, and entries other than ours are kept.

use std::path::{Path, PathBuf};

use artcraft_launcher_core::protocol::{McpAgents, McpClient, McpClientState};
use serde_json::{Map, Value, json};

use crate::local_mcp::LocalService;

/// The app list the gateway reads.
pub const APPS_FILE: &str = "apps.json";
/// Same override the gateway honours (tests, portable installs).
pub const DIR_ENV: &str = "CRAFT_MCP_DIR";
/// The key our entry has in a client's `mcpServers`.
pub const SERVER_KEY: &str = "craft-apps";
/// Largest client configuration read; anything bigger is not a configuration file.
const MAX_CONFIG_BYTES: u64 = 64 << 20;

/// The gateway's folder: [`DIR_ENV`], otherwise `ArtCraft/mcp` in the per-user local
/// configuration folder (beside the AI layer's `ArtCraft/ai`).
pub fn mcp_dir() -> Option<PathBuf> {
    match std::env::var_os(DIR_ENV) {
        Some(d) if !d.is_empty() => Some(PathBuf::from(d)),
        _ => dirs::config_local_dir().map(|b| b.join("ArtCraft").join("mcp")),
    }
}

/// The app list for `services`: each app with a CLI, started as `<cli> mcp`. Apps without a CLI
/// beside them are left out.
pub fn apps_json(services: &[LocalService]) -> Value {
    let apps: Vec<Value> = services
        .iter()
        .filter_map(|s| {
            let cli = s.cli_path.as_ref().filter(|p| p.is_absolute())?;
            Some(json!({"id": s.id, "name": s.name, "command": cli, "args": ["mcp"]}))
        })
        .collect();
    json!({"version": 1, "apps": apps})
}

/// Whether agents are on: the app list exists in `dir`.
pub fn enabled(dir: &Path) -> bool {
    dir.join(APPS_FILE).is_file()
}

/// Write (or refresh) the app list in `dir`.
pub fn write_apps(dir: &Path, services: &[LocalService]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&apps_json(services)).map_err(|e| e.to_string())?;
    crate::paths::write_atomic(&dir.join(APPS_FILE), text.as_bytes())
}

/// Remove the app list in `dir`. A missing list is fine.
pub fn remove_apps(dir: &Path) -> Result<(), String> {
    match std::fs::remove_file(dir.join(APPS_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("couldn't remove {} ({e})", dir.join(APPS_FILE).display())),
    }
}

/// Every client the launcher can add the entry to.
pub const CLIENTS: [McpClient; 4] = [McpClient::ClaudeDesktop, McpClient::ClaudeCode, McpClient::Cursor, McpClient::LmStudio];

/// Where `client` keeps its MCP servers on this machine, and the folder whose presence shows the
/// client is installed.
pub fn client_paths(client: McpClient) -> Option<(PathBuf, PathBuf)> {
    let home = dirs::home_dir()?;
    Some(match client {
        McpClient::ClaudeDesktop => {
            let dir = dirs::config_dir()?.join("Claude");
            (dir.join("claude_desktop_config.json"), dir)
        }
        McpClient::ClaudeCode => (home.join(".claude.json"), home.join(".claude")),
        McpClient::Cursor => (home.join(".cursor").join("mcp.json"), home.join(".cursor")),
        McpClient::LmStudio => (home.join(".lmstudio").join("mcp.json"), home.join(".lmstudio")),
    })
}

/// A client's display name.
pub fn client_name(client: McpClient) -> &'static str {
    match client {
        McpClient::ClaudeDesktop => "Claude Desktop",
        McpClient::ClaudeCode => "Claude Code",
        McpClient::Cursor => "Cursor",
        McpClient::LmStudio => "LM Studio",
    }
}

/// Read a client configuration: `Ok(None)` when it does not exist yet. A file that is not a JSON
/// object is an error, so it is never replaced.
fn read_config(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("couldn't read {} ({e})", path.display())),
    };
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return Err(format!("{} is not a configuration file the launcher can change", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("couldn't read {} ({e})", path.display()))?;
    // An empty file is a fresh configuration.
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Some(Map::new()));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(m)) => Ok(Some(m)),
        Ok(_) | Err(_) => Err(format!("{} is not valid JSON; the launcher leaves it unchanged", path.display())),
    }
}

/// Whether `config` holds our entry.
pub fn registered(config: &Path) -> bool {
    matches!(read_config(config), Ok(Some(m)) if m.get("mcpServers").and_then(|s| s.get(SERVER_KEY)).is_some())
}

/// Add (or update) our entry in the client configuration at `config`, starting `gateway`.
/// Everything else in the file is kept.
pub fn register(config: &Path, gateway: &Path) -> Result<(), String> {
    let mut root = read_config(config)?.unwrap_or_default();
    let servers = root.entry("mcpServers").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(format!("`mcpServers` in {} is not an object; the launcher leaves it unchanged", config.display()));
    };
    servers.insert(SERVER_KEY.to_string(), json!({"command": gateway.to_string_lossy(), "args": ["--stdio"]}));
    let text = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())?;
    crate::paths::write_atomic(config, text.as_bytes())
}

/// Remove our entry from the client configuration at `config`. Nothing to do when the file or
/// the entry is missing.
pub fn unregister(config: &Path) -> Result<(), String> {
    let Some(mut root) = read_config(config)? else {
        return Ok(());
    };
    let removed = match root.get_mut("mcpServers") {
        Some(Value::Object(servers)) => servers.remove(SERVER_KEY).is_some(),
        _ => false,
    };
    if !removed {
        return Ok(());
    }
    let text = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())?;
    crate::paths::write_atomic(config, text.as_bytes())
}

/// The `craft-ai-mcp` gateway binary: beside the launcher, or `ARTCRAFT_LAUNCHER_BRIDGE_PATH`.
pub fn locate_gateway() -> Option<PathBuf> {
    crate::local_mcp::locate_bridge()
}

/// What Settings shows. While agents are on, the app list is refreshed first, so an app
/// installed since then is reachable too (a failed refresh keeps the previous list).
pub fn state(services: &[LocalService]) -> McpAgents {
    let dir = mcp_dir();
    let on = dir.as_deref().is_some_and(enabled);
    if on && let Some(d) = &dir {
        let _ = write_apps(d, services);
    }
    let clients = CLIENTS
        .iter()
        .map(|c| {
            let paths = client_paths(*c);
            McpClientState {
                client: *c,
                name: client_name(*c).to_string(),
                installed: paths.as_ref().is_some_and(|(config, folder)| folder.is_dir() || config.is_file()),
                added: paths.as_ref().is_some_and(|(config, _)| registered(config)),
            }
        })
        .collect();
    McpAgents {
        gateway_found: locate_gateway().is_some(),
        enabled: on,
        apps: services.iter().filter(|s| s.cli_path.as_ref().is_some_and(|p| p.is_absolute())).count(),
        clients,
    }
}

/// Turn agents on (write the app list) or off (remove it and every client entry this module
/// added). All clients are tried; the first error is returned.
pub fn set_enabled(on: bool, services: &[LocalService]) -> Result<(), String> {
    let dir = mcp_dir().ok_or("couldn't find the per-user configuration folder")?;
    if on {
        if locate_gateway().is_none() {
            return Err("the Craft gateway (craft-ai-mcp) is not installed beside the launcher".into());
        }
        return write_apps(&dir, services);
    }
    let mut first_error = remove_apps(&dir).err();
    for c in CLIENTS {
        if let Some((config, _)) = client_paths(c)
            && let Err(e) = unregister(&config)
        {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Add the gateway entry to `client`, or remove it. Adding needs agents on and the gateway found.
pub fn set_client(client: McpClient, added: bool) -> Result<(), String> {
    let (config, _) = client_paths(client).ok_or("couldn't find the home folder")?;
    if !added {
        return unregister(&config);
    }
    if !mcp_dir().as_deref().is_some_and(enabled) {
        return Err("turn on \"Let AI agents use Craft apps\" first".into());
    }
    let gateway = locate_gateway().ok_or("the Craft gateway (craft-ai-mcp) is not installed beside the launcher")?;
    register(&config, &gateway)
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
