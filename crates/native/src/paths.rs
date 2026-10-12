//! Where the launcher keeps its own files.

use std::path::PathBuf;

/// The launcher's folders.
#[derive(Clone, Debug)]
pub struct Paths {
    /// Records of what the launcher installed, and tarball installs.
    pub data: PathBuf,
    /// Saved release answers and in-progress downloads.
    pub cache: PathBuf,
}

const DIR: &str = if cfg!(any(target_os = "macos", target_os = "windows")) { "ArtCraft Launcher" } else { "artcraft-launcher" };

impl Paths {
    pub fn new() -> Result<Self, String> {
        if let Some(root) = portable_root() {
            return Ok(Self::under(&root));
        }
        let data = dirs::data_local_dir().ok_or("no per-user data folder on this system")?.join(DIR);
        let cache = dirs::cache_dir().ok_or("no per-user cache folder on this system")?.join(DIR);
        Ok(Self { data, cache })
    }

    /// For tests: everything under one folder.
    pub fn under(root: &std::path::Path) -> Self {
        Self { data: root.join("data"), cache: root.join("cache") }
    }

    pub fn downloads(&self) -> PathBuf {
        self.cache.join("downloads")
    }

    pub fn release_cache(&self) -> PathBuf {
        self.cache.join("releases.json")
    }

    /// The last answer from ArtCraft's release feed, for an instant first paint and as a fallback.
    pub fn feed_cache(&self) -> PathBuf {
        self.cache.join("feed.json")
    }

    /// How long to hold off calling ArtCraft's API (after failures or HTTP 429), across restarts.
    pub fn feed_throttle(&self) -> PathBuf {
        self.cache.join("feed-throttle.json")
    }

    pub fn manifest(&self) -> PathBuf {
        self.data.join("installed.json")
    }

    /// The launcher's own log files (`artcraft-launcher.log` and the previous runs').
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }

    /// Where tarball installs (FreeBSD, Linux without AppImage) are unpacked: `<data>/apps/<app>`.
    pub fn apps(&self) -> PathBuf {
        self.data.join("apps")
    }

    /// The folder an app unpacked in [`Self::apps`] sees as its home: `<data>/apps/<app>/home`.
    /// A Craft App keeps its settings in the XDG folders (`~/.config`, `~/.local/share`,
    /// `~/.cache`), so pointing `HOME` and those variables here keeps every app's configuration
    /// beside it, inside the folder the launcher owns.
    #[cfg(target_os = "linux")]
    pub fn app_home(&self, id: &str) -> PathBuf {
        self.apps().join(id).join("home")
    }
}

/// Writes `bytes` to `path` via a temporary file and a rename, so a crash never leaves half a file.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("couldn't write {} ({e})", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("couldn't replace {} ({e})", path.display()))
}

/// Portable mode: a `portable.txt` beside the executable keeps everything in
/// `ArtCraftLauncherData` next to it (the Windows portable zip ships one).
pub fn portable_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    dir.join("portable.txt").is_file().then(|| dir.join("ArtCraftLauncherData"))
}
