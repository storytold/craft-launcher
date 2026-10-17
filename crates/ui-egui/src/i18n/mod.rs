//! UI localisation for ArtCraft Launcher. Strings in code stay English and are the default
//! lookup keys; a per-language catalog (`*.tsv`) maps them to display text at render time.
//!
//! # Adding a language
//! 1. Add `xx.tsv` next to `ru.tsv` (copy its header; translate from the *meaning* of the
//!    English text, clean-room).
//! 2. Add one row to [`LANGUAGES`] (code, native name, catalog, plural rule). For a complete
//!    catalog, cover every UI literal and plural key; the tests enforce that claim. A translation
//!    that isn't finished yet starts with `complete_menus: false`: it still shows in
//!    Preferences, strings without a row fall back to English, and the completeness tests
//!    skip it until every row is done. A CJK language also sets `script` and takes its system
//!    font from `crate::cjk_font`.
//!
//! That is all: the Preferences dropdown, the system-locale match and the catalog tests
//! (parse, placeholders, plural forms, coverage of every `tl!` literal) pick it up from the
//! registry.
//!
//! # Looking strings up
//! - [`t`] / the `tl!` macro: a plain string in the language the UI is drawn in.
//!   [`tr`] takes the language explicitly; [`tr_ctx`] when one English word needs different
//!   translations.
//! - [`tr_id`]: a command-id keyed string with the English label as fallback (menu items).
//! - [`trn`]: plural-aware (`{n}` is filled in). [`fmt`]: fill `{name}` placeholders after [`tr`].
//! - [`message`] / [`msg`]: a status or error message, translated only where it is shown.
//!   The message itself stays English (the control channel, tests read it); `@msg` catalog
//!   rows hold whole messages or templates such as `Couldn't open {name}: {e}`, whose
//!   placeholders match any text and are carried over into the translation.

mod catalog;
mod detect;
mod lang;
mod translate;

#[cfg(test)]
mod tests;

pub use detect::{detect_system_lang_in_background, set_system_locales, set_test_lang, system_lang};
pub use lang::{LANGUAGES, Lang, LangInfo, Script, lang_from_tag};
pub use translate::{current, fmt, has, message, msg, set_current, t, t_id, tn, tr, tr_ctx, tr_id, trn, upper};
