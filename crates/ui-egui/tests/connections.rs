//! The Connections screen driven the way a person would: open it from the header, add a connection,
//! test it, remove it. A fake port stands in for the keychain and the network; the real port is
//! covered where it lives, in the desktop shell.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event};
use artcraft_launcher_core::{Arch, Os, Platform};
use artcraft_launcher_ui_egui::i18n::{Lang, set_test_lang};
use artcraft_launcher_ui_egui::theme::ThemeChoice;
use artcraft_launcher_ui_egui::{ConnectionsPort, Failure, Host, KeyView, LauncherUi, Row, Settings, TestOutcome, TestTarget};
use craft_ai_model::{ConnectionDraft, ConnectionId, Locality, ModelId, ModelInfo};
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
    rows: Vec<Row>,
    saved: Vec<ConnectionDraft>,
    deleted: Vec<ConnectionId>,
    answer: Option<Result<TestOutcome, String>>,
    tests: usize,
}

#[derive(Clone, Default)]
struct Port(Rc<RefCell<World>>);

impl ConnectionsPort for Port {
    fn rows(&mut self) -> Result<Vec<Row>, String> {
        Ok(self.0.borrow().rows.clone())
    }
    fn save(&mut self, draft: &ConnectionDraft) -> Result<Option<String>, Failure> {
        let prepared = draft.prepare(&[]).map_err(|e| Failure { field: Some(e.field), message: e.message })?;
        let c = prepared.connection;
        let mut w = self.0.borrow_mut();
        w.saved.push(draft.clone());
        w.rows.push(Row {
            id: c.id.clone(),
            label: c.label.clone(),
            provider: c.provider.clone(),
            endpoint: c.endpoint.clone().unwrap_or_default(),
            local: c.locality == Locality::Local,
            key: if c.auth.secret_ref().is_some() { KeyView::Saved } else { KeyView::NotNeeded },
            models: c.models.len(),
            bindings: 0,
            enabled: true,
            draft: ConnectionDraft::from_connection(&c),
        });
        Ok(None)
    }
    fn delete(&mut self, id: &ConnectionId) -> Result<(), String> {
        let mut w = self.0.borrow_mut();
        w.deleted.push(id.clone());
        w.rows.retain(|r| &r.id != id);
        Ok(())
    }
    fn set_enabled(&mut self, _: &ConnectionId, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn start_test(&mut self, _: TestTarget) {
        let mut w = self.0.borrow_mut();
        w.tests += 1;
        let models = ["gpt-4o", "gpt-image-1"].map(|m| ModelInfo::new(ModelId::new(m).unwrap(), m)).to_vec();
        w.answer = Some(Ok(TestOutcome { models, millis: 87 }));
    }
    fn poll_test(&mut self) -> Option<Result<TestOutcome, String>> {
        self.0.borrow_mut().answer.take()
    }
}

struct Shell {
    port: Option<Port>,
}

impl Host for Shell {
    fn connections(&mut self) -> Option<&mut dyn ConnectionsPort> {
        self.port.as_mut().map(|p| p as &mut dyn ConnectionsPort)
    }
}

type State = (Option<LauncherUi>, Shell);

fn harness(port: Option<Port>) -> Harness<'static, State> {
    set_test_lang(Lang::EN);
    Harness::builder().with_size(egui::vec2(1180.0, 2000.0)).build_ui_state(
        |ui, state: &mut State| {
            let (ui_state, shell) = state;
            let l = ui_state.get_or_insert_with(|| {
                let s = Settings { notify: false, theme: ThemeChoice::Dark, ..Settings::default() };
                LauncherUi::new(ui.ctx(), Box::new(Quiet), s)
            });
            l.logic(&ui.ctx().clone(), shell);
            l.ui(ui, shell);
        },
        (None, Shell { port }),
    )
}

fn settle(h: &mut Harness<'_, State>) {
    for _ in 0..4 {
        h.step();
    }
}

#[test]
fn the_header_link_only_exists_where_the_shell_can_keep_connections() {
    let mut without = harness(None);
    settle(&mut without);
    assert!(without.query_by_label("Connections").is_none(), "the web build has no port");
    let mut with = harness(Some(Port::default()));
    settle(&mut with);
    with.get_by_label("Connections");
}

#[test]
fn add_test_and_remove_a_connection_from_the_screen() {
    let port = Port::default();
    let mut h = harness(Some(port.clone()));
    settle(&mut h);

    h.get_by_label("Connections").click();
    settle(&mut h);
    h.get_by_label("No connections yet.");

    h.get_by_label("Add connection").click();
    settle(&mut h);
    // Step one names the groups, so the choice explains where data goes.
    h.get_by_label("Add a provider");
    h.get_by_label("Hosted services");
    h.get_by_label("On this computer");
    assert!(h.query_by_label("Start from").is_none(), "no wall of buttons");
    h.get_by_role_and_label(Role::Button, "OpenAI").click();
    settle(&mut h);
    h.get_by_label("Connect OpenAI");
    h.get_by_label("Data sent to this connection leaves this computer.");
    // A hosted service asks for its key and tucks the rest under Details.
    assert!(h.query_by_label("Address").is_none(), "its address is fixed, so it is not asked for");

    // Saving without a key is refused on the form, not lost.
    h.get_by_label("Save").click();
    settle(&mut h);
    h.get_by_label_contains("Paste the key");

    h.get_by_role(Role::PasswordInput).focus();
    h.get_by_role(Role::PasswordInput).type_text("sk-test-123");
    settle(&mut h);
    h.get_by_label("Test").click();
    settle(&mut h);
    h.get_by_label_contains("Connected. Models found: 2");
    h.get_by_label("Save").click();
    settle(&mut h);

    {
        let w = port.0.borrow();
        assert_eq!(w.saved.len(), 1);
        assert_eq!(w.saved[0].key, "sk-test-123");
        assert_eq!(w.saved[0].models.len(), 2, "the models the test found are saved with it");
    }
    h.get_by_label("OpenAI");
    h.get_by_label("Leaves this computer");
    h.get_by_label("Key saved");

    h.get_by_label("Remove").click();
    settle(&mut h);
    h.get_by_label("Remove OpenAI?");
    h.get_by_label("Its saved key is deleted too.");
    h.get_by_label("Remove").click();
    settle(&mut h);
    assert_eq!(port.0.borrow().deleted.len(), 1);
    h.get_by_label("No connections yet.");
}

#[test]
fn a_local_preset_needs_no_key_and_says_nothing_leaves() {
    let port = Port::default();
    let mut h = harness(Some(port.clone()));
    settle(&mut h);
    h.get_by_label("Connections").click();
    settle(&mut h);
    h.get_by_label("Add connection").click();
    settle(&mut h);
    h.get_by_label("Ollama").click();
    settle(&mut h);
    h.get_by_label("Data sent to this connection stays on this computer.");
    assert!(h.query_by_role(Role::PasswordInput).is_none(), "no key box for a server that needs none");
    h.get_by_label("Save").click();
    settle(&mut h);
    h.get_by_label("This computer");
    h.get_by_label("No key needed");
    assert_eq!(port.0.borrow().saved.len(), 1);
}

#[test]
fn cancel_leaves_nothing_behind_and_closing_works() {
    let port = Port::default();
    let mut h = harness(Some(port.clone()));
    settle(&mut h);
    h.get_by_label("Connections").click();
    settle(&mut h);
    h.get_by_label("Add connection").click();
    settle(&mut h);
    h.get_by_label("Cancel").click();
    settle(&mut h);
    h.get_by_label("No connections yet.");
    assert!(port.0.borrow().saved.is_empty());
    let _ = Cell::new(0);
    h.get_by_label("Close").click();
    settle(&mut h);
    assert!(h.query_by_label("No connections yet.").is_none());
}
