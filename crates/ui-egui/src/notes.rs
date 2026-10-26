//! Release notes: GitHub's generated Markdown, shown readably without a Markdown engine.
//!
//! Only the markup a release body actually carries is understood: headings, bullets, `**bold**`,
//! `` `code` `` and `[label](url)`. Anything else stays literal, so text that merely looks like
//! markup (`2 * 3`, `snake_case_name`, an unclosed backtick) keeps every character.

use egui::{Color32, RichText, Ui};

use crate::theme::{self, Tokens};

/// One line of notes, classified.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Line<'a> {
    Heading(&'a str),
    /// A bullet: its text, and the pull request link GitHub appends (`… in https://…/pull/12`).
    Bullet(&'a str, Option<&'a str>),
    /// A markdown table row: cells are the pipe-separated fields, with the header row first.
    /// The separator row (`|---|---|`) is not a separate variant; it's identified by all cells
    /// being `---` (or empty after trim) and skipped when rendering.
    Table(Vec<&'a str>),
    Text(&'a str),
    Blank,
}

/// One piece of a line's inline markup.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Span<'a> {
    Plain(&'a str),
    /// `**bold**`.
    Bold(&'a str),
    /// `` `code` ``.
    Code(&'a str),
    /// `[label](url)`. GitHub writes a file reference as `` [`docs/x.md`](url) ``, so a label
    /// wrapped in backticks keeps its text without them.
    Link {
        label: &'a str,
        url: &'a str,
    },
}

pub(crate) fn classify(line: &str) -> Line<'_> {
    let l = line.trim();
    if l.is_empty() {
        return Line::Blank;
    }
    if let Some(h) = l.strip_prefix('#') {
        return Line::Heading(h.trim_start_matches('#').trim());
    }
    // Table row: starts and ends with '|', has at least two pipes (header or data).
    if l.starts_with('|') && l.ends_with('|') && l.matches('|').count() >= 3 {
        // Split by '|', drop first and last (empty), trim each cell.
        let cells: Vec<&str> = l.split('|').skip(1).map(|c| c.trim()).collect();
        // Trailing empty cell from the final '|' (e.g. "|a|b|" -> ["", "a", "b", ""]).
        let cells = cells.into_iter().filter(|c| !c.is_empty()).collect::<Vec<_>>();
        // If all cells are just dashes (separator row), treat as Text so it's drawn but we can
        // skip it in draw. The caller will still get a Table variant for the header/data rows.
        return Line::Table(cells);
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

/// A line's inline pieces, in order. Empty for an empty line.
pub(crate) fn spans(text: &str) -> Vec<Span<'_>> {
    let mut out: Vec<Span<'_>> = Vec::new();
    let mut plain_from = 0;
    let mut i = 0;
    while let Some(rest) = text.get(i..) {
        if rest.is_empty() {
            break;
        }
        match piece(rest) {
            Some((span, len)) => {
                if i > plain_from
                    && let Some(plain) = text.get(plain_from..i)
                {
                    out.push(Span::Plain(plain));
                }
                out.push(span);
                i += len;
                plain_from = i;
            }
            // No markup here: step over one character (never a byte, so `i` stays on a boundary)
            // and let the plain run keep growing.
            None => i += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    if let Some(plain) = text.get(plain_from..)
        && !plain.is_empty()
    {
        out.push(Span::Plain(plain));
    }
    out
}

/// The line's text with the markup markers dropped: what a heading, which draws no emphasis,
/// should show.
pub(crate) fn without_markup(text: &str) -> String {
    spans(text)
        .iter()
        .map(|span| match span {
            Span::Plain(s) | Span::Bold(s) | Span::Code(s) => *s,
            Span::Link { label, .. } => *label,
        })
        .collect()
}

/// Does this line carry markup that a plain label would show as typed?
fn marked_up(text: &str) -> bool {
    !matches!(spans(text).as_slice(), [] | [Span::Plain(_)])
}

/// The longest label and address a link may carry. GitHub's are far shorter, and a bounded search
/// is what keeps this linear: `spans` retries after every unmatched `[`, so an unbounded search
/// here would scan the rest of the line once per bracket.
const MAX_LABEL: usize = 128;
const MAX_URL: usize = 512;

/// The markup at the start of `rest`: the piece and how many bytes it takes.
fn piece(rest: &str) -> Option<(Span<'_>, usize)> {
    if let Some(inner) = rest.strip_prefix('`') {
        let end = inner.find('`')?;
        let text = inner.get(..end)?;
        // Empty or space-padded runs are not code: a lone pair of backticks stays as written.
        if text.is_empty() || padded(text) {
            return None;
        }
        return Some((Span::Code(text), end + 2));
    }
    if let Some(inner) = rest.strip_prefix("**") {
        let end = inner.find("**")?;
        let text = inner.get(..end)?;
        // CommonMark refuses emphasis with a space just inside the markers, and nested emphasis
        // (`***x***`) stays literal rather than coming out half-rendered.
        if text.is_empty() || text.contains('*') || padded(text) {
            return None;
        }
        return Some((Span::Bold(text), end + 4));
    }
    if let Some(inner) = rest.strip_prefix('[') {
        // Both searches run over a bounded window, never the rest of the line: `spans` retries
        // after every unmatched `[`, so an unbounded search would cost a scan per bracket.
        // Snap the bound to a UTF-8 character boundary: `MAX_LABEL` is a byte count, and slicing
        // at a byte inside a multi-byte character returns `None`, dropping a link whose label
        // crosses the cap. `char_indices` lands on the first boundary strictly past the cap, so a
        // character that starts before it and ends after it is still inside the window.
        let label_cap = inner.char_indices().find(|(i, _)| *i > MAX_LABEL).map_or(inner.len(), |(i, _)| i);
        let label_end = inner.get(..label_cap)?.find("](")?;
        let label = inner.get(..label_end)?;
        let after = inner.get(label_end + 2..)?;
        // Snap the bound to a UTF-8 character boundary: `MAX_URL` is a byte count, and slicing
        // at a byte inside a multi-byte character returns `None`, dropping a link whose URL
        // crosses the cap. `char_indices` lands on the first boundary strictly past the cap, so a
        // character that starts before it and ends after it is still inside the window.
        let url_cap = after.char_indices().find(|(i, _)| *i > MAX_URL).map_or(after.len(), |(i, _)| i);
        // Find the matching closing paren by counting nested parentheses.
        // Wikipedia links like `_(disambiguation)` have parens inside; we need the *matching* one.
        let url_end = after.get(..url_cap).and_then(|window| {
            let mut depth = 0;
            window.char_indices().find_map(|(i, ch)| match ch {
                '(' => {
                    depth += 1;
                    None
                }
                ')' => {
                    if depth == 0 {
                        Some(i)
                    } else {
                        depth -= 1;
                        None
                    }
                }
                _ => None,
            })
        })?;
        let url = after.get(..url_end)?;
        // A URL is one word: `[text](url "title")` and `[text][ref]` are not links.
        if label.is_empty() || url.is_empty() || url.contains(char::is_whitespace) {
            return None;
        }
        let label = label.strip_prefix('`').and_then(|l| l.strip_suffix('`')).unwrap_or(label);
        return Some((Span::Link { label, url }, label_end + url_end + 4));
    }
    None
}

/// A space just inside a marker pair means it is not markup.
fn padded(text: &str) -> bool {
    text.starts_with(char::is_whitespace) || text.ends_with(char::is_whitespace)
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
/// Only bullets that appear after a "What's Changed" (or similar) heading are counted.
/// A short list has SHOWN + 1 or fewer relevant bullets; a long list folds at the (SHOWN + 1)th.
fn fold_at(lines: &[Line<'_>]) -> Option<usize> {
    // Find the index of the first "What's Changed" heading (case-insensitive, any # level).
    let whats_changed_idx = lines.iter().position(|l| matches!(l, Line::Heading(h) if h.eq_ignore_ascii_case("What's Changed")));
    let start = whats_changed_idx.map_or(0, |i| i + 1);
    let bullets = lines[start..].iter().filter(|l| matches!(l, Line::Bullet(..))).count();
    if bullets <= SHOWN + 1 {
        return None;
    }
    lines[start..].iter().enumerate().filter(|(_, l)| matches!(l, Line::Bullet(..))).nth(SHOWN).map(|(i, _)| start + i)
}

/// The address on a "**Full Changelog**: …" line, when it is a safe web link.
fn full_changelog<'a>(line: &Line<'a>) -> Option<&'a str> {
    let Line::Text(text) = line else { return None };
    text.strip_prefix("**Full Changelog**: ").map(str::trim).filter(|url| artcraft_launcher_core::release::safe_web_link(url))
}

fn draw(ui: &mut Ui, line: &Line<'_>, t: &Tokens, scale: f32) {
    match line {
        Line::Heading(h) => {
            ui.add_space(6.0);
            if marked_up(h) {
                ui.label(crate::widgets::heading(&without_markup(h), 16.0 * scale, t.ink_strong));
            } else {
                ui.label(crate::widgets::heading(h, 16.0 * scale, t.ink_strong));
            }
        }
        Line::Bullet(text, link) => {
            // The author's handle is noise in a list of what changed.
            let text = text.rsplit_once(" by @").map_or(*text, |(change, _)| change);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(RichText::new("•").color(t.faint));
                ui.add_space(4.0);
                inline(ui, text, t.ink);
                if let Some(url) = link.filter(|u| artcraft_launcher_core::release::safe_web_link(u)) {
                    let short = url.rsplit('/').next().map_or_else(|| tl!("link").to_string(), |n| format!("#{n}"));
                    ui.add_space(4.0);
                    ui.hyperlink_to(RichText::new(short).size(12.0 * scale), url);
                }
            });
        }
        Line::Table(cells) => {
            // Skip the separator row (all cells are dashes or empty after trim).
            if cells.iter().all(|c| c.trim().chars().all(|ch| ch == '-') || c.trim().is_empty()) {
                return;
            }
            ui.add_space(4.0);
            egui::Grid::new(ui.id().with("notes-table")).striped(true).spacing([8.0 * scale, 2.0 * scale]).show(ui, |ui| {
                for (i, cell) in cells.iter().enumerate() {
                    let text = if marked_up(cell) { without_markup(cell) } else { (*cell).to_string() };
                    let color = if i == 0 { t.ink } else { t.muted };
                    ui.label(RichText::new(text).color(color).size(12.0 * scale));
                }
                ui.end_row();
            });
        }
        Line::Text(text) => {
            if marked_up(text) {
                ui.horizontal_wrapped(|ui| inline(ui, text, t.muted));
            } else {
                // Nothing to render: one label, exactly as before.
                ui.label(RichText::new(*text).color(t.muted));
            }
        }
        Line::Blank => {
            ui.add_space(2.0);
        }
    }
}

/// One line's inline pieces: bold in Inter 600, code in Geist Mono, and a link as a hyperlink
/// carrying its own label, so the URL itself never has to be read (or wrapped) in the note.
///
/// The pieces are laid out flush: the runs already carry the spaces the note wrote, and egui's
/// item spacing between them would push a trailing period away from the number it follows.
fn inline(ui: &mut Ui, text: &str, color: Color32) {
    ui.spacing_mut().item_spacing.x = 0.0;
    for span in spans(text) {
        match span {
            Span::Plain(s) => ui.label(RichText::new(s).color(color)),
            Span::Bold(s) => ui.label(RichText::new(s).family(theme::semibold()).color(color)),
            Span::Code(s) => ui.label(RichText::new(s).family(theme::mono()).color(color)),
            Span::Link { label, url } => match clickable(url) {
                Some(url) => ui.hyperlink_to(RichText::new(label), url),
                None => ui.label(RichText::new(label).color(color)),
            },
        };
    }
}

/// The address to open for a link, when it is one: a release body is hostile input, so it gets the
/// same check as the pull-request link beside it, and anything else is drawn as plain text.
fn clickable(url: &str) -> Option<&str> {
    artcraft_launcher_core::release::safe_web_link(url).then_some(url)
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
        // Table rows
        assert_eq!(classify("| Tool | Score |"), Line::Table(vec!["Tool", "Score"]));
        assert_eq!(classify("|---|---|"), Line::Table(vec!["---", "---"]));
        assert_eq!(classify("| a | 1 |"), Line::Table(vec!["a", "1"]));
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
    fn fold_at_only_counts_bullets_after_whats_changed() {
        // Bullets before "What's Changed" don't count toward SHOWN.
        // SHOWN = 6, short list threshold = SHOWN + 1 = 7 bullets.
        // 7 bullets after heading = short list, no fold.
        let notes: Vec<Line> = vec![
            Line::Bullet("pre-change 1", None),
            Line::Bullet("pre-change 2", None),
            Line::Bullet("pre-change 3", None),
            Line::Bullet("pre-change 4", None),
            Line::Bullet("pre-change 5", None),
            Line::Heading("What's Changed"),
            Line::Bullet("change 1", None),
            Line::Bullet("change 2", None),
            Line::Bullet("change 3", None),
            Line::Bullet("change 4", None),
            Line::Bullet("change 5", None),
            Line::Bullet("change 6", None),
            Line::Bullet("change 7", None),
        ];
        // 7 bullets after heading = short list (≤ SHOWN+1), no fold
        assert_eq!(fold_at(&notes), None);
        // 8 bullets after heading = long list, fold at 7th (index 13 = 5 pre + 1 heading + 7)
        let long: Vec<Line> = vec![
            Line::Bullet("pre-change 1", None),
            Line::Bullet("pre-change 2", None),
            Line::Bullet("pre-change 3", None),
            Line::Bullet("pre-change 4", None),
            Line::Bullet("pre-change 5", None),
            Line::Heading("What's Changed"),
            Line::Bullet("change 1", None),
            Line::Bullet("change 2", None),
            Line::Bullet("change 3", None),
            Line::Bullet("change 4", None),
            Line::Bullet("change 5", None),
            Line::Bullet("change 6", None),
            Line::Bullet("change 7", None),
            Line::Bullet("change 8", None),
        ];
        assert_eq!(fold_at(&long), Some(12));
        // Only 6 bullets after heading = no fold
        let few: Vec<Line> = vec![
            Line::Heading("What's Changed"),
            Line::Bullet("c1", None),
            Line::Bullet("c2", None),
            Line::Bullet("c3", None),
            Line::Bullet("c4", None),
            Line::Bullet("c5", None),
            Line::Bullet("c6", None),
        ];
        assert_eq!(fold_at(&few), None);
        // No heading = counts from start (old behavior)
        let no_heading: Vec<Line> = (0..8).map(|_| Line::Bullet("c", None)).collect();
        // 8 bullets, no heading: nth(6) gives 7th bullet at index 6
        assert_eq!(fold_at(&no_heading), Some(6));
    }

    #[test]
    fn reads_bold_code_and_links() {
        // A file reference, as GitHub writes one: the label is code and the URL is a link.
        assert_eq!(
            spans("From [`docs/scorecard.md`](https://github.com/storytold/photocraft/blob/v0.10.0/docs/scorecard.md) (done):"),
            vec![
                Span::Plain("From "),
                Span::Link { label: "docs/scorecard.md", url: "https://github.com/storytold/photocraft/blob/v0.10.0/docs/scorecard.md" },
                Span::Plain(" (done):"),
            ]
        );
        // Two runs of bold on one line, the markers gone.
        assert_eq!(
            spans("**Tools:** 29 / 13 / 1 → **33 / 11 / 0**."),
            vec![Span::Bold("Tools:"), Span::Plain(" 29 / 13 / 1 → "), Span::Bold("33 / 11 / 0"), Span::Plain(".")]
        );
        assert_eq!(spans("run `cargo test` first"), vec![Span::Plain("run "), Span::Code("cargo test"), Span::Plain(" first")]);
        assert_eq!(spans("[notes](https://example.com/n)"), vec![Span::Link { label: "notes", url: "https://example.com/n" }]);
        assert_eq!(spans("**whole line**"), vec![Span::Bold("whole line")]);
        // A heading keeps its text and loses its markers.
        assert_eq!(without_markup("**Backend** and [docs](https://example.com/d)"), "Backend and docs");
        assert_eq!(without_markup("plain heading"), "plain heading");
    }

    #[test]
    fn leaves_text_that_only_looks_like_markup_alone() {
        for text in [
            "29 * 3 and 7 * 4",
            "snake_case_name",
            "a `b starts no code",
            "** left open",
            "** padded **",
            "see https://example.com/a(b)",
            "[]()",
            "[text][ref]",
            "[text](url \"title\")",
        ] {
            assert_eq!(spans(text), vec![Span::Plain(text)], "{text}");
        }
        assert!(spans("").is_empty());
        // Nested emphasis is the one thing left half-rendered: the inner run is taken and the
        // outer asterisks stay visible. No character is dropped.
        assert_eq!(spans("***nested***"), vec![Span::Plain("*"), Span::Bold("nested"), Span::Plain("*")]);
    }

    #[test]
    fn only_a_safe_web_address_is_clickable() {
        assert_eq!(clickable("https://example.com/scorecard.md"), Some("https://example.com/scorecard.md"));
        assert_eq!(clickable("http://plain.example/x"), None);
        assert_eq!(clickable("file:///etc/passwd"), None);
        assert_eq!(clickable("https://user@evil.example/x"), None);
        assert_eq!(clickable("javascript:alert(1)"), None);
    }

    #[test]
    fn the_dialog_draws_the_text_not_the_markup() {
        // `i18n::tests`' idiom: a bare context, the app's fonts installed before the first pass.
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx, None);
        let notes = "From [`docs/scorecard.md`](https://example.com/scorecard.md) (done):\n* **Tools:** 29 / 13 / 1";
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| show(ui, notes));
        output.textures_delta.clear();
        let drawn = drawn_text(output.shapes);

        // The link is drawn with its label and the bold run with its text, each as a run of its
        // own: the markers themselves are what these last two assertions refuse.
        assert!(drawn.iter().any(|s| s.as_str() == "docs/scorecard.md"), "the link's label is not drawn: {drawn:?}");
        assert!(drawn.iter().any(|s| s.as_str() == "Tools:"), "the bold run is not drawn: {drawn:?}");
        assert!(!drawn.iter().any(|s| s.contains("**")), "markdown markers reached the dialog: {drawn:?}");
        assert!(!drawn.iter().any(|s| s.contains("](http")), "the link's markup reached the dialog: {drawn:?}");
    }

    #[test]
    fn a_line_full_of_brackets_is_not_a_scan_per_bracket() {
        // Every unmatched `[` is retried, so the link searches stay inside a window. Searching the
        // rest of the line instead cost 57 ms at 16 k brackets and about a second at the 64 KiB
        // `release::MAX_NOTES` allows — twice per frame, because a line is parsed twice.
        let hostile = "[".repeat(64 * 1024);
        let started = std::time::Instant::now();
        assert_eq!(spans(&hostile), vec![Span::Plain(&hostile)]);
        assert!(started.elapsed() < std::time::Duration::from_millis(250), "the parse took {:?}", started.elapsed());
    }

    #[test]
    fn a_link_has_to_fit_inside_the_window() {
        let long_label = format!("[{}](https://example.com/x)", "a".repeat(MAX_LABEL));
        assert_eq!(spans(&long_label), vec![Span::Plain(&long_label)]);
        let long_url = format!("[label](https://example.com/{})", "a".repeat(MAX_URL));
        assert_eq!(spans(&long_url), vec![Span::Plain(&long_url)]);
        // Just inside the window the same shape is still read as a link.
        let just_fits = format!("[{}](https://example.com/x)", "a".repeat(MAX_LABEL - 2));
        assert!(matches!(spans(&just_fits).as_slice(), [Span::Link { .. }]), "{:?}", spans(&just_fits));
    }

    #[test]
    fn a_link_whose_label_crosses_the_byte_cap_is_still_read() {
        // `MAX_LABEL` is a byte count. If the cap byte falls inside a multi-byte character
        // in the URL that immediately follows the label, the old slice returned `None` and
        // dropped the link. The cap now snaps to the first character boundary strictly past
        // the cap, so a link whose label fits and whose URL starts with a multi-byte char
        // is still parsed.
        // Label = 123 'a' + 'é' = 125 bytes; `](` at 125,126; URL starts at 127 with 'é'
        // (2 bytes: 127,128). Byte 128 is a continuation → old `get(..128)` returned `None`.
        let label = format!("{}é", "a".repeat(123));
        let line = format!("[{label}](éx)");
        assert!(matches!(spans(&line).as_slice(), [Span::Link { .. }]), "{:?}", spans(&line));
    }

    #[test]
    fn the_runs_of_a_line_are_drawn_flush() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx, None);
        let notes = "From [`docs/scorecard.md`](https://example.com/scorecard.md) (done):\n* **Tools:** 29 / 13 / 1";
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| show(ui, notes));
        output.textures_delta.clear();
        let runs = drawn_runs(output.shapes);
        let starts = |text: &str| runs.iter().find(|run| run.text == text).map_or(f32::NAN, |run| run.starts);
        let ends = |text: &str| runs.iter().find(|run| run.text == text).map_or(f32::NAN, |run| run.ends);

        // A run starts where the one before it ended: the note's own spaces are the only ones drawn,
        // and a period stays on the number it follows instead of floating a gap away.
        assert!((starts("docs/scorecard.md") - ends("From ")).abs() < 0.5, "the link is not flush: {runs:?}");
        assert!((starts(" (done):") - ends("docs/scorecard.md")).abs() < 0.5, "the tail is not flush: {runs:?}");
        assert!((starts(" 29 / 13 / 1") - ends("Tools:")).abs() < 0.5, "the bullet's runs are not flush: {runs:?}");
        // The bullet's own gap survives: four points of air between the dot and the text.
        assert!((starts("Tools:") - ends("•") - 4.0).abs() < 0.5, "the bullet lost its gap: {runs:?}");
    }

    /// One run a frame drew: its text and the range it covers along its row.
    #[derive(Debug)]
    struct Run {
        text: String,
        starts: f32,
        ends: f32,
    }

    /// Every run a frame drew, with where it landed: what the user reads, and how far apart.
    fn drawn_runs(shapes: Vec<egui::epaint::ClippedShape>) -> Vec<Run> {
        fn walk(shape: &egui::Shape, out: &mut Vec<Run>) {
            match shape {
                egui::Shape::Text(text) => {
                    // A run's own offset lives in its section's leading space; `pos` is the row's.
                    let indent = text.galley.job.sections.first().map_or(0.0, |section| section.leading_space);
                    out.push(Run { text: text.galley.text().to_owned(), starts: text.pos.x + indent, ends: text.pos.x + text.galley.rect.max.x });
                }
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    /// Everything a frame drew, as egui laid it out: what the user reads.
    fn drawn_text(shapes: Vec<egui::epaint::ClippedShape>) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }
}
