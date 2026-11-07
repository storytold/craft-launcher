//! The helper's log, written into its run folder (`elevated::LOG`) so a run stopped part way (by
//! antivirus, say) still says how far it got.
//!
//! Records from before the run folder exists are kept in memory (up to [`MAX_PENDING`]) and
//! written first. Writing never panics; a file that can't be created leaves the records unwritten.
//! Kept apart from the launcher's own logger, which brings env_logger and log rotation that a
//! single short run doesn't need.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};

use artcraft_launcher_native::install::elevated::LOG;

/// Records kept until the run folder exists. The helper logs a handful.
const MAX_PENDING: usize = 64;

#[derive(Default)]
struct Sink {
    file: Option<File>,
    pending: Vec<String>,
}

/// The installed logger.
pub struct RunLog {
    sink: Mutex<Sink>,
}

static LOGGER: OnceLock<RunLog> = OnceLock::new();

/// Installs the logger at `info`; `None` if one was installed already.
pub fn install() -> Option<&'static RunLog> {
    let logger = LOGGER.get_or_init(|| RunLog { sink: Mutex::new(Sink::default()) });
    log::set_logger(logger).ok()?;
    log::set_max_level(log::LevelFilter::Info);
    Some(logger)
}

impl RunLog {
    /// Creates [`LOG`] in `dir` (never one already there) and writes the kept records to it.
    pub fn attach(&self, dir: &Path) {
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let Ok(mut file) = File::create_new(dir.join(LOG)) else { return };
        for line in sink.pending.drain(..) {
            let _ = file.write_all(line.as_bytes());
        }
        sink.file = Some(file);
    }
}

impl log::Log for RunLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{} {}: {}\n", record.level(), record.target(), record.args());
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let Sink { file, pending } = &mut *sink;
        match file {
            Some(file) => {
                let _ = file.write_all(line.as_bytes());
            }
            None if pending.len() < MAX_PENDING => pending.push(line),
            None => {}
        }
    }

    fn flush(&self) {
        if let Some(file) = self.sink.lock().unwrap_or_else(PoisonError::into_inner).file.as_mut() {
            let _ = file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(message: &str) -> String {
        let logger = RunLog { sink: Mutex::new(Sink::default()) };
        log::Log::log(&logger, &log::Record::builder().level(log::Level::Info).target("t").args(format_args!("before")).build());
        let dir = tempfile_dir();
        logger.attach(&dir);
        log::Log::log(&logger, &log::Record::builder().level(log::Level::Warn).target("t").args(format_args!("{message}")).build());
        log::Log::log(&logger, &log::Record::builder().level(log::Level::Debug).target("t").args(format_args!("hidden")).build());
        log::Log::flush(&logger);
        let text = std::fs::read_to_string(dir.join(LOG)).unwrap_or_default();
        let _ = std::fs::remove_file(dir.join(LOG));
        let _ = std::fs::remove_dir(&dir);
        text
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("artcraft-helper-log-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn keeps_early_records_and_skips_debug() {
        assert_eq!(record("after"), "INFO t: before\nWARN t: after\n");
    }

    #[test]
    fn never_writes_into_a_log_already_there() {
        let dir = tempfile_dir().with_extension("planted");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(LOG), "planted");
        let logger = RunLog { sink: Mutex::new(Sink::default()) };
        logger.attach(&dir);
        log::Log::log(&logger, &log::Record::builder().level(log::Level::Info).target("t").args(format_args!("x")).build());
        assert_eq!(std::fs::read_to_string(dir.join(LOG)).unwrap_or_default(), "planted");
        let _ = std::fs::remove_file(dir.join(LOG));
        let _ = std::fs::remove_dir(&dir);
    }
}
