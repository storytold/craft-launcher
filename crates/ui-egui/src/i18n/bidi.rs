//! Bidirectional text handling (UAX #9) for RTL interface languages such as Arabic.
//!
//! Reorders mixed RTL/LTR runs and words into visual presentation order so that
//! `egui`/`epaint`'s left-to-right galley layout renders Arabic sentences naturally
//! from right to left while preserving logical character order within each word for
//! OpenType cursive shaping (HarfRust).

use std::borrow::Cow;
use unicode_bidi::{BidiInfo, Level};

/// Reorder text with mixed directionality into visual presentation order for layout and display.
pub fn reorder(text: &str) -> Cow<'_, str> {
    if !has_rtl(text) {
        return Cow::Borrowed(text);
    }

    let bidi_info = BidiInfo::new(text, Some(Level::rtl()));
    if !bidi_info.has_rtl() {
        return Cow::Borrowed(text);
    }

    let mut result = String::with_capacity(text.len());
    for para in &bidi_info.paragraphs {
        let (levels, runs) = bidi_info.visual_runs(para, para.range.clone());
        if runs.iter().all(|r| levels[r.start].is_ltr()) {
            result.push_str(&text[para.range.clone()]);
            continue;
        }

        for run in runs {
            let is_rtl = levels[run.start].is_rtl();
            let slice = &text[run.clone()];
            if is_rtl {
                reverse_tokens(slice, &mut result);
            } else {
                result.push_str(slice);
            }
        }
    }
    Cow::Owned(result)
}

/// Reverse tokens (words and whitespace blocks) within an RTL run so words appear in
/// visual reading order from right to left, while preserving character order within
/// each word for OpenType cursive shaping.
fn reverse_tokens(slice: &str, out: &mut String) {
    let mut tokens = Vec::new();
    let mut curr_start = 0;
    let mut in_whitespace = None;

    for (byte_idx, ch) in slice.char_indices() {
        let is_ws = ch.is_whitespace();
        match in_whitespace {
            None => {
                in_whitespace = Some(is_ws);
            }
            Some(ws) if ws != is_ws => {
                tokens.push(&slice[curr_start..byte_idx]);
                curr_start = byte_idx;
                in_whitespace = Some(is_ws);
            }
            _ => {}
        }
    }
    if curr_start < slice.len() {
        tokens.push(&slice[curr_start..]);
    }

    tokens.reverse();
    for token in tokens {
        out.push_str(token);
    }
}

/// Does this text contain any right-to-left characters (Arabic, Hebrew, etc.)?
pub fn has_rtl(text: &str) -> bool {
    text.chars().any(is_rtl_char)
}

/// True if `c` is a character from a Right-to-Left script.
pub fn is_rtl_char(c: char) -> bool {
    matches!(
        c,
        '\u{0590}'..='\u{05FF}' // Hebrew
        | '\u{0600}'..='\u{06FF}' // Arabic
        | '\u{0750}'..='\u{077F}' // Arabic Supplement
        | '\u{08A0}'..='\u{08FF}' // Arabic Extended-A
        | '\u{FB50}'..='\u{FDFF}' // Arabic Presentation Forms-A
        | '\u{FE70}'..='\u{FEFF}' // Arabic Presentation Forms-B
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_arabic_reorders_words_for_visual_reading() {
        assert_eq!(reorder("تعديل ومعالجة الصور"), "الصور ومعالجة تعديل");
        assert_eq!(reorder("البحث في التطبيقات"), "التطبيقات في البحث");
        assert_eq!(reorder("تسجيل وتحرير ومكساج الصوت"), "الصوت ومكساج وتحرير تسجيل");
    }

    #[test]
    fn mixed_english_and_arabic_reorders_correctly() {
        assert_eq!(reorder("يتوفر إصدار ArtCraft Launcher 0.5.1."), ".ArtCraft Launcher 0.5.1 إصدار يتوفر");
        assert_eq!(reorder("ArtCraft Launcher · 3 تحديثات متوفرة"), "متوفرة تحديثات ArtCraft Launcher · 3");
    }

    #[test]
    fn parenthesized_sort_labels_reorder_correctly() {
        assert_eq!(reorder("الاسم (أ–ي)"), "(أ–ي) الاسم");
        assert_eq!(reorder("الاسم (ي–أ)"), "(ي–أ) الاسم");
    }

    #[test]
    fn english_only_text_is_unmodified() {
        let text = "ArtCraft Launcher";
        assert_eq!(reorder(text), text);
    }
}
