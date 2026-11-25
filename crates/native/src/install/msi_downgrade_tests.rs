use std::cell::RefCell;

use super::*;

/// Runs a step-by-step downgrade whose steps answer `remove` (an error, and whether something was
/// removed before it), `chosen` and `back`, and returns its result with the steps that ran, in order.
fn downgrade(remove: Result<(), (&str, bool)>, chosen: Result<(), &str>, back: Result<(), &str>) -> (Result<(), Failed>, Vec<&'static str>) {
    let ran = RefCell::new(Vec::new());
    let result = run(Steps {
        app: "FilmCraft",
        chosen: "0.13.0",
        previous: "0.14.0",
        remove: &mut || {
            ran.borrow_mut().push("remove");
            remove.map_err(|(error, partly)| Removal { error: error.to_string(), partly })
        },
        install_chosen: &mut || {
            ran.borrow_mut().push("install chosen");
            chosen.map_err(str::to_string)
        },
        reinstall_previous: &mut || {
            ran.borrow_mut().push("put back");
            back.map_err(str::to_string)
        },
    });
    (result, ran.into_inner())
}

#[test]
fn the_newer_copy_goes_before_the_chosen_version_and_nothing_is_put_back_on_success() {
    let (result, ran) = downgrade(Ok(()), Ok(()), Ok(()));
    assert_eq!(result, Ok(()));
    assert_eq!(ran, ["remove", "install chosen"]);
}

#[test]
fn a_failed_removal_changes_nothing_and_installs_nothing() {
    let (result, ran) = downgrade(Err(("the Windows Installer stopped with error 1603", false)), Ok(()), Ok(()));
    assert_eq!(result, Err(Failed::without_put_back("the Windows Installer stopped with error 1603".to_string())));
    assert_eq!(ran, ["remove"]);
}

/// An app registered twice (say per-user and all-users): the first copy is gone when removing the
/// second fails, so the previous version goes back in, and nothing claims nothing changed.
#[test]
fn a_removal_that_fails_after_removing_one_copy_puts_the_previous_version_back() {
    let (result, ran) = downgrade(Err(("the Windows Installer stopped with error 1603", true)), Ok(()), Ok(()));
    assert_eq!(ran, ["remove", "put back"]);
    let failed = result.unwrap_err();
    let message = &failed.error;
    assert!(message.contains("FilmCraft 0.14.0 couldn't be fully removed") && message.contains("still installed"), "{message}");
    assert!(!message.contains("nothing was changed"), "{message}");
    assert!(failed.put_back, "the registration the put-back made is the one to record");
    let (result, _) = downgrade(Err(("error 1603", true)), Ok(()), Err("error 1603"));
    let failed = result.unwrap_err();
    let message = &failed.error;
    assert!(message.contains("putting it back failed too") && message.contains("Install FilmCraft again from the launcher"), "{message}");
    assert!(!failed.put_back, "nothing was put back, so there is no registration to record");
}

#[test]
fn a_failed_install_puts_the_previous_version_back_and_says_so() {
    let (result, ran) = downgrade(Ok(()), Err("the Windows Installer stopped with error 1603"), Ok(()));
    assert_eq!(ran, ["remove", "install chosen", "put back"]);
    let failed = result.unwrap_err();
    let message = &failed.error;
    assert!(message.contains("FilmCraft 0.13.0 couldn't be installed"), "{message}");
    assert!(message.contains("FilmCraft 0.14.0 was put back"), "{message}");
    assert!(failed.put_back, "the put-back's registration is reported for recording");
}

#[test]
fn a_refused_prompt_also_puts_the_previous_version_back() {
    let (result, ran) = downgrade(Ok(()), Err(CANCELLED), Ok(()));
    assert_eq!(ran, ["remove", "install chosen", "put back"]);
    let message = result.unwrap_err().error;
    assert!(message.contains("permission wasn't given"), "{message}");
    assert!(!message.contains(CANCELLED), "{message}");
}

#[test]
fn when_putting_back_fails_too_the_person_is_told_how_to_recover() {
    let (result, _) = downgrade(Ok(()), Err("error 1603"), Err(CANCELLED));
    let failed = result.unwrap_err();
    let message = &failed.error;
    assert!(message.contains("putting 0.14.0 back failed too (permission wasn't given)"), "{message}");
    assert!(message.contains("Install FilmCraft again from the launcher"), "{message}");
    assert!(!failed.put_back);
}

/// The exit code the elevated script builds for `step`, as PowerShell computes it (a code outside
/// the Windows Installer range reads as 4095).
fn code(step: i32, first: i32, second: i32) -> i32 {
    let fit = |c: i32| if (0..=4094).contains(&c) { c } else { 4095 };
    OURS | (step << 24) | (fit(first) << 12) | fit(second)
}

#[test]
fn the_session_exit_code_says_which_step_stopped_it() {
    assert_eq!(ended(Some(0)), Ended::Done);
    assert_eq!(ended(Some(DECLINED)), Ended::Declined);
    assert_eq!(ended(Some(LAUNCH_FAILED)), Ended::LaunchFailed);
    assert_eq!(ended(Some(code(REMOVAL_FAILED, 1603, 0))), Ended::RemovalFailed(1603));
    assert_eq!(ended(Some(code(PUT_BACK, 1603, 0))), Ended::PutBack(1603));
    assert_eq!(ended(Some(code(NOT_PUT_BACK, 1603, 1638))), Ended::NotPutBack { install: 1603, back: 1638 });
    assert_eq!(ended(Some(code(PARTLY_REMOVED_PUT_BACK, 1603, 0))), Ended::PartlyRemovedPutBack(1603));
    assert_eq!(ended(Some(code(PARTLY_REMOVED_NOT_PUT_BACK, 1603, 1638))), Ended::PartlyRemovedNotPutBack { removal: 1603, back: 1638 });
    // A plain msiexec or PowerShell code is never read as one of ours.
    for other in [1, 1603, 1602, 3010, -1] {
        assert_eq!(ended(Some(other)), Ended::Other(Some(other)), "{other}");
    }
    assert_eq!(ended(None), Ended::Other(None));
}

#[test]
fn each_outcome_tells_the_person_what_is_installed_now() {
    let report = |e| report(e, "FilmCraft", "0.13.0", "0.14.0");
    assert_eq!(report(Ended::Done), Ok(()));
    // A refused prompt is an ordinary cancel: nothing ran.
    assert_eq!(report(Ended::Declined), Err(Failed::without_put_back(CANCELLED.to_string())));
    let removal = report(Ended::RemovalFailed(1603)).unwrap_err();
    let message = &removal.error;
    assert!(message.contains("FilmCraft 0.14.0 couldn't be removed") && message.contains("nothing was changed"), "{message}");
    assert!(!removal.put_back);
    let back = report(Ended::PutBack(1603)).unwrap_err();
    assert!(back.error.contains("FilmCraft 0.14.0 was put back"), "{}", back.error);
    assert!(back.put_back, "the previous package is back, so its registration is reported");
    let lost = report(Ended::NotPutBack { install: 1603, back: 1603 }).unwrap_err();
    assert!(lost.error.contains("Install FilmCraft again from the launcher"), "{}", lost.error);
    assert!(!lost.put_back);
    let partly = report(Ended::PartlyRemovedPutBack(1603)).unwrap_err();
    assert!(partly.error.contains("couldn't be fully removed") && partly.error.contains("FilmCraft 0.14.0 is still installed"), "{}", partly.error);
    assert!(partly.put_back);
    let partly_lost = report(Ended::PartlyRemovedNotPutBack { removal: 1603, back: 1603 }).unwrap_err();
    assert!(partly_lost.error.contains("Install FilmCraft again from the launcher"), "{}", partly_lost.error);
    assert!(!partly_lost.put_back);
    let odd = report(Ended::Other(Some(5))).unwrap_err();
    assert!(odd.error.contains("Check which version of FilmCraft is installed"), "{}", odd.error);
    assert!(!odd.put_back);
}

/// The registration a put-back made is the one the installer's own log names for the package it
/// ran: its product code, never a registry row that matches the previous version's numbers.
#[test]
fn the_installed_packages_log_names_the_registration_it_created() {
    const CODE: &str = "{3D425FF2-3EB2-4950-9833-95B01B102845}";
    let log = format!(
        "=== Verbose logging started: 11/25/2026  10:00:00  Build type: SHIP UNICODE 5.00.10011.00\r\n\
         MSI (c) (A4:1C) [10:00:00:001]: Resetting cached policy values\r\n\
         MSI (c) (A4:1C) [10:00:00:002]: PROPERTY CHANGE: Adding ProductCode property. Its value is '{CODE}'.\r\n\
         MSI (c) (A4:1C) [10:00:00:003]: PROPERTY CHANGE: Adding UpgradeCode property. Its value is '{{11111111-2222-3333-4444-555555555555}}'.\r\n"
    );
    assert_eq!(product_code_in(&log).as_deref(), Some(CODE), "the product code, not the upgrade code");
    // The property dump form Windows Installer also logs.
    assert_eq!(product_code_in(&format!("ProductCode = {CODE}\r\n")).as_deref(), Some(CODE));
    // A log that names no valid product code records nothing.
    assert_eq!(product_code_in("no properties here\r\n"), None);
    assert_eq!(product_code_in("ProductCode = {not-a-code}\r\n"), None);
    // Read from beside the package itself.
    let dir = tempfile::tempdir().unwrap();
    let msi = std::fs::canonicalize(dir.path()).unwrap().join("filmcraft-0.14.0.msi");
    std::fs::write(&msi, b"package").unwrap();
    assert_eq!(registered_product_code(&msi), None, "no log yet");
    std::fs::write(super::super::msi_log(&msi), &log).unwrap();
    assert_eq!(registered_product_code(&msi).as_deref(), Some(CODE));
}

/// An all-users downgrade with no elevated session stops before removing anything, and says so.
#[test]
fn without_the_one_prompt_session_nothing_is_changed() {
    let message = no_session("FilmCraft", "0.13.0");
    assert!(message.contains("go back to FilmCraft 0.13.0") && message.contains("nothing was changed"), "{message}");
}

#[test]
fn only_a_real_product_code_reaches_the_elevated_session() {
    assert_eq!(removal("{3D425FF2-3EB2-4950-9833-95B01B102845}"), Ok("/x {3D425FF2-3EB2-4950-9833-95B01B102845} /passive /norestart".into()));
    for bad in [
        "",
        "3D425FF2-3EB2-4950-9833-95B01B102845",
        "{3D425FF2-3EB2-4950-9833-95B01B10284}",
        "{3D425FF2-3EB2-4950-9833-95B01B10284'}",
        "{x' ; calc ; '}",
    ] {
        assert!(removal(bad).is_err(), "{bad}");
    }
}

/// A per-user uninstall record, which the user's own programs can write, never names what the
/// elevated session removes.
#[test]
fn only_machine_wide_records_name_what_the_elevated_session_removes() {
    use crate::install::MsiScope;
    const OURS: &str = "{3D425FF2-3EB2-4950-9833-95B01B102845}";
    const PLANTED: &str = "{11111111-2222-3333-4444-555555555555}";
    assert_eq!(
        elevated_removals("FilmCraft", [(PLANTED, MsiScope::PerUser), (OURS, MsiScope::PerMachine)]),
        Ok(vec![format!("/x {OURS} /passive /norestart")])
    );
    let none = elevated_removals("FilmCraft", [(PLANTED, MsiScope::PerUser)]).unwrap_err();
    assert!(none.contains("FilmCraft isn't registered"), "{none}");
    assert!(elevated_removals("FilmCraft", [("{x' ; calc ; '}", MsiScope::PerMachine)]).is_err());
}

/// Runs the generated elevated script for real, but not elevated and with `cmd.exe` standing in
/// for `msiexec`: each step writes a marker file named after it and exits with a chosen code.
#[cfg(windows)]
mod powershell {
    use std::path::{Path, PathBuf};

    use super::*;

    fn cmd() -> PathBuf {
        Path::new(&std::env::var_os("SystemRoot").unwrap()).join(r"System32\cmd.exe")
    }

    fn step(dir: &Path, name: &str, exit: i32) -> String {
        format!("/c type nul > \"{}\" & exit {exit}", dir.join(name).display())
    }

    /// One session at a time. Run in parallel, the stand-in `cmd.exe` sometimes never started (its
    /// marker missing, its code out of range), which the script rightly took as a failed step; the
    /// launcher never runs two Windows Installer jobs at once.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Runs the script with removal, install and put-back steps exiting with these codes, and
    /// returns how it ended and which steps ran.
    fn session(remove: i32, chosen: i32, back: i32) -> (Ended, Vec<&'static str>) {
        sessions(&[remove], chosen, back)
    }

    /// [`session`] for an app registered once per code in `removals`.
    fn sessions(removals: &[i32], chosen: i32, back: i32) -> (Ended, Vec<&'static str>) {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        let names = ["remove", "remove2"];
        let removals: Vec<String> = removals.iter().zip(names).map(|(code, name)| step(dir.path(), name, *code)).collect();
        let script = script(cmd().to_str().unwrap(), &removals, &step(dir.path(), "chosen", chosen), &step(dir.path(), "back", back));
        let root = std::env::var_os("SystemRoot").unwrap();
        let powershell = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let status = std::process::Command::new(powershell)
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &msi_batch::encoded(&script)])
            .status()
            .unwrap();
        let ran = ["remove", "remove2", "chosen", "back"].into_iter().filter(|name| dir.path().join(name).exists()).collect();
        (ended(status.code()), ran)
    }

    #[test]
    fn a_downgrade_removes_then_installs_and_puts_nothing_back() {
        assert_eq!(session(0, 0, 0), (Ended::Done, vec!["remove", "chosen"]));
        // A restart being needed is still success.
        assert_eq!(session(0, 3010, 0), (Ended::Done, vec!["remove", "chosen"]));
    }

    #[test]
    fn a_failed_removal_stops_before_anything_is_installed() {
        assert_eq!(session(1603, 0, 0), (Ended::RemovalFailed(1603), vec!["remove"]));
    }

    #[test]
    fn a_failed_install_puts_the_previous_version_back_without_asking_again() {
        assert_eq!(session(0, 1603, 0), (Ended::PutBack(1603), vec!["remove", "chosen", "back"]));
    }

    #[test]
    fn a_second_removal_that_fails_puts_the_previous_version_back_without_installing() {
        assert_eq!(sessions(&[0, 1603], 0, 0), (Ended::PartlyRemovedPutBack(1603), vec!["remove", "remove2", "back"]));
        assert_eq!(sessions(&[0, 1603], 0, 1638), (Ended::PartlyRemovedNotPutBack { removal: 1603, back: 1638 }, vec!["remove", "remove2", "back"]));
        // The first removal failing still changes nothing.
        assert_eq!(sessions(&[1603, 0], 0, 0), (Ended::RemovalFailed(1603), vec!["remove"]));
    }

    #[test]
    fn when_the_put_back_fails_too_both_codes_come_back() {
        assert_eq!(session(0, 1603, 1638), (Ended::NotPutBack { install: 1603, back: 1638 }, vec!["remove", "chosen", "back"]));
    }
}
