//! Reading and updating per-app interface languages in the Craft Apps' preferences files, and
//! applying changes in real time over the apps' localhost control channel when they are open.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use artcraft_launcher_core::SUITE;
use artcraft_launcher_core::catalog::CraftApp;
use artcraft_launcher_core::status::Installed;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::paths::{Paths, write_atomic};

/// Converts a launcher language code (`auto`, `pt-BR`, `zh-CN`, `en`, …) into the lowercase
/// BCP 47 code stored by Craft Apps in `preferences.json` (`auto`, `pt-br`, `zh-hans`, `en`, …).
pub fn to_app_code(code: &str) -> String {
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
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
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
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

fn update_prefs_file(path: &Path, app_code: &str) -> Result<(), String> {
    let mut root = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()));
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ControlEndpoint {
    port: u16,
    token: String,
}

fn control_file(paths: &Paths, app_id: &str) -> PathBuf {
    paths.data.join("control").join(format!("{app_id}.json"))
}

/// Generates a 64-character lowercase hexadecimal token for authenticating the loopback control
/// channel between the launcher and a Craft App it starts.
fn generate_hex_token(app_id: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let pid = u128::from(std::process::id());
    let seq = u128::from(COUNTER.fetch_add(1, Ordering::Relaxed));
    let app_hash = app_id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |acc, b| (acc ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    let a = nanos ^ (pid << 32) ^ u128::from(app_hash);
    let b = nanos.wrapping_mul(0x9e37_79b9_7f4a_7c15_u128) ^ (seq << 48) ^ u128::from(app_hash.rotate_left(17));
    format!("{a:032x}{b:032x}")
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
    let file = control_file(paths, app.id);
    if let Some(existing) = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<ControlEndpoint>(&t).ok())
        && port_is_listening(existing.port)
    {
        envs.push((format!("{upper}_CONTROL_PORT"), existing.port.to_string()));
        envs.push((format!("{upper}_CONTROL_TOKEN"), existing.token));
        return envs;
    }
    if let Ok(listener) = TcpListener::bind(("127.0.0.1", 0))
        && let Ok(addr) = listener.local_addr()
    {
        let port = addr.port();
        drop(listener);
        let token = generate_hex_token(app.id);
        let endpoint = ControlEndpoint { port, token: token.clone() };
        if let Ok(bytes) = serde_json::to_vec(&endpoint) {
            let _ = write_atomic(&file, &bytes);
        }
        envs.push((format!("{upper}_CONTROL_PORT"), port.to_string()));
        envs.push((format!("{upper}_CONTROL_TOKEN"), token));
    }
    envs
}

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
    let Some(endpoint) = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<ControlEndpoint>(&t).ok()) else {
        return false;
    };
    send_control_language(endpoint.port, &endpoint.token, &to_app_code(language))
}

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
    if auth_reply.get("ok").and_then(Value::as_bool) != Some(true) {
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
        let token = generate_hex_token("photocraft");
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
}
