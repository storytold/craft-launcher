//! Starting programs: the Craft Apps themselves, the file manager, and helper tools.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A `Command` that won't flash a console window on Windows, and clears AppImage-injected
/// environment variables that corrupt system helper binaries and external applications.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    clean_appimage_env(&mut cmd);
    cmd
}

/// Cleans AppImage runtime environment variables so child processes don't load incompatible
/// libraries from the launcher's bundled AppImage.
#[cfg(unix)]
pub fn clean_appimage_env(cmd: &mut Command) {
    if std::env::var_os("APPIMAGE").is_some() {
        for var in [
            "LD_LIBRARY_PATH",
            "LD_PRELOAD",
            "APPDIR",
            "APPIMAGE",
            "ARGV0",
            "OWD",
            "GDK_PIXBUF_MODULE_FILE",
            "GIO_MODULE_DIR",
            "GTK_PATH",
            "PYTHONHOME",
            "QT_PLUGIN_PATH",
        ] {
            cmd.env_remove(var);
        }
    }
}

/// Runs a helper and returns its standard output when it succeeds within `timeout`.
pub fn capture(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    run(program, args, timeout).ok()
}

/// Runs a helper and returns its standard output when it succeeds within `timeout`; otherwise
/// the reason, in the helper's own words (its standard error) where it gave one.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start {program} ({e})"))?;
    // Read both pipes while it runs, so a chatty helper can't block on a full one.
    let (stdout, stderr) = (read_all(child.stdout.take()), read_all(child.stderr.take()));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} didn't finish within {} seconds", timeout.as_secs()));
            }
        }
    };
    let text = |pipe: std::thread::JoinHandle<Vec<u8>>| String::from_utf8_lossy(&pipe.join().unwrap_or_default()).into_owned();
    let (out, err) = (text(stdout), text(stderr));
    if status.success() {
        return Ok(out);
    }
    let reason = err.trim();
    Err(if reason.is_empty() { format!("{program} failed ({status})") } else { reason.to_string() })
}

/// Reads a child's pipe to the end (at most 8 MB) on its own thread.
fn read_all<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(pipe) = pipe {
            let _ = std::io::Read::read_to_end(&mut std::io::Read::take(pipe, 8 << 20), &mut out);
        }
        out
    })
}

/// Starts `cmd` without waiting for it, and reaps it when it exits.
pub fn spawn_detached(mut cmd: Command) -> Result<(), String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| format!("couldn't start it ({e})"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Whether this process is running inside a Flatpak container sandbox.
pub fn in_sandbox() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists()
}

/// A command to run `flatpak`, delegating to the host via `flatpak-spawn --host` if inside a sandbox.
pub fn flatpak_command() -> Command {
    if in_sandbox() {
        let mut cmd = command("flatpak-spawn");
        cmd.args(["--host", "flatpak"]);
        cmd
    } else {
        command("flatpak")
    }
}

/// Runs a flatpak command and captures its standard output.
pub fn capture_flatpak(args: &[&str], timeout: Duration) -> Option<String> {
    if in_sandbox() {
        let mut spawn_args = vec!["--host", "flatpak"];
        spawn_args.extend_from_slice(args);
        capture("flatpak-spawn", &spawn_args, timeout)
    } else {
        capture("flatpak", args, timeout)
    }
}

/// Whether Flatpak is available on the machine (or on the host when sandboxed).
pub fn is_flatpak_available() -> bool {
    if in_sandbox() {
        capture("flatpak-spawn", &["--host", "flatpak", "--version"], Duration::from_secs(3)).is_some()
    } else {
        which_on_path("flatpak").is_some()
    }
}

fn which_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Starts an app binary from its own folder.
pub fn launch_binary(exe: &Path, args: &[&str]) -> Result<(), String> {
    let mut cmd = if in_sandbox() {
        let mut c = command("flatpak-spawn");
        c.arg("--host").arg(exe);
        c
    } else {
        command(exe)
    };
    cmd.args(args);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    spawn_detached(cmd)
}

/// Opens a file, folder or URL with the system's default handler.
pub fn open(target: &std::ffi::OsStr) -> Result<(), String> {
    let mut cmd = if cfg!(target_os = "macos") {
        command("open")
    } else if cfg!(windows) {
        let mut c = command("explorer");
        c.arg(target);
        return spawn_detached(c);
    } else if in_sandbox() {
        let mut c = command("flatpak-spawn");
        c.arg("--host").arg("xdg-open");
        c
    } else {
        command("xdg-open")
    };
    cmd.arg(target);
    spawn_detached(cmd)
}

/// Shows `path` selected in the file manager.
pub fn reveal(path: &Path) -> Result<(), String> {
    if cfg!(target_os = "macos") {
        let mut cmd = command("open");
        cmd.arg("-R").arg(path);
        spawn_detached(cmd)
    } else if cfg!(windows) {
        reveal_in_explorer(path)
    } else {
        open(path.parent().unwrap_or(path).as_os_str())
    }
}

#[cfg(windows)]
fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut cmd = command("explorer");
    // Passed raw: `arg` would wrap all of `/select,...` in quotes when the path has a space
    // (`C:\Program Files\...`), and Explorer then ignores the switch and opens Documents.
    // A quote in the path would end the quoted part early and let the rest reach Explorer as
    // switches; no Windows path has one, so refuse it.
    let arg = explorer_select_arg(path).ok_or_else(|| format!("{} isn't a usable path", path.display()))?;
    cmd.raw_arg(arg);
    spawn_detached(cmd)
}

#[cfg(not(windows))]
fn reveal_in_explorer(_path: &Path) -> Result<(), String> {
    Err("Explorer is only on Windows".into())
}

/// Explorer's `/select,"<path>"` argument as it goes on the command line: only the path is quoted,
/// and forward slashes become backslashes, which Explorer needs to find the item. `None` for a path
/// with a `"` in it.
#[cfg_attr(not(windows), allow(dead_code))]
fn explorer_select_arg(path: &Path) -> Option<std::ffi::OsString> {
    let s = path.to_str()?;
    if s.contains('"') {
        return None;
    }
    let mut arg = std::ffi::OsString::from("/select,\"");
    arg.push(s.replace('/', "\\"));
    arg.push("\"");
    Some(arg)
}

/// Whether a process started from inside `path` (an exe, an AppImage, or a `.app`) is running.
pub fn is_running(path: &Path) -> bool {
    if cfg!(windows) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
        let filter = format!("IMAGENAME eq {name}");
        capture("tasklist", &["/FI", &filter, "/NH", "/FO", "CSV"], Duration::from_secs(5))
            .is_some_and(|out| out.to_ascii_lowercase().contains(&name.to_ascii_lowercase()))
    } else {
        let Some(p) = path.to_str() else { return false };
        let pattern = pgrep_pattern(p);
        // pgrep exits 0 when something matches; it never matches itself.
        if in_sandbox() {
            capture("flatpak-spawn", &["--host", "pgrep", "-f", "--", &pattern], Duration::from_secs(5)).is_some()
        } else {
            capture("pgrep", &["-f", "--", &pattern], Duration::from_secs(5)).is_some()
        }
    }
}

/// Quote the path's metacharacters for pgrep's extended regular expression.
fn pgrep_pattern(path: &str) -> String {
    let mut pattern = String::with_capacity(path.len());
    for c in path.chars() {
        if matches!(c, '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::process::Child;

    use super::*;

    struct RunningProbe(Child);

    impl RunningProbe {
        fn start(path: &str) -> Self {
            let child = command("/bin/sh")
                .args(["-c", "printf 'ready\\n'; read line", path])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut probe = Self(child);
            let mut ready = String::new();
            BufReader::new(probe.0.stdout.take().unwrap()).read_line(&mut ready).unwrap();
            assert_eq!(ready, "ready\n");
            probe
        }
    }

    impl Drop for RunningProbe {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn running_paths_are_matched_literally() {
        let root = format!("/tmp/artcraft-launcher-process-{}", std::process::id());
        for (actual, requested, expected) in [
            ("plain/PhotoCraft.app", "plain/PhotoCraft.app", true),
            ("Apps (work)/PhotoCraft.app", "Apps (work)/PhotoCraft.app", true),
            ("Apps [work]/PhotoCraft.app", "Apps [work]/PhotoCraft.app", true),
            ("other/PhotoCraftXapp", "other/PhotoCraft.app", false),
            ("symbols/应用.^$*+?()[]{}|\\.app", "symbols/应用.^$*+?()[]{}|\\.app", true),
        ] {
            let actual = format!("{root}/{actual}");
            let requested = format!("{root}/{requested}");
            let _probe = RunningProbe::start(&actual);
            assert_eq!(is_running(Path::new(&requested)), expected, "{requested}");
        }
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;

    /// A shell command line, for the platform's shell.
    fn shell(script: &str) -> (&'static str, [&str; 2]) {
        if cfg!(windows) { ("cmd", ["/C", script]) } else { ("/bin/sh", ["-c", script]) }
    }

    #[test]
    fn a_failing_helper_reports_its_own_error() {
        let (program, args) = shell(if cfg!(windows) { "echo no such image 1>&2 & exit /b 3" } else { "echo no such image >&2; exit 3" });
        assert_eq!(run(program, &args, Duration::from_secs(30)), Err("no such image".into()));
        assert_eq!(capture(program, &args, Duration::from_secs(30)), None);
    }

    #[test]
    fn a_silent_failure_and_a_success() {
        let (program, args) = shell("exit 4");
        let err = run(program, &args, Duration::from_secs(30)).unwrap_err();
        assert!(err.starts_with(&format!("{program} failed")), "{err}");
        let (program, args) = shell("echo hello");
        assert_eq!(run(program, &args, Duration::from_secs(30)).map(|out| out.trim().to_string()), Ok("hello".into()));
    }
}

#[cfg(test)]
mod explorer_tests {
    use super::*;

    #[test]
    fn explorer_select_quotes_only_the_path() {
        let arg = explorer_select_arg(Path::new(r"C:\Program Files\PhotoCraft\photocraft.exe"));
        assert_eq!(arg.unwrap(), r#"/select,"C:\Program Files\PhotoCraft\photocraft.exe""#);
    }

    #[test]
    fn explorer_select_without_spaces() {
        let arg = explorer_select_arg(Path::new(r"C:\Apps\photocraft.exe"));
        assert_eq!(arg.unwrap(), r#"/select,"C:\Apps\photocraft.exe""#);
        assert_eq!(explorer_select_arg(Path::new(r#"C:\Apps" /root,C:\x.exe"#)), None);
    }

    #[test]
    fn explorer_select_uses_backslashes() {
        let arg = explorer_select_arg(Path::new("C:/Users/A B/AppData/Local/ArtCraft Launcher/apps/photocraft.exe"));
        assert_eq!(arg.unwrap(), r#"/select,"C:\Users\A B\AppData\Local\ArtCraft Launcher\apps\photocraft.exe""#);
    }
}
