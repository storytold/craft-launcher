//! The control channel: a Craft App opened with `--control` serves JSON lines on a loopback port,
//! so an agent can drive its window while the user watches.
//!
//! Every Craft App has one, switched on its own way ([`Style`], the catalog's `control`); [`args`]
//! builds the arguments that switch it on.

use crate::catalog::{CraftApp, SUITE};

/// How an app switches its control channel on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// `--control <port>`: it listens on `127.0.0.1:<port>`.
    Port,
    /// `--control <port> --control-token-file <file>` (PhotoCraft): every connection authenticates
    /// first, with the token in `<file>`, which the app writes when the file isn't there.
    PortAndTokenFile,
    /// `--control <file>` (PdfCraft): the app picks a free port and writes it, with a token, to
    /// `<file>` as JSON (`{"port", "token", "pid"}`).
    File,
}

/// PhotoCraft's port, the one its documentation uses; the other apps take the ports after it.
const FIRST_PORT: u16 = 7878;

/// The port an app's control channel gets until the user picks another: one each, in suite order
/// from PhotoCraft's 7878, so every app can be open for agents at the same time.
pub fn default_port(app: &CraftApp) -> u16 {
    let index = SUITE.iter().position(|a| a.id == app.id).and_then(|i| u16::try_from(i).ok()).unwrap_or(0);
    FIRST_PORT.saturating_add(index)
}

/// The name of the file the app keeps its token or connection details in, for the styles that
/// have one.
pub fn file_name(app: &CraftApp) -> Option<String> {
    match app.control {
        Style::Port => None,
        Style::PortAndTokenFile => Some(format!("{}-control.token", app.id)),
        Style::File => Some(format!("{}-control.json", app.id)),
    }
}

/// The arguments that open `app` with its control channel on `port`. `file` is where it keeps its
/// token or connection details ([`file_name`]); the styles without one leave it out.
pub fn args(app: &CraftApp, port: u16, file: &str) -> Vec<String> {
    let port = port.to_string();
    match app.control {
        Style::Port => vec!["--control".into(), port],
        Style::PortAndTokenFile => vec!["--control".into(), port, "--control-token-file".into(), file.into()],
        Style::File => vec!["--control".into(), file.into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> &'static CraftApp {
        CraftApp::by_id(id).unwrap()
    }

    #[test]
    fn every_app_gets_a_port_of_its_own() {
        assert_eq!(default_port(app("photocraft")), 7878);
        assert_eq!(default_port(app("vectorcraft")), 7879);
        let mut ports: Vec<u16> = SUITE.iter().map(default_port).collect();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(ports.len(), SUITE.len());
    }

    #[test]
    fn each_style_has_its_own_arguments() {
        assert_eq!(args(app("vectorcraft"), 7879, ""), ["--control", "7879"]);
        assert_eq!(
            args(app("photocraft"), 7878, "/c/photocraft-control.token"),
            ["--control", "7878", "--control-token-file", "/c/photocraft-control.token"]
        );
        assert_eq!(args(app("pdfcraft"), 7882, "/c/pdfcraft-control.json"), ["--control", "/c/pdfcraft-control.json"]);
    }

    #[test]
    fn only_apps_that_keep_a_token_have_a_file() {
        assert_eq!(file_name(app("photocraft")).as_deref(), Some("photocraft-control.token"));
        assert_eq!(file_name(app("pdfcraft")).as_deref(), Some("pdfcraft-control.json"));
        assert_eq!(file_name(app("wordcraft")), None);
        let with_files = SUITE.iter().filter(|a| file_name(a).is_some()).count();
        assert_eq!(with_files, 2);
    }
}
