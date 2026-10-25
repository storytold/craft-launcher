//! The Windows MSI batch helper: the launcher started with `--install-msis` refuses arguments
//! the launcher wouldn't send, installs nothing unless it is signed by ArtCraft's publisher, and
//! exits, without opening a window or claiming the single instance.
//!
//! Running the exe also checks it starts at all: it links with `/DEPENDENTLOADFLAG` (`build.rs`),
//! so every DLL it imports must resolve from System32.

#![cfg(windows)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A new, empty folder of this test's own.
fn folder(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("artcraft-launcher-msi-helper-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str]) -> std::process::ExitStatus {
    let mut helper = Command::new(env!("CARGO_BIN_EXE_artcraft-launcher"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = helper.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = helper.kill();
            panic!("the launcher started instead of the MSI helper");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_launcher_starts_with_its_imports_from_system32() {
    assert!(run(&["--version"]).success());
}

#[test]
fn the_msi_helper_refuses_bad_arguments_and_exits() {
    let dir = folder("arguments");
    let results = dir.join("results.txt");
    let results = results.to_str().unwrap();
    let msi = dir.join("photocraft-0.5.0-windows-x64.msi");
    let msi = msi.to_str().unwrap();
    let sha = "a".repeat(64);
    assert_eq!(run(&["--install-msis"]).code(), Some(2), "incomplete arguments");
    assert_eq!(run(&["--install-msis", results, "relative.msi", &sha, "-"]).code(), Some(2), "a relative MSI");
    assert_eq!(run(&["--install-msis", results, msi, "not-a-checksum", "-"]).code(), Some(2), "a bad checksum");
    assert!(!std::path::Path::new(results).exists(), "nothing was written");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_unsigned_build_installs_nothing_even_when_asked_properly() {
    let dir = folder("unsigned");
    let results = dir.join("results.txt");
    let msi = dir.join("photocraft-0.5.0-windows-x64.msi");
    std::fs::write(&msi, b"not really a package").unwrap();
    let sha = "a".repeat(64);
    let code = run(&["--install-msis", results.to_str().unwrap(), msi.to_str().unwrap(), &sha, "-"]).code();
    assert_eq!(code, Some(4), "the test build isn't signed by ArtCraft's publisher");
    assert!(!results.exists(), "it stopped before touching anything");
    let _ = std::fs::remove_dir_all(dir);
}
