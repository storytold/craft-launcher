//! The desktop shell's side of the Tools tab's model choices and sample tests: which connected
//! model does each tool's job, and does it work when asked.
//!
//! Everything goes through [`ToolDesk`] in `craft-ai-client`, which acts as the app `launcher`:
//! the choice is stored as the launcher's default binding that apps inherit, and a sample takes the
//! same path an app's request does (checked against the model, agreement first for a hosted
//! destination, the key read only when the job starts). A sample runs on its own thread; starting
//! a different one, cancelling or dismissing makes a late answer from an older one disappear.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use artcraft_launcher_ui_egui::features::{Sample, ToolChoice, ToolState, ToolStatus, bytes};
use craft_ai_client::{Job, RoleStatus, SampleReport, TestStart, ToolDesk};
use craft_ai_features::{Feature, sample_is_costly, sample_request};
use craft_ai_model::{ConnectionId, Locality, ModelId, PayloadKind};

/// How long a sample may take before it is given up on. Video is slow.
const IMAGE_WAIT: Duration = Duration::from_secs(180);
const VIDEO_WAIT: Duration = Duration::from_secs(900);

#[derive(Default)]
struct State {
    tools: Vec<ToolState>,
    sample: Sample,
    /// Bumped whenever the sample is started, cancelled or dismissed.
    generation: u64,
    job: Option<Arc<Job>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone)]
pub(crate) struct Tools {
    desk: Option<Arc<ToolDesk>>,
    /// Why there is no desk (no store location on this system), shown instead of a silent failure.
    problem: Option<String>,
    state: Arc<Mutex<State>>,
}

impl Tools {
    pub(crate) fn new(desk: Result<ToolDesk, String>) -> Self {
        match desk {
            Ok(desk) => Self { desk: Some(Arc::new(desk)), problem: None, state: Arc::default() },
            Err(problem) => Self { desk: None, problem: Some(problem), state: Arc::default() },
        }
    }

    pub(crate) fn snapshot(&self) -> (Vec<ToolState>, Sample) {
        let guard = lock(&self.state);
        (guard.tools.clone(), guard.sample.clone())
    }

    /// Re-reads connections and choices off the UI thread (the keychain may be slow).
    pub(crate) fn refresh(&self, features: &[Feature]) {
        let Some(desk) = self.desk.clone() else { return };
        let (state, features) = (Arc::clone(&self.state), features.to_vec());
        std::thread::spawn(move || {
            // Whatever was just connected switches its tools on, but never over a choice made.
            let roles: Vec<_> = features.iter().filter_map(Feature::role).collect();
            let _ = desk.auto_bind(&roles);
            let tools = compute(&desk, &features);
            lock(&state).tools = tools;
        });
    }

    pub(crate) fn choose(&self, features: &[Feature], feature: &str, choice: Option<(ConnectionId, ModelId)>) -> Result<(), String> {
        let desk = self.desk.as_ref().ok_or_else(|| self.problem.clone().unwrap_or_default())?;
        let role = features.iter().find(|f| f.id == feature).and_then(Feature::role).ok_or("That tool is not known.")?;
        match choice {
            Some((connection, model)) => desk.choose(&role, &connection, &model),
            // "Automatic": forget the choice, then take the best one that is connected.
            None => desk.clear(&role.id).and_then(|()| desk.auto_bind(std::slice::from_ref(&role)).map(|_| ())),
        }
        .map_err(|e| e.message)?;
        // A sample of the old choice says nothing about the new one.
        self.dismiss();
        let tools = compute(desk, features);
        lock(&self.state).tools = tools;
        Ok(())
    }

    pub(crate) fn test(&self, features: &[Feature], feature: &str, agreed: bool) {
        let Some(desk) = self.desk.clone() else {
            lock(&self.state).sample = Sample::Failed { feature: feature.to_owned(), message: self.problem.clone().unwrap_or_default() };
            return;
        };
        let Some(found) = features.iter().find(|f| f.id == feature).cloned() else { return };
        let generation = {
            let mut guard = lock(&self.state);
            if matches!(guard.sample, Sample::Running { .. }) {
                return;
            }
            guard.generation += 1;
            guard.sample = Sample::Running { feature: feature.to_owned() };
            guard.job = None;
            guard.generation
        };
        let state = Arc::clone(&self.state);
        std::thread::spawn(move || {
            let outcome = run(&desk, &found, agreed, &state, generation);
            let mut guard = lock(&state);
            if guard.generation == generation {
                guard.sample = outcome;
                guard.job = None;
            }
        });
    }

    pub(crate) fn cancel(&self) {
        let mut guard = lock(&self.state);
        guard.generation += 1;
        if let Some(job) = guard.job.take() {
            // Best effort: the job may already be finished.
            let _ = job.cancel();
        }
        guard.sample = Sample::Idle;
    }

    pub(crate) fn dismiss(&self) {
        let mut guard = lock(&self.state);
        if !matches!(guard.sample, Sample::Running { .. }) {
            guard.generation += 1;
            guard.sample = Sample::Idle;
        }
    }
}

fn run(desk: &ToolDesk, feature: &Feature, agreed: bool, state: &Mutex<State>, generation: u64) -> Sample {
    let failed = |message: String| Sample::Failed { feature: feature.id.clone(), message };
    let (Some(role), Some(request)) = (feature.role(), sample_request(feature)) else {
        return failed("There is no test for this tool yet.".to_owned());
    };
    match desk.start_test(&role, request, agreed) {
        Err(error) => failed(error.message),
        Ok(TestStart::NeedsConsent { disclosure, connection_label, billing }) => Sample::NeedsConsent {
            feature: feature.id.clone(),
            connection: connection_label,
            destination: disclosure.destination,
            items: disclosure
                .items
                .iter()
                .map(|i| match i.bytes {
                    Some(n) => format!("{} {}", kind_word(i.kind), bytes(n)),
                    None => kind_word(i.kind).to_owned(),
                })
                .collect(),
            billing,
        },
        Ok(TestStart::Started(job)) => {
            let job = Arc::new(job);
            {
                let mut guard = lock(state);
                if guard.generation != generation {
                    // Cancelled while it was starting.
                    let _ = job.cancel();
                    return Sample::Idle;
                }
                guard.job = Some(Arc::clone(&job));
            }
            let wait = if sample_is_costly(feature) { VIDEO_WAIT } else { IMAGE_WAIT };
            match ToolDesk::finish(&role, &job, wait) {
                SampleReport::Passed { summary, millis } => Sample::Passed { feature: feature.id.clone(), summary, millis },
                SampleReport::Failed { message } => failed(message),
            }
        }
    }
}

fn kind_word(kind: PayloadKind) -> &'static str {
    match kind {
        PayloadKind::Text => "text",
        PayloadKind::Image => "picture",
        PayloadKind::Video => "video",
        PayloadKind::Audio => "audio",
        PayloadKind::Document => "document",
        PayloadKind::Other => "file",
    }
}

/// Every tool's choices, current choice and whether that choice works now.
pub(crate) fn compute(desk: &ToolDesk, features: &[Feature]) -> Vec<ToolState> {
    features
        .iter()
        .filter_map(|feature| {
            let role = feature.role()?;
            let view = desk.view(&role).ok()?;
            let choices: Vec<ToolChoice> = view
                .choices
                .iter()
                .map(|c| ToolChoice {
                    connection: c.connection.clone(),
                    model: c.model.clone(),
                    label: format!("{}, {}", c.model_label, c.connection_label),
                    local: c.locality == Locality::Local,
                })
                .collect();
            let status = match (&view.bound, view.status) {
                (None, _) => ToolStatus::NoChoice,
                (Some(_), RoleStatus::Ready(ready)) => ToolStatus::Ready {
                    label: format!("{}, {}", ready.model_label, ready.connection_label),
                    local: ready.locality == Locality::Local,
                },
                (Some(_), RoleStatus::Unavailable { reason }) => ToolStatus::Unavailable(reason),
            };
            Some(ToolState {
                feature: feature.id.clone(),
                choices,
                bound: view.bound,
                status,
                can_test: sample_request(feature).is_some(),
                costly: sample_is_costly(feature),
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "features_tools_tests.rs"]
mod tests;
