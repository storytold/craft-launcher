//! Where the suite keeps its list of accounts.
//!
//! One folder for the whole Craft App Suite. It sits beside the per-application folders. It is not
//! a child of any of them.
//!
//! `craft-settings` gives each application `<platform config dir>/<app id>`. That is the right home
//! for that application's own preferences. An account is not one of those. The same person signs in
//! once and every Craft App acts as them. The record therefore belongs to the suite. An account
//! placed under one application's folder would make every other application read a folder it does
//! not own.
//! Removing that application would then sign everybody out. The launcher's own folder fails for the
//! same reason. An application must not have to know the launcher exists.
//!
//! So: `<platform config dir>/ArtCraft` on Windows and macOS. `<platform config dir>/artcraft` on
//! Linux and the BSDs. The launcher already spells its own folder that way per platform. Written
//! out, those are `%APPDATA%\ArtCraft`, `~/Library/Application Support/ArtCraft` and
//! `~/.config/artcraft`.
//!
//! It is the configuration folder. The record then follows a Windows profile the way a user would
//! expect. The per-application session file this replaces lived in `%APPDATA%` too. The folder
//! holds no secret. The token is in the keychain.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The suite folder's name, spelled the way the launcher spells its own per platform.
pub const DIR: &str = if cfg!(any(target_os = "macos", target_os = "windows")) { "ArtCraft" } else { "artcraft" };

/// The file inside it.
pub const FILE: &str = "accounts.json";

/// Points the suite folder somewhere else. Set and empty means read and write nothing.
pub const ENV_VAR: &str = "ARTCRAFT_SUITE_DIR";

/// File names beside the executable that switch portable mode on. The launcher's Windows portable
/// zip ships `portable.txt`. `portable` is `craft-settings`' own spelling.
pub const PORTABLE_MARKERS: &[&str] = &["portable.txt", "portable"];

/// The folder a portable install keeps the suite's accounts in, beside the executable.
pub const PORTABLE_DIR: &str = "ArtCraftSuite";

/// Which rule chose the folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// [`ENV_VAR`] named it.
    EnvVar,
    /// [`ENV_VAR`] was set and empty. Nothing is read or written.
    Isolated,
    /// A portable marker beside the executable.
    Portable,
    /// The platform's configuration folder.
    Platform,
    /// This system has no per-user configuration folder. Nothing is read or written. A file in the
    /// current directory would be the wrong home for a record of who is signed in.
    Nowhere,
}

/// A chosen folder and the rule behind it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The suite folder. `None` for [`Rule::Isolated`] and [`Rule::Nowhere`].
    pub dir: Option<PathBuf>,
    pub rule: Rule,
}

/// Reads an environment variable by name.
pub type EnvReader = Box<dyn Fn(&str) -> Option<OsString>>;

/// What the rules look at. The default reads the real process. Tests supply their own.
pub struct Probe {
    pub env: EnvReader,
    /// The folder holding the executable, if known.
    pub exe_dir: Option<PathBuf>,
    /// The platform configuration folder (`%APPDATA%`, `Application Support`, `~/.config`).
    pub platform_config_dir: Option<PathBuf>,
}

impl Default for Probe {
    fn default() -> Probe {
        Probe {
            env: Box::new(|name| std::env::var_os(name)),
            exe_dir: std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)),
            platform_config_dir: dirs::config_dir(),
        }
    }
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probe").field("exe_dir", &self.exe_dir).field("platform_config_dir", &self.platform_config_dir).finish_non_exhaustive()
    }
}

/// Applies the rules, first match winning.
pub fn resolve(probe: &Probe) -> Resolved {
    if let Some(value) = (probe.env)(ENV_VAR) {
        if value.is_empty() {
            return Resolved { dir: None, rule: Rule::Isolated };
        }
        return Resolved { dir: Some(PathBuf::from(value)), rule: Rule::EnvVar };
    }
    if let Some(dir) = probe.exe_dir.as_deref().and_then(portable_dir) {
        return Resolved { dir: Some(dir), rule: Rule::Portable };
    }
    match &probe.platform_config_dir {
        Some(base) => Resolved { dir: Some(base.join(DIR)), rule: Rule::Platform },
        None => Resolved { dir: None, rule: Rule::Nowhere },
    }
}

/// The accounts file inside a suite folder.
pub fn file(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The folder a portable marker points at, when `exe_dir` has one.
///
/// On macOS the executable sits in `X.app/Contents/MacOS`. The marker is therefore looked for
/// beside the `.app` bundle. Writing inside a signed bundle breaks its signature.
fn portable_dir(exe_dir: &Path) -> Option<PathBuf> {
    let base = bundle_parent(exe_dir).unwrap_or(exe_dir);
    PORTABLE_MARKERS.iter().any(|m| base.join(m).is_file()).then(|| base.join(PORTABLE_DIR))
}

fn bundle_parent(exe_dir: &Path) -> Option<&Path> {
    if !cfg!(target_os = "macos") || exe_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents = exe_dir.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let bundle = contents.parent()?;
    if bundle.extension()? != "app" {
        return None;
    }
    bundle.parent()
}
