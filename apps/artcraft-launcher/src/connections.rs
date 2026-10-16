//! The desktop shell's side of the Connections screen: the real store, the system keychain and the
//! network test, behind the UI's [`ConnectionsPort`].
//!
//! The screen itself never touches a key. Everything here goes through `ConnectionManager`, which
//! is also what an app's fallback form uses, so a connection added here is read by every Craft app
//! with no further setup. A test runs on its own thread (it may wait on a slow server) and the UI
//! polls for the answer; starting a new test makes an older answer that arrives late disappear.

use artcraft_launcher_ui_egui::{ConnectionsPort, Failure, KeyView, Row, TestOutcome, TestTarget};
use craft_ai_client::{ConnectionManager, KeyState, SaveError, TestRunner};
use craft_ai_model::{ConnectionDraft, ConnectionId, Locality};

pub struct DesktopConnections {
    manager: Result<ConnectionManager, String>,
    /// Present when there is a manager to test through.
    runner: Option<TestRunner>,
    /// A test that could not even start (no store on this system), answered on the next poll.
    unstartable: Option<String>,
}

impl DesktopConnections {
    /// The standard store and keychain for this user.
    pub fn new() -> Self {
        let manager = ConnectionManager::standard().map_err(|e| e.message);
        Self::with(manager)
    }

    fn with(manager: Result<ConnectionManager, String>) -> Self {
        let runner = manager.as_ref().ok().map(|m| TestRunner::new(m.clone()));
        Self { manager, runner, unstartable: None }
    }

    fn manager(&self) -> Result<&ConnectionManager, String> {
        self.manager.as_ref().map_err(Clone::clone)
    }
}

fn key_view(k: KeyState) -> KeyView {
    match k {
        KeyState::NotNeeded => KeyView::NotNeeded,
        KeyState::Saved => KeyView::Saved,
        KeyState::Missing => KeyView::Missing,
        KeyState::Unreadable => KeyView::Unreadable,
    }
}

impl ConnectionsPort for DesktopConnections {
    fn rows(&mut self) -> Result<Vec<Row>, String> {
        let views = self.manager()?.views().map_err(|e| e.message)?;
        Ok(views
            .into_iter()
            .map(|v| {
                let c = v.connection;
                Row {
                    id: c.id.clone(),
                    label: c.label.clone(),
                    provider: c.provider.clone(),
                    endpoint: c.endpoint.clone().unwrap_or_default(),
                    local: c.locality == Locality::Local,
                    key: key_view(v.key),
                    models: c.models.len(),
                    bindings: v.bindings,
                    enabled: c.enabled,
                    draft: ConnectionDraft::from_connection(&c),
                }
            })
            .collect())
    }

    fn save(&mut self, draft: &ConnectionDraft) -> Result<Option<String>, Failure> {
        let manager = self.manager().map_err(|message| Failure { field: None, message })?;
        match manager.save(draft) {
            Ok(saved) => Ok(saved.warning),
            Err(SaveError::Invalid(e)) => Err(Failure { field: Some(e.field), message: e.message }),
            Err(SaveError::Failed(e)) => Err(Failure { field: None, message: e.message }),
        }
    }

    fn delete(&mut self, id: &ConnectionId) -> Result<(), String> {
        let removed = self.manager()?.delete(id).map_err(|e| e.message)?;
        if removed.key_removed { Ok(()) } else { Err("The connection was removed, but its key could not be deleted from the keychain.".to_owned()) }
    }

    fn set_enabled(&mut self, id: &ConnectionId, enabled: bool) -> Result<(), String> {
        self.manager()?.set_enabled(id, enabled).map_err(|e| e.message)
    }

    fn start_test(&mut self, target: TestTarget) {
        let Some(runner) = self.runner.as_mut() else {
            self.unstartable = Some(self.manager.as_ref().err().cloned().unwrap_or_default());
            return;
        };
        self.unstartable = None;
        runner.start(match target {
            TestTarget::Draft(d) => craft_ai_client::TestTarget::Draft(d),
            TestTarget::Saved(id) => craft_ai_client::TestTarget::Saved(id),
        });
    }

    fn poll_test(&mut self) -> Option<Result<TestOutcome, String>> {
        if let Some(e) = self.unstartable.take() {
            return Some(Err(e));
        }
        let answer = self.runner.as_mut()?.poll()?;
        Some(
            answer.map(|r| TestOutcome { models: r.models, millis: u64::try_from(r.elapsed.as_millis()).unwrap_or(u64::MAX) }).map_err(|e| e.message),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use craft_ai_model::{Field, PRESETS};
    use craft_ai_native::secrets::MemoryStore;
    use craft_ai_native::shared::SharedStore;

    use super::*;

    fn adapter() -> (DesktopConnections, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let manager = ConnectionManager::new(SharedStore::new(dir.path().join("connections.json")), Arc::new(MemoryStore::new()));
        (DesktopConnections::with(Ok(manager)), dir)
    }

    fn openai(key: &str) -> ConnectionDraft {
        let mut d = ConnectionDraft::from_preset(&PRESETS[0]);
        d.key = key.into();
        d
    }

    fn wait(port: &mut DesktopConnections) -> Option<Result<TestOutcome, String>> {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if let Some(a) = port.poll_test() {
                return Some(a);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    #[test]
    fn a_saved_connection_comes_back_as_a_row_without_its_key() {
        let (mut port, _dir) = adapter();
        assert!(port.save(&openai("sk-secret-value")).unwrap().is_none());
        let rows = port.rows().unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!((r.label.as_str(), r.provider.as_str(), r.key, r.local), ("OpenAI", "openai", KeyView::Saved, false));
        assert!(r.draft.key.is_empty(), "the edit form never starts with the key");
        assert!(!format!("{r:?}").contains("sk-secret-value"));
    }

    #[test]
    fn a_bad_form_reports_the_box_and_a_missing_store_reports_why() {
        let (mut port, _dir) = adapter();
        let f = port.save(&openai("has a space")).unwrap_err();
        assert_eq!(f.field, Some(Field::Key));
        let mut broken = DesktopConnections::with(Err("no place to keep connections".into()));
        assert!(broken.rows().unwrap_err().contains("no place"));
        assert!(broken.save(&openai("k")).unwrap_err().message.contains("no place"));
        broken.start_test(TestTarget::Saved(ConnectionId::new("x").unwrap()));
        assert!(wait(&mut broken).unwrap().unwrap_err().contains("no place"));
    }

    #[test]
    fn remove_and_enable_work_and_a_missing_id_is_an_error() {
        let (mut port, _dir) = adapter();
        port.save(&openai("k")).unwrap();
        let id = port.rows().unwrap().remove(0).id;
        port.set_enabled(&id, false).unwrap();
        assert!(!port.rows().unwrap()[0].enabled);
        port.delete(&id).unwrap();
        assert!(port.rows().unwrap().is_empty());
        assert!(port.delete(&id).is_err());
    }

    #[test]
    fn a_test_of_an_unreachable_server_comes_back_as_a_message_not_a_hang() {
        let (mut port, _dir) = adapter();
        let mut d = ConnectionDraft::from_preset(&PRESETS[2]);
        // Nothing listens on this port.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        d.endpoint = format!("http://127.0.0.1:{}/v1", l.local_addr().unwrap().port());
        drop(l);
        port.start_test(TestTarget::Draft(d));
        let answer = wait(&mut port).expect("an answer within 10 s");
        assert!(answer.is_err());
    }

    #[test]
    fn an_older_answer_that_arrives_late_is_dropped() {
        let (mut port, _dir) = adapter();
        // Two tests started back to back: only the second one's answer is wanted.
        let bad = ConnectionDraft::from_preset(&PRESETS[0]); // no key: fails fast, as the first test
        port.start_test(TestTarget::Draft(bad));
        port.start_test(TestTarget::Saved(ConnectionId::new("nope").unwrap()));
        let answer = wait(&mut port).unwrap().unwrap_err();
        assert!(answer.contains("nope"), "the answer is the second test's: {answer}");
        assert!(port.poll_test().is_none(), "the first answer was discarded, not delivered later");
        std::thread::sleep(Duration::from_millis(200));
        assert!(port.poll_test().is_none());
    }
}
