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
use std::sync::{Mutex, MutexGuard, PoisonError};

use artcraft_launcher_core::protocol::{FlagState, McpAgents, McpClient, McpClientState};
use serde_json::{Map, Value, json};

use artcraft_launcher_core::CraftApp;

use crate::detect;
use crate::manifest::Manifest;

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
pub fn apps_json(services: &[InstalledApp]) -> Value {
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
pub fn write_apps(dir: &Path, services: &[InstalledApp]) -> Result<(), String> {
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

/// An installed Craft app, with the CLI beside it that serves MCP (`<app>-cli mcp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApp {
    /// The app id (`wordcraft`), stable across versions.
    pub id: String,
    /// Display name (`WordCraft`).
    pub name: String,
    /// The detected main executable; the CLI lives beside it.
    pub location: String,
    /// The `-cli` binary. `None` when detection found the app but no CLI beside it.
    pub cli_path: Option<PathBuf>,
}

/// Every installed Craft app, in suite order. Detection decides what is installed; this adds the
/// CLI each app serves MCP from.
pub fn installed_apps(manifest: &Manifest, snapshot: &detect::Snapshot) -> Vec<InstalledApp> {
    artcraft_launcher_core::SUITE
        .iter()
        .filter_map(|app| {
            let installed = detect::detect(app, manifest, snapshot)?;
            Some(InstalledApp {
                id: app.id.to_string(),
                name: app.name.to_string(),
                location: installed.location.clone(),
                cli_path: cli_path_for(&installed.location, app),
            })
        })
        .collect()
}

/// The `-cli` binary beside a detected main executable, under any name the app has shipped as
/// (current first), with or without `.exe`.
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

/// Names another `craft-ai-mcp` than the one beside the launcher (development, tests).
pub const GATEWAY_PATH_ENV: &str = "ARTCRAFT_LAUNCHER_GATEWAY_PATH";

/// The `craft-ai-mcp` gateway the launcher ships beside itself, or [`GATEWAY_PATH_ENV`].
pub fn locate_gateway() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(GATEWAY_PATH_ENV).filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    ["craft-ai-mcp.exe", "craft-ai-mcp"].into_iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// The folder in [`mcp_dir`] that holds the gateway copy clients start.
pub const GATEWAY_DIR: &str = "bin";

/// Where the gateway copy lives in `dir`.
pub fn gateway_copy(dir: &Path) -> PathBuf {
    dir.join(GATEWAY_DIR).join(format!("craft-ai-mcp{}", std::env::consts::EXE_SUFFIX))
}

/// Copy the shipped gateway `src` into `dir`, so client configurations name a path that stays
/// valid: not one inside an AppImage mount that goes away when the launcher quits, a Flatpak
/// sandbox path host clients can't run, or a folder an update or a move replaces. An existing
/// copy of the same size that is not older than `src` is kept. A copy a client is running can't
/// be replaced on Windows; then the existing copy is used and refreshed later.
pub fn install_gateway(src: &Path, dir: &Path) -> Result<PathBuf, String> {
    let dest = gateway_copy(dir);
    let current = match (std::fs::metadata(src), std::fs::metadata(&dest)) {
        // Same size: compare the bytes (a shipped build can keep an older timestamp).
        (Ok(a), Ok(b)) => a.len() == b.len() && matches!((std::fs::read(src), std::fs::read(&dest)), (Ok(x), Ok(y)) if x == y),
        (Err(e), _) => return Err(format!("couldn't read the Craft gateway at {} ({e})", src.display())),
        _ => false,
    };
    if current {
        return Ok(dest);
    }
    let bytes = std::fs::read(src).map_err(|e| format!("couldn't read the Craft gateway at {} ({e})", src.display()))?;
    match crate::paths::write_atomic(&dest, &bytes) {
        Ok(()) => {}
        Err(_) if dest.is_file() => return Ok(dest),
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("couldn't make {} executable ({e})", dest.display()))?;
    }
    Ok(dest)
}

/// Serialises every change to the app list and the gateway copy. Detection refreshes them on a
/// background thread while Settings may be turning agents off; without this, a refresh that
/// read "on" just before the switch could write the list back after it was removed.
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What Settings shows. While agents are on, the app list and the gateway copy are refreshed
/// first, so an app installed (or a gateway updated) since then is used too; a failed refresh
/// keeps the previous ones.
pub fn state(services: &[InstalledApp]) -> McpAgents {
    let flag_on = flag_on();
    let dir = mcp_dir();
    let on = {
        let _guard = lock();
        let on = flag_on && dir.as_deref().is_some_and(enabled);
        if on && let Some(d) = &dir {
            let _ = write_apps(d, services);
            if let Some(src) = locate_gateway() {
                let _ = install_gateway(&src, d);
            }
        }
        on
    };
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
        flag_on,
        gateway_found: locate_gateway().is_some(),
        enabled: on,
        apps: services.iter().filter(|s| s.cli_path.as_ref().is_some_and(|p| p.is_absolute())).count(),
        clients,
    }
}

/// Whether the `AI-MCP` feature flag is on (`ArtCraft/flags.toml`, `CRAFT_FLAGS`).
pub fn flag_on() -> bool {
    craft_flags::load().enabled(&craft_flags::AI_MCP)
}

const NO_FLAG: &str = "turn on \"AI agents (MCP)\" under Experimental features first";

/// Every feature flag, for Settings.
pub fn flags_state() -> Vec<FlagState> {
    let flags = craft_flags::load();
    craft_flags::ALL
        .iter()
        .map(|f| FlagState {
            id: f.id.to_string(),
            title: f.title.to_string(),
            description: f.description.to_string(),
            on: flags.enabled(f),
            forced: flags.forced(f.id).is_some(),
        })
        .collect()
}

/// Turn a feature flag on or off for the launcher and every app. Turning `AI-MCP` off also turns
/// agents off, so no client keeps an entry for a feature that is switched off.
pub fn set_flag(id: &str, on: bool, services: &[InstalledApp]) -> Result<(), String> {
    let path = craft_flags::default_path().ok_or("couldn't find the per-user configuration folder")?;
    // Clean up first: if that fails, the flag stays on and Settings keeps the controls to retry.
    if id == craft_flags::AI_MCP.id && !on {
        set_enabled(false, services)?;
    }
    craft_flags::set(&path, id, on).map_err(|e| e.to_string())
}

const NO_GATEWAY: &str = "the Craft gateway (craft-ai-mcp) is not installed beside the launcher";

/// Turn agents on (copy the gateway, write the app list) or off (remove the list, every client
/// entry this module added, and the copy). All clients are tried; the first error is returned.
pub fn set_enabled(on: bool, services: &[InstalledApp]) -> Result<(), String> {
    let dir = mcp_dir().ok_or("couldn't find the per-user configuration folder")?;
    let _guard = lock();
    if on {
        if !flag_on() {
            return Err(NO_FLAG.into());
        }
        let src = locate_gateway().ok_or(NO_GATEWAY)?;
        install_gateway(&src, &dir)?;
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
    // A client still running the copy keeps it busy on Windows; it goes next time.
    let _ = std::fs::remove_file(gateway_copy(&dir));
    first_error.map_or(Ok(()), Err)
}

/// Add the gateway entry to `client`, or remove it. Adding needs agents on; the entry names the
/// gateway copy in [`mcp_dir`].
pub fn set_client(client: McpClient, added: bool) -> Result<(), String> {
    let (config, _) = client_paths(client).ok_or("couldn't find the home folder")?;
    if !added {
        return unregister(&config);
    }
    if !flag_on() {
        return Err(NO_FLAG.into());
    }
    let dir = mcp_dir().ok_or("couldn't find the per-user configuration folder")?;
    let _guard = lock();
    if !enabled(&dir) {
        return Err("turn on \"Let AI agents use Craft apps\" first".into());
    }
    let copy = gateway_copy(&dir);
    let gateway = match locate_gateway() {
        Some(src) => install_gateway(&src, &dir)?,
        None if copy.is_file() => copy,
        None => return Err(NO_GATEWAY.into()),
    };
    register(&config, &gateway)
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
