//! Small pieces of an app card's text: the version in its top strip, and the short description and
//! last-updated line in its body.
//!
//! Invariants: nothing is drawn that has no content; dates come from `views::date_text`.

use artcraft_launcher_core::status::AppStatus;
use egui::{Color32, FontId, Ui};

use crate::i18n::fmt;
use crate::model::AppEntry;

/// "v0.4.0" for an app; "0.5.0 → 0.6.0" when an update is waiting. Empty when nothing is known yet.
pub(crate) fn strip_version(e: &AppEntry, status: &AppStatus) -> String {
    let installed = e.installed.as_ref().and_then(|i| i.version.as_ref().map(ToString::to_string));
    let latest = e.latest_label();
    match (status, installed.as_deref(), latest.as_deref()) {
        (AppStatus::UpdateAvailable, Some(i), Some(l)) => format!("{i} → {l}"),
        (_, Some(i), _) => format!("v{i}"),
        (_, None, Some(l)) => format!("v{l}"),
        _ => String::new(),
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
