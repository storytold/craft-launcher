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
fn the_elevated_script_runs_each_package_and_keeps_the_first_failure() {
    let lines = vec!["/i \"C:\\a.msi\" /passive".to_string(), "/i \"C:\\O'Brien\\b.msi\"".to_string()];
    let script = inner_script(r"C:\Windows\System32\msiexec.exe", &lines);
    assert!(script.contains("$program = 'C:\\Windows\\System32\\msiexec.exe'"), "{script}");
    assert!(script.contains("@('/i \"C:\\a.msi\" /passive','/i \"C:\\O''Brien\\b.msi\"')"), "{script}");
    assert!(script.contains("Start-Process -FilePath $program -ArgumentList $arguments -Wait -PassThru"));
    assert!(script.contains("-ne 0 -and $code -ne 3010 -and $code -ne 1641"));
    assert!(script.trim_end().ends_with("exit $worst"));
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
fn exit_codes_mean_the_same_as_for_one_app() {
    for ok in [0, 3010, 1641] {
        assert_eq!(outcome(Some(ok), 3), Ok(()), "{ok}");
    }
    // A declined prompt is a cancel, said plainly and never as a shared failure.
    assert_eq!(outcome(Some(1223), 3), Err(CANCELLED.to_string()));
    assert_eq!(outcome(Some(1602), 3), Err(CANCELLED.to_string()));
    assert!(outcome(Some(1618), 1).unwrap_err().starts_with("another installation is in progress"));
    assert_eq!(outcome(Some(1603), 1), Err("the Windows Installer stopped with error 1603".to_string()));
    assert_eq!(outcome(None, 1), Err("the Windows Installer was interrupted".to_string()));
}

#[test]
fn a_failure_of_a_shared_call_says_so_so_the_backend_checks_what_landed() {
    let error = outcome(Some(1603), 3).unwrap_err();
    assert_eq!(error, "the Windows Installer stopped with error 1603 while handling 3 apps in one go. Try them one at a time.");
    assert!(crate::install::failed_in_shared_call(&error));
    assert!(!crate::install::failed_in_shared_call(&outcome(Some(1603), 1).unwrap_err()));
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

#[test]
fn a_job_counts_as_preparing_until_it_arrives_or_leaves() {
    let gather = Gather::new();
    assert_eq!(gather.preparing(), 0);
    let first = Joined::on(&gather);
    let second = Joined::on(&gather);
    assert_eq!(gather.preparing(), 2);
    first.arrived();
    first.arrived(); // twice changes nothing
    assert_eq!(gather.preparing(), 1);
    drop(first); // already arrived: dropping must not count it down again
    assert_eq!(gather.preparing(), 1);
    drop(second); // failed or cancelled before arriving: the others stop waiting for it
    assert_eq!(gather.preparing(), 0);
}

/// The checked package stays exactly as checked until its batch ends (CWE-367): the elevated
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

#[test]
fn a_batch_too_long_for_one_command_line_falls_back_to_one_prompt_each() {
    let short = vec!["/i \"C:\\a.msi\"".to_string()];
    assert!(matches!(plan("C:\\Windows\\System32\\msiexec.exe", &short), Plan::Elevated(_)));
    let long: Vec<String> =
        (0..200).map(|i| format!("/i \"C:\\Users\\someone\\AppData\\Local\\ArtCraft Launcher\\downloads\\app{i}.msi\"")).collect();
    assert!(matches!(plan("C:\\Windows\\System32\\msiexec.exe", &long), Plan::EachAlone));
}

/// The real queue and counter, with the installers replaced by recorders: what "Update all" does with
/// several all-users updates, and with one.
#[cfg(windows)]
mod through_the_queue {
    use std::ffi::OsString;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use artcraft_launcher_core::protocol::JobPhase;

    use super::*;
    use crate::http::Stop;
    use crate::install::windows::run_all_users_with;

    fn package(dir: &Path, name: &str) -> std::path::PathBuf {
        let file = dir.join(name);
        std::fs::write(&file, b"x").unwrap();
        file
    }

    /// Several jobs share one call; a job on its own runs as before. Both in one test because the
    /// queue and the counter are the launcher's real, shared ones.
    #[test]
    fn several_all_users_updates_share_one_call_and_one_runs_alone() {
        let dir = tempfile::tempdir().unwrap();
        let together: Mutex<Vec<Vec<OsString>>> = Mutex::new(Vec::new());
        let alone = AtomicUsize::new(0);
        let stop = Stop::default();

        // Three jobs, as "Update all" starts them: all join first, then finish preparing apart.
        let files: Vec<_> = (0..3).map(|i| package(dir.path(), &format!("app{i}.msi"))).collect();
        let joined: Vec<_> = files.iter().map(|_| Joined::join()).collect();
        assert!(!everyone_here());
        let results: Vec<Result<(), String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = files
                .iter()
                .zip(joined)
                .enumerate()
                .map(|(i, (file, joined))| {
                    let (together, alone, stop) = (&together, &alone, &stop);
                    scope.spawn(move || {
                        std::thread::sleep(Duration::from_millis(300 * i as u64));
                        run_all_users_with(
                            file,
                            None,
                            &joined,
                            stop,
                            &mut |_: JobPhase| {},
                            &|| {
                                alone.fetch_add(1, Ordering::SeqCst);
                                Ok(())
                            },
                            &|targets: &[OsString]| {
                                together.lock().unwrap().push(targets.to_vec());
                                Ok(())
                            },
                        )
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(results, vec![Ok(()); 3]);
        assert!(everyone_here());
        let calls = together.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "one permission prompt for all three: {calls:?}");
        assert_eq!(calls[0].len(), 3);
        assert_eq!(alone.load(Ordering::SeqCst), 0);

        // One job: the route it always had.
        together.lock().unwrap().clear();
        let file = package(dir.path(), "solo.msi");
        let only = Joined::join();
        let result = run_all_users_with(
            &file,
            None,
            &only,
            &stop,
            &mut |_: JobPhase| {},
            &|| {
                alone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            &|targets: &[OsString]| {
                together.lock().unwrap().push(targets.to_vec());
                Ok(())
            },
        );
        drop(only);
        assert_eq!(result, Ok(()));
        assert_eq!((alone.load(Ordering::SeqCst), together.lock().unwrap().len()), (1, 0));
    }
}

/// Runs the generated elevated script for real, but not elevated and with `cmd.exe` standing in for
/// `msiexec`, so only the script's own logic is under test (the prompt needs a person).
#[cfg(windows)]
mod powershell {
    use super::*;

    fn run(program: &Path, lines: &[&str]) -> Option<i32> {
        let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        let script = encoded(&inner_script(program.to_str().unwrap(), &lines));
        let root = std::env::var_os("SystemRoot").unwrap();
        let powershell = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        std::process::Command::new(powershell).args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &script]).status().unwrap().code()
    }

    fn cmd() -> std::path::PathBuf {
        Path::new(&std::env::var_os("SystemRoot").unwrap()).join(r"System32\cmd.exe")
    }

    #[test]
    fn every_package_runs_and_the_first_failure_is_the_exit_code() {
        assert_eq!(run(&cmd(), &["/c exit 0", "/c exit 3010", "/c exit 1641"]), Some(0));
        assert_eq!(run(&cmd(), &["/c exit 0", "/c exit 1603", "/c exit 5"]), Some(1603));
        assert_eq!(run(&cmd(), &["/c exit 5"]), Some(5));
    }

    #[test]
    fn a_failure_does_not_stop_the_packages_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran-last.txt");
        let last = format!("/c echo x> \"{}\"", marker.display());
        assert_eq!(run(&cmd(), &["/c exit 1603", &last]), Some(1603));
        assert!(marker.exists(), "the package after the failing one did not run");
    }

    #[test]
    fn quotes_in_the_program_path_survive_powershell() {
        // The folder name has an apostrophe, a typographic quote and a space.
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("O'Brien \u{2019}s apps");
        std::fs::create_dir(&folder).unwrap();
        let program = folder.join("cmd.exe");
        std::fs::copy(cmd(), &program).unwrap();
        assert_eq!(run(&program, &["/c exit 3010", "/c exit 7"]), Some(7));
    }

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
