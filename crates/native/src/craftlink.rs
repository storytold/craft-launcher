//! Explicit read-only CraftLink v1 client. No discovery, launch or install.
use artcraft_launcher_core::craftlink::Binding;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};
const REJECTED: &str = "CraftLink request was rejected";
const UNAVAILABLE: &str = "CraftLink is unavailable";
const LIMIT: usize = 1024 * 1024;

/// Inspect only `bindings` on loopback after an explicit user request. Credential
/// contents and peer error bodies never enter returned summaries or errors.
pub fn inspect(port: u16, credential_file: &Path) -> Result<Vec<Binding>, String> {
    if port == 0 {
        return Err(UNAVAILABLE.into());
    }
    let meta = std::fs::metadata(credential_file).map_err(|_| UNAVAILABLE.to_string())?;
    if !meta.is_file() || meta.len() > 4096 {
        return Err(REJECTED.into());
    }
    let mut credential = String::new();
    std::fs::File::open(credential_file)
        .map_err(|_| UNAVAILABLE.to_string())?
        .take(4097)
        .read_to_string(&mut credential)
        .map_err(|_| REJECTED.to_string())?;
    let credential = credential.trim();
    if !(32..=4096).contains(&credential.len()) {
        return Err(REJECTED.into());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = TcpStream::connect_timeout(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(1))
        .map_err(|_| UNAVAILABLE.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(1))).map_err(|_| UNAVAILABLE.to_string())?;
    stream.write_all(format!("{}\n", json!({"token":credential,"method":"bindings","params":{}})).as_bytes()).map_err(|_| UNAVAILABLE.to_string())?;
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(UNAVAILABLE.into());
        }
        stream.set_read_timeout(Some(remaining)).map_err(|_| UNAVAILABLE.to_string())?;
        let count = stream.read(&mut buffer).map_err(|_| UNAVAILABLE.to_string())?;
        if count == 0 {
            return Err(UNAVAILABLE.into());
        }
        bytes.extend_from_slice(buffer.get(..count).ok_or(UNAVAILABLE)?);
        if bytes.len() > LIMIT {
            return Err(REJECTED.into());
        }
        if bytes.contains(&b'\n') {
            break;
        }
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| REJECTED.to_string())?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(REJECTED.into());
    }
    let rows = value.get("result").and_then(Value::as_array).filter(|rows| rows.len() <= 4096).ok_or(REJECTED)?;
    let mut bindings = Vec::new();
    for row in rows {
        if row.get("phase").and_then(Value::as_str) == Some("creating") {
            continue;
        }
        let binding: Binding = serde_json::from_value(row.clone()).map_err(|_| REJECTED.to_string())?;
        if binding.profile.len() > 128
            || binding.object.len() != 36
            || !binding.object.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
            || binding.status.len() > 64
        {
            return Err(REJECTED.into());
        }
        bindings.push(binding);
    }
    Ok(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    fn credential() -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("test.credential");
        std::fs::write(&file, "test-credential-for-loopback-fixture-only").unwrap();
        (root, file)
    }
    fn server(reply: Value) -> (u16, std::thread::JoinHandle<Value>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
            writeln!(stream, "{reply}").unwrap();
            serde_json::from_str(&line).unwrap()
        });
        (port, worker)
    }
    #[test]
    fn craftlink_reads_only_binding_summaries() {
        let (_root, file) = credential();
        let (port, worker) = server(
            json!({"ok":true,"result":[{"object":"00000000-0000-4000-8000-000000000001","profile":"figcraft","applied":7,"status":"current","token_file":"not copied"}]}),
        );
        let rows = inspect(port, &file).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].applied, 7);
        let request = worker.join().unwrap();
        assert_eq!(request["method"], "bindings");
        assert_eq!(request["params"], json!({}));
    }
    #[test]
    fn craftlink_rejections_never_echo_peer_or_credential() {
        let (_root, file) = credential();
        let (port, worker) = server(json!({"ok":false,"error":"test-credential-for-loopback-fixture-only"}));
        let error = inspect(port, &file).unwrap_err();
        assert_eq!(error, REJECTED);
        assert!(!error.contains("fixture-only"));
        worker.join().unwrap();
        assert!(inspect(0, &file).is_err());
        assert!(inspect(1, Path::new("missing-credential-fixture")).is_err());
    }
    #[test]
    fn craftlink_rejects_oversized_or_malformed_response() {
        let (_root, file) = credential();
        let (port, worker) = server(json!({"ok":true,"result":"x".repeat(LIMIT)}));
        assert!(inspect(port, &file).is_err());
        let _ = worker.join();
        let (port, worker) = server(json!({"ok":true,"result":[{"profile":"figcraft"}]}));
        assert!(inspect(port, &file).is_err());
        worker.join().unwrap();
    }
}
