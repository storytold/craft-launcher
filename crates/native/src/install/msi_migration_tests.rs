use std::cell::RefCell;

use super::*;

/// Runs a migration whose steps answer `install` and `remove`, and returns its result with the
/// steps that ran, in order.
fn migrate(install: Result<(), &str>, remove: Result<(), &str>) -> (Result<(), String>, Vec<&'static str>) {
    let ran = RefCell::new(Vec::new());
    let result = run(Steps {
        app: "PhotoCraft",
        chosen: "0.15.0",
        previous: "0.14.0",
        install_for_user: &mut || {
            ran.borrow_mut().push("install for this account");
            install.map_err(str::to_string)
        },
        remove_shared: &mut || {
            ran.borrow_mut().push("remove for everyone");
            remove.map_err(str::to_string)
        },
    });
    (result, ran.into_inner())
}

#[test]
fn the_copy_for_this_account_goes_in_before_the_shared_copy_is_removed() {
    let (result, ran) = migrate(Ok(()), Ok(()));
    assert_eq!(result, Ok(()));
    assert_eq!(ran, ["install for this account", "remove for everyone"]);
}

/// A managed account whose policy blocks per-user installs: the shared copy is never touched, so
/// the person keeps a working app and needs no administrator.
#[test]
fn a_failed_install_for_this_account_removes_nothing() {
    let (result, ran) = migrate(Err("the Windows Installer stopped with error 1625"), Ok(()));
    assert_eq!(ran, ["install for this account"]);
    let message = result.unwrap_err();
    assert!(
        message.contains("PhotoCraft 0.15.0 couldn't be installed for this account (the Windows Installer stopped with error 1625)"),
        "{message}"
    );
    assert!(message.contains("PhotoCraft 0.14.0 is still installed for everyone; nothing was removed"), "{message}");
}

/// The removal is the only permission prompt. Refusing it leaves both copies, and nothing asks again.
#[test]
fn a_refused_removal_leaves_both_copies_and_asks_nothing_more() {
    let (result, ran) = migrate(Ok(()), Err(CANCELLED));
    assert_eq!(ran, ["install for this account", "remove for everyone"]);
    let message = result.unwrap_err();
    assert!(message.contains("PhotoCraft 0.15.0 is installed for this account"), "{message}");
    assert!(message.contains("permission wasn't given") && !message.contains(CANCELLED), "{message}");
    assert!(message.contains("Both copies are installed now"), "{message}");
}

#[test]
fn a_failed_removal_says_how_to_remove_the_shared_copy() {
    let (result, _) = migrate(Ok(()), Err("the Windows Installer stopped with error 1603"));
    let message = result.unwrap_err();
    assert!(message.contains("couldn't be removed (the Windows Installer stopped with error 1603)"), "{message}");
    assert!(message.contains("Windows Settings > Apps"), "{message}");
}

/// The same product can't be registered for this account and for everyone at once, so a package
/// with the installed copy's product code is never migrated, whatever the case of its letters.
#[test]
fn a_package_of_the_installed_product_is_not_migrated() {
    let installed = "{12345678-1234-1234-1234-123456789ABC}";
    assert!(same_product(Some(installed), installed));
    assert!(same_product(Some(installed), &installed.to_ascii_lowercase()));
    assert!(!same_product(Some(installed), "{12345678-1234-1234-1234-123456789ABD}"));
    // Without the installed copy's code there is no telling, so nothing is migrated.
    assert!(same_product(None, installed));
}
