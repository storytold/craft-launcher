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

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use artcraft_launcher_core::version::{self, Version};

const PORT: u16 = 47_413;
/// What launchers before the version handshake send (and still the answer to an unknown peer).
const SHOW: &[u8] = b"artcraft-launcher:show";
/// `hello <version> <token>`: a second start introduces itself and proves it is the same OS user.
const HELLO: &str = "artcraft-launcher:hello ";
/// `running <version>`: the first instance's answer.
const RUNNING: &str = "artcraft-launcher:running ";
/// How long a newer start waits for the old one to quit and free the port.
const TAKE_OVER_WITHIN: Duration = Duration::from_secs(10);
/// How large a greeting may be, now that it carries a token as well as a version.
const MAX_GREETING: usize = 128;

/// A per-user secret that proves a hello comes from this OS account.
///
/// The fixed loopback port is reachable by any local process, so a `hello` alone can't be trusted
/// to decide the launcher's lifecycle: another local user could send a newer version and make an
/// idle launcher quit (CWE-862). The token lives in the user's own data folder, readable only by
/// this account, so a peer that echoes it must be able to read that file — i.e. be the same user.
/// It is read once, created on first use with owner-only permissions.
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

/// Answers other starts on `listener` until the receiver is dropped.
fn listen(listener: TcpListener, ours: Option<Version>, token: Option<String>, wake: Wake) -> Receiver<Request> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let message = read_message(&mut stream);
            let Some((request, reply)) = answer(&message, ours.as_ref(), token.as_deref()) else { continue };
            if let Some(reply) = reply {
                let _ = stream.write_all(reply.as_bytes());
            }
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

/// Reads a greeting through EOF, at most [`MAX_GREETING`] bytes and one second in total. New
/// starts close their write half to delimit the message; older starts keep it open while waiting
/// for a reply, so a read timeout also ends their greeting.
fn read_message(stream: &mut TcpStream) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut message = Vec::new();
    let mut buf = [0u8; MAX_GREETING];
    while message.len() < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            break;
        }
        let available = buf.len() - message.len();
        let Some(room) = buf.get_mut(..available) else { break };
        match stream.read(room) {
            Ok(0) => break,
            Ok(n) => {
                if let Some(bytes) = room.get(..n) {
                    message.extend_from_slice(bytes);
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    message
}

/// The running launcher's side: what `message` asks for, and what to answer.
///
/// A `Replace` (which can make the launcher quit) is only granted when the greeting carries this
/// user's token: the fixed loopback port is reachable by any local account, so an unauthenticated
/// peer may bring the window forward (`Show`, harmless) but may not end the launcher (CWE-862).
fn answer(message: &[u8], ours: Option<&Version>, token: Option<&str>) -> Option<(Request, Option<String>)> {
    if message == SHOW {
        return Some((Request::Show, None));
    }
    let rest = std::str::from_utf8(message).ok()?.strip_prefix(HELLO)?;
    // `hello <version> <token>`; the token is optional (legacy peers omit it).
    let (theirs, their_token) = match rest.split_once(' ') {
        Some((v, t)) => (v, Some(t.trim())),
        None => (rest, None),
    };
    let authenticated = match (token, their_token) {
        (Some(ours_tok), Some(theirs_tok)) => !ours_tok.is_empty() && ours_tok == theirs_tok,
        _ => false,
    };
    let newer = match (version::parse(theirs), ours) {
        (Some(theirs), Some(ours)) => version::is_newer(&theirs, ours),
        _ => false,
    };
    let reply = format!("{RUNNING}{}", ours.map_or_else(|| "unknown".to_string(), ToString::to_string));
    // Only a newer start from the same user may replace us; anything else just shows the window.
    let request = if newer && authenticated { Request::Replace } else { Request::Show };
    Some((request, Some(reply)))
}

/// The new start's side: says hello to whatever holds the port and reads the answer.
fn ask(addr: SocketAddr, ours: Option<&Version>, token: Option<&str>) -> std::io::Result<Answer> {
    let Some(ours) = ours else {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
        // Can't compare without our own version: ask it to show itself, as before.
        stream.write_all(SHOW)?;
        return Ok(Answer::Defer);
    };
    let greeting = match token {
        Some(tok) if !tok.is_empty() => format!("{HELLO}{ours} {tok}"),
        _ => format!("{HELLO}{ours}"),
    };
    let answer = ask_once(addr, ours, &greeting)?;
    // Launchers from before the token handshake (0.1.4 to 0.6.1) try to parse the whole greeting
    // after `hello ` as a version and hang up when a space + token follows. Retry once without the
    // token so an upgrade over such a running copy still replaces it.
    if matches!(answer, Answer::Legacy) && matches!(token, Some(tok) if !tok.is_empty()) {
        return ask_once(addr, ours, &format!("{HELLO}{ours}"));
    }
    Ok(answer)
}

fn ask_once(addr: SocketAddr, ours: &Version, greeting: &str) -> std::io::Result<Answer> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
    stream.write_all(greeting.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut reply = Vec::new();
    match (&mut stream).take(MAX_GREETING as u64).read_to_end(&mut reply) {
        Ok(_) => {}
        // An old launcher may reset the connection rather than close it.
        Err(e) if reply.is_empty() && matches!(e.kind(), ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted) => {}
        // No answer in time: leave the running one be.
        Err(_) if reply.is_empty() => return Ok(Answer::Defer),
        Err(_) => {}
    }
    Ok(verdict(&reply, ours))
}

/// Reads the running launcher's answer.
fn verdict(reply: &[u8], ours: &Version) -> Answer {
    if reply.is_empty() {
        return Answer::Legacy;
    }
    let running = std::str::from_utf8(reply).ok().and_then(|r| r.strip_prefix(RUNNING)).and_then(version::parse);
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

    // Issue #29: 0.1.2 started while 0.1.0 sat in the tray, and the 0.1.0 window came up
    // offering 0.1.1. A newer start must replace the older one, not show it.
    #[test]
    fn a_newer_start_replaces_an_older_running_launcher() {
        let (addr, rx) = primary("0.1.0");
        assert_eq!(ask(addr, Some(&v("0.1.2")), Some(TOK)).ok(), Some(Answer::TakeOver));
        assert_eq!(next(&rx), Request::Replace);
    }

    #[test]
    fn a_replace_requires_the_same_users_token() {
        // A local peer on the fixed port with the right (newer) version but no/invalid token can
        // only bring the window forward, never make the launcher quit (CWE-862).
        let (addr, rx) = primary("0.1.0");
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("{HELLO}9.9.9 not-the-token").as_bytes()).unwrap();
        s.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        s.read_to_end(&mut reply).unwrap();
        assert_eq!(next(&rx), Request::Show, "an unauthenticated peer must not replace us");

        // And one with no token at all.
        let (addr, rx) = primary("0.1.0");
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("{HELLO}9.9.9").as_bytes()).unwrap();
        s.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        s.read_to_end(&mut reply).unwrap();
        assert_eq!(next(&rx), Request::Show);
    }

    #[test]
    fn a_fragmented_greeting_waits_for_the_complete_version() {
        for (running, first, rest, version) in [("0.1.0", "", "0.1.2", "0.1.2"), ("0.1.2", "0.1.1", "0", "0.1.10")] {
            let (addr, rx) = primary(running);
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            stream.write_all(format!("{HELLO}{first}").as_bytes()).unwrap();
            assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "an incomplete greeting must not show the old launcher");
            stream.write_all(format!("{rest} {TOK}").as_bytes()).unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut reply = Vec::new();
            stream.read_to_end(&mut reply).unwrap();
            assert_eq!(verdict(&reply, &v(version)), Answer::TakeOver);
            assert_eq!(next(&rx), Request::Replace);
        }
    }

    #[test]
    fn an_older_greeting_without_a_write_shutdown_still_gets_an_answer() {
        let (addr, rx) = primary("0.1.0");
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        stream.write_all(format!("{HELLO}0.1.2 {TOK}").as_bytes()).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        assert_eq!(verdict(&reply, &v("0.1.2")), Answer::TakeOver);
        assert_eq!(next(&rx), Request::Replace);
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
        // What 0.1.0 to 0.1.3 do with a message they don't know: read it and hang up (on both the
        // initial token-bearing greeting and the tokenless retry).
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
        // What 0.1.4 to 0.6.1 do: reject `hello <ver> <tok>` with no reply, then accept `hello <ver>`.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let old = std::thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((mut s, _)) = listener.accept() {
                    let msg = read_message(&mut s);
                    if let Some(ver) = std::str::from_utf8(&msg).ok().and_then(|m| m.strip_prefix(HELLO)).and_then(version::parse)
                        && version::is_newer(&ver, &v("0.6.1"))
                    {
                        let _ = s.write_all(b"artcraft-launcher:running 0.6.1");
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
    fn answers_are_read_defensively() {
        let tok = Some(TOK);
        // A newer version replaces us only with a matching token; without it, it just shows.
        assert_eq!(answer(format!("artcraft-launcher:hello 9.9.9 {TOK}").as_bytes(), Some(&v("0.1.0")), tok).map(|a| a.0), Some(Request::Replace));
        assert_eq!(answer(b"artcraft-launcher:hello 9.9.9 wrong", Some(&v("0.1.0")), tok).map(|a| a.0), Some(Request::Show));
        assert_eq!(answer(b"artcraft-launcher:hello 9.9.9", Some(&v("0.1.0")), tok).map(|a| a.0), Some(Request::Show));
        assert_eq!(answer(b"artcraft-launcher:hello nonsense", Some(&v("0.1.0")), tok).map(|a| a.0), Some(Request::Show));
        assert_eq!(answer(format!("artcraft-launcher:hello 9.9.9 {TOK}").as_bytes(), None, tok).map(|a| a.0), Some(Request::Show));
        assert_eq!(answer(b"hello", Some(&v("0.1.0")), tok), None);
        assert_eq!(answer(&[0xff, 0xfe], Some(&v("0.1.0")), tok), None);
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.0", &v("0.1.2")), Answer::TakeOver);
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.2", &v("0.1.2")), Answer::Defer);
        assert_eq!(verdict(b"artcraft-launcher:running unknown", &v("0.1.2")), Answer::Defer);
        assert_eq!(verdict(b"garbage", &v("0.1.2")), Answer::Defer);
        assert_eq!(verdict(b"", &v("0.1.2")), Answer::Legacy);
    }
}
