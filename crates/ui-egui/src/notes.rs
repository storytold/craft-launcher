//! Release notes: GitHub's generated Markdown, shown readably without a Markdown engine.

use egui::{Color32, RichText, Ui};

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

/// How many changes show before the rest folds away.
const SHOWN: usize = 6;

pub(crate) fn show(ui: &mut Ui, notes: &str) {
    show_scaled(ui, notes, 1.0);
}

/// The notes at `scale` times their usual size (the notes dialog's own zoom).
pub(crate) fn show_scaled(ui: &mut Ui, notes: &str, scale: f32) {
    ui.scope(|ui| {
        for font in ui.style_mut().text_styles.values_mut() {
            font.size *= scale;
        }
        show_lines(ui, notes, scale);
    });
}

/// The notes, short: the first few changes in full, the rest behind "Show N more changes", and the
/// link to the full changelog always in view.
fn show_lines(ui: &mut Ui, notes: &str, scale: f32) {
    let t = Tokens::get(ui.ctx());
    if notes.trim().is_empty() {
        ui.label(RichText::new(tl!("No notes were published with this release.")).color(t.muted));
        return;
    }
    let (changelog, lines): (Vec<Line<'_>>, Vec<Line<'_>>) = notes.lines().map(classify).partition(|l| full_changelog(l).is_some());
    let bullets = lines.iter().filter(|l| matches!(l, Line::Bullet(..))).count();
    let (head, rest) = lines.split_at(fold_at(&lines).unwrap_or(lines.len()));
    for line in head {
        draw(ui, line, &t, scale);
    }
    if !rest.is_empty() {
        let label = crate::i18n::fmt(tl!("Show {n} more changes"), &[("n", &bullets.saturating_sub(SHOWN).to_string())]);
        egui::CollapsingHeader::new(label).id_salt(ui.scope_id().with(("more-notes", notes.len()))).show(ui, |ui| {
            for line in rest {
                draw(ui, line, &t, scale);
            }
        });
    }
    for line in &changelog {
        if let Some(url) = full_changelog(line) {
            ui.add_space(6.0);
            ui.hyperlink_to(tl!("Full changelog"), url);
        }
    }
}

/// Where a long list folds: just before its (SHOWN + 1)th change. A short list is shown whole.
fn fold_at(lines: &[Line<'_>]) -> Option<usize> {
    let bullets = lines.iter().filter(|l| matches!(l, Line::Bullet(..))).count();
    if bullets <= SHOWN + 1 {
        return None;
    }
    lines.iter().enumerate().filter(|(_, l)| matches!(l, Line::Bullet(..))).nth(SHOWN).map(|(i, _)| i)
}

/// The address on a "**Full Changelog**: …" line, when it is a safe web link.
fn full_changelog<'a>(line: &Line<'a>) -> Option<&'a str> {
    let Line::Text(text) = line else { return None };
    text.strip_prefix("**Full Changelog**: ").map(str::trim).filter(|url| artcraft_launcher_core::release::safe_web_link(url))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Inline<'a> {
    Plain(&'a str),
    Bold(&'a str),
    Code(&'a str),
    Link { text: &'a str, url: &'a str },
}

pub(crate) fn parse_inline(orig: &str) -> Vec<Inline<'_>> {
    let mut out = Vec::new();
    let mut cursor = 0;
    let mut plain_start = 0;
    let mut look_for_bold = true;
    let mut look_for_code = true;
    let mut look_for_brackets = true;

    while cursor < orig.len() {
        let Some(rest) = orig.get(cursor..) else { break };

        let next_bold = if look_for_bold { rest.find("**").map(|i| (cursor + i, 0)) } else { None };
        let next_code = if look_for_code { rest.find('`').map(|i| (cursor + i, 1)) } else { None };
        let next_bracket = if look_for_brackets { rest.find('[').map(|i| (cursor + i, 2)) } else { None };

        let Some((special_idx, kind)) = [next_bold, next_code, next_bracket].into_iter().flatten().min_by_key(|&(idx, _)| idx) else {
            break;
        };

        match kind {
            0 => {
                let Some(after_stars) = orig.get(special_idx + 2..) else { break };
                if let Some(end) = after_stars.find("**")
                    && let Some(bold_text) = after_stars.get(..end)
                {
                    let end_pos = special_idx + 2 + end + 2;
                    if plain_start < special_idx
                        && let Some(plain) = orig.get(plain_start..special_idx)
                    {
                        out.push(Inline::Plain(plain));
                    }
                    out.push(Inline::Bold(bold_text));
                    plain_start = end_pos;
                    cursor = end_pos;
                } else {
                    look_for_bold = false;
                    cursor = special_idx + 2;
                }
            }
            1 => {
                let Some(after_tick) = orig.get(special_idx + 1..) else { break };
                if let Some(end) = after_tick.find('`')
                    && let Some(code_text) = after_tick.get(..end)
                {
                    let end_pos = special_idx + 1 + end + 1;
                    if plain_start < special_idx
                        && let Some(plain) = orig.get(plain_start..special_idx)
                    {
                        out.push(Inline::Plain(plain));
                    }
                    out.push(Inline::Code(code_text));
                    plain_start = end_pos;
                    cursor = end_pos;
                } else {
                    look_for_code = false;
                    cursor = special_idx + 1;
                }
            }
            2 => {
                let Some(after_bracket) = orig.get(special_idx + 1..) else { break };
                if let Some(close_bracket) = after_bracket.find(']')
                    && let Some(link_text_candidate) = after_bracket.get(..close_bracket)
                    && let Some(after_close) = after_bracket.get(close_bracket + 1..)
                {
                    let (actual_start, link_text) = if let Some(inner_open) = link_text_candidate.rfind('[')
                        && let Some(inner_text) = link_text_candidate.get(inner_open + 1..)
                    {
                        (special_idx + 1 + inner_open, inner_text)
                    } else {
                        (special_idx, link_text_candidate)
                    };

                    if let Some(after_paren) = after_close.strip_prefix('(')
                        && let Some(close_paren) = after_paren.find(')')
                        && let Some(url) = after_paren.get(..close_paren)
                    {
                        let end_pos = special_idx + 1 + close_bracket + 1 + 1 + close_paren + 1;
                        if plain_start < actual_start
                            && let Some(plain) = orig.get(plain_start..actual_start)
                        {
                            out.push(Inline::Plain(plain));
                        }
                        out.push(Inline::Link { text: link_text, url });
                        plain_start = end_pos;
                        cursor = end_pos;
                    } else {
                        cursor = special_idx + 1 + close_bracket + 1;
                    }
                } else {
                    look_for_brackets = false;
                    cursor = special_idx + 1;
                }
            }
            _ => break,
        }
    }

    if plain_start < orig.len()
        && let Some(remaining) = orig.get(plain_start..)
    {
        out.push(Inline::Plain(remaining));
    }

    out
}

fn render_inline(ui: &mut Ui, text: &str, color: Color32, t: &Tokens, scale: f32) {
    let space_w = (ui.spacing().item_spacing.x * scale).max(2.0);
    ui.spacing_mut().item_spacing.x = 0.0;
    let mut needs_space = text.starts_with(char::is_whitespace);

    let tokens = parse_inline(text);
    for token in tokens {
        match token {
            Inline::Plain(plain) => {
                if plain.starts_with(char::is_whitespace) {
                    needs_space = true;
                }
                let mut words = plain.split_whitespace().peekable();
                while let Some(word) = words.next() {
                    if needs_space {
                        ui.add_space(space_w);
                    }
                    ui.label(RichText::new(word).size(13.5 * scale).color(color));
                    needs_space = if words.peek().is_some() { true } else { plain.ends_with(char::is_whitespace) };
                }
            }
            Inline::Bold(bold) => {
                if bold.starts_with(char::is_whitespace) {
                    needs_space = true;
                }
                let mut words = bold.split_whitespace().peekable();
                while let Some(word) = words.next() {
                    if needs_space {
                        ui.add_space(space_w);
                    }
                    ui.label(RichText::new(word).strong().size(13.5 * scale).color(t.ink_strong));
                    needs_space = if words.peek().is_some() { true } else { bold.ends_with(char::is_whitespace) };
                }
            }
            Inline::Code(code) => {
                if needs_space || code.starts_with(char::is_whitespace) {
                    ui.add_space(space_w);
                }
                ui.label(RichText::new(code).monospace().size(12.0 * scale).color(t.ink));
                needs_space = code.ends_with(char::is_whitespace);
            }
            Inline::Link { text, url } => {
                if needs_space || text.starts_with(char::is_whitespace) {
                    ui.add_space(space_w);
                }
                if artcraft_launcher_core::release::safe_web_link(url) {
                    ui.hyperlink_to(RichText::new(text).size(13.5 * scale), url);
                } else {
                    ui.label(RichText::new(text).size(13.5 * scale).color(color));
                }
                needs_space = text.ends_with(char::is_whitespace);
            }
        }
    }
}

fn draw(ui: &mut Ui, line: &Line<'_>, t: &Tokens, scale: f32) {
    match line {
        Line::Heading(h) => {
            ui.add_space(6.0);
            ui.label(crate::widgets::heading(h, 16.0 * scale, t.ink_strong));
        }
        Line::Bullet(text, link) => {
            // The author's handle is noise in a list of what changed.
            let text = text.rsplit_once(" by @").map_or(*text, |(change, _)| change);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new("•").color(t.faint));
                render_inline(ui, text, t.ink, t, scale);
                if let Some(url) = link.filter(|u| artcraft_launcher_core::release::safe_web_link(u)) {
                    let short = url.rsplit('/').next().map_or_else(|| tl!("link").to_string(), |n| format!("#{n}"));
                    ui.add_space(4.0 * scale);
                    ui.hyperlink_to(RichText::new(short).size(12.0 * scale), url);
                }
            });
        }
        Line::Text(text) => {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                render_inline(ui, text, t.muted, t, scale);
            });
        }
        Line::Blank => {
            ui.add_space(2.0);
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

    #[test]
    fn a_long_list_folds_after_six_changes_and_a_short_one_does_not() {
        let many: Vec<Line> = std::iter::once(Line::Heading("What's Changed")).chain((0..10).map(|_| Line::Bullet("change", None))).collect();
        // The heading, then six bullets: the seventh bullet (index 7) is the first one folded.
        assert_eq!(fold_at(&many), Some(7));
        let few: Vec<Line> = (0..7).map(|_| Line::Bullet("change", None)).collect();
        assert_eq!(fold_at(&few), None);
        assert_eq!(fold_at(&[]), None);
    }

    #[test]
    fn parses_inline_markdown() {
        assert_eq!(
            parse_inline("Hello **world** and `code` with [site](https://getartcraft.com)."),
            vec![
                Inline::Plain("Hello "),
                Inline::Bold("world"),
                Inline::Plain(" and "),
                Inline::Code("code"),
                Inline::Plain(" with "),
                Inline::Link { text: "site", url: "https://getartcraft.com" },
                Inline::Plain("."),
            ]
        );
        assert_eq!(parse_inline("**unclosed"), vec![Inline::Plain("**unclosed")]);
        assert_eq!(parse_inline("`unclosed"), vec![Inline::Plain("`unclosed")]);
        assert_eq!(parse_inline("[unclosed](https://"), vec![Inline::Plain("[unclosed](https://")]);
        assert_eq!(parse_inline("[[[[[unclosed"), vec![Inline::Plain("[[[[[unclosed")]);
        assert_eq!(
            parse_inline("[unmatched [site](https://getartcraft.com)"),
            vec![Inline::Plain("[unmatched "), Inline::Link { text: "site", url: "https://getartcraft.com" },]
        );
        assert_eq!(
            parse_inline("[tag] and [site](https://getartcraft.com)"),
            vec![Inline::Plain("[tag] and "), Inline::Link { text: "site", url: "https://getartcraft.com" },]
        );
    }
}
