//! The Windows update helper: the launcher started with `--finish-update` does its step and
//! exits, without opening a window or claiming the single instance.

#![cfg(windows)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn the_update_helper_exits_without_opening_the_launcher() {
    let mut helper = Command::new(env!("CARGO_BIN_EXE_artcraft-launcher"))
        .arg("--finish-update")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = helper.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = helper.kill();
            panic!("the launcher started instead of the update helper");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success(), "incomplete helper arguments are an error");
}
