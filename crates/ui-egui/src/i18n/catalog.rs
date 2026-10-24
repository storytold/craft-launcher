//! TSV catalog parsing for UI localisation.
//!
//! Format (one entry per line):
//!   context<TAB>source<TAB>translation
//! - Empty context = plain UI string (`tl!`, `t`).
//! - `@id` context = keyed by command id (`tr_id`).
//! - `@plural` context = plural forms: the English `one|other` key, and the language's forms
//!   separated by `|` (one form for a language whose nouns don't inflect for number, up to
//!   three: `one|few|many` for Russian, Ukrainian and Polish).
//! - `@msg` context = a status/error message, or a template whose `{placeholders}` match any text
//!   (`message`, `msg`).
//! - Any other context = disambiguation for `tr_ctx`.
//!
//! Escapes in source/translation: `\t` `\n` `\\` (parsed once at load time).
//! Placeholders `{name}` must appear in both source and translation.
//! A trailing `…` (U+2026) in source must be preserved in translation.

use std::collections::HashMap;

/// A parsed translation catalog for one language.
#[derive(Debug, Default)]
pub struct Catalog {
    /// Plain strings: English -> translation.
    plain: HashMap<String, String>,
    /// Contextual strings: context -> (English -> translation).
    contextual: HashMap<String, HashMap<String, String>>,
    /// Command-id keyed strings: id -> translation.
    by_id: HashMap<String, String>,
    /// Plural forms: English "one|other" -> the language's forms.
    plural: HashMap<String, Vec<String>>,
    /// Messages: English message or template -> translation.
    messages: HashMap<String, String>,
    /// The `@msg` rows with placeholders, matched against messages that have no exact row.
    templates: Vec<Template>,
}

/// An `@msg` template, split into its literal text and placeholders.
#[derive(Debug)]
struct Template {
    /// Literal text around the placeholders: always one more than `names`.
    literals: Vec<String>,
    names: Vec<String>,
    translation: String,
}

impl Template {
    fn new(source: &str, translation: String) -> Option<Self> {
        let mut literals = Vec::new();
        let mut names = Vec::new();
        let mut rest = source;
        while let Some(open) = rest.find('{') {
            let close = open + rest.get(open..)?.find('}')?;
            literals.push(rest.get(..open)?.to_string());
            names.push(rest.get(open + 1..close)?.to_string());
            rest = rest.get(close + 1..)?;
        }
        literals.push(rest.to_string());
        // Two placeholders in a row can't be told apart.
        if names.is_empty() || literals.iter().skip(1).rev().skip(1).any(String::is_empty) {
            return None;
        }
        Some(Self { literals, names, translation })
    }

    /// The length of the template's literal text: the more of a message a template spells out,
    /// the better it fits.
    fn specificity(&self) -> usize {
        self.literals.iter().map(String::len).sum()
    }

    /// The translation with this message's placeholder values filled in, if `s` fits the template.
    /// Each placeholder matches at least one character; between two literals it takes the
    /// shortest text that fits, the last one takes everything up to the final literal.
    fn apply(&self, s: &str) -> Option<String> {
        let (first, between) = self.literals.split_first()?;
        let mut rest = s.strip_prefix(first.as_str())?;
        let mut values = Vec::with_capacity(self.names.len());
        for (i, literal) in between.iter().enumerate() {
            let value = if i + 1 == between.len() {
                let value = rest.strip_suffix(literal.as_str())?;
                rest = "";
                value
            } else {
                // Skip one character, so the value isn't empty.
                let skip = rest.chars().next()?.len_utf8();
                let at = skip + rest.get(skip..)?.find(literal.as_str())?;
                let value = rest.get(..at)?;
                rest = rest.get(at + literal.len()..)?;
                value
            };
            if value.is_empty() {
                return None;
            }
            values.push(value);
        }
        let args: Vec<(&str, &str)> = self.names.iter().map(String::as_str).zip(values).collect();
        Some(super::translate::fmt(&self.translation, &args))
    }
}

impl Catalog {
    /// Parse a catalog from TSV content. Malformed rows are logged and skipped.
    pub fn parse(source: &str) -> Self {
        let mut cat = Catalog::default();
        for (line_no, line) in source.lines().enumerate() {
            let line_no = line_no + 1;
            // Only the end: a plain row starts with its (empty) context and a TAB.
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            let [context, source_text, translation] = parts.as_slice() else {
                log::warn!("i18n catalog: line {line_no}: expected 3 tab-separated fields, got {}", parts.len());
                continue;
            };
            let (context, source_text, translation) = (unescape(context), unescape(source_text), unescape(translation));
            let display_text = if !translation.contains('{') && super::bidi::has_rtl(&translation) {
                super::bidi::reorder(&translation).into_owned()
            } else {
                translation.clone()
            };
            let duplicate = match context.as_str() {
                "" => cat.plain.insert(source_text.clone(), display_text).is_some(),
                "@id" => cat.by_id.insert(source_text.clone(), display_text).is_some(),
                "@plural" => {
                    let forms: Vec<String> = translation.split('|').map(str::to_string).collect();
                    if forms.len() > 6 {
                        log::warn!("i18n catalog: line {line_no}: @plural expects 1 to 6 forms separated by '|', got {}", forms.len());
                        continue;
                    }
                    cat.plural.insert(source_text.clone(), forms).is_some()
                }
                "@msg" => {
                    if source_text.contains('{') {
                        match Template::new(&source_text, translation.clone()) {
                            Some(t) => cat.templates.push(t),
                            None => log::warn!("i18n catalog: line {line_no}: @msg template can't be matched: {source_text:?}"),
                        }
                    }
                    cat.messages.insert(source_text.clone(), translation).is_some()
                }
                _ => cat.contextual.entry(context.clone()).or_default().insert(source_text.clone(), display_text).is_some(),
            };
            if duplicate {
                log::warn!("i18n catalog: line {line_no}: duplicate key {context:?} {source_text:?}");
            }
        }
        // The most specific template first.
        cat.templates.sort_by_key(|t| std::cmp::Reverse(t.specificity()));
        cat
    }

    /// Look up a plain string.
    pub fn plain(&self, s: &str) -> Option<&str> {
        self.plain.get(s).map(String::as_str)
    }

    /// Look up a contextual string.
    pub fn contextual(&self, context: &str, s: &str) -> Option<&str> {
        self.contextual.get(context)?.get(s).map(String::as_str)
    }

    /// Look up a command-id keyed string.
    pub fn id(&self, id: &str) -> Option<&str> {
        self.by_id.get(id).map(String::as_str)
    }

    /// Look up plural forms for an English "one|other" key; the form at `index` (0 = one).
    /// A row with fewer forms than the language's rule uses its last form for the rest.
    pub fn plural(&self, key: &str, index: usize) -> Option<&str> {
        let forms = self.plural.get(key)?;
        forms.get(index).or_else(|| forms.last()).map(String::as_str)
    }

    /// Translate a message: an exact `@msg` row, else the most specific template it fits.
    pub fn message(&self, s: &str) -> Option<String> {
        if let Some(t) = self.messages.get(s) {
            return Some(t.clone());
        }
        self.templates.iter().find_map(|t| t.apply(s))
    }
}

/// Unescape `\t`, `\n`, `\\` in a TSV field.
pub(super) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
