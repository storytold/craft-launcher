use std::cell::RefCell;
use std::rc::Rc;

use craft_ai_features::{Feature, Plan, State};
use craft_ai_model::{ConnectionId, ModelId};

use super::*;

#[derive(Default)]
struct Calls {
    scans: usize,
    refreshes: usize,
    chosen: Vec<(String, Option<(ConnectionId, ModelId)>)>,
    installs: Vec<(String, bool)>,
    cancels: usize,
    refuse: Option<String>,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<Calls>>);

impl FeaturesPort for Fake {
    fn features(&self) -> Vec<Feature> {
        craft_ai_features::builtin()
    }
    fn rescan(&mut self) {
        self.0.borrow_mut().scans += 1;
    }
    fn refresh_tools(&mut self) {
        self.0.borrow_mut().refreshes += 1;
    }
    fn snapshot(&mut self) -> Snapshot {
        Snapshot::default()
    }
    fn choose(&mut self, feature: &str, choice: Option<(ConnectionId, ModelId)>) -> Result<(), String> {
        let mut calls = self.0.borrow_mut();
        calls.chosen.push((feature.to_owned(), choice));
        calls.refuse.clone().map_or(Ok(()), Err)
    }
    fn test(&mut self, _: &str, _: bool) {}
    fn cancel_test(&mut self) {}
    fn dismiss_test(&mut self) {}
    fn install(&mut self, feature: &str, accepted: bool) -> Result<(), String> {
        let mut calls = self.0.borrow_mut();
        calls.installs.push((feature.to_owned(), accepted));
        calls.refuse.clone().map_or(Ok(()), Err)
    }
    fn cancel_install(&mut self) {
        self.0.borrow_mut().cancels += 1;
    }
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

fn plan_of(feature: &str, state: State) -> Plan {
    Plan { feature: feature.into(), state, missing_files: vec![], missing_packs: vec![], download_bytes: 0, licences: vec![], blockers: vec![] }
}

#[test]
fn opening_loads_the_features_and_asks_for_one_scan() {
    let mut port = Fake::default();
    let mut ui = FeaturesUi::default();
    ui.open();
    ui.ensure_loaded(&mut port);
    ui.ensure_loaded(&mut port);
    assert_eq!(ui.features.len(), 7);
    assert_eq!(port.0.borrow().scans, 1, "a scan per opening, not per frame");
    assert_eq!(port.0.borrow().refreshes, 1, "and one re-read of the model choices");
    ui.open();
    ui.ensure_loaded(&mut port);
    assert_eq!(port.0.borrow().scans, 2);
}

#[test]
fn setup_starts_unaccepted_and_the_choice_is_passed_on() {
    let mut port = Fake::default();
    let mut ui = FeaturesUi { accepted: true, ..FeaturesUi::default() };
    ui.begin_setup("select-subject");
    assert!(!ui.accepted, "a licence agreed for one tool is not agreed for the next");
    ui.start_install("select-subject", &mut port);
    ui.accepted = true;
    ui.start_install("select-subject", &mut port);
    assert_eq!(port.0.borrow().installs, [("select-subject".to_owned(), false), ("select-subject".to_owned(), true)]);
}

#[test]
fn a_refusal_from_the_shell_is_shown_and_clears_when_it_works() {
    let mut port = Fake::default();
    port.0.borrow_mut().refuse = Some("Accept the licence first.".into());
    let mut ui = FeaturesUi::default();
    ui.start_install("select-subject", &mut port);
    assert_eq!(ui.notice, Some(("Accept the licence first.".to_owned(), true)));
    port.0.borrow_mut().refuse = None;
    ui.start_install("select-subject", &mut port);
    assert_eq!(ui.notice, None);
}

#[test]
fn going_back_returns_to_the_list_and_drops_the_notice() {
    let mut ui = FeaturesUi::default();
    ui.begin_setup("select-subject");
    ui.notice = Some(("x".into(), true));
    ui.back();
    assert_eq!(ui.page, Page::Home);
    assert!(ui.notice.is_none());
}

#[test]
fn a_plan_is_found_by_feature_id() {
    let ui = FeaturesUi::default();
    let snap = Snapshot { plans: vec![plan_of("a", State::Ready), plan_of("b", State::NeedsInstall)], ..Snapshot::default() };
    assert_eq!(ui.plan_for(&snap, "b").map(|p| p.state.clone()), Some(State::NeedsInstall));
    assert!(ui.plan_for(&snap, "c").is_none());
}

#[test]
fn a_choice_is_passed_on_and_a_refusal_is_shown_then_cleared() {
    let mut port = Fake::default();
    let mut ui = FeaturesUi::default();
    let pick = Some((ConnectionId::new("c").unwrap(), ModelId::new("m").unwrap()));
    port.0.borrow_mut().refuse = Some("That model cannot do this job.".into());
    ui.choose("generate-image", pick.clone(), &mut port);
    assert_eq!(ui.notice, Some(("That model cannot do this job.".to_owned(), true)));
    port.0.borrow_mut().refuse = None;
    ui.choose("generate-image", None, &mut port);
    assert_eq!(ui.notice, None);
    assert_eq!(port.0.borrow().chosen, [("generate-image".to_owned(), pick), ("generate-image".to_owned(), None)]);
}

#[test]
fn the_options_come_from_the_features_and_going_back_asks_for_a_fresh_look() {
    let mut port = Fake::default();
    let mut ui = FeaturesUi::default();
    ui.ensure_loaded(&mut port);
    assert!(ui.options.iter().any(|o| o.id == "service:openai"));
    assert!(ui.options.iter().any(|o| o.id == "local:select-subject"));
    assert_eq!(port.0.borrow().refreshes, 1);
    ui.begin_setup("select-subject");
    assert_eq!(ui.page, Page::Local("select-subject".to_owned()));
    ui.back();
    assert_eq!(ui.page, Page::Home);
    ui.ensure_loaded(&mut port);
    assert_eq!(port.0.borrow().refreshes, 2, "what was set up may have switched tools on");
    ui.ensure_loaded(&mut port);
    assert_eq!(port.0.borrow().refreshes, 2, "but only once");
    ui.open();
    assert_eq!(ui.page, Page::Home);
}

#[test]
fn a_sample_knows_which_tool_it_belongs_to() {
    assert_eq!(Sample::Idle.feature(), None);
    for sample in [
        Sample::Running { feature: "a".into() },
        Sample::Passed { feature: "a".into(), summary: String::new(), millis: 0 },
        Sample::Failed { feature: "a".into(), message: String::new() },
        Sample::NeedsConsent {
            feature: "a".into(),
            connection: String::new(),
            destination: String::new(),
            items: vec![],
            billing: craft_ai_model::Billing::Free,
        },
    ] {
        assert_eq!(sample.feature(), Some("a"));
    }
}
