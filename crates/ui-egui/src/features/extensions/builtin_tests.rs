use super::*;
use craft_ext_core::palette::contrast_ratio;

fn app(s: &str) -> AppId {
    AppId::new(s).unwrap()
}

fn names(list: &[BuiltIn]) -> Vec<&str> {
    list.iter().map(|b| b.name).collect()
}

#[test]
fn every_app_that_carries_themes_is_listed_with_all_of_them() {
    let all = all();
    let count = |id: Option<&str>| all.iter().filter(|b| b.app.as_ref().map(AppId::as_str) == id).count();
    assert_eq!(count(Some("photocraft")), 14);
    assert_eq!(count(Some("lightcraft")), 4);
    assert_eq!(count(None), 2, "the launcher's own dark and light");
    assert_eq!(all.len(), 20);
}

#[test]
fn names_are_unique_and_apps_are_known_to_the_suite() {
    let all = all();
    let mut seen = std::collections::BTreeSet::new();
    for b in &all {
        assert!(seen.insert((b.app_name, b.name)), "{} twice", b.name);
        if let Some(id) = &b.app {
            let known = artcraft_launcher_core::CraftApp::by_id(id.as_str()).expect("an app of the suite");
            assert_eq!(known.name, b.app_name, "the app name is the suite's");
        }
    }
    assert!(all.iter().all(|b| !b.name.is_empty()));
}

#[test]
fn the_listed_order_is_the_apps_own_not_alphabetical() {
    let all = all();
    let photo: Vec<&str> = all.iter().filter(|b| b.app_name == "PhotoCraft").map(|b| b.name).collect();
    assert_eq!(photo.first(), Some(&"Pro (Dark)"));
    assert_eq!(photo.get(1), Some(&"Pro (Medium Gray)"));
    assert_eq!(photo.last(), Some(&"Soft Light"));
}

#[test]
fn the_dark_flag_agrees_with_the_colours() {
    let lum = |c: Rgb| contrast_ratio(c, Rgb(0, 0, 0));
    for b in all() {
        let text_is_lighter = lum(b.palette.text) > lum(b.palette.background);
        assert_eq!(b.palette.dark, text_is_lighter, "{}", b.name);
    }
}

#[test]
fn text_stays_readable_on_the_colours_it_is_drawn_on() {
    // The cards draw text bars on the background and muted bars on the panel: they must be tellable apart.
    for b in all() {
        let p = &b.palette;
        assert!(contrast_ratio(p.text, p.background) >= 3.0, "{} text on background", b.name);
        assert!(contrast_ratio(p.text, p.surface) >= 3.0, "{} text on surface", b.name);
    }
}

#[test]
fn a_focus_narrows_to_the_app_and_leaves_out_other_kinds() {
    let focus = |app_id: Option<&str>, kind: Option<Kind>| Focus { app: app_id.map(app), kind };
    assert_eq!(shown(&Focus::default()).len(), 20);
    assert_eq!(shown(&focus(Some("lightcraft"), None)).len(), 4);
    assert_eq!(names(&shown(&focus(Some("lightcraft"), Some(Kind::Theme)))), ["Charcoal", "Midnight", "Silver", "Paper"]);
    assert_eq!(shown(&focus(Some("photocraft"), None)).len(), 14);
    assert!(shown(&focus(Some("pdfcraft"), None)).is_empty(), "an app with no themes of its own");
    assert!(shown(&focus(None, Some(Kind::Plugin))).is_empty());
    assert!(shown(&focus(Some("photocraft"), Some(Kind::Resource))).is_empty());
    assert_eq!(shown(&focus(None, Some(Kind::Theme))).len(), 20);
}
