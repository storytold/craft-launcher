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
    h.get_by_label("Extensions").click();
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
    h.get_by_label("In use: PhotoCraft");

    h.get_all_by_label("USE IN…").nth(1).unwrap().click();
    h.run();
    h.get_by_label("PhotoCraft").click();
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
    h.get_by_label("Themes 6");
    h.get_by_label("Installed 2");
    h.get_by_label("Show all").click();
    h.run_steps(3);
    h.get_by_label("Ocean");
    h.get_by_label(&format!("Themes {}", 3 + artcraft_launcher_ui_egui::features::extensions::builtin::all().len()));
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
    for (first, second) in [("Apps", "Extensions"), ("Themes 23", "Installed 3")] {
        let rect = |label: &str| h.get_all_by_label(label).next().unwrap_or_else(|| panic!("no {label}")).rect();
        let (a, b) = (rect(first), rect(second));
        assert!((a.top() - b.top()).abs() < 1.0, "{first} and {second} are on different lines: {a:?} {b:?}");
        assert!(b.left() >= a.right() - 1.0, "{second} should follow {first}: {a:?} {b:?}");
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
fn the_demo_list_fills_both_tabs() {
    let store = Shared(Rc::new(RefCell::new(SampleSource::demo())));
    let h = open(&store, None);
    h.get_by_label("Themes 28");
    h.get_by_label("Installed 16");
    for name in ["High Contrast", "Forest", "Sunset", "Rose Quartz", "Graphite"] {
        assert_eq!(count(&h, name), 1, "{name}");
    }
}
