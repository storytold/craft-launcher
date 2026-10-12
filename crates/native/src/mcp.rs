//! Connecting the Craft apps' MCP servers to AI tools.
//!
//! Each Craft app ships `<app>-cli`, and `<app>-cli mcp` serves the Model Context Protocol on
//! stdio. AI tools start such servers from a settings file of their own, each in its own place
//! and its own dialect. This module knows those places and dialects, finds the CLI of every
//! installed app, and adds or removes the apps' entries (named after the app: `photocraft`)
//! without touching anything else in the file: other servers, comments and formatting stay.
//!
//! The launcher only ever changes entries that start a Craft app's MCP server. An entry under an
//! app's name that starts something else is the user's, and stays.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use artcraft_launcher_core::mcp::{AiTool, Entry, McpServer};
use artcraft_launcher_core::{CraftApp, InstallSource, Installed, Os, SUITE};
use serde_json::{Value, json};

use crate::jsonc;
use crate::process::which_on_path;

/// The biggest settings file the launcher reads (Claude Code's grows with its project history).
const MAX_SIZE: u64 = 64 << 20;

/// The folders AI tools keep their settings in. Tests point them at a temporary folder.
#[derive(Clone, Debug)]
pub struct Bases {
    pub os: Os,
    pub home: PathBuf,
    /// Per-user settings: `%APPDATA%`, `~/Library/Application Support`, `~/.config`.
    pub config: PathBuf,
    /// Per-user local data: `%LOCALAPPDATA%`, where Microsoft Store apps keep theirs.
    pub local: PathBuf,
    /// `$XDG_CONFIG_HOME` or `~/.config`, which command-line tools use on every system.
    pub xdg_config: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`: where Claude Code keeps `.claude.json` instead of the home folder.
    pub claude_config_dir: Option<PathBuf>,
    /// `$CODEX_HOME`: Codex's folder instead of `~/.codex`.
    pub codex_home: Option<PathBuf>,
}

impl Bases {
    pub fn current() -> Option<Self> {
        let home = dirs::home_dir()?;
        let env_dir = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|p| p.is_absolute());
        Some(Self {
            os: artcraft_launcher_core::Platform::current()?.os,
            config: dirs::config_dir().unwrap_or_else(|| home.join(".config")),
            local: dirs::data_local_dir().unwrap_or_else(|| home.join(".local").join("share")),
            xdg_config: env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            claude_config_dir: env_dir("CLAUDE_CONFIG_DIR"),
            codex_home: env_dir("CODEX_HOME"),
            home,
        })
    }
}

/// How an AI tool writes a server down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// `"mcpServers": {"name": {"command", "args"}}`: Claude Desktop, Cursor, Windsurf, Gemini CLI
    /// and most others.
    Plain,
    /// Claude Code: `{"type": "stdio", "command", "args", "env": {}}`, as `claude mcp add` writes.
    ClaudeCode,
    /// VS Code: `"servers": {"name": {"type": "stdio", "command", "args"}}`.
    VsCode,
    /// GitHub Copilot CLI: `{"type": "local", "command", "args", "tools": ["*"]}`.
    Copilot,
    /// Zed: `"context_servers": {"name": {"command", "args", "env": {}}}`.
    Zed,
    /// opencode: `"mcp": {"name": {"type": "local", "command": [program, args…], "enabled": true}}`.
    OpenCode,
    /// Codex: `[mcp_servers.name]` tables with `command` and `args`, in TOML.
    Codex,
}

impl Shape {
    /// The member (or TOML table) the servers are listed under.
    fn key(self) -> &'static str {
        match self {
            Self::VsCode => "servers",
            Self::Zed => "context_servers",
            Self::OpenCode => "mcp",
            Self::Codex => "mcp_servers",
            Self::Plain | Self::ClaudeCode | Self::Copilot => "mcpServers",
        }
    }

    /// The entry that starts `s`, in this tool's dialect, in the order people write it.
    fn fields(self, s: &McpServer) -> Vec<(&'static str, Value)> {
        let (command, args) = (json!(s.command), json!(s.args));
        match self {
            Self::Plain | Self::Codex => vec![("command", command), ("args", args)],
            Self::ClaudeCode => vec![("type", json!("stdio")), ("command", command), ("args", args), ("env", json!({}))],
            Self::VsCode => vec![("type", json!("stdio")), ("command", command), ("args", args)],
            Self::Copilot => vec![("type", json!("local")), ("command", command), ("args", args), ("tools", json!(["*"]))],
            Self::Zed => vec![("command", command), ("args", args), ("env", json!({}))],
            Self::OpenCode => {
                let all: Vec<&str> = std::iter::once(s.command.as_str()).chain(s.args.iter().map(String::as_str)).collect();
                vec![("type", json!("local")), ("command", json!(all)), ("enabled", json!(true))]
            }
        }
    }
}

/// What a JSON entry starts, in any of the dialects above (and Zed's older
/// `"command": {"path", "args"}`). `None` for entries that start no program (a remote server).
fn started_by(entry: &Value) -> Option<McpServer> {
    let strings = |v: Option<&Value>| -> Option<Vec<String>> {
        match v {
            None => Some(Vec::new()),
            Some(v) => v.as_array()?.iter().map(|s| s.as_str().map(str::to_string)).collect(),
        }
    };
    let command = entry.get("command")?;
    let (command, args) = match command {
        Value::String(program) => (program.clone(), strings(entry.get("args"))?),
        Value::Array(_) => {
            let all = strings(Some(command))?;
            let (program, args) = all.split_first()?;
            (program.clone(), args.to_vec())
        }
        Value::Object(cmd) => (cmd.get("path")?.as_str()?.to_string(), strings(cmd.get("args"))?),
        _ => return None,
    };
    Some(McpServer { command, args })
}

/// Whether `s` is `app`'s MCP server: its CLI (from any folder) or its Flatpak's, with the `mcp`
/// subcommand.
fn is_craft_server(app: &CraftApp, s: &McpServer) -> bool {
    let cli = format!("{}-cli", app.id);
    let file = s.command.rsplit(['/', '\\']).next().unwrap_or(&s.command);
    let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
    let runs_cli = stem.eq_ignore_ascii_case(&cli) || (stem == "flatpak" && s.args.iter().any(|a| *a == format!("--command={cli}")));
    runs_cli && s.args.iter().any(|a| a == "mcp")
}

/// What an entry under `app`'s name is, given how the launcher would start the server now.
fn classify(app: &CraftApp, now: Option<&McpServer>, started: Option<&McpServer>) -> Entry {
    match started {
        Some(s) if now == Some(s) => Entry::Current,
        Some(s) if is_craft_server(app, s) => Entry::Different,
        _ => Entry::Foreign,
    }
}

/// An AI tool the launcher can connect apps to.
struct Tool {
    id: &'static str,
    name: &'static str,
    shape: Shape,
    /// Where its settings may be, best first: (the file, a path that exists when that's the one
    /// in use).
    places: Vec<(PathBuf, PathBuf)>,
}

impl Tool {
    /// The settings file to change, and whether the tool looks installed.
    fn target(&self) -> (PathBuf, bool) {
        if let Some((file, _)) = self.places.iter().find(|(_, marker)| marker.exists()) {
            return (file.clone(), true);
        }
        (self.places.first().map(|(file, _)| file.clone()).unwrap_or_default(), false)
    }
}

/// Where Claude Desktop from the Microsoft Store keeps its settings: inside its package, not in
/// `%APPDATA%\Claude` (the copy its own "Edit Config" button opens). Each is (the file, the
/// package's folder).
fn store_claude_desktop(local: &Path) -> Vec<(PathBuf, PathBuf)> {
    let Ok(packages) = std::fs::read_dir(local.join("Packages")) else { return Vec::new() };
    let mut found: Vec<PathBuf> = packages
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("Claude_") || n.contains(".Claude_")))
        .collect();
    found.sort();
    found.into_iter().map(|package| (package.join("LocalCache").join("Roaming").join("Claude").join("claude_desktop_config.json"), package)).collect()
}

/// The AI tools, in the order they're listed.
fn tools(b: &Bases) -> Vec<Tool> {
    let home = &b.home;
    let tool = |id, name, shape, places: Vec<(PathBuf, PathBuf)>| Tool { id, name, shape, places };
    // A settings file in a folder that is there when the tool is installed.
    let in_dir = |dir: PathBuf, file: &str| (dir.join(file), dir);
    // A settings file that is only there once the tool has been used.
    let itself = |file: PathBuf| (file.clone(), file);

    let mut claude_desktop = if b.os == Os::Windows { store_claude_desktop(&b.local) } else { Vec::new() };
    claude_desktop.push(in_dir(b.config.join("Claude"), "claude_desktop_config.json"));
    let claude_json = b.claude_config_dir.as_ref().unwrap_or(home).join(".claude.json");
    let codex = b.codex_home.clone().unwrap_or_else(|| home.join(".codex"));
    let code = |edition: &str| b.config.join(edition).join("User");
    let extension = |id: &str, file: &str| {
        let dir = code("Code").join("globalStorage").join(id);
        (dir.join("settings").join(file), dir)
    };
    let zed = if b.os == Os::Windows { b.config.join("Zed") } else { b.xdg_config.join("zed") };
    let opencode = b.xdg_config.join("opencode");
    // LM Studio can live elsewhere; `~/.lmstudio-home-pointer` then says where.
    let lm_studio = std::fs::read_to_string(home.join(".lmstudio-home-pointer"))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_absolute() && p.is_dir())
        .unwrap_or_else(|| home.join(".lmstudio"));
    let gemini = home.join(".gemini");

    vec![
        tool("claude-desktop", "Claude Desktop", Shape::Plain, claude_desktop),
        tool("claude-code", "Claude Code", Shape::ClaudeCode, vec![itself(claude_json.clone()), (claude_json, home.join(".claude"))]),
        tool("codex", "Codex", Shape::Codex, vec![in_dir(codex, "config.toml")]),
        tool("cursor", "Cursor", Shape::Plain, vec![in_dir(home.join(".cursor"), "mcp.json")]),
        tool("vscode", "VS Code", Shape::VsCode, vec![in_dir(code("Code"), "mcp.json")]),
        tool("vscode-insiders", "VS Code Insiders", Shape::VsCode, vec![in_dir(code("Code - Insiders"), "mcp.json")]),
        tool("windsurf", "Windsurf", Shape::Plain, vec![in_dir(home.join(".codeium").join("windsurf"), "mcp_config.json")]),
        tool("gemini-cli", "Gemini CLI", Shape::Plain, vec![itself(gemini.join("settings.json"))]),
        tool("copilot-cli", "GitHub Copilot CLI", Shape::Copilot, vec![in_dir(home.join(".copilot"), "mcp-config.json")]),
        // The folder its tools share first, its own otherwise.
        tool(
            "antigravity",
            "Antigravity",
            Shape::Plain,
            vec![in_dir(gemini.join("config"), "mcp_config.json"), in_dir(gemini.join("antigravity"), "mcp_config.json")],
        ),
        tool("zed", "Zed", Shape::Zed, vec![in_dir(zed, "settings.json")]),
        tool("cline", "Cline", Shape::Plain, vec![extension("saoudrizwan.claude-dev", "cline_mcp_settings.json")]),
        tool("roo-code", "Roo Code", Shape::Plain, vec![extension("rooveterinaryinc.roo-cline", "mcp_settings.json")]),
        tool("kiro", "Kiro", Shape::Plain, vec![(home.join(".kiro").join("settings").join("mcp.json"), home.join(".kiro"))]),
        tool("junie", "Junie", Shape::Plain, vec![(home.join(".junie").join("mcp").join("mcp.json"), home.join(".junie"))]),
        tool("amazon-q", "Amazon Q Developer", Shape::Plain, vec![in_dir(home.join(".aws").join("amazonq"), "mcp.json")]),
        tool("lm-studio", "LM Studio", Shape::Plain, vec![in_dir(lm_studio, "mcp.json")]),
        tool("opencode", "opencode", Shape::OpenCode, vec![itself(opencode.join("opencode.jsonc")), in_dir(opencode, "opencode.json")]),
    ]
}

/// `path`'s text, `None` when there's no file yet.
fn read_text(path: &Path) -> Result<Option<String>, String> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("couldn't read it ({e})")),
        Ok(m) if m.is_dir() => return Err("it's a folder".into()),
        Ok(m) if m.len() > MAX_SIZE => return Err("it's too big to change safely".into()),
        Ok(_) => {}
    }
    let bytes = std::fs::read(path).map_err(|e| format!("couldn't read it ({e})"))?;
    String::from_utf8(bytes).map(Some).map_err(|_| "it isn't UTF-8 text".into())
}

/// `text` as a TOML document that keeps its formatting. An error comes back on one line: "TOML
/// parse error at line 3, column 5: invalid string".
fn toml(text: &str) -> Result<toml_edit::DocumentMut, String> {
    text.parse().map_err(|e: toml_edit::TomlError| {
        let message = e.to_string();
        let mut lines = message.lines().map(str::trim).filter(|l| !l.is_empty());
        let first = lines.next().unwrap_or("invalid TOML");
        match lines.next_back() {
            Some(last) if last != first => format!("{first}: {last}"),
            _ => first.to_string(),
        }
    })
}

/// The servers a settings file lists: name → what it starts (`None`: nothing the launcher can
/// read as a program). Of two entries with one name the last counts, as the tools read them.
type Listed = BTreeMap<String, Option<McpServer>>;

fn listed(shape: Shape, text: &str) -> Result<Listed, String> {
    if shape == Shape::Codex {
        let doc = toml(text)?;
        let Some(servers) = doc.get(shape.key()) else { return Ok(Listed::new()) };
        let servers = servers.as_table_like().ok_or("\"mcp_servers\" isn't a table")?;
        let started = |item: &toml_edit::Item| -> Option<McpServer> {
            let t = item.as_table_like()?;
            let command = t.get("command")?.as_str()?.to_string();
            let args = match t.get("args") {
                None => Vec::new(),
                Some(a) => a.as_array()?.iter().map(|v| v.as_str().map(str::to_string)).collect::<Option<_>>()?,
            };
            Some(McpServer { command, args })
        };
        return Ok(servers.iter().map(|(name, item)| (name.to_string(), started(item))).collect());
    }
    let doc = jsonc::parse(text, shape.key())?;
    Ok(doc.entries()?.into_iter().map(|(name, v)| (name, started_by(&v))).collect())
}

/// Sets the entry `name` to start `s`, keeping the rest of the file.
fn with_server(shape: Shape, text: &str, name: &str, s: &McpServer) -> Result<String, String> {
    if shape != Shape::Codex {
        return jsonc::set(text, shape.key(), name, &shape.fields(s));
    }
    use toml_edit::{Array, Item, Table, value};
    let mut doc = toml(text)?;
    let root = doc.as_table_mut();
    if !root.contains_key(shape.key()) {
        // `[mcp_servers.photocraft]` without an empty `[mcp_servers]` above it.
        let mut servers = Table::new();
        servers.set_implicit(true);
        root.insert(shape.key(), Item::Table(servers));
    }
    let servers = root.get_mut(shape.key()).and_then(Item::as_table_like_mut).ok_or("\"mcp_servers\" isn't a table")?;
    if servers.get(name).and_then(Item::as_table_like).is_none() {
        servers.insert(name, Item::Table(Table::new()));
    }
    // An entry that was there keeps its other settings (env, timeouts).
    let entry = servers.get_mut(name).and_then(Item::as_table_like_mut).ok_or("the entry isn't a table")?;
    entry.insert("command", value(s.command.as_str()));
    entry.insert("args", value(s.args.iter().map(String::as_str).collect::<Array>()));
    Ok(doc.to_string())
}

/// Removes the entry `name`, keeping the rest of the file.
fn without_server(shape: Shape, text: &str, name: &str) -> Result<String, String> {
    if shape != Shape::Codex {
        return jsonc::remove(text, shape.key(), name);
    }
    let mut doc = toml(text)?;
    let removed = doc.get_mut(shape.key()).and_then(toml_edit::Item::as_table_like_mut).and_then(|t| t.remove(name)).is_some();
    Ok(if removed { doc.to_string() } else { text.to_string() })
}

/// Every AI tool, with the Craft apps its settings name. `servers` says how each installed app's
/// server starts now.
pub fn scan(bases: &Bases, servers: &BTreeMap<String, Result<McpServer, String>>) -> Vec<AiTool> {
    tools(bases)
        .into_iter()
        .map(|tool| {
            let (path, found) = tool.target();
            let read = read_text(&path).and_then(|text| text.map_or_else(|| Ok(Listed::new()), |t| listed(tool.shape, &t)));
            let (entries, error) = match read {
                Ok(list) => {
                    let entry = |app: &CraftApp| {
                        let now = servers.get(app.id).and_then(|s| s.as_ref().ok());
                        Some((app.id.to_string(), classify(app, now, list.get(app.id)?.as_ref())))
                    };
                    (SUITE.iter().filter_map(entry).collect(), None)
                }
                Err(e) => (BTreeMap::new(), Some(e)),
            };
            AiTool {
                id: tool.id.to_string(),
                name: tool.name.to_string(),
                found: found || path.exists(),
                path: path.display().to_string(),
                entries,
                error,
            }
        })
        .collect()
}

/// What [`change`] did, for the user: what changed, and the entries it left alone because they
/// start something else.
#[derive(Debug, PartialEq, Eq)]
pub struct Changed {
    pub message: String,
    pub kept: Option<String>,
}

fn names(apps: &[&CraftApp]) -> String {
    apps.iter().map(|a| a.name).collect::<Vec<_>>().join(", ")
}

/// Adds (`connect`) or removes the servers of `apps` in AI tool `tool_id`'s settings. Each app
/// comes with how its server starts (`None`: it can't be started here, so it isn't added). The
/// file as it was is kept in `backups` first.
pub fn change(
    bases: &Bases,
    backups: &Path,
    tool_id: &str,
    apps: &[(&'static CraftApp, Option<McpServer>)],
    connect: bool,
) -> Result<Changed, String> {
    let tool = tools(bases).into_iter().find(|t| t.id == tool_id).ok_or_else(|| format!("unknown AI tool {tool_id}"))?;
    let failed = |e: String| format!("Couldn't change the settings of {}: {e}", tool.name);
    let (path, _) = tool.target();
    // Settings kept elsewhere through a symbolic link (a dotfiles folder) stay there.
    let path =
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) { std::fs::canonicalize(&path).unwrap_or(path) } else { path };
    let original = read_text(&path).map_err(failed)?;
    let mut text = original.clone().unwrap_or_default();
    let list = listed(tool.shape, &text).map_err(failed)?;
    let (mut done, mut already, mut kept): (Vec<&CraftApp>, Vec<&CraftApp>, Vec<&CraftApp>) = Default::default();
    for &(app, ref server) in apps {
        let entry = list.get(app.id).map(|started| classify(app, server.as_ref(), started.as_ref()));
        match (connect, entry, server) {
            (_, Some(Entry::Foreign), _) => kept.push(app),
            (true, Some(Entry::Current), _) => already.push(app),
            (true, _, Some(s)) => {
                text = with_server(tool.shape, &text, app.id, s).map_err(failed)?;
                done.push(app);
            }
            (false, Some(Entry::Current | Entry::Different), _) => {
                text = without_server(tool.shape, &text, app.id).map_err(failed)?;
                done.push(app);
            }
            _ => {}
        }
    }
    if original.as_deref() != Some(text.as_str()) {
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        save(&path, &text, original.as_deref(), &backups.join(format!("{}-{file}", tool.id))).map_err(failed)?;
    }
    let message = match (connect, done.is_empty()) {
        (true, false) => format!("{} can now use {}. Restart it to load the change.", tool.name, names(&done)),
        (true, true) if !already.is_empty() => format!("{} already uses {}.", tool.name, names(&already)),
        (true, true) => format!("Nothing was added to {}.", tool.name),
        (false, false) => format!("Removed {} from {}. Restart it to load the change.", names(&done), tool.name),
        (false, true) => format!("{} doesn't use these apps.", tool.name),
    };
    let kept = (!kept.is_empty()).then(|| format!("{} has its own settings for {}, which were left as they are.", tool.name, names(&kept)));
    Ok(Changed { message, kept })
}

/// Replaces `path` with `text` in one step, after copying the old file to `backup`. The new file
/// gets the old one's permissions (Claude Code's is private to the user).
fn save(path: &Path, text: &str, original: Option<&str>, backup: &Path) -> Result<(), String> {
    if let Some(old) = original {
        crate::paths::write_atomic(backup, old.as_bytes()).map_err(|e| format!("couldn't keep a copy of it first: {e}"))?;
    }
    let dir = path.parent().ok_or("it has no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{file}.artcraft-launcher.tmp"));
    std::fs::write(&tmp, text).map_err(|e| format!("couldn't write {} ({e})", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("couldn't replace {} ({e})", path.display())
    })
}

/// How to start `app`'s MCP server on this machine: `<app>-cli mcp`, from where the app is
/// installed. The error says why it can't be started, for the user.
pub fn server_for(app: &CraftApp, installed: &Installed) -> Result<McpServer, String> {
    let cli = format!("{}-cli{}", app.id, std::env::consts::EXE_SUFFIX);
    match installed.source {
        InstallSource::Flatpak => {
            // By its full path where there is one: an AI tool's own PATH may be shorter.
            let flatpak = which_on_path("flatpak").filter(|p| p.is_absolute()).map_or_else(|| "flatpak".to_string(), |p| p.display().to_string());
            let args = vec!["run".to_string(), format!("--command={}-cli", app.id), app.bundle_id(), "mcp".to_string()];
            Ok(McpServer { command: flatpak, args })
        }
        InstallSource::AppImage => Err(format!(
            "The {} AppImage can't start its MCP server on its own. Install its .deb, .rpm or Flatpak to use it with AI tools.",
            app.name
        )),
        _ => {
            let location = Path::new(&installed.location);
            let mut candidates = Vec::new();
            if cfg!(target_os = "macos") {
                // The .app holds only the desktop app today; look inside in case a release adds it.
                candidates.push(location.join("Contents").join("MacOS").join(&cli));
            } else if location.is_dir() {
                candidates.extend([location.join("bin").join(&cli), location.join(&cli)]);
            } else if let Some(dir) = location.parent() {
                candidates.push(dir.join(&cli));
            }
            candidates.extend(which_on_path(&cli));
            if cfg!(unix) {
                candidates.extend(["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin"].iter().map(|d| Path::new(d).join(&cli)));
                if let Some(home) = dirs::home_dir() {
                    candidates.extend([home.join(".local").join("bin").join(&cli), home.join("bin").join(&cli)]);
                }
            }
            match candidates.into_iter().find(|p| p.is_absolute() && p.is_file()) {
                Some(p) => Ok(McpServer { command: p.display().to_string(), args: vec!["mcp".into()] }),
                None if cfg!(target_os = "macos") => Err(format!(
                    "On macOS, the command-line tool of {} is a separate download ({}-cli-<version>-macos-universal.zip on its release page). Unzip it into a folder on your PATH, such as /usr/local/bin.",
                    app.name, app.id
                )),
                None => {
                    Err(format!("The command-line tool of {} ({cli}) wasn't found next to the app. Reinstalling the app puts it back.", app.name))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bases(root: &Path, os: Os) -> Bases {
        let home = root.join("home");
        let (config, local) = match os {
            Os::Windows => (home.join("AppData").join("Roaming"), home.join("AppData").join("Local")),
            Os::MacOs => (home.join("Library").join("Application Support"), home.join("Library").join("Application Support")),
            _ => (home.join(".config"), home.join(".local").join("share")),
        };
        Bases { os, xdg_config: home.join(".config"), home, config, local, claude_config_dir: None, codex_home: None }
    }

    fn server(cmd: &str) -> McpServer {
        McpServer { command: cmd.to_string(), args: vec!["mcp".into()] }
    }

    fn photo() -> &'static CraftApp {
        CraftApp::by_id("photocraft").unwrap()
    }

    fn target(b: &Bases, id: &str) -> (PathBuf, bool) {
        tools(b).into_iter().find(|t| t.id == id).unwrap().target()
    }

    #[test]
    fn finds_each_tools_settings_where_it_keeps_them() {
        let root = tempfile::tempdir().unwrap();
        let b = bases(root.path(), Os::Windows);
        let home = &b.home;
        let roaming = home.join("AppData").join("Roaming");
        // Nothing installed: the usual places, not found.
        assert_eq!(target(&b, "cursor"), (home.join(".cursor").join("mcp.json"), false));
        assert_eq!(target(&b, "claude-desktop"), (roaming.join("Claude").join("claude_desktop_config.json"), false));
        assert_eq!(target(&b, "vscode"), (roaming.join("Code").join("User").join("mcp.json"), false));
        assert_eq!(target(&b, "zed"), (roaming.join("Zed").join("settings.json"), false));
        assert_eq!(target(&b, "codex"), (home.join(".codex").join("config.toml"), false));
        assert_eq!(target(&b, "opencode").0, home.join(".config").join("opencode").join("opencode.jsonc"));
        // The Microsoft Store's Claude Desktop wins over %APPDATA%\Claude.
        std::fs::create_dir_all(roaming.join("Claude")).unwrap();
        assert_eq!(target(&b, "claude-desktop"), (roaming.join("Claude").join("claude_desktop_config.json"), true));
        let package = home.join("AppData").join("Local").join("Packages").join("Claude_pzs8sxrjxfjjc");
        std::fs::create_dir_all(&package).unwrap();
        assert_eq!(
            target(&b, "claude-desktop"),
            (package.join("LocalCache").join("Roaming").join("Claude").join("claude_desktop_config.json"), true)
        );
        // opencode: an existing opencode.json is used when there's no .jsonc.
        std::fs::create_dir_all(home.join(".config").join("opencode")).unwrap();
        assert_eq!(target(&b, "opencode"), (home.join(".config").join("opencode").join("opencode.json"), true));
        // Antigravity: the shared ~/.gemini/config first, its own folder otherwise.
        std::fs::create_dir_all(home.join(".gemini").join("antigravity")).unwrap();
        assert_eq!(target(&b, "antigravity"), (home.join(".gemini").join("antigravity").join("mcp_config.json"), true));
        std::fs::create_dir_all(home.join(".gemini").join("config")).unwrap();
        assert_eq!(target(&b, "antigravity"), (home.join(".gemini").join("config").join("mcp_config.json"), true));
        // LM Studio moved elsewhere.
        let moved = root.path().join("lms");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::write(home.join(".lmstudio-home-pointer"), format!("{}\n", moved.display())).unwrap();
        assert_eq!(target(&b, "lm-studio"), (moved.join("mcp.json"), true));
        // Overrides.
        let b = Bases { claude_config_dir: Some(root.path().join("cc")), codex_home: Some(root.path().join("cx")), ..b };
        assert_eq!(target(&b, "claude-code").0, root.path().join("cc").join(".claude.json"));
        assert_eq!(target(&b, "codex").0, root.path().join("cx").join("config.toml"));

        let mac = bases(root.path(), Os::MacOs);
        let support = mac.home.join("Library").join("Application Support");
        assert_eq!(target(&mac, "claude-desktop").0, support.join("Claude").join("claude_desktop_config.json"));
        assert_eq!(target(&mac, "vscode").0, support.join("Code").join("User").join("mcp.json"));
        assert_eq!(target(&mac, "zed").0, mac.home.join(".config").join("zed").join("settings.json"));
        let linux = bases(root.path(), Os::Linux);
        assert_eq!(
            target(&linux, "cline").0,
            linux.home.join(".config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json")
        );
        let ids: Vec<&str> = tools(&linux).iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 18);
    }

    #[test]
    fn tells_craft_servers_from_other_entries() {
        let app = photo();
        let now = server(r"C:\Program Files\PhotoCraft\photocraft-cli.exe");
        let entry = |p: &str, args: &[&str]| McpServer { command: p.to_string(), args: args.iter().map(|s| s.to_string()).collect() };
        assert_eq!(classify(app, Some(&now), Some(&entry(r"C:\Program Files\PhotoCraft\photocraft-cli.exe", &["mcp"]))), Entry::Current);
        assert_eq!(classify(app, Some(&now), Some(&entry(r"D:\Old\PhotoCraft-CLI.EXE", &["mcp", "--connect", "7979"]))), Entry::Different);
        assert_eq!(classify(app, None, Some(&entry("/home/me/photocraft/target/release/photocraft-cli", &["mcp"]))), Entry::Different);
        let flatpak = ["run", "--command=photocraft-cli", "ai.storyteller.photocraft", "mcp"];
        assert_eq!(classify(app, Some(&now), Some(&entry("/usr/bin/flatpak", &flatpak))), Entry::Different);
        assert_eq!(classify(app, Some(&now), Some(&entry("photocraft-cli", &["--version"]))), Entry::Foreign);
        assert_eq!(classify(app, Some(&now), Some(&entry("npx", &["photocraft-mcp"]))), Entry::Foreign);
        assert_eq!(classify(app, Some(&now), None), Entry::Foreign);
        // Every dialect's way of writing the program down.
        assert_eq!(started_by(&json!({"command": "a", "args": ["mcp"]})), Some(entry("a", &["mcp"])));
        assert_eq!(started_by(&json!({"type": "local", "command": ["a", "mcp"], "enabled": true})), Some(entry("a", &["mcp"])));
        assert_eq!(started_by(&json!({"command": {"path": "a", "args": ["mcp"]}})), Some(entry("a", &["mcp"])));
        assert_eq!(started_by(&json!({"command": "a"})), Some(entry("a", &[])));
        assert_eq!(started_by(&json!({"url": "https://example.com/mcp"})), None);
        assert_eq!(started_by(&json!({"command": []})), None);
    }

    /// Connects PhotoCraft and WordCraft to every tool, then disconnects them, and checks each
    /// file is valid, says what the tool expects, and comes back exactly as it was.
    #[test]
    fn connects_and_disconnects_every_tool() {
        let root = tempfile::tempdir().unwrap();
        let b = bases(root.path(), Os::Linux);
        let backups = root.path().join("backups");
        let word = CraftApp::by_id("wordcraft").unwrap();
        let apps = [(photo(), Some(server("/usr/bin/photocraft-cli"))), (word, Some(server("/usr/bin/wordcraft-cli")))];
        for tool in tools(&b) {
            let (path, _) = tool.target();
            let before = match tool.shape {
                Shape::Codex => "# mine\nmodel = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n".to_string(),
                shape => format!("{{\n  // mine\n  \"{}\": {{\n    \"other\": {{\"command\": \"x\"}}\n  }}\n}}\n", shape.key()),
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &before).unwrap();

            let done = change(&b, &backups, tool.id, &apps, true).unwrap();
            assert_eq!(done.message, format!("{} can now use PhotoCraft, WordCraft. Restart it to load the change.", tool.name));
            assert_eq!(done.kept, None);
            let after = std::fs::read_to_string(&path).unwrap();
            assert!(after.contains("mine"), "{}: {after}", tool.id);
            let listed = listed(tool.shape, &after).unwrap();
            assert_eq!(listed["photocraft"], Some(server("/usr/bin/photocraft-cli")), "{}", tool.id);
            assert!(listed.contains_key("other"));
            match tool.shape {
                Shape::ClaudeCode => assert!(after.contains("\"type\": \"stdio\"")),
                Shape::VsCode => assert!(after.contains("\"servers\"") && after.contains("\"type\": \"stdio\"")),
                Shape::Copilot => assert!(after.contains("\"tools\": [\"*\"]")),
                Shape::OpenCode => assert!(after.contains("\"command\": [\"/usr/bin/photocraft-cli\", \"mcp\"]")),
                Shape::Codex => assert!(after.contains("[mcp_servers.photocraft]\ncommand = \"/usr/bin/photocraft-cli\"\nargs = [\"mcp\"]")),
                _ => {}
            }
            assert_eq!(
                std::fs::read_to_string(backups.join(format!("{}-{}", tool.id, path.file_name().unwrap().to_string_lossy()))).unwrap(),
                before
            );
            let report = scan(&b, &BTreeMap::from([("photocraft".to_string(), Ok(server("/usr/bin/photocraft-cli")))]));
            let state = report.iter().find(|t| t.id == tool.id).unwrap();
            assert_eq!(state.entries.get("photocraft"), Some(&Entry::Current));
            assert_eq!(state.entries.get("wordcraft"), Some(&Entry::Different), "no server known for it now");
            assert!(state.found && state.error.is_none());

            // Again: nothing to do.
            assert_eq!(change(&b, &backups, tool.id, &apps, true).unwrap().message, format!("{} already uses PhotoCraft, WordCraft.", tool.name));
            let removed = change(&b, &backups, tool.id, &apps, false).unwrap();
            assert_eq!(removed.message, format!("Removed PhotoCraft, WordCraft from {}. Restart it to load the change.", tool.name));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "{}", tool.id);
            assert_eq!(change(&b, &backups, tool.id, &apps, false).unwrap().message, format!("{} doesn't use these apps.", tool.name));
        }
    }

    #[test]
    fn creates_missing_settings_and_updates_moved_apps() {
        let root = tempfile::tempdir().unwrap();
        let b = bases(root.path(), Os::Windows);
        let backups = root.path().join("backups");
        let new = r"C:\Program Files\PhotoCraft\photocraft-cli.exe";
        change(&b, &backups, "cursor", &[(photo(), Some(server(r"D:\Apps\photocraft-cli.exe")))], true).unwrap();
        let path = b.home.join(".cursor").join("mcp.json");
        assert!(!backups.exists(), "nothing to keep a copy of");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"mcpServers\": {\n    \"photocraft\": {\n      \"command\": \"D:\\\\Apps\\\\photocraft-cli.exe\",\n      \"args\": [\"mcp\"]\n    }\n  }\n}\n"
        );
        let servers = BTreeMap::from([("photocraft".to_string(), Ok(server(new)))]);
        assert_eq!(scan(&b, &servers).iter().find(|t| t.id == "cursor").unwrap().entries["photocraft"], Entry::Different);
        change(&b, &backups, "cursor", &[(photo(), Some(server(new)))], true).unwrap();
        assert_eq!(scan(&b, &servers).iter().find(|t| t.id == "cursor").unwrap().entries["photocraft"], Entry::Current);
        // Codex: a moved app keeps its other settings.
        let codex = b.home.join(".codex").join("config.toml");
        std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
        std::fs::write(&codex, "[mcp_servers.photocraft]\ncommand = 'D:\\old\\photocraft-cli.exe'\nargs = [\"mcp\"]\nstartup_timeout_sec = 30\n")
            .unwrap();
        change(&b, &backups, "codex", &[(photo(), Some(server(new)))], true).unwrap();
        assert_eq!(
            std::fs::read_to_string(&codex).unwrap(),
            "[mcp_servers.photocraft]\ncommand = 'C:\\Program Files\\PhotoCraft\\photocraft-cli.exe'\nargs = [\"mcp\"]\nstartup_timeout_sec = 30\n"
        );
    }

    #[test]
    fn leaves_other_entries_and_broken_files_alone() {
        let root = tempfile::tempdir().unwrap();
        let b = bases(root.path(), Os::Linux);
        let backups = root.path().join("backups");
        let path = b.home.join(".cursor").join("mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mine = "{\"mcpServers\": {\"photocraft\": {\"url\": \"https://example.com/mcp\"}}}";
        std::fs::write(&path, mine).unwrap();
        let apps = [(photo(), Some(server("/usr/bin/photocraft-cli")))];
        for connect in [true, false] {
            let done = change(&b, &backups, "cursor", &apps, connect).unwrap();
            assert_eq!(done.kept.as_deref(), Some("Cursor has its own settings for PhotoCraft, which were left as they are."));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
        }
        assert_eq!(scan(&b, &BTreeMap::new()).iter().find(|t| t.id == "cursor").unwrap().entries["photocraft"], Entry::Foreign);

        std::fs::write(&path, "{\"mcpServers\": {").unwrap();
        let err = change(&b, &backups, "cursor", &apps, true).unwrap_err();
        assert_eq!(err, "Couldn't change the settings of Cursor: unexpected end of file at line 1, column 17");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"mcpServers\": {");
        let state = scan(&b, &BTreeMap::new()).into_iter().find(|t| t.id == "cursor").unwrap();
        assert!(state.error.is_some() && state.entries.is_empty());

        let codex = b.home.join(".codex").join("config.toml");
        std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
        std::fs::write(&codex, "model = \n").unwrap();
        assert!(
            change(&b, &backups, "codex", &apps, true).unwrap_err().starts_with("Couldn't change the settings of Codex: TOML parse error at line 1")
        );
        assert!(change(&b, &backups, "nope", &apps, true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn keeps_links_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let b = bases(root.path(), Os::Linux);
        let dotfiles = root.path().join("dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(&b.home).unwrap();
        let real = dotfiles.join("claude.json");
        std::fs::write(&real, "{\"numStartups\": 3}").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = b.home.join(".claude.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        change(&b, &root.path().join("backups"), "claude-code", &[(photo(), Some(server("/usr/bin/photocraft-cli")))], true).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(std::fs::read_to_string(&real).unwrap().contains("photocraft-cli"));
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn finds_the_cli_beside_the_app() {
        let root = tempfile::tempdir().unwrap();
        let exe = |name: &str| root.path().join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(exe("photocraft"), b"").unwrap();
        let installed = |source, location: &Path| Installed { version: None, source, location: location.display().to_string(), package: None };
        let msi = installed(InstallSource::Msi, &exe("photocraft"));
        if cfg!(target_os = "macos") {
            assert!(server_for(photo(), &msi).unwrap_err().starts_with("On macOS"));
        } else {
            assert!(server_for(photo(), &msi).unwrap_err().starts_with("The command-line tool of PhotoCraft (photocraft-cli"));
            std::fs::write(exe("photocraft-cli"), b"").unwrap();
            assert_eq!(server_for(photo(), &msi).unwrap(), server(&exe("photocraft-cli").display().to_string()));
            // A tree install: the folder, with the CLI in bin/.
            let tree = root.path().join("tree");
            std::fs::create_dir_all(tree.join("bin")).unwrap();
            std::fs::write(tree.join("bin").join(format!("photocraft-cli{}", std::env::consts::EXE_SUFFIX)), b"").unwrap();
            let found = server_for(photo(), &installed(InstallSource::Launcher, &tree)).unwrap();
            assert!(found.command.ends_with(&format!("photocraft-cli{}", std::env::consts::EXE_SUFFIX)) && found.command.contains("tree"));
        }
        let flatpak = server_for(photo(), &installed(InstallSource::Flatpak, Path::new("ai.storyteller.photocraft"))).unwrap();
        assert!(flatpak.command.ends_with("flatpak"));
        assert_eq!(flatpak.args, ["run", "--command=photocraft-cli", "ai.storyteller.photocraft", "mcp"]);
        assert!(server_for(photo(), &installed(InstallSource::AppImage, &exe("photocraft"))).unwrap_err().starts_with("The PhotoCraft AppImage"));
    }
}
