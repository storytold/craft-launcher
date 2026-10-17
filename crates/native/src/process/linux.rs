//! Native Linux process checks read command-line bytes rather than locale-dependent display text.
use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) fn is_running(path: &Path) -> io::Result<bool> {
    scan(Path::new("/proc"), path.as_os_str().as_bytes(), std::process::id(), Instant::now() + Duration::from_secs(5))
}

fn scan(root: &Path, query: &[u8], own_pid: u32, deadline: Instant) -> io::Result<bool> {
    let mut matcher = Matcher::new(query)?;
    let mut incomplete = None;
    for entry in std::fs::read_dir(root)? {
        check_deadline(deadline)?;
        let entry = entry?;
        let name = entry.file_name();
        let Some(pid) = numeric_pid(&name) else { continue };
        if pid == own_pid {
            continue;
        }
        matcher.reset();
        let result = std::fs::File::open(entry.path().join("cmdline")).and_then(|file| matches_command_line(file, &mut matcher, deadline));
        match result {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            // Processes can exit during the scan. As with pgrep, inaccessible processes are
            // outside this best-effort observation, including hidepid-protected processes.
            Err(error) if process_unavailable(&error) => {}
            Err(error) => incomplete = Some(error),
        }
    }
    check_deadline(deadline)?;
    match incomplete {
        Some(error) => Err(error),
        None => Ok(false),
    }
}

fn process_unavailable(error: &io::Error) -> bool {
    // ESRCH is Linux errno 3. A task may disappear after its cmdline file was opened.
    error.raw_os_error() == Some(3) || matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied)
}

fn numeric_pid(name: &OsStr) -> Option<u32> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline { Err(io::Error::new(io::ErrorKind::TimedOut, "process scan did not finish within five seconds")) } else { Ok(()) }
}

fn matches_command_line(mut reader: impl Read, matcher: &mut Matcher<'_>, deadline: Instant) -> io::Result<bool> {
    let mut buffer = [0; 8192];
    let mut separators = 0usize;
    loop {
        check_deadline(deadline)?;
        let count = match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            check_deadline(deadline)?;
            // procps drops all terminal NULs, including empty final arguments. Actual spaces
            // in the final argument remain searchable; they have already reached the matcher.
            return Ok(false);
        }
        for &byte in &buffer[..count] {
            if byte == 0 {
                separators =
                    separators.checked_add(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "too many command-line separators"))?;
                continue;
            }
            while separators > 0 {
                if separators % buffer.len() == 0 {
                    check_deadline(deadline)?;
                }
                if matcher.feed(b' ') {
                    return Ok(true);
                }
                separators -= 1;
            }
            if matcher.feed(byte) {
                return Ok(true);
            }
        }
    }
}

/// A linear-time literal search carrying only query-sized state between fixed-size reads.
struct Matcher<'a> {
    query: &'a [u8],
    prefix: Vec<usize>,
    matched: usize,
}

impl<'a> Matcher<'a> {
    fn new(query: &'a [u8]) -> io::Result<Self> {
        if query.is_empty() || query.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "application path is empty or contains NUL"));
        }
        let mut prefix = Vec::new();
        prefix.try_reserve_exact(query.len()).map_err(io::Error::other)?;
        prefix.resize(query.len(), 0);
        let mut matched = 0;
        for index in 1..query.len() {
            while matched > 0 && query.get(index) != query.get(matched) {
                matched = prefix[matched - 1];
            }
            if query.get(index) == query.get(matched) {
                matched += 1;
            }
            prefix[index] = matched;
        }
        Ok(Self { query, prefix, matched: 0 })
    }

    fn reset(&mut self) {
        self.matched = 0;
    }

    fn feed(&mut self, byte: u8) -> bool {
        while self.matched > 0 && self.query.get(self.matched) != Some(&byte) {
            self.matched = self.prefix[self.matched - 1];
        }
        if self.query.get(self.matched) == Some(&byte) {
            self.matched += 1;
        }
        self.matched == self.query.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    fn matches(bytes: &[u8], query: &[u8]) -> bool {
        matches_command_line(bytes, &mut Matcher::new(query).unwrap(), deadline()).unwrap()
    }

    #[test]
    fn literal_command_lines_preserve_wrappers_and_argument_spacing() {
        let bytes = b"/bin/sh\0/Apps (work)/\xe5\xba\x94\xe7\x94\xa8.AppImage\0--name\0\0Inter\0";
        assert!(matches(bytes, b"/Apps (work)/\xe5\xba\x94\xe7\x94\xa8.AppImage"));
        assert!(matches(bytes, b"--name  Inter"));
        assert!(!matches(bytes, b"Inter "));
        assert!(!matches(bytes, b"/Apps (work)/\xe5\xba\x94\xe7\x94\xa8XAppImage"));
        assert!(matches(b"/fonts/\xff/app\0", b"/fonts/\xff/app"));
        assert!(matches(b"prefix\0/Apps/PhotoCraft.app/Contents/MacOS/app\0", b"/Apps/PhotoCraft.app"));
        assert!(!matches(b"", b"app"));
        assert!(!matches(b"/app\0\0\0", b"/app "));
        assert!(matches(b"/app \0\0\0", b"/app "));
        assert!(matches(b"/app\0\0tail\0\0", b"/app  tail"));
    }

    #[test]
    fn matches_across_reads_without_retaining_the_command_line() {
        let mut bytes = vec![b'x'; 8191];
        bytes.extend_from_slice(b"/path with spaces/app\0");
        assert!(matches(&bytes, b"/path with spaces/app"));
        assert!(matches(b"aaaaab\0", b"aaab"));
        let mut separated = vec![b'x'; 8191];
        separated.extend_from_slice(b"\0\0tail\0\0");
        assert!(matches(&separated, b"x  tail"));
        assert!(!matches(&separated, b"tail "));
        assert!(Matcher::new(b"").is_err());
        assert!(Matcher::new(b"/bad\0path").is_err());
    }

    #[test]
    fn incomplete_scans_are_errors_and_names_are_numeric() {
        let mut matcher = Matcher::new(b"app").unwrap();
        assert_eq!(matches_command_line(&b"app\0"[..], &mut matcher, Instant::now()).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(scan(Path::new("/proc/no-such-artcraft-process-root"), b"app", 0, deadline()).is_err());
        assert_eq!(numeric_pid(OsStr::new("123")), Some(123));
        assert_eq!(numeric_pid(OsStr::new("123x")), None);
        assert_eq!(numeric_pid(OsStr::new("self")), None);
        assert_eq!(numeric_pid(OsStr::new("")), None);
        assert!(process_unavailable(&io::Error::from_raw_os_error(3)));
        assert!(process_unavailable(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(process_unavailable(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(!process_unavailable(&io::Error::from(io::ErrorKind::Other)));
    }

    #[test]
    fn scanning_skips_self_and_disappeared_entries() {
        let temp = tempfile::tempdir().unwrap();
        for pid in [12, 13] {
            std::fs::create_dir(temp.path().join(pid.to_string())).unwrap();
        }
        std::fs::write(temp.path().join("12/cmdline"), b"/app\0").unwrap();
        assert!(!scan(temp.path(), b"/app", 12, deadline()).unwrap());
        std::fs::write(temp.path().join("13/cmdline"), b"/app\0").unwrap();
        assert!(scan(temp.path(), b"/app", 12, deadline()).unwrap());
        std::fs::remove_file(temp.path().join("13/cmdline")).unwrap();
        std::fs::create_dir(temp.path().join("13/cmdline")).unwrap();
        assert!(scan(temp.path(), b"/app", 12, deadline()).is_err());
    }
}
