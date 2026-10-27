//! One password prompt for a run of system installs.
//!
//! `pkexec` authenticates every call on its own, so updating several apps at once asked for the
//! password once per app. The first call instead starts this program again as root, through a
//! single `pkexec`, and keeps it running as a helper. Later installs and removals go through
//! that helper over a private Unix socket, until it has been idle for fifteen minutes or the
//! launcher exits.
//!
//! The socket is inside a root-visible, user-private temporary folder. The launcher checks the
//! connecting process's pid against the helper it started, so another process of the same user
//! cannot use the helper without its own permission prompt.
//!
//! The helper runs only a fixed set of package-manager commands. Install targets are copied to a
//! root-owned staging folder first and checked against the release's SHA-256, and removal names
//! are limited to Craft app packages. A package can still run its own maintainer scripts as root
//! while it is installed; that is the point of installing it, and the user approved the install.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitCode, Stdio};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use artcraft_launcher_core::SUITE;
use artcraft_launcher_core::protocol::CANCELLED;

use super::{SYSTEM_BIN_DIRS, find_in, pkexec, process, with_absolute_program};

/// The argument that turns this program into the privileged helper, with the socket path.
pub const FLAG: &str = "--artcraft-root-helper=";

/// How long the helper waits for another install before it exits. Every request restarts it.
const IDLE: Duration = Duration::from_secs(15 * 60);

/// How long the desktop may take to collect the password before the first call gives up.
const START_LIMIT: Duration = Duration::from_secs(5 * 60);

/// The helper's first line once it is running as root with the desktop's permission. The number
/// is the protocol version, so a launcher never talks to a helper from another build.
const READY: &str = "READY 1\n";

/// The helper's executable was replaced (a package upgrade) after this process started; the
/// privileged helper cannot be started from it, so the caller falls back to a direct prompt.
const REPLACED: &str = "this program was replaced on disk";

const SOCKET_NAME: &str = "helper.sock";
const MAX_TARGETS: usize = 64;
const MAX_TARGET: usize = 4096;
const MAX_PAYLOAD: usize = 1 << 20;
const MAX_MESSAGE_CHARS: usize = 1000;
const HASH_LENGTH: usize = 32;

// Every command the helper may run, with the fixed arguments before the targets. The
// launcher's package commands are these same constants, so the two can't drift apart.
// `DPkg::Lock::Timeout=60` makes apt wait for another package tool (a software centre,
// automatic updates) to let go of its lock instead of failing at once.
pub const APT_INSTALL: &[&str] = &["apt-get", "-o", "DPkg::Lock::Timeout=60", "install", "-y", "--allow-downgrades"];
pub const APT_REMOVE: &[&str] = &["apt-get", "-o", "DPkg::Lock::Timeout=60", "remove", "-y"];
pub const DPKG_REMOVE: &[&str] = &["dpkg", "-r"];
pub const DNF_INSTALL: &[&str] = &["dnf", "install", "-y"];
pub const DNF_REMOVE: &[&str] = &["dnf", "remove", "-y"];
pub const ZYPPER_INSTALL: &[&str] = &["zypper", "--non-interactive", "install", "--allow-unsigned-rpm"];
pub const RPM_INSTALL: &[&str] = &["rpm", "-U", "--replacepkgs"];
pub const RPM_REMOVE: &[&str] = &["rpm", "-e"];
pub const PACMAN_INSTALL: &[&str] = &["pacman", "-U", "--noconfirm"];
pub const PACMAN_REMOVE: &[&str] = &["pacman", "-R", "--noconfirm"];
pub const FLATPAK_SYSTEM_UNINSTALL: &[&str] = &["flatpak", "uninstall", "--system", "-y", "--noninteractive"];

/// A command the helper may run.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootCommand {
    AptInstall = 0,
    AptRemove = 1,
    DpkgRemove = 2,
    DnfInstall = 3,
    DnfRemove = 4,
    ZypperInstall = 5,
    RpmInstall = 6,
    RpmRemove = 7,
    PacmanInstall = 8,
    PacmanRemove = 9,
    FlatpakSystemUninstall = 10,
}

impl RootCommand {
    const ALL: [Self; 11] = [
        Self::AptInstall,
        Self::AptRemove,
        Self::DpkgRemove,
        Self::DnfInstall,
        Self::DnfRemove,
        Self::ZypperInstall,
        Self::RpmInstall,
        Self::RpmRemove,
        Self::PacmanInstall,
        Self::PacmanRemove,
        Self::FlatpakSystemUninstall,
    ];

    fn argv(self) -> &'static [&'static str] {
        match self {
            Self::AptInstall => APT_INSTALL,
            Self::AptRemove => APT_REMOVE,
            Self::DpkgRemove => DPKG_REMOVE,
            Self::DnfInstall => DNF_INSTALL,
            Self::DnfRemove => DNF_REMOVE,
            Self::ZypperInstall => ZYPPER_INSTALL,
            Self::RpmInstall => RPM_INSTALL,
            Self::RpmRemove => RPM_REMOVE,
            Self::PacmanInstall => PACMAN_INSTALL,
            Self::PacmanRemove => PACMAN_REMOVE,
            Self::FlatpakSystemUninstall => FLATPAK_SYSTEM_UNINSTALL,
        }
    }

    fn targets(self) -> Targets {
        match self {
            Self::AptInstall | Self::DnfInstall | Self::ZypperInstall | Self::RpmInstall | Self::PacmanInstall => Targets::Files,
            Self::AptRemove | Self::DpkgRemove | Self::DnfRemove | Self::RpmRemove | Self::PacmanRemove | Self::FlatpakSystemUninstall => {
                Targets::Names
            }
        }
    }

    fn from_index(index: u8) -> Option<Self> {
        Self::ALL.get(usize::from(index)).copied()
    }

    /// The command whose fixed arguments start `args`, with the rest as its targets. The
    /// longest match wins, so no shorter command can claim a longer one's call.
    fn of<'a>(args: &'a [&'a OsStr]) -> Option<(Self, Vec<&'a OsStr>)> {
        let mut found: Option<(Self, Vec<&OsStr>)> = None;
        for command in Self::ALL {
            let argv = command.argv();
            if args.len() <= argv.len() {
                continue;
            }
            if argv.iter().zip(args).all(|(expected, actual)| actual.as_bytes() == expected.as_bytes())
                && found.as_ref().is_none_or(|(best, _)| argv.len() > best.argv().len())
            {
                found = Some((command, args[argv.len()..].to_vec()));
            }
        }
        found
    }
}

/// What a command puts after its fixed arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Targets {
    /// Downloaded package files.
    Files,
    /// Package names.
    Names,
}

/// Runs the privileged helper when this program was started as one. The launcher calls this
/// before it sets anything else up; every other start has no [`FLAG`] and returns `None`.
pub fn run_if_requested() -> Option<ExitCode> {
    let socket = std::env::args_os().find_map(|arg| arg.to_str().and_then(|text| text.strip_prefix(FLAG)).map(PathBuf::from))?;
    Some(ExitCode::from(serve(&socket)))
}

/// The checksum the launcher verified `path` against before installing it. Dropping the guard
/// forgets it again.
pub(crate) struct ExpectedHash {
    path: PathBuf,
}

static EXPECTED: LazyLock<Mutex<HashMap<PathBuf, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Remembers the release checksum for `path` until the returned guard is dropped.
pub(crate) fn expect_hash(path: &Path, sha: &str) -> ExpectedHash {
    if let Ok(mut expected) = EXPECTED.lock() {
        expected.insert(path.to_path_buf(), sha.to_string());
    }
    ExpectedHash { path: path.to_path_buf() }
}

impl Drop for ExpectedHash {
    fn drop(&mut self) {
        if let Ok(mut expected) = EXPECTED.lock() {
            expected.remove(&self.path);
        }
    }
}

fn expected_hash(target: &OsStr) -> Option<[u8; HASH_LENGTH]> {
    let expected = EXPECTED.lock().ok()?;
    parse_hash(expected.get(Path::new(target))?)
}

/// The helper's side: connect to the waiting launcher and serve requests until the launcher goes
/// away or nothing has been asked for a while.
fn serve(socket: &Path) -> u8 {
    // `pkexec` sets this to the user who approved the prompt. Without it this wasn't started
    // by a permission prompt, so it is not allowed to install anything.
    let Some(uid) = std::env::var("PKEXEC_UID").ok().and_then(|uid| uid.parse::<u32>().ok()) else {
        return 1;
    };
    let Ok(mut stream) = UnixStream::connect(socket) else { return 1 };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
    if stream.write_all(READY.as_bytes()).and_then(|()| stream.flush()).is_err() {
        return 1;
    }
    let _ = stream.set_read_timeout(Some(IDLE));
    loop {
        let request = match read_request(&mut stream) {
            Ok(request) => request,
            Err(()) => return 0,
        };
        let outcome = execute(request, uid);
        if write_response(&mut stream, &outcome).is_err() {
            return 1;
        }
    }
}

struct Request {
    command: RootCommand,
    targets: Vec<(OsString, Option<[u8; HASH_LENGTH]>)>,
}

fn read_request(input: &mut impl Read) -> Result<Request, ()> {
    let mut header = [0u8; 4];
    input.read_exact(&mut header).map_err(|_| ())?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_PAYLOAD {
        return Err(());
    }
    let mut payload = vec![0u8; length];
    input.read_exact(&mut payload).map_err(|_| ())?;
    decode(&payload).map_err(|_| ())
}

fn decode(payload: &[u8]) -> Result<Request, String> {
    if payload.len() > MAX_PAYLOAD {
        return Err("the install request is too large".into());
    }
    let (&index, rest) = payload.split_first().ok_or("empty request")?;
    let command = RootCommand::from_index(index).ok_or("unknown command")?;
    let mut at = 0;
    let count = take_u32(rest, &mut at)?;
    if count == 0 || count > MAX_TARGETS as u32 {
        return Err("wrong number of targets".into());
    }
    let mut targets = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let length = take_u32(rest, &mut at)? as usize;
        if length == 0 || length > MAX_TARGET {
            return Err("invalid target".into());
        }
        let end = at.checked_add(length).ok_or("truncated request")?;
        let bytes = rest.get(at..end).ok_or("truncated request")?;
        at = end;
        let flag = *rest.get(at).ok_or("truncated request")?;
        at += 1;
        let expected = match flag {
            0 => None,
            1 => {
                let end = at.checked_add(HASH_LENGTH).ok_or("truncated checksum")?;
                let bytes = rest.get(at..end).ok_or("truncated checksum")?;
                at = end;
                Some(<[u8; HASH_LENGTH]>::try_from(bytes).map_err(|_| "truncated checksum")?)
            }
            _ => return Err("unknown checksum flag".into()),
        };
        targets.push((OsString::from_vec(bytes.to_vec()), expected));
    }
    if at != rest.len() {
        return Err("trailing request data".into());
    }
    Ok(Request { command, targets })
}

fn take_u32(bytes: &[u8], at: &mut usize) -> Result<u32, String> {
    let end = at.checked_add(4).ok_or("truncated request")?;
    let part = bytes.get(*at..end).ok_or("truncated request")?;
    let value = <[u8; 4]>::try_from(part).map_err(|_| "truncated request")?;
    *at = end;
    Ok(u32::from_be_bytes(value))
}

fn execute(request: Request, uid: u32) -> Result<(), String> {
    validate(request.command, &request.targets, Some(uid))?;
    let argv = request.command.argv();
    let program = find_in(&SYSTEM_BIN_DIRS, OsStr::new(argv[0])).ok_or_else(|| format!("{} isn't installed", argv[0]))?;
    let (targets, _staging) = stage_targets(request.command, &request.targets, uid)?;
    let status = process::command(program)
        .args(&argv[1..])
        .args(&targets)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("couldn't start {} ({e})", argv[0]))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(format!("the system installer stopped with error {code}")),
        None => Err("the system installer was interrupted".into()),
    }
}

/// Copies the packages the installer is about to read into a root-owned folder, checking each
/// copy against the release checksum the launcher verified. The package manager then reads a
/// file no other process can change while it runs; names are passed through unchanged.
fn stage_targets(
    command: RootCommand,
    targets: &[(OsString, Option<[u8; HASH_LENGTH]>)],
    uid: u32,
) -> Result<(Vec<OsString>, Option<tempfile::TempDir>), String> {
    if command.targets() != Targets::Files {
        return Ok((targets.iter().map(|(target, _)| target.clone()).collect(), None));
    }
    let staging = tempfile::Builder::new().prefix("artcraft-pkg-").tempdir().map_err(|error| format!("couldn't prepare the package ({error})"))?;
    let mut copies = Vec::with_capacity(targets.len());
    for (index, (target, expected)) in targets.iter().enumerate() {
        let source_path = Path::new(target);
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source_path)
            .map_err(|error| format!("couldn't read {} ({error})", source_path.display()))?;
        let metadata = source.metadata().map_err(|error| format!("couldn't read {} ({error})", source_path.display()))?;
        if !metadata.file_type().is_file() || metadata.uid() != uid {
            return Err(format!("{} isn't a downloaded package of the user who approved the prompt", source_path.display()));
        }
        let name = source_path.file_name().ok_or_else(|| format!("{} isn't a package file", source_path.display()))?;
        let mut copy_name = OsString::from(format!("{index}-"));
        copy_name.push(name);
        let copy_path = staging.path().join(copy_name);
        let mut copy = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&copy_path)
            .map_err(|error| format!("couldn't prepare {} ({error})", copy_path.display()))?;
        std::io::copy(&mut source, &mut copy).map_err(|error| format!("couldn't copy {} ({error})", source_path.display()))?;
        copy.flush().map_err(|error| format!("couldn't save {} ({error})", copy_path.display()))?;
        drop(copy);
        if let Some(expected) = expected {
            let actual = crate::http::sha256_file(&copy_path)?;
            if actual != crate::http::hex(expected) {
                return Err(format!("{} doesn't match the release checksum", source_path.display()));
            }
        }
        copies.push(copy_path.into_os_string());
    }
    Ok((copies, Some(staging)))
}

fn write_response(output: &mut impl Write, outcome: &Result<(), String>) -> std::io::Result<()> {
    let (status, message) = match outcome {
        Ok(()) => (0u8, String::new()),
        Err(error) => (1u8, error.chars().take(MAX_MESSAGE_CHARS).collect()),
    };
    let message = message.as_bytes();
    let length = u32::try_from(message.len()).unwrap_or(u32::MAX).to_be_bytes();
    output.write_all(&[status])?;
    output.write_all(&length)?;
    output.write_all(message)?;
    output.flush()
}

/// Whether `targets` is something `command` may run, checked without root before a prompt is
/// shown. The helper repeats the check with the file owner's user id.
fn validate(command: RootCommand, targets: &[(OsString, Option<[u8; HASH_LENGTH]>)], uid: Option<u32>) -> Result<(), String> {
    if targets.is_empty() || targets.len() > MAX_TARGETS {
        return Err("the install request has the wrong number of files".into());
    }
    for (target, _) in targets {
        match command.targets() {
            Targets::Files => check_file(target, uid)?,
            Targets::Names => check_name(target)?,
        }
    }
    Ok(())
}

fn check_file(target: &OsStr, uid: Option<u32>) -> Result<(), String> {
    let path = Path::new(target);
    if !path.is_absolute() {
        return Err("a package to install must be an absolute path".into());
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| format!("{} can't be installed", path.display()))?;
    if !metadata.file_type().is_file() || uid.is_some_and(|uid| metadata.uid() != uid) {
        return Err(format!("{} isn't a downloaded package of the user who approved the prompt", path.display()));
    }
    Ok(())
}

fn check_name(target: &OsStr) -> Result<(), String> {
    let Some(name) = target.to_str() else {
        return Err("a package name must be text".into());
    };
    let known = SUITE.iter().any(|app| {
        std::iter::once(app.id)
            .chain(app.former_ids.iter().copied())
            .any(|id| name == id || name == format!("{id}-bin") || name == format!("{id}-git") || name == format!("ai.storyteller.{id}"))
    });
    if known { Ok(()) } else { Err(format!("{name} isn't a Craft app package")) }
}

fn parse_hash(text: &str) -> Option<[u8; HASH_LENGTH]> {
    if text.len() != HASH_LENGTH * 2 {
        return None;
    }
    let mut hash = [0u8; HASH_LENGTH];
    for (index, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        hash[index] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(hash)
}

/// The launcher's side of the helper.
struct RootSession {
    child: Child,
    stream: UnixStream,
    _socket_dir: tempfile::TempDir,
}

/// The helper of this launcher, when it has one.
static SESSION: Mutex<Option<RootSession>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<RootSession>> {
    SESSION.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs a command that needs root. A command the helper knows is sent to it, starting it (and
/// with it the desktop's one password prompt) if needed; anything else goes through `pkexec`
/// on its own, as before.
pub fn run(args: &[&OsStr]) -> Result<(), String> {
    let Some((command, targets)) = RootCommand::of(args) else {
        return direct(args);
    };
    let hashes: Vec<Option<[u8; HASH_LENGTH]>> =
        if command.targets() == Targets::Files { targets.iter().map(|target| expected_hash(target)).collect() } else { vec![None; targets.len()] };
    let with_hashes: Vec<(OsString, Option<[u8; HASH_LENGTH]>)> =
        targets.iter().map(|target| target.to_os_string()).zip(hashes.iter().copied()).collect();
    validate(command, &with_hashes, None)?;
    // An AppImage's own binary lives in a temporary mount, and inside a Flatpak sandbox root
    // means the sandbox's own root: neither is a place to keep a privileged helper. Those keep
    // the per-install prompt.
    if std::env::var_os("APPIMAGE").is_some() || process::in_sandbox() {
        return direct(args);
    }
    let mut session = lock();
    if session.as_mut().is_some_and(|session| session.finished()) {
        *session = None;
    }
    if session.is_none() {
        match RootSession::start() {
            Ok(started) => *session = Some(started),
            Err(error) if error == REPLACED => return direct(args),
            Err(error) => return Err(error),
        }
    }
    let exchanged = match session.as_mut() {
        Some(active) => active.exchange(command, &targets, &hashes),
        None => Exchange::Broken("the elevated install session wasn't available; try again".into()),
    };
    match exchanged {
        Exchange::Installed(outcome) => outcome,
        Exchange::Broken(error) => {
            *session = None;
            Err(error)
        }
    }
}

/// What one exchange with the helper says.
enum Exchange {
    /// The helper answered: the command's own outcome. The helper is still usable.
    Installed(Result<(), String>),
    /// The helper could not be reached. It is dropped; the next call asks for permission again.
    Broken(String),
}

impl RootSession {
    /// Starts the helper through polkit. `Err("cancelled")` when the prompt was dismissed.
    fn start() -> Result<Self, String> {
        let executable = std::env::current_exe().map_err(|e| format!("couldn't find this program ({e})"))?;
        if !executable.is_file() {
            return Err(REPLACED.into());
        }
        let pkexec = pkexec().ok_or_else(|| "installing system-wide needs polkit (pkexec), which isn't installed".to_string())?;
        let socket_dir =
            tempfile::Builder::new().prefix("artcraft-helper-").tempdir().map_err(|e| format!("couldn't prepare the permission request ({e})"))?;
        let socket = socket_dir.path().join(SOCKET_NAME);
        let listener = UnixListener::bind(&socket).map_err(|e| format!("couldn't prepare the permission request ({e})"))?;
        listener.set_nonblocking(true).map_err(|e| format!("couldn't prepare the permission request ({e})"))?;
        let socket_text = socket.to_str().ok_or_else(|| "the permission socket path isn't text".to_string())?;
        let mut child = process::command(pkexec)
            .arg(&executable)
            .arg(format!("{FLAG}{socket_text}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("couldn't ask for permission ({e})"))?;
        let deadline = Instant::now() + START_LIMIT;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(start_failed_code(status.code()));
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let pid = i32::try_from(child.id()).ok();
                    let peer = match rustix::net::sockopt::socket_peercred(&stream) {
                        Ok(peer) => peer,
                        Err(error) => {
                            let reason = format!("couldn't verify the permission helper ({error})");
                            return Err(end(&mut child, reason));
                        }
                    };
                    if Some(peer.pid.as_raw_pid()) != pid {
                        continue;
                    }
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let mut ready = Vec::new();
                    if read_line(&mut stream, &mut ready).is_err() || ready != READY.trim_end().as_bytes() {
                        return Err(end(&mut child, "the permission helper didn't start".into()));
                    }
                    let _ = stream.set_read_timeout(None);
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(300)));
                    return Ok(Self { child, stream, _socket_dir: socket_dir });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(end(&mut child, "the permission prompt wasn't answered".into()));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(end(&mut child, format!("couldn't ask for permission ({error})"))),
            }
        }
    }

    fn finished(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    fn exchange(&mut self, command: RootCommand, targets: &[&OsStr], hashes: &[Option<[u8; HASH_LENGTH]>]) -> Exchange {
        let payload = match encode(command, targets, hashes) {
            Ok(payload) => payload,
            Err(()) => return Exchange::Broken("the install request is too large".into()),
        };
        let length = u32::try_from(payload.len()).unwrap_or(u32::MAX).to_be_bytes();
        let written = self.stream.write_all(&length).and_then(|()| self.stream.write_all(&payload)).and_then(|()| self.stream.flush());
        if let Err(e) = written {
            return Exchange::Broken(gone(e));
        }
        let mut status = [0u8; 1];
        if let Err(e) = self.stream.read_exact(&mut status) {
            return Exchange::Broken(gone(e));
        }
        let mut length = [0u8; 4];
        if let Err(e) = self.stream.read_exact(&mut length) {
            return Exchange::Broken(gone(e));
        }
        let size = u32::from_be_bytes(length) as usize;
        if size > MAX_PAYLOAD {
            return Exchange::Broken("the elevated install session sent an invalid answer; try again".into());
        }
        let mut message = vec![0u8; size];
        if let Err(e) = self.stream.read_exact(&mut message) {
            return Exchange::Broken(gone(e));
        }
        match status[0] {
            0 => Exchange::Installed(Ok(())),
            1 => Exchange::Installed(Err(String::from_utf8_lossy(&message).into_owned())),
            _ => Exchange::Broken("the elevated install session sent an invalid answer; try again".into()),
        }
    }
}

/// A session that stopped answering has no useful answers left; end it. A healthy helper is
/// never dropped: it sees each command through and exits once its socket closes or it is idle.
impl Drop for RootSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_line(stream: &mut UnixStream, out: &mut Vec<u8>) -> std::io::Result<()> {
    let mut byte = [0u8; 1];
    while out.len() < 256 {
        match stream.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => match byte[0] {
                b'\n' => return Ok(()),
                b'\r' => {}
                other => out.push(other),
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "handshake line too long"))
}

fn gone(error: std::io::Error) -> String {
    format!("the elevated install session ended ({error}); try again")
}

/// Stops a helper that will not be used and reports why.
fn end(child: &mut Child, reason: String) -> String {
    let _ = child.kill();
    let _ = child.wait();
    reason
}

fn start_failed_code(code: Option<i32>) -> String {
    match code {
        Some(126 | 127) => CANCELLED.to_string(),
        Some(code) => format!("couldn't ask for permission (exit {code})"),
        None => "couldn't ask for permission (interrupted)".into(),
    }
}

/// The helper's request: the command number, then every target with an optional release hash.
fn encode(command: RootCommand, targets: &[&OsStr], hashes: &[Option<[u8; HASH_LENGTH]>]) -> Result<Vec<u8>, ()> {
    if targets.is_empty() || targets.len() > MAX_TARGETS || hashes.len() != targets.len() {
        return Err(());
    }
    let count = u32::try_from(targets.len()).map_err(|_| ())?;
    let mut payload = Vec::new();
    payload.push(command as u8);
    payload.extend_from_slice(&count.to_be_bytes());
    for (target, expected) in targets.iter().zip(hashes) {
        let bytes = target.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_TARGET {
            return Err(());
        }
        payload.extend_from_slice(&u32::try_from(bytes.len()).map_err(|_| ())?.to_be_bytes());
        payload.extend_from_slice(bytes);
        match expected {
            Some(hash) => {
                payload.push(1);
                payload.extend_from_slice(hash);
            }
            None => payload.push(0),
        }
    }
    if payload.len() > MAX_PAYLOAD { Err(()) } else { Ok(payload) }
}

/// A command that needs root, run on its own through `pkexec` (used where the helper can't).
fn direct(args: &[&OsStr]) -> Result<(), String> {
    let args = with_absolute_program(&SYSTEM_BIN_DIRS, args)?;
    let pkexec = pkexec().ok_or("installing system-wide needs polkit (pkexec), which isn't installed")?;
    let status = process::command(pkexec).args(&args).status().map_err(|e| format!("couldn't ask for permission ({e})"))?;
    super::root_outcome(status.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn the_package_commands_are_the_ones_the_launcher_builds() {
        use artcraft_launcher_core::PackageKind;
        assert_eq!(crate::install::linux::install_command(PackageKind::Deb), Some(APT_INSTALL));
        for command in RootCommand::ALL {
            let argv = command.argv();
            let mut args: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
            args.push(OsStr::new("/downloads/app.deb"));
            let (found, targets) = RootCommand::of(&args).expect("every command is recognized");
            assert_eq!(found, command);
            assert_eq!(targets, vec![OsStr::new("/downloads/app.deb")]);
        }
    }

    #[test]
    fn the_longest_command_claims_the_call() {
        let mut args: Vec<&OsStr> = DNF_INSTALL.iter().map(OsStr::new).collect();
        args.push(OsStr::new("/downloads/app.rpm"));
        assert_eq!(RootCommand::of(&args).map(|(command, _)| command), Some(RootCommand::DnfInstall));
    }

    #[test]
    fn a_call_without_targets_is_not_shared() {
        let args: Vec<&OsStr> = APT_INSTALL.iter().map(OsStr::new).collect();
        assert_eq!(RootCommand::of(&args), None);
    }

    #[test]
    fn an_unknown_command_is_not_shared() {
        let args = [OsStr::new("/bin/sh"), OsStr::new("-c"), OsStr::new("id")];
        assert_eq!(RootCommand::of(&args), None);
    }

    #[test]
    fn requests_round_trip() {
        let targets = [os("/downloads/a.deb"), os("/downloads/b.deb")];
        let borrowed: Vec<&OsStr> = targets.iter().map(OsString::as_os_str).collect();
        let hashes = [Some([0x11; HASH_LENGTH]), None];
        let payload = encode(RootCommand::AptInstall, &borrowed, &hashes).expect("a small request");
        let request = decode(&payload).expect("the helper reads it back");
        assert_eq!(request.command, RootCommand::AptInstall);
        assert_eq!(request.targets, vec![(targets[0].clone(), hashes[0]), (targets[1].clone(), hashes[1])]);
    }

    #[test]
    fn malformed_requests_are_refused() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[99]).is_err());
        assert!(decode(&[0, 0, 0, 0, 0]).is_err(), "no targets");
        // A target that claims to be longer than the request.
        assert!(decode(&[0, 0, 0, 0, 1, 0, 0, 0, 9, b'a']).is_err());
        // Data after the last target.
        let mut payload = encode(RootCommand::AptRemove, &[OsStr::new("photocraft")], &[None]).expect("a small request");
        payload.push(0);
        assert!(decode(&payload).is_err());
        // An unknown checksum flag.
        let mut payload = encode(RootCommand::AptInstall, &[OsStr::new("/tmp/a.deb")], &[None]).expect("a small request");
        let last = payload.len() - 1;
        payload[last] = 2;
        assert!(decode(&payload).is_err());
    }

    #[test]
    fn oversized_requests_are_refused() {
        let big = "a".repeat(MAX_TARGET + 1);
        assert!(encode(RootCommand::AptInstall, &[OsStr::new(&big)], &[None]).is_err());
        assert!(encode(RootCommand::AptInstall, &[], &[]).is_err());
        assert!(encode(RootCommand::AptInstall, &[OsStr::new("/tmp/a.deb")], &[]).is_err());
    }

    #[test]
    fn package_files_must_be_absolute_and_exist() {
        assert!(check_file(OsStr::new("relative.deb"), None).is_err());
        assert!(check_file(OsStr::new("/no/such/package.deb"), None).is_err());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.deb");
        std::fs::write(&file, b"package").unwrap();
        assert_eq!(check_file(file.as_os_str(), None), Ok(()));
        // The helper only trusts the user who approved the prompt.
        assert!(check_file(file.as_os_str(), Some(u32::MAX)).is_err());
    }

    #[test]
    fn package_names_are_craft_packages() {
        for name in ["photocraft", "photocraft-bin", "photocraft-git", "ai.storyteller.photocraft", "printcraft"] {
            assert_eq!(check_name(OsStr::new(name)), Ok(()), "{name}");
        }
        for name in ["", "--option", "a/b", "a b", "a\nb", "gcc@12"] {
            assert!(check_name(OsStr::new(name)).is_err(), "{name}");
        }
    }

    #[test]
    fn removal_commands_take_names_and_installs_take_files() {
        assert_eq!(RootCommand::AptInstall.targets(), Targets::Files);
        assert_eq!(RootCommand::PacmanInstall.targets(), Targets::Files);
        assert_eq!(RootCommand::AptRemove.targets(), Targets::Names);
        assert_eq!(RootCommand::FlatpakSystemUninstall.targets(), Targets::Names);
    }

    #[test]
    fn checksums_are_read_from_hex() {
        let text = "5a".repeat(HASH_LENGTH);
        assert_eq!(parse_hash(&text), Some([0x5a; HASH_LENGTH]));
        assert_eq!(parse_hash(&text[..text.len() - 1]), None);
        assert_eq!(parse_hash(&"zz".repeat(HASH_LENGTH)), None);
    }

    #[test]
    fn packages_are_copied_and_checked_before_install() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.deb");
        std::fs::write(&file, b"package").unwrap();
        let uid = std::fs::metadata(&file).unwrap().uid();
        let expected = crate::http::sha256_file(&file).unwrap();
        let hash = parse_hash(&expected).unwrap();
        let targets = vec![(file.as_os_str().to_os_string(), Some(hash))];
        let (copies, staging) = stage_targets(RootCommand::AptInstall, &targets, uid).unwrap();
        assert_eq!(copies.len(), 1);
        assert_ne!(Path::new(&copies[0]).parent(), Some(dir.path()));
        assert_eq!(std::fs::read(&copies[0]).unwrap(), b"package");
        drop(staging);
        assert!(!Path::new(&copies[0]).exists(), "the staging copy is removed with the staging folder");
        assert!(stage_targets(RootCommand::AptInstall, &[(file.as_os_str().to_os_string(), Some([0; HASH_LENGTH]))], uid).is_err());
    }
}
