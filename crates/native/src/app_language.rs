//! Reading and updating per-app interface languages in the Craft Apps' preferences files, and
//! applying changes in real time over the apps' localhost control channel when they are open.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use artcraft_launcher_core::SUITE;
use artcraft_launcher_core::catalog::CraftApp;
use artcraft_launcher_core::status::Installed;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::paths::{Paths, write_atomic};

/// Returns `true` when `tag` is a syntactically valid BCP 47 language tag (e.g. `en`, `pt-BR`, `zh-Hans-CN`).
pub fn is_valid_bcp47(tag: &str) -> bool {
    let trimmed = tag.trim();
    if trimmed.is_empty() || trimmed.len() > 35 {
        return false;
    }
    trimmed.split(['-', '_']).all(|sub| !sub.is_empty() && sub.len() <= 8 && sub.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Converts a launcher language code (`auto`, `pt-BR`, `zh-CN`, `en`, …) into the lowercase
/// BCP 47 code stored by Craft Apps in `preferences.json` (`auto`, `pt-br`, `zh-hans`, `en`, …).
pub fn to_app_code(code: &str) -> String {
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") || !is_valid_bcp47(trimmed) {
        return "auto".to_string();
    }
    if trimmed.eq_ignore_ascii_case("zh-cn") || trimmed.eq_ignore_ascii_case("zh-hans") || trimmed.eq_ignore_ascii_case("zh") {
        return "zh-hans".to_string();
    }
    trimmed.to_ascii_lowercase()
}

/// Converts a language code stored by a Craft App (`zh-hans`, `pt-br`, `auto`, …) into the
/// canonical code used by the launcher's language selector (`zh-CN`, `pt-BR`, `auto`, …).
pub fn from_app_code(code: &str) -> String {
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") || !is_valid_bcp47(trimmed) {
        return "auto".to_string();
    }
    if trimmed.eq_ignore_ascii_case("zh-hans") || trimmed.eq_ignore_ascii_case("zh-cn") || trimmed.eq_ignore_ascii_case("zh") {
        return "zh-CN".to_string();
    }
    if trimmed.eq_ignore_ascii_case("pt-br") || trimmed.eq_ignore_ascii_case("pt") {
        return "pt-BR".to_string();
    }
    trimmed.to_ascii_lowercase()
}

/// The folder name a Craft App uses inside `%APPDATA%` (Windows) or `~/Library/Application Support` (macOS).
fn app_folder_name(app: &CraftApp) -> &'static str {
    if app.id == "photocraft" { "Photocraft" } else { app.name }
}

/// The portable-mode data directory (`<exe dir>/<App>Data`) when a portable marker or data folder
/// sits beside the installed executable.
fn portable_config_dir(app: &CraftApp, installed: Option<&Installed>) -> Option<PathBuf> {
    let loc = Path::new(&installed?.location);
    let exe_dir = if loc.is_dir() { loc } else { loc.parent()? };
    let marker = ["portable.txt", &format!("{}.portable", app.name), &format!("{}.portable", app_folder_name(app))]
        .into_iter()
        .any(|m| exe_dir.join(m).is_file());
    let data_dir = exe_dir.join(format!("{}Data", app.name));
    if marker || data_dir.is_dir() { Some(data_dir) } else { None }
}

/// The per-user configuration directory of `app` on the current platform.
pub fn platform_config_dir(app: &CraftApp) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support").join(app_folder_name(app)));
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").filter(|a| !a.is_empty()).map(|a| PathBuf::from(a).join(app_folder_name(app)));
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".config")))
        .map(|c| c.join(app.id))
}

/// Candidate configuration directories for `app`, in priority order (portable mode, launcher
/// sandbox home, then the platform's per-user config directory).
pub fn config_dirs(app: &CraftApp, installed: Option<&Installed>, paths: &Paths) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(portable) = portable_config_dir(app, installed) {
        dirs.push(portable);
    }
    let app_home = paths.apps().join(app.id).join("home");
    if app_home.is_dir() {
        dirs.push(app_home.join(".config").join(app.id));
    }
    if let Some(platform) = platform_config_dir(app)
        && !dirs.contains(&platform)
    {
        dirs.push(platform);
    }
    dirs
}

/// Reads the configured interface language of `app` from its `preferences.json` or `ui.json`, if
/// one has been saved on disk.
pub fn read_app_language(app: &CraftApp, installed: Option<&Installed>, paths: &Paths) -> Option<String> {
    for dir in config_dirs(app, installed, paths) {
        for file in ["preferences.json", "ui.json"] {
            let path = dir.join(file);
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(val) = serde_json::from_str::<Value>(&text) else { continue };
            if let Some(code) = val.pointer("/interface/language").and_then(Value::as_str).or_else(|| val.get("language").and_then(Value::as_str)) {
                return Some(from_app_code(code));
            }
        }
    }
    None
}

/// Reads the current interface languages of all apps in [`SUITE`] from disk, merging with any
/// existing preferences.
pub fn detect_app_languages(installed: &std::collections::HashMap<String, Installed>, paths: &Paths) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for app in &SUITE {
        if let Some(lang) = read_app_language(app, installed.get(app.id), paths) {
            out.insert(app.id.to_string(), lang);
        }
    }
    out
}

/// Updates `interface.language` in `app`'s `preferences.json` (and `ui.json` if present) without
/// disturbing any other preferences in the file.
pub fn write_app_language(app: &CraftApp, installed: Option<&Installed>, paths: &Paths, language: &str) -> Result<(), String> {
    let dirs = config_dirs(app, installed, paths);
    let Some(primary_dir) = dirs.first() else {
        return Err("no configuration folder found on this system".into());
    };
    let app_code = to_app_code(language);
    for (idx, dir) in dirs.iter().enumerate() {
        let prefs_path = dir.join("preferences.json");
        if idx == 0 || prefs_path.is_file() {
            update_prefs_file(&prefs_path, &app_code)?;
        }
        let ui_path = dir.join("ui.json");
        if ui_path.is_file() {
            let _ = update_prefs_file(&ui_path, &app_code);
        }
    }
    let _ = primary_dir;
    Ok(())
}

/// Updates `interface.language` in the JSON preferences file at `path`, creating a new preferences
/// object only when reading fails with `NotFound`, returning an error for invalid JSON or a
/// non-object root before writing, and atomically writing the updated content to disk.
fn update_prefs_file(path: &Path, app_code: &str) -> Result<(), String> {
    let mut root = match std::fs::read_to_string(path) {
        Ok(text) => {
            let val = serde_json::from_str::<Value>(&text).map_err(|e| format!("invalid JSON in {}: {e}", path.display()))?;
            if !val.is_object() {
                return Err(format!("expected JSON object in {}", path.display()));
            }
            val
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    if let Some(obj) = root.as_object_mut() {
        let interface = obj.entry("interface").or_insert_with(|| Value::Object(Map::new()));
        if !interface.is_object() {
            *interface = Value::Object(Map::new());
        }
        if let Some(iface_obj) = interface.as_object_mut() {
            iface_obj.insert("language".to_string(), Value::String(app_code.to_string()));
        }
    }
    let bytes = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    write_atomic(path, &bytes)
}

// ------------------------------------------------------------------ live control channel

/// The TCP port and secret authorization token used to connect to an app's control channel.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ControlEndpoint {
    port: u16,
    token: String,
}

/// Path to the JSON file recording the active control channel endpoint for `app_id`.
fn control_file(paths: &Paths, app_id: &str) -> PathBuf {
    paths.data.join("control").join(format!("{app_id}.json"))
}

/// Generates a 64-character lowercase hexadecimal token from the OS CSPRNG for authenticating the
/// loopback control channel between the launcher and a Craft App it starts. Returns `None` if
/// random byte generation fails.
fn generate_hex_token() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Restricts access to `path` so only the current user account can read or write it,
/// removing inherited permissions on Windows. Returns `false` when `path` is a symlink,
/// cannot be secured, or is not a regular file.
#[cfg(windows)]
fn secure_control_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.is_symlink() || !meta.is_file() {
        return false;
    }
    let sid = match crate::msi_context::current_user_sid() {
        Ok(sid) => sid,
        Err(e) => {
            log::warn!("couldn't get current user SID to secure {}: {e}", path.display());
            return false;
        }
    };
    let system_root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let icacls = system_root.join(r"System32\icacls.exe");
    let arg_grant = format!("*{sid}:(F)");
    let Ok(output) = crate::process::command(&icacls).arg(path).arg("/inheritance:r").arg("/grant:r").arg(&arg_grant).output() else {
        return false;
    };
    output.status.success()
}

/// Restricts access to `path` so only the owner can read or write it (mode 0o600).
/// Returns `false` when `path` is a symlink, cannot be secured, or is not a regular file.
#[cfg(unix)]
fn secure_control_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.is_symlink() || !meta.is_file() {
        return false;
    }
    if std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).is_err() {
        return false;
    }
    let Ok(updated) = std::fs::metadata(path) else {
        return false;
    };
    (updated.permissions().mode() & 0o077) == 0
}

/// Fallback for non-Windows, non-Unix platforms.
#[cfg(not(any(windows, unix)))]
fn secure_control_file(_path: &Path) -> bool {
    true
}

/// Prepares or reuses the control channel endpoint for `app_id` under `paths.data`.
/// Returns the allocated or existing `ControlEndpoint` recording the port and token.
fn prepare_control_endpoint(paths: &Paths, app_id: &str) -> Option<ControlEndpoint> {
    let file = control_file(paths, app_id);
    if file.is_file()
        && secure_control_file(&file)
        && let Some(existing) = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<ControlEndpoint>(&t).ok())
        && port_is_listening(existing.port)
    {
        return Some(existing);
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let addr = listener.local_addr().ok()?;
    let token = generate_hex_token()?;
    let port = addr.port();
    drop(listener);
    let endpoint = ControlEndpoint { port, token };
    if let Ok(bytes) = serde_json::to_vec(&endpoint)
        && write_atomic(&file, &bytes).is_ok()
        && !secure_control_file(&file)
    {
        let _ = std::fs::remove_file(&file);
    }
    Some(endpoint)
}

/// Allocates a loopback control port and token for `app` before launching it, saving them under
/// the launcher's state directory and returning the environment variables to pass to the child.
pub fn prepare_launch_env(app: &CraftApp, installed: Option<&Installed>, paths: &Paths, configured_lang: Option<&str>) -> Vec<(String, String)> {
    let mut envs = Vec::new();
    let upper = app.id.to_ascii_uppercase().replace('-', "_");
    let effective_lang = configured_lang.map(str::to_string).or_else(|| read_app_language(app, installed, paths));
    if let Some(lang) = effective_lang {
        let _ = write_app_language(app, installed, paths, &lang);
        let app_code = to_app_code(&lang);
        if app_code != "auto" {
            envs.push((format!("{upper}_LOCALE"), app_code));
        }
    }
    if let Some(endpoint) = prepare_control_endpoint(paths, app.id) {
        envs.push((format!("{upper}_CONTROL_PORT"), endpoint.port.to_string()));
        envs.push((format!("{upper}_CONTROL_TOKEN"), endpoint.token));
    }
    envs
}

/// Returns `true` if a TCP listener is actively accepting connections on localhost at `port`.
fn port_is_listening(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_ok()
}

/// Sends a live `prefs.set` (`interface.language`) request to a running Craft App over its
/// loopback control channel so its UI updates immediately in the current frame without restarting.
/// Returns `true` when a running instance acknowledged the update.
pub fn apply_live_language(app: &CraftApp, paths: &Paths, language: &str) -> bool {
    let file = control_file(paths, app.id);
    if !file.is_file() || !secure_control_file(&file) {
        return false;
    }
    let Some(endpoint) = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<ControlEndpoint>(&t).ok()) else {
        return false;
    };
    send_control_language(endpoint.port, &endpoint.token, &to_app_code(language))
}

/// Returns `true` if the JSON-RPC reply indicates the requested method is not supported by the server.
fn is_unsupported_method(reply: &Value) -> bool {
    if let Some(err) = reply.get("error") {
        if let Some(code) = err.get("code").and_then(Value::as_i64) {
            return code == -32601;
        }
        if let Some(msg) = err.get("message").and_then(Value::as_str).or_else(|| err.as_str()) {
            let lower = msg.to_ascii_lowercase();
            return lower.contains("method not found") || lower.contains("unknown method");
        }
    }
    false
}

/// Connects to the loopback control channel at `port` and sends the language update request.
/// Authenticates if supported by the server; allows proceeding if the server rejects `auth` as
/// an unsupported method (e.g. SoundCraft), but rejects actual authentication failures.
fn send_control_language(port: u16, token: &str, app_code: &str) -> bool {
    if port == 0 {
        return false;
    }
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let Ok(read_half) = stream.try_clone() else { return false };
    let mut reader = BufReader::new(read_half);

    let auth = json!({
        "id": 1,
        "method": "auth",
        "params": { "token": token }
    });
    if writeln!(stream, "{auth}").is_err() || stream.flush().is_err() {
        return false;
    }
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    let Ok(auth_reply) = serde_json::from_str::<Value>(&line) else { return false };
    let auth_ok =
        auth_reply.get("ok").and_then(Value::as_bool) == Some(true) || (auth_reply.get("result").is_some() && auth_reply.get("error").is_none());
    if !auth_ok && !is_unsupported_method(&auth_reply) {
        return false;
    }

    let req = json!({
        "id": 2,
        "method": "engine.execute",
        "params": {
            "command": "prefs.set",
            "params": {
                "path": "interface.language",
                "value": app_code
            }
        }
    });
    if writeln!(stream, "{req}").is_err() || stream.flush().is_err() {
        return false;
    }
    line.clear();
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    if serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("ok").and_then(Value::as_bool)) == Some(true) {
        return true;
    }

    // Fallback for apps that switch language via `ui.menu.invoke` (`app.language.<code>`).
    let menu_req = json!({
        "id": 3,
        "method": "ui.menu.invoke",
        "params": {
            "id": format!("app.language.{app_code}")
        }
    });
    if writeln!(stream, "{menu_req}").is_err() || stream.flush().is_err() {
        return false;
    }
    line.clear();
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("ok").and_then(Value::as_bool)) == Some(true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn language_codes_round_trip_between_launcher_and_apps() {
        assert_eq!(to_app_code("auto"), "auto");
        assert_eq!(to_app_code("pt-BR"), "pt-br");
        assert_eq!(to_app_code("zh-CN"), "zh-hans");
        assert_eq!(to_app_code("ja"), "ja");

        assert_eq!(from_app_code("auto"), "auto");
        assert_eq!(from_app_code("pt-br"), "pt-BR");
        assert_eq!(from_app_code("zh-hans"), "zh-CN");
        assert_eq!(from_app_code("de"), "de");

        assert_eq!(to_app_code("../../etc/passwd"), "auto");
        assert_eq!(to_app_code("rm -rf /"), "auto");
        assert_eq!(from_app_code("malicious;cmd"), "auto");
    }

    #[test]
    fn writing_preferences_preserves_other_settings_and_updates_language() {
        let dir = std::env::temp_dir().join(format!("craft-launcher-app-lang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prefs_path = dir.join("preferences.json");
        std::fs::write(&prefs_path, r#"{"general":{"beepWhenDone":true},"interface":{"theme":"dark"}}"#).unwrap();

        update_prefs_file(&prefs_path, "pt-br").unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&prefs_path).unwrap()).unwrap();
        assert_eq!(saved["general"]["beepWhenDone"], json!(true));
        assert_eq!(saved["interface"]["theme"], json!("dark"));
        assert_eq!(saved["interface"]["language"], json!("pt-br"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_control_channel_authenticates_and_sends_prefs_set() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = generate_hex_token().unwrap();
        let expected_token = token.clone();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let auth: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(auth["method"], "auth");
            assert_eq!(auth["params"]["token"], expected_token);
            writeln!(stream, "{}", json!({"id": 1, "ok": true, "result": {"authenticated": true}})).unwrap();

            line.clear();
            reader.read_line(&mut line).unwrap();
            let cmd: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(cmd["method"], "engine.execute");
            assert_eq!(cmd["params"]["command"], "prefs.set");
            assert_eq!(cmd["params"]["params"]["path"], "interface.language");
            assert_eq!(cmd["params"]["params"]["value"], "pt-br");
            writeln!(stream, "{}", json!({"id": 2, "ok": true, "result": {}})).unwrap();
        });

        assert!(send_control_language(port, &token, "pt-br"));
        server.join().unwrap();
    }
    #[test]
    fn invalid_or_non_object_preferences_file_is_preserved_and_errors() {
        let dir = std::env::temp_dir().join(format!("craft-launcher-app-lang-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let invalid_json_path = dir.join("invalid.json");
        std::fs::write(&invalid_json_path, "{broken json").unwrap();
        assert!(update_prefs_file(&invalid_json_path, "pt-br").is_err());
        assert_eq!(std::fs::read_to_string(&invalid_json_path).unwrap(), "{broken json");

        let array_path = dir.join("array.json");
        std::fs::write(&array_path, "[1, 2, 3]").unwrap();
        assert!(update_prefs_file(&array_path, "pt-br").is_err());
        assert_eq!(std::fs::read_to_string(&array_path).unwrap(), "[1, 2, 3]");

        let non_existent = dir.join("new_prefs.json");
        assert!(update_prefs_file(&non_existent, "pt-br").is_ok());
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&non_existent).unwrap()).unwrap();
        assert_eq!(saved["interface"]["language"], json!("pt-br"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn soundcraft_unsupported_auth_allows_control_language_to_proceed() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = "test-token";

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let auth: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(auth["method"], "auth");
            // SoundCraft rejects unsupported auth method:
            writeln!(stream, "{}", json!({"id": 1, "error": {"code": -32601, "message": "Method not found"}})).unwrap();

            line.clear();
            reader.read_line(&mut line).unwrap();
            let cmd: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(cmd["method"], "engine.execute");
            assert_eq!(cmd["params"]["command"], "prefs.set");
            writeln!(stream, "{}", json!({"id": 2, "ok": true, "result": {}})).unwrap();
        });

        assert!(send_control_language(port, token, "pt-br"));
        server.join().unwrap();
    }

    #[test]
    fn auth_failure_is_rejected_without_sending_control_commands() {
        for rejection in ["unauthorized", "token not found", "invalid token"] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let token = "bad-token";

            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let auth: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(auth["method"], "auth");
                // Server supports auth, but rejects invalid token:
                writeln!(stream, "{}", json!({"id": 1, "ok": false, "error": rejection})).unwrap();
            });

            assert!(!send_control_language(port, token, "pt-br"));
            server.join().unwrap();
        }
    }

    #[test]
    fn secure_control_file_protects_file() {
        let dir = std::env::temp_dir().join(format!("craft-launcher-secure-control-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.json");
        std::fs::write(&file, "{\"port\":1234,\"token\":\"abc\"}").unwrap();

        assert!(secure_control_file(&file));
        assert!(secure_control_file(&file));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_control_endpoint_creates_and_reuses_secure_file() {
        let dir = std::env::temp_dir().join(format!("craft-launcher-launch-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths::under(&dir);

        let endpoint = prepare_control_endpoint(&paths, "photocraft").unwrap();
        let cfile = control_file(&paths, "photocraft");
        assert!(cfile.is_file());
        assert!(secure_control_file(&cfile));

        let listener = TcpListener::bind(("127.0.0.1", endpoint.port)).unwrap();
        let reused = prepare_control_endpoint(&paths, "photocraft").unwrap();
        assert_eq!(reused.port, endpoint.port);
        assert_eq!(reused.token, endpoint.token);
        drop(listener);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
