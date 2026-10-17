//! The language registry and matching locale tags to it.

use std::sync::OnceLock;

use super::catalog::Catalog;
use super::detect::system_lang;

/// The writing system a language is drawn in, and so the system font it needs. Languages that
/// share one can share a face; Simplified and Traditional Chinese do not (they want different
/// faces), so each script is its own. The bundled faces have no CJK glyphs, so a language with a
/// script borrows a system font for it (`crate::theme::cjk_font`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    Chinese,
    Japanese,
}

impl Script {
    /// Every script a language can be written in.
    pub const ALL: [Script; 2] = [Script::Chinese, Script::Japanese];
}

/// One supported UI language.
pub struct LangInfo {
    /// Canonical BCP 47 code (`en`, `ru`, `zh-CN`, `de`, `fr`, `pt-BR`, `es`, `cs`, `pl`, `hu`, `ja`, `tr`).
    /// Also the `interfaceLanguage` value.
    pub code: &'static str,
    /// The language's name in itself, shown in the Preferences dropdown.
    pub name: &'static str,
    /// The language's name in English, shown instead where its own script can't be drawn.
    pub english_name: &'static str,
    /// The writing system this language is drawn in, or `None` if it needs no system CJK font.
    pub script: Option<Script>,
    /// Catalog file contents (empty for the built-in English).
    pub source: &'static str,
    /// Plural form index for a count (English, German, Spanish and Hungarian: 0 = one, 1 = other;
    /// Russian: 0 = one, 1 = few, 2 = many; Czech: 0 = one, 1 = few, 2 = other; Polish: 0 = one only
    /// for 1, 1 = few, 2 = many; French: 0 = one for 0 and 1; a language whose nouns don't inflect
    /// for number: always 0).
    pub plural: fn(u64) -> usize,
    /// Number of plural forms the catalog's `@plural` rows carry (checked by tests).
    pub plural_forms: usize,
    /// Must the catalog cover every `tl!` literal? (checked by tests)
    pub complete_menus: bool,
    catalog: OnceLock<Catalog>,
}

fn plural_one_other(n: u64) -> usize {
    usize::from(n != 1)
}

/// French: 0 and 1 are singular ("0 mise à jour"), everything else plural.
fn plural_zero_one(n: u64) -> usize {
    usize::from(n > 1)
}

/// A language whose nouns don't inflect for number: one form for every count.
fn plural_none(_: u64) -> usize {
    0
}

/// Russian and Ukrainian integer counts: 1 (but not 11) is `one`, 2–4 (but not 12–14)
/// `few`, everything else `many`.
pub(super) fn plural_east_slavic(n: u64) -> usize {
    match n % 100 {
        11..=14 => 2,
        _ => match n % 10 {
            1 => 0,
            2..=4 => 1,
            _ => 2,
        },
    }
}

/// Czech integer counts: 1 is `one`, 2–4 are `few`, everything else is `other`.
pub(super) fn plural_czech(n: u64) -> usize {
    match n {
        1 => 0,
        2..=4 => 1,
        _ => 2,
    }
}

/// Polish integer counts: only 1 itself is `one` (21 is "21 aplikacji"), 2–4 in the last digit
/// (but not 12–14) `few`, everything else `many`.
pub(super) fn plural_polish(n: u64) -> usize {
    if n == 1 {
        0
    } else if matches!(n % 10, 2..=4) && !matches!(n % 100, 12..=14) {
        1
    } else {
        2
    }
}

/// The registry. English first: it is the fallback and the source language.
pub static LANGUAGES: [LangInfo; 12] = [
    LangInfo {
        code: "en",
        name: "English",
        english_name: "English",
        script: None,
        source: "",
        plural: plural_one_other,
        plural_forms: 2,
        complete_menus: false,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "ru",
        name: "Русский",
        english_name: "Russian",
        script: None,
        source: include_str!("ru.tsv"),
        plural: plural_east_slavic,
        plural_forms: 3,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "zh-CN",
        name: "简体中文",
        english_name: "Simplified Chinese",
        script: Some(Script::Chinese),
        source: include_str!("zh-CN.tsv"),
        plural: plural_none,
        plural_forms: 1,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "de",
        name: "Deutsch",
        english_name: "German",
        script: None,
        source: include_str!("de.tsv"),
        plural: plural_one_other,
        plural_forms: 2,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "fr",
        name: "Français",
        english_name: "French",
        script: None,
        source: include_str!("fr.tsv"),
        plural: plural_zero_one,
        plural_forms: 2,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "pt-BR",
        name: "Português (Brasil)",
        english_name: "Portuguese (Brazil)",
        script: None,
        source: include_str!("pt-BR.tsv"),
        plural: plural_one_other,
        plural_forms: 2,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "es",
        name: "Español",
        english_name: "Spanish",
        script: None,
        source: include_str!("es.tsv"),
        plural: plural_one_other,
        plural_forms: 2,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "cs",
        name: "Čeština",
        english_name: "Czech",
        script: None,
        source: include_str!("cs.tsv"),
        plural: plural_czech,
        plural_forms: 3,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "pl",
        name: "Polski",
        english_name: "Polish",
        script: None,
        source: include_str!("pl.tsv"),
        plural: plural_polish,
        plural_forms: 3,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "hu",
        name: "Magyar",
        english_name: "Hungarian",
        script: None,
        source: include_str!("hu.tsv"),
        plural: plural_one_other,
        plural_forms: 2,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "ja",
        name: "日本語",
        english_name: "Japanese",
        script: Some(Script::Japanese),
        source: include_str!("ja.tsv"),
        plural: plural_none,
        plural_forms: 1,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
    LangInfo {
        code: "tr",
        name: "Türkçe",
        english_name: "Turkish",
        script: None,
        source: include_str!("tr.tsv"),
        plural: plural_none,
        plural_forms: 1,
        complete_menus: true,
        catalog: OnceLock::new(),
    },
];

impl LangInfo {
    pub(super) fn catalog(&self) -> &Catalog {
        self.catalog.get_or_init(|| Catalog::parse(self.source))
    }
}

/// A language the UI can be shown in (a handle into [`LANGUAGES`]).
#[derive(Clone, Copy)]
pub struct Lang(pub(super) &'static LangInfo);

impl std::fmt::Debug for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lang({})", self.0.code)
    }
}

impl PartialEq for Lang {
    fn eq(&self, other: &Self) -> bool {
        self.0.code == other.0.code
    }
}

impl Eq for Lang {}

impl Lang {
    pub const EN: Lang = Lang(&LANGUAGES[0]);
    pub const RU: Lang = Lang(&LANGUAGES[1]);
    pub const ZH_CN: Lang = Lang(&LANGUAGES[2]);
    pub const DE: Lang = Lang(&LANGUAGES[3]);
    pub const FR: Lang = Lang(&LANGUAGES[4]);
    pub const PT_BR: Lang = Lang(&LANGUAGES[5]);
    pub const ES: Lang = Lang(&LANGUAGES[6]);
    pub const CS: Lang = Lang(&LANGUAGES[7]);
    pub const PL: Lang = Lang(&LANGUAGES[8]);
    pub const HU: Lang = Lang(&LANGUAGES[9]);
    pub const JA: Lang = Lang(&LANGUAGES[10]);
    pub const TR: Lang = Lang(&LANGUAGES[11]);

    pub fn code(self) -> &'static str {
        self.0.code
    }

    /// A language by its exact code (case-insensitive).
    pub fn from_code(code: &str) -> Option<Lang> {
        LANGUAGES.iter().find(|l| l.code.eq_ignore_ascii_case(code)).map(Lang)
    }

    /// Resolve the `interfaceLanguage` preference: a language code, or `auto` (and anything
    /// unknown) to follow the system locale.
    pub fn from_pref(pref: &str) -> Lang {
        Lang::from_code(pref).unwrap_or_else(system_lang)
    }

    /// Every registered language.
    pub fn all() -> impl Iterator<Item = Lang> {
        LANGUAGES.iter().map(Lang)
    }

    pub fn name(self) -> &'static str {
        self.0.name
    }

    /// The language's name in English.
    pub fn english_name(self) -> &'static str {
        self.0.english_name
    }

    /// Does drawing this language need a CJK font from the system?
    pub fn needs_cjk_font(self) -> bool {
        self.0.script.is_some()
    }

    /// The writing system this language is drawn in, if it needs a system CJK font.
    pub fn script(self) -> Option<Script> {
        self.0.script
    }

    /// Does this language's catalog claim to cover every `tl!` literal?
    pub fn complete_menus(self) -> bool {
        self.0.complete_menus
    }

    /// Index into [`LANGUAGES`].
    pub(super) fn index(self) -> usize {
        LANGUAGES.iter().position(|l| l.code == self.0.code).unwrap_or(0)
    }

    pub(super) fn catalog(self) -> &'static Catalog {
        self.0.catalog()
    }
}

/// Candidate language codes for a locale tag, most specific first: `ru_RU.UTF-8` →
/// `ru-ru`, `ru`.
pub(super) fn candidates(tag: &str) -> Vec<String> {
    let base = tag.split(['.', '@']).next().unwrap_or("").replace('_', "-").to_ascii_lowercase();
    let parts: Vec<&str> = base.split('-').filter(|p| !p.is_empty()).collect();
    (1..=parts.len()).rev().filter_map(|n| parts.get(..n)).map(|p| p.join("-")).collect()
}

/// The registered language for a locale tag such as `ru_RU.UTF-8`, `ru-RU`, `zh-Hans-CN`;
/// `None` if the language isn't supported. `C`/`POSIX` mean English.
pub fn lang_from_tag(tag: &str) -> Option<Lang> {
    let cands = candidates(tag);
    match cands.last().map(String::as_str) {
        Some("c" | "posix") => Some(Lang::EN),
        Some("zh") => chinese(cands.first().map_or("zh", String::as_str)),
        Some("pt") => Some(Lang::PT_BR),
        _ => cands.iter().find_map(|c| Lang::from_code(c)),
    }
}

/// Chinese by script: Simplified for `zh-Hans`, and for a bare `zh` or the regions that write
/// Simplified (mainland China, Singapore) when no script is given. An explicit script wins over
/// the region (`zh-Hant-CN` is Traditional); Traditional Chinese isn't offered.
fn chinese(tag: &str) -> Option<Lang> {
    let parts: Vec<&str> = tag.split('-').collect();
    if parts.contains(&"hant") {
        None
    } else if parts.contains(&"hans") || parts.len() == 1 || parts.iter().any(|p| matches!(*p, "cn" | "sg")) {
        Some(Lang::ZH_CN)
    } else {
        None
    }
}
