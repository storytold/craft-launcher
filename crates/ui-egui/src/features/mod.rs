//! The Connections dialog's tools: what the Craft apps can do with AI, which model does each job,
//! and whether it really does it.
//!
//! The Tools window is for choosing, for each tool, which connected model does that job, then
//! checking it. Connecting providers is a separate window (Connections).
//!
//! This module holds the state and the port the desktop shell implements. `hub` lays out the
//! dialog, `rail` draws the list on the left, `detail` and `tool` draw a tool's pane (apps, model
//! picker, sample test), `setup` draws the local install step, `text` has the wording. Scanning the disk, downloading, binding and running a sample all go through
//! [`FeaturesPort`], so the UI never touches the disk, the keychain or the network (the web build
//! has no port and the screen never appears there).

mod advanced;
pub mod extensions;
mod folders;
mod history;
mod history_overview;
mod history_table;
mod home;
mod setup;
#[cfg(test)]
mod tests;
mod text;
mod tool;

use craft_ai_features::{Feature, Plan, SetupOption};
use craft_ai_model::{Billing, ConnectionId, ModelId, Preset};

pub use history::Clock;
pub use text::bytes;

/// How an install is going.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Install {
    #[default]
    Idle,
    Running {
        feature: String,
        file: String,
        /// The download is done and the file is being checked.
        verifying: bool,
        done: u64,
        total: u64,
        /// Which file this is out of how many.
        step: usize,
        of: usize,
    },
    /// Everything for the feature was downloaded and checked.
    Done {
        feature: String,
    },
    Failed {
        feature: String,
        message: String,
    },
}

/// One model that could do a tool's job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolChoice {
    pub connection: ConnectionId,
    pub model: ModelId,
    /// "Model name, Connection name", for the picker.
    pub label: String,
    /// The model runs on this computer.
    pub local: bool,
}

/// Whether a tool's chosen model works right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    NoChoice,
    Ready {
        label: String,
        local: bool,
    },
    /// A choice was made and no longer holds, with a sentence saying why.
    Unavailable(String),
}

/// A tool's model choice, as the launcher shows it.
#[derive(Clone, Debug)]
pub struct ToolState {
    pub feature: String,
    pub choices: Vec<ToolChoice>,
    pub bound: Option<(ConnectionId, ModelId)>,
    pub status: ToolStatus,
    /// A sample request exists for this tool.
    pub can_test: bool,
    /// The sample is likely to cost noticeably more than a few pennies at hosted prices.
    pub costly: bool,
}

/// One folder of model files, as the Model folders page shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderCard {
    pub id: String,
    pub label: String,
    pub path: String,
    /// New downloads are saved here.
    pub default: bool,
    /// Read from another program's settings, so nothing is ever saved into it.
    pub read_only: bool,
    /// The folder is not on disk right now (a drive that is unplugged, or a folder that was moved).
    pub missing: bool,
    /// The kinds of model found, each with how many places in the folder hold that kind.
    pub kinds: Vec<(String, usize)>,
}

/// The state of the one sample that can run at a time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Sample {
    #[default]
    Idle,
    /// Nothing was sent. The person has to agree to this destination first.
    NeedsConsent {
        feature: String,
        connection: String,
        destination: String,
        items: Vec<String>,
        billing: Billing,
    },
    Running {
        feature: String,
    },
    Passed {
        feature: String,
        summary: String,
        millis: u64,
    },
    Failed {
        feature: String,
        message: String,
    },
}

impl Sample {
    /// The tool this sample is about, if any.
    pub fn feature(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::NeedsConsent { feature, .. } | Self::Running { feature } | Self::Passed { feature, .. } | Self::Failed { feature, .. } => {
                Some(feature)
            }
        }
    }
}

/// What the shell knows right now.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// The disk is being scanned.
    pub scanning: bool,
    /// One plan per feature the port listed. Empty until the first scan finishes.
    pub plans: Vec<Plan>,
    pub tools: Vec<ToolState>,
    pub install: Install,
    pub sample: Sample,
}

/// What only the desktop shell can do for this screen.
pub trait FeaturesPort {
    /// Every feature to show, in order.
    fn features(&self) -> Vec<Feature>;
    /// Starts a scan of this computer in the background. Cheap to call again.
    fn rescan(&mut self);
    /// Re-reads connections and model choices. Quick; call it when returning from the Providers tab.
    fn refresh_tools(&mut self);
    fn snapshot(&mut self) -> Snapshot;
    /// Starts downloading what `feature` is missing. `accepted` says the person agreed to the
    /// licences shown; the port refuses to start without it when a licence needs accepting.
    fn install(&mut self, feature: &str, accepted: bool) -> Result<(), String>;
    fn cancel_install(&mut self);
    /// Makes this model the launcher's choice for the tool, or forgets the choice for `None`.
    fn choose(&mut self, feature: &str, choice: Option<(ConnectionId, ModelId)>) -> Result<(), String>;
    /// Runs the tool's sample. A remote destination not yet agreed to comes back as
    /// [`Sample::NeedsConsent`] with nothing sent; call again with `agreed` once the person says yes.
    fn test(&mut self, feature: &str, agreed: bool);
    fn cancel_test(&mut self);
    /// Forgets a finished or refused sample so the card goes back to its Test button.
    fn dismiss_test(&mut self);
    /// Every logged AI job from every app, newest first.
    fn history(&mut self) -> Result<craft_ai_model::HistoryLog, String>;
    /// Deletes the log for every app.
    fn clear_history(&mut self) -> Result<(), String>;
    /// Saves CSV text where the person will find it and returns the path, for the screen to show.
    fn export_history(&mut self, csv: &str) -> Result<String, String>;
    /// The time now and the local offset from UTC.
    fn clock(&self) -> Clock;
    /// The folders the person keeps models in, default first.
    fn model_folders(&mut self) -> Vec<FolderCard> {
        Vec::new()
    }
    /// The folder proposed when there are none, if this computer has a place for one.
    fn suggested_model_folder(&self) -> Option<String> {
        None
    }
    /// Registers a folder and reads what is in it. `create` makes it first if it is not there (for
    /// the proposed folder). Returns a sentence saying what was found.
    fn add_model_folder(&mut self, _path: &str, _create: bool) -> Result<String, String> {
        Err("Model folders are not available here.".to_owned())
    }
    /// Reads a folder again. Returns a sentence saying what was found.
    fn rescan_model_folder(&mut self, _id: &str) -> Result<String, String> {
        Err("Model folders are not available here.".to_owned())
    }
    /// Makes new downloads go to this folder.
    fn set_default_model_folder(&mut self, _id: &str) -> Result<(), String> {
        Err("Model folders are not available here.".to_owned())
    }
    /// Forgets a folder. Its files are not touched.
    fn remove_model_folder(&mut self, _id: &str) -> Result<(), String> {
        Err("Model folders are not available here.".to_owned())
    }
}

/// What a tool's pane asks of its window.
pub enum Outcome {
    None,
    Close,
    /// Open the connection form for this service.
    AddConnection(&'static Preset),
    /// Open the list of every service and server.
    AddServer,
}

/// Which page of the Set up AI window is showing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Page {
    /// What can be set up, and what each thing switches on.
    #[default]
    Home,
    /// Downloading the files for this feature's local model.
    Local(String),
    /// Connecting, editing or removing a service (the connection screens).
    Connect,
    /// Which model each tool uses.
    Advanced,
    /// Every AI job the apps have run, and what it cost.
    History,
    /// Where the person keeps model files.
    Folders,
}

/// State of the Set up AI window.
#[derive(Default)]
pub struct FeaturesUi {
    pub(crate) page: Page,
    pub(crate) features: Vec<Feature>,
    /// What can be set up, derived from the features.
    pub(crate) options: Vec<SetupOption>,
    scan_requested: bool,
    /// Something was set up, so connections may have changed: read the tools again.
    refresh_pending: bool,
    /// The person ticked the box agreeing to the licences on the setup screen.
    pub(crate) accepted: bool,
    /// A line under the screen: `(text, is_error)`.
    pub(crate) notice: Option<(String, bool)>,
    pub(crate) history: history::HistoryUi,
    pub(crate) folders: folders::FoldersUi,
}

impl FeaturesUi {
    /// Called when the window opens: start at the list and look at the computer afresh.
    pub fn open(&mut self) {
        self.page = Page::Home;
        self.scan_requested = false;
        self.refresh_pending = false;
        self.accepted = false;
        self.notice = None;
        self.folders.refresh();
    }

    pub(crate) fn begin_setup(&mut self, feature: &str) {
        self.page = Page::Local(feature.to_owned());
        self.accepted = false;
        self.notice = None;
    }

    /// Back to the list. Whatever was just done may have changed what is switched on.
    pub(crate) fn back(&mut self) {
        self.page = Page::Home;
        self.refresh_pending = true;
        self.notice = None;
    }

    pub(crate) fn ensure_loaded(&mut self, port: &mut dyn FeaturesPort) {
        if self.features.is_empty() {
            self.features = port.features();
            self.options = craft_ai_features::setup_options(&self.features);
        }
        if !self.scan_requested {
            self.scan_requested = true;
            port.rescan();
            port.refresh_tools();
        } else if self.refresh_pending {
            port.refresh_tools();
        }
        self.refresh_pending = false;
    }

    /// Install pressed on the setup screen.
    pub(crate) fn start_install(&mut self, feature: &str, port: &mut dyn FeaturesPort) {
        self.notice = match port.install(feature, self.accepted) {
            Ok(()) => None,
            Err(message) => Some((message, true)),
        };
    }

    /// A model picked (or cleared) in a tool's picker.
    pub(crate) fn choose(&mut self, feature: &str, choice: Option<(ConnectionId, ModelId)>, port: &mut dyn FeaturesPort) {
        self.notice = port.choose(feature, choice).err().map(|message| (message, true));
    }

    pub(crate) fn plan_for<'a>(&self, snapshot: &'a Snapshot, feature: &str) -> Option<&'a Plan> {
        snapshot.plans.iter().find(|p| p.feature == feature)
    }
}
