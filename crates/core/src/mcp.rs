//! Connecting the Craft apps to AI tools over the Model Context Protocol.
//!
//! Every Craft app ships a command-line tool, `<app>-cli`, whose `mcp` subcommand serves MCP on
//! stdio. AI tools (Claude, Codex, Cursor, VS Code…) start such servers from a settings file of
//! their own; the desktop backend adds or removes the apps' entries there. These are the shapes
//! the UI and the backend exchange.

use std::collections::BTreeMap;

/// How to start an app's MCP server: a program and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    pub command: String,
    pub args: Vec<String>,
}

impl McpServer {
    /// The servers as the `mcpServers` block most AI tools read, for pasting into one that isn't
    /// listed.
    pub fn snippet(servers: &[(&str, &McpServer)]) -> String {
        let entries: serde_json::Map<String, serde_json::Value> =
            servers.iter().map(|(name, s)| ((*name).to_string(), serde_json::json!({ "command": s.command, "args": s.args }))).collect();
        let doc = serde_json::json!({ "mcpServers": entries });
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    }
}

/// What an AI tool's settings hold under an app's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// The app's MCP server, started exactly as the launcher would start it now.
    Current,
    /// The app's MCP server started another way (an older path, other options). Connecting again
    /// brings it up to date.
    Different,
    /// Something else under the app's name: the launcher leaves it alone.
    Foreign,
}

/// One AI tool and what its settings hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiTool {
    /// Machine name (`claude-code`).
    pub id: String,
    /// Display name (`Claude Code`).
    pub name: String,
    /// The settings file the apps' servers go in.
    pub path: String,
    /// The tool looks installed (its settings folder exists).
    pub found: bool,
    /// App id → what the settings hold under that name, for every app they name.
    pub entries: BTreeMap<String, Entry>,
    /// Why the settings couldn't be read. They're left alone until that's fixed.
    pub error: Option<String>,
}

/// Which apps can be connected, and the AI tools they can be connected to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpReport {
    /// Installed app id → how to start its server, or why it can't be started here.
    pub servers: BTreeMap<String, Result<McpServer, String>>,
    pub tools: Vec<AiTool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_is_the_usual_mcp_servers_block() {
        let s = McpServer { command: r"C:\Program Files\PhotoCraft\photocraft-cli.exe".into(), args: vec!["mcp".into()] };
        let text = McpServer::snippet(&[("photocraft", &s)]);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["mcpServers"]["photocraft"]["command"], r"C:\Program Files\PhotoCraft\photocraft-cli.exe");
        assert_eq!(v["mcpServers"]["photocraft"]["args"], serde_json::json!(["mcp"]));
        assert_eq!(McpServer::snippet(&[]), "{\n  \"mcpServers\": {}\n}");
    }
}
