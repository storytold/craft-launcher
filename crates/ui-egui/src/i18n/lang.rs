//! The language registry and matching locale tags to it.

use std::sync::OnceLock;

use super::catalog::Catalog;
use super::detect::system_lang;

/// One supported UI language.
pub struct LangInfo {
    /// Canonical BCP 47 code (`en`, `ru`, `zh-CN`, `de`, `fr`). Also the `interfaceLanguage` value.
    pub code: &'static str,
    /// The language's name in itself, shown in the Preferences dropdown.
    pub name: &'static str,
    /// The language's name in English, shown instead where its own script can't be drawn.
    pub english_name: &'static str,
    /// Does its script need a CJK font? The bundled faces have none, so it comes from the
    /// system (`theme::cjk_font`).
    pub cjk: bool,
    /// Catalog file contents (empty for the built-in English).
    pub source: &'static str,
    /// Plural form index for a count (English and German: 0 = one, 1 = other; Russian: 0 = one,
    /// 1 = few, 2 = many; French: 0 = one for 0 and 1; Chinese: always 0).
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

/// Languages whose nouns don't inflect for number (Chinese): one form for every count.
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

/// The registry. English first: it is the fallback and the source language.
pub static LANGUAGES: [LangInfo; 6] = [
    LangInfo {
        code: "en",
        name: "English",
        english_name: "English",
        cjk: false,
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
        cjk: false,
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
        cjk: true,
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
        cjk: false,
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
        cjk: false,
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
        cjk: false,
        source: include_str!("pt-BR.tsv"),
        plural: plural_one_other,
        plural_forms: 2,
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
        self.0.cjk
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
