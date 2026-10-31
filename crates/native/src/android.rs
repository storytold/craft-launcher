//! Starting an installed app from the launcher.
//!
//! The workspace forbids `unsafe`, and reaching `PackageManager` needs JNI, so the two halves talk
//! through a small file instead: this writes the package id, the Activity reads it and calls
//! `startActivity`. The launcher is in the foreground whenever a card is tapped, so the Activity is
//! alive to see the request.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where the Activity looks for a pending request. Kept in sync with `MainActivity`.
pub const LAUNCH_FILE: &str = "launch-request.txt";

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Called once by the Android shell with the app's data directory.
pub fn set_root(dir: PathBuf) {
    let _ = ROOT.set(dir);
}

/// Ask the Activity to start `package`.
pub fn launch(package: &str) -> Result<(), String> {
    let root = ROOT.get().ok_or_else(|| "no data directory registered".to_string())?;
    write_request(root, package)
}

fn write_request(root: &Path, package: &str) -> Result<(), String> {
    std::fs::write(root.join(LAUNCH_FILE), package)
        .map_err(|e| format!("could not record the launch request: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_the_package_id_alone() {
        let dir = std::env::temp_dir().join("artcraft-launch-test");
        std::fs::create_dir_all(&dir).unwrap();
        write_request(&dir, "ai.storyteller.photocraft").unwrap();
        let written = std::fs::read_to_string(dir.join(LAUNCH_FILE)).unwrap();
        assert_eq!(written, "ai.storyteller.photocraft");
        std::fs::remove_dir_all(&dir).ok();
    }
}
