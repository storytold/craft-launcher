//! The Tools tab's shell side against a real store on disk and fake providers: choosing, refusing,
//! sampling, asking first, cancelling, and having no store at all.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use artcraft_launcher_ui_egui::FeaturesPort;
use artcraft_launcher_ui_egui::features::{Sample, Snapshot, ToolStatus};
use craft_ai_client::{AiClient, ProviderFactory, ToolDesk};
use craft_ai_model::job::{Event, Output};
use craft_ai_model::{
    AiError, Billing, Connection, ConnectionId, JobId, JobSnapshot, JobState, ModelId, ModelInfo, Payload, Provider, Request, SecretRef,
    SecretString, StoreFile,
};
use craft_ai_native::ConsentFile;
use craft_ai_native::secrets::{MemoryStore, SecretStore};
use craft_ai_native::shared::SharedStore;
use serde_json::json;

use craft_ai_comfy_env::DownloadConfig;

use super::Tools;
use crate::features::DesktopFeatures;

/// Answers by its connection's name: `good-…` returns a picture, `empty-…` nothing, `slow-…` stays
/// running until `release` is set.
struct Fake {
    how: String,
    release: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

impl Provider for Fake {
    fn models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(Vec::new())
    }
    fn submit(&self, _: &ModelId, _: &Request) -> Result<JobId, AiError> {
        JobId::new("job-1").map_err(|e| AiError::invalid(e.to_string()))
    }
    fn poll(&self, _: &JobId) -> Result<JobSnapshot, AiError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Ok(JobSnapshot::new(JobState::Cancelled));
        }
        if self.how == "slow" && !self.release.load(Ordering::SeqCst) {
            return Ok(JobSnapshot::new(JobState::Running { percent: None }));
        }
        let mut snap = JobSnapshot::new(JobState::Succeeded);
        if self.how != "empty" {
            let payload = Payload::inline("image/png", vec![1, 2, 3]).map_err(|e| AiError::invalid(e.to_string()))?;
            snap.events.push(Event::Output(Output { payload, label: None }));
        }
        Ok(snap)
    }
    fn cancel(&self, _: &JobId) -> Result<(), AiError> {
        self.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct World {
    _dir: tempfile::TempDir,
    port: DesktopFeatures,
    release: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    sent: Arc<Mutex<usize>>,
}

fn connection(id: &str, remote: bool, models: &[&str]) -> Connection {
    let models: Vec<_> = models.iter().filter_map(|m| craft_ai_openai::catalog::classify(m)).collect();
    serde_json::from_value(json!({
        "id": id, "label": format!("Connection {id}"), "provider": "fake-image",
        "endpoint": if remote { "https://api.example.com/v1" } else { "http://127.0.0.1:9/v1" },
        "locality": if remote { "remote" } else { "local" },
        "auth": if remote { json!({"kind":"api_key","secret": format!("{id}-key")}) } else { json!({"kind":"none"}) },
        "billing": if remote { "metered" } else { "free" },
        "enabled": true,
        "models": models,
    }))
    .unwrap()
}

fn world(connections: Vec<Connection>) -> World {
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedStore::new(dir.path().join("connections.json"));
    shared.replace(&StoreFile { connections: connections.clone(), ..StoreFile::default() }).unwrap();
    let secrets = Arc::new(MemoryStore::new());
    for c in &connections {
        if let Some(r) = c.auth.secret_ref() {
            secrets.set(&SecretRef::new(r.as_str()).unwrap(), &SecretString::new("k")).unwrap();
        }
    }
    let (release, cancelled, sent) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(0)));
    let (r, c, s) = (Arc::clone(&release), Arc::clone(&cancelled), Arc::clone(&sent));
    let mut factory = ProviderFactory::standard();
    factory.register("fake-image", move |conn, _| {
        *s.lock().unwrap() += 1;
        let how = conn.id.as_str().split('-').next().unwrap_or("").to_owned();
        Ok(Arc::new(Fake { how, release: Arc::clone(&r), cancelled: Arc::clone(&c) }))
    });
    let client = AiClient::new(ToolDesk::app().unwrap(), shared.clone(), secrets).with_factory(factory);
    let desk = ToolDesk::new(client, shared, Some(ConsentFile::new(dir.path().join("consent.json"))));
    let config = DownloadConfig::default();
    let port = DesktopFeatures::with(craft_ai_features::builtin(), Arc::new(|| None), config, Tools::new(Ok(desk)));
    World { _dir: dir, port, release, cancelled, sent }
}

fn until(port: &mut DesktopFeatures, what: &str, done: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let snap = port.snapshot();
        if done(&snap) {
            return snap;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}: {:?}", snap.sample);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn tool<'a>(snap: &'a Snapshot, id: &str) -> &'a artcraft_launcher_ui_egui::features::ToolState {
    snap.tools.iter().find(|t| t.feature == id).unwrap_or_else(|| panic!("no tool {id}"))
}

fn cid(s: &str) -> ConnectionId {
    ConnectionId::new(s).unwrap()
}

fn image_choice(snap: &Snapshot) -> (ConnectionId, ModelId) {
    let c = &tool(snap, "generate-image").choices[0];
    (c.connection.clone(), c.model.clone())
}

fn ready_world(connection_name: &str, remote: bool) -> (World, Snapshot) {
    let mut w = world(vec![connection(connection_name, remote, &["gpt-image-1", "gpt-4o"])]);
    w.port.refresh_tools();
    let snap = until(&mut w.port, "the tools", |s| !s.tools.is_empty());
    let pick = image_choice(&snap);
    w.port.choose("generate-image", Some(pick)).unwrap();
    let snap = w.port.snapshot();
    (w, snap)
}

#[test]
fn the_tools_list_only_models_that_can_do_each_job() {
    let mut w = world(vec![connection("good-local", false, &["gpt-image-1", "gpt-4o"])]);
    w.port.refresh_tools();
    let snap = until(&mut w.port, "the tools", |s| s.tools.len() == 7);
    let image = tool(&snap, "generate-image");
    assert_eq!(image.choices.len(), 1);
    assert!(image.choices[0].local);
    assert!(matches!(image.status, ToolStatus::Ready { local: true, .. }), "a connected model switches its tool on by itself");
    assert!(image.can_test && !image.costly);
    assert!(tool(&snap, "generate-video").costly);
    assert!(tool(&snap, "generative-expand").choices.is_empty());
    assert!(!tool(&snap, "generative-expand").can_test);
}

#[test]
fn choosing_makes_the_tool_ready_and_automatic_takes_the_best_again() {
    let (mut w, snap) = ready_world("good-local", false);
    assert!(matches!(tool(&snap, "generate-image").status, ToolStatus::Ready { local: true, .. }));
    assert_eq!(tool(&snap, "generate-image").bound, Some(image_choice(&snap)));
    w.port.choose("generate-image", None).unwrap();
    let again = w.port.snapshot();
    assert_eq!(tool(&again, "generate-image").bound, Some(image_choice(&again)), "Automatic picks the same one again");
}

#[test]
fn a_connection_added_later_switches_its_tools_on_at_the_next_look() {
    let mut w = world(vec![]);
    w.port.refresh_tools();
    let snap = until(&mut w.port, "the tools", |s| s.tools.len() == 7);
    assert_eq!(tool(&snap, "generate-image").status, ToolStatus::NoChoice);
    // The person connects something in the launcher, then the screen looks again.
    let desk_path = w._dir.path().join("connections.json");
    let shared = SharedStore::new(desk_path);
    shared.update(|s| s.connections.push(connection("good-local", false, &["gpt-image-1"]))).unwrap();
    w.port.refresh_tools();
    let snap = until(&mut w.port, "the tool to switch on", |s| {
        s.tools.iter().any(|t| t.feature == "generate-image" && matches!(t.status, ToolStatus::Ready { .. }))
    });
    assert!(tool(&snap, "generate-image").bound.is_some());
}

#[test]
fn a_model_that_cannot_do_the_job_is_refused_with_a_reason() {
    let mut w = world(vec![connection("good-local", false, &["gpt-image-1", "gpt-4o"])]);
    let err = w.port.choose("generate-image", Some((cid("good-local"), ModelId::new("gpt-4o").unwrap()))).unwrap_err();
    assert!(!err.is_empty());
    assert!(w.port.choose("no-such-tool", None).unwrap_err().contains("not known"));
}

#[test]
fn a_local_sample_runs_and_reports_success() {
    let (mut w, _) = ready_world("good-local", false);
    w.port.test("generate-image", false);
    let snap = until(&mut w.port, "the sample", |s| matches!(s.sample, Sample::Passed { .. } | Sample::Failed { .. }));
    match snap.sample {
        Sample::Passed { feature, summary, .. } => {
            assert_eq!(feature, "generate-image");
            assert!(summary.contains("image"), "{summary}");
        }
        other => panic!("{other:?}"),
    }
    w.port.dismiss_test();
    assert_eq!(w.port.snapshot().sample, Sample::Idle);
}

#[test]
fn a_hosted_sample_asks_first_sending_nothing_and_runs_once_agreed() {
    let (mut w, _) = ready_world("good-remote", true);
    w.port.test("generate-image", false);
    let snap = until(&mut w.port, "the question", |s| matches!(s.sample, Sample::NeedsConsent { .. } | Sample::Failed { .. }));
    match &snap.sample {
        Sample::NeedsConsent { destination, connection, billing, items, .. } => {
            assert_eq!(destination, "https://api.example.com");
            assert_eq!(connection, "Connection good-remote");
            assert_eq!(*billing, Billing::Metered);
            assert_eq!(items, &["text 40 B"]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(*w.sent.lock().unwrap(), 0, "no provider was built, so nothing could have been sent");
    w.port.test("generate-image", true);
    until(&mut w.port, "the sample", |s| matches!(s.sample, Sample::Passed { .. }));
    // The agreement was recorded, so a second sample needs no question.
    w.port.dismiss_test();
    w.port.test("generate-image", false);
    until(&mut w.port, "the second sample", |s| matches!(s.sample, Sample::Passed { .. }));
}

#[test]
fn a_model_that_returns_nothing_fails_the_check() {
    let (mut w, _) = ready_world("empty-local", false);
    w.port.test("generate-image", false);
    let snap = until(&mut w.port, "the sample", |s| matches!(s.sample, Sample::Passed { .. } | Sample::Failed { .. }));
    match snap.sample {
        Sample::Failed { message, .. } => assert!(message.contains("returned nothing"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn cancelling_stops_the_job_and_a_late_answer_does_not_come_back() {
    let (mut w, _) = ready_world("slow-local", false);
    w.port.test("generate-image", false);
    until(&mut w.port, "the job to start", |_| *w.sent.lock().unwrap() == 1);
    std::thread::sleep(Duration::from_millis(50));
    w.port.cancel_test();
    assert_eq!(w.port.snapshot().sample, Sample::Idle);
    assert!(w.cancelled.load(Ordering::SeqCst), "the provider was told to stop");
    w.release.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(w.port.snapshot().sample, Sample::Idle, "the cancelled run's answer must not reappear");
}

#[test]
fn a_second_test_while_one_runs_is_ignored() {
    let (mut w, _) = ready_world("slow-local", false);
    w.port.test("generate-image", false);
    w.port.test("generate-image", false);
    until(&mut w.port, "the job to start", |_| *w.sent.lock().unwrap() >= 1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(*w.sent.lock().unwrap(), 1);
    w.port.cancel_test();
}

#[test]
fn changing_the_choice_drops_a_finished_sample_of_the_old_one() {
    let (mut w, _) = ready_world("good-local", false);
    w.port.test("generate-image", false);
    until(&mut w.port, "the sample", |s| matches!(s.sample, Sample::Passed { .. }));
    w.port.choose("generate-image", None).unwrap();
    assert_eq!(w.port.snapshot().sample, Sample::Idle);
}

#[test]
fn with_no_store_on_this_system_the_screen_is_told_why() {
    let mut port =
        DesktopFeatures::with(craft_ai_features::builtin(), Arc::new(|| None), DownloadConfig::default(), Tools::new(Err("no home folder".into())));
    port.refresh_tools();
    assert!(port.snapshot().tools.is_empty());
    assert_eq!(port.choose("generate-image", None).unwrap_err(), "no home folder");
    port.test("generate-image", false);
    match port.snapshot().sample {
        Sample::Failed { message, .. } => assert_eq!(message, "no home folder"),
        other => panic!("{other:?}"),
    }
}
