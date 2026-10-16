//! The desktop shell's side of the History page: reads and clears the shared request log, saves a
//! CSV export, and tells the screen what time it is here.
//!
//! The log is the Craft AI layer's `jobs.jsonl`, written by every app. This side only reads it,
//! except for "Clear history", which the person confirms on the page. Nothing here can fail the
//! launcher: every problem comes back as a sentence for the page to show.

use std::path::PathBuf;

use artcraft_launcher_ui_egui::features::Clock;
use craft_ai_model::HistoryLog;
use craft_ai_model::history::{merge, report::iso_utc};
use craft_ai_native::HistoryFile;

pub struct DesktopHistory {
    file: Option<HistoryFile>,
    export_dir: Option<PathBuf>,
    /// Made-up history kept in memory (`--demo-data`): the real log is never read, cleared or exported.
    demo: Option<std::sync::Mutex<HistoryLog>>,
}

impl DesktopHistory {
    /// The standard log, and exports into the person's Downloads folder.
    pub fn new() -> Self {
        Self { file: HistoryFile::at_default_location(), export_dir: dirs::download_dir().or_else(dirs::home_dir), demo: None }
    }

    /// Made-up history that looks recent, for seeing how the page lays out.
    pub fn demo() -> Self {
        let mut history = Self { file: None, export_dir: None, demo: None };
        history.demo = Some(std::sync::Mutex::new(crate::history_demo::log(history.clock().now_ms)));
        history
    }

    #[cfg(test)]
    pub(crate) fn at(file: HistoryFile, export_dir: PathBuf) -> Self {
        Self { file: Some(file), export_dir: Some(export_dir), demo: None }
    }

    fn file(&self) -> Result<&HistoryFile, String> {
        self.file.as_ref().ok_or_else(|| "This computer has no place to keep the history.".to_owned())
    }

    pub fn read(&self) -> Result<HistoryLog, String> {
        if let Some(demo) = &self.demo {
            return Ok(demo.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone());
        }
        let read = self.file()?.read().map_err(|e| format!("Could not read the history: {e}"))?;
        Ok(merge(read.records, read.skipped))
    }

    pub fn clear(&self) -> Result<(), String> {
        if let Some(demo) = &self.demo {
            *demo.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = HistoryLog::default();
            return Ok(());
        }
        self.file()?.clear().map_err(|e| format!("Could not clear the history: {e}"))
    }

    /// Writes `csv` to a new file named for the time, never over an existing one. Returns its path.
    pub fn export(&self, csv: &str) -> Result<String, String> {
        if self.demo.is_some() {
            return Err("This is sample data, so there is nothing real to save.".to_owned());
        }
        let dir = self.export_dir.as_ref().ok_or("There is no Downloads folder to save into.")?;
        let stamp: String = iso_utc(self.clock().now_ms).chars().filter(char::is_ascii_digit).collect();
        for n in 0..100 {
            let name = if n == 0 { format!("craft-ai-history-{stamp}.csv") } else { format!("craft-ai-history-{stamp}-{n}.csv") };
            let path = dir.join(name);
            if path.exists() {
                continue;
            }
            artcraft_launcher_native::paths::write_atomic(&path, csv.as_bytes()).map_err(|e| format!("Could not save the file: {e}"))?;
            return Ok(path.display().to_string());
        }
        Err("Could not find a free file name.".to_owned())
    }

    pub fn clock(&self) -> Clock {
        let now = jiff::Timestamp::now();
        let offset_seconds = jiff::tz::TimeZone::system().to_offset(now).seconds();
        Clock { now_ms: u64::try_from(now.as_millisecond()).unwrap_or(0), offset_minutes: offset_seconds / 60 }
    }
}
