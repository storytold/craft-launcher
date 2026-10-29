//! Starting programs: craft-update's helpers (the Craft Apps themselves, the file manager, helper
//! tools), plus what only the launcher does: one `PATH` lookup for detection, an app started with
//! extra environment (the version choice it's told about), and a Flatpak helper that can be
//! cancelled.

pub use craft_update::process::*;

use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

/// Captures at most 8 MB of a child's pipe and drains the rest on its own thread (craft-update's
/// own is private).
#[cfg(target_os = "linux")]
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

/// Finds a program on `PATH`, for detecting a binary install.
#[cfg(target_os = "linux")]
pub(crate) fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
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

/// Runs a helper while polling cancellation and draining its bounded output.
#[cfg(target_os = "linux")]
pub(crate) fn cancellable_output(mut cmd: Command, check: impl Fn() -> Result<(), String>) -> Result<std::process::Output, String> {
    check()?;
    let mut child =
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("couldn't run flatpak ({e})"))?;
    let (stdout, stderr) = (read_all(child.stdout.take()), read_all(child.stderr.take()));
    let started = Instant::now();
    let status = loop {
        if started.elapsed() >= Duration::from_secs(60 * 60) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Flatpak did not finish within one hour".into());
        }
        if let Err(e) = check() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("couldn't wait for flatpak ({e})"));
            }
        }
    };
    let deadline = Instant::now();
    while !stdout.is_finished() || !stderr.is_finished() {
        check()?;
        if deadline.elapsed() > Duration::from_secs(5) {
            return Err("Flatpak output did not close after completion".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(std::process::Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

#[cfg(all(test, target_os = "linux"))]
mod update_cancel_tests {
    use super::*;

    #[test]
    fn cancels_a_running_helper_promptly() {
        let mut cmd = command("sleep");
        cmd.arg("30");
        let start = Instant::now();
        let result = cancellable_output(cmd, || if start.elapsed() >= Duration::from_millis(100) { Err("cancelled".into()) } else { Ok(()) });
        assert_eq!(result.unwrap_err(), "cancelled");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn retains_both_output_streams_and_exit_status() {
        let mut cmd = command("sh");
        cmd.args(["-c", "printf output; printf error >&2; exit 7"]);
        let output = cancellable_output(cmd, || Ok(())).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"output");
        assert_eq!(output.stderr, b"error");
    }
}
