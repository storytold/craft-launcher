//! One UAC prompt for a run of Windows Installer packages.
//!
//! The Craft apps' MSIs are per-machine, so Windows asks for administrator permission for each
//! one. The first install instead starts this program as an elevated helper through a single
//! `runas` prompt, and later installs and removals are sent to it over a loopback connection.
//! The helper stays alive until it has been idle for fifteen minutes or the launcher exits.
//!
//! The package bytes are streamed to the helper and checked against their SHA-256, so the
//! elevated process never needs to read the launcher's cache, and the package cannot change
//! between the launcher's download check and the installer. The helper accepts only installs of
//! streamed MSIs and removals of product codes; nothing it receives can run another program.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use artcraft_launcher_core::protocol::CANCELLED;
use sha2::{Digest, Sha256};

use super::windows_wire::{self, Request};

/// The launcher passes this to the helper it starts elevated.
const FLAG: &str = "--artcraft-elevated-helper=";

/// How long the helper waits for another install before it exits.
const IDLE: Duration = Duration::from_secs(15 * 60);

/// How long the launcher waits for the permission helper to connect after UAC has been answered.
const CONNECT_LIMIT: Duration = Duration::from_secs(60);

const MAX_MESSAGE: usize = 4 << 10;
const HASH_LENGTH: usize = 32;

/// Runs the elevated helper when this program was started as one. The launcher calls this before
/// it sets anything else up; every other start has no [`FLAG`] and returns `None`.
pub fn run_if_requested() -> Option<ExitCode> {
    let value = std::env::args_os().find_map(|arg| arg.to_str().and_then(|text| text.strip_prefix(FLAG)).map(str::to_string))?;
    let Some((port, token)) = value.split_once(':') else { return Some(ExitCode::FAILURE) };
    let Ok(port) = port.parse::<u16>() else { return Some(ExitCode::FAILURE) };
    let token_ok = !token.is_empty() && token.len() <= 128 && token.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !token_ok {
        return Some(ExitCode::FAILURE);
    }
    Some(ExitCode::from(serve(port, token.to_string())))
}

/// Installs `msi` as an administrator. The caller is already past the download check; this only
/// fetches the install permission.
pub fn install(msi: &Path, destination: Option<&Path>, expected: Option<&str>) -> Result<(), String> {
    if !msi.is_file() {
        return Err(format!("{} isn't a downloaded package", msi.display()));
    }
    let expected = match expected {
        Some(text) => Some(windows_wire::parse_sha256(text).ok_or_else(|| "the release checksum isn't a SHA-256".to_string())?),
        None => None,
    };
    with_session(|session| session.install(msi, destination, expected))
}

/// Removes one MSI product as an administrator.
pub fn uninstall(product: &str) -> Result<(), String> {
    windows_wire::validate_product(product)?;
    with_session(|session| session.uninstall(product))
}

/// The helper's side: connect to the waiting launcher and serve requests until the launcher goes
/// away or nothing has been asked for a while.
fn serve(port: u16, token: String) -> u8 {
    let Some(mut stream) = connect(port) else { return 1 };
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
    if writeln!(stream, "HELLO {token}").and_then(|()| stream.flush()).is_err() {
        return 1;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let mut answer = Vec::new();
    if read_line(&mut stream, &mut answer).is_err() || answer != b"OK" {
        return 1;
    }
    let _ = stream.set_read_timeout(Some(IDLE));
    let mut counter = 0u32;
    loop {
        let header = match read_frame(&mut stream) {
            Ok(Some(header)) => header,
            Ok(None) => return 0,
            Err(_) => return 1,
        };
        let (outcome, intact) = match windows_wire::decode(&header) {
            Ok(Request::Install { destination, length, expected }) => install_package(&mut stream, destination, length, expected, &token, counter),
            Ok(Request::Uninstall { product }) => (uninstall_package(&product), true),
            Err(_) => return 1,
        };
        if write_response(&mut stream, &outcome).is_err() || !intact {
            return 1;
        }
        counter = counter.wrapping_add(1);
    }
}

fn connect(port: u16) -> Option<TcpStream> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match TcpStream::connect_timeout(&address, Duration::from_secs(2)) {
            Ok(stream) => return Some(stream),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
}

/// Reads one line, for the small plain-text handshake.
fn read_line(stream: &mut TcpStream, out: &mut Vec<u8>) -> std::io::Result<()> {
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

/// Reads a length-prefixed request header. `None` means the launcher is gone or idle.
fn read_frame(stream: &mut TcpStream) -> Result<Option<Vec<u8>>, String> {
    let mut length = [0u8; 4];
    match stream.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if matches!(error.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {
            return Ok(None);
        }
        Err(error) => return Err(format!("couldn't read the next request ({error})")),
    }
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > windows_wire::MAX_HEADER {
        return Err("the install request is too large".into());
    }
    let mut header = vec![0u8; length];
    stream.read_exact(&mut header).map_err(|error| format!("couldn't read the next request ({error})"))?;
    Ok(Some(header))
}

/// Receives one MSI into the elevated account's temp folder, checks it and runs it. The second
/// value says whether the connection is still at a request boundary; a short or failed transfer
/// leaves it out of step, so the helper then exits.
fn install_package(
    stream: &mut TcpStream,
    destination: Option<String>,
    length: u64,
    expected: Option<[u8; HASH_LENGTH]>,
    token: &str,
    counter: u32,
) -> (Result<(), String>, bool) {
    let (temp, mut file) = match TempMsi::create(token, counter) {
        Ok(pair) => pair,
        Err(error) => return (Err(error), false),
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 << 10];
    let mut remaining = length;
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        match stream.read(&mut buffer[..want]) {
            Ok(0) => return (Err("the installer transfer ended early".into()), false),
            Ok(read) => {
                hasher.update(&buffer[..read]);
                if let Err(error) = file.write_all(&buffer[..read]) {
                    return (Err(format!("couldn't save the installer ({error})")), false);
                }
                remaining -= read as u64;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return (Err(format!("couldn't receive the installer ({error})")), false),
        }
    }
    if let Err(error) = file.flush() {
        return (Err(format!("couldn't save the installer ({error})")), false);
    }
    drop(file);
    // Reopen read-only with `FILE_SHARE_READ` and keep that handle open while `msiexec` reads
    // the package: no same-account process can replace it in the meantime.
    let mut lock = match lock_msi(&temp.path) {
        Ok(file) => file,
        Err(error) => return (Err(error), false),
    };
    let stored = match hash_file(&mut lock) {
        Ok(hash) => hash,
        Err(error) => return (Err(error), false),
    };
    let mut digest = [0u8; HASH_LENGTH];
    if let Err(error) = stream.read_exact(&mut digest) {
        return (Err(format!("couldn't verify the installer ({error})")), false);
    }
    let received = hasher.finalize();
    if received.as_slice() != &digest[..] {
        return (Err("the installer changed on its way to the permission helper".into()), true);
    }
    if stored.as_slice() != received.as_slice() {
        return (Err("the installer changed on disk before it could run".into()), true);
    }
    if let Some(expected) = expected
        && stored != expected
    {
        return (Err("the installer doesn't match the release checksum".into()), true);
    }
    let destination = match destination {
        Some(destination) => {
            let path = PathBuf::from(destination);
            if !path.is_absolute() {
                return (Err("the install folder must be an absolute path".into()), true);
            }
            Some(path)
        }
        None => None,
    };
    let result = install_msi(&temp.path, destination.as_deref());
    // A cancelled install must stay exactly `cancelled`, so the launcher can tell it from an
    // error; everything else gets the end of the installer's log to explain it.
    let result = result.map_err(|error| if error == CANCELLED { error } else { with_log_tail(error, &log_path(&temp.path)) });
    (result, true)
}

/// `msi_command` canonicalizes the package path (junctions and symlinks, #338), so its log sits
/// beside the canonical path rather than the path this module created.
fn log_path(msi: &Path) -> PathBuf {
    super::msi_path(msi).map_or_else(|_| super::msi_log(msi), |path| super::msi_log(path.as_path()))
}

/// Opens the staged MSI read-only and keeps other processes from writing or deleting it.
fn lock_msi(path: &Path) -> Result<std::fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path).map_err(|error| format!("couldn't lock the installer ({error})"))
}

fn hash_file(file: &mut std::fs::File) -> Result<[u8; HASH_LENGTH], String> {
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(0)).map_err(|error| format!("couldn't verify the installer ({error})"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 << 10];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("couldn't verify the installer ({error})")),
        }
    }
    let mut hash = [0u8; HASH_LENGTH];
    hash.copy_from_slice(hasher.finalize().as_slice());
    Ok(hash)
}

fn install_msi(temp: &Path, destination: Option<&Path>) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut command = super::msi_command(&super::windows::msiexec(), temp)?;
    if let Some(destination) = destination {
        command.raw_arg(super::msi_folder_argument(destination)?);
    }
    let status = command.status().map_err(|error| format!("couldn't start the Windows Installer ({error})"))?;
    super::windows::explain(status.code())
}

fn uninstall_package(product: &str) -> Result<(), String> {
    let status = crate::process::command(super::windows::msiexec())
        .args(["/x", product, "/passive", "/norestart"])
        .status()
        .map_err(|error| format!("couldn't start the Windows Installer ({error})"))?;
    super::windows::explain(status.code())
}

fn write_response(stream: &mut TcpStream, outcome: &Result<(), String>) -> std::io::Result<()> {
    let (status, message) = match outcome {
        Ok(()) => (0u8, String::new()),
        Err(error) => (1u8, error.chars().take(1000).collect()),
    };
    let message = message.as_bytes();
    let length = u32::try_from(message.len()).unwrap_or(u32::MAX).to_be_bytes();
    stream.write_all(&[status])?;
    stream.write_all(&length)?;
    stream.write_all(message)?;
    stream.flush()
}

/// The end of the installer's verbose log, for an error the user can act on.
fn with_log_tail(error: String, log: &Path) -> String {
    use std::io::{Seek, SeekFrom};
    let Ok(metadata) = std::fs::metadata(log) else { return error };
    let Ok(mut file) = std::fs::File::open(log) else { return error };
    let start = metadata.len().saturating_sub(4096);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return error;
    }
    let mut bytes = Vec::new();
    if file.take(4096).read_to_end(&mut bytes).is_err() {
        return error;
    }
    let text = String::from_utf8_lossy(&bytes);
    let tail: String = text.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("").trim().chars().take(500).collect();
    if tail.is_empty() { error } else { format!("{error}: {tail}") }
}

/// A downloaded MSI on the elevated account's temp folder, removed however its install ends.
struct TempMsi {
    path: PathBuf,
}

impl TempMsi {
    fn create(token: &str, counter: u32) -> Result<(Self, std::fs::File), String> {
        use std::os::windows::fs::OpenOptionsExt;
        // Only readers may share this file while the helper holds it open: no writer or deleter
        // can replace the package between the digest check and `msiexec`.
        const FILE_SHARE_READ: u32 = 0x1;
        let directory = std::env::temp_dir();
        let prefix: String = token.chars().take(16).collect();
        for attempt in 0..16u32 {
            let path = directory.join(format!("artcraft-update-{prefix}-{counter}-{attempt}.msi"));
            match std::fs::OpenOptions::new().write(true).create_new(true).share_mode(FILE_SHARE_READ).open(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("couldn't prepare the installer ({error})")),
            }
        }
        Err("couldn't prepare the installer".into())
    }
}

impl Drop for TempMsi {
    fn drop(&mut self) {
        // Resolve the log while the package still exists, so canonicalization can succeed.
        let log = log_path(&self.path);
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(log);
    }
}

/// The launcher's side of the helper.
struct Session {
    stream: TcpStream,
}

/// The helper of this launcher, when it has one.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Session>> {
    SESSION.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What one exchange with the helper says.
enum Reply {
    /// The helper answered: the installer's own outcome. The helper is still usable.
    Done(Result<(), String>),
    /// The helper could not be reached. It is dropped; the next call asks for permission again.
    Broken(String),
}

/// Runs `work` against the helper, starting it (and with it the one UAC prompt) if needed.
fn with_session(work: impl FnOnce(&mut Session) -> Reply) -> Result<(), String> {
    let mut slot = lock();
    if slot.as_ref().is_some_and(|session| !session.alive()) {
        *slot = None;
    }
    if slot.is_none() {
        *slot = Some(Session::start()?);
    }
    let reply = match slot.as_mut() {
        Some(session) => work(session),
        None => Reply::Broken("the elevated install session wasn't available; try again".into()),
    };
    match reply {
        Reply::Done(outcome) => outcome,
        Reply::Broken(error) => {
            *slot = None;
            Err(error)
        }
    }
}

impl Session {
    /// Starts the helper through UAC. `Err("cancelled")` when the prompt was dismissed.
    fn start() -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| format!("couldn't prepare the permission request ({error})"))?;
        listener.set_nonblocking(true).map_err(|error| format!("couldn't prepare the permission request ({error})"))?;
        let port = listener.local_addr().map_err(|error| format!("couldn't prepare the permission request ({error})"))?.port();
        let token = random_token()?;
        let executable = std::env::current_exe().map_err(|error| format!("couldn't find this program ({error})"))?;
        let executable = executable.to_str().ok_or_else(|| "this program's path isn't text".to_string())?;
        let parameters = format!("{FLAG}{port}:{token}");
        let info = winsafe::SHELLEXECUTEINFO {
            mask: winsafe::co::SEE_MASK::NOASYNC | winsafe::co::SEE_MASK::FLAG_NO_UI,
            verb: Some("runas"),
            file: executable,
            parameters: Some(&parameters),
            show: winsafe::co::SW::HIDE,
            ..Default::default()
        };
        winsafe::ShellExecuteEx(&info).map_err(|error| {
            if error == winsafe::co::ERROR::CANCELLED { CANCELLED.to_string() } else { format!("couldn't ask for permission ({error})") }
        })?;
        let deadline = Instant::now() + CONNECT_LIMIT;
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut hello = Vec::new();
                    if read_line(&mut stream, &mut hello).is_err() || hello != format!("HELLO {token}").into_bytes() {
                        continue;
                    }
                    if stream.write_all(b"OK\n").and_then(|()| stream.flush()).is_err() {
                        continue;
                    }
                    let _ = stream.set_read_timeout(None);
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(300)));
                    return Ok(Self { stream });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err("the permission helper didn't start".into());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(format!("couldn't ask for permission ({error})")),
            }
        }
    }

    /// Whether the helper is still connected (it exits after being idle for `IDLE`).
    fn alive(&self) -> bool {
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let mut probe = [0u8; 1];
        let alive = matches!(self.stream.peek(&mut probe), Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock);
        let _ = self.stream.set_nonblocking(false);
        alive
    }

    fn install(&mut self, msi: &Path, destination: Option<&Path>, expected: Option<[u8; HASH_LENGTH]>) -> Reply {
        let destination = match destination {
            Some(path) => match path.to_str() {
                Some(text) => Some(text),
                None => return Reply::Done(Err("the install folder isn't text".into())),
            },
            None => None,
        };
        let mut file = match std::fs::File::open(msi) {
            Ok(file) => file,
            Err(error) => return Reply::Done(Err(format!("couldn't read the installer ({error})"))),
        };
        let length = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => return Reply::Done(Err(format!("couldn't read the installer ({error})"))),
        };
        let header = match windows_wire::encode_install(destination, length, expected.as_ref()) {
            Ok(header) => header,
            Err(error) => return Reply::Done(Err(error)),
        };
        if let Err(error) = self.write_header(&header) {
            return Reply::Broken(gone(error));
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 << 10];
        let mut sent = 0u64;
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if sent.saturating_add(read as u64) > length {
                        return Reply::Broken("the installer changed while it was being sent; try again".into());
                    }
                    hasher.update(&buffer[..read]);
                    if let Err(error) = self.stream.write_all(&buffer[..read]) {
                        return Reply::Broken(gone(error));
                    }
                    sent += read as u64;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Reply::Broken(gone(error)),
            }
        }
        if sent != length {
            return Reply::Broken("the installer changed while it was being sent; try again".into());
        }
        let digest = hasher.finalize();
        if let Err(error) = self.stream.write_all(&digest).and_then(|()| self.stream.flush()) {
            return Reply::Broken(gone(error));
        }
        self.read_reply()
    }

    fn uninstall(&mut self, product: &str) -> Reply {
        let header = match windows_wire::encode_uninstall(product) {
            Ok(header) => header,
            Err(error) => return Reply::Done(Err(error)),
        };
        if let Err(error) = self.write_header(&header) {
            return Reply::Broken(gone(error));
        }
        self.read_reply()
    }

    fn write_header(&mut self, header: &[u8]) -> std::io::Result<()> {
        let length = u32::try_from(header.len()).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "header too large"))?;
        self.stream.write_all(&length.to_be_bytes())?;
        self.stream.write_all(header)?;
        self.stream.flush()
    }

    fn read_reply(&mut self) -> Reply {
        let mut status = [0u8; 1];
        if let Err(error) = self.stream.read_exact(&mut status) {
            return Reply::Broken(gone(error));
        }
        let mut length = [0u8; 4];
        if let Err(error) = self.stream.read_exact(&mut length) {
            return Reply::Broken(gone(error));
        }
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_MESSAGE {
            return Reply::Broken("the elevated install session sent an invalid answer; try again".into());
        }
        let mut message = vec![0u8; length];
        if let Err(error) = self.stream.read_exact(&mut message) {
            return Reply::Broken(gone(error));
        }
        match status[0] {
            0 => Reply::Done(Ok(())),
            1 => Reply::Done(Err(String::from_utf8_lossy(&message).into_owned())),
            _ => Reply::Broken("the elevated install session sent an invalid answer; try again".into()),
        }
    }
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| format!("couldn't prepare the permission request ({error})"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn gone(error: std::io::Error) -> String {
    format!("the elevated install session ended ({error}); try again")
}
