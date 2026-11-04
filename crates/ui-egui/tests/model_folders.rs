//! The Model folders page driven the way a person would: open Set up AI, press Model folders, add
//! a folder (the proposed one or one they pick), and manage it. A fake port stands in for the disk;
//! the real one is covered in the desktop shell.

use std::cell::RefCell;
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event};
use artcraft_launcher_core::{Arch, Os, Platform};
use artcraft_launcher_ui_egui::features::{Clock, FolderCard, Install, Snapshot};
use artcraft_launcher_ui_egui::i18n::{Lang, set_test_lang};
use artcraft_launcher_ui_egui::theme::ThemeChoice;
use artcraft_launcher_ui_egui::{FeaturesPort, Host, LauncherUi, Settings};
use craft_ai_features::{Feature, builtin};
use craft_ai_model::{ConnectionId, ModelId};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

struct Quiet;
impl Backend for Quiet {
    fn send(&mut self, _: Command) {}
    fn poll(&mut self) -> Vec<Event> {
        Vec::new()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: false, reveal_label: "Show in Finder", self_update: false }
    }
    fn platform(&self) -> Option<Platform> {
        Some(Platform { os: Os::MacOs, arch: Arch::Arm64 })
    }
    fn now(&self) -> i64 {
        1_791_500_000
    }
}

#[derive(Default)]
struct World {
    folders: Vec<FolderCard>,
    suggested: Option<String>,
    /// What the folder picker answers.
    picked: Option<String>,
    picks: usize,
    /// Every call the page made, in order.
    calls: Vec<String>,
    refuse: Option<String>,
}

#[derive(Clone, Default)]
struct Port(Rc<RefCell<World>>);

impl Port {
    fn answer<T>(&self, call: String, ok: T) -> Result<T, String> {
        let mut w = self.0.borrow_mut();
        w.calls.push(call);
        w.refuse.clone().map_or(Ok(ok), Err)
    }
}

impl FeaturesPort for Port {
    fn features(&self) -> Vec<Feature> {
        builtin()
    }
    fn rescan(&mut self) {}
    fn refresh_tools(&mut self) {}
    fn snapshot(&mut self) -> Snapshot {
        Snapshot::default()
    }
    fn install(&mut self, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn cancel_install(&mut self) {}
    fn choose(&mut self, _: &str, _: Option<(ConnectionId, ModelId)>) -> Result<(), String> {
        Ok(())
    }
    fn test(&mut self, _: &str, _: bool) {}
    fn cancel_test(&mut self) {}
    fn dismiss_test(&mut self) {}
    fn history(&mut self) -> Result<craft_ai_model::HistoryLog, String> {
        Ok(craft_ai_model::HistoryLog::default())
    }
    fn clear_history(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn export_history(&mut self, _: &str) -> Result<String, String> {
        Ok(String::new())
    }
    fn clock(&self) -> Clock {
        Clock::default()
    }
    fn model_folders(&mut self) -> Vec<FolderCard> {
        self.0.borrow().folders.clone()
    }
    fn suggested_model_folder(&self) -> Option<String> {
        self.0.borrow().suggested.clone()
    }
    fn add_model_folder(&mut self, path: &str, create: bool) -> Result<String, String> {
        let said = self.answer(format!("add {path} create={create}"), "Found 2 model files: 2 vae.".to_owned())?;
        let mut w = self.0.borrow_mut();
        let default = w.folders.is_empty();
        w.folders.push(card("new", path, default));
        Ok(said)
    }
    fn rescan_model_folder(&mut self, id: &str) -> Result<String, String> {
        self.answer(format!("rescan {id}"), "Found 5 model files.".to_owned())
    }
    fn set_default_model_folder(&mut self, id: &str) -> Result<(), String> {
        self.answer(format!("default {id}"), ())
    }
    fn remove_model_folder(&mut self, id: &str) -> Result<(), String> {
        self.answer(format!("remove {id}"), ())?;
        self.0.borrow_mut().folders.retain(|f| f.id != id);
        Ok(())
    }
}

fn card(id: &str, path: &str, default: bool) -> FolderCard {
    FolderCard {
        id: id.into(),
        label: id.to_uppercase(),
        path: path.into(),
        default,
        kinds: vec![("checkpoints".into(), 2), ("vae".into(), 1)],
        ..FolderCard::default()
    }
}

struct Shell(Port);

impl Host for Shell {
    fn features(&mut self) -> Option<&mut dyn FeaturesPort> {
        Some(&mut self.0)
    }
    fn pick_folder(&mut self, _: Option<&str>) -> Option<String> {
        let mut w = (self.0).0.borrow_mut();
        w.picks += 1;
        w.picked.clone()
    }
}

type State = (Option<LauncherUi>, Shell);

fn harness(port: Port) -> Harness<'static, State> {
    set_test_lang(Lang::EN);
    Harness::builder().with_size(egui::vec2(1180.0, 1400.0)).build_ui_state(
        |ui, state: &mut State| {
            let (ui_state, shell) = state;
            let l = ui_state.get_or_insert_with(|| {
                let s = Settings { notify: false, theme: ThemeChoice::Dark, ..Settings::default() };
                LauncherUi::new(ui.ctx(), Box::new(Quiet), s)
            });
            l.logic(&ui.ctx().clone(), shell);
            l.ui(ui, shell);
        },
        (None, Shell(port)),
    )
}

fn settle(h: &mut Harness<'_, State>) {
    for _ in 0..4 {
        h.step();
    }
}

fn press(h: &mut Harness<'_, State>, label: &str) {
    h.get_by_role_and_label(Role::Button, label).click();
    settle(h);
}

fn open_page(port: &Port) -> Harness<'static, State> {
    let mut h = harness(port.clone());
    settle(&mut h);
    h.get_by_label("Set up AI").click();
    settle(&mut h);
    press(&mut h, "Model folders");
    h
}

fn calls(port: &Port) -> Vec<String> {
    port.0.borrow().calls.clone()
}

#[test]
fn the_page_explains_that_files_are_never_touched_and_offers_the_proposed_folder() {
    let port = Port::default();
    port.0.borrow_mut().suggested = Some("C:\\Users\\me\\AppData\\Local\\ArtCraft\\models".into());
    let h = open_page(&port);
    h.get_by_label("No model folders yet");
    h.get_by_label_contains("nothing is moved, renamed or deleted");
    h.get_by_label("C:\\Users\\me\\AppData\\Local\\ArtCraft\\models");
    h.get_by_role_and_label(Role::Button, "Use suggested folder");
    h.get_by_role_and_label(Role::Button, "Choose a folder");
}

#[test]
fn accepting_the_proposal_adds_it_with_create_and_says_what_was_found() {
    let port = Port::default();
    port.0.borrow_mut().suggested = Some("/models".into());
    let mut h = open_page(&port);
    press(&mut h, "Use suggested folder");
    assert_eq!(calls(&port), ["add /models create=true"]);
    h.get_by_label("Found 2 model files: 2 vae.");
    h.get_by_label("Downloads go here");
    assert!(h.query_by_label("No model folders yet").is_none());
}

#[test]
fn choosing_a_folder_adds_what_the_picker_returns_and_cancelling_adds_nothing() {
    let port = Port::default();
    let mut h = open_page(&port);
    press(&mut h, "Choose a folder");
    assert_eq!((port.0.borrow().picks, calls(&port)), (1, vec![]), "a cancelled picker adds nothing");
    port.0.borrow_mut().picked = Some("D:\\Models".into());
    press(&mut h, "Choose a folder");
    assert_eq!(calls(&port), ["add D:\\Models create=false"]);
    h.get_by_label("D:\\Models");
}

#[test]
fn a_folder_card_shows_what_was_found_and_only_the_actions_that_make_sense() {
    let port = Port::default();
    {
        let mut w = port.0.borrow_mut();
        w.folders.push(card("main", "D:\\Models", true));
        w.folders.push(card("spare", "E:\\Spare", false));
        w.folders.push(FolderCard { read_only: true, ..card("comfy", "C:\\ComfyUI\\models", false) });
        w.folders.push(FolderCard { missing: true, ..card("gone", "F:\\Unplugged", false) });
    }
    let h = open_page(&port);
    h.get_by_label("Downloads go here");
    h.get_by_label("Read only, from ComfyUI");
    h.get_by_label("Not found");
    assert_eq!(h.query_all_by_label("checkpoints (2)  ·  vae").count(), 4);
    // main (default): Look again, Remove. spare: all three. comfy (read only): Look again.
    // gone (missing): Make default, Remove.
    assert_eq!(h.query_all_by_role_and_label(Role::Button, "Make default").count(), 2);
    assert_eq!(h.query_all_by_role_and_label(Role::Button, "Look again").count(), 3);
    assert_eq!(h.query_all_by_role_and_label(Role::Button, "Remove").count(), 3);
}

#[test]
fn make_default_look_again_and_remove_go_to_the_right_folder() {
    let port = Port::default();
    {
        let mut w = port.0.borrow_mut();
        w.folders.push(card("main", "D:\\Models", true));
        w.folders.push(card("spare", "E:\\Spare", false));
    }
    let mut h = open_page(&port);
    press(&mut h, "Make default");
    h.get_by_label("New downloads will go there.");
    assert_eq!(calls(&port), ["default spare"]);
    let buttons: Vec<_> = h.query_all_by_role_and_label(Role::Button, "Look again").collect();
    buttons[1].click();
    settle(&mut h);
    h.get_by_label("Found 5 model files.");
    assert_eq!(calls(&port).last().unwrap(), "rescan spare");
    let buttons: Vec<_> = h.query_all_by_role_and_label(Role::Button, "Remove").collect();
    buttons[0].click();
    settle(&mut h);
    h.get_by_label("Forgotten. Its files were not touched.");
    assert_eq!(calls(&port).last().unwrap(), "remove main");
    assert!(h.query_by_label("MAIN").is_none() && h.query_by_label("SPARE").is_some());
}

#[test]
fn a_refusal_is_shown_in_plain_words_and_the_page_stays_usable() {
    let port = Port::default();
    port.0.borrow_mut().suggested = Some("/models".into());
    port.0.borrow_mut().refuse = Some("that folder is already in the list".into());
    let mut h = open_page(&port);
    press(&mut h, "Use suggested folder");
    h.get_by_label("that folder is already in the list");
    h.get_by_role_and_label(Role::Button, "Choose a folder");
}

#[test]
fn back_returns_to_the_list_and_the_page_is_read_again_next_time() {
    let port = Port::default();
    port.0.borrow_mut().folders.push(card("main", "D:\\Models", true));
    let mut h = open_page(&port);
    h.get_by_label("MAIN");
    press(&mut h, "Back");
    h.get_by_label("Hosted services");
    port.0.borrow_mut().folders.push(card("later", "E:\\Later", false));
    press(&mut h, "Model folders");
    h.get_by_label("LATER");
}

#[test]
fn a_shell_with_no_folder_support_shows_an_empty_page_and_says_why_on_add() {
    struct Bare;
    impl FeaturesPort for Bare {
        fn features(&self) -> Vec<Feature> {
            Vec::new()
        }
        fn rescan(&mut self) {}
        fn refresh_tools(&mut self) {}
        fn snapshot(&mut self) -> Snapshot {
            Snapshot { install: Install::Idle, ..Snapshot::default() }
        }
        fn install(&mut self, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn cancel_install(&mut self) {}
        fn choose(&mut self, _: &str, _: Option<(ConnectionId, ModelId)>) -> Result<(), String> {
            Ok(())
        }
        fn test(&mut self, _: &str, _: bool) {}
        fn cancel_test(&mut self) {}
        fn dismiss_test(&mut self) {}
        fn history(&mut self) -> Result<craft_ai_model::HistoryLog, String> {
            Ok(craft_ai_model::HistoryLog::default())
        }
        fn clear_history(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn export_history(&mut self, _: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn clock(&self) -> Clock {
            Clock::default()
        }
    }
    let mut bare = Bare;
    assert!(bare.model_folders().is_empty() && bare.suggested_model_folder().is_none());
    assert!(bare.add_model_folder("/x", false).is_err());
    assert!(bare.rescan_model_folder("a").is_err() && bare.set_default_model_folder("a").is_err() && bare.remove_model_folder("a").is_err());
}
