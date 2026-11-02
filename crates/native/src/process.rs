//! Starting programs: the Craft Apps themselves, the file manager, and helper tools.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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
    let mut cmd = command(program);
    cmd.args(args);
    run_command(cmd, program, timeout)
}

/// Runs a configured helper with bounded output and a timeout.
pub fn run_command(mut cmd: Command, program: &str, timeout: Duration) -> Result<String, String> {
    let mut child =
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("couldn't start {program} ({e})"))?;
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
    // A descendant may retain a pipe after the direct child exits. Keep reader completion
    // under the same deadline; on timeout the readers detach, as in the child timeout path.
    while !stdout.is_finished() || !stderr.is_finished() {
        if start.elapsed() >= timeout {
            return Err(format!("{program} didn't finish within {} seconds", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let text = |pipe: std::thread::JoinHandle<Vec<u8>>| String::from_utf8_lossy(&pipe.join().unwrap_or_default()).into_owned();
    let (out, err) = (text(stdout), text(stderr));
    if status.success() {
        return Ok(out);
    }
    let reason = err.trim();
    Err(if reason.is_empty() { format!("{program} failed ({status})") } else { reason.to_string() })
}

/// Captures at most 8 MB of a child's pipe and drains the rest on its own thread.
fn read_all<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = std::io::Read::read_to_end(&mut std::io::Read::take(&mut pipe, 8 << 20), &mut out);
            // Keep reading without retaining more output: closing a full pipe early can make a
            // successful helper fail with a broken pipe (SIGPIPE on Unix).
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
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

/// Waits up to `timeout` for the process `pid` to exit, and says whether it has. A process that
/// can't be opened, because it's gone or never was, counts as exited.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    use winsafe::{HPROCESS, co};
    let Ok(process) = HPROCESS::OpenProcess(co::PROCESS::SYNCHRONIZE, false, pid) else { return true };
    // u32::MAX would mean INFINITE.
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    process.WaitForSingleObject(Some(ms)).is_ok_and(|w| w == co::WAIT::OBJECT_0)
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

static DISCORD_HANDLER_DETECTED: AtomicBool = AtomicBool::new(false);
static DETECTION_STARTED: AtomicBool = AtomicBool::new(false);

/// Starts asynchronous detection of a registered Discord URL scheme handler in the background.
pub fn ensure_discord_detection_started() {
    if !DETECTION_STARTED.swap(true, Ordering::Relaxed) {
        let _ = std::thread::Builder::new().name("discord-detect".into()).spawn(|| {
            let detected = detect_discord_scheme_handler();
            DISCORD_HANDLER_DETECTED.store(detected, Ordering::Relaxed);
        });
    }
}

/// Whether a registered Discord scheme handler has been detected on this machine.
///
/// Triggers background detection if not yet started, and returns immediately without
/// blocking the calling thread.
pub fn is_discord_installed() -> bool {
    ensure_discord_detection_started();
    DISCORD_HANDLER_DETECTED.load(Ordering::Relaxed)
}

/// The URL to use for Discord links: deep-links to the app if a handler is registered,
/// else the web invite. Non-blocking (detection runs in the background).
pub fn discord_url() -> &'static str {
    if is_discord_installed() { artcraft_launcher_core::catalog::DISCORD_APP_URL } else { artcraft_launcher_core::catalog::DISCORD_URL }
}

#[cfg(windows)]
fn detect_discord_scheme_handler() -> bool {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};

    for hive in [HKEY_CURRENT_USER, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE] {
        let subkey = if hive == HKEY_CLASSES_ROOT { r"discord\shell\open\command" } else { r"Software\Classes\discord\shell\open\command" };
        if let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(subkey, KEY_READ)
            && let Ok(val) = key.get_value::<String, _>("")
            && !val.trim().is_empty()
        {
            return true;
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn detect_discord_scheme_handler() -> bool {
    macos_discord_candidates(dirs::home_dir().as_deref()).iter().any(|app_dir| {
        let plist_path = app_dir.join("Contents/Info.plist");
        if let Ok(content) = std::fs::read_to_string(&plist_path) { content.contains("<string>discord</string>") } else { false }
    })
}

#[cfg(any(target_os = "macos", test))]
fn macos_discord_candidates(home: Option<&Path>) -> Vec<std::path::PathBuf> {
    let apps = ["Discord.app", "Discord Canary.app", "Discord PTB.app", "Discord Development.app"];
    let mut dirs = vec![std::path::PathBuf::from("/Applications")];
    if let Some(home) = home {
        dirs.push(home.join("Applications"));
    }
    let mut out = Vec::new();
    for dir in dirs {
        for app in apps {
            out.push(dir.join(app));
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn detect_discord_scheme_handler() -> bool {
    // 1. Query xdg-mime for the registered scheme handler (on host or in flatpak sandbox).
    let mime_query = if in_sandbox() {
        capture("flatpak-spawn", &["--host", "xdg-mime", "query", "default", "x-scheme-handler/discord"], Duration::from_millis(800))
    } else {
        capture("xdg-mime", &["query", "default", "x-scheme-handler/discord"], Duration::from_millis(800))
    };
    if let Some(out) = mime_query {
        let trimmed = out.trim();
        if !trimmed.is_empty() && trimmed.ends_with(".desktop") {
            return true;
        }
    }

    // 2. Fall back to inspecting installed desktop files to ensure MimeType includes
    // x-scheme-handler/discord (preventing silent failures as in Arch FS#76847).
    for path in linux_discord_candidates(dirs::home_dir().as_deref()) {
        if path.extension().is_some_and(|ext| ext == "desktop")
            && let Ok(content) = std::fs::read_to_string(&path)
            && content.lines().any(|line| {
                let line = line.trim();
                line.starts_with("MimeType=") && line.contains("x-scheme-handler/discord")
            })
        {
            return true;
        }
    }

    false
}

#[cfg(any(target_os = "linux", test))]
fn linux_discord_candidates(home: Option<&Path>) -> Vec<std::path::PathBuf> {
    let mut files = vec![
        std::path::PathBuf::from("/usr/share/applications/discord.desktop"),
        std::path::PathBuf::from("/usr/share/applications/discord-canary.desktop"),
        std::path::PathBuf::from("/usr/share/applications/discord-ptb.desktop"),
        std::path::PathBuf::from("/var/lib/flatpak/exports/share/applications/com.discordapp.Discord.desktop"),
        std::path::PathBuf::from("/var/lib/flatpak/exports/share/applications/com.discordapp.DiscordCanary.desktop"),
        std::path::PathBuf::from("/var/lib/snapd/desktop/applications/discord_discord.desktop"),
        std::path::PathBuf::from("/snap/bin/discord"),
    ];
    if let Some(home) = home {
        files.push(home.join(".local/share/applications/discord.desktop"));
        files.push(home.join(".local/share/applications/discord-canary.desktop"));
        files.push(home.join(".local/share/applications/discord-ptb.desktop"));
        files.push(home.join(".local/share/flatpak/exports/share/applications/com.discordapp.Discord.desktop"));
        files.push(home.join(".local/share/flatpak/exports/share/applications/com.discordapp.DiscordCanary.desktop"));
    }
    files
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn detect_discord_scheme_handler() -> bool {
    false
}

fn which_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Starts an app binary from its own folder.
pub fn launch_binary(exe: &Path, args: &[&str]) -> Result<(), String> {
    launch_binary_with_env(exe, args, &[])
}

/// Starts an app binary from its own folder with `env` added to its environment. Inside the
/// sandbox `flatpak-spawn` does not pass the launcher's environment on, so each one becomes an
/// `--env=` argument instead.
pub fn launch_binary_with_env(exe: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)]) -> Result<(), String> {
    let mut cmd = if in_sandbox() {
        let mut c = command("flatpak-spawn");
        c.arg("--host");
        c.args(env_args(env));
        c.arg(exe);
        c
    } else {
        let mut c = command(exe);
        c.envs(env.iter().copied());
        c
    };
    cmd.args(args);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    spawn_detached(cmd)
}

/// `--env=NAME=VALUE` for each of `env`, as `flatpak run` and `flatpak-spawn` take them.
pub fn env_args(env: &[(&str, &std::ffi::OsStr)]) -> Vec<std::ffi::OsString> {
    env.iter()
        .map(|(name, value)| {
            let mut arg = std::ffi::OsString::from(format!("--env={name}="));
            arg.push(value);
            arg
        })
        .collect()
}

/// Opens a file, folder or URL with the system's default handler.
pub fn open(target: &std::ffi::OsStr) -> Result<(), String> {
    spawn_detached(opener(target)?)
}

/// The system's default opener for `target`: `explorer` on Windows, `open` on macOS, and
/// `xdg-open` elsewhere (on the host, through `flatpak-spawn --host`, inside the sandbox).
fn opener(target: &std::ffi::OsStr) -> Result<Command, String> {
    let mut cmd = if cfg!(target_os = "macos") {
        command("open")
    } else if cfg!(windows) {
        command(windows_system_program("explorer.exe")?)
    } else if in_sandbox() {
        let mut c = command("flatpak-spawn");
        c.arg("--host").arg("xdg-open");
        c
    } else {
        command("xdg-open")
    };
    cmd.arg(target);
    Ok(cmd)
}

/// Opens `primary_url` with the system's default handler. If opening fails (for example,
/// if a custom scheme handler is not found or fails to execute), falls back to `fallback_url`.
pub fn open_url_with_fallback(primary_url: &str, fallback_url: &str) {
    let primary = primary_url.to_string();
    let fallback = fallback_url.to_string();
    std::thread::spawn(move || {
        if !open_and_wait_success(&primary, Duration::from_millis(1500)) {
            log::warn!("couldn't open {primary}; falling back to {fallback}");
            let _ = open(std::ffi::OsStr::new(&fallback));
        }
    });
}

/// Starts the default system opener for `target` and returns `true` unless it fails to start
/// or exits in error within `timeout`. An opener still running after `timeout` counts as
/// success and is reaped in the background.
///
/// `explorer` exits with an error status even when it opens the target, so on Windows only a
/// failure to start counts; the registry check in `detect_discord_scheme_handler` covers the rest.
fn open_and_wait_success(target: &str, timeout: Duration) -> bool {
    let mut cmd = match opener(std::ffi::OsStr::new(target)) {
        Ok(cmd) => cmd,
        Err(error) => {
            log::warn!("couldn't resolve the system opener for {target}: {error}");
            return false;
        }
    };
    if cfg!(windows) {
        return spawn_detached(cmd).is_ok();
    }
    let mut child = match cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("couldn't start the opener for {target}: {e}");
            return false;
        }
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return true;
            }
            Err(_) => return false,
        }
    }
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
    let mut cmd = command(windows_system_program("explorer.exe")?);
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
        let Some(name) = path.file_name().and_then(OsStr::to_str) else { return false };
        let filter = format!("IMAGENAME eq {name}");
        let Ok(tasklist) = windows_system_program(r"System32\tasklist.exe") else { return false };
        let mut cmd = command(tasklist);
        cmd.args(["/FI", &filter, "/NH", "/FO", "CSV"]);
        run_command(cmd, "tasklist", Duration::from_secs(5)).ok().is_some_and(|out| out.to_ascii_lowercase().contains(&name.to_ascii_lowercase()))
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

fn windows_system_program(relative: &str) -> Result<PathBuf, String> {
    let root = std::env::var_os("SystemRoot")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "Windows didn't provide its system folder".to_string())?;
    if !root.is_absolute() {
        return Err("Windows provided a non-absolute system folder".into());
    }
    Ok(root.join(relative))
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
mod env_tests {
    use super::*;

    #[test]
    fn each_variable_becomes_one_env_argument_with_its_value_intact() {
        let path = std::ffi::OsStr::new("/home/ana/.local/share/artcraft-launcher/versions with space.json");
        let args = env_args(&[("CRAFT_VERSIONS_FILE", path)]);
        assert_eq!(args, [std::ffi::OsString::from("--env=CRAFT_VERSIONS_FILE=/home/ana/.local/share/artcraft-launcher/versions with space.json")]);
        assert!(env_args(&[]).is_empty());
    }
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

    #[cfg(windows)]
    #[test]
    fn waiting_for_a_process_ends_when_it_exits() {
        let mut child = Command::new("ping").args(["-n", "3", "127.0.0.1"]).stdout(Stdio::null()).spawn().unwrap();
        assert!(!wait_for_exit(child.id(), Duration::from_millis(200)), "it's still running");
        assert!(wait_for_exit(child.id(), Duration::from_secs(30)), "it has exited");
        child.wait().unwrap();
    }

    #[test]
    fn a_failing_helper_reports_its_own_error() {
        let (program, args) = shell(if cfg!(windows) { "echo no such image 1>&2 & exit /b 3" } else { "echo no such image >&2; exit 3" });
        assert_eq!(run(program, &args, Duration::from_secs(30)), Err("no such image".into()));
        assert_eq!(capture(program, &args, Duration::from_secs(30)), None);
    }

    #[cfg(unix)]
    #[test]
    fn successful_helpers_can_write_past_the_stdout_capture_limit() {
        let out = run("/bin/sh", &["-c", "head -c 9437184 /dev/zero"], Duration::from_secs(10)).unwrap();
        assert_eq!(out.len(), 8 << 20);
    }

    #[cfg(unix)]
    #[test]
    fn successful_helpers_can_write_past_the_stderr_capture_limit() {
        let out = run("/bin/sh", &["-c", "head -c 9437184 /dev/zero >&2"], Duration::from_secs(10));
        assert!(out.is_ok(), "the helper should finish without a broken pipe");
        assert!(out.unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_that_keeps_writing_still_times_out() {
        let start = Instant::now();
        let err = run("yes", &[], Duration::from_millis(100)).unwrap_err();
        assert!(err.contains("didn't finish within"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(5));
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

    #[cfg(windows)]
    #[test]
    fn system_helpers_use_absolute_system_paths() {
        let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        assert_eq!(windows_system_program("explorer.exe").unwrap(), root.join("explorer.exe"));
        assert_eq!(windows_system_program(r"System32\tasklist.exe").unwrap(), root.join(r"System32\tasklist.exe"));
    }

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

#[cfg(test)]
mod discord_tests {
    use super::*;

    #[test]
    fn linux_candidates_include_flatpak_and_system_paths() {
        let fake_home = Path::new("/home/testuser");
        let candidates = linux_discord_candidates(Some(fake_home));
        assert!(candidates.iter().any(|p| p.ends_with("com.discordapp.Discord.desktop")));
        assert!(candidates.iter().any(|p| p == Path::new("/home/testuser/.local/share/applications/discord.desktop")));
        assert!(candidates.iter().any(|p| p == Path::new("/usr/share/applications/discord.desktop")));
        assert!(candidates.iter().any(|p| p == Path::new("/snap/bin/discord")));
    }

    #[test]
    fn macos_candidates_include_standard_apps() {
        let fake_home = Path::new("/Users/testuser");
        let candidates = macos_discord_candidates(Some(fake_home));
        assert!(candidates.iter().any(|p| p == Path::new("/Applications/Discord.app")));
        assert!(candidates.iter().any(|p| p == Path::new("/Users/testuser/Applications/Discord.app")));
        assert!(candidates.iter().any(|p| p == Path::new("/Applications/Discord Canary.app")));
    }

    #[test]
    fn discord_url_gives_app_or_web_url() {
        use artcraft_launcher_core::catalog::{DISCORD_APP_URL, DISCORD_URL};
        let url = discord_url();
        assert!(url == DISCORD_URL || url == DISCORD_APP_URL);
    }
}

#[cfg(test)]
mod inherited_pipe_tests {
    use super::*;
    use std::io::Write;

    fn fixture_args(name: &str, marker: &str) -> Vec<String> {
        let (_, module) = module_path!().split_once("::").unwrap();
        // libtest accepts arbitrary skip filters; this one carries our private fixture path.
        vec!["--ignored".into(), "--exact".into(), format!("{module}::{name}"), "--nocapture".into(), "--skip".into(), marker.into()]
    }

    fn fixture_dir() -> std::path::PathBuf {
        let args: Vec<String> = std::env::args().collect();
        std::path::PathBuf::from(&args[args.iter().position(|arg| arg == "--skip").unwrap() + 1])
    }

    #[test]
    #[ignore = "self-spawned subprocess fixture"]
    fn pipe_holder() {
        let dir = fixture_dir();
        let buffer = [b'x'; 64 << 10];
        // Both streams exceed the capture cap before the direct child exits.
        for _ in 0..144 {
            std::io::stdout().write_all(&buffer).unwrap();
            std::io::stderr().write_all(&buffer).unwrap();
        }
        std::fs::write(dir.join("ready"), []).unwrap();
        // Hold the pipes far longer than any deadline the test tries, so a capture that waited
        // for them would show; the test's guard stops this early.
        let start = Instant::now();
        while !dir.join("stop").exists() && start.elapsed() < HOLD {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::write(dir.join("done"), []).unwrap();
    }

    #[test]
    #[ignore = "self-spawned subprocess fixture"]
    fn pipe_parent() {
        let dir = fixture_dir();
        let descendant = command(std::env::current_exe().unwrap()).args(fixture_args("pipe_holder", dir.to_str().unwrap())).spawn().unwrap();
        std::fs::write(dir.join("spawned"), []).unwrap();
        // No deadline of its own: run() kills this process at its deadline if it is too slow.
        let start = Instant::now();
        while !dir.join("ready").exists() && start.elapsed() < HOLD {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(dir.join("ready").exists(), "descendant did not fill both captured pipes");
        std::fs::write(dir.join("parent-returning"), []).unwrap();
        // Dropping Child leaves the finite fixture running with inherited stdout/stderr.
        drop(descendant);
    }

    /// How long the descendant holds the pipes when nothing stops it.
    const HOLD: Duration = Duration::from_secs(120);

    struct StopHolder(tempfile::TempDir);

    impl Drop for StopHolder {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.path().join("stop"), []);
            if !self.0.path().join("spawned").exists() {
                return;
            }
            let start = Instant::now();
            while !self.0.path().join("done").exists() && start.elapsed() < Duration::from_secs(30) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn exited_helper_does_not_wait_past_deadline_for_inherited_pipes() {
        let exe = std::env::current_exe().unwrap();
        // The direct child has to start a second test process and fill both pipes before the
        // deadline, which can take seconds on a slow or emulated machine. An attempt where it
        // didn't get that far says nothing about inherited pipes: retry with a longer deadline.
        let mut deadline = Duration::from_secs(1);
        loop {
            let holder = StopHolder(tempfile::tempdir().unwrap());
            let args = fixture_args("pipe_parent", holder.0.path().to_str().unwrap());
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let start = Instant::now();
            let result = run(exe.to_str().unwrap(), &args, deadline);
            let elapsed = start.elapsed();
            if !holder.0.path().join("parent-returning").exists() && deadline < Duration::from_secs(16) {
                deadline *= 4;
                continue;
            }
            assert!(holder.0.path().join("parent-returning").exists(), "direct child did not exit its fixture within {deadline:?}");
            assert!(
                matches!(&result, Err(error) if error.contains("didn't finish within")),
                "expected a pipe deadline error; helper returned success: {}, elapsed: {elapsed:?}",
                result.is_ok()
            );
            // The descendant would hold the pipes for HOLD, far past this bound.
            assert!(deadline * 2 < HOLD);
            assert!(elapsed < deadline * 2, "capture exceeded its caller deadline {deadline:?}: {elapsed:?}");
            // The guard stops the finite descendant and waits for its completion marker on failures too.
            break;
        }
    }
}
