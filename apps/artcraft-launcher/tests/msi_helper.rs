//! The Windows MSI batch helper: the launcher started with `--install-msis` refuses arguments
//! the launcher wouldn't send and exits, without opening a window or claiming the single
//! instance.

#![cfg(windows)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
fn the_msi_helper_refuses_bad_arguments_and_exits() {
    assert_eq!(run(&["--install-msis"]).code(), Some(2), "incomplete arguments");
    let results = std::env::temp_dir().join("artcraft-launcher-msi-helper-test.txt");
    let results = results.to_str().unwrap();
    assert_eq!(run(&["--install-msis", results, "relative.msi", &"a".repeat(64), "-"]).code(), Some(2), "a relative MSI");
    assert_eq!(run(&["--install-msis", results, r"C:\x\a.msi", "not-a-checksum", "-"]).code(), Some(2), "a bad checksum");
    assert!(!std::path::Path::new(results).exists(), "nothing was written");
}
