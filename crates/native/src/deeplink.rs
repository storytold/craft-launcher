//! Where another Craft app sends the launcher: `extensions?app=photocraft&kind=theme`.
//!
//! An app starts the launcher with `--open extensions?app=<id>&kind=<kind>`; a launcher that is
//! already running gets the same text over the single-instance handshake.
//!
//! Invariants:
//! - The text comes from another program on this computer, so it is never trusted: only the page
//!   name `extensions` is accepted, an app is kept only if it looks like an app id, a kind only if
//!   it is one of the three known ones, and anything else in the text is ignored. A bad value is
//!   dropped, never repaired, and the page still opens unfiltered.
//! - Nothing here acts on the text: it only names a place to show.

/// Longest app id the launcher will carry (the store's own limit).
const MAX_APP: usize = 40;
/// The kinds of extension, as the store spells them.
const KINDS: [&str; 3] = ["theme", "plugin", "resource"];

/// A place in the launcher to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// Only what works with this app.
    pub app: Option<String>,
    /// Only this kind: `theme`, `plugin` or `resource`.
    pub kind: Option<String>,
}

impl Target {
    /// Reads `extensions` or `extensions?app=..&kind=..`. `None` for any other page.
    pub fn parse(text: &str) -> Option<Target> {
        let text = text.trim();
        let (page, query) = text.split_once('?').unwrap_or((text, ""));
        if page != "extensions" {
            return None;
        }
        let mut target = Target::default();
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("app", v)) if valid_app(v) => target.app = Some(v.to_string()),
                Some(("kind", v)) if KINDS.contains(&v) => target.kind = Some(v.to_string()),
                _ => {}
            }
        }
        Some(target)
    }

    /// The text [`Target::parse`] reads back.
    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        if let Some(app) = &self.app {
            parts.push(format!("app={app}"));
        }
        if let Some(kind) = &self.kind {
            parts.push(format!("kind={kind}"));
        }
        if parts.is_empty() { "extensions".to_string() } else { format!("extensions?{}", parts.join("&")) }
    }
}

fn valid_app(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= MAX_APP
        && v.starts_with(|c: char| c.is_ascii_lowercase())
        && v.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
}

/// The target named by `--open <text>` or `--open=<text>` among `args`, if any. A page this
/// launcher does not have counts as no request.
pub fn from_args(args: &[String]) -> Option<Target> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if let Some(text) = arg.strip_prefix("--open=") {
            return Target::parse(text);
        }
        if arg == "--open" {
            return it.next().and_then(|t| Target::parse(t));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(app: Option<&str>, kind: Option<&str>) -> Option<Target> {
        Some(Target { app: app.map(String::from), kind: kind.map(String::from) })
    }

    #[test]
    fn reads_the_page_with_and_without_filters() {
        assert_eq!(Target::parse("extensions"), t(None, None));
        assert_eq!(Target::parse("extensions?app=photocraft"), t(Some("photocraft"), None));
        assert_eq!(Target::parse("extensions?app=photocraft&kind=theme"), t(Some("photocraft"), Some("theme")));
        assert_eq!(Target::parse("extensions?kind=plugin&app=pdfcraft"), t(Some("pdfcraft"), Some("plugin")));
        assert_eq!(Target::parse("  extensions?kind=resource \n"), t(None, Some("resource")));
    }

    #[test]
    fn any_other_page_is_not_a_request() {
        for text in ["", "settings", "Extensions", "extensions/", "https://example.com", "extensionsx?app=a", "?app=photocraft"] {
            assert_eq!(Target::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn bad_values_are_dropped_and_the_page_still_opens() {
        for text in [
            "extensions?app=",
            "extensions?app=PhotoCraft",
            "extensions?app=photo craft",
            "extensions?app=../../etc",
            "extensions?app=1photocraft",
            "extensions?app=photocraft%00",
            "extensions?app=a_b",
            "extensions?kind=Theme",
            "extensions?kind=",
            "extensions?kind=themes",
            "extensions?app",
            "extensions?=photocraft",
        ] {
            assert_eq!(Target::parse(text), t(None, None), "{text:?}");
        }
        assert_eq!(Target::parse(&format!("extensions?app={}", "a".repeat(41))), t(None, None));
        assert_eq!(Target::parse(&format!("extensions?app={}", "a".repeat(40))), t(Some(&"a".repeat(40)), None));
    }

    #[test]
    fn a_good_value_survives_a_bad_neighbour_and_unknown_names_are_ignored() {
        assert_eq!(Target::parse("extensions?app=Bad&kind=theme"), t(None, Some("theme")));
        assert_eq!(Target::parse("extensions?kind=nope&app=photocraft&x=1&app"), t(Some("photocraft"), None));
        // The last good value wins, as in any query.
        assert_eq!(Target::parse("extensions?app=a&app=b"), t(Some("b"), None));
    }

    #[test]
    fn text_reads_back_the_same() {
        for target in [
            Target::default(),
            Target { app: Some("photocraft".into()), kind: None },
            Target { app: Some("lightcraft".into()), kind: Some("theme".into()) },
        ] {
            assert_eq!(Target::parse(&target.text()), Some(target));
        }
        assert_eq!(Target::default().text(), "extensions");
    }

    #[test]
    fn the_command_line_takes_the_text_after_open_in_either_spelling() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(from_args(&args(&["--open", "extensions?app=photocraft"])), t(Some("photocraft"), None));
        assert_eq!(from_args(&args(&["--hidden", "--open=extensions?kind=theme"])), t(None, Some("theme")));
        assert_eq!(from_args(&args(&["--open"])), None);
        assert_eq!(from_args(&args(&["--open", "settings"])), None);
        assert_eq!(from_args(&args(&["--hidden"])), None);
        assert_eq!(from_args(&[]), None);
    }
}
