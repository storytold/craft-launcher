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
/// `hello <version>`: a second start introduces itself.
const HELLO: &str = "artcraft-launcher:hello ";
/// `running <version>`: the first instance's answer.
const RUNNING: &str = "artcraft-launcher:running ";
/// How long a newer start waits for the old one to quit and free the port.
const TAKE_OVER_WITHIN: Duration = Duration::from_secs(10);

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
    if let Ok(listener) = TcpListener::bind(addr) {
        return Instance::Primary(listen(listener, ours, wake));
    }
    match ask(addr, ours.as_ref()) {
        Ok(Answer::Defer) => Instance::Secondary,
        Ok(Answer::TakeOver) => {
            let deadline = Instant::now() + TAKE_OVER_WITHIN;
            while Instant::now() < deadline {
                if let Ok(listener) = TcpListener::bind(addr) {
                    return Instance::Primary(listen(listener, ours, wake));
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
fn listen(listener: TcpListener, ours: Option<Version>, wake: Wake) -> Receiver<Request> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let message = read_message(&mut stream);
            let Some((request, reply)) = answer(&message, ours.as_ref()) else { continue };
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

/// Reads a greeting through EOF, at most 64 bytes and one second in total. New starts close
/// their write half to delimit the message; older starts keep it open while waiting for a
/// reply, so a read timeout also ends their greeting.
fn read_message(stream: &mut TcpStream) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut message = Vec::new();
    let mut buf = [0u8; 64];
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
fn answer(message: &[u8], ours: Option<&Version>) -> Option<(Request, Option<String>)> {
    if message == SHOW {
        return Some((Request::Show, None));
    }
    let theirs = std::str::from_utf8(message).ok()?.strip_prefix(HELLO)?;
    let newer = match (version::parse(theirs), ours) {
        (Some(theirs), Some(ours)) => version::is_newer(&theirs, ours),
        _ => false,
    };
    let reply = format!("{RUNNING}{}", ours.map_or_else(|| "unknown".to_string(), ToString::to_string));
    Some((if newer { Request::Replace } else { Request::Show }, Some(reply)))
}

/// The new start's side: says hello to whatever holds the port and reads the answer.
fn ask(addr: SocketAddr, ours: Option<&Version>) -> std::io::Result<Answer> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
    let Some(ours) = ours else {
        // Can't compare without our own version: ask it to show itself, as before.
        stream.write_all(SHOW)?;
        return Ok(Answer::Defer);
    };
    stream.write_all(format!("{HELLO}{ours}").as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut reply = Vec::new();
    match (&mut stream).take(64).read_to_end(&mut reply) {
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

    /// A primary on a free loopback port, as version `running`.
    fn primary(running: &str) -> (SocketAddr, Receiver<Request>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        (addr, listen(listener, Some(v(running)), Wake::default()))
    }

    fn next(rx: &Receiver<Request>) -> Request {
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    // Issue #29: 0.1.2 started while 0.1.0 sat in the tray, and the 0.1.0 window came up
    // offering 0.1.1. A newer start must replace the older one, not show it.
    #[test]
    fn a_newer_start_replaces_an_older_running_launcher() {
        let (addr, rx) = primary("0.1.0");
        assert_eq!(ask(addr, Some(&v("0.1.2"))).ok(), Some(Answer::TakeOver));
        assert_eq!(next(&rx), Request::Replace);
    }

    #[test]
    fn a_fragmented_greeting_waits_for_the_complete_version() {
        for (running, first, rest, version) in [("0.1.0", "", "0.1.2", "0.1.2"), ("0.1.2", "0.1.1", "0", "0.1.10")] {
            let (addr, rx) = primary(running);
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            stream.write_all(format!("{HELLO}{first}").as_bytes()).unwrap();
            assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "an incomplete greeting must not show the old launcher");
            stream.write_all(rest.as_bytes()).unwrap();
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
        stream.write_all(format!("{HELLO}0.1.2").as_bytes()).unwrap();
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
        assert_eq!(ask(addr, Some(&v("0.1.2"))).ok(), Some(Answer::Defer));
        assert_eq!(next(&rx), Request::Show);
        assert_eq!(ask(addr, Some(&v("0.1.0"))).ok(), Some(Answer::Defer));
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
        // What 0.1.0 to 0.1.3 do with a message they don't know: read it and hang up.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let old = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 64];
                let _ = s.read(&mut buf);
            }
        });
        assert_eq!(ask(addr, Some(&v("0.1.4"))).ok(), Some(Answer::Legacy));
        let _ = old.join();
    }

    #[test]
    fn answers_are_read_defensively() {
        assert_eq!(answer(b"artcraft-launcher:hello 9.9.9", Some(&v("0.1.0"))).map(|a| a.0), Some(Request::Replace));
        assert_eq!(answer(b"artcraft-launcher:hello nonsense", Some(&v("0.1.0"))).map(|a| a.0), Some(Request::Show));
        assert_eq!(answer(b"artcraft-launcher:hello 9.9.9", None).map(|a| a.0), Some(Request::Show));
        assert_eq!(answer(b"hello", Some(&v("0.1.0"))), None);
        assert_eq!(answer(&[0xff, 0xfe], Some(&v("0.1.0"))), None);
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.0", &v("0.1.2")), Answer::TakeOver);
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.2", &v("0.1.2")), Answer::Defer);
        assert_eq!(verdict(b"artcraft-launcher:running unknown", &v("0.1.2")), Answer::Defer);
        assert_eq!(verdict(b"garbage", &v("0.1.2")), Answer::Defer);
        assert_eq!(verdict(b"", &v("0.1.2")), Answer::Legacy);
    }
}
