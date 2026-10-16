use craft_ai_model::history::{EndRecord, HISTORY_SCHEMA, StartRecord};
use craft_ai_model::{HistoryRecord, Locality, Outcome};
use craft_ai_native::HistoryFile;

use crate::history_port::DesktopHistory;

fn start(run: &str, at_ms: u64) -> HistoryRecord {
    HistoryRecord::Start(StartRecord {
        v: HISTORY_SCHEMA,
        run: run.into(),
        at_ms,
        app: "photocraft".into(),
        role_id: "fill".into(),
        role_label: "Generative Fill".into(),
        capability: "image.inpaint".into(),
        connection: "c".into(),
        connection_label: "Hosted".into(),
        model: "flux".into(),
        locality: Locality::Remote,
        destination: "https://api.example.com".into(),
        sent: Vec::new(),
        params: Vec::new(),
        provider_job: None,
        estimate: None,
        prompt: None,
    })
}

fn end(run: &str, at_ms: u64) -> HistoryRecord {
    HistoryRecord::End(EndRecord { v: HISTORY_SCHEMA, run: run.into(), at_ms, outcome: Outcome::Done, usage: None })
}

fn rig() -> (tempfile::TempDir, DesktopHistory) {
    let dir = tempfile::tempdir().unwrap();
    let history = DesktopHistory::at(HistoryFile::new(dir.path().join("jobs.jsonl")), dir.path().join("downloads"));
    (dir, history)
}

#[test]
fn what_the_apps_logged_is_read_back_newest_first_and_clear_empties_it() {
    let (dir, history) = rig();
    let file = HistoryFile::new(dir.path().join("jobs.jsonl"));
    for record in [start("a", 1_000), end("a", 3_000), start("b", 9_000)] {
        file.append(&record).unwrap();
    }
    let log = history.read().unwrap();
    let runs: Vec<_> = log.entries.iter().map(|e| e.start.run.as_str()).collect();
    assert_eq!(runs, ["b", "a"]);
    assert_eq!(log.entries[1].duration_ms(), Some(2_000));
    history.clear().unwrap();
    assert!(history.read().unwrap().entries.is_empty());
}

#[test]
fn no_log_yet_is_an_empty_history_not_an_error() {
    let (_dir, history) = rig();
    assert!(history.read().unwrap().entries.is_empty());
}

#[test]
fn an_export_is_a_new_file_each_time_and_never_overwrites() {
    let (dir, history) = rig();
    let first = history.export("a,b\r\n").unwrap();
    let second = history.export("c,d\r\n").unwrap();
    assert_ne!(first, second);
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "a,b\r\n");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "c,d\r\n");
    assert!(first.starts_with(&dir.path().join("downloads").display().to_string()));
    assert!(first.contains("craft-ai-history-") && first.ends_with(".csv"), "{first}");
}

#[test]
fn the_clock_is_now_with_a_plausible_offset() {
    let (_dir, history) = rig();
    let clock = history.clock();
    assert!(clock.now_ms > 1_700_000_000_000, "{}", clock.now_ms);
    assert!((-14 * 60..=14 * 60).contains(&clock.offset_minutes), "{}", clock.offset_minutes);
}
