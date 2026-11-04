//! Drives the Extensions page against the in-memory sample store: what shows, and what each click
//! changes. No files and no network.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event};
use artcraft_launcher_core::{Arch, Os, Platform};
use artcraft_launcher_ui_egui::features::extensions::sample::SampleSource;
use artcraft_launcher_ui_egui::features::extensions::{ExtensionSource, Row};
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings};
use craft_ext_core::id::{AppId, ExtensionId};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

struct Quiet;

impl Backend for Quiet {
    fn send(&mut self, _command: Command) {}
    fn poll(&mut self) -> Vec<Event> {
        Vec::new()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: true, reveal_label: "Show in Finder", self_update: false }
    }
    fn platform(&self) -> Option<Platform> {
        Some(Platform { os: Os::MacOs, arch: Arch::Arm64 })
    }
    fn now(&self) -> i64 {
        1_791_500_000
    }
}

/// The sample store, shared with the test so it can look at it and change it behind the screen's back.
#[derive(Clone, Default)]
struct Shared(Rc<RefCell<SampleSource>>);

impl ExtensionSource for Shared {
    fn is_demo(&self) -> bool {
        self.0.borrow().is_demo()
    }
    fn generation(&mut self) -> u64 {
        self.0.borrow_mut().generation()
    }
    fn rows(&mut self) -> Result<Vec<Row>, String> {
        self.0.borrow_mut().rows()
    }
    fn add_file(&mut self, path: &Path) -> Result<String, String> {
        self.0.borrow_mut().add_file(path)
    }
    fn remove(&mut self, id: &ExtensionId) -> Result<(), String> {
        self.0.borrow_mut().remove(id)
    }
    fn set_enabled(&mut self, app: &AppId, id: &ExtensionId, on: bool) -> Result<(), String> {
        self.0.borrow_mut().set_enabled(app, id, on)
    }
    fn wear(&mut self, app: &AppId, id: Option<&ExtensionId>) -> Result<(), String> {
        self.0.borrow_mut().wear(app, id)
    }
}

struct WithStore {
    store: Shared,
    picked: Option<PathBuf>,
}

impl Host for WithStore {
    fn pick_file(&mut self, _title: &str, _extensions: &[&str]) -> Option<PathBuf> {
        self.picked.take()
    }
    fn extensions(&mut self) -> Option<&mut dyn ExtensionSource> {
        Some(&mut self.store)
    }
}

struct NoStore;
impl Host for NoStore {}

fn harness<H: Host + 'static>(mut host: H) -> Harness<'static, Option<LauncherUi>> {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let mut backend: Option<Box<dyn Backend>> = Some(Box::new(Quiet));
    Harness::builder().with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if let Some(b) = backend.take() {
                *state = Some(LauncherUi::new(ui.ctx(), b, Settings { notify: false, ..Settings::default() }));
            }
            if let Some(launcher) = state {
                launcher.logic(&ui.ctx().clone(), &mut host);
                launcher.ui(ui, &mut host);
            }
        },
        None,
    )
}

fn open(store: &Shared, picked: Option<PathBuf>) -> Harness<'static, Option<LauncherUi>> {
    let mut h = harness(WithStore { store: store.clone(), picked });
    h.run();
    h.get_by_label("Add-ons").click();
    h.run();
    h
}

/// How many things on the page are announced with exactly this label.
fn count(h: &Harness<'static, Option<LauncherUi>>, label: &str) -> usize {
    h.query_all_by_label(label).count()
}

fn app(s: &str) -> AppId {
    AppId::new(s).unwrap()
}

fn worn_by(store: &Shared, app_id: &str) -> Vec<String> {
    let rows = store.0.borrow_mut().rows().unwrap();
    rows.iter().filter(|r| r.worn.contains(&app(app_id))).map(|r| r.name.clone()).collect()
}

#[test]
fn the_shared_catalogue_is_called_add_ons_in_navigation() {
    let store = Shared::default();
    let mut h = harness(WithStore { store, picked: None });
    h.run();
    h.get_by_label("Add-ons").click();
    h.run();
    h.get_by_value("Themes 23");
    assert!(h.query_by_label("Extensions").is_none());
}

#[test]
fn a_build_with_no_store_has_no_extensions_page() {
    let mut h = harness(NoStore);
    h.run();
    assert!(h.query_by_label("Extensions").is_none());
    assert!(h.query_by_label("Apps").is_none());
}

#[test]
fn the_page_lists_every_theme_and_changes_nobodys_look_by_itself() {
    let store = Shared::default();
    let h = open(&store, None);
    // Added themes, and the apps' own built-in ones that share a name (LightCraft has a Midnight and a Paper).
    assert_eq!((count(&h, "Midnight"), count(&h, "Ocean"), count(&h, "Paper")), (2, 1, 2));
    h.get_by_label("Built into your apps");
    assert_eq!(count(&h, "Built into PhotoCraft"), 14);
    assert_eq!(count(&h, "Built into LightCraft"), 4);
    assert_eq!(count(&h, "Built into ArtCraft Launcher"), 2);
    for name in ["Pro (Dark)", "Soft Light", "Charcoal", "Silver", "Launcher Dark", "Launcher Light"] {
        assert_eq!(count(&h, name), 1, "{name}");
    }
    assert_eq!(count(&h, "Choose it inside PhotoCraft"), 14);
    assert_eq!(h.get_all_by_label("Made by ArtCraft").count(), 2);
    h.get_by_label("Added from a file");
    assert!(worn_by(&store, "photocraft").is_empty(), "opening the page chose nothing");
}

#[test]
fn choosing_a_theme_from_the_use_in_menu_makes_that_app_wear_it_and_clicking_again_gives_the_look_back() {
    let store = Shared::default();
    let mut h = open(&store, None);
    // Themes are listed by name: Midnight, Ocean, Paper.
    h.get_all_by_label("USE IN…").nth(1).unwrap().click();
    h.run();
    h.get_by_label("PhotoCraft").click();
    h.run();
    assert_eq!(worn_by(&store, "photocraft"), ["Ocean"]);
    // The theme is marked in use, and the chip says where.
    h.get_by_label("In use");
    h.get_by_label("PhotoCraft");

    h.get_all_by_label("USE IN…").nth(1).unwrap().click();
    h.run();
    // The chip and the menu's tick box share the name; the menu is drawn last.
    h.get_all_by_label("PhotoCraft").last().unwrap().click();
    h.run();
    assert!(worn_by(&store, "photocraft").is_empty());
}

#[test]
fn use_in_all_apps_makes_every_app_the_theme_is_made_for_wear_it() {
    let store = Shared::default();
    let mut h = open(&store, None);
    // Midnight is made for three apps; Ocean for one, so only two cards offer "All apps".
    assert_eq!(h.get_all_by_label("All apps").count(), 2);
    h.get_all_by_label("All apps").next().unwrap().click();
    h.run();
    for id in ["photocraft", "lightcraft", "pdfcraft"] {
        assert_eq!(worn_by(&store, id), ["Midnight"], "{id}");
    }
}

#[test]
fn removing_asks_first_and_then_removes_the_extension_from_the_list() {
    let store = Shared::default();
    let mut h = open(&store, None);
    h.get_by_value("Themes 23").click();
    h.run();
    h.get_by_label("Installed 3").click();
    h.run();
    h.get_all_by_label("Remove").nth(1).unwrap().click();
    h.run();
    h.get_by_label("Remove from every Craft app?");
    assert_eq!(store.0.borrow_mut().rows().unwrap().len(), 3, "asking removes nothing");
    h.get_by_label("Keep").click();
    h.run();
    assert!(h.query_by_label("Remove from every Craft app?").is_none());
    assert_eq!(store.0.borrow_mut().rows().unwrap().len(), 3);

    h.get_all_by_label("Remove").nth(1).unwrap().click();
    h.run();
    // The row asking for a yes now has the red Remove button, still second in the list.
    h.get_all_by_label("Remove").nth(1).unwrap().click();
    h.run_steps(3);
    let names: Vec<String> = store.0.borrow_mut().rows().unwrap().into_iter().map(|r| r.name).collect();
    assert_eq!(names, ["Midnight", "Paper"], "Ocean is the second row and went");
    h.get_by_label("Removed Ocean");
}

#[test]
fn a_file_that_cannot_be_added_says_why_in_words() {
    let store = Shared::default();
    let mut h = open(&store, Some(PathBuf::from("not-an-extension.zip")));
    h.get_by_label("Add from file").click();
    h.run_steps(3);
    h.get_by_label("Couldn't add that file: this sample list cannot add files");
}

#[test]
fn a_change_made_elsewhere_shows_up_within_a_few_seconds() {
    let store = Shared::default();
    let mut h = open(&store, None);
    h.get_by_label("Ocean");
    store.0.borrow_mut().remove(&ExtensionId::new("org.example.ocean").unwrap()).unwrap();
    for _ in 0..12 {
        h.step();
    }
    assert!(h.query_by_label("Ocean").is_none());
}

fn go_to(h: &mut Harness<'static, Option<LauncherUi>>, app: Option<&str>, kind: Option<&str>) {
    h.state_mut().as_mut().unwrap().open_extensions(app, kind);
    h.run_steps(3);
}

#[test]
fn another_app_sending_the_person_here_shows_only_what_works_with_it() {
    let store = Shared::default();
    let mut h = harness(WithStore { store: store.clone(), picked: None });
    h.run();
    assert!(h.query_by_label("Add from file").is_none(), "still on the apps page");
    go_to(&mut h, Some("lightcraft"), Some("theme"));
    assert_eq!((count(&h, "Midnight"), count(&h, "Paper")), (2, 2));
    assert!(h.query_by_label("Ocean").is_none(), "Ocean is only made for PhotoCraft");
    h.get_by_label("Charcoal");
    assert_eq!(count(&h, "Pro (Dark)") + count(&h, "Launcher Dark"), 0, "other apps' own themes are left out");
    h.get_by_label("Showing only works with LightCraft, themes");
    go_to(&mut h, Some("photocraft"), None);
    h.get_by_label("Ocean");
    h.get_by_label("Pro (Dark)");
    assert_eq!(count(&h, "Charcoal"), 0);
    h.get_by_label("Showing only works with PhotoCraft");
    assert!(worn_by(&store, "photocraft").is_empty(), "arriving chose nothing");
}

#[test]
fn show_all_lifts_the_narrowing_and_the_banner_goes() {
    let mut h = harness(WithStore { store: Shared::default(), picked: None });
    h.run();
    go_to(&mut h, Some("lightcraft"), None);
    assert!(h.query_by_label("Ocean").is_none());
    // Two added themes made for LightCraft and the four it carries; the Installed list holds added ones only.
    h.get_by_value("Themes 6");
    h.get_by_value("Themes 6").click();
    h.run();
    h.get_by_label("Installed 2");
    h.get_by_value("Themes 6").click();
    h.run();
    h.get_by_label("Show all").click();
    h.run_steps(3);
    h.get_by_label("Ocean");
    h.get_by_value(&format!("Themes {}", 3 + artcraft_launcher_ui_egui::features::extensions::builtin::all().len()));
    h.get_by_value(&format!("Themes {}", 3 + artcraft_launcher_ui_egui::features::extensions::builtin::all().len())).click();
    h.run();
    h.get_by_label("Installed 3");
    assert!(h.query_by_label("Show all").is_none());
    assert!(h.query_by_label_contains("Showing only").is_none());
}

#[test]
fn a_kind_other_than_themes_lands_on_the_installed_list_and_says_so() {
    let mut h = harness(WithStore { store: Shared::default(), picked: None });
    h.run();
    go_to(&mut h, None, Some("plugin"));
    h.get_by_label("Showing only plug-ins");
    h.get_by_label("Nothing added yet");
    assert!(h.query_by_label("Midnight").is_none());
    assert_eq!(count(&h, "Pro (Dark)") + count(&h, "Built into your apps"), 0, "built-in themes are themes, not plug-ins");
}

#[test]
fn a_place_with_nonsense_in_it_opens_the_page_unnarrowed() {
    let mut h = harness(WithStore { store: Shared::default(), picked: None });
    h.run();
    go_to(&mut h, Some("Photo Craft"), Some("nonsense"));
    assert_eq!((count(&h, "Midnight"), count(&h, "Ocean"), count(&h, "Paper")), (2, 1, 2));
    h.get_by_label("Pro (Dark)");
    assert!(h.query_by_label_contains("Showing only").is_none());
}

#[test]
fn a_build_with_no_store_ignores_a_request_to_open_the_page() {
    let mut h = harness(NoStore);
    h.run();
    go_to(&mut h, Some("photocraft"), None);
    assert!(h.query_by_label("Add from file").is_none());
    assert!(h.query_by_label("Extensions").is_none());
}

/// Options that fit side by side are never stacked: same top edge, one after the other.
#[test]
fn the_page_switch_and_the_tabs_sit_side_by_side() {
    let h = open(&Shared::default(), None);
    for (first, second) in [("Apps", "Add-ons"), ("Add-ons", "Themes 23")] {
        let rect = |label: &str| if label.starts_with("Themes") { h.get_by_value(label).rect() } else { h.get_by_label(label).rect() };
        let (a, b) = (rect(first), rect(second));
        assert!((a.top() - b.top()).abs() < 1.0, "{first} and {second} are on different lines: {a:?} {b:?}");
        assert!(b.left() >= a.right() - 1.0, "{second} should follow {first}: {a:?} {b:?}");
    }
}

/// Navigation and the list filter share one row even at the smallest supported window width.
#[test]
fn extension_navigation_and_filter_fit_narrow_windows() {
    let mut h = open(&Shared::default(), None);
    for width in [720.0, 800.0, 1180.0] {
        h.set_size(egui::vec2(width, 900.0));
        h.run_steps(3);
        let nav = h.get_by_label("Add-ons").rect();
        let filter = h.get_by_value("Themes 23").rect();
        let add = h.get_by_label("Add from file").rect();
        assert!((nav.top() - filter.top()).abs() < 2.0, "{width}: navigation and filter on different rows");
        assert!(nav.right() <= filter.left() + 1.0 && filter.right() <= add.left() + 1.0, "{width}: controls overlap");
        assert!(add.right() <= width, "{width}: Add from file clipped");
    }
}

#[test]
fn the_demo_list_has_every_kind_from_every_source_and_some_in_use() {
    use craft_ext_core::manifest::Kind;
    use craft_ext_core::tier::Tier;
    let rows = SampleSource::demo().rows().unwrap();
    let of = |k: Kind| rows.iter().filter(|r| r.kind == k).count();
    assert_eq!((of(Kind::Theme), of(Kind::Plugin), of(Kind::Resource)), (8, 4, 4), "an invalid sample id would drop a row silently");
    for tier in [Tier::FirstParty, Tier::FromFile, Tier::ReviewedThirdParty] {
        assert!(rows.iter().any(|r| r.tier == tier && r.kind != Kind::Theme), "{tier:?} has a plug-in or resource");
    }
    assert!(rows.iter().filter(|r| r.kind != Kind::Theme).all(|r| r.palette.is_none()));
    let worn: Vec<_> = rows.iter().flat_map(|r| r.worn.iter().map(AppId::as_str)).collect();
    assert_eq!(worn.len(), 3);
    assert!(rows.iter().any(|r| r.apps.iter().any(|a| !r.enabled.contains(a))), "some are switched off for an app");
    let ids: std::collections::BTreeSet<_> = rows.iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids.len(), rows.len());
}

#[test]
fn plug_ins_are_reachable_from_the_list_dropdown_without_showing_resources() {
    let store = Shared(Rc::new(RefCell::new(SampleSource::demo())));
    let mut h = open(&store, None);
    h.get_by_value("Themes 28").click();
    h.run();
    h.get_by_label("Plug-ins 4").click();
    h.run_steps(3);
    assert_eq!(count(&h, "Contact Sheet"), 2);
    assert_eq!(count(&h, "Batch Rename"), 3);
    assert!(h.query_by_label("Film Grain Textures").is_none());
    assert!(h.query_by_label("High Contrast").is_none());
    h.get_by_value("Plug-ins 4");
}

#[test]
fn plug_ins_are_grouped_by_app_and_a_shared_plugin_appears_in_each_app() {
    let store = Shared(Rc::new(RefCell::new(SampleSource::demo())));
    let mut h = open(&store, None);
    h.set_size(egui::vec2(1180.0, 1600.0));
    h.run_steps(3);
    h.get_by_value("Themes 28").click();
    h.run();
    h.get_by_label("Plug-ins 4").click();
    h.run_steps(3);
    let positions = ["LightCraft", "PdfCraft", "PhotoCraft"].map(|label| h.get_by_label(label).rect().top());
    assert!(positions[0] < positions[1] && positions[1] < positions[2], "app sections are ordered by name: {positions:?}");
    let batch: Vec<_> = h.get_all_by_label("Batch Rename").map(|node| node.rect().top()).collect();
    assert_eq!(batch.len(), 3, "a shared plug-in belongs to each compatible app section");
    assert!(positions.iter().zip(batch.iter()).all(|(section, plugin)| plugin > section));
    assert!(batch[0] < positions[1] && batch[1] < positions[2]);
    assert_eq!(count(&h, "Palette From Photo"), 1);
    assert_eq!(count(&h, "Contact Sheet"), 2);
}

#[test]
fn a_publishers_plugin_picture_is_shown_in_each_compatible_app_section() {
    let store = Shared(Rc::new(RefCell::new(SampleSource::demo())));
    let mut h = open(&store, None);
    h.get_by_value("Themes 28").click();
    h.run();
    h.get_by_label("Plug-ins 4").click();
    h.run_steps(3);
    assert_eq!(count(&h, "Thumbnail for Contact Sheet"), 2);
    assert_eq!(count(&h, "Thumbnail for Batch Rename"), 3);
}

#[test]
fn demo_plugins_are_marked_as_examples_not_installed_plugins() {
    let demo = Shared(Rc::new(RefCell::new(SampleSource::demo())));
    let h = open(&demo, None);
    h.get_by_label("Example data, not installed");
    let real = open(&Shared::default(), None);
    assert!(real.query_by_label("Example data, not installed").is_none());
}

#[test]
fn the_demo_list_fills_both_tabs() {
    let store = Shared(Rc::new(RefCell::new(SampleSource::demo())));
    let mut h = open(&store, None);
    h.get_by_value("Themes 28");
    h.get_by_value("Themes 28").click();
    h.run();
    h.get_by_label("Installed 16");
    for name in ["High Contrast", "Forest", "Sunset", "Rose Quartz", "Graphite"] {
        assert_eq!(count(&h, name), 1, "{name}");
    }
}

#[test]
fn a_theme_in_use_is_tagged_and_says_where_and_one_that_is_not_has_no_tag() {
    let store = Shared::default();
    let mut h = open(&store, None);
    assert_eq!(count(&h, "In use"), 0, "nothing is in use yet");
    assert_eq!(count(&h, "Not used yet"), 3);
    store.0.borrow_mut().wear(&app("photocraft"), Some(&ExtensionId::new("org.storytold.midnight").unwrap())).unwrap();
    store.0.borrow_mut().wear(&app("lightcraft"), Some(&ExtensionId::new("org.storytold.midnight").unwrap())).unwrap();
    store.0.borrow_mut().wear(&app("pdfcraft"), Some(&ExtensionId::new("org.storytold.paper").unwrap())).unwrap();
    h.run_steps(90);
    assert_eq!(count(&h, "In use"), 2, "Midnight and Paper, not Ocean");
    assert_eq!(count(&h, "Not used yet"), 1, "Ocean");
    assert_eq!((count(&h, "PhotoCraft"), count(&h, "LightCraft"), count(&h, "PdfCraft")), (1, 1, 1), "one chip per app, on its theme");
    // The same marks on the Installed list.
    h.get_by_value("Themes 23").click();
    h.run();
    h.get_by_label("Installed 3").click();
    h.run_steps(3);
    assert_eq!((count(&h, "PhotoCraft"), count(&h, "LightCraft"), count(&h, "PdfCraft")), (1, 1, 1));
    assert_eq!(count(&h, "In use"), 0, "the tag belongs on a picture, the list has chips");
    // Taking the theme back removes the marks.
    store.0.borrow_mut().wear(&app("photocraft"), None).unwrap();
    store.0.borrow_mut().wear(&app("lightcraft"), None).unwrap();
    store.0.borrow_mut().wear(&app("pdfcraft"), None).unwrap();
    h.run_steps(90);
    assert_eq!((count(&h, "PhotoCraft"), count(&h, "LightCraft"), count(&h, "PdfCraft")), (0, 0, 0));
}

#[test]
fn cards_have_a_gap_between_them_and_do_not_touch() {
    let h = open(&Shared::default(), None);
    let rect = |label: &str| h.query_all_by_label(label).next().unwrap_or_else(|| panic!("no {label}")).rect();
    let (a, b) = (rect("Preview of Midnight"), rect("Preview of Ocean"));
    assert!((a.top() - b.top()).abs() < 1.0, "the first two cards share a row");
    let gap = b.left() - a.right();
    assert!((15.0..=19.0).contains(&gap), "gap between cards is {gap}");
}

/// Nothing on a card may sit on top of anything else, in the busiest case: a theme worn by three apps.
#[test]
fn the_chips_never_touch_the_buttons_below_them() {
    let store = Shared::default();
    let mut h = open(&store, None);
    for app_id in ["photocraft", "lightcraft", "pdfcraft"] {
        store.0.borrow_mut().wear(&app(app_id), Some(&ExtensionId::new("org.storytold.midnight").unwrap())).unwrap();
    }
    h.run_steps(90);
    let rect = |label: &str| h.query_all_by_label(label).next().unwrap_or_else(|| panic!("no {label}")).rect();
    let chips = ["PhotoCraft", "LightCraft", "PdfCraft"].map(|n| h.query_all_by_label(n).next().unwrap().rect());
    let buttons = h.get_all_by_label("USE IN…").next().unwrap().rect();
    let badge = rect("Dark");
    for chip in chips {
        assert!(chip.bottom() + 8.0 <= buttons.top(), "chip {chip:?} is too close to the buttons {buttons:?}");
        assert!(chip.top() >= badge.bottom() + 6.0, "chip {chip:?} is too close to the badges {badge:?}");
    }
}

#[test]
fn apps_add_ons_and_refresh_are_separate_boxes_with_equal_gaps() {
    let store = Shared::default();
    let mut h = harness(WithStore { store, picked: None });
    h.run();
    let apps = h.get_by_label("Apps").rect();
    let add_ons = h.get_by_label("Add-ons").rect();
    let refresh = h.get_by_label("Check for updates").rect();
    let (first, second) = (add_ons.left() - apps.right(), refresh.left() - add_ons.right());
    assert!(first >= 4.0, "Apps and Add-ons must not touch: {apps:?} {add_ons:?}");
    assert!((first - second).abs() <= 1.0, "gaps differ: {first} vs {second}");
    assert!((apps.center().y - refresh.center().y).abs() <= 1.0 && (apps.height() - refresh.height()).abs() <= 1.0);
}

#[test]
fn the_list_offers_plug_ins_then_installed_then_themes() {
    let mut h = open(&Shared::default(), None);
    h.get_by_value("Themes 23").click();
    h.run();
    let top = |prefix: &str| h.query_all_by_label_contains(prefix).map(|n| n.rect().top()).fold(f32::INFINITY, f32::min);
    let (plugins, installed, themes) = (top("Plug-ins "), top("Installed "), top("Themes "));
    assert!(plugins < installed && installed < themes, "{plugins} {installed} {themes}");
}

#[test]
fn themes_and_plug_ins_can_be_cards_or_rows() {
    let previews = |h: &Harness<'static, Option<LauncherUi>>| h.query_all_by_label_contains("Preview of ").count();
    let mut h = open(&Shared(Rc::new(RefCell::new(SampleSource::demo()))), None);
    // Themes open as cards; the switch turns them into rows.
    assert!(previews(&h) > 0);
    h.get_by_label("Show add-ons as a list").click();
    h.run();
    assert_eq!(previews(&h), 0, "rows have no big pictures");
    h.get_by_label("Show add-ons as cards").click();
    h.run();
    assert!(previews(&h) > 0);
    // Plug-ins open as rows; the switch turns them into cards. (Once chosen, a view holds for every tab.)
    let mut h = open(&Shared(Rc::new(RefCell::new(SampleSource::demo()))), None);
    h.get_by_value("Themes 28").click();
    h.run();
    h.get_by_label_contains("Plug-ins ").click();
    h.run();
    assert_eq!(previews(&h), 0, "plug-ins open as rows");
    h.get_by_label("Show add-ons as cards").click();
    h.run();
    assert!(previews(&h) > 0, "plug-ins as cards");
}

#[test]
fn an_add_on_card_without_a_picture_starts_at_its_name_and_is_short() {
    let mut h = open(&Shared(Rc::new(RefCell::new(SampleSource::demo()))), None);
    h.get_by_value("Themes 28").click();
    h.run();
    h.get_by_label_contains("Installed ").click();
    h.run();
    h.get_by_label("Show add-ons as cards").click();
    h.run();
    // A resource has no picture: its details line sits right under its name, and its Remove button
    // is within a short card (a picture card is 336 tall).
    h.query_all_by_label_contains("Resource · ").next().unwrap().scroll_to_me();
    h.run();
    let details = h.query_all_by_label_contains("Resource · ").next().unwrap().rect();
    let remove = h.get_all_by_label("Remove").map(|n| n.rect()).find(|r| (r.top() - details.top()).abs() < 160.0 && r.top() > details.top()).unwrap();
    assert!(remove.bottom() - details.top() < 140.0, "{remove:?} {details:?}");
}
