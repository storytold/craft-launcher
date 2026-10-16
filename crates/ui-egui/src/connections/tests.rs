use std::cell::RefCell;
use std::rc::Rc;

use craft_ai_model::{ConnectionDraft, ConnectionId, Field, ModelId, ModelInfo, PRESETS, Preset};

use super::*;

#[derive(Default)]
struct World {
    rows: Vec<Row>,
    saved: Vec<ConnectionDraft>,
    deleted: Vec<ConnectionId>,
    toggled: Vec<(ConnectionId, bool)>,
    started: Vec<TestTarget>,
    result: Option<Result<TestOutcome, String>>,
    reloads: usize,
    fail_rows: bool,
    fail_save: Option<Failure>,
    save_warning: Option<String>,
    fail_delete: bool,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<World>>);

impl ConnectionsPort for Fake {
    fn rows(&mut self) -> Result<Vec<Row>, String> {
        let mut w = self.0.borrow_mut();
        w.reloads += 1;
        if w.fail_rows { Err("disk on fire".into()) } else { Ok(w.rows.clone()) }
    }
    fn save(&mut self, draft: &ConnectionDraft) -> Result<Option<String>, Failure> {
        let mut w = self.0.borrow_mut();
        if let Some(f) = w.fail_save.clone() {
            return Err(f);
        }
        w.saved.push(draft.clone());
        Ok(w.save_warning.clone())
    }
    fn delete(&mut self, id: &ConnectionId) -> Result<(), String> {
        let mut w = self.0.borrow_mut();
        if w.fail_delete {
            return Err("cannot delete".into());
        }
        w.deleted.push(id.clone());
        w.rows.retain(|r| &r.id != id);
        Ok(())
    }
    fn set_enabled(&mut self, id: &ConnectionId, enabled: bool) -> Result<(), String> {
        self.0.borrow_mut().toggled.push((id.clone(), enabled));
        Ok(())
    }
    fn start_test(&mut self, target: TestTarget) {
        self.0.borrow_mut().started.push(target);
    }
    fn poll_test(&mut self) -> Option<Result<TestOutcome, String>> {
        self.0.borrow_mut().result.take()
    }
}

fn preset(key: &str) -> &'static Preset {
    PRESETS.iter().find(|p| p.key == key).unwrap()
}

fn row(preset_key: &str, label: &str) -> Row {
    let p = preset(preset_key);
    let mut d = ConnectionDraft::from_preset(p);
    d.label = label.into();
    d.key = "sk-test".into();
    let c = d.prepare(&[]).unwrap().connection;
    Row {
        id: c.id.clone(),
        label: c.label.clone(),
        provider: c.provider.clone(),
        endpoint: c.endpoint.clone().unwrap_or_default(),
        local: c.locality == craft_ai_model::Locality::Local,
        key: KeyView::Saved,
        models: 0,
        bindings: 0,
        enabled: true,
        draft: ConnectionDraft::from_connection(&c),
    }
}

fn outcome(n: usize) -> TestOutcome {
    let models = (0..n).map(|i| ModelInfo::new(ModelId::new(format!("m{i}")).unwrap(), "M")).collect();
    TestOutcome { models, millis: 42 }
}

fn setup() -> (ConnectionsUi, Fake) {
    (ConnectionsUi::default(), Fake::default())
}

fn form(ui: &mut ConnectionsUi) -> &mut FormState {
    match &mut ui.screen {
        Screen::Form(f) => f,
        _ => panic!("expected the form"),
    }
}

#[test]
fn opening_reads_the_list_and_a_failure_is_shown_not_hidden() {
    let (mut ui, mut port) = setup();
    port.0.borrow_mut().rows = vec![row("openai", "OpenAI")];
    ui.reload(&mut port);
    assert_eq!(ui.rows.len(), 1);
    assert!(ui.notice.is_none());
    port.0.borrow_mut().fail_rows = true;
    ui.reload(&mut port);
    assert!(ui.rows.is_empty());
    assert!(ui.notice.as_ref().is_some_and(|(t, bad)| *bad && t.contains("disk on fire")));
}

#[test]
fn saving_a_new_connection_returns_to_a_refreshed_list() {
    let (mut ui, mut port) = setup();
    ui.begin_add(&PRESETS[0]);
    form(&mut ui).draft.key = "sk-abc".into();
    assert!(ui.submit(&mut port));
    assert!(matches!(ui.screen, Screen::List));
    assert_eq!(port.0.borrow().saved.len(), 1);
    assert_eq!(port.0.borrow().saved[0].key, "sk-abc");
    assert!(port.0.borrow().reloads >= 1, "the list was read again");
}

#[test]
fn a_refused_save_keeps_the_form_open_with_the_reason() {
    let (mut ui, mut port) = setup();
    port.0.borrow_mut().fail_save = Some(Failure { field: Some(Field::Key), message: "Paste the key.".into() });
    ui.begin_add(&PRESETS[0]);
    form(&mut ui).draft.key = "typed".into();
    assert!(!ui.submit(&mut port));
    let f = form(&mut ui);
    assert_eq!(f.error.as_ref().unwrap().field, Some(Field::Key));
    assert_eq!(f.draft.key, "typed", "what was typed is not lost");
}

#[test]
fn a_save_warning_is_shown_on_the_list() {
    let (mut ui, mut port) = setup();
    port.0.borrow_mut().save_warning = Some("old key left behind".into());
    ui.begin_add(preset("ollama"));
    assert!(ui.submit(&mut port));
    assert!(ui.notice.as_ref().is_some_and(|(t, bad)| *bad && t.contains("old key")));
}

#[test]
fn editing_starts_from_the_saved_connection() {
    let (mut ui, mut port) = setup();
    let r = row("openai", "Work");
    port.0.borrow_mut().rows = vec![r.clone()];
    ui.reload(&mut port);
    ui.begin_edit(&r.id);
    let f = form(&mut ui);
    assert_eq!(f.draft.editing.as_ref(), Some(&r.id));
    assert!(f.draft.key.is_empty(), "the saved key is never shown");
    ui.begin_edit(&ConnectionId::new("gone").unwrap());
    assert!(matches!(ui.screen, Screen::Form(_)), "an unknown id changes nothing");
}

#[test]
fn going_back_to_the_services_and_choosing_another_drops_the_previous_key() {
    let (mut ui, _port) = setup();
    ui.begin_pick();
    assert!(matches!(ui.screen, Screen::Pick));
    ui.begin_add(preset("openai"));
    form(&mut ui).draft.key = "sk-keep".into();
    ui.begin_pick();
    ui.begin_add(preset("ollama"));
    let f = form(&mut ui);
    assert_eq!(f.draft.provider, "ollama");
    assert!(f.draft.key.is_empty(), "a key must never follow the user to another service");
    assert_eq!(f.preset, Some("ollama"));
}

#[test]
fn every_service_is_in_exactly_one_group_by_where_its_data_goes() {
    use super::pick::{Group, group};
    let hosted: Vec<_> = PRESETS.iter().filter(|p| group(p) == Group::Hosted).map(|p| p.key).collect();
    let local: Vec<_> = PRESETS.iter().filter(|p| group(p) == Group::Local).map(|p| p.key).collect();
    let other: Vec<_> = PRESETS.iter().filter(|p| group(p) == Group::Other).map(|p| p.key).collect();
    assert_eq!(hosted, ["openai", "openrouter", "bfl", "seedance", "vertex-express", "artcraft-omni"]);
    assert_eq!(local, ["ollama", "lmstudio", "llamacpp", "comfyui"]);
    assert_eq!(other, ["custom"]);
    assert_eq!(hosted.len() + local.len() + other.len(), PRESETS.len());
    // A group must agree with what the preset says about itself.
    for p in PRESETS.iter().filter(|p| group(p) == Group::Local) {
        assert!(!p.needs_key && p.endpoint.contains("127.0.0.1"), "{}", p.key);
    }
}

#[test]
fn a_form_test_carries_the_models_it_found_onto_the_draft() {
    let (mut ui, mut port) = setup();
    ui.begin_add(preset("ollama"));
    ui.test_form(&mut port);
    assert!(ui.is_testing());
    assert_eq!(form(&mut ui).test, TestState::Running);
    assert!(matches!(port.0.borrow().started[0], TestTarget::Draft(_)));
    assert!(!ui.poll(&mut port), "nothing finished yet");
    port.0.borrow_mut().result = Some(Ok(outcome(3)));
    assert!(ui.poll(&mut port));
    assert!(!ui.is_testing());
    let f = form(&mut ui);
    assert_eq!(f.test, TestState::Passed { models: 3, millis: 42 });
    assert_eq!(f.draft.models.len(), 3);
}

#[test]
fn a_failed_form_test_says_why_and_adds_no_models() {
    let (mut ui, mut port) = setup();
    ui.begin_add(preset("ollama"));
    ui.test_form(&mut port);
    port.0.borrow_mut().result = Some(Err("could not reach the server".into()));
    ui.poll(&mut port);
    let f = form(&mut ui);
    assert_eq!(f.test, TestState::Failed("could not reach the server".into()));
    assert!(f.draft.models.is_empty());
}

#[test]
fn an_answer_that_arrives_after_the_form_closed_is_dropped() {
    let (mut ui, mut port) = setup();
    ui.begin_add(preset("ollama"));
    ui.test_form(&mut port);
    ui.back();
    port.0.borrow_mut().result = Some(Ok(outcome(5)));
    assert!(!ui.poll(&mut port));
    // A later test on a saved connection is not confused by the old one.
    let r = row("openai", "Work");
    ui.test_saved(&r.id, &mut port);
    assert!(ui.tested.get(&r.id) == Some(&TestState::Running));
}

#[test]
fn testing_a_saved_connection_records_the_result_and_refreshes_the_list() {
    let (mut ui, mut port) = setup();
    let r = row("openai", "Work");
    port.0.borrow_mut().rows = vec![r.clone()];
    ui.reload(&mut port);
    let before = port.0.borrow().reloads;
    ui.test_saved(&r.id, &mut port);
    assert!(matches!(port.0.borrow().started[0], TestTarget::Saved(_)));
    port.0.borrow_mut().result = Some(Ok(outcome(2)));
    ui.poll(&mut port);
    assert_eq!(ui.tested.get(&r.id), Some(&TestState::Passed { models: 2, millis: 42 }));
    assert!(port.0.borrow().reloads > before, "the remembered models show up");
    // A failure is recorded without a refresh.
    let n = port.0.borrow().reloads;
    ui.test_saved(&r.id, &mut port);
    port.0.borrow_mut().result = Some(Err("401".into()));
    ui.poll(&mut port);
    assert_eq!(ui.tested.get(&r.id), Some(&TestState::Failed("401".into())));
    assert_eq!(port.0.borrow().reloads, n);
}

#[test]
fn removing_goes_through_the_port_and_clears_what_was_known() {
    let (mut ui, mut port) = setup();
    let r = row("openai", "Work");
    port.0.borrow_mut().rows = vec![r.clone()];
    ui.reload(&mut port);
    ui.tested.insert(r.id.clone(), TestState::Running);
    ui.remove(&r.id, &mut port);
    assert_eq!(port.0.borrow().deleted, vec![r.id.clone()]);
    assert!(ui.rows.is_empty());
    assert!(ui.tested.is_empty());
    assert!(matches!(ui.screen, Screen::List));
}

#[test]
fn a_refused_remove_or_toggle_is_shown() {
    let (mut ui, mut port) = setup();
    let r = row("openai", "Work");
    port.0.borrow_mut().rows = vec![r.clone()];
    port.0.borrow_mut().fail_delete = true;
    ui.reload(&mut port);
    ui.remove(&r.id, &mut port);
    assert!(ui.notice.as_ref().is_some_and(|(t, bad)| *bad && t.contains("cannot delete")));
    assert_eq!(ui.rows.len(), 1, "still listed");
    ui.set_enabled(&r.id, false, &mut port);
    assert_eq!(port.0.borrow().toggled, vec![(r.id, false)]);
}

#[test]
fn reopening_starts_clean() {
    let (mut ui, mut port) = setup();
    ui.begin_add(&PRESETS[0]);
    ui.notice = Some(("old".into(), true));
    ui.open();
    assert!(matches!(ui.screen, Screen::List));
    assert!(ui.notice.is_none());
    port.0.borrow_mut().rows = vec![row("ollama", "Ollama")];
    ui.reload(&mut port);
    assert_eq!(ui.rows.len(), 1);
}
