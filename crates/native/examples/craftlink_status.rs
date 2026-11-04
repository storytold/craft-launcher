//! Read-only integration probe; does not start the launcher's singleton shell.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#[cfg(feature = "craftlink-status")]
fn main() -> Result<(), String> {
    let args = std::env::args().collect::<Vec<_>>();
    let port = args.get(1).ok_or("usage: craftlink_status PORT CREDENTIAL_FILE")?.parse::<u16>().map_err(|_| "invalid port")?;
    let file = args.get(2).ok_or("credential file required")?;
    let bindings = artcraft_launcher_native::craftlink::inspect(port, std::path::Path::new(file))?;
    println!("{}", serde_json::to_string(&bindings).map_err(|_| "invalid binding result")?);
    Ok(())
}
#[cfg(not(feature = "craftlink-status"))]
fn main() {
    eprintln!("enable the craftlink-status feature");
}
