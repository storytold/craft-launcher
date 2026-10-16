use super::*;
use craft_ext_core::palette::{Palette, Rgb};
use craft_ext_core::version::Version;
use std::collections::BTreeSet;

fn app(s: &str) -> AppId {
    AppId::new(s).unwrap()
}

fn ext(s: &str) -> ExtensionId {
    ExtensionId::new(s).unwrap()
}

fn palette() -> Palette {
    Palette {
        dark: true,
        background: Rgb(0x12, 0x13, 0x16),
        surface: Rgb(0x1c, 0x1e, 0x24),
        text: Rgb(0xf2, 0xf1, 0xee),
        muted_text: Rgb(0xb0, 0xb2, 0xb8),
        line: Rgb(0x44, 0x46, 0x4c),
        accent: Rgb(0x2d, 0x81, 0xff),
        good: Rgb(0x3f, 0xd6, 0xb8),
        warn: Rgb(0xff, 0xc1, 0x4d),
        bad: Rgb(0xff, 0x62, 0x59),
    }
}

fn row(id: &str, name: &str, apps: &[&str], enabled: &[&str], worn: &[&str]) -> Row {
    let set = |names: &[&str]| names.iter().map(|n| app(n)).collect::<BTreeSet<_>>();
    Row {
        id: ext(id),
        version: Version::new(1, 0, 0),
        kind: Kind::Theme,
        name: name.to_string(),
        tier: Tier::FromFile,
        apps: apps.iter().map(|n| app(n)).collect(),
        enabled: set(enabled),
        worn: set(worn),
        palette: Some(palette()),
    }
}

#[test]
fn choosing_a_theme_in_an_app_it_is_not_switched_on_for_switches_it_on_first() {
    let r = row("org.example.ocean", "Ocean", &["photocraft", "lightcraft"], &[], &[]);
    assert_eq!(
        wear_steps(&r, &app("photocraft")),
        vec![
            Step::SetEnabled { app: app("photocraft"), id: ext("org.example.ocean"), on: true },
            Step::Wear { app: app("photocraft"), id: Some(ext("org.example.ocean")) },
        ]
    );
}

#[test]
fn choosing_a_theme_that_is_already_switched_on_only_chooses_it() {
    let r = row("org.example.ocean", "Ocean", &["photocraft"], &["photocraft"], &[]);
    assert_eq!(wear_steps(&r, &app("photocraft")), vec![Step::Wear { app: app("photocraft"), id: Some(ext("org.example.ocean")) }]);
}

#[test]
fn choosing_a_theme_the_app_already_wears_gives_the_app_its_own_look_back() {
    let r = row("org.example.ocean", "Ocean", &["photocraft"], &["photocraft"], &["photocraft"]);
    assert_eq!(wear_steps(&r, &app("photocraft")), vec![Step::Wear { app: app("photocraft"), id: None }]);
}

#[test]
fn an_app_the_theme_is_not_made_for_gets_no_steps() {
    let r = row("org.example.ocean", "Ocean", &["photocraft"], &["photocraft"], &[]);
    assert_eq!(wear_steps(&r, &app("filmcraft")), Vec::new());
    assert_eq!(enable_steps(&r, &app("filmcraft")), Vec::new());
}

#[test]
fn use_in_all_apps_skips_the_apps_already_wearing_it_and_keeps_a_stable_order() {
    let r = row("org.example.ocean", "Ocean", &["photocraft", "lightcraft", "pdfcraft"], &["photocraft"], &["pdfcraft"]);
    let id = ext("org.example.ocean");
    assert_eq!(
        everywhere_steps(&r),
        vec![
            Step::SetEnabled { app: app("lightcraft"), id: id.clone(), on: true },
            Step::Wear { app: app("lightcraft"), id: Some(id.clone()) },
            Step::Wear { app: app("photocraft"), id: Some(id) },
        ]
    );
}

#[test]
fn switching_an_extension_flips_its_state_for_that_app() {
    let on = row("org.example.ocean", "Ocean", &["photocraft"], &["photocraft"], &[]);
    let off = row("org.example.ocean", "Ocean", &["photocraft"], &[], &[]);
    assert_eq!(enable_steps(&on, &app("photocraft")), vec![Step::SetEnabled { app: app("photocraft"), id: ext("org.example.ocean"), on: false }]);
    assert_eq!(enable_steps(&off, &app("photocraft")), vec![Step::SetEnabled { app: app("photocraft"), id: ext("org.example.ocean"), on: true }]);
}

#[test]
fn the_themes_tab_lists_only_themes_it_can_draw_sorted_by_name_ignoring_case() {
    let mut plugin = row("org.example.tone", "Aardvark tone", &["photocraft"], &[], &[]);
    plugin.kind = Kind::Plugin;
    // Even a plug-in that carries colours is not a theme.
    let mut unreadable = row("org.example.broken", "Broken", &["photocraft"], &[], &[]);
    unreadable.palette = None;
    let rows = vec![
        row("org.example.ember", "ember", &["photocraft"], &[], &[]),
        plugin,
        row("org.example.amber", "Amber", &["photocraft"], &[], &[]),
        unreadable,
        row("org.example.mist", "Mist", &["photocraft"], &[], &[]),
    ];
    let names: Vec<&str> = themes(&rows).iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Amber", "ember", "Mist"], "sorted ignoring case");
}

#[test]
fn the_installed_list_keeps_every_kind_and_orders_equal_names_by_id() {
    let mut plugin = row("org.example.tone", "Tone", &["photocraft"], &[], &[]);
    plugin.kind = Kind::Plugin;
    let rows = vec![row("org.example.b", "Same", &["photocraft"], &[], &[]), plugin, row("org.example.a", "Same", &["photocraft"], &[], &[])];
    let ids: Vec<&str> = ordered(&rows).iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["org.example.a", "org.example.b", "org.example.tone"]);
}

#[test]
fn known_apps_use_their_catalogue_name_and_unknown_ones_their_id() {
    assert_eq!(app_name(&app("photocraft")), "PhotoCraft");
    assert_eq!(app_name(&app("somenewcraft")), "somenewcraft");
}

#[test]
fn the_in_use_line_names_the_apps_in_a_stable_order_or_is_absent() {
    let none = row("org.example.ocean", "Ocean", &["photocraft"], &["photocraft"], &[]);
    assert_eq!(in_use_line(&none), None);
    let two = row("org.example.ocean", "Ocean", &["photocraft", "lightcraft"], &["photocraft", "lightcraft"], &["photocraft", "lightcraft"]);
    assert_eq!(in_use_line(&two).as_deref(), Some("LightCraft, PhotoCraft"));
    // Ordered by the words a person reads, not by id: an unknown app shows its lowercase id.
    let mixed = row("org.example.ocean", "Ocean", &["photocraft", "aaacraft"], &["photocraft", "aaacraft"], &["photocraft", "aaacraft"]);
    assert_eq!(in_use_line(&mixed).as_deref(), Some("PhotoCraft, aaacraft"));
}

#[test]
fn every_origin_and_kind_has_its_own_words_with_no_developer_terms() {
    let tiers = [Tier::FirstParty, Tier::FromFile, Tier::ReviewedThirdParty];
    let labels: BTreeSet<&str> = tiers.iter().map(|t| trust_label(*t)).collect();
    assert_eq!(labels.len(), 3, "each origin reads differently");
    let kinds: BTreeSet<&str> = [Kind::Theme, Kind::Plugin, Kind::Resource].iter().map(|k| kind_label(*k)).collect();
    assert_eq!(kinds.len(), 3);
    for text in labels.into_iter().chain(kinds) {
        assert!(!text.is_empty());
        let lower = text.to_lowercase();
        for banned in ["tier", "sha", "hash", "wasm", "abi", "sandbox", "manifest", "namespace", "adapter"] {
            assert!(!lower.contains(banned), "{text:?} contains {banned:?}");
        }
    }
}
