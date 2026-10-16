//! The History page of the Set up AI window driven the way a person would: open it from the
//! window, filter, switch tables, export and clear. A fake port stands in for the log on disk.

use std::cell::RefCell;
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event};
use artcraft_launcher_core::{Arch, Os, Platform};
use artcraft_launcher_ui_egui::features::{Clock, Snapshot};
use artcraft_launcher_ui_egui::i18n::{Lang, set_test_lang};
use artcraft_launcher_ui_egui::theme::ThemeChoice;
use artcraft_launcher_ui_egui::{ConnectionsPort, Failure, FeaturesPort, Host, LauncherUi, Row, Settings, TestOutcome, TestTarget};
use craft_ai_features::{Feature, builtin};
use craft_ai_model::history::{EndRecord, HISTORY_SCHEMA, StartRecord, merge};
use craft_ai_model::job::Usage;
use craft_ai_model::{ConnectionDraft, ConnectionId, ErrorKind, HistoryLog, HistoryRecord, Locality, ModelId, Outcome};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

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

/// 2025-10-09T08:53:20Z.
const T0: u64 = 1_760_000_000_000;
const DAY: u64 = 86_400_000;

#[derive(Default)]
struct World {
    log: HistoryLog,
    fail_read: Option<String>,
    fail_export: Option<String>,
    fail_clear: Option<String>,
    exports: Vec<String>,
    clears: usize,
    reads: usize,
    now_ms: u64,
}

#[derive(Clone, Default)]
struct Features(Rc<RefCell<World>>);

impl FeaturesPort for Features {
    fn features(&self) -> Vec<Feature> {
        builtin()
    }
    fn rescan(&mut self) {}
    fn refresh_tools(&mut self) {}
    fn snapshot(&mut self) -> Snapshot {
        Snapshot::default()
    }
    fn choose(&mut self, _: &str, _: Option<(ConnectionId, ModelId)>) -> Result<(), String> {
        Ok(())
    }
    fn test(&mut self, _: &str, _: bool) {}
    fn cancel_test(&mut self) {}
    fn dismiss_test(&mut self) {}
    fn install(&mut self, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn cancel_install(&mut self) {}
    fn history(&mut self) -> Result<HistoryLog, String> {
        let mut w = self.0.borrow_mut();
        w.reads += 1;
        match &w.fail_read {
            Some(message) => Err(message.clone()),
            None => Ok(w.log.clone()),
        }
    }
    fn clear_history(&mut self) -> Result<(), String> {
        let mut w = self.0.borrow_mut();
        if let Some(message) = w.fail_clear.clone() {
            return Err(message);
        }
        w.clears += 1;
        w.log = HistoryLog::default();
        Ok(())
    }
    fn export_history(&mut self, csv: &str) -> Result<String, String> {
        let mut w = self.0.borrow_mut();
        if let Some(message) = w.fail_export.clone() {
            return Err(message);
        }
        w.exports.push(csv.to_owned());
        Ok("C:/Users/test/Downloads/craft-ai-history.csv".to_owned())
    }
    fn clock(&self) -> Clock {
        Clock { now_ms: self.0.borrow().now_ms, offset_minutes: 0 }
    }
}

#[derive(Clone, Default)]
struct Connections;

impl ConnectionsPort for Connections {
    fn rows(&mut self) -> Result<Vec<Row>, String> {
        Ok(Vec::new())
    }
    fn save(&mut self, _: &ConnectionDraft) -> Result<Option<String>, Failure> {
        Ok(None)
    }
    fn delete(&mut self, _: &ConnectionId) -> Result<(), String> {
        Ok(())
    }
    fn set_enabled(&mut self, _: &ConnectionId, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn start_test(&mut self, _: TestTarget) {}
    fn poll_test(&mut self) -> Option<Result<TestOutcome, String>> {
        None
    }
}

struct Shell {
    features: Features,
    connections: Connections,
}

impl Host for Shell {
    fn connections(&mut self) -> Option<&mut dyn ConnectionsPort> {
        Some(&mut self.connections)
    }
    fn features(&mut self) -> Option<&mut dyn FeaturesPort> {
        Some(&mut self.features)
    }
}

type State = (Option<LauncherUi>, Shell);

fn harness(features: Features) -> Harness<'static, State> {
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
        (None, Shell { features, connections: Connections }),
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

fn job(run: &str, at: u64, app: &str, model: &str, outcome: Option<Outcome>, cost: Option<u64>) -> Vec<HistoryRecord> {
    let mut records = vec![HistoryRecord::Start(StartRecord {
        v: HISTORY_SCHEMA,
        run: run.into(),
        at_ms: at,
        app: app.into(),
        role_id: "fill".into(),
        role_label: "Generative Fill".into(),
        capability: "image.inpaint".into(),
        connection: "c".into(),
        connection_label: "Hosted".into(),
        model: model.into(),
        locality: Locality::Remote,
        destination: "https://api.example.com".into(),
        sent: Vec::new(),
        params: Vec::new(),
        provider_job: None,
        estimate: None,
        prompt: None,
    })];
    if let Some(outcome) = outcome {
        records.push(HistoryRecord::End(EndRecord {
            v: HISTORY_SCHEMA,
            run: run.into(),
            at_ms: at + 2_500,
            outcome,
            usage: cost.map(|c| Usage { cost_micros: Some(c), currency: Some("USD".into()), ..Usage::default() }),
        }));
    }
    records
}

/// Three jobs: a priced success, a failure for lack of credit, and an unfinished one.
fn sample_log() -> HistoryLog {
    let mut records = job("a", T0, "photocraft", "flux", Some(Outcome::Done), Some(31_000));
    records.extend(job("b", T0 + 60_000, "wordcraft", "gpt-4o", Some(Outcome::Failed { kind: ErrorKind::Quota }), None));
    records.extend(job("c", T0 + 120_000, "photocraft", "flux", None, None));
    merge(records, 0)
}

fn world(log: HistoryLog) -> Features {
    let f = Features::default();
    {
        let mut w = f.0.borrow_mut();
        w.log = log;
        w.now_ms = T0 + 3_600_000;
    }
    f
}

fn open_history(f: &Features) -> Harness<'static, State> {
    let mut h = harness(f.clone());
    settle(&mut h);
    h.get_by_label("Set up AI").click();
    settle(&mut h);
    press(&mut h, "History");
    h
}

const DONE_ROW: &str = "2025-10-09 08:53, photocraft, Generative Fill, Done";

#[test]
fn the_page_opens_from_the_window_and_shows_the_numbers() {
    let f = world(sample_log());
    let h = open_history(&f);
    h.get_by_label("AI jobs run by your Craft apps");
    assert_eq!(f.0.borrow().reads, 1, "one read when the page opens");
    h.get_by_label("Reported cost");
    assert_eq!(h.get_all_by_label("0.03 USD").count(), 2, "in the stat strip and on the job row");
    h.get_by_label("Only costs the provider reported are counted.");
    h.get_by_label(DONE_ROW);
    h.get_by_label("2025-10-09 08:54, wordcraft, Generative Fill, Failed");
    h.get_by_label("2025-10-09 08:55, photocraft, Generative Fill, Not finished");
    h.get_by_label("Out of credit");
}

#[test]
fn an_empty_log_says_so_and_offers_neither_export_nor_clear() {
    let f = world(HistoryLog::default());
    let h = open_history(&f);
    h.get_by_label("No AI jobs yet. They appear here once a Craft app uses AI.");
    assert!(h.get_by_role_and_label(Role::Button, "Export CSV").accesskit_node().is_disabled());
    assert!(h.get_by_role_and_label(Role::Button, "Clear history").accesskit_node().is_disabled());
}

#[test]
fn a_log_that_cannot_be_read_shows_why_and_still_lets_you_go_back() {
    let f = world(sample_log());
    f.0.borrow_mut().fail_read = Some("Could not read the history: access denied".into());
    let mut h = open_history(&f);
    h.get_by_label("Could not read the history: access denied");
    press(&mut h, "Back");
    h.get_by_label("Switched on");
}

#[test]
fn a_range_with_nothing_in_it_says_no_jobs_match() {
    let f = world(sample_log());
    f.0.borrow_mut().now_ms = T0 + 40 * DAY;
    let mut h = open_history(&f);
    h.get_by_label(DONE_ROW);
    press(&mut h, "7 days");
    h.get_by_label("No jobs match these filters.");
    assert!(h.query_by_label(DONE_ROW).is_none());
    press(&mut h, "All time");
    h.get_by_label(DONE_ROW);
}

#[test]
fn the_day_and_month_tables_total_the_jobs() {
    let mut records = job("a", T0, "photocraft", "flux", Some(Outcome::Done), Some(10_000));
    records.extend(job("b", T0 + 60_000, "photocraft", "flux", Some(Outcome::Done), Some(20_000)));
    records.extend(job("c", T0 + 30 * DAY, "photocraft", "flux", Some(Outcome::Done), Some(5_000)));
    let f = world(merge(records, 0));
    let mut h = open_history(&f);
    press(&mut h, "By day");
    h.get_by_label("2025-11-08, 1");
    h.get_by_label("2025-10-09, 2");
    press(&mut h, "By month");
    h.get_by_label("2025-11, 1");
    h.get_by_label("2025-10, 2");
}

#[test]
fn export_hands_the_port_what_is_showing_and_says_where_it_went() {
    let f = world(sample_log());
    let mut h = open_history(&f);
    press(&mut h, "Export CSV");
    {
        let w = f.0.borrow();
        assert_eq!(w.exports.len(), 1);
        assert!(w.exports[0].starts_with("started_utc,app,feature"));
        assert_eq!(w.exports[0].matches("\r\n").count(), 4, "header and three jobs");
    }
    h.get_by_label("Saved to C:/Users/test/Downloads/craft-ai-history.csv");
}

#[test]
fn a_failed_export_shows_the_reason() {
    let f = world(sample_log());
    f.0.borrow_mut().fail_export = Some("There is no Downloads folder to save into.".into());
    let mut h = open_history(&f);
    press(&mut h, "Export CSV");
    h.get_by_label("There is no Downloads folder to save into.");
}

#[test]
fn clearing_needs_a_second_yes_and_then_empties_the_page() {
    let f = world(sample_log());
    let mut h = open_history(&f);
    press(&mut h, "Clear history");
    assert_eq!(f.0.borrow().clears, 0, "one press only asks");
    h.get_by_label("Delete the history for every app?");
    press(&mut h, "Keep it");
    assert_eq!(f.0.borrow().clears, 0);
    press(&mut h, "Clear history");
    press(&mut h, "Yes, delete it");
    assert_eq!(f.0.borrow().clears, 1);
    h.get_by_label("No AI jobs yet. They appear here once a Craft app uses AI.");
}

#[test]
fn a_clear_that_fails_keeps_the_jobs_and_says_why() {
    let f = world(sample_log());
    f.0.borrow_mut().fail_clear = Some("Could not clear the history: in use".into());
    let mut h = open_history(&f);
    press(&mut h, "Clear history");
    press(&mut h, "Yes, delete it");
    h.get_by_label("Could not clear the history: in use");
    h.get_by_label(DONE_ROW);
}

#[test]
fn refresh_reads_the_log_again() {
    let f = world(sample_log());
    let mut h = open_history(&f);
    f.0.borrow_mut().log = merge(job("z", T0 + 300_000, "filmcraft", "seedance", Some(Outcome::Done), None), 0);
    press(&mut h, "Refresh");
    assert_eq!(f.0.borrow().reads, 2);
    h.get_by_label("2025-10-09 08:58, filmcraft, Generative Fill, Done");
    assert!(h.query_by_label(DONE_ROW).is_none());
}

#[test]
fn the_chart_has_a_bar_for_each_day_that_says_what_happened_on_it() {
    let mut records = job("a", T0, "photocraft", "flux", Some(Outcome::Done), Some(31_000));
    records.extend(job("b", T0 + 60_000, "wordcraft", "gpt-4o", Some(Outcome::Failed { kind: ErrorKind::Quota }), None));
    // Two days later, so the day between must show as an empty bar rather than being skipped.
    records.extend(job("c", T0 + 2 * DAY, "photocraft", "flux", Some(Outcome::Done), None));
    let f = world(merge(records, 0));
    let h = open_history(&f);
    h.get_by_label("Jobs per day");
    h.get_by_label("2025-10-09, Done 1, Failed 1, 0.03 USD");
    h.get_by_label("2025-10-10, Done 0, Failed 0");
    h.get_by_label("2025-10-11, Done 1, Failed 0");
}

#[test]
fn the_model_and_app_tables_rank_by_how_busy_each_was() {
    let f = world(sample_log());
    let mut h = open_history(&f);
    press(&mut h, "By model");
    h.get_by_label("flux, 2");
    h.get_by_label("gpt-4o, 1");
    press(&mut h, "By app");
    h.get_by_label("photocraft, 2");
    h.get_by_label("wordcraft, 1");
}

#[test]
fn nothing_is_drawn_on_top_of_anything_else() {
    let f = world(sample_log());
    let h = open_history(&f);
    // The tab row sits below the whole overview band: its second stat row ends 140 px below the
    // band's top, and "Reported cost" is the label at the top of that second row.
    let cost_label = h.get_by_label("Reported cost").rect();
    let tabs = h.get_by_role_and_label(Role::Button, "Jobs 3").rect();
    assert!(tabs.top() >= cost_label.bottom() + 35.0, "tabs at {} overlap the stats ending near {}", tabs.top(), cost_label.bottom() + 35.0);
    let header = h.get_by_label("When").rect();
    assert!(header.top() >= tabs.bottom(), "the table header at {} overlaps the tabs ending at {}", header.top(), tabs.bottom());
    // Rows are one row height apart, so a row's cells cannot have dragged the next one up.
    let first = h.get_by_label("2025-10-09 08:55, photocraft, Generative Fill, Not finished").rect();
    let second = h.get_by_label("2025-10-09 08:54, wordcraft, Generative Fill, Failed").rect();
    assert!((second.top() - first.top() - 54.0).abs() < 0.6, "rows are {} apart", second.top() - first.top());
}
