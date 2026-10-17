// Adapted from NibrasAz7 (MIT OR Apache-2.0):
// https://github.com/storytold/craft-launcher/pull/397
//! Display order for the labels of a right-to-left interface language (Persian).
//!
//! egui shapes each font-face run with the script's own direction, so the letters of an Arabic
//! word join and run right to left, but it places the runs themselves left to right in logical
//! order: `زبان فارسی` would show its words swapped. [`display_rtl`] puts the runs in display
//! order and leaves the letters of each word alone for the shaper.

use unicode_bidi::{BidiInfo, Level, ParagraphInfo};

/// A label of a right-to-left interface language, in logical order, rearranged for egui: each
/// line reads right to left, with Latin words and numbers inside it left to right.
///
/// - `{name}` placeholders stay whole and unmirrored, so [`crate::i18n::fmt`] still fills them.
///   What it inserts is drawn left to right at that place.
/// - Spaces and separator dots at either end stay at that end, next to what the code appends.
/// - A short caption's closing colon stays on the right too, next to the widget it captions.
///
/// Text without right-to-left characters comes back unchanged. This is for painting one line;
/// a label egui wraps breaks at the wrong end.
pub(crate) fn display_rtl(text: &str) -> String {
    if !text.chars().any(is_rtl_script) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        push_rtl_line(&mut out, line);
    }
    out
}

/// `text` in display order for the paragraph direction `base`, appended to `out`.
fn push_visual(out: &mut String, text: &str, base: Level) {
    let info = BidiInfo::new(text, Some(base));
    for para in &info.paragraphs {
        push_line(out, text, &info, para, para.range.clone());
    }
}

/// The `line` of paragraph `para` (a non-empty byte range of `text`) in display order, as egui
/// needs it, appended to `out`.
fn push_line(out: &mut String, text: &str, info: &BidiInfo<'_>, para: &ParagraphInfo, line: std::ops::Range<usize>) {
    let (levels, runs) = info.visual_runs(para, line);
    // First the true display order, every character left to right as it appears on screen.
    let mut shown = String::with_capacity(text.len());
    for run in runs {
        let rtl = levels.get(run.start).is_some_and(Level::is_rtl);
        // Runs lie on character boundaries; a missing slice is skipped, never a panic.
        let Some(piece) = text.get(run) else { continue };
        let chars = piece.chars().filter(|c| !is_bidi_control(*c));
        if rtl {
            shown.extend(chars.rev().map(mirrored));
        } else {
            shown.extend(chars);
        }
    }
    push_for_shaper(out, &shown);
}

/// `shown` (in screen order) as egui must get it: egui hands each stretch of right-to-left
/// script to the shaper as one run, which lays it out right to left (and joins its letters), so
/// each such stretch goes back to logical order. What lies between them (spaces, punctuation,
/// Latin) is drawn by the Latin face left to right and stays as it is. The stretches include
/// Persian and Arabic-Indic digits: Arabic script to the shaper, though they read left to right.
fn push_for_shaper(out: &mut String, shown: &str) {
    let mut stretch: Vec<char> = Vec::new();
    for c in shown.chars() {
        if is_rtl_script(c) || is_joiner(c) {
            stretch.push(c);
        } else {
            out.extend(stretch.drain(..).rev());
            out.push(c);
        }
    }
    out.extend(stretch.drain(..).rev());
}

/// The most words a caption may have for its closing colon to stay on the right.
const CAPTION_WORDS: usize = 3;
/// First of the private-use characters that stand for placeholders while a line is reordered.
/// They are strong left-to-right, like the names they replace.
const PLACEHOLDER_BASE: u32 = 0xE000;
/// Placeholders protected per line; a line with more keeps the rest as plain text.
const MAX_PLACEHOLDERS: usize = 64;

fn push_rtl_line(out: &mut String, line: &str) {
    let edge = |c: char| c.is_whitespace() || c == '·';
    let core = line.trim_start_matches(edge);
    let lead = line.get(..line.len().saturating_sub(core.len())).unwrap_or_default();
    let trimmed = core.trim_end_matches(edge);
    let tail = core.get(trimmed.len()..).unwrap_or_default();
    let mut core = trimmed;
    let mut colon = "";
    if let Some(caption) = core.strip_suffix(':')
        && caption.split_whitespace().count() <= CAPTION_WORDS
    {
        core = caption;
        colon = ":";
    }
    // A line that already uses the stand-in characters is reordered with its placeholders as text.
    let reserved = core.chars().any(|c| (PLACEHOLDER_BASE..PLACEHOLDER_BASE.saturating_add(MAX_PLACEHOLDERS as u32)).contains(&u32::from(c)));
    let (masked, names) = if reserved { (core.to_owned(), Vec::new()) } else { mask_placeholders(core) };
    let mut visual = String::with_capacity(masked.len());
    push_visual(&mut visual, &masked, Level::rtl());
    out.push_str(lead);
    for c in visual.chars() {
        let index = u32::from(c).checked_sub(PLACEHOLDER_BASE).and_then(|i| usize::try_from(i).ok());
        match index.and_then(|i| names.get(i)) {
            Some(name) => out.push_str(name),
            None => out.push(c),
        }
    }
    out.push_str(colon);
    out.push_str(tail);
}

/// `text` with each `{name}` placeholder and each keyboard shortcut (`⇧⌘+`) replaced by one
/// stand-in character, and what they replaced, in order. A shortcut's symbols have no direction
/// of their own, so left alone they would be drawn in reverse.
fn mask_placeholders(text: &str) -> (String, Vec<&str>) {
    let mut masked = String::with_capacity(text.len());
    let mut names: Vec<&str> = Vec::new();
    let mut rest = text;
    while names.len() < MAX_PLACEHOLDERS
        && let Some((start, end)) = next_atom(rest)
    {
        let (Some(before), Some(atom), Some(after)) = (rest.get(..start), rest.get(start..end), rest.get(end..)) else { break };
        // `names.len()` is below MAX_PLACEHOLDERS, so the sum is a private-use scalar.
        let Some(stand_in) = u32::try_from(names.len()).ok().and_then(|i| char::from_u32(PLACEHOLDER_BASE.saturating_add(i))) else { break };
        masked.push_str(before);
        masked.push(stand_in);
        names.push(atom);
        rest = after;
    }
    masked.push_str(rest);
    (masked, names)
}

/// The byte range of the first placeholder or keyboard shortcut in `text`.
fn next_atom(text: &str) -> Option<(usize, usize)> {
    let placeholder = text.find('{').and_then(|open| {
        let len = text.get(open..)?.find('}')?;
        Some((open, open.saturating_add(len).saturating_add(1)))
    });
    let shortcut = text.find(is_modifier_key).map(|at| {
        // The shortcut is the stretch around the symbol up to a space, a bracket or script text.
        let outside = |c: char| c.is_whitespace() || is_rtl_script(c) || matches!(c, '(' | ')' | '[' | ']' | '{' | '}');
        let start = text
            .get(..at)
            .and_then(|before| before.rfind(outside).and_then(|i| before.get(i..)?.chars().next().map(|c| i.saturating_add(c.len_utf8()))))
            .unwrap_or(0);
        let end = text.get(at..).and_then(|after| after.find(outside)).map_or(text.len(), |i| at.saturating_add(i));
        (start, end)
    });
    match (placeholder, shortcut) {
        (Some(p), Some(s)) => Some(if s.0 < p.0 { s } else { p }),
        (p, s) => p.or(s),
    }
}

/// The modifier-key symbols of keyboard shortcuts: ⌘ ⇧ ⌥ ⌃.
fn is_modifier_key(c: char) -> bool {
    matches!(c, '\u{2318}' | '\u{21E7}' | '\u{2325}' | '\u{2303}')
}

/// Characters of the right-to-left scripts' blocks: letters, marks and the scripts' own
/// punctuation, all drawn by the script's face.
fn is_rtl_script(c: char) -> bool {
    matches!(u32::from(c), 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFC | 0x1_0800..=0x1_0FFF | 0x1_E800..=0x1_EFFF)
}

/// The zero-width (non-)joiners inside a word (Persian `می‌روم`): egui shapes them with the
/// letter before them.
fn is_joiner(c: char) -> bool {
    matches!(c, '\u{200C}' | '\u{200D}')
}

/// Direction marks, embeddings, overrides and isolates: they steer the reordering and are not drawn.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn mirrored(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        other => other,
    }
}

/// Reorder at the rendering boundary only; catalogs and persisted values stay logical.
pub(crate) fn visual(text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    if crate::i18n::current().is_rtl() { display_rtl(&isolate_numbers(text)) } else { text.to_owned() }
}
/// Content may be RTL while the application's fixed chrome remains LTR.
pub(crate) fn content_row(align: egui::Align) -> egui::Layout {
    if crate::i18n::current().is_rtl() { egui::Layout::right_to_left(align) } else { egui::Layout::left_to_right(align) }
}
pub(crate) fn content_trailing(align: egui::Align) -> egui::Layout {
    if crate::i18n::current().is_rtl() { egui::Layout::left_to_right(align) } else { egui::Layout::right_to_left(align) }
}
pub(crate) fn content_column() -> egui::Layout {
    egui::Layout::top_down(if crate::i18n::current().is_rtl() { egui::Align::Max } else { egui::Align::Min })
}

// Keep versions and day/year tokens separate from surrounding neutrals at the paint boundary.
fn isolate_numbers(text: &str) -> String {
    let mut out = String::new();
    let mut atom = String::new();
    let flush = |out: &mut String, atom: &mut String| {
        let numeric = atom.chars().any(|c| c.is_ascii_digit());
        if numeric {
            out.push('\u{2066}');
        }
        out.push_str(atom);
        if numeric {
            out.push('\u{2069}');
        }
        atom.clear();
    };
    for c in text.chars() {
        if c.is_ascii_graphic() {
            atom.push(c);
        } else {
            flush(&mut out, &mut atom);
            out.push(c);
        }
    }
    flush(&mut out, &mut atom);
    out
}

/// Wrap logical text before bidi reordering. Reordering a whole paragraph before egui wraps it
/// puts the last words on the first line. Each painted line is independent; accessibility keeps
/// the original logical paragraph. Used by the editorial details body, not fixed LTR chrome.
pub(crate) fn paragraph(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: egui::Color32) -> egui::Response {
    if !crate::i18n::current().is_rtl() {
        return ui.add(egui::Label::new(egui::RichText::new(text).font(font).color(color)).wrap());
    }
    let width = ui.available_width().max(1.0);
    let lines = wrapped_lines(ui, text, &font, color, width);
    let height: f32 = lines.iter().map(|g| g.size().y).sum();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let mut y = rect.top();
    for line in lines {
        let h = line.size().y;
        ui.painter().galley(egui::pos2(rect.right() - line.size().x, y), line, color);
        y += h;
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    response
}

fn wrapped_lines(ui: &egui::Ui, text: &str, font: &egui::FontId, color: egui::Color32, width: f32) -> Vec<std::sync::Arc<egui::Galley>> {
    let layout = |s: &str| ui.painter().layout_no_wrap(visual(s), font.clone(), color);
    logical_lines(text, width, |s| layout(s).size().x).iter().map(|line| layout(line)).collect()
}

fn logical_lines(text: &str, width: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
            if !line.is_empty() && measure(&candidate) > width {
                lines.push(std::mem::take(&mut line));
            }
            let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
            if measure(&candidate) <= width {
                line = candidate;
                continue;
            }
            // A long unbroken token must not extend into the scrollbar or outside the drawer.
            for c in word.chars() {
                let mut candidate = line.clone();
                candidate.push(c);
                if !line.is_empty() && measure(&candidate) > width {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(c);
            }
        }
        lines.push(line);
    }
    lines
}

/// A single label with logical accessibility text; use `paragraph` for prose that wraps.
pub(crate) fn label(ui: &mut egui::Ui, text: impl AsRef<str>, font: egui::FontId, color: egui::Color32) -> egui::Response {
    let text = text.as_ref();
    let response = ui.label(egui::RichText::new(visual(text)).font(font).color(color));
    accessible(response, text)
}

pub(crate) fn accessible(response: egui::Response, text: &str) -> egui::Response {
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, response.enabled(), text));
    response
}

/// Height used to keep adjacent technical fact cells equal when one value wraps.
pub(crate) fn paragraph_height(ui: &egui::Ui, text: &str, font: &egui::FontId, color: egui::Color32, width: f32) -> f32 {
    wrapped_lines(ui, text, font, color, width.max(1.0)).iter().map(|g| g.size().y).sum()
}

/// Use the same logical-first wrapping for translated tooltips as for body copy.
pub(crate) trait LocalizedTooltip {
    fn localized_hover_text(self, text: impl AsRef<str>) -> Self;
}

impl LocalizedTooltip for egui::Response {
    fn localized_hover_text(self, text: impl AsRef<str>) -> Self {
        let text = text.as_ref();
        if !crate::i18n::current().is_rtl() {
            return self.on_hover_text(text);
        }
        self.on_hover_ui(|ui| {
            ui.set_max_width(320.0);
            let font = egui::TextStyle::Body.resolve(ui.style());
            paragraph(ui, text, font, ui.visuals().text_color());
        })
    }
}

#[cfg(test)]
mod persian_tests {
    use super::*;
    #[test]
    fn wrapping_preserves_logical_reading_order() {
        let source = "لایه و ماسک با H.264 و ProRes روی دستگاه شما.";
        let lines = logical_lines(source, 12.0, |s| s.chars().count() as f32);
        assert_eq!(lines, ["لایه و ماسک", "با H.264 و", "ProRes روی", "دستگاه شما."]);
        assert_eq!(lines.join(" "), source);
        assert!(logical_lines("averylongtechnicaltoken", 5.0, |s| s.len() as f32).iter().all(|line| line.len() <= 5));
    }
    #[test]
    fn leaves_technical_tokens_unchanged() {
        for s in ["PhotoCraft", "0.4.1", "/Applications/PhotoCraft.app", "150 MB"] {
            assert_eq!(display_rtl(s), s);
        }
    }
    #[test]
    fn numeric_atoms_stay_whole() {
        assert_eq!(isolate_numbers("کدک H.264 و ProRes"), "کدک \u{2066}H.264\u{2069} و ProRes");
        assert!(display_rtl(&isolate_numbers("خروجی H.264 و ProRes")).contains("H.264"));
    }
    #[test]
    fn reorders_words_without_reversing_letters() {
        assert_eq!(display_rtl("زبان فارسی"), "فارسی زبان");
        assert!(display_rtl("نسخه PhotoCraft 0.4.1").contains("PhotoCraft 0.4.1"));
        assert!(display_rtl("به‌روزرسانی برنامه").contains("به‌روزرسانی"));
    }
}
