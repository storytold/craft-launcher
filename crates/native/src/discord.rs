//! Discord Rich Presence: while a Craft App opened from the launcher is running, the person's
//! Discord profile shows it: the app's icon with the ArtCraft mark, its name and the time since
//! it opened. Off unless the person turns it on in Settings.
//!
//! Only the app's name and start time go to Discord, over Discord's local IPC socket (a named
//! pipe on Windows), and, when the person turns that on too, the name of the open file as the
//! app's window title shows it (Windows only): never a folder or anything about the person. With
//! nothing to show, the connection is closed, which also clears the status.

use std::io::{self, Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use artcraft_launcher_core::CraftApp;
use serde_json::{Value, json};

/// The Discord application ("ArtCraft") whose name heads the status. Its Application ID, from
/// <https://discord.com/developers/applications>. Empty switches the feature off.
pub const CLIENT_ID: &str = "1567046333360250911";

/// Where the icons Discord shows come from: the launcher's own copies of the apps' icons.
const ICONS: &str = "https://raw.githubusercontent.com/storytold/craft-launcher/main/assets";

/// Discord isn't running (or not yet): look again this often while there is something to show.
const RETRY: Duration = Duration::from_secs(20);

/// How often to look at the app's window title for the open file.
const TITLE_EVERY: Duration = Duration::from_secs(5);

/// Discord's limit for the line under the app's name.
#[cfg(any(windows, test))]
const MAX_STATE: usize = 128;

/// Discord's replies are a few hundred bytes; anything far larger is not Discord.
const MAX_FRAME: usize = 64 * 1024;

/// Discord IPC opcodes: the hello, a command or its answer, Discord hanging up, and a ping and
/// its pong (sent by either side).
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

/// Frames to read past (pings, anything unrelated) before giving up on Discord's answer.
const MAX_SKIPPED: usize = 8;

/// What the backend tells the worker thread.
enum Msg {
    Enable { on: bool, files: bool },
    Started { token: u64, app: &'static CraftApp, start: i64 },
    Opened { token: u64, pid: u32 },
    Exited { token: u64 },
}

/// An app opened from the launcher that hasn't quit yet.
struct Running {
    token: u64,
    app: &'static CraftApp,
    start: i64,
    /// Its process, once started.
    pid: Option<u32>,
}

/// The handle the backend keeps; cheap to clone into a launched app's exit watcher.
#[derive(Clone)]
pub struct Presence {
    tx: Option<Sender<Msg>>,
    next: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Presence {
    /// Starts the worker thread (nothing connects until the setting is on and an app runs).
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = (!CLIENT_ID.is_empty())
            .then(|| std::thread::Builder::new().name("discord-presence".into()).spawn(move || run(&rx)).ok().map(|_| tx))
            .flatten();
        Self { tx, next: std::sync::Arc::default() }
    }

    /// The person's settings: show the running app at all, and with it the open file's name.
    pub fn set_enabled(&self, on: bool, files: bool) {
        self.send(Msg::Enable { on, files });
    }

    /// `app` is being opened; the token goes to [`Self::opened`] and [`Self::exited`].
    pub fn started(&self, app: &'static CraftApp) -> u64 {
        let token = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.send(Msg::Started { token, app, start: crate::now() });
        token
    }

    /// The app is running as process `pid`, whose window title names the open file.
    pub fn opened(&self, token: u64, pid: u32) {
        self.send(Msg::Opened { token, pid });
    }

    /// The app opened under `token` has quit, or never started.
    pub fn exited(&self, token: u64) {
        self.send(Msg::Exited { token });
    }

    /// Hands `msg` to the worker; nothing happens when there is none (no [`CLIENT_ID`]).
    fn send(&self, msg: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg);
        }
    }
}

impl Default for Presence {
    fn default() -> Self {
        Self::new()
    }
}

/// The worker: keeps Discord showing the most recently opened app that is still running.
fn run(rx: &Receiver<Msg>) {
    let mut enabled = false;
    let mut files = false;
    let mut running: Vec<Running> = Vec::new();
    let mut conn: Option<Box<dyn Pipe>> = None;
    let mut shown: Option<Value> = None;
    // Tells Discord's answer to each request apart from anything else it sends.
    let mut nonce: u64 = 0;
    loop {
        let top = enabled.then(|| running.last()).flatten();
        let want = top.map(|r| activity(r.app, r.start, files.then(|| r.pid.and_then(|pid| open_file(pid, r.app.name))).flatten().as_deref()));
        if want.is_none() {
            // Closing the connection clears the status.
            conn = None;
            shown = None;
        } else if want != shown {
            if conn.is_none() {
                conn = connect();
            }
            if let Some(c) = conn.as_mut() {
                nonce = nonce.wrapping_add(1);
                match set_activity(c.as_mut(), want.as_ref(), &nonce.to_string()) {
                    Ok(()) => shown.clone_from(&want),
                    Err(e) => {
                        log::info!("Discord presence: {e}");
                        conn = None;
                        shown = None;
                    }
                }
            }
        }
        // Look again later: for Discord when it isn't reachable, for a new title when files show,
        // and to check that Discord is still there while the status stays the same.
        let wait = match (&want, &conn) {
            (Some(_), None) => Some(RETRY),
            (Some(_), Some(_)) if files && cfg!(windows) => Some(TITLE_EVERY),
            (Some(_), Some(_)) => Some(RETRY),
            _ => None,
        };
        let msg = if let Some(wait) = wait {
            match rx.recv_timeout(wait) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    // Discord quitting or restarting closes the connection without a word; a ping
                    // finds out, and the next pass connects again and shows the status anew.
                    if want.is_some()
                        && want == shown
                        && let Some(c) = conn.as_mut()
                        && let Err(e) = ping(c.as_mut())
                    {
                        log::info!("Discord presence: {e}");
                        conn = None;
                        shown = None;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(m) => m,
                Err(_) => return,
            }
        };
        match msg {
            Msg::Enable { on, files: f } => (enabled, files) = (on, f),
            Msg::Started { token, app, start } => running.push(Running { token, app, start, pid: None }),
            Msg::Opened { token, pid } => {
                if let Some(r) = running.iter_mut().find(|r| r.token == token) {
                    r.pid = Some(pid);
                }
            }
            Msg::Exited { token } => running.retain(|r| r.token != token),
        }
    }
}

/// What Discord shows for `app`, opened at Unix second `start`, with `file` open.
fn activity(app: &CraftApp, start: i64, file: Option<&str>) -> Value {
    let mut a = json!({
        "details": app.name,
        "timestamps": { "start": start },
        "assets": {
            "large_image": format!("{ICONS}/app-icons/{}.png", app.id),
            "large_text": app.name,
            "small_image": format!("{ICONS}/app-icon/hicolor/128x128/apps/ai.storyteller.artcraft-launcher.png"),
            "small_text": "ArtCraft",
        },
    });
    if let (Some(file), Some(a)) = (file, a.as_object_mut()) {
        a.insert("state".into(), file.into());
    }
    a
}

/// The file a Craft App's window title names: `report.pdf — PdfCraft` (or with ` - `) gives
/// `report.pdf`, without an unsaved-changes `*`. `None` for a title that is only the app's name,
/// or not in that shape at all, so nothing else a window says reaches Discord. Only Windows reads
/// window titles (see [`open_file`]).
#[cfg(any(windows, test))]
fn file_from_title(title: &str, app: &str) -> Option<String> {
    let file = [" — ", " – ", " - "].iter().find_map(|sep| title.strip_suffix(app)?.strip_suffix(sep))?;
    // Only the last part of a path, so a folder never reaches Discord.
    let file = file.rsplit(['\\', '/']).next().unwrap_or(file);
    let file = file.trim().trim_start_matches('*').trim_end_matches(['*', '•']).trim();
    // Discord wants 2 to 128 characters here.
    (file.chars().count() >= 2).then(|| file.chars().take(MAX_STATE).collect())
}

/// The file process `pid`'s visible window names.
#[cfg(windows)]
fn open_file(pid: u32, app: &str) -> Option<String> {
    let mut found = None;
    let _ = winsafe::EnumWindows(|hwnd: winsafe::HWND| {
        if hwnd.GetWindowThreadProcessId().1 == pid && hwnd.IsWindowVisible() {
            found = hwnd.GetWindowText().ok().and_then(|t| file_from_title(&t, app));
        }
        found.is_none()
    });
    found
}

/// Elsewhere a window's title isn't readable without asking for more than a launcher should.
#[cfg(not(windows))]
fn open_file(_pid: u32, _app: &str) -> Option<String> {
    None
}

/// Discord's IPC connection: a named pipe on Windows, a Unix socket elsewhere.
trait Pipe: Read + Write + Send {}
impl<T: Read + Write + Send> Pipe for T {}

/// Opens Discord's IPC socket and says hello. `None` when Discord isn't running.
fn connect() -> Option<Box<dyn Pipe>> {
    let mut pipe = (0..10).find_map(open)?;
    write_frame(&mut pipe, OP_HANDSHAKE, &json!({ "v": 1, "client_id": CLIENT_ID })).ok()?;
    match read_frame(&mut pipe) {
        Ok((OP_FRAME, _)) => Some(pipe),
        Ok((_, reply)) => {
            log::warn!("Discord refused the presence connection: {reply}");
            None
        }
        Err(e) => {
            log::info!("Discord presence: {e}");
            None
        }
    }
}

/// Discord's pipe number `n` (Discord takes the first free one of 0 to 9). Any program can serve
/// a pipe by that name, so it is opened at identification level: the server can tell who
/// connected but can never act as the launcher.
#[cfg(windows)]
fn open(n: u8) -> Option<Box<dyn Pipe>> {
    use std::os::windows::fs::OpenOptionsExt;
    // SECURITY_IDENTIFICATION (std adds SECURITY_SQOS_PRESENT).
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    let pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(format!(r"\\.\pipe\discord-ipc-{n}"))
        .ok()?;
    Some(Box::new(pipe))
}

/// Discord's socket number `n`, in the folders Discord, its Flatpak and its Snap put it in.
/// Reads time out, so a socket that never answers can't hold the worker up.
#[cfg(unix)]
fn open(n: u8) -> Option<Box<dyn Pipe>> {
    let roots = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"].iter().filter_map(std::env::var_os).map(std::path::PathBuf::from);
    let roots = roots.chain(std::iter::once(std::path::PathBuf::from("/tmp")));
    // Discord itself, or Discord installed as a Flatpak or a Snap.
    roots.flat_map(|root| ["", "app/com.discordapp.Discord", "snap.discord"].map(|sub| root.join(sub).join(format!("discord-ipc-{n}")))).find_map(
        |path| {
            let stream = std::os::unix::net::UnixStream::connect(path).ok()?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
            Some(Box::new(stream) as Box<dyn Pipe>)
        },
    )
}

/// No Discord desktop app runs anywhere else.
#[cfg(not(any(windows, unix)))]
fn open(_n: u8) -> Option<Box<dyn Pipe>> {
    None
}

/// Sets (or, with `None`, clears) the status, and waits for Discord's answer to this request (the
/// one carrying `nonce`). An error answer is an error too: dropping the connection makes the
/// worker try again.
fn set_activity(pipe: &mut dyn Pipe, activity: Option<&Value>, nonce: &str) -> io::Result<()> {
    let body = json!({ "cmd": "SET_ACTIVITY", "args": { "pid": std::process::id(), "activity": activity }, "nonce": nonce });
    let reply = exchange(pipe, OP_FRAME, &body, |op, reply| op == OP_FRAME && reply.get("nonce").and_then(Value::as_str) == Some(nonce))?;
    if reply.get("evt").and_then(Value::as_str) == Some("ERROR") {
        return Err(io::Error::other(format!("Discord didn't take the presence: {reply}")));
    }
    Ok(())
}

/// Whether Discord still answers: a ping and its pong.
fn ping(pipe: &mut dyn Pipe) -> io::Result<()> {
    exchange(pipe, OP_PING, &json!({}), |op, _| op == OP_PONG).map(|_| ())
}

/// Sends a frame, then reads until the answer `is_answer` picks out. On the way it answers
/// Discord's pings and skips anything else (up to [`MAX_SKIPPED`] frames); Discord hanging up is
/// an error.
fn exchange(pipe: &mut dyn Pipe, op: u32, body: &Value, is_answer: impl Fn(u32, &Value) -> bool) -> io::Result<Value> {
    write_frame(pipe, op, body)?;
    for _ in 0..MAX_SKIPPED {
        match read_frame(pipe)? {
            (OP_CLOSE, reply) => return Err(io::Error::other(format!("Discord closed the connection: {reply}"))),
            (op, reply) if is_answer(op, &reply) => return Ok(reply),
            (OP_PING, ping) => write_frame(pipe, OP_PONG, &ping)?,
            _ => {}
        }
    }
    Err(io::Error::other("Discord didn't answer"))
}

/// A frame: opcode and body length (little-endian `u32`s), then the JSON body.
fn write_frame(w: &mut (impl Write + ?Sized), op: u32, body: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(body)?;
    let len = u32::try_from(body.len()).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(body.len().saturating_add(8));
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&body);
    w.write_all(&frame)?;
    w.flush()
}

/// Reads one frame. Discord's answer is hostile input like anything else: a body over
/// [`MAX_FRAME`], a short read or JSON that doesn't parse is an error, never a panic.
fn read_frame(r: &mut (impl Read + ?Sized)) -> io::Result<(u32, Value)> {
    let mut op = [0; 4];
    let mut len = [0; 4];
    r.read_exact(&mut op)?;
    r.read_exact(&mut len)?;
    let len = usize::try_from(u32::from_le_bytes(len)).map_err(io::Error::other)?;
    if len > MAX_FRAME {
        return Err(io::Error::other(format!("a {len}-byte reply is not Discord's")));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok((u32::from_le_bytes(op), serde_json::from_slice(&body)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, OP_FRAME, &json!({ "cmd": "SET_ACTIVITY" })).unwrap();
        assert_eq!(&buf[..4], &1u32.to_le_bytes());
        let (op, body) = read_frame(&mut buf.as_slice()).unwrap();
        assert_eq!(op, OP_FRAME);
        assert_eq!(body["cmd"], "SET_ACTIVITY");
    }

    /// A connection whose replies are `replies`, and which keeps what the launcher sent.
    struct FakeDiscord {
        replies: io::Cursor<Vec<u8>>,
        sent: Vec<u8>,
    }

    impl FakeDiscord {
        fn replying(frames: &[(u32, Value)]) -> Self {
            let mut replies = Vec::new();
            for (op, body) in frames {
                write_frame(&mut replies, *op, body).unwrap();
            }
            Self { replies: io::Cursor::new(replies), sent: Vec::new() }
        }

        /// The opcodes of the frames the launcher sent.
        fn sent_ops(&self) -> Vec<u32> {
            let mut sent = self.sent.as_slice();
            std::iter::from_fn(|| read_frame(&mut sent).ok().map(|(op, _)| op)).collect()
        }
    }

    impl Read for FakeDiscord {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.replies.read(buf)
        }
    }

    impl Write for FakeDiscord {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.sent.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_status_waits_for_its_own_answer_and_answers_pings() {
        let mut discord = FakeDiscord::replying(&[
            (OP_PING, json!({ "n": 1 })),
            (OP_FRAME, json!({ "evt": "ERROR", "nonce": "6" })),
            (OP_FRAME, json!({ "cmd": "SET_ACTIVITY", "nonce": "7" })),
        ]);
        set_activity(&mut discord, None, "7").unwrap();
        assert_eq!(discord.sent_ops(), [OP_FRAME, OP_PONG], "the request, then a pong for the ping");
    }

    #[test]
    fn an_error_answer_hanging_up_or_silence_is_an_error() {
        let error = [(OP_FRAME, json!({ "evt": "ERROR", "nonce": "7" }))];
        assert!(set_activity(&mut FakeDiscord::replying(&error), None, "7").is_err());
        let close = [(OP_CLOSE, json!({ "code": 4000 }))];
        assert!(set_activity(&mut FakeDiscord::replying(&close), None, "7").is_err());
        let unrelated = vec![(OP_FRAME, json!({ "nonce": "other" })); MAX_SKIPPED];
        assert!(set_activity(&mut FakeDiscord::replying(&unrelated), None, "7").is_err());
        assert!(ping(&mut FakeDiscord::replying(&[])).is_err(), "no pong: Discord is gone");
        assert!(ping(&mut FakeDiscord::replying(&[(OP_PONG, json!({}))])).is_ok());
    }

    #[test]
    fn oversized_or_short_frames_are_errors() {
        let mut huge = Vec::new();
        huge.extend_from_slice(&1u32.to_le_bytes());
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame(&mut huge.as_slice()).is_err());
        assert!(read_frame(&mut [1u8, 0, 0].as_slice()).is_err());
    }

    #[test]
    fn activity_shows_the_app_and_nothing_else() {
        let app = CraftApp::by_id("photocraft").unwrap();
        let a = activity(app, 1_700_000_000, None);
        assert_eq!(a["details"], "PhotoCraft");
        assert_eq!(a["timestamps"]["start"], 1_700_000_000);
        assert_eq!(a["assets"]["large_image"], format!("{ICONS}/app-icons/photocraft.png"));
        assert!(a.get("state").is_none());
        assert_eq!(activity(app, 0, Some("cat.png"))["state"], "cat.png");
    }

    #[test]
    fn file_names_come_only_from_titles_in_the_apps_shape() {
        assert_eq!(file_from_title("images.pdf — PdfCraft", "PdfCraft").as_deref(), Some("images.pdf"));
        assert_eq!(file_from_title("*Untitled-1 - PhotoCraft", "PhotoCraft").as_deref(), Some("Untitled-1"));
        assert_eq!(file_from_title("plan.svg • — VectorCraft", "VectorCraft").as_deref(), Some("plan.svg"));
        assert_eq!(file_from_title(r"C:\Users\me\plan.pdf — PdfCraft", "PdfCraft").as_deref(), Some("plan.pdf"));
        assert_eq!(file_from_title("/home/me/plan.pdf — PdfCraft", "PdfCraft").as_deref(), Some("plan.pdf"));
        assert_eq!(file_from_title("PdfCraft", "PdfCraft"), None);
        assert_eq!(file_from_title("Open file", "PdfCraft"), None);
        assert_eq!(file_from_title("x — PdfCraft", "PdfCraft"), None);
        let long = format!("{} — PdfCraft", "a".repeat(300));
        assert_eq!(file_from_title(&long, "PdfCraft").map(|f| f.chars().count()), Some(MAX_STATE));
    }

    #[test]
    fn every_app_has_an_icon_for_discord() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        assert!(dir.join("app-icon/hicolor/128x128/apps/ai.storyteller.artcraft-launcher.png").is_file());
        for app in artcraft_launcher_core::catalog::SUITE {
            assert!(dir.join(format!("app-icons/{}.png", app.id)).is_file(), "{}", app.id);
        }
    }
}
