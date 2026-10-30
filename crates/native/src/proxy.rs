//! Which HTTP proxy the launcher uses: the proxy environment variables first, else the proxy set
//! in the OS (Windows Internet Settings, macOS network settings). Automatic configuration (PAC
//! scripts, WPAD) is not supported.

use ureq::{Proxy, ProxyProtocol};

/// A manually configured proxy read from the OS settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemProxy {
    pub host: String,
    pub port: Option<u16>,
    /// Hosts that skip the proxy, in `NO_PROXY` form (`example.com`, `*.example.com`, `10.*`).
    pub bypass: Vec<String>,
    /// Where it came from, for the log.
    pub source: &'static str,
}

/// The proxy every agent uses, or `None` to connect directly. Logs the choice at info level,
/// without any user name or password.
pub fn pick() -> Option<Proxy> {
    if let Some(proxy) = Proxy::try_from_env() {
        let mut builder = Proxy::builder(proxy.protocol()).host(proxy.host()).port(proxy.port());
        if let Some(user) = proxy.username() {
            builder = builder.username(user);
        }
        if let Some(pass) = proxy.password() {
            builder = builder.password(pass);
        }
        if let Ok(no_proxy_env) = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) {
            for entry in no_proxy_env.split(',') {
                let entry = entry.trim();
                if !entry.is_empty() {
                    builder = builder.no_proxy(entry);
                }
            }
        }
        for entry in ["localhost", "127.0.0.1", "[::1]"] {
            builder = builder.no_proxy(entry);
        }
        if let Ok(p) = builder.build() {
            log::info!("HTTP proxy: {} {}:{} (from environment variables)", p.protocol(), p.host(), p.port());
            return Some(p);
        }
        log::info!("HTTP proxy: {} {}:{} (from environment variables)", proxy.protocol(), proxy.host(), proxy.port());
        return Some(proxy);
    }
    let system = system()?;
    match to_proxy(&system) {
        Ok(proxy) => {
            log::info!("HTTP proxy: {} {}:{} (from {})", proxy.protocol(), proxy.host(), proxy.port(), system.source);
            Some(proxy)
        }
        Err(e) => {
            log::warn!("ignoring the proxy in {} ({e}); connecting directly", system.source);
            None
        }
    }
}

/// A ureq proxy for `system`. Loopback addresses always go direct, as they do on Windows and
/// macOS.
pub fn to_proxy(system: &SystemProxy) -> Result<Proxy, ureq::Error> {
    let mut builder = Proxy::builder(ProxyProtocol::Http).host(&system.host);
    if let Some(port) = system.port {
        builder = builder.port(port);
    }
    for entry in ["localhost", "127.0.0.1", "[::1]"].iter().copied().chain(system.bypass.iter().map(String::as_str)) {
        builder = builder.no_proxy(entry);
    }
    builder.build()
}

#[cfg(windows)]
fn system() -> Option<SystemProxy> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings").ok()?;
    let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
    if enabled == 0 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    let overrides: String = key.get_value("ProxyOverride").unwrap_or_default();
    let (host, port) = windows_proxy_server(&server)?;
    Some(SystemProxy { host, port, bypass: windows_bypass(&overrides), source: "Windows proxy settings" })
}

#[cfg(target_os = "macos")]
fn system() -> Option<SystemProxy> {
    let out = crate::process::capture("/usr/sbin/scutil", &["--proxy"], std::time::Duration::from_secs(3))?;
    parse_scutil(&out)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn system() -> Option<SystemProxy> {
    // Linux and the BSDs: desktop proxy settings reach programs through the environment.
    None
}

/// Parses Windows' `ProxyServer` value: either `host:port` for every protocol, or a list such as
/// `http=h:p;https=h:p;socks=h:p`. Prefers the HTTPS entry (every request is HTTPS), then HTTP.
/// SOCKS-only settings are ignored.
pub fn windows_proxy_server(value: &str) -> Option<(String, Option<u16>)> {
    let entries: Vec<&str> = value.split([';', ' ']).map(str::trim).filter(|e| !e.is_empty()).collect();
    let chosen = if entries.iter().any(|e| e.contains('=')) {
        let find = |scheme: &str| {
            entries.iter().find_map(|e| e.split_once('=').filter(|(k, _)| k.trim().eq_ignore_ascii_case(scheme)).map(|(_, v)| v.trim()))
        };
        find("https").or_else(|| find("http"))?
    } else {
        entries.first().copied()?
    };
    host_port(chosen)
}

/// Splits `host:port`, `http://host:port/`, `[::1]:8080` and similar into host and port.
fn host_port(value: &str) -> Option<(String, Option<u16>)> {
    let rest = match value.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some(_) => return None,
        None => value,
    };
    let rest = rest.trim_end_matches('/');
    // Credentials never come from these settings; refuse rather than mis-parse.
    if rest.is_empty() || rest.contains(['@', '/', '?', '#']) {
        return None;
    }
    if rest.starts_with('[') {
        let (host, after) = rest.split_once(']')?;
        let host = format!("{host}]");
        return match after.strip_prefix(':') {
            Some(port) => Some((host, Some(port.parse().ok()?))),
            None if after.is_empty() => Some((host, None)),
            None => None,
        };
    }
    match rest.split_once(':') {
        Some((host, port)) if !host.is_empty() && !port.contains(':') => Some((host.to_string(), Some(port.parse().ok()?))),
        Some(_) => None,
        None => Some((rest.to_string(), None)),
    }
}

/// Parses Windows' `ProxyOverride` bypass list (`*.local;10.*;<local>`). `<local>` (plain host
/// names without a dot) has no `NO_PROXY` equivalent and is dropped.
pub fn windows_bypass(value: &str) -> Vec<String> {
    value.split([';', ' ']).map(str::trim).filter(|e| !e.is_empty() && !e.eq_ignore_ascii_case("<local>")).map(str::to_string).collect()
}

/// Parses `scutil --proxy` output: the HTTPS proxy when enabled, else the HTTP proxy, plus the
/// exceptions list.
pub fn parse_scutil(out: &str) -> Option<SystemProxy> {
    let mut fields: Vec<(&str, &str)> = Vec::new();
    let mut bypass = Vec::new();
    let mut in_exceptions = false;
    for line in out.lines().map(str::trim) {
        if in_exceptions {
            if line.starts_with('}') {
                in_exceptions = false;
            } else if let Some((_, v)) = line.split_once(" : ") {
                let v = v.trim();
                if !v.is_empty() {
                    bypass.push(v.to_string());
                }
            }
            continue;
        }
        let Some((k, v)) = line.split_once(" : ") else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k == "ExceptionsList" && v.starts_with("<array>") {
            in_exceptions = !v.ends_with('}');
        } else {
            fields.push((k, v));
        }
    }
    let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    ["HTTPS", "HTTP"].into_iter().find_map(|kind| {
        if get(&format!("{kind}Enable")) != Some("1") {
            return None;
        }
        let host = get(&format!("{kind}Proxy")).filter(|h| !h.is_empty() && !h.contains(['@', '/', ' ']))?;
        let port = match get(&format!("{kind}Port")) {
            Some(p) => Some(p.parse().ok()?),
            None => None,
        };
        Some(SystemProxy { host: host.to_string(), port, bypass: bypass.clone(), source: "macOS proxy settings" })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hp(host: &str, port: Option<u16>) -> Option<(String, Option<u16>)> {
        Some((host.to_string(), port))
    }

    #[test]
    fn windows_single_proxy_for_everything() {
        assert_eq!(windows_proxy_server("proxy.corp:8080"), hp("proxy.corp", Some(8080)));
        assert_eq!(windows_proxy_server("127.0.0.1:7890"), hp("127.0.0.1", Some(7890)));
        assert_eq!(windows_proxy_server("proxy.corp"), hp("proxy.corp", None));
        assert_eq!(windows_proxy_server("http://proxy.corp:3128/"), hp("proxy.corp", Some(3128)));
        assert_eq!(windows_proxy_server("[::1]:8080"), hp("[::1]", Some(8080)));
    }

    #[test]
    fn windows_per_protocol_list_prefers_https() {
        assert_eq!(windows_proxy_server("http=h1:80;https=h2:443;ftp=h3:21;socks=h4:1080"), hp("h2", Some(443)));
        assert_eq!(windows_proxy_server("ftp=h3:21;http=h1:8080"), hp("h1", Some(8080)));
        assert_eq!(windows_proxy_server("HTTPS=h2:8443 ; "), hp("h2", Some(8443)));
    }

    #[test]
    fn windows_settings_that_cannot_be_used() {
        assert_eq!(windows_proxy_server(""), None);
        assert_eq!(windows_proxy_server("socks=h4:1080"), None);
        assert_eq!(windows_proxy_server("socks5://h4:1080"), None);
        assert_eq!(windows_proxy_server("proxy.corp:notaport"), None);
        assert_eq!(windows_proxy_server("proxy.corp:99999"), None);
        assert_eq!(windows_proxy_server("user:pw@proxy.corp:8080"), None);
    }

    #[test]
    fn windows_bypass_list() {
        assert_eq!(windows_bypass("*.local;10.*; localhost;<local>"), ["*.local", "10.*", "localhost"]);
        assert!(windows_bypass("").is_empty());
    }

    const SCUTIL: &str = "<dictionary> {
  ExceptionsList : <array> {
    0 : *.local
    1 : 169.254/16
  }
  FTPPassive : 1
  HTTPEnable : 1
  HTTPPort : 8080
  HTTPProxy : http.corp
  HTTPSEnable : 1
  HTTPSPort : 3128
  HTTPSProxy : https.corp
  SOCKSEnable : 0
}
";

    #[test]
    fn scutil_https_proxy_with_exceptions() {
        let p = parse_scutil(SCUTIL).unwrap();
        assert_eq!((p.host.as_str(), p.port), ("https.corp", Some(3128)));
        assert_eq!(p.bypass, ["*.local", "169.254/16"]);
    }

    #[test]
    fn scutil_falls_back_to_http_proxy() {
        let out = SCUTIL.replace("HTTPSEnable : 1", "HTTPSEnable : 0");
        let p = parse_scutil(&out).unwrap();
        assert_eq!((p.host.as_str(), p.port), ("http.corp", Some(8080)));
    }

    #[test]
    fn scutil_without_a_proxy() {
        let out = "<dictionary> {\n  ExceptionsList : <array> {\n    0 : *.local\n  }\n  FTPPassive : 1\n  HTTPEnable : 0\n  HTTPSEnable : 0\n}\n";
        assert_eq!(parse_scutil(out), None);
        assert_eq!(parse_scutil(""), None);
        assert_eq!(parse_scutil("HTTPSEnable : 1\nHTTPSProxy : \nHTTPSPort : 1\n"), None);
    }

    #[test]
    fn system_proxy_becomes_a_ureq_proxy_that_skips_loopback() {
        let p = to_proxy(&SystemProxy { host: "proxy.corp".into(), port: Some(8080), bypass: vec!["*.local".into()], source: "test" }).unwrap();
        assert_eq!((p.protocol(), p.host(), p.port()), (ProxyProtocol::Http, "proxy.corp", 8080));
        for (url, direct) in
            [("http://127.0.0.1:1/", true), ("http://localhost/", true), ("https://nas.local/", true), ("https://github.com/", false)]
        {
            assert_eq!(p.is_no_proxy(&url.parse().unwrap()), direct, "{url}");
        }
        let p = to_proxy(&SystemProxy { host: "proxy.corp".into(), port: None, bypass: vec![], source: "test" }).unwrap();
        assert_eq!(p.port(), 80);
    }
}
