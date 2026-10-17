//! Looking strings up in the language the UI is drawn in.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::lang::{LANGUAGES, Lang};

/// Index into [`LANGUAGES`] of the language the UI is drawn in this frame.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Set the UI language for drawing (the shell calls this once per frame from the preference), so
/// widgets can translate without every call site carrying a language around.
pub fn set_current(lang: Lang) {
    CURRENT.store(lang.index(), Ordering::Relaxed);
}

/// The language the UI is drawn in.
pub fn current() -> Lang {
    LANGUAGES.get(CURRENT.load(Ordering::Relaxed)).map_or(Lang::EN, Lang)
}

/// Does `lang` have a catalog entry for this plain string? (English never does: it is the source.)
pub fn has(lang: Lang, s: &str) -> bool {
    lang.catalog().plain(s).is_some()
}

/// Translate an English UI string into the current language ([`tr`] with [`current`]).
pub fn t(s: &str) -> &str {
    tr(current(), s)
}

/// Translate an English UI string; unknown strings come back unchanged.
pub fn tr(lang: Lang, s: &str) -> &str {
    lang.catalog().plain(s).unwrap_or(s)
}

/// Like [`tr`], for an English string that needs a disambiguating `context`.
pub fn tr_ctx<'a>(lang: Lang, context: &str, s: &'a str) -> &'a str {
    lang.catalog().contextual(context, s).unwrap_or_else(|| tr(lang, s))
}

/// A string keyed by its command id, falling back to the translation of the English `label`.
pub fn tr_id<'a>(lang: Lang, id: &str, label: &'a str) -> &'a str {
    lang.catalog().id(id).unwrap_or_else(|| tr(lang, label))
}

/// [`tr_id`] in the current language.
pub fn t_id<'a>(id: &str, label: &'a str) -> &'a str {
    tr_id(current(), id, label)
}

/// Words that stay in English in every catalog besides the `…Craft` names, and so keep English
/// upper case in a language that cases `i` differently.
const ENGLISH_WORDS: [&str; 7] = ["Suite", "GitHub", "Discord", "Finder", "Windows", "Linux", "FreeBSD"];

/// `s` in upper case for `lang`. Turkish dots its capital i (`indir` → `İNDİR`; `ı` → `I`), but
/// not in the names that stay in English (`GitHub` → `GITHUB`, `DesignCraft` → `DESIGNCRAFT`).
pub fn upper(lang: Lang, s: &str) -> String {
    if lang != Lang::TR {
        return s.to_uppercase();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        // Alternate runs of letters and digits (a word) and everything else.
        let in_word = rest.starts_with(char::is_alphanumeric);
        let end = rest.find(|c: char| c.is_alphanumeric() != in_word).unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        if !in_word || run.ends_with("Craft") || ENGLISH_WORDS.contains(&run) {
            out.push_str(&run.to_uppercase());
        } else {
            out.extend(run.chars().flat_map(|c| if c == 'i' { 'İ'.to_uppercase() } else { c.to_uppercase() }));
        }
        rest = tail;
    }
    out
}

/// Fill `{name}` placeholders. Unknown placeholders are left as written.
pub fn fmt(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// A plural-aware string: `one`/`other` are the English forms (with `{n}` where the count goes),
/// and together (`one|other`) the key of the language's `@plural` row, which holds as many forms
/// as the language has. Without a row, English: `one` for 1, `other` for everything else.
pub fn trn(lang: Lang, n: u64, one: &str, other: &str) -> String {
    let idx = (lang.0.plural)(n);
    let english = if n == 1 { one } else { other };
    let text = lang.catalog().plural(&format!("{one}|{other}"), idx).unwrap_or(english);
    fmt(text, &[("n", &n.to_string())])
}

/// [`trn`] in the current language.
pub fn tn(n: u64, one: &str, other: &str) -> String {
    trn(current(), n, one, other)
}

/// Translate a status or error message for display (see the module docs). Unknown messages, and
/// the parts of a message a template's placeholders stand for (names, versions, reasons), stay
/// as they are.
pub fn message(lang: Lang, s: &str) -> String {
    lang.catalog().message(s).unwrap_or_else(|| s.to_string())
}

/// [`message`] in the current language, remembering the last answer: the status bar asks for the
/// same message every frame.
pub fn msg(s: &str) -> String {
    static LAST: std::sync::Mutex<Option<(&'static str, String, String)>> = std::sync::Mutex::new(None);
    let lang = current();
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((code, src, out)) = last.as_ref()
        && *code == lang.code()
        && src == s
    {
        return out.clone();
    }
    let out = message(lang, s);
    *last = Some((lang.code(), s.to_string(), out.clone()));
    out
}
