//! Reuse the launcher's installed-app detection; never fetch or install software.

use std::path::{Path, PathBuf};
use std::process::Command;

use artcraft_launcher_core::{CraftApp, InstallSource, Installed, SUITE};
use artcraft_launcher_native::{detect, manifest::Manifest, paths::Paths, process};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub path: PathBuf,
    pub args: Vec<String>,
}

impl Program {
    pub fn command(&self) -> Command {
        let mut command = process::command(&self.path);
        command.args(&self.args);
        command
    }
}

pub(crate) struct Found {
    pub app: &'static CraftApp,
    pub installed: Option<Installed>,
    pub cli: Option<Program>,
}

impl Found {
    pub fn describe(&self) -> Value {
        json!({
            "app":self.app.id, "name":self.app.name, "purpose":self.app.tagline,
            "installed":self.installed.is_some() || self.cli.is_some(),
            "cli_available":self.cli.is_some(),
            "version":self.installed.as_ref().and_then(|i| i.version.as_ref()).map(ToString::to_string),
            "live_supported":live_flag(self.app.id).is_some(),
            "headless_available":self.cli.is_some(),
            "installation":self.installed.as_ref().map(|i| &i.location),
        })
    }

    pub fn gui(&self) -> Result<Program, String> {
        let installed = self.installed.as_ref().ok_or("Desktop app not detected; install it with ArtCraft Launcher")?;
        let path = PathBuf::from(&installed.location);
        #[cfg(target_os = "macos")]
        if path.extension().is_some_and(|e| e == "app") {
            return Ok(Program { path: "open".into(), args: vec!["-n".into(), "-a".into(), path.display().to_string(), "--args".into()] });
        }
        if path.is_file() && !matches!(installed.source, InstallSource::AppImage | InstallSource::Flatpak) {
            return Ok(Program { path, args: Vec::new() });
        }
        Err("Live launching this installation format is not supported yet; start the app with --control PORT and supply port to app_connect".into())
    }
}

/// Only adapters whose live argument conventions are implemented in this first increment.
pub(crate) fn live_flag(app: &str) -> Option<&'static str> {
    match app {
        "figcraft" => Some("--attach"),
        "designcraft" | "vectorcraft" => Some("--connect"),
        _ => None,
    }
}

pub(crate) fn scan(cli_dirs: &[PathBuf]) -> Vec<Found> {
    let manifest = Paths::new().ok().map(|p| Manifest::load(&p.manifest())).unwrap_or_default();
    let snapshot = detect::Snapshot::take();
    SUITE
        .iter()
        .map(|app| {
            let installed = detect::detect(app, &manifest, &snapshot);
            let cli = find_cli(app, installed.as_ref(), cli_dirs);
            Found { app, installed, cli }
        })
        .collect()
}

fn find_cli(app: &CraftApp, installed: Option<&Installed>, cli_dirs: &[PathBuf]) -> Option<Program> {
    let mut dirs = cli_dirs.to_vec();
    if let Some(installed) = installed {
        let location = Path::new(&installed.location);
        if let Some(parent) = location.parent() {
            dirs.push(parent.to_path_buf());
        }
        if location.is_dir() {
            dirs.push(location.join("bin"));
            dirs.push(location.join("Contents/MacOS"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    for dir in dirs {
        for stem in app.stems() {
            let path = dir.join(format!("{stem}-cli{}", std::env::consts::EXE_SUFFIX));
            if path.is_file() {
                return Some(Program { path, args: Vec::new() });
            }
        }
    }
    if let Some(installed) = installed
        && installed.source == InstallSource::Flatpak
    {
        // Match how the launcher reaches host Flatpaks from inside its own Flatpak.
        let mut args: Vec<String> = if process::in_sandbox() { vec!["--host".into(), "flatpak".into()] } else { Vec::new() };
        args.extend(["run".into(), format!("--command={}-cli", app.id), app.bundle_id()]);
        return Some(Program { path: if process::in_sandbox() { "flatpak-spawn".into() } else { "flatpak".into() }, args });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_cli_installed_after_the_first_scan_without_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let app = CraftApp::by_id("figcraft").unwrap();
        let dirs = [root.path().to_path_buf()];
        assert!(find_cli(app, None, &dirs).is_none_or(|p| !p.path.starts_with(root.path())));
        let path = root.path().join(format!("figcraft-cli{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, b"test fixture, never executed").unwrap();
        assert_eq!(find_cli(app, None, &dirs).unwrap().path, path);
    }

    #[test]
    fn live_coverage_is_explicit_and_pdf_never_claims_live_support() {
        assert_eq!(live_flag("figcraft"), Some("--attach"));
        assert_eq!(live_flag("vectorcraft"), Some("--connect"));
        assert_eq!(live_flag("designcraft"), Some("--connect"));
        assert_eq!(live_flag("pdfcraft"), None);
        assert_eq!(live_flag("not-an-app"), None);
    }
}
