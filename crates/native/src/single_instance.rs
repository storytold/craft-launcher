//! One launcher at a time: a second start asks the first to show its window, then exits.
//!
//! The first instance listens on a fixed loopback port. A second start says hello with its
//! version, and the first answers with its own:
//!
//! - the newcomer is the same version or older: the running one shows its window and the newcomer
//!   exits, as before;
//! - the newcomer is newer: the running one quits (unless it's in the middle of installing
//!   something; then it shows itself) and the newcomer takes over. Otherwise starting a new
//!   version while an old one sat in the tray brought back the old window, which still offered
//!   whatever update it had found back then (issue #29: 0.1.2 showed "0.1.1 is available");
//! - the running one is from before this handshake (0.1.3 or older) and closes without an
//!   answer: it can't be asked to quit, so the newcomer starts anyway.
//!
//! Launchers from before the handshake send a bare `show`, which is still understood. The worst
//! another local program can do with any of this is bring the window forward or make the launcher
//! quit.
//!
//! ## Upgrading over the published 0.7.0 build
//!
//! Replacement of a running 0.7.0 instance remains an unqualified compatibility gap. The
//! published 0.7.0 `answer` expects `hello <version> <token>` and grants `Replace` only when the
//! second word equals its owner-only token file. A newer build sends `hello <version> <nonce>`
//! instead, so 0.7.0 reads the nonce as a wrong token, replies `running 0.7.0`, and merely shows
//! its window. If that reply arrives in time, a newer newcomer requests takeover but the old
//! process does not release its port; a same-version newcomer defers. The old reader and new
//! client have similar timeouts, so a delayed reply can instead reach the legacy fallback and
//! leave another process running without the listening socket. These timing paths need an
//! old-release interoperability test. Quit the running 0.7.0 launcher from its tray menu before
//! starting the new version. Do not make the new side
//! send the token to force a `Replace`: that would restore the clear-text shared-secret handshake
//! this protocol removed (0.7.0 itself already puts the token on the wire when it starts). A
//! future compatibility shim would need an out-of-band, authenticated way to stop the old process;
//! it must not accept an unauthenticated lifecycle request.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use artcraft_launcher_core::version::{self, Version};

const PORT: u16 = 47_413;
/// What launchers before the version handshake send (and still the answer to an unknown peer).
const SHOW: &[u8] = b"artcraft-launcher:show";
/// `hello <version> <nonce>`: a second start introduces itself with its version and client nonce.
const HELLO: &str = "artcraft-launcher:hello ";
/// `challenge <nonce> <proof>`: running instance challenges newcomer to prove knowledge of token.
const CHALLENGE: &str = "artcraft-launcher:challenge ";
/// `auth <proof>`: newcomer's response proving possession of token.
const AUTH: &str = "artcraft-launcher:auth ";
/// `running <version>`: the first instance's answer.
const RUNNING: &str = "artcraft-launcher:running ";
/// How long a newer start waits for the old one to quit and free the port.
const TAKE_OVER_WITHIN: Duration = Duration::from_secs(10);
/// How large a greeting or challenge-response line may be.
const MAX_GREETING: usize = 256;

/// A per-user secret that proves a hello comes from this OS account.
///
/// The fixed loopback port is reachable by any local process, so a `hello` alone can't be trusted
/// to decide the launcher's lifecycle: another local user could send a newer version and make an
/// idle launcher quit (CWE-862). The token lives in the user's own data folder, readable only by
/// this account. Mutual challenge-response uses this secret without ever sending it across the wire
/// (CWE-522, CWE-287).
fn token() -> Option<String> {
    use std::io::Read;
    let dir =
        dirs::data_local_dir()?.join(if cfg!(any(target_os = "macos", target_os = "windows")) { "ArtCraft Launcher" } else { "artcraft-launcher" });
    let path = dir.join(".instance-token");
    if let Ok(mut f) = std::fs::File::open(&path) {
        let mut s = String::new();
        if f.read_to_string(&mut s).is_ok() {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    // Create it, owner-only, using the OS CSPRNG.
    let _ = std::fs::create_dir_all(&dir);
    let secret = fresh_secret()?;
    write_private(&path, secret.as_bytes()).ok()?;
    Some(secret)
}

/// A 256-bit hex secret filled directly from the OS CSPRNG.
fn fresh_secret() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Writes `bytes` to `path` readable and writable only by the owner.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        // On Windows the per-user profile directory is already access-controlled to the user.
        let mut f = std::fs::File::create(path)?;
        f.write_all(bytes)
    }
}

/// What another start asks of the running launcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Bring the window forward.
    Show,
    /// A newer launcher is starting: quit so it can take over (or show the window when quitting
    /// now would cut short an install).
    Replace,
}

pub enum Instance {
    /// We're the only one. A request arrives on the receiver whenever another start asks
    /// something; `wake` is called to repaint the (perhaps hidden) window.
    Primary(Receiver<Request>),
    /// Another launcher is running and has been asked to show itself.
    Secondary,
}

/// How the running launcher answered our hello.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    /// It's as new as we are, or newer, and shows its window: we exit.
    Defer,
    /// It's older and quits: we take over the port.
    TakeOver,
    /// It closed without an answer: a launcher from before the handshake.
    Legacy,
}

type Wake = std::sync::Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>>;

/// This launcher's own version.
fn ours() -> Option<Version> {
    version::parse(env!("CARGO_PKG_VERSION"))
}

/// Claims the instance; `wake` is set later (the UI context doesn't exist yet).
pub fn claim(wake: Wake) -> Instance {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, PORT));
    let ours = ours();
    let token = token();
    if let Ok(listener) = TcpListener::bind(addr) {
        return Instance::Primary(listen(listener, ours, token, wake));
    }
    match ask(addr, ours.as_ref(), token.as_deref()) {
        Ok(Answer::Defer) => Instance::Secondary,
        Ok(Answer::TakeOver) => {
            let deadline = Instant::now() + TAKE_OVER_WITHIN;
            while Instant::now() < deadline {
                if let Ok(listener) = TcpListener::bind(addr) {
                    return Instance::Primary(listen(listener, ours, token, wake));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // It stayed (busy installing) and showed its window instead.
            Instance::Secondary
        }
        Ok(Answer::Legacy) => {
            log::warn!("an older ArtCraft Launcher is running and can't be asked to quit; starting anyway");
            Instance::Primary(channel().1)
        }
        // The port is taken by something else: run anyway rather than refuse to start.
        Err(_) => Instance::Primary(channel().1),
    }
}

fn compute_proof(role: &str, token: &str, client_nonce: &str, server_nonce: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"artcraft-launcher:auth:v1:");
    hasher.update(role.as_bytes());
    hasher.update(b":");
    hasher.update(token.as_bytes());
    hasher.update(b":");
    hasher.update(client_nonce.as_bytes());
    hasher.update(b":");
    hasher.update(server_nonce.as_bytes());
    let hash = hasher.finalize();
    format!("{hash:x}")
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.as_bytes().iter().zip(b.as_bytes()) {
        diff |= x ^ y;
    }
    core::hint::black_box(diff) == 0
}

/// Answers other starts on `listener` until the receiver is dropped.
fn listen(listener: TcpListener, ours: Option<Version>, token: Option<String>, wake: Wake) -> Receiver<Request> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let Some(request) = handle_incoming(&mut stream, ours.as_ref(), token.as_deref()) else { continue };
            drop(stream);
            if tx.send(request).is_err() {
                break;
            }
            if let Some(wake) = wake.get() {
                wake();
            }
        }
    });
    rx
}

/// Reads a line or message up to `\n` or EOF, at most [`MAX_GREETING`] bytes and one second in total.
fn read_message(stream: &mut TcpStream) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut message = Vec::new();
    let mut byte = [0u8; 1];
    while message.len() < MAX_GREETING {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            break;
        }
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                message.push(byte[0]);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    if message.ends_with(b"\r") {
        message.pop();
    }
    message
}

/// The running launcher's side: handles a newcomer's greeting, performing challenge-response
/// authentication before authorizing replacement (CWE-287, CWE-522, F-016, F-017).
fn handle_incoming(stream: &mut TcpStream, ours: Option<&Version>, token: Option<&str>) -> Option<Request> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let message = read_message(stream);
    if message.is_empty() {
        return None;
    }
    if message == SHOW {
        return Some(Request::Show);
    }
    let text = std::str::from_utf8(&message).ok()?;
    let rest = text.strip_prefix(HELLO)?;
    let mut parts = rest.split_whitespace();
    let theirs_str = parts.next()?;
    let client_nonce = parts.next();
    let theirs_ver = version::parse(theirs_str);

    let newer = match (&theirs_ver, ours) {
        (Some(theirs), Some(ours)) => version::is_newer(theirs, ours),
        _ => false,
    };

    let reply_running = format!("{RUNNING}{}\n", ours.map_or_else(|| "unknown".to_string(), ToString::to_string));

    if !newer {
        let _ = stream.write_all(reply_running.as_bytes());
        return Some(Request::Show);
    }

    // Newcomer is newer and asks to replace us.
    // Mutual challenge-response: require proof of token possession before granting Replace.
    let Some(ours_token) = token.filter(|t| !t.is_empty()) else {
        let _ = stream.write_all(reply_running.as_bytes());
        return Some(Request::Show);
    };

    let Some(client_nonce) = client_nonce else {
        let _ = stream.write_all(reply_running.as_bytes());
        return Some(Request::Show);
    };

    let Some(server_nonce) = fresh_secret() else {
        let _ = stream.write_all(reply_running.as_bytes());
        return Some(Request::Show);
    };

    let server_proof = compute_proof("server", ours_token, client_nonce, &server_nonce);
    let challenge_msg = format!("{CHALLENGE}{server_nonce} {server_proof}\n");
    if stream.write_all(challenge_msg.as_bytes()).is_err() {
        return None;
    }

    let auth_msg = read_message(stream);
    let auth_text = std::str::from_utf8(&auth_msg).unwrap_or("");
    let client_proof = auth_text.strip_prefix(AUTH).map(str::trim).unwrap_or("");
    let expected_client_proof = compute_proof("client", ours_token, client_nonce, &server_nonce);

    let _ = stream.write_all(reply_running.as_bytes());

    if constant_time_eq(client_proof, &expected_client_proof) { Some(Request::Replace) } else { Some(Request::Show) }
}

/// The new start's side: says hello to whatever holds the port and reads the answer.
fn ask(addr: SocketAddr, ours: Option<&Version>, token: Option<&str>) -> std::io::Result<Answer> {
    let Some(ours) = ours else {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
        stream.write_all(SHOW)?;
        return Ok(Answer::Defer);
    };
    let answer = ask_once(addr, ours, token, true)?;
    // Older running copies (0.1.4 to 0.6.1) hang up when a space follows the version.
    // Retry once without client nonce so an upgrade over such a copy still replaces it.
    if matches!(answer, Answer::Legacy) {
        return ask_once(addr, ours, token, false);
    }
    Ok(answer)
}

fn ask_once(addr: SocketAddr, ours: &Version, token: Option<&str>, with_nonce: bool) -> std::io::Result<Answer> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));

    let client_nonce = if with_nonce { fresh_secret() } else { None };
    let greeting = match (&client_nonce, token) {
        (Some(nonce), Some(tok)) if !tok.is_empty() => format!("{HELLO}{ours} {nonce}\n"),
        _ => format!("{HELLO}{ours}\n"),
    };
    stream.write_all(greeting.as_bytes())?;

    let reply = read_message(&mut stream);
    if reply.is_empty() {
        return Ok(Answer::Legacy);
    }

    let reply_str = match std::str::from_utf8(&reply) {
        Ok(s) => s.trim(),
        Err(_) => return Ok(Answer::Defer),
    };

    if let Some(challenge_rest) = reply_str.strip_prefix(CHALLENGE) {
        let mut parts = challenge_rest.split_whitespace();
        let server_nonce = parts.next().unwrap_or("");
        let server_proof = parts.next().unwrap_or("");
        let (Some(client_nonce), Some(tok)) = (client_nonce.as_deref(), token) else {
            return Ok(Answer::Defer);
        };
        let expected_server_proof = compute_proof("server", tok, client_nonce, server_nonce);
        if !constant_time_eq(server_proof, &expected_server_proof) {
            // Server failed to prove it knows the shared token (untrusted peer).
            // Do NOT disclose client proof; abort immediately.
            return Ok(Answer::Defer);
        }

        // Server is authentic. Prove our identity to server:
        let client_proof = compute_proof("client", tok, client_nonce, server_nonce);
        let auth_msg = format!("{AUTH}{client_proof}\n");
        stream.write_all(auth_msg.as_bytes())?;

        let final_reply = read_message(&mut stream);
        return Ok(verdict(&final_reply, ours));
    }

    Ok(verdict(&reply, ours))
}

/// Reads the running launcher's answer.
fn verdict(reply: &[u8], ours: &Version) -> Answer {
    if reply.is_empty() {
        return Answer::Legacy;
    }
    let trimmed = std::str::from_utf8(reply).ok().map(str::trim);
    let running = trimmed.and_then(|r| r.strip_prefix(RUNNING)).and_then(version::parse);
    match running {
        Some(running) if version::is_newer(ours, &running) => Answer::TakeOver,
        _ => Answer::Defer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        version::parse(s).unwrap()
    }

    const TOK: &str = "deadbeefcafef00d";

    /// A primary on a free loopback port, as version `running`, authenticating with [`TOK`].
    fn primary(running: &str) -> (SocketAddr, Receiver<Request>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        (addr, listen(listener, Some(v(running)), Some(TOK.to_string()), Wake::default()))
    }

    fn next(rx: &Receiver<Request>) -> Request {
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn a_newer_start_replaces_an_older_running_launcher() {
        let (addr, rx) = primary("0.1.0");
        assert_eq!(ask(addr, Some(&v("0.1.2")), Some(TOK)).ok(), Some(Answer::TakeOver));
        assert_eq!(next(&rx), Request::Replace);
    }

    #[test]
    fn a_replace_requires_the_same_users_token() {
        // A local peer on the fixed port with the right (newer) version but invalid token can
        // only bring the window forward, never make the launcher quit (CWE-862, CWE-287).
        let (addr, rx) = primary("0.1.0");
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("{HELLO}9.9.9 not-the-token\n").as_bytes()).unwrap();
        let mut reply = Vec::new();
        s.read_to_end(&mut reply).unwrap();
        assert_eq!(next(&rx), Request::Show, "an unauthenticated peer must not replace us");

        // And one with no token at all.
        let (addr, rx) = primary("0.1.0");
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("{HELLO}9.9.9\n").as_bytes()).unwrap();
        let mut reply = Vec::new();
        s.read_to_end(&mut reply).unwrap();
        assert_eq!(next(&rx), Request::Show);
    }

    #[test]
    fn token_is_never_transmitted_over_the_wire() {
        // F-016: The client must never send the persistent bearer token to an unauthenticated listener.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let n = stream.read(&mut buf).unwrap_or(0);
            buf[..n].to_vec()
        });

        let secret = "super-secret-persistent-bearer-token";
        let _ = ask(addr, Some(&v("0.1.2")), Some(secret));
        let wire_bytes = handle.join().unwrap();
        let wire_str = String::from_utf8_lossy(&wire_bytes);
        assert!(!wire_str.contains(secret), "bearer token must never be sent in the clear");
    }

    #[test]
    fn rogue_server_failing_proof_receives_no_client_credentials() {
        // F-016 / F-017: If the listening process cannot prove it knows the token, client aborts.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_message(&mut stream);
            // Rogue server sends fake challenge with forged proof
            let _ = stream.write_all(b"artcraft-launcher:challenge fake-nonce forged-proof\n");
            let mut next = [0u8; 128];
            stream.read(&mut next).unwrap_or(0)
        });

        let ans = ask(addr, Some(&v("0.1.2")), Some(TOK)).unwrap();
        assert_eq!(ans, Answer::Defer);
        let bytes_received = handle.join().unwrap();
        assert_eq!(bytes_received, 0, "client must immediately disconnect and send no auth proof");
    }

    #[test]
    fn replaying_an_old_auth_response_is_rejected() {
        // F-017: Replay attacks across connections must be rejected because server nonces are fresh.
        let (addr, rx) = primary("0.1.0");

        // Stale proof generated with a different nonce
        let fake_client_nonce = "1111111111111111111111111111111111111111111111111111111111111111";
        let fake_server_nonce = "2222222222222222222222222222222222222222222222222222222222222222";
        let replayed_proof = compute_proof("client", TOK, fake_client_nonce, fake_server_nonce);

        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(format!("{HELLO}9.9.9 {fake_client_nonce}\n").as_bytes()).unwrap();

        let challenge = read_message(&mut stream);
        assert!(std::str::from_utf8(&challenge).unwrap().starts_with(CHALLENGE));

        // Replay stale proof
        stream.write_all(format!("{AUTH}{replayed_proof}\n").as_bytes()).unwrap();

        let reply = read_message(&mut stream);
        assert!(std::str::from_utf8(&reply).unwrap().starts_with(RUNNING));
        assert_eq!(next(&rx), Request::Show, "replayed auth proof must not authorize Replace");
    }

    #[test]
    fn a_fragmented_greeting_waits_for_the_complete_message() {
        for (running, first, rest, version) in [("0.1.0", "", "0.1.2", "0.1.2"), ("0.1.2", "0.1.1", "0", "0.1.10")] {
            let (addr, rx) = primary(running);
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            stream.write_all(format!("{HELLO}{first}").as_bytes()).unwrap();
            assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "an incomplete greeting must not show the old launcher");
            stream.write_all(format!("{rest}\n").as_bytes()).unwrap();
            let mut reply = Vec::new();
            stream.read_to_end(&mut reply).unwrap();
            assert_eq!(verdict(&reply, &v(version)), Answer::TakeOver);
            assert_eq!(next(&rx), Request::Show);
        }
    }

    #[test]
    fn an_older_greeting_without_a_write_shutdown_still_gets_an_answer() {
        let (addr, rx) = primary("0.1.0");
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        stream.write_all(format!("{HELLO}0.1.2\n").as_bytes()).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        assert_eq!(verdict(&reply, &v("0.1.2")), Answer::TakeOver);
        assert_eq!(next(&rx), Request::Show);
    }

    #[test]
    fn a_peer_that_keeps_sending_cannot_extend_the_greeting_deadline() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = channel();
        let reader = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            tx.send(read_message(&mut stream)).unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();
        let writer = std::thread::spawn(move || {
            for _ in 0..100 {
                if stream.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let message = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(!message.is_empty());
        assert!(message.len() < 64, "the deadline should stop a slow greeting before the size limit");
        reader.join().unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn the_same_or_an_older_start_shows_the_running_launcher() {
        let (addr, rx) = primary("0.1.2");
        assert_eq!(ask(addr, Some(&v("0.1.2")), Some(TOK)).ok(), Some(Answer::Defer));
        assert_eq!(next(&rx), Request::Show);
        assert_eq!(ask(addr, Some(&v("0.1.0")), Some(TOK)).ok(), Some(Answer::Defer));
        assert_eq!(next(&rx), Request::Show);
    }

    #[test]
    fn a_start_from_before_the_handshake_still_shows_the_window() {
        let (addr, rx) = primary("0.1.4");
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(SHOW).unwrap();
        assert_eq!(next(&rx), Request::Show);
    }

    #[test]
    fn a_running_launcher_from_before_the_handshake_is_recognised() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let old = std::thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((mut s, _)) = listener.accept() {
                    let mut buf = [0u8; 64];
                    let _ = s.read(&mut buf);
                }
            }
        });
        assert_eq!(ask(addr, Some(&v("0.1.4")), Some(TOK)).ok(), Some(Answer::Legacy));
        let _ = old.join();
    }

    #[test]
    fn a_tokenless_retry_replaces_a_running_launcher_from_before_the_token_handshake() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let old = std::thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((mut s, _)) = listener.accept() {
                    let msg = read_message(&mut s);
                    if let Some(ver) = std::str::from_utf8(&msg).ok().and_then(|m| m.strip_prefix(HELLO)).and_then(version::parse)
                        && version::is_newer(&ver, &v("0.6.1"))
                    {
                        let _ = s.write_all(b"artcraft-launcher:running 0.6.1\n");
                    }
                }
            }
        });
        assert_eq!(ask(addr, Some(&v("0.6.2")), Some(TOK)).ok(), Some(Answer::TakeOver));
        let _ = old.join();
    }

    #[test]
    fn a_fresh_secret_is_a_distinct_nonempty_hex_string() {
        let a = fresh_secret().unwrap();
        let b = fresh_secret().unwrap();
        assert_eq!(a.len(), 64, "256-bit hex");
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "two secrets must differ");
    }

    #[test]
    fn verdict_reads_defensively() {
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.0\n", &v("0.1.2")), Answer::TakeOver);
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.2\n", &v("0.1.2")), Answer::Defer);
        assert_eq!(verdict(b"artcraft-launcher:running unknown\n", &v("0.1.2")), Answer::Defer);
        assert_eq!(verdict(b"garbage\n", &v("0.1.2")), Answer::Defer);
        assert_eq!(verdict(b"", &v("0.1.2")), Answer::Legacy);
    }
}
