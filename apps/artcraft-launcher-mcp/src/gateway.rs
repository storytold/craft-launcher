//! A stable tool catalog; downstream schemas are discovered only when requested.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use artcraft_launcher_native::process;
use serde_json::{Value, json};

use crate::discovery;
use crate::transport::Client;

struct Session {
    app: String,
    mode: String,
    port: Option<u16>,
    initial_document_inspection_error: Option<Value>,
    client: Client,
}

pub(crate) struct Gateway {
    cli_dirs: Vec<PathBuf>,
    sessions: BTreeMap<String, Session>,
    next_session: u64,
    initialized: bool,
}

impl Gateway {
    pub fn new(cli_dirs: Vec<PathBuf>) -> Self {
        Self { cli_dirs, sessions: BTreeMap::new(), next_session: 0, initialized: false }
    }

    pub fn handle(&mut self, request: Value) -> Option<Value> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str);
        let valid_id = id.is_null() || id.is_string() || id.is_i64() || id.is_u64();
        if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() || !valid_id {
            return Some(crate::rpc_error(Value::Null, -32600, "Invalid JSON-RPC request"));
        }
        let method = method.unwrap_or_default();
        // Never execute a write carried by a notification.
        request.get("id")?;
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Some(crate::rpc_error(id, -32602, "params must be an object"));
        }
        let result = match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
                let version = match requested {
                    "2024-11-05" | "2025-03-26" => requested,
                    _ => "2025-06-18",
                };
                self.initialized = true;
                Ok(json!({
                    "protocolVersion":version, "capabilities":{"tools":{}},
                    "serverInfo":{"name":"artcraft-launcher", "title":"ArtCraft Launcher", "version":env!("CARGO_PKG_VERSION")},
                    "instructions":"Use apps_list to discover installed Craft Apps. Connect the named app with app_connect, then inspect app_tools for exact schemas. Use app_read for verified read-only tools and app_call for edits. Live mode is explicit; never silently substitute headless. Pin the returned session id. Inspect before edits and after an uncertain result; never blindly retry a mutation. Return previews as images and preserve native files."
                }))
            }
            "ping" => Ok(json!({})),
            _ if !self.initialized => return Some(crate::rpc_error(id, -32002, "Initialize the MCP connection first")),
            "tools/list" => Ok(json!({"tools":tools()})),
            "tools/call" => match self.call(&params) {
                Ok(result) => Ok(result),
                Err(error) => Ok(json!({"content":[{"type":"text", "text":error}], "isError":true})),
            },
            _ => return Some(crate::rpc_error(id, -32601, "Unknown method")),
        };
        match result {
            Ok(result) => Some(json!({"jsonrpc":"2.0", "id":id, "result":result})),
            Err(error) => Some(crate::rpc_error(id, -32603, error)),
        }
    }

    fn call(&mut self, params: &Value) -> Result<Value, String> {
        keys(params, &["name", "arguments", "_meta"])?;
        let name = string(params, "name")?;
        let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Err("arguments must be an object".into());
        }
        match name {
            "apps_list" => {
                keys(&arguments, &[])?;
                let apps: Vec<Value> = discovery::scan(&self.cli_dirs).iter().map(|a| a.describe()).collect();
                text(json!({"apps":apps}))
            }
            "app_connect" => self.connect(&arguments),
            "app_tools" => {
                keys(&arguments, &["session", "filter"])?;
                let session = self.session(&arguments)?;
                let filter = match arguments.get("filter") {
                    None => String::new(),
                    Some(value) => value.as_str().ok_or("filter must be a string")?.to_lowercase(),
                };
                let matches: Vec<Value> = session
                    .client
                    .tools
                    .iter()
                    .filter(|tool| {
                        ["name", "description"]
                            .iter()
                            .any(|key| tool.get(key).and_then(Value::as_str).is_some_and(|v| v.to_lowercase().contains(&filter)))
                    })
                    .cloned()
                    .collect();
                let total = matches.len();
                text(json!({"tools":matches.into_iter().take(40).collect::<Vec<_>>(), "total":total, "truncated":total > 40}))
            }
            "app_read" | "app_call" => {
                keys(&arguments, &["session", "tool", "arguments"])?;
                let tool_name = string(&arguments, "tool")?.to_string();
                let tool_arguments = arguments.get("arguments").cloned().unwrap_or_else(|| json!({}));
                if !tool_arguments.is_object() {
                    return Err("Downstream arguments must be an object".into());
                }
                let session = self.session(&arguments)?;
                let tool = session
                    .client
                    .tools
                    .iter()
                    .find(|t| t.get("name").and_then(Value::as_str) == Some(&tool_name))
                    .ok_or("Unknown app tool; use app_tools to discover its schema")?;
                if name == "app_read" && tool.pointer("/annotations/readOnlyHint").and_then(Value::as_bool) != Some(true) {
                    return Err("This tool does not declare itself read-only; use app_call with normal edit permissions".into());
                }
                // The native MCP implementation validates its own input schema and executes.
                // Preserve the full CallToolResult, including images, structuredContent and isError.
                session.client.request("tools/call", json!({"name":tool_name, "arguments":tool_arguments}))
            }
            _ => Err(format!("Unknown gateway tool: {name}")),
        }
    }

    fn session(&mut self, arguments: &Value) -> Result<&mut Session, String> {
        let id = string(arguments, "session")?;
        self.sessions.get_mut(id).ok_or_else(|| "Unknown session; use app_connect first".into())
    }

    fn connect(&mut self, arguments: &Value) -> Result<Value, String> {
        keys(arguments, &["app", "mode", "port"])?;
        let app = string(arguments, "app")?;
        let mode = arguments.get("mode").map_or(Ok("live"), |v| v.as_str().ok_or("mode must be a string"))?;
        if !matches!(mode, "live" | "headless") {
            return Err("mode must be live or headless".into());
        }
        let supplied_port = arguments
            .get("port")
            .map(|v| v.as_u64().and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0).ok_or("port must be an integer from 1 to 65535"))
            .transpose()?;
        if mode == "headless" && supplied_port.is_some() {
            return Err("port is only valid in live mode".into());
        }
        let flag = discovery::live_flag(app);
        if mode == "live" && flag.is_none() {
            return Err(format!(
                "Live mode for {app} is not implemented in this gateway version. Explicitly choose mode=headless for file-based editing; it will not update a desktop window."
            ));
        }
        let mut target_port = supplied_port;
        let mut replaced_session = None;
        if let Some((id, session)) =
            self.sessions.iter().find(|(_, s)| s.app == app && s.mode == mode && (supplied_port.is_none() || s.port == supplied_port))
        {
            if session.client.healthy {
                return describe_session(id, session);
            }
            // Explicit reconnection must inspect the same live document after a lost reply.
            // Never replay the failed operation or silently open a different document.
            if mode == "live" {
                target_port = session.port;
            }
            replaced_session = Some(id.clone());
        }
        if replaced_session.is_none() && self.sessions.len() >= 32 {
            return Err("This gateway already has 32 sessions. Save work and restart it before opening more.".into());
        }
        let found = discovery::scan(&self.cli_dirs).into_iter().find(|a| a.app.id == app).ok_or("Unknown app; use an app id from apps_list")?;
        let mut program = found.cli.clone().ok_or_else(|| {
            format!("{app}'s CLI was not found. Install it through ArtCraft Launcher, or supply its trusted folder with --cli-dir.")
        })?;
        program.args.push("mcp".into());
        let port = if mode == "live" {
            let port = match target_port {
                Some(port) => port,
                None => {
                    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
                    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                    drop(listener);
                    let mut gui = found.gui()?.command();
                    gui.args(["--control", &port.to_string()]);
                    process::spawn_detached(gui)?;
                    wait_for_app(port).map_err(|e| format!("{e} (control port {port})"))?;
                    port
                }
            };
            program.args.extend([flag.ok_or("No live adapter")?.into(), format!("127.0.0.1:{port}")]);
            Some(port)
        } else {
            None
        };
        let mut client = Client::start(&program).map_err(|e| format!("Could not connect {app} (control port {port:?}): {e}"))?;
        let mut initial_document_inspection_error = None;
        if mode == "live" {
            let inspect = if app == "vectorcraft" { "doc_inspect" } else { "inspect_document" };
            if !client.tools.iter().any(|t| t.get("name").and_then(Value::as_str) == Some(inspect)) {
                return Err("The installed CLI lacks the expected live inspection tool; check its version".into());
            }
            let state = client.request("tools/call", json!({"name":inspect, "arguments":{}}))?;
            let ui = if state.get("isError").and_then(Value::as_bool) == Some(true) {
                // Some apps start at a welcome screen. Verify the desktop endpoint without
                // creating a document, then expose the inspection error to the caller.
                let tool = ["inspect_ui", "ui_inspect"]
                    .into_iter()
                    .find(|name| {
                        client.tools.iter().any(|t| {
                            t.get("name").and_then(Value::as_str) == Some(name)
                                && t.pointer("/annotations/readOnlyHint").and_then(Value::as_bool) == Some(true)
                        })
                    })
                    .ok_or_else(|| format!("Cannot verify the live app on port {port:?}: {state}"))?;
                Some(client.request("tools/call", json!({"name":tool, "arguments":{}}))?)
            } else {
                None
            };
            initial_document_inspection_error = validate_live_inspection(state, ui)?;
        }
        self.next_session = self.next_session.checked_add(1).ok_or("Session id exhausted")?;
        let id = format!("{app}-{}", self.next_session);
        let session = Session { app: app.into(), mode: mode.into(), port, initial_document_inspection_error, client };
        let result = describe_session(&id, &session)?;
        if let Some(old_id) = replaced_session {
            self.sessions.remove(&old_id);
        }
        self.sessions.insert(id, session);
        Ok(result)
    }
}

fn wait_for_app(port: u16) -> Result<(), String> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("The app did not open its control endpoint within 20 seconds. Check its window, then connect to its port explicitly.".into())
}

fn describe_session(id: &str, session: &Session) -> Result<Value, String> {
    text(json!({"session":id, "app":session.app, "mode":session.mode, "port":session.port, "tool_count":session.client.tools.len(),
        "initial_document_inspection_error":session.initial_document_inspection_error}))
}

fn validate_live_inspection(document: Value, ui: Option<Value>) -> Result<Option<Value>, String> {
    if document.get("isError").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    if ui.as_ref().is_some_and(|result| result.get("isError").and_then(Value::as_bool) != Some(true)) {
        return Ok(Some(document));
    }
    Err(format!("Neither the live document nor desktop UI could be inspected: {document}; UI: {ui:?}"))
}

fn text(value: Value) -> Result<Value, String> {
    Ok(json!({"content":[{"type":"text", "text":serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?}]}))
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| format!("{key} must be a string"))
}

fn keys(value: &Value, allowed: &[&str]) -> Result<(), String> {
    let object = value.as_object().ok_or("Expected an object")?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("Unknown argument: {key}"));
    }
    Ok(())
}

fn tools() -> Vec<Value> {
    let app = json!({"type":"string", "enum":artcraft_launcher_core::SUITE.iter().map(|a|a.id).collect::<Vec<_>>()});
    let session = json!({"type":"string", "description":"Session handle returned by app_connect"});
    let dispatch = json!({"type":"object", "properties":{"session":session, "tool":{"type":"string"}, "arguments":{"type":"object"}}, "required":["session","tool"], "additionalProperties":false});
    vec![
        tool(
            "apps_list",
            "Discover installed Craft Apps now, including apps installed since Codex started. Does not launch apps.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
            true,
            false,
        ),
        tool(
            "app_connect",
            "Connect to a named app. Default live mode opens a separate controlled desktop window on demand; supplying port attaches to an operator-selected localhost endpoint. Live adapters: FigCraft, DesignCraft, VectorCraft. Explicit headless mode edits a separate in-memory file session, not a visible window.",
            json!({"type":"object","properties":{"app":app,"mode":{"type":"string","enum":["live","headless"]},"port":{"type":"integer","minimum":1,"maximum":65535}},"required":["app"],"additionalProperties":false}),
            false,
            false,
        ),
        tool(
            "app_tools",
            "Discover the exact native tool schemas for a connected app. Filter by purpose or tool name; at most 40 matches are returned. Narrow the filter if truncated.",
            json!({"type":"object","properties":{"session":session,"filter":{"type":"string"}},"required":["session"],"additionalProperties":false}),
            true,
            false,
        ),
        tool(
            "app_read",
            "Call a native tool that explicitly declares readOnlyHint=true, using its exact arguments from app_tools. Inspection and rendered previews return native content, including images.",
            dispatch.clone(),
            true,
            false,
        ),
        tool(
            "app_call",
            "Call a native editing, file, export or other tool with exact arguments from app_tools. May change documents, replace files or access external services, depending on the selected app tool. Inspect before editing. Do not repeat a write after an unknown outcome.",
            dispatch,
            false,
            true,
        ),
    ]
}

fn tool(name: &str, description: &str, schema: Value, read_only: bool, destructive: bool) -> Value {
    let open_world = !read_only || name == "app_read";
    json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"readOnlyHint":read_only,"destructiveHint":destructive,"idempotentHint":read_only,"openWorldHint":open_world}})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initialized() -> Gateway {
        let mut gateway = Gateway::new(Vec::new());
        gateway.handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}));
        gateway
    }

    #[test]
    fn notifications_never_execute_tools_or_return_replies() {
        let mut gateway = initialized();
        assert!(
            gateway.handle(json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"app_connect","arguments":{"app":"figcraft"}}})).is_none()
        );
        assert!(gateway.sessions.is_empty());
    }

    #[test]
    fn tools_are_stable_without_installed_apps_and_edits_are_not_read_only() {
        let mut gateway = initialized();
        let reply = gateway.handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 5);
        assert_eq!(tools.iter().find(|t| t["name"] == "app_call").unwrap()["annotations"]["destructiveHint"], true);
        assert_eq!(tools.iter().find(|t| t["name"] == "app_read").unwrap()["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn refuses_invalid_arguments_before_launching_anything() {
        let mut gateway = initialized();
        for arguments in [
            json!({"app":"figcraft","mode":"headless","port":9}),
            json!({"app":"figcraft","port":0}),
            json!({"app":"figcraft","port":65536}),
            json!({"app":"figcraft","prot":12}),
            json!({"app":"pdfcraft"}),
        ] {
            let reply =
                gateway.handle(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"app_connect","arguments":arguments}})).unwrap();
            assert_eq!(reply["result"]["isError"], true);
            assert!(gateway.sessions.is_empty());
        }
    }

    #[test]
    fn invalid_envelopes_and_unknown_methods_have_protocol_errors() {
        let mut gateway = initialized();
        assert_eq!(gateway.handle(json!({"id":1,"method":"ping"})).unwrap()["error"]["code"], -32600);
        assert_eq!(gateway.handle(json!({"jsonrpc":"2.0","id":1,"method":"missing"})).unwrap()["error"]["code"], -32601);
        assert_eq!(gateway.handle(json!({"jsonrpc":"2.0","id":1,"method":"ping","params":[]})).unwrap()["error"]["code"], -32602);
    }

    #[test]
    fn an_empty_document_needs_a_successful_live_ui_probe() {
        let error = json!({"content":[{"type":"text","text":"no document open"}],"isError":true});
        let ready = json!({"content":[{"type":"text","text":"desktop state"}]});
        assert_eq!(validate_live_inspection(error.clone(), Some(ready.clone())).unwrap(), Some(error.clone()));
        assert!(validate_live_inspection(error.clone(), None).is_err());
        assert!(validate_live_inspection(error.clone(), Some(error)).is_err());
        assert_eq!(validate_live_inspection(ready, None).unwrap(), None);
    }
}
