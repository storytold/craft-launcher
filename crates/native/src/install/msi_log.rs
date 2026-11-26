//! What a Windows Installer log (`/l*v`) says stopped an install that ended with error 1603,
//! which by itself only says that the installation failed.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How much of a log is read from its start, and again from its end. A launch condition runs
/// before any file is copied, so its lines are near the start of the log; a package that
/// schedules the removal of the version being replaced late logs that removal near the end.
const MAX_LOG_BYTES: u64 = 32 << 20;

/// The longest launch-condition message kept, in characters.
const MAX_MESSAGE_CHARS: usize = 500;

/// A cause the log names for a failed install.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Cause {
    /// One of the package's launch conditions failed, so it changed nothing. The message is the
    /// condition's own text, when the log has it.
    LaunchCondition(Option<String>),
    /// Windows Installer's cached copy of the installed version's package is missing, so that
    /// version can't be removed to make way for the new one.
    CachedPackageMissing,
}

/// The cause the log at `log` names, reading at most [`MAX_LOG_BYTES`] from its start and as
/// much again from its end. `None` when the log can't be read or names no cause this module
/// knows.
pub(super) fn cause(log: &Path) -> Option<Cause> {
    let mut file = std::fs::File::open(log).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut head = Vec::new();
    (&mut file).take(MAX_LOG_BYTES).read_to_end(&mut head).ok()?;
    let utf16 = is_utf16(&head);
    let mut read = text(&head, utf16);
    if meta.len() > MAX_LOG_BYTES {
        // The end, never overlapping the start. A UTF-16 log is read from an even offset so its
        // characters stay aligned, which takes one byte more.
        let start = (meta.len() - MAX_LOG_BYTES).max(MAX_LOG_BYTES);
        let start = if utf16 { start & !1 } else { start };
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut tail = Vec::new();
        file.take(MAX_LOG_BYTES + 1).read_to_end(&mut tail).ok()?;
        // A gap between the parts ends the start's last line. When the end begins where the start
        // stops, a line that crosses the boundary is read whole.
        if start > MAX_LOG_BYTES {
            read.push('\n');
        }
        read.push_str(&text(&tail, utf16));
    }
    find(&read)
}

/// Whether a log that starts with `bytes` is UTF-16: a byte order mark, or an ASCII first
/// character followed by a zero byte.
fn is_utf16(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xFE]) || matches!(bytes, [first, 0, ..] if *first != 0)
}

/// The log's text. Windows Installer writes it as UTF-16 (little-endian, usually with a byte
/// order mark) or in the system's code page; text in a code page reads correctly here only
/// where it is ASCII, which covers the lines [`find`] looks for.
fn text(bytes: &[u8], utf16: bool) -> String {
    if !utf16 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    String::from_utf16_lossy(&units)
}

/// The cause the log's text names: a failed `LaunchConditions` action ("Return value 3"), or a
/// a removal failure caused by an unavailable installer source (error 1714 with system error
/// 1612, see [`removal_failed_on_missing_cache`]).
fn find(text: &str) -> Option<Cause> {
    let mut in_launch_conditions = false;
    let mut message = None;
    let mut cached_missing = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("Action start") && line.ends_with("LaunchConditions.") {
            in_launch_conditions = true;
            message = None;
            continue;
        }
        if line.starts_with("Action ended") && line.contains("LaunchConditions.") {
            if line.contains("Return value 3") {
                return Some(Cause::LaunchCondition(message));
            }
            in_launch_conditions = false;
            continue;
        }
        if in_launch_conditions && let Some(text) = condition_message(line) {
            message = Some(text);
        }
        cached_missing |= removal_failed_on_missing_cache(line);
    }
    cached_missing.then_some(Cause::CachedPackageMissing)
}

/// Whether `line` is error 1714 ("The older version of [2] cannot be removed. ... System Error
/// [3].") with system error 1612, the installation source being unavailable. Field 1 of an error
/// record is its number, and the 1714 message puts the product name in field 2 and the system
/// error in field 3 (https://learn.microsoft.com/en-us/windows/win32/msi/error-table,
/// https://learn.microsoft.com/en-us/windows/win32/msi/windows-installer-error-messages). The log
/// shows the record either as the English message ("Error 1714. ... System Error 1612.") or by
/// its numbered fields ("Note: 1: 1714 2: <product> 3: 1612"). Both numbers are compared whole,
/// and the system error is the message's last one, so a product name that holds a number or
/// those words does not count.
fn removal_failed_on_missing_cache(line: &str) -> bool {
    let rendered = line
        .split_once("Error 1714.")
        .and_then(|(_, rest)| rest.rsplit_once("System Error "))
        .is_some_and(|(_, system)| system.trim_end().trim_end_matches('.') == "1612");
    let numbered = line
        .split_once("Note: 1: 1714 ")
        .and_then(|(_, rest)| rest.strip_prefix("2: "))
        .and_then(|rest| rest.rsplit_once(" 3: "))
        .is_some_and(|(_, system)| system.trim_end() == "1612");
    rendered || numbered
}

/// The message a failed launch condition shows, from a line logged while `LaunchConditions`
/// runs: the text after "Product: <name> -- ", or a line of its own that is not one of the
/// installer's notes.
fn condition_message(line: &str) -> Option<String> {
    let shown = match line.split_once("Product: ").and_then(|(_, rest)| rest.split_once(" -- ")) {
        Some((_, shown)) => shown,
        None if line.starts_with("MSI (") || line.starts_with("Action") || line.starts_with("Info ") || line.starts_with("Property(") => return None,
        None => line,
    };
    let shown: String = shown.chars().filter(|c| !c.is_control()).take(MAX_MESSAGE_CHARS).collect();
    let shown = shown.trim();
    (!shown.is_empty()).then(|| shown.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &str = "Remove the copy installed for the other context first.";

    /// A made-up log in the layout Windows Installer writes: a launch condition that fails with
    /// `MESSAGE`.
    fn failed_launch_condition() -> String {
        [
            "=== Verbose logging started: 1/2/2026  10:00:00  Build type: SHIP UNICODE 5.00.10011.00  Calling process: C:\\Windows\\system32\\msiexec.exe ===",
            "MSI (s) (10:20) [10:00:01:000]: Doing action: LaunchConditions",
            "Action start 10:00:01: LaunchConditions.",
            &format!("MSI (s) (10:20) [10:00:01:010]: Product: ExampleCraft -- {MESSAGE}"),
            "",
            MESSAGE,
            "Action ended 10:00:01: LaunchConditions. Return value 3.",
            "Action ended 10:00:01: INSTALL. Return value 3.",
            "MSI (s) (10:20) [10:00:01:020]: Product: ExampleCraft -- Installation failed.",
            "=== Verbose logging stopped: 1/2/2026  10:00:01 ===",
        ]
        .join("\r\n")
    }

    /// A made-up log of an upgrade that can't remove the version it replaces.
    fn missing_cached_package() -> String {
        [
            "Action start 10:00:02: RemoveExistingProducts.",
            "MSI (s) (10:20) [10:00:02:000]: Warning: Local cached package 'C:\\Windows\\Installer\\1a2b3c.msi' is missing.",
            "MSI (s) (10:20) [10:00:02:010]: Product: ExampleCraft -- Error 1714. The older version of ExampleCraft cannot be removed. System Error 1612.",
            "Action ended 10:00:02: RemoveExistingProducts. Return value 3.",
        ]
        .join("\r\n")
    }

    fn utf16(text: &str) -> Vec<u8> {
        [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect()
    }

    fn written(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("examplecraft-1.0.0-windows-x64.log");
        std::fs::write(&log, bytes).unwrap();
        (dir, log)
    }

    #[test]
    fn a_failed_launch_condition_gives_its_message() {
        let expected = Some(Cause::LaunchCondition(Some(MESSAGE.into())));
        for bytes in [utf16(&failed_launch_condition()), failed_launch_condition().into_bytes()] {
            let (_dir, log) = written(&bytes);
            assert_eq!(cause(&log), expected);
        }
        // Without a "Product: ... -- " line, the condition's own line is its message.
        let plain = failed_launch_condition().lines().filter(|line| !line.contains(" -- ")).collect::<Vec<_>>().join("\n");
        assert_eq!(find(&plain), expected);
        // A log that names no message still names the cause.
        let bare = "Action start 10:00:01: LaunchConditions.\nAction ended 10:00:01: LaunchConditions. Return value 3.";
        assert_eq!(find(bare), Some(Cause::LaunchCondition(None)));
    }

    #[test]
    fn a_missing_cached_package_is_named() {
        let (_dir, log) = written(&utf16(&missing_cached_package()));
        assert_eq!(cause(&log), Some(Cause::CachedPackageMissing));
        for line in missing_cached_package().lines().filter(|line| line.contains("1714")) {
            assert_eq!(find(line), Some(Cause::CachedPackageMissing), "{line}");
        }
        assert_eq!(find("Warning: Local cached package 'old.msi' is missing."), None);
        // The same record by its numbered fields, without the English message: field 1 is the
        // error, 2 the product and 3 the system error.
        let numbered = [
            "Action start 10:00:02: RemoveExistingProducts.",
            "MSI (s) (10:20) [10:00:02:010]: Note: 1: 1714 2: ExampleCraft 1612 3: 1612 ",
            "Action ended 10:00:02: RemoveExistingProducts. Return value 3.",
        ]
        .join("\r\n");
        let (_dir, log) = written(&utf16(&numbered));
        assert_eq!(cause(&log), Some(Cause::CachedPackageMissing));
        assert_eq!(find("Note: 1: 1714 2:  3: 1612"), Some(Cause::CachedPackageMissing), "an empty product name");
    }

    #[test]
    fn a_recovered_cache_warning_does_not_name_an_unrelated_failure() {
        let log = [
            "Warning: Local cached package 'C:\\Windows\\Installer\\old.msi' is missing.",
            "SOURCEMGMT: Source is valid.",
            "Action ended 10:00:02: ResolveSource. Return value 1.",
            "Error 1303. The installer has insufficient privileges to access this directory.",
            "Action ended 10:00:03: InstallFiles. Return value 3.",
            "MainEngineThread is returning 1603",
        ]
        .join("\r\n");
        assert_eq!(find(&log), None);
    }

    #[test]
    fn a_log_without_a_known_cause_names_none() {
        let passed = failed_launch_condition().replace("LaunchConditions. Return value 3.", "LaunchConditions. Return value 1.");
        assert_eq!(find(&passed), None, "a launch condition that passed");
        assert_eq!(find("Error 1714. The older version of ExampleCraft cannot be removed. System Error 1603."), None, "1714 for another reason");
        // A product name that holds the number, or the words, is not the system error.
        for line in [
            "Product: Craft1612 -- Error 1714. The older version of Craft1612 cannot be removed. System Error 1603.",
            "Product: System Error 1612 -- Error 1714. The older version of System Error 1612 cannot be removed. System Error 1603.",
            "Error 1714. The older version of ExampleCraft cannot be removed. System Error 16120.",
            "Error 17140. The older version of ExampleCraft cannot be removed. System Error 1612.",
            "MSI (s) (10:20) [10:00:02:010]: Note: 1: 1714 2: Craft1612 3: 1603",
            "MSI (s) (10:20) [10:00:02:010]: Note: 1: 1714 2: Craft 3: 1612 3: 1603",
            "MSI (s) (10:20) [10:00:02:010]: Note: 1: 1612 2: ExampleCraft 3: 1714",
            "MSI (s) (10:20) [10:00:02:010]: Note: 1: 17140 2: ExampleCraft 3: 1612",
        ] {
            assert_eq!(find(line), None, "{line}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cause(&dir.path().join("missing.log")), None);
        assert_eq!(cause(dir.path()), None, "a folder");
        for hostile in [Vec::new(), vec![0xFF, 0xFE, 0x41], vec![0; 4096], (0..=255).collect(), vec![0xFF; 70_000]] {
            let (_dir, log) = written(&hostile);
            assert_eq!(cause(&log), None);
        }
    }

    #[test]
    fn a_long_or_odd_message_is_trimmed() {
        let long = "x".repeat(MAX_MESSAGE_CHARS * 4);
        let log = failed_launch_condition().replace(MESSAGE, &format!("{long}\u{7}"));
        let Some(Cause::LaunchCondition(Some(message))) = find(&log) else { panic!("no message") };
        assert!(message.chars().count() <= MAX_MESSAGE_CHARS && !message.chars().any(char::is_control), "{message:?}");
    }

    #[test]
    fn only_the_start_and_the_end_of_a_large_log_are_read() {
        let mut bytes = failed_launch_condition().into_bytes();
        bytes.resize(MAX_LOG_BYTES as usize, b' ');
        bytes.extend_from_slice(missing_cached_package().as_bytes());
        let (_dir, log) = written(&bytes);
        assert_eq!(cause(&log), Some(Cause::LaunchCondition(Some(MESSAGE.into()))));
        // A removal logged near the end of a large log is found there, in either encoding.
        let mut late = vec![b' '; MAX_LOG_BYTES as usize];
        late.extend_from_slice(missing_cached_package().as_bytes());
        let (_dir, log) = written(&late);
        assert_eq!(cause(&log), Some(Cause::CachedPackageMissing), "in the end of the log");
        // A log of odd length whose last line ends at its last byte, past both parts' length.
        let mut odd = vec![b' '; 2 * MAX_LOG_BYTES as usize + 1];
        odd.extend_from_slice(b"Error 1714. The older version of ExampleCraft cannot be removed. System Error 1612");
        let (_dir, log) = written(&odd);
        assert_eq!(cause(&log), Some(Cause::CachedPackageMissing), "the last byte is read");
        let mut late = utf16(&" ".repeat(MAX_LOG_BYTES as usize / 2 + 1));
        late.extend(missing_cached_package().encode_utf16().flat_map(u16::to_le_bytes));
        let (_dir, log) = written(&late);
        assert_eq!(cause(&log), Some(Cause::CachedPackageMissing), "in the end of a UTF-16 log");
        // Between the start and the end of a log larger than both, nothing is read.
        let mut middle = vec![b' '; MAX_LOG_BYTES as usize];
        middle.extend_from_slice(missing_cached_package().as_bytes());
        middle.resize(middle.len() + MAX_LOG_BYTES as usize, b' ');
        let (_dir, log) = written(&middle);
        assert_eq!(cause(&log), None, "past the parts that are read");
    }

    #[test]
    fn a_line_across_the_boundary_of_a_log_the_parts_cover_is_read_whole() {
        // A log longer than one part and shorter than two, so the end starts where the start
        // stops, with the 1714 record split between them.
        let record = "MSI (s) (10:20) [10:00:02:010]: Product: ExampleCraft -- Error 1714. The older version of ExampleCraft cannot be removed. System Error 1612.";
        let split = record.find("1612.").unwrap();
        let mut bytes = vec![b' '; MAX_LOG_BYTES as usize - split];
        bytes.extend_from_slice(record.as_bytes());
        bytes.extend_from_slice(b"\r\nAction ended 10:00:02: RemoveExistingProducts. Return value 3.\r\n");
        bytes.resize(bytes.len() + 1024, b' ');
        let (_dir, log) = written(&bytes);
        assert_eq!(cause(&log), Some(Cause::CachedPackageMissing));
        // The same in UTF-16, split at the same character, with the end read from an odd length.
        let mut wide = utf16(&" ".repeat(MAX_LOG_BYTES as usize / 2 - 1 - split));
        wide.extend(record.encode_utf16().flat_map(u16::to_le_bytes));
        wide.extend(" ".repeat(1024).encode_utf16().flat_map(u16::to_le_bytes));
        wide.push(b' ');
        let (_dir, log) = written(&wide);
        assert_eq!(cause(&log), Some(Cause::CachedPackageMissing), "in a UTF-16 log");
    }
}
