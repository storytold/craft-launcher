//! Text that is cut with an ellipsis rather than wrapped, so a row never grows.

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, FontId, Galley, Ui};

/// `text` on one line, at most `width` wide, ending in an ellipsis when it does not fit.
pub(crate) fn one_line(ui: &Ui, text: &str, font: FontId, colour: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, colour);
    job.wrap = TextWrapping::truncate_at_width(width);
    ui.painter().layout_job(job)
}
