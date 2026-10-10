//! The elevated helper that installs a batch of Windows Installer packages.
//!
//! For each [`msi_batch`](crate::msi_batch) call, [`install`] starts this same program again
//! with administrator rights, `artcraft-launcher --install-msis <results> (<msi> <sha256>)…`,
//! through PowerShell's `Start-Process -Verb RunAs`, so Windows shows one UAC prompt, naming
//! ArtCraft Launcher, for the whole batch. The helper ([`helper_main`]) installs the files one
//! after another with `msiexec /qn /norestart`, so no Windows Installer window opens, and writes
//! each result to the results file as it goes; the launcher reads it to show progress.
//!
//! Everything the helper does runs as administrator, so it trusts nothing that can change after
//! the prompt:
//!
//! - The files and their checksums arrive only as command-line arguments, fixed when the process
//!   starts. Nothing is read back from a file the user's account can edit.
//! - Each file is opened before it's checked, shared for reading only, so nobody can write,
//!   rename or delete it until its install has ended, and its SHA-256 is checked through that
//!   handle. A file that changed after the launcher checked it isn't installed.
//! - The results file must be new (it's never opened if something is already there) and is
//!   written in place, so the helper can't be pointed at an existing file to overwrite.
//! - `msiexec` is found through the machine's registry, not the user's environment.
//! - Only absolute `.msi` paths and lowercase hex checksums are accepted, and the helper exits
//!   when the batch is done.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::msi_batch::Item;

/// The first argument that makes the program the helper instead of the launcher.
pub const FLAG: &str = "--install-msis";
/// The most files one helper takes (the suite has a dozen apps).
const MAX_ITEMS: usize = 64;
/// The exit code the PowerShell wrapper reports when the UAC prompt was declined
/// (`ERROR_CANCELLED`).
#[cfg_attr(not(windows), allow(dead_code))]
const DECLINED: i32 = 1223;

/// Results the helper writes instead of an `msiexec` exit code.
const CHANGED: &str = "changed";
const UNREADABLE: &str = "unreadable";
const INTERRUPTED: &str = "interrupted";

/// What a Windows Installer exit code means for the user (`None`: it never reported one).
pub fn msi_outcome(code: Option<i32>) -> Result<(), String> {
    match code {
        // 3010 and 1641: installed, and Windows wants a restart (which `/norestart` leaves to the user).
        Some(0 | 3010 | 1641) => Ok(()),
        Some(1602) => Err("cancelled".into()),
        Some(1618) => Err("another installation is in progress. Wait for it to finish, then try again.".into()),
        Some(1925 | 1303) => Err("Windows didn't allow the install (administrator permission is needed)".into()),
        Some(c) => Err(format!("the Windows Installer stopped with error {c}")),
        None => Err("the Windows Installer was interrupted".into()),
    }
}

/// The helper's arguments (after [`FLAG`]).
pub fn helper_args(results: &Path, items: &[Item]) -> Vec<OsString> {
    let mut args = vec![results.as_os_str().to_os_string()];
    for item in items {
        args.push(item.msi.as_os_str().to_os_string());
        args.push(OsString::from(&item.sha256));
    }
    args
}

/// Reads [`helper_args`] back. `None` for anything else.
pub fn parse_args(args: &[OsString]) -> Option<(PathBuf, Vec<Item>)> {
    let (results, rest) = args.split_first()?;
    let results = PathBuf::from(results);
    let (pairs, odd) = rest.as_chunks::<2>();
    if !results.is_absolute() || pairs.is_empty() || !odd.is_empty() || pairs.len() > MAX_ITEMS {
        return None;
    }
    let items = pairs
        .iter()
        .map(|[msi, sha]| {
            let msi = PathBuf::from(msi);
            let sha = sha.to_str()?;
            let is_msi = msi.extension().is_some_and(|e| e.eq_ignore_ascii_case("msi"));
            let is_sha = sha.len() == 64 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            (msi.is_absolute() && is_msi && is_sha).then(|| Item { msi, sha256: sha.to_string() })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((results, items))
}

/// The results file so far: one line per finished file, `<position> <msiexec exit code |
/// changed | unreadable | interrupted>`. A line still being written (no newline yet), one that
/// doesn't parse, or a file already reported is skipped.
pub fn parse_results(text: &str, count: usize) -> Vec<(usize, Result<(), String>)> {
    let mut seen = vec![false; count];
    text.split_inclusive('\n')
        .filter_map(|line| {
            let (index, result) = line.strip_suffix('\n')?.trim().split_once(' ')?;
            let index: usize = index.parse().ok()?;
            let seen = seen.get_mut(index).filter(|s| !**s)?;
            let outcome = match result {
                CHANGED => Err("the download changed after it was checked, so it wasn't installed. Try again".into()),
                UNREADABLE => Err("the download couldn't be read to install it. Try again".into()),
                INTERRUPTED => msi_outcome(None),
                code => msi_outcome(Some(code.parse().ok()?)),
            };
            *seen = true;
            Some((index, outcome))
        })
        .collect()
}

/// Runs the helper: `args` are the arguments after [`FLAG`]. Returns the process's exit code.
pub fn helper_main(args: &[OsString]) -> u8 {
    let Some((results, items)) = parse_args(args) else { return 2 };
    #[cfg(windows)]
    return run_batch(&results, &items, &windows::run_msiexec);
    #[cfg(not(windows))]
    {
        let _ = (results, items);
        2
    }
}

/// Installs `items` in turn with `msiexec`, writing each result to `results` as it goes. The
/// file appearing tells the launcher the UAC prompt was answered.
fn run_batch(results: &Path, items: &[Item], msiexec: &dyn Fn(&Path) -> Option<i32>) -> u8 {
    use std::io::Write as _;
    // `create_new` never opens anything already there, a link included.
    let Ok(mut out) = std::fs::OpenOptions::new().write(true).create_new(true).open(results) else { return 3 };
    for (index, item) in items.iter().enumerate() {
        let result = install_one(item, msiexec);
        if writeln!(out, "{index} {result}").and_then(|()| out.flush()).is_err() {
            return 3;
        }
    }
    0
}

/// Checks `item` against its checksum and installs it, holding it open (for reading only)
/// throughout. Returns the result line's second half.
fn install_one(item: &Item, msiexec: &dyn Fn(&Path) -> Option<i32>) -> String {
    let Ok(file) = open_for_reading_only(&item.msi) else { return UNREADABLE.into() };
    let mut hasher = Sha256::new();
    if std::io::copy(&mut &file, &mut hasher).is_err() {
        return UNREADABLE.into();
    }
    if crate::http::hex(&hasher.finalize()) != item.sha256 {
        return CHANGED.into();
    }
    let code = msiexec(&item.msi);
    drop(file);
    code.map_or_else(|| INTERRUPTED.to_string(), |c| c.to_string())
}

/// Opens a file for reading, and on Windows lets nobody else write, rename or delete it while
/// it's open (others may still read it, as `msiexec` does).
fn open_for_reading_only(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        options.share_mode(FILE_SHARE_READ);
    }
    options.open(path)
}

/// One argument quoted the way Windows programs (and Rust's `args_os`) split a command line.
fn windows_quote(arg: &str) -> String {
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// The helper's whole command line after the program: [`FLAG`] and [`helper_args`], quoted.
fn argument_line(results: &Path, items: &[Item]) -> Result<String, String> {
    let mut line = String::from(FLAG);
    for arg in helper_args(results, items) {
        let arg = arg.to_str().ok_or("a download's path has characters Windows can't pass on")?;
        line.push(' ');
        line.push_str(&windows_quote(arg));
    }
    Ok(line)
}

#[cfg(windows)]
pub use windows::install;

#[cfg(windows)]
mod windows {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use artcraft_launcher_core::protocol::{CANCELLED, JobPhase};

    use super::*;
    use crate::msi_batch::Report;
    use crate::paths::Paths;
    use crate::process;

    /// Starts the helper with administrator rights, waits for the user's answer, and returns the
    /// helper's exit code (or [`DECLINED`]). The program and its arguments go in through the
    /// environment, so nothing in them needs PowerShell quoting.
    const ELEVATE: &str = "$ErrorActionPreference = 'Stop'; \
        try { $p = Start-Process -FilePath $env:ARTCRAFT_LAUNCHER_HELPER -ArgumentList $env:ARTCRAFT_LAUNCHER_HELPER_ARGS -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode } \
        catch { $e = $_.Exception; while ($e) { if ($e.NativeErrorCode -eq 1223) { exit 1223 }; $e = $e.InnerException }; exit 1 }";

    static RUNS: AtomicU64 = AtomicU64::new(0);

    /// Installs `items` with one UAC prompt (see the module docs), telling `report` about each.
    pub fn install(items: &[Item], paths: &Paths, report: &mut dyn FnMut(usize, Report)) {
        let fail_all = |report: &mut dyn FnMut(usize, Report), error: &str| {
            for i in 0..items.len() {
                report(i, Report::Done(Err(error.to_string())));
            }
        };
        let results = paths.cache.join(format!("msi-batch-{}-{}.txt", std::process::id(), RUNS.fetch_add(1, Ordering::SeqCst)));
        let prepared = std::fs::create_dir_all(&paths.cache)
            .map_err(|e| format!("couldn't create {} ({e})", paths.cache.display()))
            .and_then(|()| std::env::current_exe().map_err(|e| format!("couldn't find ArtCraft Launcher's program ({e})")))
            .and_then(|exe| Ok((exe, argument_line(&results, items)?)));
        let (exe, args) = match prepared {
            Ok(p) => p,
            Err(e) => return fail_all(report, &e),
        };
        let _ = std::fs::remove_file(&results);
        for i in 0..items.len() {
            report(i, Report::Phase(JobPhase::Permission));
        }
        let mut command = process::command("powershell.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", ELEVATE])
            .env("ARTCRAFT_LAUNCHER_HELPER", &exe)
            .env("ARTCRAFT_LAUNCHER_HELPER_ARGS", &args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => return fail_all(report, &format!("couldn't ask Windows for permission ({e})")),
        };
        let mut done = vec![false; items.len()];
        let mut started = false;
        let code = loop {
            let exited = match child.try_wait() {
                Ok(status) => status.map(|s| s.code()),
                Err(_) => Some(None),
            };
            // Read after checking for the exit, so the last results are always seen.
            if let Ok(text) = std::fs::read_to_string(&results) {
                if !started {
                    started = true;
                    for (i, _) in done.iter().enumerate().filter(|(_, d)| !**d) {
                        report(i, Report::Phase(JobPhase::Installing));
                    }
                }
                for (i, outcome) in parse_results(&text, items.len()) {
                    if let Some(d) = done.get_mut(i).filter(|d| !**d) {
                        *d = true;
                        report(i, Report::Done(outcome));
                    }
                }
            }
            if let Some(code) = exited {
                break code;
            }
            std::thread::sleep(Duration::from_millis(150));
        };
        let _ = std::fs::remove_file(&results);
        let missing = match code {
            Some(DECLINED) => CANCELLED.to_string(),
            _ if !started => "Windows didn't start the installer".to_string(),
            _ => "the Windows Installer stopped before installing it".to_string(),
        };
        for (i, _) in done.iter().enumerate().filter(|(_, d)| !**d) {
            report(i, Report::Done(Err(missing.clone())));
        }
    }

    /// `msiexec`, silently and without restarting Windows.
    pub fn run_msiexec(msi: &Path) -> Option<i32> {
        process::command(system_msiexec()).arg("/i").arg(msi).args(["/qn", "/norestart"]).status().ok()?.code()
    }

    /// `System32\msiexec.exe` under the Windows folder the machine's registry names: the user's
    /// environment can't redirect what the helper runs as administrator.
    fn system_msiexec() -> PathBuf {
        use winreg::RegKey;
        use winreg::enums::HKEY_LOCAL_MACHINE;
        let root: Option<String> = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
            .ok()
            .and_then(|key| key.get_value("SystemRoot").ok());
        let root = root.as_deref().map(str::trim).filter(|r| !r.is_empty()).unwrap_or(r"C:\Windows").to_string();
        PathBuf::from(root).join(r"System32\msiexec.exe")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn msiexec_comes_from_the_windows_folder() {
            let msiexec = system_msiexec();
            assert!(msiexec.ends_with(r"System32\msiexec.exe"), "{}", msiexec.display());
            assert!(msiexec.is_file(), "{}", msiexec.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn sha(bytes: &[u8]) -> String {
        crate::http::hex(&Sha256::digest(bytes))
    }

    fn absolute(name: &str) -> PathBuf {
        std::env::temp_dir().join("artcraft launcher's test").join(name)
    }

    #[test]
    fn the_helper_reads_back_exactly_what_it_was_given() {
        let results = absolute("results.txt");
        let items = vec![
            Item { msi: absolute("photocraft-0.6.0-windows-x64.msi"), sha256: "a".repeat(64) },
            Item { msi: absolute("film craft.MSI"), sha256: sha(b"x") },
        ];
        assert_eq!(parse_args(&helper_args(&results, &items)), Some((results, items)));
    }

    #[test]
    fn the_helper_refuses_anything_unexpected() {
        let ok = absolute("a.msi").into_os_string();
        let results = absolute("results.txt").into_os_string();
        let good = || "b".repeat(64).into();
        let bad: [Vec<OsString>; 7] = [
            vec![results.clone()],
            vec![results.clone(), ok.clone()],
            vec!["results.txt".into(), ok.clone(), good()],
            vec![results.clone(), "a.msi".into(), good()],
            vec![results.clone(), absolute("a.exe").into_os_string(), good()],
            vec![results.clone(), ok.clone(), "B".repeat(64).into()],
            vec![results.clone(), ok.clone(), "b".repeat(63).into()],
        ];
        for args in bad {
            assert_eq!(parse_args(&args), None, "{args:?}");
        }
        let too_many: Vec<OsString> = std::iter::once(results).chain((0..=MAX_ITEMS).flat_map(|_| [ok.clone(), good()])).collect();
        assert_eq!(parse_args(&too_many), None);
    }

    #[test]
    fn results_are_read_line_by_line_as_they_arrive() {
        let text = "0 0\n2 3010\n1 1603\n0 1603\n7 0\nx y\n3 changed\n4 unreadable\n5 interrupted\n6 16";
        let results = parse_results(text, 7);
        assert_eq!(
            results,
            [
                (0, Ok(())),
                (2, Ok(())),
                (1, Err("the Windows Installer stopped with error 1603".into())),
                (3, Err("the download changed after it was checked, so it wasn't installed. Try again".into())),
                (4, Err("the download couldn't be read to install it. Try again".into())),
                (5, Err("the Windows Installer was interrupted".into())),
            ],
            "a second result for a file, one out of range, nonsense and a half-written line are skipped"
        );
    }

    #[test]
    fn exit_codes_say_what_happened() {
        assert_eq!(msi_outcome(Some(0)), Ok(()));
        assert_eq!(msi_outcome(Some(3010)), Ok(()));
        assert_eq!(msi_outcome(Some(1602)), Err("cancelled".into()));
        assert!(msi_outcome(Some(1618)).unwrap_err().contains("another installation"));
        assert!(msi_outcome(Some(1925)).unwrap_err().contains("administrator"));
        assert_eq!(msi_outcome(None), Err("the Windows Installer was interrupted".into()));
    }

    #[test]
    fn arguments_survive_windows_quoting() {
        assert_eq!(windows_quote(r"C:\Users\Ann Lee\a.msi"), r#""C:\Users\Ann Lee\a.msi""#);
        assert_eq!(windows_quote(r"C:\dir\"), r#""C:\dir\\""#);
        assert_eq!(windows_quote(r#"say "hi"\"#), r#""say \"hi\"\\""#);
        assert_eq!(windows_quote(r#"a\\"b"#), r#""a\\\\\"b""#);
        assert_eq!(windows_quote(""), r#""""#);
        let results = absolute("results.txt");
        let items = [Item { msi: absolute("a.msi"), sha256: "c".repeat(64) }];
        let line = argument_line(&results, &items).unwrap();
        assert!(line.starts_with("--install-msis \""), "{line}");
        assert!(line.ends_with(&format!(" \"{}\"", "c".repeat(64))), "{line}");
    }

    #[test]
    fn the_helper_installs_each_file_that_still_matches_and_reports_every_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, body: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            path
        };
        let items = [
            Item { msi: file("a.msi", b"a"), sha256: sha(b"a") },
            Item { msi: file("b.msi", b"swapped"), sha256: sha(b"b") },
            Item { msi: dir.path().join("gone.msi"), sha256: sha(b"c") },
            Item { msi: file("d.msi", b"d"), sha256: sha(b"d") },
        ];
        let ran = RefCell::new(Vec::new());
        let msiexec = |msi: &Path| {
            ran.borrow_mut().push(msi.to_path_buf());
            Some(if msi.ends_with("d.msi") { 1603 } else { 0 })
        };
        let results = dir.path().join("results.txt");
        assert_eq!(run_batch(&results, &items, &msiexec), 0);
        assert_eq!(*ran.borrow(), [items[0].msi.clone(), items[3].msi.clone()], "a changed or missing file never reaches msiexec");
        assert_eq!(std::fs::read_to_string(&results).unwrap(), "0 0\n1 changed\n2 unreadable\n3 1603\n");
        // The results file must be new: one already there is never written to.
        assert_eq!(run_batch(&results, &items, &msiexec), 3);
        assert_eq!(std::fs::read_to_string(&results).unwrap(), "0 0\n1 changed\n2 unreadable\n3 1603\n");
    }

    /// While msiexec runs, nobody can change, rename or delete the file it was checked as.
    #[cfg(windows)]
    #[test]
    fn the_file_cannot_change_while_it_installs() {
        let dir = tempfile::tempdir().unwrap();
        let msi = dir.path().join("a.msi");
        std::fs::write(&msi, b"a").unwrap();
        let item = Item { msi: msi.clone(), sha256: sha(b"a") };
        let msiexec = |path: &Path| {
            assert!(std::fs::OpenOptions::new().write(true).open(path).is_err(), "written to mid-install");
            assert!(std::fs::rename(path, path.with_extension("old")).is_err(), "renamed mid-install");
            assert!(std::fs::remove_file(path).is_err(), "deleted mid-install");
            // msiexec itself only needs to read it.
            assert_eq!(std::fs::read(path).unwrap(), b"a");
            Some(0)
        };
        assert_eq!(install_one(&item, &msiexec), "0");
        // Once the install has ended, the file is the user's again.
        std::fs::remove_file(&msi).unwrap();
    }
}
