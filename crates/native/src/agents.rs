//! AI agents and the Craft apps: one switch in Settings, and one entry in each MCP client.
//!
//! With the switch on, the launcher writes the installed apps and their CLIs into
//! `ArtCraft/mcp/apps.json` (the list the `craft-ai-mcp` gateway reads, see
//! `docs/CRAFT-AI-MCP.md` in storytold/craft-ai-integrations), and can add one stdio entry,
//! `ArtCraft` → `craft-ai-mcp --stdio`, to Claude Desktop, Claude Code, Cursor and LM Studio.
//! Through that one entry an agent reaches every installed app, without one entry per app.
//!
//! Turning the switch off removes the list and the entries this module added. A client
//! configuration that does not parse is never overwritten, and entries other than ours are kept.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use artcraft_launcher_core::protocol::{McpAgents, McpClient, McpClientState};
use serde_json::{Map, Value, json};

use artcraft_launcher_core::CraftApp;

use crate::detect;
use crate::manifest::Manifest;

/// The key our entry has in a client's `mcpServers`.
pub use artcraft_launcher_core::mcp_config::SERVER_KEY;
/// The app list and its folder override, from `craft-ai-agents` (the one place that reads and
/// writes them, for the launcher, the gateway and every app).
pub use craft_ai_agents::{APPS_FILE, AppEntry, DIR_ENV};
/// Largest client configuration read; anything bigger is not a configuration file.
const MAX_CONFIG_BYTES: u64 = 64 << 20;

/// The app list's folder: [`DIR_ENV`], otherwise `ArtCraft/mcp` in the per-user local
/// configuration folder (beside the AI layer's `ArtCraft/ai`).
pub fn mcp_dir() -> Option<PathBuf> {
    craft_ai_agents::agents_dir()
}

/// Whether agents are on: the app list exists in `dir`.
pub fn enabled(dir: &Path) -> bool {
    craft_ai_agents::switched_on_in(dir)
}

/// Write (or refresh) the app list in `dir`.
pub fn write_apps(dir: &Path, services: &[AppEntry]) -> Result<(), String> {
    craft_ai_agents::write(dir, services)
}

/// Remove the app list in `dir`. A missing list is fine.
pub fn remove_apps(dir: &Path) -> Result<(), String> {
    craft_ai_agents::remove(dir)
}

/// Every client the launcher can add the entry to.
pub const CLIENTS: [McpClient; 4] = [McpClient::ClaudeDesktop, McpClient::ClaudeCode, McpClient::Cursor, McpClient::LmStudio];

/// Where `client` keeps its MCP servers on this machine, and the folder whose presence shows the
/// client is installed.
pub fn client_paths(client: McpClient) -> Option<(PathBuf, PathBuf)> {
    let home = dirs::home_dir()?;
    Some(match client {
        McpClient::ClaudeDesktop => {
            let dir = claude_desktop_dir(dirs::config_dir()?, std::env::var_os("LOCALAPPDATA").map(PathBuf::from).as_deref());
            (dir.join("claude_desktop_config.json"), dir)
        }
        McpClient::ClaudeCode => (home.join(".claude.json"), home.join(".claude")),
        McpClient::Cursor => (home.join(".cursor").join("mcp.json"), home.join(".cursor")),
        McpClient::LmStudio => (home.join(".lmstudio").join("mcp.json"), home.join(".lmstudio")),
    })
}

/// Claude Desktop's settings folder, where it reads `claude_desktop_config.json`, best first:
/// 1. `%LOCALAPPDATA%\Claude-Data\<install>` (newest first): current Windows versions keep their
///    data there, outside the Microsoft Store package, and say so in that folder's README.
/// 2. `<config>/Claude` (`%APPDATA%\Claude`): the classic installer, and every other platform.
/// 3. `%LOCALAPPDATA%\Packages\Claude_<id>\LocalCache\Roaming\Claude`: older Store versions, where
///    Windows redirects the package's `%APPDATA%`. A folder whose data has moved on holds a
///    `.user-data-left.*` marker and is skipped.
///
/// `local_appdata` is `None` outside Windows. With nothing found, the classic folder (not installed).
fn claude_desktop_dir(config: PathBuf, local_appdata: Option<&Path>) -> PathBuf {
    let subdirs = |dir: PathBuf| -> Vec<PathBuf> {
        std::fs::read_dir(dir).map(|it| it.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default()
    };
    if let Some(local) = local_appdata {
        let mut data = subdirs(local.join("Claude-Data"));
        // Newest first; the name breaks ties so the choice is stable.
        data.sort_by_key(|d| (std::cmp::Reverse(std::fs::metadata(d).and_then(|m| m.modified()).ok()), d.clone()));
        if let Some(d) = data.into_iter().next() {
            return d;
        }
    }
    let classic = config.join("Claude");
    if classic.is_dir() {
        return classic;
    }
    let moved = |d: &Path| {
        std::fs::read_dir(d).map(|it| it.flatten().any(|e| e.file_name().to_string_lossy().starts_with(".user-data-left."))).unwrap_or(false)
    };
    let mut packaged: Vec<PathBuf> = local_appdata
        .map(|local| subdirs(local.join("Packages")))
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("Claude_")))
        .map(|p| p.join("LocalCache").join("Roaming").join("Claude"))
        .filter(|d| d.is_dir() && !moved(d))
        .collect();
    packaged.sort();
    packaged.into_iter().next().unwrap_or(classic)
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

/// Whether this is the uncustomized entry for the launcher's stable gateway copy.
/// A matching filename alone is not ownership: preserve other paths and client options.
fn ours(entry: &Value, gateway: &Path) -> bool {
    let Value::Object(e) = entry else {
        return false;
    };
    let command_is_gateway = e.get("command").and_then(Value::as_str).is_some_and(|c| Path::new(c).is_absolute() && Path::new(c) == gateway);
    let no_env = match e.get("env") {
        None => true,
        Some(Value::Object(env)) => env.is_empty(),
        Some(_) => false,
    };
    let no_custom_options = e.keys().all(|k| matches!(k.as_str(), "command" | "args" | "env"));
    command_is_gateway && e.get("args") == Some(&json!(["--stdio"])) && no_env && no_custom_options
}

/// Whether `config` holds our entry (not merely some entry named [`SERVER_KEY`]).
pub fn registered(config: &Path, gateway: &Path) -> bool {
    matches!(read_config(config), Ok(Some(m)) if m.get("mcpServers").and_then(|s| s.get(SERVER_KEY)).is_some_and(|e| ours(e, gateway)))
}

/// Add (or update) our entry in the client configuration at `config`, starting `gateway`.
/// Everything else in the file is kept. An entry named [`SERVER_KEY`] that the launcher did not
/// write is an error, so a person's own configuration is never replaced.
pub fn register(config: &Path, gateway: &Path) -> Result<(), String> {
    let mut root = read_config(config)?.unwrap_or_default();
    let servers = root.entry("mcpServers").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(format!("`mcpServers` in {} is not an object; the launcher leaves it unchanged", config.display()));
    };
    if servers.get(SERVER_KEY).is_some_and(|e| !ours(e, gateway)) {
        return Err(format!(
            "{} already has its own `{SERVER_KEY}` entry; the launcher leaves it unchanged. Rename or remove it to add the launcher's",
            config.display()
        ));
    }
    servers.insert(SERVER_KEY.to_string(), json!({"command": gateway.to_string_lossy(), "args": ["--stdio"]}));
    let text = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())?;
    crate::paths::write_atomic(config, text.as_bytes())
}

/// Remove our entry from the client configuration at `config`. Nothing to do when the file or
/// our entry is missing; an entry named [`SERVER_KEY`] the launcher did not write is kept.
pub fn unregister(config: &Path, gateway: &Path) -> Result<(), String> {
    let Some(mut root) = read_config(config)? else {
        return Ok(());
    };
    let removed = match root.get_mut("mcpServers") {
        Some(Value::Object(servers)) if servers.get(SERVER_KEY).is_some_and(|e| ours(e, gateway)) => servers.remove(SERVER_KEY).is_some(),
        _ => false,
    };
    if !removed {
        return Ok(());
    }
    let text = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())?;
    crate::paths::write_atomic(config, text.as_bytes())
}

/// Every installed Craft app with a usable MCP command, in suite order.
pub fn installed_apps(manifest: &Manifest, snapshot: &detect::Snapshot) -> Vec<AppEntry> {
    artcraft_launcher_core::SUITE
        .iter()
        .filter_map(|app| {
            let installed = detect::detect(app, manifest, snapshot)?;
            #[cfg(target_os = "linux")]
            if installed.source == artcraft_launcher_core::InstallSource::Flatpak {
                // The MCP gateway starts on the host and needs an absolute host path.
                if crate::process::in_sandbox() {
                    return None;
                }
                return flatpak_entry(app, &installed, crate::process::which("flatpak")?, flatpak_agent_args(app));
            }
            native_entry(app, &installed.location)
        })
        .collect()
}

fn native_entry(app: &CraftApp, location: &str) -> Option<AppEntry> {
    let command = cli_path_for(location, app).filter(|path| path.is_absolute())?;
    Some(AppEntry { id: app.id.into(), name: app.name.into(), command, args: vec!["mcp".into()] })
}

#[cfg(target_os = "linux")]
fn flatpak_entry(app: &CraftApp, installed: &artcraft_launcher_core::Installed, flatpak: PathBuf, agent_args: Vec<String>) -> Option<AppEntry> {
    let id = installed.package.as_deref()?;
    let scope = crate::packages::flatpak_scope(installed.installation.as_deref());
    let mut info = crate::process::command(&flatpak);
    info.args(["info", &scope, "--show-location", id]);
    let location = crate::process::run_command(info, "flatpak", std::time::Duration::from_secs(3)).ok()?;
    let deployment = Path::new(location.trim());
    if !deployment.is_absolute() {
        return None;
    }
    // Inspect the deployment to confirm the CLI ships with this app, but run it through Flatpak
    // so it gets the runtime and permissions of the installed package.
    let bin = deployment.join("files/bin");
    let cli = app.stems().map(|stem| format!("{stem}-cli")).find(|name| bin.join(name).is_file())?;
    let mut args = vec!["run".into(), scope, format!("--command={cli}")];
    args.extend(agent_args);
    args.extend([id.into(), "mcp".into()]);
    Some(AppEntry { id: app.id.into(), name: app.name.into(), command: flatpak, args })
}

/// Share the same agent consent with the Flatpak window and the CLI that talks to it. Flatpak
/// uses a private XDG config directory, so expose only the registry directory, read-only.
#[cfg(target_os = "linux")]
pub(crate) fn flatpak_agent_args(app: &CraftApp) -> Vec<String> {
    if crate::process::in_sandbox() || craft_ai_agents::known_name(app.id).is_none() || !flag_on() {
        return Vec::new();
    }
    let (Some(config), Some(dir)) = (dirs::config_local_dir(), mcp_dir()) else { return Vec::new() };
    flatpak_consent_args(&config, &dir).unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn flatpak_consent_args(config: &Path, dir: &Path) -> Option<Vec<String>> {
    // CRAFT_MCP_DIR can name an arbitrary folder. Only the dedicated default registry is shared.
    if !config.is_absolute() || dir != config.join("ArtCraft/mcp") {
        return None;
    }
    // Mount the directory even before consent is on. The launcher creates, replaces and removes
    // apps.json while the sandbox is running, so mounting just that file would miss updates.
    std::fs::create_dir_all(config).ok()?;
    let mut registry = config.canonicalize().ok()?;
    for name in ["ArtCraft", "mcp"] {
        registry.push(name);
        match std::fs::create_dir(&registry) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                log::warn!("couldn't share the agent registry with Flatpak at {} ({e})", registry.display());
                return None;
            }
        }
        // Allow a relocated XDG config root, but reject aliases within it.
        if !registry.symlink_metadata().ok()?.is_dir() {
            return None;
        }
    }
    let path = registry.to_str()?;
    Some(vec![format!("--filesystem={path}:ro"), format!("--env={DIR_ENV}={path}"), format!("--env={}={}", craft_flags::ENV, craft_flags::AI_MCP.id)])
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
    if !current {
        let bytes = std::fs::read(src).map_err(|e| format!("couldn't read the Craft gateway at {} ({e})", src.display()))?;
        match crate::paths::write_atomic(&dest, &bytes) {
            Ok(()) => {}
            // A client still running the old copy keeps it busy on Windows: keep using it.
            Err(_) if dest.is_file() => {}
            Err(e) => return Err(e),
        }
    }
    // Also for a copy with the right bytes: one that lost its execute bit couldn't be started.
    make_executable(&dest)?;
    Ok(dest)
}

/// Make the gateway copy executable for everyone who may run it (`0755`) when it isn't already.
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).map_err(|e| format!("couldn't read {} ({e})", path.display()))?.permissions().mode();
    if mode & 0o111 == 0o111 {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("couldn't make {} executable ({e})", path.display()))
}

/// Windows has no execute bit.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
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
pub fn state(services: &[AppEntry]) -> McpAgents {
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
                added: paths.as_ref().is_some_and(|(config, _)| dir.as_deref().is_some_and(|d| registered(config, &gateway_copy(d)))),
                restartable: restartable(*c),
            }
        })
        .collect();
    let gateway = dir.as_deref().filter(|_| on).map(gateway_copy).filter(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned());
    McpAgents { flag_on, gateway_found: locate_gateway().is_some(), enabled: on, gateway, apps: services.len(), clients }
}

/// Whether the `AI-MCP` feature flag is on (`ArtCraft/flags.toml`, `CRAFT_FLAGS`).
pub fn flag_on() -> bool {
    craft_flags::load().enabled(&craft_flags::AI_MCP)
}

const NO_FLAG: &str = "the AI-MCP feature flag is off (ArtCraft/flags.toml or CRAFT_FLAGS)";

const NO_GATEWAY: &str = "the Craft gateway (craft-ai-mcp) is not installed beside the launcher";

/// Turn agents on (copy the gateway, write the app list) or off (remove the list, every client
/// entry this module added, and the copy). All clients are tried; the first error is returned.
pub fn set_enabled(on: bool, services: &[AppEntry]) -> Result<(), String> {
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
            && let Err(e) = unregister(&config, &gateway_copy(&dir))
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
    let dir = mcp_dir().ok_or("couldn't find the per-user configuration folder")?;
    if !added {
        return unregister(&config, &gateway_copy(&dir));
    }
    if !flag_on() {
        return Err(NO_FLAG.into());
    }
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

/// Whether the launcher can restart `client` here: Claude Desktop on Windows and macOS. The other
/// clients are left to the person (Claude Code runs in their terminal sessions).
pub fn restartable(client: McpClient) -> bool {
    client == McpClient::ClaudeDesktop && (cfg!(windows) || cfg!(target_os = "macos"))
}

/// Quit `client` and start it again, so it loads the entry just added.
pub fn restart_client(client: McpClient) -> Result<(), String> {
    if !restartable(client) {
        return Err(format!("the launcher can't restart {} here; restart it yourself", client_name(client)));
    }
    crate::process::restart_claude_desktop()
}

#[cfg(test)]
#[path = "agents_tests.rs"]
mod tests;
