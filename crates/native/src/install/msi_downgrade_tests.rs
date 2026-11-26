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
            remove.map_err(|(error, partly)| Removal { error: error.to_string(), partly, code: None })
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
    assert_eq!(failed.put_back, PutBack::AfterPartialRemoval, "part of the app may be left, so only a new code is the restore");
    let (result, _) = downgrade(Err(("error 1603", true)), Ok(()), Err("error 1603"));
    let failed = result.unwrap_err();
    let message = &failed.error;
    assert!(message.contains("putting it back failed too") && message.contains("Install FilmCraft again from the launcher"), "{message}");
    assert_eq!(failed.put_back, PutBack::Not, "nothing was put back, so there is no registration to record");
}

#[test]
fn a_failed_install_puts_the_previous_version_back_and_says_so() {
    let (result, ran) = downgrade(Ok(()), Err("the Windows Installer stopped with error 1603"), Ok(()));
    assert_eq!(ran, ["remove", "install chosen", "put back"]);
    let failed = result.unwrap_err();
    let message = &failed.error;
    assert!(message.contains("FilmCraft 0.13.0 couldn't be installed"), "{message}");
    assert!(message.contains("FilmCraft 0.14.0 was put back"), "{message}");
    assert_eq!(failed.put_back, PutBack::AfterRemoval, "every registration the removal step covers is gone, so a same-code restore is recorded too");
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
    assert_eq!(failed.put_back, PutBack::Not);
}

/// A step-by-step downgrade keeps the exit code of the step that stopped it, so a 1618 or 1603 can
/// name its cause: the chosen package's code and log, or a removal's code, which has no log. The
/// put-back's code does not replace either.
#[test]
fn a_failed_chosen_install_keeps_its_exit_code_and_log() {
    let (chosen, previous) = (Path::new(r"C:\Downloads\filmcraft-0.13.0.msi"), Path::new(r"C:\Downloads\filmcraft-0.14.0.msi"));
    let install = |msi: &Path, exit: &mut Exit| {
        let code = if msi == chosen { 1618 } else { 1603 };
        *exit = Some((code, Some(msi.with_extension("log"))));
        Err(format!("the Windows Installer stopped with error {code}"))
    };
    let mut exit = None;
    let result = run_packages("FilmCraft", (chosen, "0.13.0"), (previous, "0.14.0"), &mut || Ok(()), &mut || Ok(()), &install, &mut exit);
    assert!(result.unwrap_err().error.contains("putting 0.14.0 back failed too"));
    assert_eq!(exit, Some((1618, Some(chosen.with_extension("log")))));
    // Another installation stopped the removal before it started: nothing was removed or
    // installed, and its code is kept.
    let mut exit = None;
    let busy = "another installation is in progress. Wait for it to finish, then try again.";
    let mut removal = || Err(Removal { error: busy.to_string(), partly: false, code: Some(1618) });
    let result = run_packages(
        "FilmCraft",
        (chosen, "0.13.0"),
        (previous, "0.14.0"),
        &mut || Ok(()),
        &mut removal,
        &|_: &Path, _: &mut Exit| panic!("nothing installs"),
        &mut exit,
    );
    assert_eq!(result, Err(Failed::without_put_back(busy.to_string())));
    assert_eq!(exit, Some((1618, None)));
    // A removal that stopped after removing one registration keeps its code over the put-back's.
    let mut exit = None;
    let mut removal = || Err(Removal { error: "error 1603".to_string(), partly: true, code: Some(1603) });
    let _ = run_packages("FilmCraft", (chosen, "0.13.0"), (previous, "0.14.0"), &mut || Ok(()), &mut removal, &install, &mut exit);
    assert_eq!(exit, Some((1603, None)));
    // Windows Installer never started: no code is kept.
    let mut exit = None;
    let mut removal = || Err(Removal { error: "couldn't start the Windows Installer".to_string(), partly: false, code: None });
    let _ = run_packages("FilmCraft", (chosen, "0.13.0"), (previous, "0.14.0"), &mut || Ok(()), &mut removal, &install, &mut exit);
    assert_eq!(exit, None);
}

/// The app started while the downgrade waited for its installer turn: the check made once that
/// turn starts stops it before anything is removed or installed.
#[test]
fn a_program_started_while_waiting_stops_the_downgrade_before_removal() {
    let (chosen, previous) = (Path::new(r"C:\Downloads\filmcraft-0.13.0.msi"), Path::new(r"C:\Downloads\filmcraft-0.14.0.msi"));
    let running = "FilmCraft is running. Quit it, then try again.";
    let mut exit = None;
    let result = run_packages(
        "FilmCraft",
        (chosen, "0.13.0"),
        (previous, "0.14.0"),
        &mut || Err(running.to_string()),
        &mut || panic!("nothing is removed"),
        &|_: &Path, _: &mut Exit| panic!("nothing installs"),
        &mut exit,
    );
    assert_eq!(result, Err(Failed::without_put_back(running.to_string())));
    assert_eq!(exit, None);
}

/// An elevated session that stopped names the code of the step that stopped it: the chosen
/// package's with its log, or a removal's, which has none. Other outcomes name no code.
#[test]
fn an_elevated_session_names_the_chosen_package_code() {
    let log = PathBuf::from(r"C:\Downloads\filmcraft-0.13.0.log");
    assert_eq!(failed_exit(&Ended::PutBack(1618), log.clone()), Some((1618, Some(log.clone()))));
    assert_eq!(failed_exit(&Ended::NotPutBack { install: 1603, back: 1638 }, log.clone()), Some((1603, Some(log.clone()))));
    assert_eq!(failed_exit(&Ended::RemovalFailed(1618), log.clone()), Some((1618, None)));
    assert_eq!(failed_exit(&Ended::PartlyRemovedPutBack(1603), log.clone()), Some((1603, None)));
    assert_eq!(failed_exit(&Ended::PartlyRemovedNotPutBack { removal: 1618, back: 1638 }, log.clone()), Some((1618, None)));
    for other in [Ended::Done, Ended::Declined, Ended::Other(Some(5))] {
        assert_eq!(failed_exit(&other, log.clone()), None, "{other:?}");
    }
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
    let (chosen_log, previous_log) = (Path::new(r"C:\Cache\filmcraft-0.13.0.log"), Path::new(r"C:\Cache\filmcraft-0.14.0.log"));
    let report = |e| report(e, "FilmCraft", "0.13.0", "0.14.0", [chosen_log, previous_log]);
    let names = |message: &str, log: &Path| message.contains(&format!("Its log is at {}", log.display()));
    assert_eq!(report(Ended::Done), Ok(()));
    // A refused prompt is an ordinary cancel: nothing ran.
    assert_eq!(report(Ended::Declined), Err(Failed::without_put_back(CANCELLED.to_string())));
    let removal = report(Ended::RemovalFailed(1603)).unwrap_err();
    let message = &removal.error;
    assert!(message.contains("FilmCraft 0.14.0 couldn't be removed") && message.contains("nothing was changed"), "{message}");
    assert!(!message.contains("Its log is at"), "a removal writes no log: {message}");
    assert_eq!(removal.put_back, PutBack::Not);
    let back = report(Ended::PutBack(1603)).unwrap_err();
    assert!(back.error.contains("FilmCraft 0.14.0 was put back") && names(&back.error, chosen_log), "{}", back.error);
    assert_eq!(back.put_back, PutBack::AfterRemoval, "every registration the removal step covers is gone, so its registration is reported");
    let lost = report(Ended::NotPutBack { install: 1603, back: 1603 }).unwrap_err();
    assert!(
        lost.error.contains("Install FilmCraft again from the launcher") && names(&lost.error, chosen_log) && names(&lost.error, previous_log),
        "{}",
        lost.error
    );
    assert_eq!(lost.put_back, PutBack::Not);
    let partly = report(Ended::PartlyRemovedPutBack(1603)).unwrap_err();
    assert!(partly.error.contains("couldn't be fully removed") && partly.error.contains("FilmCraft 0.14.0 is still installed"), "{}", partly.error);
    assert_eq!(partly.put_back, PutBack::AfterPartialRemoval, "part of the app may be left, so only a new code is the restore");
    let partly_lost = report(Ended::PartlyRemovedNotPutBack { removal: 1603, back: 1603 }).unwrap_err();
    assert!(
        partly_lost.error.contains("Install FilmCraft again from the launcher") && names(&partly_lost.error, previous_log),
        "{}",
        partly_lost.error
    );
    assert_eq!(partly_lost.put_back, PutBack::Not);
    // A cancel in the progress window wrote nothing worth reading.
    let cancelled = report(Ended::PutBack(1602)).unwrap_err();
    assert!(cancelled.error.contains("it was cancelled") && !cancelled.error.contains("Its log is at"), "{}", cancelled.error);
    let odd = report(Ended::Other(Some(5))).unwrap_err();
    assert!(odd.error.contains("Check which version of FilmCraft is installed"), "{}", odd.error);
    assert_eq!(odd.put_back, PutBack::Not);
}

/// Once the removal step removed every registration it covers, the checked previous package goes
/// back in, and Windows Installer gives it the same product code the removed copy had: that same
/// code is the restore, as the one registration carrying the previous release's numbers. Anything
/// but exactly one such registration proves nothing.
#[test]
fn a_same_code_restore_after_a_completed_removal_is_the_put_back() {
    let previous = super::super::version::parse("0.25.0-rc.1").unwrap();
    let install = |code: &str, v: &str| (code.to_string(), super::super::version::parse(v));
    let (rc, second, other) = (
        "{3D425FF2-3EB2-4950-9833-95B01B102845}".to_string(),
        "{6B1C1C1E-0E0B-4B6B-9C59-2C1D2E3F4A5B}".to_string(),
        "{A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D}".to_string(),
    );
    // An unrecorded RC whose registrations the removal step covered, put back from its own
    // package: Windows Installer reinstalled it under the very product code the machine had.
    assert_eq!(put_back_registration(PutBack::AfterRemoval, &[install(&rc, "0.25.0")], Some(&previous)), Some(rc.clone()));
    // A registration the removal step never covers — an account's own per-user record, say — sits
    // beside the restore without becoming a second candidate of its own.
    assert_eq!(put_back_registration(PutBack::AfterRemoval, &[install(&rc, "0.25.0"), install(&other, "0.24.0")], Some(&previous)), Some(rc.clone()));
    // Two registrations with the previous numbers record nothing, even beside one with other
    // numbers; nor does none at all, or one carrying other numbers.
    assert_eq!(
        put_back_registration(
            PutBack::AfterRemoval,
            &[install(&rc, "0.25.0"), install(&second, "0.25.0"), install(&other, "0.24.0")],
            Some(&previous)
        ),
        None
    );
    assert_eq!(put_back_registration(PutBack::AfterRemoval, &[], Some(&previous)), None);
    assert_eq!(put_back_registration(PutBack::AfterRemoval, &[install(&rc, "0.24.0")], Some(&previous)), None);
    // Without the previous release's version there is nothing to match against, and nothing is
    // recorded when no put-back ran.
    assert_eq!(put_back_registration(PutBack::AfterRemoval, &[install(&rc, "0.25.0")], None), None);
    assert_eq!(put_back_registration(PutBack::Not, &[install(&rc, "0.25.0")], Some(&previous)), None);
}

/// A removal that stopped with part of the app gone leaves registrations behind that cannot be
/// told apart from the restore — a registration that appeared since the snapshot can carry the
/// previous numbers just as the restore does, even when the put-back reused a code the app already
/// had. Such a put-back records nothing, whatever the registrations look like.
#[test]
fn a_partial_removal_records_nothing() {
    let previous = super::super::version::parse("0.25.0-rc.1").unwrap();
    let install = |code: &str, v: &str| (code.to_string(), super::super::version::parse(v));
    let (reused, appeared, newer) = (
        "{3D425FF2-3EB2-4950-9833-95B01B102845}".to_string(),
        "{6B1C1C1E-0E0B-4B6B-9C59-2C1D2E3F4A5B}".to_string(),
        "{A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D}".to_string(),
    );
    // The case that matters: the put-back went back in under a code the app already had, and a
    // registration that appeared after the snapshot carries the previous numbers too. The one
    // "new" code would have been taken for the restore; nothing is recorded instead.
    assert_eq!(
        put_back_registration(PutBack::AfterPartialRemoval, &[install(&reused, "0.25.0"), install(&appeared, "0.25.0")], Some(&previous)),
        None
    );
    // Even alone, a registration with the previous numbers is not the restore after a partial
    // removal: it may be one the removal step left behind.
    assert_eq!(put_back_registration(PutBack::AfterPartialRemoval, &[install(&appeared, "0.25.0")], Some(&previous)), None);
    assert_eq!(put_back_registration(PutBack::AfterPartialRemoval, &[install(&reused, "0.25.0")], Some(&previous)), None);
    assert_eq!(
        put_back_registration(PutBack::AfterPartialRemoval, &[install(&newer, "0.26.0"), install(&appeared, "0.25.0")], Some(&previous)),
        None
    );
    assert_eq!(put_back_registration(PutBack::AfterPartialRemoval, &[], Some(&previous)), None);
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
    // The session removes exactly the listing the downgrade snapshotted: one that is missing from
    // it — a registration that appears later — is not removed behind the snapshot's back.
    let later = "{A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D}";
    let snapshot = elevated_removals("FilmCraft", [(OURS, MsiScope::PerMachine)]).unwrap();
    assert_eq!(snapshot, vec![format!("/x {OURS} /passive /norestart")]);
    assert!(!snapshot.iter().any(|line| line.contains(later)), "a registration outside the snapshot stays put");
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
