//! The arguments the desktop hands the launcher.
//!
//! It reads two flags — `--version`/`-V` and `--hidden` — and ignores everything else, because a
//! desktop start-up brings along what it brings along: `-psn_0_…` from Finder when a bundle is
//! opened, a file dropped on the icon, an old login entry's leftovers. An argument the launcher
//! cannot read is ignored in the same way, rather than ending the start-up before the window.
//!
//! `--version` is the only thing the launcher does without opening a window, and the packaging
//! scripts use it as their smoke test (`packaging/linux/package.sh` runs it on the staged binary,
//! under an emulator for a foreign architecture), so it is what a test can ask for from here.

use std::process::Command;

fn launcher() -> Command {
    Command::new(env!("CARGO_BIN_EXE_artcraft-launcher"))
}

const VERSION_LINE: &str = concat!("artcraft-launcher ", env!("CARGO_PKG_VERSION"), "\n");

#[test]
fn the_version_is_printed_with_arguments_the_desktop_adds() {
    for extra in [vec![], vec!["-psn_0_1234567"], vec!["/tmp/a file.png"]] {
        for flag in ["--version", "-V"] {
            let mut args = extra.clone();
            args.push(flag);
            let out = launcher().args(&args).output().unwrap_or_else(|e| panic!("{args:?}: {e}"));
            let err = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(0), "{args:?}: {err}");
            assert_eq!(String::from_utf8_lossy(&out.stdout), VERSION_LINE, "{args:?}");
        }
    }
}

/// `std::env::args` panics on an argument that is not UTF-8, which a Latin-1 file name on Linux
/// or macOS is, and the panic came before the flags were looked at: every start-up with such an
/// argument ended in `internal error: panicked at library/std/src/env.rs` and exit 101, the
/// launcher's own panic report (photocraft#1108 is the same bug in that CLI).
#[cfg(unix)]
#[test]
fn an_argument_that_is_not_utf8_is_ignored_like_any_other() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let out = launcher().arg(OsStr::from_bytes(b"/tmp/caf\xe9.png")).arg("--version").output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "an ignored argument, not a panic (101): {err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), VERSION_LINE);
    assert!(!err.contains("panicked"), "{err}");
}
