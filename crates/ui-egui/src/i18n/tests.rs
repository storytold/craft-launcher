//! Tests for the i18n catalogs (format, duplicates, placeholders, plural forms, coverage) and the
//! lookups.

use std::collections::{BTreeSet, HashSet};

use super::catalog::{Catalog, unescape};
use super::lang::{plural_czech, plural_east_slavic, plural_polish};
use super::{LANGUAGES, Lang, LangInfo, lang_from_tag, message, tr, trn};

/// Each catalog's rows as (line number, context, source, translation), split exactly as the
/// parser does: `trim_end` only, since a plain row *starts* with a TAB.
fn rows(lang: &LangInfo) -> Vec<(usize, &str, &str, &str)> {
    let mut out = Vec::new();
    for (i, line) in lang.source.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [context, source, translation] = fields.as_slice() else {
            panic!("{} line {}: expected 3 tab-separated fields: {line:?}", lang.code, i + 1);
        };
        out.push((i + 1, *context, *source, *translation));
    }
    out
}

fn catalogs() -> impl Iterator<Item = &'static LangInfo> {
    LANGUAGES.iter().filter(|l| !l.source.is_empty())
}

fn placeholders(template: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(len) = rest[open..].find('}') else { break };
        let name = &rest[open + 1..open + len];
        if !name.is_empty() {
            out.insert(name.to_string());
        }
        rest = &rest[open + len + 1..];
    }
    out
}

#[test]
fn catalogs_have_rows_and_no_empty_translations() {
    for lang in catalogs() {
        let rows = rows(lang);
        assert!(rows.iter().filter(|r| r.1.is_empty()).count() > 50, "{}: plain rows were skipped", lang.code);
        for (line, _, source, translation) in rows {
            assert!(!source.is_empty() && !translation.is_empty(), "{} line {line}: empty field", lang.code);
        }
    }
}

#[test]
fn no_duplicate_keys() {
    for lang in catalogs() {
        let mut seen = HashSet::new();
        for (line, context, source, _) in rows(lang) {
            assert!(seen.insert((context, source)), "{} line {line}: duplicate key {context:?} {source:?}", lang.code);
        }
    }
}

#[test]
fn placeholders_match_in_translation() {
    for lang in catalogs() {
        for (line, context, source, translation) in rows(lang) {
            if context == "@id" {
                continue; // keyed by id: the English label isn't in the row
            }
            // Translations may reorder placeholders, but must keep each one.
            let (src, tgt) = (placeholders(source), placeholders(translation));
            assert_eq!(src, tgt, "{} line {line}: placeholders differ: {source:?} -> {translation:?}", lang.code);
        }
    }
}

#[test]
fn plural_forms_count_matches_language() {
    for lang in catalogs() {
        for (line, context, _, translation) in rows(lang) {
            if context == "@plural" {
                let forms = translation.split('|').count();
                assert_eq!(forms, lang.plural_forms, "{} line {line}: {translation:?}", lang.code);
            }
        }
        // The plural rule never asks for a form the catalog doesn't have.
        for n in 0..200 {
            assert!((lang.plural)(n) < lang.plural_forms, "{}: {n}", lang.code);
        }
    }
}

#[test]
fn every_msg_row_translates_its_own_text() {
    for lang in catalogs() {
        let cat = Catalog::parse(lang.source);
        for (line, context, source, translation) in rows(lang) {
            if context == "@msg" {
                assert_eq!(cat.message(&unescape(source)).as_deref(), Some(unescape(translation).as_str()), "{} line {line}", lang.code);
            }
        }
    }
}

#[test]
fn ellipsis_preserved() {
    for lang in catalogs() {
        for (line, context, source, translation) in rows(lang) {
            if context.starts_with('@') {
                continue;
            }
            assert_eq!(source.ends_with('…'), translation.ends_with('…'), "{} line {line}: {source:?} -> {translation:?}", lang.code);
        }
    }
}

/// Every string the UI looks up by literal (`tl!("…")`, `t("…")`) in the language of each catalog
/// that claims to be complete.
#[test]
fn complete_catalogs_cover_every_ui_literal() {
    let sources = [
        include_str!("../app.rs"),
        include_str!("../details.rs"),
        include_str!("../dialogs.rs"),
        include_str!("../model.rs"),
        include_str!("../notes.rs"),
        include_str!("../settings.rs"),
        include_str!("../views.rs"),
        include_str!("../widgets.rs"),
        // The desktop shell's tray menu and tooltip.
        include_str!("../../../../apps/artcraft-launcher/src/tray.rs"),
    ];
    let mut keys = BTreeSet::new();
    for source in sources {
        // `t("…")` calls, not `fmt(`, `.get(` and the like.
        for (i, _) in source.match_indices("t(\"") {
            if source[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.') {
                continue;
            }
            let start = i + 3;
            if let Some(len) = source[start..].find('"') {
                keys.insert(&source[start..start + len]);
            }
        }
        for (i, _) in source.match_indices("tl!(\"") {
            let start = i + 5;
            if let Some(len) = source[start..].find('"') {
                keys.insert(&source[start..start + len]);
            }
        }
    }
    assert!(keys.len() > 100, "found only {} UI literals", keys.len());
    // Plural keys: `tn(n, "one", "other")`, `trn(lang, n, "one", "other")`.
    let mut plurals = BTreeSet::new();
    for source in sources {
        for (i, call) in source.match_indices("tn(").chain(source.match_indices("trn(")) {
            if source[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_') || source[i + call.len()..].starts_with(')') {
                continue;
            }
            let forms: Vec<&str> = source[i..].split('"').skip(1).step_by(2).take(2).collect();
            if let [one, other] = forms.as_slice() {
                plurals.insert(format!("{one}|{other}"));
            }
        }
    }
    assert!(plurals.len() >= 4, "found only {plurals:?}");
    for lang in Lang::all().filter(|l| l.complete_menus()) {
        let missing: Vec<_> = keys.iter().filter(|k| !super::has(lang, k)).collect();
        assert!(missing.is_empty(), "{} is missing translations for {missing:#?}", lang.code());
        let missing: Vec<_> = plurals.iter().filter(|k| lang.catalog().plural(k, 0).is_none()).collect();
        assert!(missing.is_empty(), "{} is missing @plural rows for {missing:#?}", lang.code());
    }
}

#[test]
fn complete_catalogs_translate_every_tagline() {
    for lang in Lang::all().filter(|l| l.complete_menus()) {
        for app in &artcraft_launcher_core::SUITE {
            assert_ne!(super::tr_ctx(lang, app.id, app.tagline), app.tagline, "{}: {}", lang.code(), app.id);
        }
    }
}

#[test]
fn product_names_stay_in_english() {
    for lang in Lang::all() {
        for name in ["ArtCraft Launcher", "Craft App Suite", "Launcher"] {
            assert_eq!(tr(lang, name), name, "{}", lang.code());
        }
    }
}

#[test]
fn russian_plural_rule_works() {
    for n in [1, 21, 31, 101] {
        assert_eq!(plural_east_slavic(n), 0, "{n}");
    }
    for n in [2, 3, 4, 22, 23, 24, 102] {
        assert_eq!(plural_east_slavic(n), 1, "{n}");
    }
    for n in [0, 5, 11, 12, 13, 14, 15, 100, 111] {
        assert_eq!(plural_east_slavic(n), 2, "{n}");
    }
    assert_eq!(trn(Lang::RU, 21, "{n} day ago", "{n} days ago"), "21 день назад");
    assert_eq!(trn(Lang::RU, 3, "{n} day ago", "{n} days ago"), "3 дня назад");
}

#[test]
fn czech_plural_rule_works() {
    for n in [0, 5, 11, 21, 22, 25] {
        assert_eq!(plural_czech(n), 2, "{n}");
    }
    assert_eq!(plural_czech(1), 0);
    for n in [2, 4] {
        assert_eq!(plural_czech(n), 1, "{n}");
    }
    assert_eq!(trn(Lang::CS, 1, "{n} app", "{n} apps"), "1 aplikace");
    assert_eq!(trn(Lang::CS, 2, "{n} app", "{n} apps"), "2 aplikace");
    assert_eq!(trn(Lang::CS, 5, "{n} app", "{n} apps"), "5 aplikací");
}

#[test]
fn polish_plural_rule_works() {
    // Polish says "one" only for 1 itself: 21, 31 and 101 take "many", unlike Russian.
    assert_eq!(plural_polish(1), 0);
    for n in [2, 3, 4, 22, 23, 24, 102, 122] {
        assert_eq!(plural_polish(n), 1, "{n}");
    }
    for n in [0, 5, 11, 12, 13, 14, 15, 21, 25, 31, 100, 101, 111, 112] {
        assert_eq!(plural_polish(n), 2, "{n}");
    }
    assert_eq!(trn(Lang::PL, 1, "{n} app", "{n} apps"), "1 aplikacja");
    assert_eq!(trn(Lang::PL, 3, "{n} app", "{n} apps"), "3 aplikacje");
    assert_eq!(trn(Lang::PL, 22, "{n} app", "{n} apps"), "22 aplikacje");
    assert_eq!(trn(Lang::PL, 5, "{n} app", "{n} apps"), "5 aplikacji");
    assert_eq!(trn(Lang::PL, 21, "{n} app", "{n} apps"), "21 aplikacji");
    // A key the Polish catalog doesn't carry falls back to the English forms.
    assert_eq!(trn(Lang::PL, 3, "{n} unknown item", "{n} unknown items"), "3 unknown items");
}

#[test]
fn untranslated_plurals_fall_back_to_english_by_count() {
    // Russian's rule says "one" for 21, but English says "other".
    assert_eq!(trn(Lang::RU, 21, "{n} unknown item", "{n} unknown items"), "21 unknown items");
    assert_eq!(trn(Lang::RU, 1, "{n} unknown item", "{n} unknown items"), "1 unknown item");
    assert_eq!(trn(Lang::EN, 0, "{n} app", "{n} apps"), "0 apps");
}

#[test]
fn locale_tags_resolve() {
    for tag in ["cs", "cs-CZ", "cs_CZ.UTF-8", "CS_cz"] {
        assert_eq!(lang_from_tag(tag), Some(Lang::CS), "{tag}");
    }
    assert_eq!(super::detect::first_supported(["xx-XX", "cs-CZ", "en-US"]), Some(Lang::CS));
    assert_eq!(lang_from_tag("pt_BR.UTF-8"), Some(Lang::PT_BR));
    assert_eq!(lang_from_tag("pt-BR"), Some(Lang::PT_BR));
    assert_eq!(lang_from_tag("pt"), Some(Lang::PT_BR));
    assert_eq!(lang_from_tag("ru_RU.UTF-8"), Some(Lang::RU));
    assert_eq!(lang_from_tag("ru-RU"), Some(Lang::RU));
    assert_eq!(lang_from_tag("en-US"), Some(Lang::EN));
    assert_eq!(lang_from_tag("C"), Some(Lang::EN));
    assert_eq!(lang_from_tag("C.UTF-8"), Some(Lang::EN));
    assert_eq!(lang_from_tag("POSIX"), Some(Lang::EN));
    assert_eq!(lang_from_tag("xx-XX"), None);
    assert_eq!(lang_from_tag(""), None);
    assert_eq!(super::detect::first_supported(["xx-XX", "ru-RU", "en-US"]), Some(Lang::RU));
}

#[test]
fn german_locales_resolve() {
    for tag in ["de", "de-DE", "de_DE.UTF-8", "de-AT", "de-CH", "de_CH.UTF-8", "DE_de"] {
        assert_eq!(lang_from_tag(tag), Some(Lang::DE), "{tag}");
    }
    assert_eq!(super::detect::first_supported(["xx-XX", "de-DE", "en-US"]), Some(Lang::DE));
}

#[test]
fn german_counts_dates_and_messages_work() {
    assert_eq!(trn(Lang::DE, 1, "{n} app", "{n} apps"), "1 App");
    assert_eq!(trn(Lang::DE, 0, "{n} app", "{n} apps"), "0 Apps");
    assert_eq!(trn(Lang::DE, 21, "{n} day ago", "{n} days ago"), "vor 21 Tagen");
    assert_eq!(trn(Lang::DE, 1, "{n} day ago", "{n} days ago"), "vor 1 Tag");
    assert_eq!(super::tr_ctx(Lang::DE, "month", "Mar"), "März");
    assert_eq!(tr(Lang::DE, "{month} {day}, {year}"), "{day}. {month} {year}");
    assert_eq!(message(Lang::DE, "PhotoCraft updated to 0.5.0"), "PhotoCraft auf 0.5.0 aktualisiert");
    assert_eq!(tr(Lang::DE, "Untranslated diagnostic"), "Untranslated diagnostic");
}

#[test]
fn hungarian_locales_and_plurals_work() {
    for tag in ["hu", "hu-HU", "hu_HU.UTF-8", "HU_hu"] {
        assert_eq!(lang_from_tag(tag), Some(Lang::HU), "{tag}");
    }
    assert_eq!(super::detect::first_supported(["xx-XX", "hu-HU", "en-US"]), Some(Lang::HU));
    // After a number the noun stays singular; 1 is still the "one" form.
    assert_eq!(trn(Lang::HU, 0, "{n} app", "{n} apps"), "0 alkalmazás");
    assert_eq!(trn(Lang::HU, 1, "{n} app", "{n} apps"), "1 alkalmazás");
    assert_eq!(trn(Lang::HU, 2, "{n} app", "{n} apps"), "2 alkalmazás");
    assert_eq!(trn(Lang::HU, 21, "{n} day ago", "{n} days ago"), "21 napja");
    assert_eq!(trn(Lang::HU, 1, "{n} day ago", "{n} days ago"), "1 napja");
    assert_eq!(super::tr_ctx(Lang::HU, "month", "Mar"), "márc.");
    assert_eq!(tr(Lang::HU, "{month} {day}, {year}"), "{year}. {month} {day}.");
    assert_eq!(message(Lang::HU, "PhotoCraft updated to 0.5.0"), "PhotoCraft frissítve. Verzió: 0.5.0");
    assert_eq!(tr(Lang::HU, "Untranslated diagnostic"), "Untranslated diagnostic");
}

#[test]
fn french_locales_and_plurals_work() {
    for tag in ["fr", "fr-FR", "fr_CA.UTF-8", "fr-BE", "FR_ch"] {
        assert_eq!(lang_from_tag(tag), Some(Lang::FR), "{tag}");
    }
    assert_eq!(super::detect::first_supported(["xx-XX", "fr-FR", "en-US"]), Some(Lang::FR));
    // 0 and 1 are singular in French, unlike English.
    assert_eq!(trn(Lang::FR, 0, "{n} app", "{n} apps"), "0 application");
    assert_eq!(trn(Lang::FR, 1, "{n} app", "{n} apps"), "1 application");
    assert_eq!(trn(Lang::FR, 2, "{n} app", "{n} apps"), "2 applications");
    assert_eq!(trn(Lang::FR, 3, "{n} day ago", "{n} days ago"), "il y a 3 jours");
}

#[test]
fn spanish_locales_and_plurals_work() {
    for tag in ["es", "es-ES", "es_MX.UTF-8", "es-419", "es-PA", "ES_ar"] {
        assert_eq!(lang_from_tag(tag), Some(Lang::ES), "{tag}");
    }
    assert_eq!(super::detect::first_supported(["xx-XX", "es-PA", "en-US"]), Some(Lang::ES));
    // Only 1 is singular in Spanish, as in English.
    assert_eq!(trn(Lang::ES, 0, "{n} app", "{n} apps"), "0 aplicaciones");
    assert_eq!(trn(Lang::ES, 1, "{n} app", "{n} apps"), "1 aplicación");
    assert_eq!(trn(Lang::ES, 3, "{n} day ago", "{n} days ago"), "hace 3 días");
    assert_eq!(tr(Lang::ES, "{month} {day}, {year}"), "{day} {month} {year}");
    assert_eq!(message(Lang::ES, "PhotoCraft updated to 0.5.0"), "PhotoCraft actualizada a 0.5.0");
}

#[test]
fn polish_locales_resolve() {
    for tag in ["pl", "pl-PL", "pl_PL.UTF-8", "PL_pl"] {
        assert_eq!(lang_from_tag(tag), Some(Lang::PL), "{tag}");
    }
    assert_eq!(super::detect::first_supported(["xx-XX", "pl-PL", "en-US"]), Some(Lang::PL));
}

#[test]
fn simplified_chinese_locales_respect_the_script() {
    for tag in ["zh", "zh-CN", "zh_CN.UTF-8", "zh-SG", "zh-Hans", "zh-Hans-CN", "zh-Hans-HK", "ZH_cn"] {
        assert_eq!(lang_from_tag(tag), Some(Lang::ZH_CN), "{tag}");
    }
    for tag in ["zh-TW", "zh-HK", "zh-MO", "zh-Hant", "zh-Hant-CN", "zh-Hant-SG", "zh_TW.UTF-8"] {
        assert_eq!(lang_from_tag(tag), None, "{tag}");
    }
    // A Traditional Chinese system that also lists English gets English, not Simplified.
    assert_eq!(super::detect::first_supported(["zh-Hant-TW", "en-US"]), Some(Lang::EN));
}

#[test]
fn chinese_counts_and_english_fallback_work() {
    for n in [0, 1, 2, 11, 100] {
        assert_eq!(trn(Lang::ZH_CN, n, "{n} app", "{n} apps"), format!("{n} 个应用"));
        assert_eq!(trn(Lang::ZH_CN, n, "{n} min ago", "{n} min ago"), format!("{n} 分钟前"));
    }
    assert_eq!(trn(Lang::ZH_CN, 2, "{n} unknown item", "{n} unknown items"), "2 unknown items");
    assert_eq!(tr(Lang::ZH_CN, "Untranslated diagnostic"), "Untranslated diagnostic");
    assert_eq!(message(Lang::ZH_CN, "PhotoCraft updated to 0.5.0"), "PhotoCraft 已更新至 0.5.0");
}

#[test]
fn japanese_locales_and_counts_work() {
    for tag in ["ja", "ja-JP", "ja_JP.UTF-8", "JA_jp"] {
        assert_eq!(lang_from_tag(tag), Some(Lang::JA), "{tag}");
    }
    assert_eq!(super::detect::first_supported(["xx-XX", "ja-JP", "en-US"]), Some(Lang::JA));
    // Japanese has one plural form.
    for n in [0, 1, 2, 11, 100] {
        assert_eq!(trn(Lang::JA, n, "{n} app", "{n} apps"), format!("{n} 個のアプリ"));
        assert_eq!(trn(Lang::JA, n, "{n} day ago", "{n} days ago"), format!("{n} 日前"));
    }
    assert_eq!(trn(Lang::JA, 2, "{n} unknown item", "{n} unknown items"), "2 unknown items");
    assert_eq!(tr(Lang::JA, "Untranslated diagnostic"), "Untranslated diagnostic");
    assert_eq!(super::tr_ctx(Lang::JA, "month", "Mar"), "3月");
    assert_eq!(tr(Lang::JA, "{month} {day}, {year}"), "{year}年{month}{day}日");
    assert_eq!(message(Lang::JA, "PhotoCraft updated to 0.5.0"), "PhotoCraft を 0.5.0 に更新しました");
}

#[test]
fn korean_locales_counts_dates_and_messages_work() {
    for tag in ["ko", "ko-KR", "ko_KR.UTF-8", "KO_kr", "ko-KP", "ko-Hang-KR"] {
        assert_eq!(lang_from_tag(tag), Some(Lang::KO), "{tag}");
    }
    assert_eq!(super::detect::first_supported(["xx-XX", "ko-KR", "en-US"]), Some(Lang::KO));
    assert_eq!(Lang::from_code("KO"), Some(Lang::KO));
    for n in [0, 1, 2, 11, 100] {
        assert_eq!(trn(Lang::KO, n, "{n} app", "{n} apps"), format!("앱 {n}개"));
        assert_eq!(trn(Lang::KO, n, "{n} day ago", "{n} days ago"), format!("{n}일 전"));
    }
    assert_eq!(trn(Lang::KO, 2, "{n} unknown item", "{n} unknown items"), "2 unknown items");
    assert_eq!(tr(Lang::KO, "Untranslated diagnostic"), "Untranslated diagnostic");
    assert_eq!(super::tr_ctx(Lang::KO, "month", "Mar"), "3월");
    assert_eq!(tr(Lang::KO, "{month} {day}, {year}"), "{year}년 {month} {day}일");
    assert_eq!(message(Lang::KO, "PhotoCraft updated to 0.5.0"), "PhotoCraft 업데이트 완료: 0.5.0");
}

/// With a system Korean font installed, every UI family can draw the whole Korean catalog.
/// Machines without one (CI, the web) skip this; the UI then stays in English.
#[test]
fn system_font_covers_korean_in_every_ui_family() {
    use egui::{FontFamily, FontId};
    if !crate::theme::cjk_font_available(Lang::KO) {
        return;
    }
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx, Some(Lang::KO));
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    let text: String = Lang::KO
        .0
        .source
        .lines()
        .filter_map(|line| line.split('\t').nth(2))
        .flat_map(str::chars)
        .filter(|c| ('\u{1100}'..='\u{11ff}').contains(c) || ('\u{3130}'..='\u{318f}').contains(c) || ('\u{ac00}'..='\u{d7af}').contains(c))
        .collect();
    assert!(text.chars().count() > 500);
    for family in [
        FontFamily::Proportional,
        FontFamily::Monospace,
        crate::theme::medium(),
        crate::theme::semibold(),
        crate::theme::display(),
        crate::theme::mono(),
        crate::theme::mono_bold(),
    ] {
        ctx.fonts_mut(|fonts| assert!(fonts.has_glyphs(&FontId::new(14.0, family.clone()), &text), "missing glyph in {family:?}"));
    }
}

/// With the system's Chinese font installed, every UI family can draw the whole Chinese catalog.
/// Machines without one (CI, the web) skip this; the UI then stays in English.
#[test]
fn system_font_covers_chinese_in_every_ui_family() {
    use egui::{FontFamily, FontId};
    if !crate::theme::cjk_font_available(Lang::ZH_CN) {
        return;
    }
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx, Some(Lang::ZH_CN));
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    let text: String = Lang::ZH_CN
        .0
        .source
        .lines()
        .filter_map(|line| line.split('\t').nth(2))
        .flat_map(str::chars)
        .filter(|c| ('\u{3400}'..='\u{9fff}').contains(c))
        .collect();
    assert!(text.chars().count() > 500);
    for family in [
        FontFamily::Proportional,
        FontFamily::Monospace,
        crate::theme::medium(),
        crate::theme::semibold(),
        crate::theme::display(),
        crate::theme::mono(),
        crate::theme::mono_bold(),
    ] {
        ctx.fonts_mut(|fonts| assert!(fonts.has_glyphs(&FontId::new(14.0, family.clone()), &text), "missing glyph in {family:?}"));
    }
}

/// With a system CJK font installed, every UI family can draw the whole Japanese catalog.
/// Machines without one (CI, the web) skip this.
#[test]
fn system_font_covers_japanese_in_every_ui_family() {
    use egui::{FontFamily, FontId};
    if !crate::theme::cjk_font_available(Lang::JA) {
        return;
    }
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx, Some(Lang::JA));
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    let text: String = Lang::JA
        .0
        .source
        .lines()
        .filter_map(|line| line.split('\t').nth(2))
        .flat_map(str::chars)
        .filter(|c| ('\u{3040}'..='\u{30ff}').contains(c) || ('\u{3400}'..='\u{9fff}').contains(c))
        .collect();
    assert!(text.chars().count() > 200);
    for family in [
        FontFamily::Proportional,
        FontFamily::Monospace,
        crate::theme::medium(),
        crate::theme::semibold(),
        crate::theme::display(),
        crate::theme::mono(),
        crate::theme::mono_bold(),
    ] {
        ctx.fonts_mut(|fonts| assert!(fonts.has_glyphs(&FontId::new(14.0, family.clone()), &text), "missing glyph in {family:?}"));
    }
}

#[test]
fn language_labels_come_from_the_registry() {
    use crate::settings::Settings;
    for lang in Lang::all().filter(|l| !l.needs_cjk_font()) {
        assert_eq!(Settings::language_label(lang.code(), None), lang.name());
    }
    // A CJK language is named in English, and its own name is added only while its face is the one
    // in the font stack: no other face draws it.
    assert_eq!(Settings::language_label(Lang::ZH_CN.code(), None), "Simplified Chinese");
    assert_eq!(Settings::language_label(Lang::ZH_CN.code(), Some(Lang::ZH_CN)), "Simplified Chinese (简体中文)");
    assert_eq!(Settings::language_label(Lang::JA.code(), Some(Lang::ZH_CN)), "Japanese");
    assert_eq!(Settings::language_label(Lang::JA.code(), Some(Lang::JA)), "Japanese (日本語)");
    assert_eq!(Settings::language_label(Lang::KO.code(), None), "Korean");
    assert_eq!(Settings::language_label(Lang::KO.code(), Some(Lang::JA)), "Korean");
    assert_eq!(Settings::language_label(Lang::KO.code(), Some(Lang::KO)), "Korean (한국어)");
    assert_eq!(Settings::language_label(Settings::LANGUAGE_AUTO, None), "Auto");
    assert_eq!(Settings::language_label("xx", None), "Auto");
}

// The parser.

#[test]
fn parse_rows_of_every_kind() {
    let cat = Catalog::parse(
        "# comment\n\thello\tworld\nmenu\tOpen\tОткрыть\n@id\tfile.open\tОткрыть файл\n@plural\t{n} file|{n} files\t{n} файл|{n} файла|{n} файлов\n\
         @plural\t{n} app|{n} apps\t{n} 个应用\nbroken row\n",
    );
    assert_eq!(cat.plain("hello"), Some("world"));
    assert_eq!(cat.contextual("menu", "Open"), Some("Открыть"));
    assert_eq!(cat.id("file.open"), Some("Открыть файл"));
    assert_eq!(cat.plural("{n} file|{n} files", 0), Some("{n} файл"));
    assert_eq!(cat.plural("{n} file|{n} files", 2), Some("{n} файлов"));
    // One form serves every count.
    assert_eq!(cat.plural("{n} app|{n} apps", 1), Some("{n} 个应用"));
}

#[test]
fn unescape_works() {
    assert_eq!(unescape("hello\\tworld"), "hello\tworld");
    assert_eq!(unescape("line1\\nline2"), "line1\nline2");
    assert_eq!(unescape("backslash\\\\here"), "backslash\\here");
}

#[test]
fn message_templates_match_real_messages() {
    let cat = Catalog::parse(
        "@msg\tDownload cancelled\tСкачивание отменено\n\
         @msg\tCouldn't open {name}: {e}\tНе удалось открыть {name}: {e}\n\
         @msg\t{name} updated to {version}\t{name} обновлено до {version}\n\
         @msg\t{name} updated to {version} (no checksum)\t{name} обновлено до {version} (без суммы)\n\
         @msg\t{a}{b}\tnever\n",
    );
    assert_eq!(cat.message("Download cancelled").as_deref(), Some("Скачивание отменено"));
    assert_eq!(cat.message("Couldn't open PhotoCraft: no such file: x").as_deref(), Some("Не удалось открыть PhotoCraft: no such file: x"));
    assert_eq!(cat.message("PhotoCraft updated to 0.5.0").as_deref(), Some("PhotoCraft обновлено до 0.5.0"));
    // The template that spells out more of the message wins.
    assert_eq!(cat.message("PhotoCraft updated to 0.5.0 (no checksum)").as_deref(), Some("PhotoCraft обновлено до 0.5.0 (без суммы)"));
    // Placeholders never match nothing, and unknown messages aren't touched.
    assert_eq!(cat.message("Couldn't open : x"), None);
    assert_eq!(cat.message("Something else"), None);
    assert_eq!(message(Lang::EN, "Something else"), "Something else");
}

#[test]
fn complete_catalogs_translate_package_formats() {
    use artcraft_launcher_core::PackageKind;
    let kinds =
        [PackageKind::Msi, PackageKind::PortableZip, PackageKind::Dmg, PackageKind::Deb, PackageKind::Rpm, PackageKind::Flatpak, PackageKind::TarGz];
    for lang in Lang::all().filter(|l| l.complete_menus()) {
        for kind in kinds {
            assert!(super::has(lang, kind.label()), "{}: {}", lang.code(), kind.label());
        }
    }
}
