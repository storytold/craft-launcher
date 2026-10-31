use std::cell::RefCell;

use super::*;

/// Runs a step-by-step downgrade whose steps answer `remove` (an error, and whether something was
/// removed before it), `chosen` and `back`, and returns its result with the steps that ran, in order.
fn downgrade(remove: Result<(), (&str, bool)>, chosen: Result<(), &str>, back: Result<(), &str>) -> (Result<(), String>, Vec<&'static str>) {
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
    assert_eq!(result, Err("the Windows Installer stopped with error 1603".to_string()));
    assert_eq!(ran, ["remove"]);
}

/// An app registered twice (say per-user and all-users): the first copy is gone when removing the
/// second fails, so the previous version goes back in, and nothing claims nothing changed.
#[test]
fn a_removal_that_fails_after_removing_one_copy_puts_the_previous_version_back() {
    let (result, ran) = downgrade(Err(("the Windows Installer stopped with error 1603", true)), Ok(()), Ok(()));
    assert_eq!(ran, ["remove", "put back"]);
    let message = result.unwrap_err();
    assert!(message.contains("FilmCraft 0.14.0 couldn't be fully removed") && message.contains("still installed"), "{message}");
    assert!(!message.contains("nothing was changed"), "{message}");
    let (result, _) = downgrade(Err(("error 1603", true)), Ok(()), Err("error 1603"));
    let message = result.unwrap_err();
    assert!(message.contains("putting it back failed too") && message.contains("Install FilmCraft again from the launcher"), "{message}");
}

#[test]
fn a_failed_install_puts_the_previous_version_back_and_says_so() {
    let (result, ran) = downgrade(Ok(()), Err("the Windows Installer stopped with error 1603"), Ok(()));
    assert_eq!(ran, ["remove", "install chosen", "put back"]);
    let message = result.unwrap_err();
    assert!(message.contains("FilmCraft 0.13.0 couldn't be installed"), "{message}");
    assert!(message.contains("FilmCraft 0.14.0 was put back"), "{message}");
}

#[test]
fn a_refused_prompt_also_puts_the_previous_version_back() {
    let (result, ran) = downgrade(Ok(()), Err(CANCELLED), Ok(()));
    assert_eq!(ran, ["remove", "install chosen", "put back"]);
    let message = result.unwrap_err();
    assert!(message.contains("permission wasn't given"), "{message}");
    assert!(!message.contains(CANCELLED), "{message}");
}

#[test]
fn when_putting_back_fails_too_the_person_is_told_how_to_recover() {
    let (result, _) = downgrade(Ok(()), Err("error 1603"), Err(CANCELLED));
    let message = result.unwrap_err();
    assert!(message.contains("putting 0.14.0 back failed too (permission wasn't given)"), "{message}");
    assert!(message.contains("Install FilmCraft again from the launcher"), "{message}");
}
