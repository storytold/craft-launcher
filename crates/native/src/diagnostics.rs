//! Local, bounded diagnostic snapshots. Never probes the network or starts a Craft App.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use artcraft_launcher_core::diagnostics::{DiagnosticContext, MAX_REPORT_BYTES};
use artcraft_launcher_core::{Installed, Release, SUITE};
use serde_json::{Value, json};

use crate::paths::Paths;

const MAX_MESSAGE: usize = 1024;
const LOG_TAIL_BYTES: u64 = 32 * 1024;
const MAX_LOG_LINES: usize = 8;
pub(crate) const MAX_FAILURES: usize = 24;

/// Kept in memory even when the log file is unavailable.
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub at: i64,
    pub app: String,
    pub operation: &'static str,
    pub message: String,
}

impl Failure {
    pub fn new(app: &str, operation: &'static str, message: &str) -> Self {
        Self { at: crate::now(), app: bounded(app), operation, message: bounded(message) }
    }
}

fn bounded(value: &str) -> String {
    if value.len() <= MAX_MESSAGE { value.to_owned() } else { "[omitted: size limit]".to_owned() }
}

/// Free-form text is untrusted, even when it came from a local helper.
/// Drop secret-bearing messages wholesale; never export a credential's partial value.
pub(crate) fn redact(value: &str) -> String {
    if value.len() > MAX_MESSAGE {
        return "[omitted: size limit]".into();
    }
    let lower = value.to_ascii_lowercase();
    if [
        "token",
        "password",
        "passwd",
        "secret",
        "authorization",
        "bearer",
        "cookie",
        "api_key",
        "api-key",
        "apikey",
        "credential",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "glpat_",
        "github_pat_",
        "-----begin",
        "access_key",
        "private_key",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "[omitted: possible credentials]".into();
    }

    // Replace paths (including quoted paths with spaces), URLs and email-like words. Preserve
    // plain error descriptions and numeric OS error codes, never filenames from user documents.
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let next = chars.get(i + 1).copied();
        let after = chars.get(i + 2).copied();
        let drive = c.is_ascii_alphabetic() && next == Some(':') && matches!(after, Some('/' | '\\'));
        let path = c == '/' || c == '\\' || (c == '~' && matches!(next, Some('/' | '\\'))) || drive;
        let rest: String = chars.iter().skip(i).collect();
        let url = rest.starts_with("https://") || rest.starts_with("http://") || rest.starts_with("file://");
        if path || url {
            out.push_str(if url { "[url]" } else { "[path]" });
            let quoted = i.checked_sub(1).and_then(|n| chars.get(n)).copied().filter(|c| *c == '\'' || *c == '"');
            while let Some(&part) = chars.get(i) {
                if quoted.is_some_and(|quote| part == quote)
                    || (quoted.is_none()
                        && if url {
                            part.is_whitespace() || matches!(part, '"' | '\'')
                        } else {
                            part == '\n' || matches!(part, '(' | ')' | '[' | ']' | '}' | '"' | '\'')
                        })
                {
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Scan ordinary words together, so an email loses both the address and its local part.
        if !c.is_whitespace() && !matches!(c, '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'') {
            let start = i;
            while let Some(&part) = chars.get(i) {
                // A path or URL beginning inside a word must still be redacted (path=... etc.).
                if part.is_whitespace() || matches!(part, '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '/' | '\\' | '=') {
                    break;
                }
                if i > start && part == ':' && chars.get(i + 1).is_some_and(|c| *c == '/' || *c == '\\') {
                    break;
                }
                i += 1;
            }
            if i == start {
                out.push(c);
                i += 1;
            } else {
                let word: String = chars.iter().skip(start).take(i - start).collect();
                if word.contains('@') {
                    out.push_str("[email]");
                } else {
                    out.push_str(&word);
                }
            }
        } else {
            // Strip control characters used for terminal escapes; retain ordinary whitespace.
            if !c.is_control() || c == '\n' || c == '\t' {
                out.push(c);
            }
            i += 1;
        }
    }
    out.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect()
}

fn local_exists(path: &Path) -> Option<bool> {
    let location = path.to_string_lossy();
    if location.starts_with("\\\\") || location.starts_with("//") {
        return None;
    }
    match std::fs::metadata(path) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// A diagnostic snapshot uses existing backend results, not a second detection/network pass.
pub(crate) struct Snapshot {
    pub manifest: crate::manifest::Manifest,
    pub installed: HashMap<String, Installed>,
    pub latest: HashMap<String, Release>,
    pub failures: Vec<Failure>,
    pub jobs: Vec<String>,
    pub detection_complete: bool,
    pub detection_in_progress: bool,
    pub secure_connections_available: bool,
    pub portable: bool,
    pub sandbox: bool,
}

pub(crate) fn collect(paths: &Paths, snapshot: &Snapshot, context: &DiagnosticContext) -> Result<String, String> {
    let manifest = &snapshot.manifest;
    let apps: Vec<Value> = SUITE
        .iter()
        .map(|app| {
            let installed = snapshot.installed.get(app.id);
            let recorded = manifest.apps.get(app.id);
            // Host Flatpak installations aren't files visible in a sandbox. Do not report them missing.
            let exists = installed
                .filter(|i| i.source != artcraft_launcher_core::status::InstallSource::Flatpak)
                .and_then(|i| local_exists(Path::new(&i.location)));
            json!({
                "app": app.id,
                "installed_version": installed.and_then(|i| i.version.as_ref()).map(|v| redact(&v.to_string())),
                "install_source": installed.map(|i| i.source),
                "location": installed.map(|_| "[path]"),
                "local_location_exists": exists,
                "recorded_by_launcher": recorded.is_some(),
                "recorded_location_exists": recorded.and_then(|r| local_exists(&r.path)),
                "recorded_format": recorded.map(|r| r.kind),
                "latest_known_version": snapshot.latest.get(app.id).map(|r| redact(&r.version_label())),
                "job_in_progress": snapshot.jobs.iter().any(|id| id == app.id),
            })
        })
        .collect();
    let failures: Vec<Value> = snapshot
        .failures
        .iter()
        .rev()
        .take(MAX_FAILURES)
        .map(|f| {
            json!({
                "at_unix": f.at, "app": redact(&f.app), "operation": f.operation, "message": redact(&f.message)
            })
        })
        .collect();
    let logs = if context.include_logs { Some(log_excerpts(&paths.logs())) } else { None };
    let document = json!({
        "schema": "artcraft-launcher.diagnostics/v1",
        "generated_at_unix": crate::now(),
        "launcher": {
            "version": env!("CARGO_PKG_VERSION"),
            "build_sha": option_env!("ARTCRAFT_LAUNCHER_BUILD_SHA").map(redact),
            "build_date": option_env!("ARTCRAFT_LAUNCHER_BUILD_DATE").map(redact),
            "portable": snapshot.portable,
            "sandbox": snapshot.sandbox,
            "secure_connections_available": snapshot.secure_connections_available,
        },
        "system": { "os": std::env::consts::OS, "process_architecture": std::env::consts::ARCH, "version": system_version() },
        "update_check": {
            "checking": context.checking, "last_check_unix": context.last_check,
            "source": context.last_source, "rate_limited_until_unix": context.rate_limited_until,
            "automatic_updates": context.auto_update, "prereleases": context.prereleases,
        },
        "interface_language": redact(&context.language),
        "detection": { "completed_once": snapshot.detection_complete, "in_progress": snapshot.detection_in_progress },
        "apps": apps,
        "recent_failures": failures,
        "logs": logs,
        "privacy": { "paths_and_urls_redacted": true, "log_excerpts_included": context.include_logs, "review_before_sharing": true },
    });
    let report = serde_json::to_string_pretty(&document).map_err(|_| "couldn't format the report".to_string())?;
    if report.len() > MAX_REPORT_BYTES {
        return Err("the report exceeds its size limit".into());
    }
    Ok(report)
}

fn read_tail(path: &Path) -> Result<String, std::io::Error> {
    // Do not follow a log symlink into a document or block forever opening a FIFO/device.
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let offset = size.saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(LOG_TAIL_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // The start of a tail might be in the middle of a secret-bearing line. Drop it wholesale.
    Ok(if offset == 0 { text.into_owned() } else { text.split_once('\n').map_or(String::new(), |(_, tail)| tail.to_string()) })
}

fn log_excerpts(dir: &Path) -> Vec<Value> {
    (0..=2)
        .map(|n| {
            let name = if n == 0 { "artcraft-launcher.log".to_string() } else { format!("artcraft-launcher.{n}.log") };
            match read_tail(&dir.join(&name)) {
                Ok(text) => {
                    let mut lines: Vec<String> = text
                        .lines()
                        .rev()
                        .filter(|line| line.contains(" WARN ") || line.contains(" ERROR "))
                        .take(MAX_LOG_LINES)
                        .map(redact)
                        .collect();
                    lines.reverse();
                    json!({"file": name, "status": "available", "warning_error_lines": lines})
                }
                Err(error) => json!({"file": name, "status": "unavailable", "reason": format!("{:?}", error.kind())}),
            }
        })
        .collect()
}

fn system_version() -> Option<String> {
    #[cfg(windows)]
    {
        use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};
        let key = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion").ok()?;
        let build: String = key.get_value("CurrentBuildNumber").ok()?;
        let display: String = key.get_value("DisplayVersion").unwrap_or_default();
        Some(redact(&format!("{display} (build {build})")))
    }
    #[cfg(target_os = "macos")]
    {
        let file = File::open("/System/Library/CoreServices/SystemVersion.plist").ok()?;
        let mut bytes = Vec::new();
        file.take(16 * 1024).read_to_end(&mut bytes).ok()?;
        let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
        Some(redact(value.as_dictionary()?.get("ProductVersion")?.as_string()?))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let file = File::open("/etc/os-release").ok()?;
        let mut text = String::new();
        file.take(16 * 1024).read_to_string(&mut text).ok()?;
        text.lines().find_map(|line| line.strip_prefix("PRETTY_NAME=").map(|s| redact(s.trim_matches('"'))))
    }
    #[cfg(not(any(windows, unix)))]
    None
}

/// Create a fresh file for exactly the preview. No overwrite, shell command, or network access.
pub(crate) fn save(paths: &Paths, report: &str) -> Result<PathBuf, String> {
    if report.len() > MAX_REPORT_BYTES {
        return Err("the report exceeds its size limit".into());
    }
    let parsed: Value = serde_json::from_str(report).map_err(|_| "the report is not valid JSON".to_string())?;
    if parsed.get("schema").and_then(Value::as_str) != Some("artcraft-launcher.diagnostics/v1") {
        return Err("the report has an unknown format".into());
    }
    let dir = paths.data.join("diagnostics");
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create the report folder ({:?})", e.kind()))?;
    let mut file = tempfile::Builder::new()
        .prefix("artcraft-launcher-diagnostics-")
        .suffix(".json")
        .tempfile_in(&dir)
        .map_err(|e| format!("couldn't create the report file ({:?})", e.kind()))?;
    file.write_all(report.as_bytes()).and_then(|_| file.flush()).map_err(|e| format!("couldn't write the report ({:?})", e.kind()))?;
    file.keep().map(|(_, path)| path).map_err(|e| format!("couldn't keep the report ({:?})", e.error.kind()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use artcraft_launcher_core::status::InstallSource;

    fn snapshot() -> Snapshot {
        Snapshot {
            manifest: crate::manifest::Manifest::default(),
            installed: HashMap::new(),
            latest: HashMap::new(),
            failures: Vec::new(),
            jobs: Vec::new(),
            detection_complete: true,
            detection_in_progress: false,
            secure_connections_available: true,
            portable: false,
            sandbox: false,
        }
    }

    #[test]
    fn redacts_secrets_urls_paths_and_emails() {
        for secret in ["Authorization: Bearer abc", "password=hunter2", "https://proxy/?token=abc", "ghp_abcdefgh", "Cookie: session=abc"] {
            assert_eq!(redact(secret), "[omitted: possible credentials]");
        }
        for text in [
            r#"couldn't start "C:\Users\Alice Example\private\photocraft.exe" (error 5)"#,
            "failed /home/alice/private/file.png (error 5)",
            "contact alice@example.org",
            "https://alice:abc@proxy.example/x?q=abc",
            "path=/Users/alice/Documents/private.pcraft",
            r"failed \\server\alice\private.png",
            r"couldn't start C:\Users\Alice Example\private\photocraft.exe (error 5)",
        ] {
            let cleaned = redact(text);
            assert!(!cleaned.contains("alice") && !cleaned.contains("Alice") && !cleaned.contains("private"), "{cleaned}");
        }
        assert_eq!(redact("Windows Installer stopped with error 1618"), "Windows Installer stopped with error 1618");
        assert_eq!(redact("http://[fd00::1234]/private?q=value"), "[url]");
        assert_eq!(redact("GitHub rejected gho_abcdef"), "[omitted: possible credentials]");
        assert_eq!(redact(&"é".repeat(MAX_MESSAGE)), "[omitted: size limit]");
    }

    #[test]
    fn missing_installation_and_failure_are_in_report_without_private_data() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let mut data = snapshot();
        data.installed.insert(
            "photocraft".into(),
            Installed {
                version: None,
                source: InstallSource::Msi,
                location: dir.path().join("Alice Private.exe").to_string_lossy().into(),
                package: None,
            },
        );
        data.failures.push(Failure::new("photocraft", "job", r"couldn't start C:\Users\Alice\private.exe (error 5)"));
        let report = collect(&paths, &data, &DiagnosticContext::default()).unwrap();
        assert!(!report.contains("Alice") && !report.contains(&*dir.path().to_string_lossy()));
        let parsed: Value = serde_json::from_str(&report).unwrap();
        let app = parsed["apps"].as_array().unwrap().iter().find(|app| app["app"] == "photocraft").unwrap();
        assert_eq!(app["local_location_exists"], false);
        assert!(parsed["logs"].is_null());
        assert!(parsed["recent_failures"][0]["message"].as_str().unwrap().contains("error 5"));
        data.installed.get_mut("photocraft").unwrap().source = InstallSource::Flatpak;
        let parsed: Value = serde_json::from_str(&collect(&paths, &data, &DiagnosticContext::default()).unwrap()).unwrap();
        assert!(parsed["apps"].as_array().unwrap().iter().find(|a| a["app"] == "photocraft").unwrap()["local_location_exists"].is_null());
    }

    #[test]
    fn logs_are_opt_in_bounded_and_skip_partial_secret_lines() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.logs()).unwrap();
        let text = format!(
            "password={} ERROR leaked-fragment\n{}\n[time ERROR app] permission denied\n[time WARN app] token=do-not-export\n",
            "a".repeat(40_000),
            "[time INFO app] ok\n".repeat(100)
        );
        std::fs::write(paths.logs().join("artcraft-launcher.log"), text).unwrap();
        let report = collect(&paths, &snapshot(), &DiagnosticContext { include_logs: true, ..Default::default() }).unwrap();
        assert!(!report.contains("leaked-fragment") && !report.contains("do-not-export"));
        assert!(report.contains("permission denied") && report.contains("possible credentials"));
        assert!(report.contains("unavailable"));
        let mut invalid = b"[time ERROR app] invalid ".to_vec();
        invalid.push(0xff);
        std::fs::write(paths.logs().join("artcraft-launcher.log"), invalid).unwrap();
        assert!(collect(&paths, &snapshot(), &DiagnosticContext { include_logs: true, ..Default::default() }).is_ok());
        let huge = "[time WARN app] warning\n".repeat(10_000);
        std::fs::write(paths.logs().join("artcraft-launcher.log"), huge).unwrap();
        let parsed: Value =
            serde_json::from_str(&collect(&paths, &snapshot(), &DiagnosticContext { include_logs: true, ..Default::default() }).unwrap()).unwrap();
        assert_eq!(parsed["logs"][0]["warning_error_lines"].as_array().unwrap().len(), MAX_LOG_LINES);
        assert_eq!(local_exists(Path::new(r"\\server\private\app.exe")), None);
    }

    #[test]
    fn saves_exact_preview_without_overwriting_and_reports_write_failures() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let report = collect(&paths, &snapshot(), &DiagnosticContext::default()).unwrap();
        let first = save(&paths, &report).unwrap();
        let second = save(&paths, &report).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(first).unwrap(), report);
        assert!(save(&paths, "not JSON").is_err());
        let blocked = Paths::under(&dir.path().join("blocked"));
        std::fs::create_dir_all(&blocked.data).unwrap();
        std::fs::write(blocked.data.join("diagnostics"), "user file").unwrap();
        assert!(save(&blocked, &report).is_err());
        assert_eq!(std::fs::read_to_string(blocked.data.join("diagnostics")).unwrap(), "user file");
    }
}
