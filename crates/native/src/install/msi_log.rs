//! What a Windows Installer log (`/l*v`) says stopped an install that ended with error 1603,
//! which by itself only says that the installation failed.

use std::io::Read;
use std::path::Path;

/// How much of a log is read. A launch condition and the removal of the version being replaced
/// both run before any file is copied, so their lines are near the start of the log.
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

/// The cause the log at `log` names, reading at most [`MAX_LOG_BYTES`] of it. `None` when the
/// log can't be read or names no cause this module knows.
pub(super) fn cause(log: &Path) -> Option<Cause> {
    let file = std::fs::File::open(log).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES).read_to_end(&mut bytes).ok()?;
    find(&text(&bytes))
}

/// The log's text. Windows Installer writes it as UTF-16 (little-endian, usually with a byte
/// order mark) or in the system's code page; text in a code page reads correctly here only
/// where it is ASCII, which covers the lines [`find`] looks for.
fn text(bytes: &[u8]) -> String {
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || matches!(bytes, [first, 0, ..] if *first != 0);
    if !utf16 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    String::from_utf16_lossy(&units)
}

/// The cause the log's text names: a failed `LaunchConditions` action ("Return value 3"), or a
/// missing cached package ("Local cached package ... is missing", or error 1714 with system
/// error 1612).
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
        cached_missing |=
            (line.contains("Local cached package") && line.contains("is missing")) || (line.contains("Error 1714") && line.contains("1612"));
    }
    cached_missing.then_some(Cause::CachedPackageMissing)
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
        // Either line alone is enough.
        for line in missing_cached_package().lines().filter(|line| line.contains("Local cached") || line.contains("1714")) {
            assert_eq!(find(line), Some(Cause::CachedPackageMissing), "{line}");
        }
    }

    #[test]
    fn a_log_without_a_known_cause_names_none() {
        let passed = failed_launch_condition().replace("LaunchConditions. Return value 3.", "LaunchConditions. Return value 1.");
        assert_eq!(find(&passed), None, "a launch condition that passed");
        assert_eq!(find("Error 1714. The older version of ExampleCraft cannot be removed. System Error 1603."), None, "1714 for another reason");
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
    fn only_the_start_of_a_large_log_is_read() {
        let mut bytes = failed_launch_condition().into_bytes();
        bytes.resize(MAX_LOG_BYTES as usize, b' ');
        bytes.extend_from_slice(missing_cached_package().as_bytes());
        let (_dir, log) = written(&bytes);
        assert_eq!(cause(&log), Some(Cause::LaunchCondition(Some(MESSAGE.into()))));
        let mut late = vec![b' '; MAX_LOG_BYTES as usize];
        late.extend_from_slice(missing_cached_package().as_bytes());
        let (_dir, log) = written(&late);
        assert_eq!(cause(&log), None, "past the part that is read");
    }
}
