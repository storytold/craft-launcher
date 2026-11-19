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

/// A suite deployment base: `https://`, or loopback `http://`, or empty (none configured).
pub fn allowed_service_base(base: &str) -> bool {
    base.is_empty() || base.starts_with("https://") || loopback_http(base)
}

/// Refuses to let a stored account present its token to an address it did not earn honestly.
///
/// `account.service` comes straight from `accounts.json`, a plain file. Nothing upstream of this
/// point validates it: `craft_suite_account::accounts::read` checks only `format`, and
/// `craft_suite_account::migrate::adopt` takes `service` from another app's old session file as
/// written. Anything that can rewrite that 0600 file can therefore point a stored row at a host of
/// its choosing and collect the bearer token the next time this account refreshes or signs out.
/// This is the one gate between a stored row and the token leaving the process: every caller about
/// to hand a token to `Service::identity` or `Service::revoke` must pass it first.
///
/// Two independent checks, both required: the address itself has to be one the launcher would ever
/// dial ([`allowed_service_base`], the same rule `Accounts::set_base` applies to whatever somebody
/// typed into Settings), and the row's `id` has to still be the handle its own `service` and
/// `user` hash to. The second check is what catches a row whose `service` was edited in place
/// while its `id` was left alone: the address changes, but nothing re-derives the handle, so a
/// naive address check alone would wave it through.
pub fn validate_account_target(account: &craft_suite_account::Account) -> Result<(), String> {
    if account.service.is_empty() || !allowed_service_base(&account.service) {
        return Err("the stored account's service address is not one this launcher may dial".to_owned());
    }
    if craft_suite_account::accounts::handle(&account.service, &account.user) != account.id {
        return Err("the stored account's service or user no longer matches its handle".to_owned());
    }
    Ok(())
}
