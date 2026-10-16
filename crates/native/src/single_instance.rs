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
//! A start with `--open <place>` (another Craft app's "Browse more" button) adds one thing: after
//! the hello, when the running launcher stays, it sends `open <place>` on a second connection, which
//! needs no answer. A launcher from before this ignores that message and has still shown its window
//! because of the hello. When the newcomer takes over instead, it carries the place itself.
//! The place is read by [`crate::deeplink`], which accepts almost nothing.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use artcraft_launcher_core::version::{self, Version};

use crate::deeplink::Target;

const PORT: u16 = 47_413;
/// What launchers before the version handshake send (and still the answer to an unknown peer).
const SHOW: &[u8] = b"artcraft-launcher:show";
/// `open <place>`: show this part of the window (after the hello that shows it).
const OPEN: &str = "artcraft-launcher:open ";
/// `hello <version>`: a second start introduces itself.
const HELLO: &str = "artcraft-launcher:hello ";
/// `running <version>`: the first instance's answer.
const RUNNING: &str = "artcraft-launcher:running ";
/// How long a newer start waits for the old one to quit and free the port.
const TAKE_OVER_WITHIN: Duration = Duration::from_secs(10);

/// What another start asks of the running launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Bring the window forward.
    Show,
    /// A newer launcher is starting: quit so it can take over (or show the window when quitting
    /// now would cut short an install).
    Replace,
    /// Go to this place in the window (the window is shown by the hello that came with it).
    Open(Target),
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

/// Claims the instance; `wake` is set later (the UI context doesn't exist yet). `open` is the place
/// this start was asked to show: it goes to the running launcher, or waits for the new one.
pub fn claim(wake: Wake, open: Option<Target>) -> Instance {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, PORT));
    let ours = ours();
    if let Ok(listener) = TcpListener::bind(addr) {
        return Instance::Primary(listen(listener, ours, wake, open));
    }
    match ask_and_open(addr, ours.as_ref(), open.as_ref()) {
        Ok(Answer::Defer) => Instance::Secondary,
        Ok(Answer::TakeOver) => {
            let deadline = Instant::now() + TAKE_OVER_WITHIN;
            while Instant::now() < deadline {
                if let Ok(listener) = TcpListener::bind(addr) {
                    return Instance::Primary(listen(listener, ours, wake, open));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // It stayed (busy installing) and showed its window instead.
            Instance::Secondary
        }
        Ok(Answer::Legacy) => {
            log::warn!("an older ArtCraft Launcher is running and can't be asked to quit; starting anyway");
            Instance::Primary(waiting(open))
        }
        // The port is taken by something else: run anyway rather than refuse to start.
        Err(_) => Instance::Primary(waiting(open)),
    }
}

/// A receiver nobody will send to, holding the place this start was asked to show.
fn waiting(open: Option<Target>) -> Receiver<Request> {
    let (tx, rx) = channel();
    start_with(&tx, open);
    rx
}

fn start_with(tx: &Sender<Request>, open: Option<Target>) {
    if let Some(place) = open {
        let _ = tx.send(Request::Open(place));
    }
}

/// Answers other starts on `listener` until the receiver is dropped.
fn listen(listener: TcpListener, ours: Option<Version>, wake: Wake, open: Option<Target>) -> Receiver<Request> {
    let (tx, rx) = channel();
    start_with(&tx, open);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0u8; 192];
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let n = stream.read(&mut buf).unwrap_or(0);
            let Some((request, reply)) = buf.get(..n).and_then(|m| answer(m, ours.as_ref())) else { continue };
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

/// The running launcher's side: what `message` asks for, and what to answer.
fn answer(message: &[u8], ours: Option<&Version>) -> Option<(Request, Option<String>)> {
    if message == SHOW {
        return Some((Request::Show, None));
    }
    if let Some(place) = message.strip_prefix(OPEN.as_bytes()) {
        let target = Target::parse(std::str::from_utf8(place).ok()?)?;
        return Some((Request::Open(target), None));
    }
    let theirs = std::str::from_utf8(message).ok()?.strip_prefix(HELLO)?;
    let newer = match (version::parse(theirs), ours) {
        (Some(theirs), Some(ours)) => version::is_newer(&theirs, ours),
        _ => false,
    };
    let reply = format!("{RUNNING}{}", ours.map_or_else(|| "unknown".to_string(), ToString::to_string));
    Some((if newer { Request::Replace } else { Request::Show }, Some(reply)))
}

/// [`ask`], then, when the running launcher stays, where to go.
fn ask_and_open(addr: SocketAddr, ours: Option<&Version>, open: Option<&Target>) -> std::io::Result<Answer> {
    let answer = ask(addr, ours)?;
    if let (Answer::Defer, Some(place)) = (&answer, open) {
        // Best effort: the window is already coming forward because of the hello.
        if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            let _ = stream.write_all(format!("{OPEN}{}", place.text()).as_bytes());
        }
    }
    Ok(answer)
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

    fn first_with(open: Option<Target>) -> Receiver<Request> {
        let (tx, rx) = channel();
        start_with(&tx, open);
        rx
    }

    fn v(s: &str) -> Version {
        version::parse(s).unwrap()
    }

    /// A primary on a free loopback port, as version `running`.
    fn primary(running: &str) -> (SocketAddr, Receiver<Request>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        (addr, listen(listener, Some(v(running)), Wake::default(), None))
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

    fn target(app: &str) -> Target {
        Target { app: Some(app.into()), kind: Some("theme".into()) }
    }

    // A second start with `--open` shows the running launcher (as any second start does) and also
    // tells it where to go.
    #[test]
    fn a_second_start_with_a_place_to_open_shows_the_window_and_sends_it_there() {
        let (addr, rx) = primary("0.1.2");
        assert_eq!(ask_and_open(addr, Some(&v("0.1.2")), Some(&target("photocraft"))).ok(), Some(Answer::Defer));
        assert_eq!(next(&rx), Request::Show);
        assert_eq!(next(&rx), Request::Open(target("photocraft")));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "nothing else is sent");
    }

    #[test]
    fn a_second_start_without_a_place_sends_nothing_extra() {
        let (addr, rx) = primary("0.1.2");
        assert_eq!(ask_and_open(addr, Some(&v("0.1.2")), None).ok(), Some(Answer::Defer));
        assert_eq!(next(&rx), Request::Show);
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    // A newer start takes over, so it opens the place itself rather than sending it to a launcher
    // that is quitting.
    #[test]
    fn a_newer_start_does_not_send_the_place_to_the_launcher_it_replaces() {
        let (addr, rx) = primary("0.1.0");
        assert_eq!(ask_and_open(addr, Some(&v("0.1.2")), Some(&target("photocraft"))).ok(), Some(Answer::TakeOver));
        assert_eq!(next(&rx), Request::Replace);
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn an_open_message_on_its_own_is_understood() {
        let (addr, rx) = primary("0.1.4");
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"artcraft-launcher:open extensions?app=photocraft").unwrap();
        drop(s);
        assert_eq!(next(&rx), Request::Open(Target { app: Some("photocraft".into()), kind: None }));
    }

    #[test]
    fn open_messages_are_read_defensively() {
        let ours = v("0.1.2");
        let open = |m: &[u8]| answer(m, Some(&ours)).map(|a| a.0);
        assert_eq!(open(b"artcraft-launcher:open extensions"), Some(Request::Open(Target::default())));
        assert_eq!(
            open(b"artcraft-launcher:open extensions?app=Bad&kind=plugin"),
            Some(Request::Open(Target { app: None, kind: Some("plugin".into()) }))
        );
        assert_eq!(open(b"artcraft-launcher:open settings"), None);
        assert_eq!(open(b"artcraft-launcher:open "), None);
        assert_eq!(open(b"artcraft-launcher:open"), None);
        assert_eq!(open(&[b'a', 0xff]), None);
        // An open message gets no reply: only the hello is answered.
        assert_eq!(answer(b"artcraft-launcher:open extensions", Some(&ours)).and_then(|a| a.1), None);
    }

    // A first launcher started by `--open` has nothing to forward to, so it carries the place itself.
    #[test]
    fn a_first_start_with_a_place_finds_it_waiting() {
        let rx = first_with(Some(Target { app: Some("photocraft".into()), kind: None }));
        assert_eq!(next(&rx), Request::Open(Target { app: Some("photocraft".into()), kind: None }));
        assert!(first_with(None).recv_timeout(Duration::from_millis(100)).is_err());
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
