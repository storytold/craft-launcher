//! Small pieces of an app card's text: the version in its top strip, and the short description and
//! last-updated line in its body.
//!
//! Invariants: nothing is drawn that has no content; dates come from `views::date_text`.

use artcraft_launcher_core::status::AppStatus;
use egui::{Color32, FontId, Ui};

use crate::i18n::fmt;
use crate::model::AppEntry;

/// The version in a card's top strip, as pieces `(text, highlight)`: only a highlighted piece takes
/// the app's colour. "v0.4.0" for an app; "0.5.0 → 0.6.0" when an update is waiting, with only the
/// new version highlighted; "v0.4.0 · latest 0.6.0" for a version kept on purpose (`behind`, the
/// newer release), the note left plain so it informs without asking for anything. Empty when
/// nothing is known yet.
pub(crate) fn strip_version(e: &AppEntry, status: &AppStatus, behind: Option<&str>) -> Vec<(String, bool)> {
    let installed = e.installed.as_ref().and_then(|i| i.version.as_ref().map(ToString::to_string));
    let latest = e.latest_label();
    match (status, installed.as_deref(), latest.as_deref()) {
        (AppStatus::UpdateAvailable, Some(i), Some(l)) => vec![(format!("{i} → "), false), (l.to_string(), true)],
        (_, Some(i), _) => {
            let mut parts = vec![(format!("v{i}"), true)];
            if let Some(newer) = behind {
                parts.push((format!(" · {}", fmt(tl!("latest {version}"), &[("version", newer)])), false));
            }
            parts
        }
        (_, None, Some(l)) => vec![(format!("v{l}"), true)],
        _ => Vec::new(),
    }
}

/// "Last updated: Oct 7, 2026": the day of the app's latest release on this computer's calendar,
/// written like every other date in the launcher.
pub(crate) fn last_updated(e: &AppEntry) -> Option<String> {
    let at = e.release().and_then(|r| r.published_at)?;
    Some(fmt(tl!("Last updated: {date}"), &[("date", &crate::views::date_text(at))]))
}

/// `text` in at most `rows` lines, ending in "…" when it is cut.
pub(crate) fn blurb(ui: &mut Ui, text: &str, color: Color32, rows: usize) {
    let mut job = egui::text::LayoutJob::simple(text.to_string(), FontId::proportional(13.0), color, ui.available_width());
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = false;
    job.wrap.overflow_character = Some('…');
    // Not selectable, so a click on the text still opens the app's page.
    ui.add(egui::Label::new(job).selectable(false));
}

#[cfg(test)]
mod tests {
    use artcraft_launcher_core::protocol::LatestRelease;
    use artcraft_launcher_core::{CraftApp, InstallSource, Installed, Release, version};

    use super::*;

    fn entry(installed: &str, latest: &str) -> AppEntry {
        let mut e = AppEntry::new(CraftApp::by_id("photocraft").unwrap());
        e.installed = Some(Installed { version: version::parse(installed), source: InstallSource::Msi, location: String::new(), package: None });
        e.latest = Some(LatestRelease::Found(Release { tag: format!("v{latest}"), version: version::parse(latest), ..Default::default() }));
        e
    }

    #[test]
    fn an_update_colours_only_the_new_version() {
        let parts = strip_version(&entry("0.5.0", "0.6.0"), &AppStatus::UpdateAvailable, None);
        assert_eq!(parts, [("0.5.0 → ".to_string(), false), ("0.6.0".to_string(), true)]);
    }

    #[test]
    fn a_kept_version_notes_the_newer_release_without_colour() {
        let parts = strip_version(&entry("0.16.0", "0.16.0"), &AppStatus::UpToDate, Some("0.17.0"));
        assert_eq!(parts, [("v0.16.0".to_string(), true), (" · latest 0.17.0".to_string(), false)]);
        // Nothing newer: just the version.
        assert_eq!(strip_version(&entry("0.17.0", "0.17.0"), &AppStatus::UpToDate, None), [("v0.17.0".to_string(), true)]);
    }
}
