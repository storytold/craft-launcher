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
//! another local program can do with any of this is bring the window forward.
//!
//! A newcomer only steps aside (exits) for a running launcher that proved it holds this account's
//! token. Anything else on the port, including a listener that answers like a launcher but fails
//! or skips the proof, is treated as unrelated, and the newcomer starts anyway.
//!
//! The Linux launcher inside WSL listens on its own port (issue #271). WSL shares loopback ports
//! with Windows (WSL 2 forwards them, WSL 1 binds Windows' own), so on the shared port a Windows
//! launcher started after a WSL one said hello to the WSL launcher and couldn't verify its token:
//! up to 0.14.0 it exited without a window, and from 0.14.1 it started without the port, so each
//! further Windows start opened another launcher.
//!
//! ## Upgrading over the published 0.7.0 build
//!
//! Replacement of a running 0.7.0 instance remains an unqualified compatibility gap. The
//! published 0.7.0 `answer` expects `hello <version> <token>` and grants `Replace` only when the
//! second word equals its owner-only token file. A newer build sends `hello <version> <nonce>`
//! instead, so 0.7.0 reads the nonce as a wrong token, replies `running 0.7.0`, and merely shows
//! its window. If that reply arrives in time, a newer newcomer requests takeover but the old
//! process does not release its port; since that reply carries no proof, the newcomer then starts
//! anyway without the listening socket. The old reader and new client have similar timeouts, so a
//! delayed reply can instead reach the legacy fallback, with the same result. These timing paths need an
//! old-release interoperability test. Quit the running 0.7.0 launcher from its tray menu before
//! starting the new version. Do not make the new side
//! send the token to force a `Replace`: that would restore the clear-text shared-secret handshake
//! this protocol removed (0.7.0 itself already puts the token on the wire when it starts). A
//! future compatibility shim would need an out-of-band, authenticated way to stop the old process;
//! it must not accept an unauthenticated lifecycle request.
//!
//! The handshake also carries one authenticated request the other way: a headless removal (an
//! app's Apps & features entry, which runs the launcher with `--uninstall`) hands itself to the
//! launcher that is running, so the one manifest of installed apps is written by the process that
//! owns it, never beside it ([`forward_uninstall`]). A process with no launcher running holds the
//! instance itself for the duration ([`Held`]), so a launcher starting meanwhile defers instead
//! of racing it.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use artcraft_launcher_core::version::{self, Version};

const PORT: u16 = 47_413;
/// The port inside WSL, so a WSL launcher never holds the Windows launcher's [`PORT`].
const WSL_PORT: u16 = 47_414;
/// What launchers before the version handshake send (and still the answer to an unknown peer).
const SHOW: &[u8] = b"artcraft-launcher:show";
/// `hello <version> <nonce>`: a second start introduces itself with its version and client nonce.
const HELLO: &str = "artcraft-launcher:hello ";
/// `challenge <nonce> <proof>`: running instance challenges newcomer to prove knowledge of token.
const CHALLENGE: &str = "artcraft-launcher:challenge ";
/// `auth <proof>`: newcomer's response proving possession of token.
const AUTH: &str = "artcraft-launcher:auth ";
/// `uninstall <app_id> <proof>`: a headless removal handing itself to the running launcher, in
/// the place of `auth` and after the same challenge. The launcher writes the one manifest that
/// says what is installed, so a removal must go through it rather than beside it; the proof is
/// made with the account's token, so another local program cannot have an app removed (CWE-862).
const UNINSTALL: &str = "artcraft-launcher:uninstall ";
/// `uninstalled ok` or `uninstalled <reason>`: the holder's answer to [`UNINSTALL`], one line.
const UNINSTALLED: &str = "artcraft-launcher:uninstalled ";
/// `running <version>`: the first instance's answer.
const RUNNING: &str = "artcraft-launcher:running ";
/// How long a newer start waits for the old one to quit and free the port.
const TAKE_OVER_WITHIN: Duration = Duration::from_secs(10);
/// How large a greeting or challenge-response line may be.
const MAX_GREETING: usize = 256;
/// How large an answer to a handed-over removal may be.
const MAX_ANSWER: usize = 1024;
/// How long a handed-over removal may take before the asking process gives up on the answer:
/// removing an app's files can take a while, and the answer is what the removal made of it.
const UNINSTALL_WITHIN: Duration = Duration::from_secs(300);
/// How long the holder waits for a removal to be registered before answering that none is. The
/// launcher registers one as soon as its backend exists; a request arriving before that is
/// served a moment later, and a holder with no backend at all says so without holding the asker
/// forever.
const HANDLER_WITHIN: Duration = Duration::from_secs(15);

/// The removal this process performs when another process hands one over (see
/// [`forward_uninstall`]). Whoever owns the manifest writes in this process registers it: the
/// launcher's backend, which holds the manifest behind its own lock, or the headless removal
/// itself, which is then the instance's owner.
pub type Uninstall = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

static UNINSTALL_HANDLER: OnceLock<Uninstall> = OnceLock::new();

/// Registers what an authenticated removal handed to this process runs. The first registration
/// stands: a launcher's backend registers while it is built, before any headless removal of the
/// same process could, and a headless removal registers before it hands anything over.
pub fn set_uninstall_handler(handler: Uninstall) {
    let _ = UNINSTALL_HANDLER.set(handler);
}

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
    /// It proved it is this account's launcher, as new as we are or newer, and shows its window:
    /// we exit.
    Defer,
    /// It's older and quits: we take over the port. `proven` when it proved it is this account's
    /// launcher; only then do we exit if it keeps the port.
    TakeOver { proven: bool },
    /// It closed without an answer: a launcher from before the handshake.
    Legacy,
    /// It didn't prove it is this account's launcher (failed or skipped the proof, or answered
    /// something unreadable): another program, so we start anyway (CWE-287).
    Unrelated,
}

type Wake = std::sync::Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>>;

/// This launcher's own version.
fn ours() -> Option<Version> {
    version::parse(env!("CARGO_PKG_VERSION"))
}

/// The port this launcher claims.
fn port() -> u16 {
    port_for(in_wsl())
}

/// [`WSL_PORT`] inside WSL, else [`PORT`].
fn port_for(wsl: bool) -> u16 {
    if wsl { WSL_PORT } else { PORT }
}

/// Whether this is the Linux launcher running inside WSL. The kernel release says so even when
/// the WSL variables aren't passed on (a systemd unit, say).
#[cfg(target_os = "linux")]
fn in_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some() || std::fs::read_to_string("/proc/sys/kernel/osrelease").is_ok_and(|r| is_wsl_kernel(&r))
}

#[cfg(not(target_os = "linux"))]
fn in_wsl() -> bool {
    false
}

/// Whether a kernel release is WSL's: `5.15.167.4-microsoft-standard-WSL2` (WSL 2) or
/// `4.4.0-19041-Microsoft` (WSL 1).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_wsl_kernel(release: &str) -> bool {
    release.to_ascii_lowercase().contains("microsoft")
}

/// Claims the instance; `wake` is set later (the UI context doesn't exist yet).
pub fn claim(wake: Wake) -> Instance {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port()));
    let ours = ours();
    let token = token();
    if let Ok(listener) = TcpListener::bind(addr) {
        return Instance::Primary(listen(listener, ours, token, wake));
    }
    match ask(addr, ours.as_ref(), token.as_deref()) {
        Ok(Answer::Defer) => Instance::Secondary,
        Ok(Answer::TakeOver { proven }) => {
            let deadline = Instant::now() + TAKE_OVER_WITHIN;
            while Instant::now() < deadline {
                if let Ok(listener) = TcpListener::bind(addr) {
                    return Instance::Primary(listen(listener, ours, token, wake));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if proven && holder_proves_itself(addr, ours.as_ref(), token.as_deref()) {
                // It stayed (busy installing) and showed its window instead. The proof is asked
                // again because the proven launcher may have quit and something else taken the port.
                Instance::Secondary
            } else {
                log::warn!("whatever holds the launcher's port didn't prove it is ArtCraft Launcher and kept the port; starting anyway");
                Instance::Primary(channel().1)
            }
        }
        Ok(Answer::Unrelated) => {
            log::warn!("something on the launcher's port didn't prove it is this account's ArtCraft Launcher; starting anyway");
            Instance::Primary(channel().1)
        }
        Ok(Answer::Legacy) => {
            log::warn!("an older ArtCraft Launcher is running and can't be asked to quit; starting anyway");
            Instance::Primary(channel().1)
        }
        // The port is taken by something else: run anyway rather than refuse to start.
        Err(_) => Instance::Primary(channel().1),
    }
}

/// Whether whatever holds the port now proves it is this account's launcher.
///
/// The probe greets with our own version, because launchers from 0.8.1 to 0.14.x only challenge a
/// newer newcomer, and checks the holder's proof. It never answers the challenge, so the holder
/// can't grant `Replace`: it can only bring its window forward.
fn holder_proves_itself(addr: SocketAddr, ours: Option<&Version>, token: Option<&str>) -> bool {
    let (Some(ours), Some(tok), Some(client_nonce)) = (ours, token.filter(|t| !t.is_empty()), fresh_secret()) else { return false };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    if stream.write_all(format!("{HELLO}{ours} {client_nonce}\n").as_bytes()).is_err() {
        return false;
    }
    let reply = read_message(&mut stream);
    let proven =
        std::str::from_utf8(&reply).ok().and_then(|r| r.trim().strip_prefix(CHALLENGE)).is_some_and(|rest| server_proved(rest, tok, &client_nonce));
    let _ = stream.shutdown(std::net::Shutdown::Both);
    proven
}

/// Whether a `challenge <nonce> <proof>` (without its prefix) proves the sender holds `token`.
fn server_proved(challenge_rest: &str, token: &str, client_nonce: &str) -> bool {
    let mut parts = challenge_rest.split_whitespace();
    let server_nonce = parts.next().unwrap_or("");
    let server_proof = parts.next().unwrap_or("");
    constant_time_eq(server_proof, &compute_proof("server", token, client_nonce, server_nonce))
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

/// The account's instance held by a process that is not a full launcher: a headless removal
/// holds it while it works, so a launcher starting meanwhile defers to this process rather than
/// racing its manifest save, and authenticated removals other processes hand over are served
/// ([`UNINSTALL_HANDLER`]). Dropping the value gives the instance up.
pub struct Held {
    stop: std::sync::mpsc::Sender<()>,
    _serve: JoinHandle<()>,
}

impl Held {
    /// Holds the instance when it is free; `None` when something else holds the port.
    pub fn take() -> Option<Held> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port()));
        let listener = TcpListener::bind(addr).ok()?;
        let (stop, stopped) = channel();
        let _serve = std::thread::spawn(move || serve_until(listener, ours(), token(), stopped));
        Some(Held { stop, _serve })
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // The serving thread sees this within a moment and closes the port; a removal it has
        // already handed to its thread finishes after, and its answer reaches whoever asked.
        let _ = self.stop.send(());
    }
}

/// Answers the handshake on `listener` until `stopped` is sent (or the sender is dropped), so a
/// headless holder gives the instance up when its work is done. Show and Replace mean nothing to
/// a holder without a window (a newer launcher starting waits its turn and defers), and removals
/// are answered on their own threads.
fn serve_until(listener: TcpListener, ours: Option<Version>, token: Option<String>, stopped: Receiver<()>) {
    let _ = listener.set_nonblocking(true);
    loop {
        match stopped.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                // Show and Replace mean nothing here: the holder is a headless removal, not a
                // launcher with a window, and any answer a removal needed went out on the stream.
                let _ = handle_incoming(&mut stream, ours.as_ref(), token.as_deref());
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
}

/// What an attempt to hand a removal to the launcher already holding the account's instance
/// made of it.
#[derive(Debug, PartialEq, Eq)]
pub enum Forwarded {
    /// Nothing answered the port: as far as this account goes, the instance is free, and the
    /// caller may take it ([`Held`]) and do the removal itself.
    Free,
    /// A proven holder took the removal and reports what it made of it: its answer is the
    /// caller's.
    Answered(Result<(), String>),
    /// Something holds the port but would not take an authenticated removal: whatever it is, it
    /// may be a launcher this process could not prove, and its manifest must be left to it.
    Held,
}

/// Hands the removal of `id` to the launcher already holding the account's instance, if one is
/// running and proves itself. The handshake is the single-instance one: the holder challenges
/// with its proof of the account's token, so no impostor is told what is being removed, and the
/// request is sent only with a proof of the same token, so another local program cannot have an
/// app removed (CWE-287, CWE-522).
pub fn forward_uninstall(id: &str) -> Forwarded {
    let (Some(ours), Some(token), Some(client_nonce)) = (ours(), token().filter(|t| !t.is_empty()), fresh_secret()) else {
        return Forwarded::Held;
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port()));
    forward_uninstall_to(addr, id, &ours, &token, &client_nonce)
}

/// [`forward_uninstall`]'s handshake against one address, so tests run it against a listener of
/// their own.
fn forward_uninstall_to(addr: SocketAddr, id: &str, ours: &Version, token: &str, client_nonce: &str) -> Forwarded {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else { return Forwarded::Free };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    if stream.write_all(format!("{HELLO}{ours} {client_nonce}\n").as_bytes()).is_err() {
        return Forwarded::Free;
    }
    let reply = read_message(&mut stream);
    // Only a holder that proves it knows the account's token is told what is being removed.
    let Some(challenge_rest) = std::str::from_utf8(&reply).ok().and_then(|r| r.trim().strip_prefix(CHALLENGE)) else {
        return Forwarded::Held;
    };
    if !server_proved(challenge_rest, token, client_nonce) {
        return Forwarded::Held;
    }
    let server_nonce = challenge_rest.split_whitespace().next().unwrap_or("");
    let proof = compute_proof("uninstall", token, client_nonce, server_nonce);
    if stream.write_all(format!("{UNINSTALL}{id} {proof}\n").as_bytes()).is_err() {
        return Forwarded::Answered(Err("the launcher closed the connection before the removal could be handed over".into()));
    }
    let answer = read_message_until(&mut stream, Instant::now() + UNINSTALL_WITHIN, MAX_ANSWER);
    let answer = String::from_utf8_lossy(&answer).trim().to_string();
    match answer.strip_prefix(UNINSTALLED) {
        Some("ok") => Forwarded::Answered(Ok(())),
        Some(error) if !error.is_empty() => Forwarded::Answered(Err(error.to_string())),
        _ if answer.starts_with(RUNNING.trim_end()) => {
            // A launcher from before removals were handed over answered the greeting as usual:
            // it cannot take this request, and the caller must not write beside it.
            Forwarded::Answered(Err("the launcher that is running is from an older version and can't take the removal; quit it and try again".into()))
        }
        _ => Forwarded::Answered(Err("the launcher took the removal but answered nothing to act on; try again".into())),
    }
}

/// Runs the registered removal for a handed-over request and answers the asking process. When
/// nothing is registered yet (the launcher is still starting), it waits a moment first.
fn answer_uninstall(stream: TcpStream, id: &str) {
    answer_uninstall_with(stream, id, wait_for_uninstall_handler().as_ref());
}

/// [`answer_uninstall`]'s answer for one handler, so tests run it with one of their own.
fn answer_uninstall_with(mut stream: TcpStream, id: &str, handler: Option<&Uninstall>) {
    let reply = match handler {
        None => format!("{UNINSTALLED}nothing here is registered to remove apps; the launcher may still be starting\n"),
        Some(handler) => match handler(id) {
            Ok(()) => format!("{UNINSTALLED}ok\n"),
            Err(error) => {
                log::warn!("the removal handed over by another process didn't finish: {error}");
                // One line: a reason that breaks the line would end the answer mid-way.
                format!("{UNINSTALLED}{}\n", error.replace(['\n', '\r'], " "))
            }
        },
    };
    let _ = stream.write_all(reply.chars().take(MAX_ANSWER).collect::<String>().as_bytes());
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// The registered removal, waiting a moment for it when the holder is still starting up.
fn wait_for_uninstall_handler() -> Option<Uninstall> {
    let deadline = Instant::now() + HANDLER_WITHIN;
    loop {
        if let Some(handler) = UNINSTALL_HANDLER.get() {
            return Some(handler.clone());
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Reads a line or message up to `\n` or EOF, at most [`MAX_GREETING`] bytes and one second in total.
fn read_message(stream: &mut TcpStream) -> Vec<u8> {
    read_message_until(stream, Instant::now() + Duration::from_secs(1), MAX_GREETING)
}

/// Reads a message up to `\n` or EOF, at most `max` bytes and `deadline` in total.
fn read_message_until(stream: &mut TcpStream, deadline: Instant, max: usize) -> Vec<u8> {
    let mut message = Vec::new();
    let mut byte = [0u8; 1];
    while message.len() < max {
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

    // Mutual challenge-response whenever the newcomer offers a nonce, whatever its version: a
    // newer one must prove itself before we grant Replace, and every newcomer needs our proof
    // before it exits in our favour. Without a token or a nonce nothing can be proven: answer,
    // and only show the window.
    let (Some(ours_token), Some(client_nonce), Some(server_nonce)) = (token.filter(|t| !t.is_empty()), client_nonce, fresh_secret()) else {
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

    if let Some(rest) = auth_text.strip_prefix(UNINSTALL) {
        // A headless removal handing itself over. Only a proof of the account's token counts:
        // anything else is dropped without an answer, like an unproven replace (CWE-287).
        let (id, proof) = rest.trim().split_once(' ')?;
        if !constant_time_eq(proof, &compute_proof("uninstall", ours_token, client_nonce, &server_nonce)) {
            return None;
        }
        // The removal runs on its own thread: it can take a while, and the listener must keep
        // answering the starts that arrive meanwhile. Nothing reaches the launcher's window: a
        // removal asked from Apps & features must not bring the launcher forward.
        if let Ok(mine) = stream.try_clone() {
            let id = id.to_string();
            std::thread::spawn(move || answer_uninstall(mine, &id));
        }
        return None;
    }

    let _ = stream.write_all(reply_running.as_bytes());

    if newer && constant_time_eq(client_proof, &expected_client_proof) { Some(Request::Replace) } else { Some(Request::Show) }
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
        Err(_) => return Ok(Answer::Unrelated),
    };

    if let Some(challenge_rest) = reply_str.strip_prefix(CHALLENGE) {
        let server_nonce = challenge_rest.split_whitespace().next().unwrap_or("");
        let (Some(client_nonce), Some(tok)) = (client_nonce.as_deref(), token) else {
            return Ok(Answer::Unrelated);
        };
        if !server_proved(challenge_rest, tok, client_nonce) {
            // Server failed to prove it knows the shared token (untrusted peer).
            // Do NOT disclose client proof; abort immediately, and don't step aside for it.
            return Ok(Answer::Unrelated);
        }

        // Server is authentic. Prove our identity to server:
        let client_proof = compute_proof("client", tok, client_nonce, server_nonce);
        let auth_msg = format!("{AUTH}{client_proof}\n");
        stream.write_all(auth_msg.as_bytes())?;

        let final_reply = read_message(&mut stream);
        return Ok(verdict(&final_reply, ours, true));
    }

    Ok(verdict(&reply, ours, false))
}

/// Reads the running launcher's answer. `proven` when it proved it holds this account's token:
/// only then is an answer that it is as new as we are a reason to exit.
fn verdict(reply: &[u8], ours: &Version, proven: bool) -> Answer {
    if reply.is_empty() {
        return Answer::Legacy;
    }
    let trimmed = std::str::from_utf8(reply).ok().map(str::trim);
    let running = trimmed.and_then(|r| r.strip_prefix(RUNNING)).and_then(version::parse);
    match running {
        Some(running) if version::is_newer(ours, &running) => Answer::TakeOver { proven },
        _ if proven => Answer::Defer,
        _ => Answer::Unrelated,
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
        assert_eq!(ask(addr, Some(&v("0.1.2")), Some(TOK)).ok(), Some(Answer::TakeOver { proven: true }));
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
        assert_eq!(ans, Answer::Unrelated, "a peer that fails the proof must not make us exit");
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
            assert_eq!(verdict(&reply, &v(version), false), Answer::TakeOver { proven: false });
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
        assert_eq!(verdict(&reply, &v("0.1.2"), false), Answer::TakeOver { proven: false });
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
        assert_eq!(ask(addr, Some(&v("0.6.2")), Some(TOK)).ok(), Some(Answer::TakeOver { proven: false }));
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
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.0\n", &v("0.1.2"), true), Answer::TakeOver { proven: true });
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.2\n", &v("0.1.2"), true), Answer::Defer);
        assert_eq!(verdict(b"artcraft-launcher:running unknown\n", &v("0.1.2"), true), Answer::Defer);
        assert_eq!(verdict(b"garbage\n", &v("0.1.2"), true), Answer::Defer);
        assert_eq!(verdict(b"", &v("0.1.2"), true), Answer::Legacy);
        // Without the proof, nothing makes us exit.
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.0\n", &v("0.1.2"), false), Answer::TakeOver { proven: false });
        assert_eq!(verdict(b"artcraft-launcher:running 0.1.2\n", &v("0.1.2"), false), Answer::Unrelated);
        assert_eq!(verdict(b"artcraft-launcher:running 9.9.9\n", &v("0.1.2"), false), Answer::Unrelated);
        assert_eq!(verdict(b"garbage\n", &v("0.1.2"), false), Answer::Unrelated);
        assert_eq!(verdict(b"", &v("0.1.2"), false), Answer::Legacy);
    }

    /// After a proven takeover stalls, the newcomer exits only if the port's holder proves itself
    /// again, and the probe never asks that holder to quit, even though it greets as newer.
    #[test]
    fn the_port_holder_is_proven_again_before_stepping_aside() {
        let (addr, rx) = primary("0.1.0");
        assert!(holder_proves_itself(addr, Some(&v("0.1.2")), Some(TOK)));
        assert_eq!(next(&rx), Request::Show, "the probe must not ask the running launcher to quit");

        // A holder of the same version (this build) is challenged too.
        let (addr, rx) = primary("0.1.2");
        assert!(holder_proves_itself(addr, Some(&v("0.1.2")), Some(TOK)));
        assert_eq!(next(&rx), Request::Show);

        // Something that took the port without the token can't keep the newcomer from starting.
        let (addr, _rx) = primary("0.1.0");
        assert!(!holder_proves_itself(addr, Some(&v("0.1.2")), Some("another-accounts-token")));
        let (addr, _rx) = primary("0.1.0");
        assert!(!holder_proves_itself(addr, Some(&v("0.1.2")), None));

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let imposter = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let _ = read_message(&mut s);
            let _ = s.write_all(b"artcraft-launcher:running 9.9.9\n");
        });
        assert!(!holder_proves_itself(addr, Some(&v("0.1.2")), Some(TOK)));
        imposter.join().unwrap();
    }

    /// A busy 0.8.1 to 0.14.x launcher that stayed after a takeover only challenges a newer
    /// newcomer. The probe greets as newer, so it is still proven, and it never sends a client proof.
    #[test]
    fn the_probe_proves_a_running_launcher_that_only_challenges_newer_starts() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let old = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let hello = String::from_utf8(read_message(&mut s)).unwrap();
            let mut parts = hello.strip_prefix(HELLO).unwrap().split_whitespace();
            let theirs = v(parts.next().unwrap());
            let Some(client_nonce) = parts.next().filter(|_| version::is_newer(&theirs, &v("0.1.0"))) else {
                let _ = s.write_all(b"artcraft-launcher:running 0.1.0\n");
                return Vec::new();
            };
            let server_nonce = fresh_secret().unwrap();
            let proof = compute_proof("server", TOK, client_nonce, &server_nonce);
            s.write_all(format!("{CHALLENGE}{server_nonce} {proof}\n").as_bytes()).unwrap();
            let auth = read_message(&mut s);
            let _ = s.write_all(b"artcraft-launcher:running 0.1.0\n");
            auth
        });
        assert!(holder_proves_itself(addr, Some(&v("0.1.2")), Some(TOK)));
        assert!(old.join().unwrap().is_empty(), "the probe must never answer the challenge");
    }

    /// A listener that answers like a newer launcher but never proves it holds our token (it
    /// skips the challenge) is unrelated: the newcomer doesn't exit for it (CWE-287).
    #[test]
    fn an_unproven_running_answer_does_not_make_a_start_exit() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let imposter = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let _ = read_message(&mut s);
            let _ = s.write_all(b"artcraft-launcher:running 9.9.9\n");
        });
        assert_eq!(ask(addr, Some(&v("0.1.2")), Some(TOK)).ok(), Some(Answer::Unrelated));
        imposter.join().unwrap();
    }

    /// A same-version start is challenged too, and steps aside only after the running launcher
    /// proved itself; it never gets to replace it.
    #[test]
    fn a_same_version_start_is_challenged_and_cannot_replace() {
        let (addr, rx) = primary("0.1.2");
        let nonce = fresh_secret().unwrap();
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(format!("{HELLO}0.1.2 {nonce}\n").as_bytes()).unwrap();
        let challenge = read_message(&mut stream);
        let rest = std::str::from_utf8(&challenge).unwrap().strip_prefix(CHALLENGE).unwrap().to_string();
        let mut parts = rest.split_whitespace();
        let (server_nonce, server_proof) = (parts.next().unwrap(), parts.next().unwrap());
        assert_eq!(server_proof, compute_proof("server", TOK, &nonce, server_nonce));
        stream.write_all(format!("{AUTH}{}\n", compute_proof("client", TOK, &nonce, server_nonce)).as_bytes()).unwrap();
        assert!(std::str::from_utf8(&read_message(&mut stream)).unwrap().starts_with(RUNNING));
        assert_eq!(next(&rx), Request::Show, "a same-version start must never replace the running one");
    }

    // Issue #271: a launcher inside WSL held the Windows launcher's port, so the Windows one
    // exited without a window.
    #[test]
    fn wsl_kernels_are_recognised() {
        assert!(is_wsl_kernel("5.15.167.4-microsoft-standard-WSL2\n"));
        assert!(is_wsl_kernel("6.6.36.6-microsoft-standard-WSL2+"));
        assert!(is_wsl_kernel("4.4.0-19041-Microsoft"));
        assert!(!is_wsl_kernel("6.8.0-45-generic"));
        assert!(!is_wsl_kernel("6.10.14-linuxkit"));
        assert!(!is_wsl_kernel(""));
    }

    #[test]
    fn wsl_launchers_keep_off_the_windows_launchers_port() {
        assert_eq!(port_for(false), PORT);
        assert_eq!(port_for(true), WSL_PORT);
        assert_ne!(port_for(true), port_for(false));
    }

    // The removal a headless uninstaller hands over (an app's Apps & features entry runs
    // `<launcher> --uninstall`): only a proven holder is told anything, only a proven request is
    // served, and the holder's answer is the asking process's own result.

    /// The one removal handler the test binary registers: what it answers is steered by the id,
    /// so tests of the serving side need no registration of their own (the first one stands, and
    /// a launcher's backend may have registered before it).
    fn ensure_test_handler() {
        set_uninstall_handler(Arc::new(|id| match id.strip_prefix("succeeds:") {
            Some(_) => Ok(()),
            _ => Err(format!("{id} isn't installed by the launcher's fast install")),
        }));
    }

    /// Serves one connection with the real server side, then drops its stream: a request the
    /// server does not answer reads EOF on the client, and an answer the removal thread writes
    /// reaches the client through the stream the server cloned for it.
    fn serve_one(listener: TcpListener) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = handle_incoming(&mut stream, Some(&v("0.20.4")), Some(TOK));
        })
    }

    /// A proven holder takes the handed-over removal and answers with what the removal made of
    /// it: its `ok` and its failure reason both reach the asking process as its own result.
    #[test]
    fn a_handed_removal_gets_the_holders_answer() {
        for (answer, expected) in
            [("ok", Forwarded::Answered(Ok(()))), ("the folder was left alone", Forwarded::Answered(Err("the folder was left alone".into())))]
        {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let hello = String::from_utf8(read_message(&mut s)).unwrap();
                let client_nonce = hello.strip_prefix(HELLO).unwrap().split_whitespace().nth(1).unwrap().to_string();
                let server_nonce = fresh_secret().unwrap();
                let proof = compute_proof("server", TOK, &client_nonce, &server_nonce);
                s.write_all(format!("{CHALLENGE}{server_nonce} {proof}\n").as_bytes()).unwrap();
                let request = String::from_utf8(read_message(&mut s)).unwrap();
                let (id, client_proof) = request.strip_prefix(UNINSTALL).unwrap().trim().split_once(' ').unwrap();
                assert_eq!(id, "photocraft");
                assert_eq!(client_proof, compute_proof("uninstall", TOK, &client_nonce, &server_nonce), "the request must prove the account's token");
                s.write_all(format!("{UNINSTALLED}{answer}\n").as_bytes()).unwrap();
            });
            let client_nonce = fresh_secret().unwrap();
            assert_eq!(forward_uninstall_to(addr, "photocraft", &v("0.20.4"), TOK, &client_nonce), expected);
            server.join().unwrap();
        }
    }

    /// A holder that does not prove it holds the account's token is told nothing, and an answer
    /// that does not prove itself is not trusted: the manifest must be left to whatever it is.
    #[test]
    fn a_removal_is_only_handed_to_a_proven_holder() {
        // No challenge at all: a launcher from before the handshake, or something answering like one.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let _ = read_message(&mut s);
            let _ = s.write_all(b"artcraft-launcher:running 0.20.4\n");
        });
        let client_nonce = fresh_secret().unwrap();
        assert_eq!(forward_uninstall_to(addr, "photocraft", &v("0.20.4"), TOK, &client_nonce), Forwarded::Held);
        server.join().unwrap();

        // A challenge that fails its own proof: an impostor on the port.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let hello = String::from_utf8(read_message(&mut s)).unwrap();
            let client_nonce = hello.strip_prefix(HELLO).unwrap().split_whitespace().nth(1).unwrap().to_string();
            let server_nonce = fresh_secret().unwrap();
            let proof = compute_proof("server", "not-the-token", &client_nonce, &server_nonce);
            s.write_all(format!("{CHALLENGE}{server_nonce} {proof}\n").as_bytes()).unwrap();
        });
        let client_nonce = fresh_secret().unwrap();
        assert_eq!(forward_uninstall_to(addr, "photocraft", &v("0.20.4"), TOK, &client_nonce), Forwarded::Held);
        server.join().unwrap();
    }

    /// Nothing answers the port: the instance is the caller's to take, and the removal is its own.
    #[test]
    fn a_removal_with_no_holder_is_the_callers_own() {
        // An ephemeral port nothing listens on refuses the connection rather than timing out.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let client_nonce = fresh_secret().unwrap();
        assert_eq!(forward_uninstall_to(addr, "photocraft", &v("0.20.4"), TOK, &client_nonce), Forwarded::Free);
    }

    /// A proven handed-over request runs the registered removal and answers with its result, and
    /// no window request reaches the holder; an unproven one gets no answer at all.
    #[test]
    fn the_holder_serves_a_proven_removal_and_drops_an_unproven_one() {
        ensure_test_handler();

        // Unproven: nothing comes back, and the removal is never started.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = serve_one(listener);
        let mut s = TcpStream::connect(addr).unwrap();
        let client_nonce = fresh_secret().unwrap();
        s.write_all(format!("{HELLO}0.20.4 {client_nonce}\n").as_bytes()).unwrap();
        let challenge = String::from_utf8(read_message(&mut s)).unwrap();
        assert!(challenge.starts_with(CHALLENGE), "{challenge}");
        s.write_all(format!("{UNINSTALL}photocraft not-the-proof\n").as_bytes()).unwrap();
        server.join().unwrap();
        let mut eof = Vec::new();
        assert_eq!(s.read_to_end(&mut eof).unwrap(), 0, "an unproven removal gets no answer");

        // Proven: the removal runs on its own thread and its answer comes back on the stream.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = serve_one(listener);
        let mut s = TcpStream::connect(addr).unwrap();
        let client_nonce = fresh_secret().unwrap();
        s.write_all(format!("{HELLO}0.20.4 {client_nonce}\n").as_bytes()).unwrap();
        let challenge = String::from_utf8(read_message(&mut s)).unwrap();
        let server_nonce = challenge.strip_prefix(CHALLENGE).unwrap().split_whitespace().next().unwrap().to_string();
        let proof = compute_proof("uninstall", TOK, &client_nonce, &server_nonce);
        s.write_all(format!("{UNINSTALL}succeeds:photocraft {proof}\n").as_bytes()).unwrap();
        let answer = String::from_utf8(read_message_until(&mut s, Instant::now() + Duration::from_secs(5), MAX_ANSWER)).unwrap();
        assert!(answer.starts_with(UNINSTALLED), "{answer}");
        server.join().unwrap();
    }

    /// A held instance serves handed-over removals while it holds the port and gives the port up
    /// when its work is done, so the next holder can take the instance.
    #[test]
    fn a_held_instance_serves_removals_and_then_gives_the_port_up() {
        ensure_test_handler();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = channel();
        let serve = std::thread::spawn(move || serve_until(listener, Some(v("0.20.4")), Some(TOK.to_string()), stopped));
        let client_nonce = fresh_secret().unwrap();
        let handed = forward_uninstall_to(addr, "succeeds:photocraft", &v("0.20.4"), TOK, &client_nonce);
        assert!(matches!(handed, Forwarded::Answered(_)), "{handed:?}");
        drop(stop);
        serve.join().unwrap();
        // The port is free again: something else can hold the instance now.
        TcpListener::bind(addr).unwrap();
    }

    /// The answer to a handed-over removal is one line: a reason with a line break in it cannot
    /// cut the answer short, and nothing beyond the answer's size is sent.
    #[test]
    fn the_answer_to_a_handed_removal_is_one_bounded_line() {
        fn answers_ok(_id: &str) -> Result<(), String> {
            Ok(())
        }
        fn answers_in_broken_lines(_id: &str) -> Result<(), String> {
            Err("the first line\nthe second\r\nthe third".into())
        }
        let answer = |handler: Uninstall, id: &str| {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let addr = listener.local_addr().unwrap();
            let id = id.to_string();
            let server = std::thread::spawn(move || {
                let (mine, _) = listener.accept().unwrap();
                answer_uninstall_with(mine, &id, Some(&handler));
            });
            let mut s = TcpStream::connect(addr).unwrap();
            server.join().unwrap();
            String::from_utf8(read_message_until(&mut s, Instant::now() + Duration::from_secs(2), MAX_ANSWER)).unwrap()
        };
        assert_eq!(answer(Arc::new(answers_ok), "photocraft"), format!("{UNINSTALLED}ok"));
        assert_eq!(answer(Arc::new(answers_in_broken_lines), "photocraft"), format!("{UNINSTALLED}the first line the second  the third"));
    }

    /// With nothing registered to remove apps, a handed-over request is answered saying so, not
    /// left hanging: the asking process reports it and the manifest is untouched.
    #[test]
    fn a_removal_with_no_registered_handler_is_answered_with_a_reason() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mine, _) = listener.accept().unwrap();
            answer_uninstall_with(mine, "photocraft", None);
        });
        let mut s = TcpStream::connect(addr).unwrap();
        server.join().unwrap();
        let answer = String::from_utf8(read_message_until(&mut s, Instant::now() + Duration::from_secs(2), MAX_ANSWER)).unwrap();
        assert_eq!(answer, format!("{UNINSTALLED}nothing here is registered to remove apps; the launcher may still be starting"));
    }
}
