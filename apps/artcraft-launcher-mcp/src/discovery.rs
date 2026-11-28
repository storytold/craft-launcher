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
    pub fn command(&self) -> Result<Command, String> {
        if !self.path.is_absolute() || !self.path.is_file() {
            return Err(format!("CLI/helper must be an existing absolute file: {}", self.path.display()));
        }
        let mut command = process::command(&self.path);
        command.args(&self.args);
        Ok(command)
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
        let location = Path::new(&installed.location);
        if !location.is_absolute() {
            return Err("The detected desktop path must be absolute".into());
        }
        let path = location.canonicalize().map_err(|e| format!("Cannot resolve the detected desktop path: {e}"))?;
        #[cfg(target_os = "macos")]
        if path.extension().is_some_and(|e| e == "app") {
            return Ok(Program {
                path: Path::new("/usr/bin/open").canonicalize().map_err(|e| e.to_string())?,
                args: vec!["-n".into(), "-a".into(), path.display().to_string(), "--args".into()],
            });
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
        // A Flatpak id or a relative manifest entry is not a filesystem trust root.
        if location.is_absolute()
            && let Ok(location) = location.canonicalize()
        {
            if location.is_dir() {
                dirs.push(location.join("bin"));
                dirs.push(location.join("Contents/MacOS"));
            } else if location.is_file()
                && let Some(parent) = location.parent()
            {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    for dir in dirs {
        if !dir.is_absolute() {
            continue;
        }
        for stem in app.stems() {
            let path = dir.join(format!("{stem}-cli{}", std::env::consts::EXE_SUFFIX));
            if let Ok(path) = path.canonicalize()
                && path.is_file()
            {
                return Some(Program { path, args: Vec::new() });
            }
        }
    }
    if let Some(installed) = installed
        && installed.source == InstallSource::Flatpak
    {
        // Match how the launcher reaches host Flatpaks from inside its own Flatpak.
        let sandbox = process::in_sandbox();
        let mut args: Vec<String> = if sandbox { vec!["--host".into(), "/usr/bin/flatpak".into()] } else { Vec::new() };
        args.extend(["run".into(), format!("--command={}-cli", app.id), app.bundle_id()]);
        let path = Path::new(if sandbox { "/usr/bin/flatpak-spawn" } else { "/usr/bin/flatpak" }).canonicalize().ok()?;
        return path.is_file().then_some(Program { path, args });
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
        assert_eq!(find_cli(app, None, &dirs).unwrap().path, path.canonicalize().unwrap());
    }

    #[test]
    fn ignores_path_even_when_it_contains_a_matching_cli() {
        // A subprocess gives this test its own PATH without unsafe global env mutation.
        const MARKER: &str = "ARTCRAFT_MCP_PATH_TEST";
        if std::env::var_os(MARKER).is_some() {
            assert!(find_cli(CraftApp::by_id("figcraft").unwrap(), None, &[]).is_none());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(format!("figcraft-cli{}", std::env::consts::EXE_SUFFIX)), b"never executed").unwrap();
        let result = process::command(std::env::current_exe().unwrap())
            .args(["--exact", "discovery::tests::ignores_path_even_when_it_contains_a_matching_cli"])
            .env("PATH", root.path())
            .env(MARKER, "1")
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stdout));
    }

    #[test]
    fn detected_installation_uses_an_absolute_cli_and_never_a_relative_manifest_entry() {
        let root = tempfile::tempdir().unwrap();
        let gui = root.path().join(format!("figcraft{}", std::env::consts::EXE_SUFFIX));
        let cli = root.path().join(format!("figcraft-cli{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&gui, b"not executed").unwrap();
        std::fs::write(&cli, b"not executed").unwrap();
        let app = CraftApp::by_id("figcraft").unwrap();
        let mut installed = Installed { version: None, source: InstallSource::Launcher, location: gui.display().to_string(), package: None };
        assert_eq!(find_cli(app, Some(&installed), &[]).unwrap().path, cli.canonicalize().unwrap());
        installed.location = "figcraft.exe".into();
        assert!(find_cli(app, Some(&installed), &[]).is_none());
        assert!(Found { app, installed: Some(installed), cli: None }.gui().is_err());
    }

    #[test]
    fn process_arguments_keep_spaces_and_shell_characters_as_literal_arguments() {
        let args = vec!["--allow-commands".into(), "inspect,export".into(), "folder with spaces; $(literal)".into()];
        let program = Program { path: std::env::current_exe().unwrap().canonicalize().unwrap(), args: args.clone() };
        let command = program.command().unwrap();
        assert_eq!(command.get_program(), program.path.as_os_str());
        assert_eq!(command.get_args().collect::<Vec<_>>(), args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>());
        assert!(Program { path: "figcraft-cli".into(), args: vec![] }.command().is_err());
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
