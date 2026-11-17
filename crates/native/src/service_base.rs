//! Which deployment base URLs the launcher may use for HTTPS traffic.

/// Plain `http://` is only allowed on loopback, with a numeric port.
pub fn loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]") && !port.is_empty() && port.parse::<u16>().is_ok()
}

/// A CollabCraft deployment base: `https://`, or loopback `http://`, or empty (none configured).
pub fn allowed_service_base(base: &str) -> bool {
    base.is_empty() || base.starts_with("https://") || loopback_http(base)
}
