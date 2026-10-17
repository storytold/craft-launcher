//! Where log records go: standard error and a log file.
//!
//! A launcher started from the Dock, the Start menu or at login has no terminal (and the Windows
//! build has no console at all), so without a file every `log::` record and the report of a panic
//! vanished. The update service (`artcraft-launcher-updater`) has no terminal either. `env_logger`
//! still formats and filters the records; its output goes to standard error and to
//! `logs/<name>.log` in the program's data folder (`Paths::logs`). Each start moves the previous
//! log to `<name>.1.log` (and that one to `.2`), so the log of a run that crashed survives the next
//! launch. `<name>` is the program's machine name (`artcraft-launcher`, `artcraft-launcher-updater`).
//!
//! Levels: `info` for the launcher's own crates, `warn` for everything else (wgpu and zbus are
//! chatty at `info`). `RUST_LOG` replaces that as before: `debug`, `info,zbus=warn`, ….
//!
//! Records logged before the file is attached are kept (up to [`MAX_PENDING`]) and written first.
//! Writing never panics: a file that can't be created or written leaves standard error as the
//! only sink. Never log a sign-in token.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// How many previous logs are kept (`<name>.1.log` … `<name>.<KEEP>.log`).
pub const KEEP: usize = 2;
/// The log file stops growing past this size (a runaway warning can't fill the disk).
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Records kept in memory until the log file is attached.
pub const MAX_PENDING: usize = 512;
/// The filter when `RUST_LOG` is unset (env_logger matches `artcraft_launcher` as a prefix, so the
/// updater's `artcraft_launcher_updater` records are included).
pub const DEFAULT_FILTER: &str = "warn,artcraft_launcher=info";

/// `<name>.log`, or `<name>.<n>.log` for the `n`th previous one.
fn numbered(dir: &Path, name: &str, n: usize) -> PathBuf {
    if n == 0 { dir.join(format!("{name}.log")) } else { dir.join(format!("{name}.{n}.log")) }
}

/// Shift the previous logs up one (`.log` → `.1.log` → … → `.<KEEP>.log`, the oldest dropped)
/// and create a fresh, empty `<name>.log` in `dir` (created if missing).
pub fn rotate(dir: &Path, name: &str) -> std::io::Result<(PathBuf, File)> {
    std::fs::create_dir_all(dir)?;
    for n in (1..=KEEP).rev() {
        match std::fs::rename(numbered(dir, name, n - 1), numbered(dir, name, n)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    let path = numbered(dir, name, 0);
    let file = File::create(&path)?;
    Ok((path, file))
}

/// Where formatted records go besides standard error: kept in memory until a file is attached,
/// then appended to it until it reaches its size cap.
pub struct Sink {
    file: Option<File>,
    pending: Vec<String>,
    dropped: usize,
    written: u64,
    max_bytes: u64,
    capped: bool,
}

impl Sink {
    pub fn new(max_bytes: u64) -> Sink {
        Sink { file: None, pending: Vec::new(), dropped: 0, written: 0, max_bytes, capped: false }
    }

    /// Append `record` to the file, or keep it until [`Sink::attach`].
    pub fn write(&mut self, record: &str) {
        if self.file.is_none() && !self.capped {
            if self.pending.len() < MAX_PENDING {
                self.pending.push(record.to_owned());
            } else {
                self.dropped = self.dropped.saturating_add(1);
            }
            return;
        }
        self.append(record);
    }

    /// Start writing to `file`, first the records kept so far.
    pub fn attach(&mut self, file: File) {
        self.file = Some(file);
        for record in std::mem::take(&mut self.pending) {
            self.append(&record);
        }
        if self.dropped > 0 {
            let note = format!("{} earlier log lines were dropped before the log file was opened\n", self.dropped);
            self.dropped = 0;
            self.append(&note);
        }
    }

    /// No file this run: stop keeping records for one.
    pub fn no_file(&mut self) {
        self.pending = Vec::new();
        self.dropped = 0;
        self.capped = true;
    }

    fn append(&mut self, record: &str) {
        if self.capped {
            return;
        }
        let Some(file) = self.file.as_mut() else { return };
        let len = u64::try_from(record.len()).unwrap_or(u64::MAX);
        let text = if self.written.saturating_add(len) > self.max_bytes {
            self.capped = true;
            format!("log file reached {} bytes; later records go to standard error only\n", self.max_bytes)
        } else {
            self.written = self.written.saturating_add(len);
            record.to_owned()
        };
        // A full disk or a vanished file must not take the launcher down: drop the file sink.
        if file.write_all(text.as_bytes()).is_err() {
            self.file = None;
            self.capped = true;
        }
    }
}

/// env_logger's output: each formatted record to standard error (unless turned off) and to the [`Sink`].
struct Tee {
    sink: Arc<Mutex<Sink>>,
    stderr: bool,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.stderr {
            let _ = std::io::stderr().write_all(buf);
        }
        self.sink.lock().unwrap_or_else(PoisonError::into_inner).write(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.stderr {
            let _ = std::io::stderr().flush();
        }
        Ok(())
    }
}

/// The installed logger's file side.
pub struct Logs {
    sink: Arc<Mutex<Sink>>,
    name: &'static str,
}

impl Logs {
    /// Rotate the logs in `dir` and send records to the fresh file; returns its path.
    pub fn attach_dir(&self, dir: &Path) -> Result<PathBuf, String> {
        let (path, file) = rotate(dir, self.name).map_err(|e| format!("{}: {e}", dir.display()))?;
        self.sink.lock().unwrap_or_else(PoisonError::into_inner).attach(file);
        Ok(path)
    }

    /// No log file this run (no data folder on this system).
    pub fn no_file(&self) {
        self.sink.lock().unwrap_or_else(PoisonError::into_inner).no_file();
    }
}

/// An env_logger builder (filter from `RUST_LOG`, else [`DEFAULT_FILTER`]) writing through a [`Tee`].
fn builder(sink: Arc<Mutex<Sink>>, stderr: bool) -> env_logger::Builder {
    let mut b = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(DEFAULT_FILTER));
    b.target(env_logger::Target::Pipe(Box::new(Tee { sink, stderr })));
    b
}

/// Install the logger for the program `name` (the log file's name); `None` when another logger
/// was installed first. `stderr` also echoes every record to standard error (a command-line run
/// that prints its own results turns it off). Call [`Logs::attach_dir`] (or [`Logs::no_file`])
/// once it is known whether this run keeps a log.
pub fn install(name: &'static str, stderr: bool) -> Option<Logs> {
    let sink = Arc::new(Mutex::new(Sink::new(MAX_FILE_BYTES)));
    builder(sink.clone(), stderr).try_init().ok()?;
    Some(Logs { sink, name })
}

/// Report a panic through the logger (standard error and the log file), with a backtrace when
/// `RUST_BACKTRACE` asks for one; standard error alone when `RUST_LOG` turned errors off.
pub fn install_panic_hook(name: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        let trace = std::backtrace::Backtrace::capture();
        let report = if trace.status() == std::backtrace::BacktraceStatus::Captured {
            format!("internal error: {info}\n{trace}")
        } else {
            format!("internal error: {info}")
        };
        if log::log_enabled!(log::Level::Error) {
            log::error!("{report}");
        } else {
            let _ = writeln!(std::io::stderr(), "{name}: {report}");
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const NAME: &str = "artcraft-launcher";

    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("artcraft-launcher-logging-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn rotation_keeps_the_previous_logs_and_starts_an_empty_file() {
        let dir = temp_dir("rotate");
        for run in 1..=4 {
            let (path, _file) = rotate(&dir, NAME).unwrap();
            assert_eq!(path, dir.join("artcraft-launcher.log"));
            assert_eq!(read(&path), "");
            std::fs::write(&path, format!("run {run}")).unwrap();
        }
        assert_eq!(read(&dir.join("artcraft-launcher.log")), "run 4");
        assert_eq!(read(&dir.join("artcraft-launcher.1.log")), "run 3");
        assert_eq!(read(&dir.join("artcraft-launcher.2.log")), "run 2");
        assert!(!dir.join("artcraft-launcher.3.log").exists(), "only {KEEP} old logs are kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_program_rotates_its_own_logs() {
        let dir = temp_dir("names");
        let (launcher, _a) = rotate(&dir, NAME).unwrap();
        std::fs::write(&launcher, "launcher").unwrap();
        let (updater, _b) = rotate(&dir, "artcraft-launcher-updater").unwrap();
        assert_eq!(updater, dir.join("artcraft-launcher-updater.log"));
        assert_eq!(read(&launcher), "launcher", "the updater's start rotated the launcher's log");
        assert!(!dir.join("artcraft-launcher.1.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_into_an_unusable_directory_is_an_error_not_a_panic() {
        let dir = temp_dir("blocked");
        std::fs::write(&dir, "a file where the directory should be").unwrap();
        assert!(rotate(&dir, NAME).is_err());
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn records_logged_before_the_file_exists_are_written_when_it_is_attached() {
        let dir = temp_dir("pending");
        let (path, file) = rotate(&dir, NAME).unwrap();
        let mut sink = Sink::new(MAX_FILE_BYTES);
        sink.write("early 1\n");
        sink.write("early 2\n");
        sink.attach(file);
        sink.write("late\n");
        assert_eq!(read(&path), "early 1\nearly 2\nlate\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pending_buffer_is_bounded_and_says_how_much_it_dropped() {
        let dir = temp_dir("bounded");
        let (path, file) = rotate(&dir, NAME).unwrap();
        let mut sink = Sink::new(MAX_FILE_BYTES);
        for i in 0..MAX_PENDING + 5 {
            sink.write(&format!("line {i}\n"));
        }
        sink.attach(file);
        let text = read(&path);
        assert_eq!(text.lines().filter(|l| l.starts_with("line ")).count(), MAX_PENDING);
        assert_eq!(text.matches("5 earlier log lines were dropped").count(), 1, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_file_nothing_is_kept() {
        let mut sink = Sink::new(MAX_FILE_BYTES);
        sink.write("early\n");
        sink.no_file();
        sink.write("late\n");
        assert!(sink.pending.is_empty() && sink.dropped == 0);
    }

    #[test]
    fn the_file_stops_at_its_size_cap_with_one_note() {
        let dir = temp_dir("cap");
        let (path, file) = rotate(&dir, NAME).unwrap();
        let mut sink = Sink::new(100);
        sink.attach(file);
        for i in 0..50 {
            sink.write(&format!("line {i:02} with some padding\n"));
        }
        let text = read(&path);
        assert!(text.lines().filter(|l| l.starts_with("line ")).count() < 5, "{text}");
        assert_eq!(text.matches("log file reached").count(), 1, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// env_logger's formatted records reach the file: the launcher's info records and everyone's
    /// warnings.
    #[test]
    fn env_logger_records_reach_the_attached_file() {
        let dir = temp_dir("logger");
        let sink = Arc::new(Mutex::new(Sink::new(MAX_FILE_BYTES)));
        let logger = builder(sink.clone(), false).parse_filters(DEFAULT_FILTER).build();
        let logs = Logs { sink, name: NAME };
        let record = |level, target: &'static str, msg: &'static str| {
            logger.log(&log::Record::builder().level(level).target(target).args(format_args!("{msg}")).build());
        };
        record(log::Level::Info, "artcraft_launcher", "before the file");
        let path = logs.attach_dir(&dir).unwrap();
        record(log::Level::Warn, "wgpu_hal::vulkan", "a real warning");
        record(log::Level::Info, "artcraft_launcher_native::proxy", "proxy in use");
        record(log::Level::Info, "artcraft_launcher_updater::server", "update service started");
        record(log::Level::Info, "zbus::connection", "too chatty");
        record(log::Level::Debug, "artcraft_launcher", "below info");
        let text = read(&path);
        assert!(text.contains("artcraft_launcher] before the file\n"), "{text}");
        assert!(text.contains("WARN  wgpu_hal::vulkan] a real warning\n"), "{text}");
        assert!(text.contains("artcraft_launcher_native::proxy] proxy in use\n"), "{text}");
        assert!(text.contains("artcraft_launcher_updater::server] update service started\n"), "{text}");
        assert!(!text.contains("too chatty") && !text.contains("below info") && !text.contains('\u{1b}'), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_poisoned_lock_does_not_stop_logging() {
        let dir = temp_dir("poison");
        let logs = Logs { sink: Arc::new(Mutex::new(Sink::new(MAX_FILE_BYTES))), name: NAME };
        let path = logs.attach_dir(&dir).unwrap();
        let sink = logs.sink.clone();
        let _ = std::thread::spawn(move || {
            let _guard = sink.lock();
            panic!("poison the sink");
        })
        .join();
        assert!(logs.sink.is_poisoned());
        let mut tee = Tee { sink: logs.sink.clone(), stderr: false };
        tee.write_all(b"after the panic\n").unwrap();
        assert!(read(&path).contains("after the panic\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
