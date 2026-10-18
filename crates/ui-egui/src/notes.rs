//! Release notes: GitHub's generated Markdown, shown readably without a Markdown engine.

use egui::{RichText, Ui};

use crate::theme::Tokens;

/// One line of notes, classified.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Line<'a> {
    Heading(&'a str),
    /// A bullet: its text, and the pull request link GitHub appends (`… in https://…/pull/12`).
    Bullet(&'a str, Option<&'a str>),
    Text(&'a str),
    Blank,
}

pub(crate) fn classify(line: &str) -> Line<'_> {
    let l = line.trim();
    if l.is_empty() {
        return Line::Blank;
    }
    if let Some(h) = l.strip_prefix('#') {
        return Line::Heading(h.trim_start_matches('#').trim());
    }
    if let Some(b) = l.strip_prefix("* ").or_else(|| l.strip_prefix("- ")) {
        if let Some((text, url)) = b.rsplit_once(" in https://")
            && !url.contains(' ')
        {
            let start = b.len() - url.len() - "https://".len();
            return Line::Bullet(text, b.get(start..));
        }
        return Line::Bullet(b, None);
    }
    Line::Text(l)
}

pub(crate) fn show(ui: &mut Ui, notes: &str) {
    show_scaled(ui, notes, 1.0);
}

pub(crate) fn show_scaled(ui: &mut Ui, notes: &str, scale: f32) {
    ui.scope(|ui| {
        for font in ui.style_mut().text_styles.values_mut() {
            font.size *= scale;
        }
        show_lines(ui, notes, scale);
    });
}

fn show_lines(ui: &mut Ui, notes: &str, scale: f32) {
    let t = Tokens::get(ui.ctx());
    if notes.trim().is_empty() {
        ui.label(RichText::new(tl!("No notes were published with this release.")).color(t.muted));
        return;
    }
    for line in notes.lines() {
        match classify(line) {
            Line::Heading(h) => {
                ui.add_space(6.0);
                ui.label(crate::widgets::heading(h, 16.0 * scale, t.ink_strong));
            }
            Line::Bullet(text, link) => {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new("•").color(t.faint));
                    ui.label(RichText::new(text).color(t.ink));
                    if let Some(url) = link {
                        let short = url.rsplit('/').next().map_or_else(|| tl!("link").to_string(), |n| format!("#{n}"));
                        ui.hyperlink_to(RichText::new(short).size(12.0 * scale), url);
                    }
                });
            }
            Line::Text(text) => {
                if let Some(url) = text.strip_prefix("**Full Changelog**: ") {
                    ui.add_space(6.0);
                    ui.hyperlink_to(tl!("Full changelog"), url.trim());
                } else {
                    ui.label(RichText::new(text).color(t.muted));
                }
            }
            Line::Blank => {
                ui.add_space(2.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_githubs_generated_notes() {
        assert_eq!(classify("## What's Changed"), Line::Heading("What's Changed"));
        assert_eq!(
            classify("* Add the Patch tool (#213) by @a in https://github.com/storytold/photocraft/pull/381"),
            Line::Bullet("Add the Patch tool (#213) by @a", Some("https://github.com/storytold/photocraft/pull/381"))
        );
        assert_eq!(classify("- plain bullet"), Line::Bullet("plain bullet", None));
        assert_eq!(classify("   "), Line::Blank);
        assert_eq!(classify("hello"), Line::Text("hello"));
    }
}
