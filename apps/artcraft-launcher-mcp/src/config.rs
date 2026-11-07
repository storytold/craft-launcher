//! Operator-owned startup settings, never writable through MCP tool arguments.

use std::collections::BTreeMap;
use std::path::PathBuf;

use artcraft_launcher_core::CraftApp;

#[derive(Default)]
pub(crate) struct Config {
    pub cli_dirs: Vec<PathBuf>,
    cli_args: BTreeMap<String, Vec<String>>,
    gui_args: BTreeMap<String, Vec<String>>,
}

impl Config {
    pub fn add_args(&mut self, gui: bool, app: &str, encoded: &str) -> Result<(), String> {
        if CraftApp::by_id(app).is_none() {
            return Err(format!("Unknown app in startup settings: {app}"));
        }
        if encoded.len() > 65536 {
            return Err("Native arguments exceed 64 KiB".into());
        }
        let args: Vec<String> = serde_json::from_str(encoded).map_err(|e| format!("Native arguments must be a JSON array of strings: {e}"))?;
        if args.len() > 64 || args.iter().any(|arg| arg.contains('\0')) {
            return Err("Native arguments must contain at most 64 strings without NUL characters".into());
        }
        // Routing belongs to the gateway. Do not allow an option terminator or a
        // second endpoint option to reinterpret/override its loopback arguments.
        if args
            .iter()
            .any(|arg| arg == "--" || ["--attach", "--connect", "--control"].iter().any(|flag| arg == flag || arg.starts_with(&format!("{flag}="))))
        {
            return Err("Native settings cannot override --attach, --connect, --control or use --".into());
        }
        let settings = if gui { &mut self.gui_args } else { &mut self.cli_args };
        if settings.contains_key(app) {
            return Err(format!("Native arguments for {app} were already configured for this target"));
        }
        settings.insert(app.into(), args);
        Ok(())
    }

    pub fn args(&self, gui: bool, app: &str) -> &[String] {
        let settings = if gui { &self.gui_args } else { &self.cli_args };
        settings.get(app).map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_native_restrictions_and_argument_boundaries_unchanged() {
        let mut config = Config::default();
        let args = vec!["--allow-commands", "inspect,export", "--automation-read-root", "folder with spaces; $(literal)", ""];
        config.add_args(false, "figcraft", &serde_json::to_string(&args).unwrap()).unwrap();
        assert_eq!(config.args(false, "figcraft"), args);
        assert!(config.args(true, "figcraft").is_empty());
        assert!(config.args(false, "designcraft").is_empty());
        config.add_args(true, "figcraft", r#"["--automation-read-root","/projects"]"#).unwrap();
        assert_eq!(config.args(true, "figcraft"), ["--automation-read-root", "/projects"]);
        assert!(config.add_args(false, "figcraft", "[]").is_err());
    }

    #[test]
    fn refuses_unknown_apps_invalid_arrays_and_endpoint_overrides() {
        for encoded in [
            r#"["--attach","evil.example:80"]"#,
            r#"["--connect=localhost:80"]"#,
            r#"["--control=80"]"#,
            r#"["--"]"#,
            r#"[1]"#,
            r#"{}"#,
            r#"["\u0000"]"#,
        ] {
            assert!(Config::default().add_args(false, "figcraft", encoded).is_err(), "{encoded}");
        }
        assert!(Config::default().add_args(true, "unknown-app", "[]").is_err());
        assert!(Config::default().add_args(false, "figcraft", &serde_json::to_string(&vec!["arg"; 65]).unwrap()).is_err());
        assert!(Config::default().add_args(false, "figcraft", &" ".repeat(65537)).is_err());
    }
}
