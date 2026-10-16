//! The Connections screen: where a person adds, edits, tests and removes the AI models the Craft
//! apps can use. Keys are typed here once and every app sees them.
//!
//! This module holds the screen's state and what happens when a button is pressed; `list` and
//! `form` draw it. Everything that touches disk, the keychain or the network goes through
//! [`ConnectionsPort`], which the desktop shell implements (the web build has no port, so the
//! screen never appears there). The rules for what a valid connection is live in
//! `craft-ai-model`'s `ConnectionDraft`, shared with the form apps show when no launcher exists.

mod form;
mod list;
mod pick;
pub(crate) use list::title;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use craft_ai_model::{ConnectionDraft, ConnectionId, Field, ModelInfo, Preset};

/// Whether a connection's key is where it should be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyView {
    NotNeeded,
    Saved,
    Missing,
    Unreadable,
}

/// One saved connection, as the list shows it.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: ConnectionId,
    pub label: String,
    pub provider: String,
    pub endpoint: String,
    /// Data stays on this computer.
    pub local: bool,
    pub key: KeyView,
    pub models: usize,
    /// Default or app choices that use it.
    pub bindings: usize,
    pub enabled: bool,
    /// The form's starting state when this row is edited.
    pub draft: ConnectionDraft,
}

/// A problem to show, with the box it belongs to when there is one.
#[derive(Clone, Debug)]
pub struct Failure {
    pub field: Option<Field>,
    pub message: String,
}

/// What to test.
#[derive(Clone, Debug)]
pub enum TestTarget {
    /// The form as typed, before saving.
    Draft(ConnectionDraft),
    /// A saved connection; the models found are remembered.
    Saved(ConnectionId),
}

/// A passed test.
#[derive(Clone, Debug)]
pub struct TestOutcome {
    pub models: Vec<ModelInfo>,
    pub millis: u64,
}

/// What only the desktop shell can do for this screen.
pub trait ConnectionsPort {
    /// Every saved connection.
    fn rows(&mut self) -> Result<Vec<Row>, String>;
    /// Adds or updates. `Ok` carries a minor warning to show, if any.
    fn save(&mut self, draft: &ConnectionDraft) -> Result<Option<String>, Failure>;
    fn delete(&mut self, id: &ConnectionId) -> Result<(), String>;
    fn set_enabled(&mut self, id: &ConnectionId, enabled: bool) -> Result<(), String>;
    /// Starts a test in the background. Only one runs at a time; starting another replaces it.
    fn start_test(&mut self, target: TestTarget);
    /// The finished test, once, if there is one.
    fn poll_test(&mut self) -> Option<Result<TestOutcome, String>>;
}

/// How a test is going.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TestState {
    Idle,
    Running,
    /// Passed with this many models.
    Passed {
        models: usize,
        millis: u64,
    },
    Failed(String),
}

/// The add or edit form.
pub(crate) struct FormState {
    /// The preset the form started from, to show its note.
    pub preset: Option<&'static str>,
    pub draft: ConnectionDraft,
    pub error: Option<Failure>,
    pub test: TestState,
    /// Reached from the list of every service, so Back returns there.
    pub via_pick: bool,
}

pub(crate) enum Screen {
    List,
    /// Step one of adding: which service.
    Pick,
    Form(Box<FormState>),
    ConfirmRemove(ConnectionId),
}

/// Which test is in flight, so its answer lands in the right place.
enum Pending {
    Form,
    Saved(ConnectionId),
}

/// State of the Connections dialog.
pub struct ConnectionsUi {
    pub(crate) screen: Screen,
    pub(crate) rows: Vec<Row>,
    loaded: bool,
    /// A message under the list: `(text, is_error)`.
    pub(crate) notice: Option<(String, bool)>,
    /// Latest test of each saved connection this session.
    pub(crate) tested: HashMap<ConnectionId, TestState>,
    pending: Option<Pending>,
}

impl Default for ConnectionsUi {
    fn default() -> Self {
        Self { screen: Screen::List, rows: Vec::new(), loaded: false, notice: None, tested: HashMap::new(), pending: None }
    }
}

impl ConnectionsUi {
    /// Called when the dialog opens: start at the list and read it afresh.
    pub fn open(&mut self) {
        self.screen = Screen::List;
        self.loaded = false;
        self.notice = None;
    }

    /// Reads the list once after `open`.
    pub(crate) fn ensure_loaded(&mut self, port: &mut dyn ConnectionsPort) {
        if !self.loaded {
            self.reload(port);
        }
    }

    /// Remove pressed on a saved connection: ask first.
    pub(crate) fn begin_remove(&mut self, id: &ConnectionId) {
        self.screen = Screen::ConfirmRemove(id.clone());
    }

    pub(crate) fn reload(&mut self, port: &mut dyn ConnectionsPort) {
        self.loaded = true;
        match port.rows() {
            Ok(rows) => self.rows = rows,
            Err(e) => {
                self.rows.clear();
                self.notice = Some((crate::i18n::fmt(tl!("Couldn't load connections: {e}"), &[("e", &e)]), true));
            }
        }
    }

    /// Add pressed: choose a service first.
    pub(crate) fn begin_pick(&mut self) {
        self.screen = Screen::Pick;
    }

    pub(crate) fn begin_add(&mut self, preset: &Preset) {
        self.screen = Screen::Form(Box::new(FormState {
            preset: Some(preset.key),
            draft: ConnectionDraft::from_preset(preset),
            error: None,
            test: TestState::Idle,
            via_pick: false,
        }));
    }

    pub(crate) fn begin_edit(&mut self, id: &ConnectionId) {
        let Some(row) = self.rows.iter().find(|r| &r.id == id) else { return };
        self.screen =
            Screen::Form(Box::new(FormState { preset: None, draft: row.draft.clone(), error: None, test: TestState::Idle, via_pick: false }));
    }

    /// Save pressed. Returns whether the form closed.
    pub(crate) fn submit(&mut self, port: &mut dyn ConnectionsPort) -> bool {
        let Screen::Form(form) = &mut self.screen else { return false };
        match port.save(&form.draft) {
            Ok(warning) => {
                self.screen = Screen::List;
                self.notice = warning.map(|w| (w, true));
                self.reload(port);
                true
            }
            Err(failure) => {
                form.error = Some(failure);
                false
            }
        }
    }

    /// Test pressed in the form.
    pub(crate) fn test_form(&mut self, port: &mut dyn ConnectionsPort) {
        let Screen::Form(form) = &mut self.screen else { return };
        form.test = TestState::Running;
        form.error = None;
        self.pending = Some(Pending::Form);
        port.start_test(TestTarget::Draft(form.draft.clone()));
    }

    /// Test pressed on a saved connection.
    pub(crate) fn test_saved(&mut self, id: &ConnectionId, port: &mut dyn ConnectionsPort) {
        self.tested.insert(id.clone(), TestState::Running);
        self.pending = Some(Pending::Saved(id.clone()));
        port.start_test(TestTarget::Saved(id.clone()));
    }

    pub(crate) fn is_testing(&self) -> bool {
        self.pending.is_some()
    }

    /// Collects a finished test. Returns true if the screen changed.
    pub(crate) fn poll(&mut self, port: &mut dyn ConnectionsPort) -> bool {
        if self.pending.is_none() {
            return false;
        }
        let Some(result) = port.poll_test() else { return false };
        let state = match &result {
            Ok(o) => TestState::Passed { models: o.models.len(), millis: o.millis },
            Err(e) => TestState::Failed(e.clone()),
        };
        match self.pending.take() {
            Some(Pending::Form) => {
                if let Screen::Form(form) = &mut self.screen {
                    if let Ok(o) = result {
                        form.draft.models = o.models;
                    }
                    form.test = state;
                }
            }
            Some(Pending::Saved(id)) => {
                let passed = matches!(state, TestState::Passed { .. });
                self.tested.insert(id, state);
                if passed {
                    // The models found were remembered; show the new count.
                    self.reload(port);
                }
            }
            None => {}
        }
        true
    }

    pub(crate) fn set_enabled(&mut self, id: &ConnectionId, on: bool, port: &mut dyn ConnectionsPort) {
        match port.set_enabled(id, on) {
            Ok(()) => self.reload(port),
            Err(e) => self.notice = Some((e, true)),
        }
    }

    pub(crate) fn remove(&mut self, id: &ConnectionId, port: &mut dyn ConnectionsPort) {
        self.screen = Screen::List;
        match port.delete(id) {
            Ok(()) => self.notice = None,
            Err(e) => self.notice = Some((e, true)),
        }
        self.tested.remove(id);
        self.reload(port);
    }

    /// Closes the form or confirmation and goes back to the list.
    pub(crate) fn back(&mut self) {
        self.screen = Screen::List;
        self.pending = None;
    }
}
