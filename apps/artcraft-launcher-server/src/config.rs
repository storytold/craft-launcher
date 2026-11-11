use std::fs::{self, File};
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use openidconnect::IssuerUrl;
use serde::Deserialize;
use url::Url;

use crate::artifact_paths::is_link;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_SECRET_BYTES: u64 = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    enabled: bool,
    server: Option<Settings>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) listen: SocketAddr,
    pub(crate) origin: Url,
    pub(crate) launcher_root: PathBuf,
    pub(crate) data_root: PathBuf,
    pub(crate) artifact_root: PathBuf,
    pub(crate) worker_token_file: PathBuf,
    pub(crate) auth: AuthSettings,
    pub(crate) per_user_bytes: u64,
    pub(crate) total_file_bytes: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthSettings {
    pub(crate) issuer: IssuerUrl,
    pub(crate) client_id: String,
    pub(crate) client_secret_file: PathBuf,
    #[serde(default)]
    pub(crate) allowed_subjects: Vec<String>,
    #[serde(default)]
    pub(crate) admin_subjects: Vec<String>,
    #[serde(default = "default_idle_minutes")]
    idle_minutes: u32,
}

fn default_idle_minutes() -> u32 {
    60
}

impl AuthSettings {
    pub(crate) fn idle_timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.idle_minutes) * 60)
    }

    pub(crate) fn allows(&self, subject: &str) -> bool {
        self.allowed_subjects.iter().any(|id| id == subject)
    }

    pub(crate) fn is_admin(&self, subject: &str) -> bool {
        self.admin_subjects.iter().any(|id| id == subject)
    }
}

pub(crate) fn load(path: &Path) -> Result<Option<Settings>, String> {
    let config = path.canonicalize().map_err(|error| format!("Cannot resolve the server configuration: {error}"))?;
    let input = read_limited(&config, MAX_CONFIG_BYTES)?;
    let file: ConfigFile = serde_json::from_slice(&input).map_err(|error| format!("Invalid configuration: {error}"))?;
    if !file.enabled {
        return Ok(None);
    }
    let base = config.parent().ok_or_else(|| String::from("Configuration has no parent directory"))?;
    let mut settings = file.server.ok_or_else(|| String::from("The enabled configuration requires a server object"))?;
    settings.resolve(base)?;
    Ok(Some(settings))
}

impl Settings {
    fn resolve(&mut self, base: &Path) -> Result<(), String> {
        if !self.listen.ip().is_loopback() || self.listen.port() == 0 {
            return Err(String::from("Listen on a fixed loopback port behind HTTPS"));
        }
        if !https_url(&self.origin) || self.origin.path() != "/" {
            return Err(String::from("Origin must be a bare HTTPS origin"));
        }
        self.launcher_root = directory(base, &self.launcher_root)?;
        self.data_root = directory(base, &self.data_root)?;
        self.artifact_root = directory(base, &self.artifact_root)?;
        validate_separate_roots([&self.launcher_root, &self.data_root, &self.artifact_root])?;
        private_path(&self.data_root)?;
        private_path(&self.artifact_root)?;
        self.auth.client_secret_file = secret_path(base, &self.auth.client_secret_file, self)?;
        self.worker_token_file = secret_path(base, &self.worker_token_file, self)?;
        if self.worker_token_file == self.auth.client_secret_file {
            return Err(String::from("Use separate secrets for OIDC and the PC update worker"));
        }
        validate_launcher_site(&self.launcher_root)?;
        if !https_url(self.auth.issuer.url())
            || !bounded_text(self.auth.issuer.as_str(), 2048)
            || self.auth.issuer.chars().any(char::is_whitespace)
            || !bounded_text(&self.auth.client_id, 512)
            || !(5..=60).contains(&self.auth.idle_minutes)
        {
            return Err(String::from("Invalid OIDC issuer, client ID, or idle timeout"));
        }
        validate_subjects(&self.auth)?;
        if self.per_user_bytes < 80 * 1024 * 1024
            || self.per_user_bytes > self.total_file_bytes
            || self.total_file_bytes > 4 * 1024 * 1024 * 1024 * 1024
        {
            return Err(String::from("Invalid per-user or total file quota"));
        }
        Ok(())
    }
}

fn validate_launcher_site(root: &Path) -> Result<(), String> {
    const MAX_ENTRIES: usize = 4096;
    const MAX_DEPTH: usize = 12;

    if !root.join("index.html").is_file() {
        return Err(String::from("The hosted launcher root requires an index.html"));
    }
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut count = 0;
    while let Some((directory, depth)) = pending.pop() {
        let entries = fs::read_dir(directory).map_err(|error| format!("Cannot read the launcher site: {error}"))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("Cannot read a launcher asset: {error}"))?;
            count += 1;
            if count > MAX_ENTRIES || depth >= MAX_DEPTH {
                return Err(String::from("The launcher site exceeds its inventory limit"));
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(|error| format!("Cannot inspect a launcher asset: {error}"))?;
            if is_link(&metadata) {
                return Err(String::from("The launcher site must contain no links"));
            }
            if metadata.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if !metadata.is_file() {
                return Err(String::from("The launcher site must contain only regular files"));
            }
        }
    }
    Ok(())
}

fn secret_path(base: &Path, path: &Path, settings: &Settings) -> Result<PathBuf, String> {
    let path = base.join(path).canonicalize().map_err(|error| format!("Cannot resolve a configured secret file: {error}"))?;
    if path.starts_with(&settings.launcher_root) || path.starts_with(&settings.artifact_root) {
        return Err(String::from("Secret files must be outside served roots"));
    }
    Ok(path)
}

fn validate_subjects(auth: &AuthSettings) -> Result<(), String> {
    if auth.allowed_subjects.is_empty()
        || auth.allowed_subjects.len() > 1024
        || auth.admin_subjects.len() > 128
        || auth.allowed_subjects.iter().chain(&auth.admin_subjects).any(|id| !bounded_text(id, 255))
        || auth.admin_subjects.iter().any(|id| !auth.allows(id))
    {
        return Err(String::from("Invalid OIDC subject allowlists"));
    }
    Ok(())
}

fn validate_separate_roots(roots: [&Path; 3]) -> Result<(), String> {
    for (i, root) in roots.iter().enumerate() {
        if roots.iter().skip(i + 1).any(|other| root.starts_with(other) || other.starts_with(root)) {
            return Err(String::from("Launcher, data and artifact roots must not overlap"));
        }
    }
    Ok(())
}

fn directory(base: &Path, path: &Path) -> Result<PathBuf, String> {
    let path = base.join(path).canonicalize().map_err(|error| format!("Cannot resolve a configured directory: {error}"))?;
    if !path.is_dir() {
        return Err(String::from("A configured root is not a directory"));
    }
    Ok(path)
}

pub(crate) fn https_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

pub(crate) fn bounded_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

pub(crate) fn read_secret(path: &Path) -> Result<String, String> {
    private_path(path)?;
    let raw = read_limited(path, MAX_SECRET_BYTES)?;
    let value = String::from_utf8(raw).map_err(|_| String::from("The secret must be UTF-8"))?;
    let trimmed = value.trim_end_matches(['\r', '\n']);
    if !bounded_text(trimmed, MAX_SECRET_BYTES as usize) {
        return Err(String::from("The secret file is empty or malformed"));
    }
    Ok(trimmed.to_owned())
}

fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    if !fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return Err(String::from("The configuration or secret is not a regular file"));
    }
    let file = File::open(path).map_err(|error| format!("Cannot open a configured file: {error}"))?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes).map_err(|error| format!("Cannot read a configured file: {error}"))?;
    if bytes.len() as u64 > max {
        return Err(String::from("The configuration or secret exceeds its size limit"));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn private_path(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = fs::metadata(path).map_err(|error| format!("Cannot inspect permissions: {error}"))?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(String::from("Private paths must be inaccessible to group and others"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn private_path(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AuthSettings, ConfigFile, https_url, validate_separate_roots};
    use std::path::Path;
    use url::Url;

    #[test]
    fn configuration_is_disabled_by_default() {
        let config: ConfigFile = serde_json::from_str("{}").expect("empty configuration");
        assert!(!config.enabled);
        assert!(config.server.is_none());
        assert!(serde_json::from_str::<ConfigFile>(r#"{"enabld":true}"#).is_err());
    }

    #[test]
    fn rejects_overlapping_roots() {
        let root = Path::new("public");
        let data = Path::new("public/hidden");
        let artifacts = Path::new("artifacts");
        assert!(validate_separate_roots([root, data, artifacts]).is_err());
    }

    #[test]
    fn identity_configuration_preserves_the_exact_issuer() {
        for issuer in ["https://issuer.example", "https://issuer.example/", "https://issuer.example/tenant"] {
            let settings: AuthSettings = serde_json::from_value(serde_json::json!({
                "issuer": issuer,
                "client_id": "launcher",
                "client_secret_file": "oidc-secret",
            }))
            .expect("test identity settings");
            assert_eq!(settings.issuer.as_str(), issuer);
            assert!(https_url(settings.issuer.url()));
        }
    }

    #[test]
    fn identity_configuration_requires_https_without_credentials() {
        for value in ["http://example.com", "https://user@example.com", "https://example.com/#x"] {
            assert!(!https_url(&Url::parse(value).expect("test URL")));
        }
        assert!(https_url(&Url::parse("https://example.com/identity").expect("test URL")));
    }
}
