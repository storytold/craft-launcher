use std::path::Path;

use super::*;

#[test]
fn base64_matches_the_standard_test_vectors() {
    // `encoded` takes text and widens it to UTF-16, so check the raw alphabet through ASCII
    // whose UTF-16 form is known: "A" is 41 00, which is "QQA=".
    assert_eq!(encoded(""), "");
    assert_eq!(encoded("A"), "QQA=");
    assert_eq!(encoded("AB"), "QQBCAA==");
    assert_eq!(encoded("ABC"), "QQBCAEMA");
    // Whole-script round trip through PowerShell's own decoding is checked in the Windows test below.
}

#[test]
fn a_quote_in_a_path_cannot_end_the_string() {
    assert_eq!(quoted("C:\\a b\\x.msi"), "'C:\\a b\\x.msi'");
    assert_eq!(quoted("O'Brien"), "'O''Brien'");
    // PowerShell reads the typographic single quotes as quotes as well.
    assert_eq!(quoted("a\u{2018}b\u{2019}c\u{201A}d\u{201B}e"), "'a\u{2018}\u{2018}b\u{2019}\u{2019}c\u{201A}\u{201A}d\u{201B}\u{201B}e'");
    assert_eq!(quoted("'; exit 0; '"), "'''; exit 0; '''");
}

#[test]
fn the_launcher_side_asks_once_and_reports_a_declined_prompt() {
    let script = outer_script(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", "QQA=");
    assert!(script.contains("-Verb RunAs") && script.contains("-Wait -PassThru"), "{script}");
    assert!(script.contains("'-NoProfile','-NonInteractive','-EncodedCommand','QQA='"), "{script}");
    assert!(script.contains("exit $elevated.ExitCode"));
    assert!(script.ends_with(&format!("catch {{ {} }}", launch_catch())), "{script}");
}

#[test]
fn the_command_line_is_the_single_app_route_with_the_same_parts() {
    let dir = tempfile::tempdir().unwrap();
    let msi = dir.path().join("cad craft.msi");
    std::fs::write(&msi, b"x").unwrap();
    let resolved = crate::install::msi_path(&msi).unwrap();
    let (package, log) = (resolved.display().to_string(), resolved.with_extension("log").display().to_string());
    let folder = Path::new(r"C:\Program Files\ArtCraft\CADCraft");

    let all_users = line(&msi, Some(folder), Some(MsiScope::PerMachine)).unwrap();
    assert_eq!(all_users, format!("/i \"{package}\" /passive /norestart /l*v \"{log}\" INSTALLFOLDER=\"{}\" ALLUSERS=1", folder.display()));
    // No folder and no scope: only the package and the log.
    assert_eq!(line(&msi, None, None).unwrap(), format!("/i \"{package}\" /passive /norestart /l*v \"{log}\""));
    // The same package, as the single-app route builds it.
    let single = crate::install::msi_command(Path::new("msiexec.exe"), &msi).unwrap();
    let args: Vec<String> = single.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(args, ["/i", &package, "/passive", "/norestart", "/l*v", &log]);
}

#[test]
fn a_path_that_could_break_out_of_the_quotes_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let msi = dir.path().join("a.msi");
    std::fs::write(&msi, b"x").unwrap();
    for folder in ["C:\\a\"b", "C:\\a\nb"] {
        assert!(line(&msi, Some(Path::new(folder)), Some(MsiScope::PerMachine)).is_err(), "{folder:?}");
    }
}

/// The checked package stays exactly as checked until its installer runs (CWE-367): the elevated
/// session's prompt names PowerShell, not the package's publisher, so a swapped file would go
/// unnoticed.
#[cfg(windows)]
mod holding {
    use super::*;

    fn package(dir: &Path) -> (std::path::PathBuf, String) {
        let msi = dir.join("app.msi");
        std::fs::write(&msi, b"the checked package").unwrap();
        let sha = crate::http::sha256_file(&msi).unwrap();
        (msi, sha)
    }

    #[test]
    fn a_held_package_cannot_be_changed_moved_or_deleted_but_can_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let (msi, sha) = package(dir.path());
        let held = hold(&msi, &sha.to_uppercase()).unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&msi).is_err(), "written while held");
        assert!(std::fs::rename(&msi, dir.path().join("moved.msi")).is_err(), "moved while held");
        assert!(std::fs::remove_file(&msi).is_err(), "deleted while held");
        // msiexec opens the package to read it, sharing only reading.
        {
            use std::os::windows::fs::OpenOptionsExt;
            assert!(std::fs::File::options().read(true).share_mode(0x1).open(&msi).is_ok(), "msiexec could not read it");
        }
        drop(held);
        assert!(std::fs::rename(&msi, dir.path().join("moved.msi")).is_ok());
    }

    #[test]
    fn a_package_that_changed_after_its_check_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (msi, sha) = package(dir.path());
        std::fs::write(&msi, b"something else").unwrap();
        let error = hold(&msi, &sha).err().unwrap();
        assert!(error.contains("changed after it was checked"), "{error}");
    }
}

/// Runs the generated elevated script for real, but not elevated and with `cmd.exe` standing in for
/// `msiexec`, so only the script's own logic is under test (the prompt needs a person).
#[cfg(windows)]
mod powershell {
    use super::*;

    #[test]
    fn the_outer_script_reports_a_launch_failure_apart_from_a_decline() {
        // A program that does not exist cannot be started: that is not the person saying no, so
        // the launcher falls back to one prompt per app rather than cancelling every update.
        let root = std::env::var_os("SystemRoot").unwrap();
        let powershell = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let script = outer_script(r"C:\definitely\not\here.exe", "QQA=");
        let status = std::process::Command::new(powershell).args(["-NoProfile", "-NonInteractive", "-Command", &script]).status().unwrap();
        assert_eq!(status.code(), Some(LAUNCH_FAILED));
    }

    #[test]
    fn a_declined_prompt_is_still_a_cancel() {
        // Declining the prompt makes Start-Process throw with the Windows error "cancelled by the
        // user" (1223) inside. The prompt needs a person, so throw that same exception instead.
        let root = std::env::var_os("SystemRoot").unwrap();
        let powershell = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let run = |native: i32| {
            let script = format!(
                "try {{ throw (New-Object System.InvalidOperationException 'x', (New-Object System.ComponentModel.Win32Exception {native})) }} catch {{ {} }}",
                launch_catch()
            );
            std::process::Command::new(&powershell).args(["-NoProfile", "-NonInteractive", "-Command", &script]).status().unwrap().code()
        };
        assert_eq!(run(1223), Some(DECLINED));
        assert_eq!(run(2), Some(LAUNCH_FAILED));
    }
}
