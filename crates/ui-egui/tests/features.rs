//! The Set up AI window driven the way a person would: open it, see what can be set up and what
//! each thing switches on, set one up or connect a service. Fake ports stand in for the disk, the
//! network and the keychain; the real ones are covered where they live, in the desktop shell.

use std::cell::RefCell;
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event};
use artcraft_launcher_core::{Arch, Os, Platform};
use artcraft_launcher_ui_egui::features::{Install, Sample, Snapshot, ToolChoice, ToolState, ToolStatus};
use artcraft_launcher_ui_egui::i18n::{Lang, set_test_lang};
use artcraft_launcher_ui_egui::theme::ThemeChoice;
use artcraft_launcher_ui_egui::{ConnectionsPort, Failure, FeaturesPort, Host, KeyView, LauncherUi, Row, Settings, TestOutcome, TestTarget};
use craft_ai_features::{Feature, InstalledFile, Inventory, builtin, plan};
use craft_ai_model::{Billing, ConnectionDraft, ConnectionId, Locality, ModelId, PRESETS};
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

#[derive(Default)]
struct World {
    inventory: Inventory,
    install: Install,
    scans: usize,
    installs: Vec<(String, bool)>,
    cancels: usize,
    refreshes: usize,
    tools: Vec<ToolState>,
    sample: Sample,
    chosen: Vec<(String, Option<(ConnectionId, ModelId)>)>,
    choose_refused: Option<String>,
    tests: Vec<(String, bool)>,
    test_cancels: usize,
    dismissals: usize,
}

#[derive(Clone, Default)]
struct Features(Rc<RefCell<World>>);

impl FeaturesPort for Features {
    fn features(&self) -> Vec<Feature> {
        builtin()
    }
    fn rescan(&mut self) {
        self.0.borrow_mut().scans += 1;
    }
    fn refresh_tools(&mut self) {
        self.0.borrow_mut().refreshes += 1;
    }
    fn snapshot(&mut self) -> Snapshot {
        let w = self.0.borrow();
        Snapshot {
            scanning: false,
            plans: builtin().iter().map(|f| plan(f, &w.inventory)).collect(),
            tools: w.tools.clone(),
            install: w.install.clone(),
            sample: w.sample.clone(),
        }
    }
    fn choose(&mut self, feature: &str, choice: Option<(ConnectionId, ModelId)>) -> Result<(), String> {
        let mut w = self.0.borrow_mut();
        if let Some(why) = w.choose_refused.clone() {
            return Err(why);
        }
        w.chosen.push((feature.to_owned(), choice.clone()));
        if let Some(tool) = w.tools.iter_mut().find(|t| t.feature == feature) {
            tool.bound = choice.clone();
            tool.status = match &choice {
                Some(_) => ToolStatus::Ready { label: "chosen".into(), local: false },
                None => ToolStatus::NoChoice,
            };
        }
        Ok(())
    }
    fn test(&mut self, feature: &str, agreed: bool) {
        let mut w = self.0.borrow_mut();
        w.tests.push((feature.to_owned(), agreed));
        w.sample = Sample::Running { feature: feature.to_owned() };
    }
    fn cancel_test(&mut self) {
        let mut w = self.0.borrow_mut();
        w.test_cancels += 1;
        w.sample = Sample::Idle;
    }
    fn dismiss_test(&mut self) {
        let mut w = self.0.borrow_mut();
        w.dismissals += 1;
        w.sample = Sample::Idle;
    }
    fn install(&mut self, feature: &str, accepted: bool) -> Result<(), String> {
        let mut w = self.0.borrow_mut();
        w.installs.push((feature.to_owned(), accepted));
        w.install = Install::Running {
            feature: feature.to_owned(),
            file: "sam3.safetensors".into(),
            verifying: false,
            done: 1_700_000_000,
            total: 3_449_860_632,
            step: 1,
            of: 1,
        };
        Ok(())
    }
    fn cancel_install(&mut self) {
        let mut w = self.0.borrow_mut();
        w.cancels += 1;
        w.install = Install::Idle;
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
    fn clock(&self) -> artcraft_launcher_ui_egui::features::Clock {
        artcraft_launcher_ui_egui::features::Clock::default()
    }
}

/// A connections store the tests can look inside: what was saved and deleted, and the rows shown.
#[derive(Default)]
struct ConnWorld {
    rows: Vec<Row>,
    saved: Vec<ConnectionDraft>,
    deleted: Vec<ConnectionId>,
}

#[derive(Clone, Default)]
struct Connections(Rc<RefCell<ConnWorld>>);

fn row_of(c: &craft_ai_model::Connection) -> Row {
    Row {
        id: c.id.clone(),
        label: c.label.clone(),
        provider: c.provider.clone(),
        endpoint: c.endpoint.clone().unwrap_or_default(),
        local: c.locality == Locality::Local,
        key: if c.auth.secret_ref().is_some() { KeyView::Saved } else { KeyView::NotNeeded },
        models: c.models.len(),
        bindings: 0,
        enabled: true,
        draft: ConnectionDraft::from_connection(c),
    }
}

/// A saved connection made from a service's own preset, as the form would have saved it.
fn saved_preset(key: &str) -> Row {
    let preset = PRESETS.iter().find(|p| p.key == key).unwrap();
    let mut draft = ConnectionDraft::from_preset(preset);
    draft.key = "sk-test".into();
    row_of(&draft.prepare(&[]).unwrap().connection)
}

impl ConnectionsPort for Connections {
    fn rows(&mut self) -> Result<Vec<Row>, String> {
        Ok(self.0.borrow().rows.clone())
    }
    fn save(&mut self, draft: &ConnectionDraft) -> Result<Option<String>, Failure> {
        let prepared = draft.prepare(&[]).map_err(|e| Failure { field: Some(e.field), message: e.message })?;
        let mut w = self.0.borrow_mut();
        w.saved.push(draft.clone());
        w.rows.push(row_of(&prepared.connection));
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

fn harness(features: Features, connections: Connections) -> Harness<'static, State> {
    set_test_lang(Lang::EN);
    Harness::builder().with_size(egui::vec2(1180.0, 2400.0)).build_ui_state(
        |ui, state: &mut State| {
            let (ui_state, shell) = state;
            let l = ui_state.get_or_insert_with(|| {
                let s = Settings { notify: false, theme: ThemeChoice::Dark, ..Settings::default() };
                LauncherUi::new(ui.ctx(), Box::new(Quiet), s)
            });
            l.logic(&ui.ctx().clone(), shell);
            l.ui(ui, shell);
        },
        (None, Shell { features, connections }),
    )
}

fn settle(h: &mut Harness<'_, State>) {
    for _ in 0..4 {
        h.step();
    }
}

fn bare_comfy() -> Inventory {
    Inventory { comfyui_found: true, ..Inventory::default() }
}

fn with_sam3() -> Inventory {
    Inventory {
        comfyui_found: true,
        files: vec![InstalledFile { folder: "sam3".into(), filename: "sam3.safetensors".into(), size_bytes: 3_449_860_632 }],
        node_folders: vec!["comfyui-sam3".into()],
        ..Inventory::default()
    }
}

fn open_with(features: &Features, connections: &Connections) -> Harness<'static, State> {
    let mut h = harness(features.clone(), connections.clone());
    settle(&mut h);
    h.get_by_label("Set up AI").click();
    settle(&mut h);
    h
}

fn open(features: &Features) -> Harness<'static, State> {
    open_with(features, &Connections::default())
}

fn press(h: &mut Harness<'_, State>, label: &str) {
    h.get_by_role_and_label(Role::Button, label).click();
    settle(h);
}

// ---- the list ----

#[test]
fn set_up_ai_lists_what_can_be_set_up_not_the_tools() {
    let f = Features::default();
    f.0.borrow_mut().inventory = bare_comfy();
    let h = open(&f);
    h.get_by_label("Set up one or more of these and the AI tools switch on in every Craft app that has them.");
    h.get_by_label("On this computer");
    h.get_by_label("Hosted services");
    for name in ["SAM 3", "OpenAI", "Black Forest Labs", "Seedance (Volcengine Ark)"] {
        h.get_by_label(name);
    }
    assert!(h.query_by_label("Start from").is_none() && h.query_by_label("Add a provider").is_none(), "no provider form to start with");
    assert_eq!(f.0.borrow().scans, 1);
    // No wall of every tool and no tabs: tools appear only as what a card switches on.
    assert!(h.query_by_role_and_label(Role::Button, "Select subject").is_none());
    assert!(h.query_by_role_and_label(Role::Button, "Providers").is_none());
}

#[test]
fn a_card_says_which_tools_it_switches_on_and_in_which_apps() {
    let f = Features::default();
    f.0.borrow_mut().inventory = bare_comfy();
    let h = open(&f);
    // OpenAI is named by generative fill and generate image.
    assert!(h.query_all_by_label("Generative fill").count() >= 1);
    assert!(h.query_all_by_label("Generate images").count() >= 1);
    assert!(h.query_all_by_label_contains("In PhotoCraft").count() >= 1, "the apps are named");
    assert!(h.query_all_by_label("Switches on").count() >= 5);
}

#[test]
fn what_is_already_switched_on_is_shown_at_the_top() {
    let f = Features::default();
    let h = open(&f);
    h.get_by_label("Nothing yet. Set something up below.");

    let f = Features::default();
    f.0.borrow_mut().tools = vec![ready_tool("generate-image")];
    let h = open(&f);
    assert!(h.query_by_label("Nothing yet. Set something up below.").is_none());
    // Once in the strip, once on each card that switches it on.
    assert!(h.query_all_by_label("Generate images").count() >= 2);
}

#[test]
fn the_local_model_says_what_this_computer_still_needs() {
    let f = Features::default();
    f.0.borrow_mut().inventory = bare_comfy();
    let h = open(&f);
    h.get_by_label("Set up, 3.4 GB");

    let f = Features::default();
    f.0.borrow_mut().inventory = with_sam3();
    let h = open(&f);
    h.get_by_label("Files in place");
    assert!(h.query_by_label("Ready").is_none(), "installed files are not a runnable tool");
    h.get_by_label("Planned for"); // The local card must not promise an enabled tool.
    assert!(h.query_by_label("Set up, 3.4 GB").is_none());

    let f = Features::default();
    let h = open(&f);
    h.get_by_label("ComfyUI was not found on this computer");
}

#[test]
fn completed_download_does_not_claim_the_tool_can_run() {
    let (f, mut h) = open_setup();
    {
        let mut w = f.0.borrow_mut();
        w.inventory = with_sam3();
        w.install = Install::Done { feature: "select-subject".into() };
    }
    settle(&mut h);
    assert!(h.query_by_label("Installed. This tool is ready on this computer.").is_none());
}

#[test]
fn a_local_model_no_app_can_run_yet_says_so_instead_of_promising_a_tool() {
    let f = Features::default();
    f.0.borrow_mut().inventory = bare_comfy();
    let h = open(&f);
    h.get_by_label("The apps cannot run this model yet. You can download it now, and the tool switches on when they can.");
}

#[test]
fn a_model_whose_files_are_here_is_not_told_to_download_them() {
    let f = Features::default();
    f.0.borrow_mut().inventory = with_sam3();
    let h = open(&f);
    h.get_by_label("Files in place");
    h.get_by_label("The files are in place. The apps cannot run this model yet, and the tool switches on when they can.");
    assert!(h.query_by_label("The apps cannot run this model yet. You can download it now, and the tool switches on when they can.").is_none());
}

// ---- local setup ----

fn open_setup() -> (Features, Harness<'static, State>) {
    let f = Features::default();
    f.0.borrow_mut().inventory = bare_comfy();
    let mut h = open(&f);
    press(&mut h, "Set up, 3.4 GB");
    (f, h)
}

#[test]
fn setting_up_shows_the_size_the_licence_and_waits_for_agreement() {
    let (f, mut h) = open_setup();
    h.get_by_label("Set up Select subject");
    h.get_by_label("Will be downloaded");
    h.get_by_label_contains("sam3.safetensors, 3.4 GB, from huggingface.co");
    h.get_by_label("Licence");
    assert!(h.query_all_by_label_contains("community copy").count() >= 1);
    h.get_by_label_contains("Also needs the ComfyUI-SAM3 add-on");
    // Pressing Download before agreeing does nothing.
    h.get_by_label("Download 3.4 GB").click();
    settle(&mut h);
    assert!(f.0.borrow().installs.is_empty());
    h.get_by_label("I have read the licence and accept it").click();
    settle(&mut h);
    h.get_by_label("Download 3.4 GB").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().installs, [("select-subject".to_owned(), true)]);
}

#[test]
fn progress_and_cancel_are_shown_while_downloading() {
    let (f, mut h) = open_setup();
    h.get_by_label("I have read the licence and accept it").click();
    settle(&mut h);
    h.get_by_label("Download 3.4 GB").click();
    settle(&mut h);
    h.get_by_label_contains("Downloading sam3.safetensors: 1.7 GB of 3.4 GB");
    assert!(h.query_by_role(Role::ProgressIndicator).is_some());
    h.get_by_label("Cancel download").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().cancels, 1);
}

#[test]
fn a_failed_download_says_why() {
    let (f, mut h) = open_setup();
    f.0.borrow_mut().install = Install::Failed { feature: "select-subject".into(), message: "the source refused the download".into() };
    settle(&mut h);
    h.get_by_label_contains("The download failed: the source refused the download");
}

#[test]
fn back_from_setup_returns_to_the_list_and_looks_at_the_tools_again() {
    let (f, mut h) = open_setup();
    press(&mut h, "Back");
    h.get_by_label("Hosted services");
    assert_eq!(f.0.borrow().refreshes, 2);
}

// ---- hosted services ----

#[test]
fn connect_opens_that_services_own_short_form_and_back_returns_to_the_list() {
    let f = Features::default();
    let mut h = open(&f);
    // The first Connect on the list is the first hosted service.
    h.query_all_by_role_and_label(Role::Button, "Connect").next().unwrap().click();
    settle(&mut h);
    h.get_by_label("Connect OpenAI");
    h.get_by_label("Data sent to this connection leaves this computer.");
    assert!(h.query_by_label("Add a provider").is_none());
    press(&mut h, "Back");
    h.get_by_label("Hosted services");
    assert!(h.query_by_label("Connect OpenAI").is_none());
}

#[test]
fn saving_a_key_connects_the_service_and_the_list_shows_it() {
    let f = Features::default();
    let c = Connections::default();
    let mut h = open_with(&f, &c);
    h.query_all_by_role_and_label(Role::Button, "Connect").next().unwrap().click();
    settle(&mut h);
    h.get_by_role(Role::PasswordInput).focus();
    h.get_by_role(Role::PasswordInput).type_text("sk-test-123");
    settle(&mut h);
    press(&mut h, "Save");
    assert_eq!(c.0.borrow().saved.len(), 1);
    assert_eq!(c.0.borrow().saved[0].key, "sk-test-123");
    h.get_by_label("Hosted services");
    h.get_by_label("Connected");
    h.get_by_role_and_label(Role::Button, "Edit");
    assert_eq!(f.0.borrow().refreshes, 2, "the tools are read again so the new connection switches them on");
}

#[test]
fn a_connected_service_can_be_edited_and_removed_after_asking() {
    let f = Features::default();
    let c = Connections::default();
    c.0.borrow_mut().rows = vec![saved_preset("openai")];
    let mut h = open_with(&f, &c);
    h.get_by_label("Connected");
    press(&mut h, "Edit");
    h.get_by_label("Edit connection");
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    settle(&mut h);
    press(&mut h, "Remove");
    h.get_by_label("Remove OpenAI?");
    h.get_by_label("Its saved key is deleted too.");
    assert!(c.0.borrow().deleted.is_empty(), "asked first");
    h.get_by_role_and_label(Role::Button, "Remove").click();
    settle(&mut h);
    assert_eq!(c.0.borrow().deleted.len(), 1);
    h.get_by_label("Hosted services");
    assert!(h.query_by_label("Connected").is_none());
}

#[test]
fn a_connection_that_is_not_one_of_the_listed_services_is_still_shown() {
    let f = Features::default();
    let c = Connections::default();
    c.0.borrow_mut().rows = vec![saved_preset("openai"), saved_preset("ollama")];
    let h = open_with(&f, &c);
    h.get_by_label("Your other connections");
    h.get_by_label("Ollama");
    assert_eq!(h.query_all_by_label("OpenAI").count(), 1, "a listed service shows only on its own card");
}

#[test]
fn add_another_server_lives_under_advanced_and_cancel_comes_back() {
    let f = Features::default();
    let mut h = open(&f);
    assert!(h.query_by_role_and_label(Role::Button, "Add another server").is_none(), "a power-user thing, not on the list");
    press(&mut h, "Advanced");
    press(&mut h, "Add another server");
    h.get_by_label("Add a provider");
    h.get_by_label("Choose the one you use");
    assert!(h.query_by_role_and_label(Role::Button, "SAM 3 (local)").is_none(), "SAM 3 is a model, not a server address");
    press(&mut h, "Cancel");
    h.get_by_label("Hosted services");
    assert!(h.query_by_label("Add a provider").is_none());
    // Choosing from that list, then Back, returns to the list of services, not to Set up AI.
    press(&mut h, "Advanced");
    press(&mut h, "Add another server");
    press(&mut h, "Ollama");
    h.get_by_label("Connect Ollama");
    press(&mut h, "Back");
    h.get_by_label("Add a provider");
}

#[test]
fn close_closes_the_window_and_opening_again_looks_afresh() {
    let f = Features::default();
    let mut h = open(&f);
    h.get_by_label("Close").click();
    settle(&mut h);
    assert!(h.query_by_label("Hosted services").is_none());
    h.get_by_label("Set up AI").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().scans, 2);
    assert_eq!(f.0.borrow().refreshes, 2);
}

// ---- Advanced: which model each tool uses ----

fn choice(connection: &str, model: &str, label: &str, local: bool) -> ToolChoice {
    ToolChoice { connection: ConnectionId::new(connection).unwrap(), model: ModelId::new(model).unwrap(), label: label.into(), local }
}

fn tool(feature: &str, choices: Vec<ToolChoice>) -> ToolState {
    ToolState { feature: feature.into(), choices, bound: None, status: ToolStatus::NoChoice, can_test: true, costly: false }
}

fn ready_tool(feature: &str) -> ToolState {
    let mut t = tool(feature, vec![choice("openai", "m", "Model, OpenAI", false)]);
    t.bound = Some((ConnectionId::new("openai").unwrap(), ModelId::new("m").unwrap()));
    t.status = ToolStatus::Ready { label: "Model, OpenAI".into(), local: false };
    t
}

/// Generate images has two models to pick from; nothing else is connected.
fn with_image_choices(f: &Features) {
    let mut w = f.0.borrow_mut();
    w.inventory = bare_comfy();
    w.tools = vec![tool(
        "generate-image",
        vec![choice("openai", "gpt-image-1", "GPT Image 1, OpenAI", false), choice("local", "flux", "Flux, My ComfyUI", true)],
    )];
}

fn ready_image_tool(f: &Features) {
    with_image_choices(f);
    let mut w = f.0.borrow_mut();
    let t = &mut w.tools[0];
    t.bound = Some((ConnectionId::new("openai").unwrap(), ModelId::new("gpt-image-1").unwrap()));
    t.status = ToolStatus::Ready { label: "GPT Image 1, OpenAI".into(), local: false };
}

fn open_advanced(f: &Features) -> Harness<'static, State> {
    let mut h = open(f);
    press(&mut h, "Advanced");
    h
}

/// The model pickers. The launcher's own "Suite order" box is behind the dialog.
fn pickers<'a>(h: &'a Harness<'_, State>) -> Vec<egui_kittest::Node<'a>> {
    h.query_all_by_role(Role::ComboBox).filter(|n| n.accesskit_node().value().as_deref() != Some("Suite order")).collect()
}

fn picker_text(h: &Harness<'_, State>) -> String {
    pickers(h)[0].accesskit_node().value().map(|v| v.to_string()).unwrap_or_default()
}

fn pick(h: &mut Harness<'_, State>, label: &str) {
    pickers(h)[0].click();
    settle(h);
    h.get_by_label_contains(label).click();
    settle(h);
}

fn asking() -> Sample {
    Sample::NeedsConsent {
        feature: "generate-image".into(),
        connection: "OpenAI".into(),
        destination: "https://api.openai.com".into(),
        items: vec!["text".into()],
        billing: Billing::Metered,
    }
}

#[test]
fn advanced_lists_only_the_tools_that_have_a_model_and_back_returns() {
    let f = Features::default();
    with_image_choices(&f);
    let mut h = open_advanced(&f);
    h.get_by_label("Servers, and which model each tool uses");
    assert_eq!(pickers(&h).len(), 1, "only generate images has anything to choose between");
    press(&mut h, "Back");
    h.get_by_label("Hosted services");

    let f = Features::default();
    let mut h = open_advanced(&f);
    h.get_by_label("No tool has a connected model yet.");
    assert!(pickers(&h).is_empty());
    press(&mut h, "Back");
}

#[test]
fn picking_a_model_binds_the_tool() {
    let f = Features::default();
    with_image_choices(&f);
    let mut h = open_advanced(&f);
    assert_eq!(picker_text(&h), "Choose a model");
    pick(&mut h, "GPT Image 1, OpenAI");
    assert_eq!(
        f.0.borrow().chosen,
        [("generate-image".to_owned(), Some((ConnectionId::new("openai").unwrap(), ModelId::new("gpt-image-1").unwrap())))]
    );
    h.get_by_label("Ready. Sends to a hosted service.");
}

#[test]
fn a_local_model_is_marked_as_staying_on_this_computer() {
    let f = Features::default();
    with_image_choices(&f);
    {
        let mut w = f.0.borrow_mut();
        let t = &mut w.tools[0];
        t.bound = Some((ConnectionId::new("local").unwrap(), ModelId::new("flux").unwrap()));
        t.status = ToolStatus::Ready { label: "Flux, My ComfyUI".into(), local: true };
    }
    let mut h = open_advanced(&f);
    h.get_by_label("Ready. Runs on this computer.");
    pickers(&h)[0].click();
    settle(&mut h);
    h.get_by_label_contains("(this computer)");
}

#[test]
fn a_refusal_from_the_shell_is_shown_and_nothing_changes() {
    let f = Features::default();
    with_image_choices(&f);
    f.0.borrow_mut().choose_refused = Some("That model cannot do this job.".into());
    let mut h = open_advanced(&f);
    pick(&mut h, "GPT Image 1, OpenAI");
    assert!(f.0.borrow().chosen.is_empty());
    h.get_by_label_contains("That model cannot do this job.");
}

#[test]
fn a_choice_that_stopped_working_says_why_and_automatic_takes_the_best_again() {
    let f = Features::default();
    with_image_choices(&f);
    {
        let mut w = f.0.borrow_mut();
        let t = &mut w.tools[0];
        t.bound = Some((ConnectionId::new("gone").unwrap(), ModelId::new("gpt-image-1").unwrap()));
        t.status = ToolStatus::Unavailable("Connection OpenAI is switched off.".into());
    }
    let mut h = open_advanced(&f);
    assert_eq!(picker_text(&h), "Chosen model unavailable");
    h.get_by_label("Connection OpenAI is switched off.");
    assert!(h.get_by_label("Test it").accesskit_node().is_disabled(), "no test of a model that cannot run");
    pickers(&h)[0].click();
    settle(&mut h);
    h.get_by_label("Automatic (best available)").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().chosen, [("generate-image".to_owned(), None)]);
}

#[test]
fn test_it_is_off_until_a_working_model_is_chosen() {
    let f = Features::default();
    with_image_choices(&f);
    let h = open_advanced(&f);
    assert!(h.get_by_label("Test it").accesskit_node().is_disabled());
}

#[test]
fn test_it_runs_the_sample_and_shows_that_it_worked() {
    let f = Features::default();
    ready_image_tool(&f);
    let mut h = open_advanced(&f);
    h.get_by_label("Test it").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().tests, [("generate-image".to_owned(), false)]);
    h.get_by_label("Testing…");
    f.0.borrow_mut().sample = Sample::Passed { feature: "generate-image".into(), summary: "Returned 1 image result(s)".into(), millis: 1_250 };
    settle(&mut h);
    h.get_by_label("Works. Returned 1 image result(s), in 1.3 s.");
    h.get_by_label("Done").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().dismissals, 1);
    h.get_by_label("Test it");
}

#[test]
fn a_failed_test_says_why_and_can_be_tried_again() {
    let f = Features::default();
    ready_image_tool(&f);
    f.0.borrow_mut().sample = Sample::Failed { feature: "generate-image".into(), message: "the service said no".into() };
    let mut h = open_advanced(&f);
    h.get_by_label("The test failed: the service said no");
    h.get_by_label("Try again").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().tests, [("generate-image".to_owned(), false)]);
}

#[test]
fn a_hosted_test_asks_first_and_names_where_it_goes_and_what_it_costs() {
    let f = Features::default();
    ready_image_tool(&f);
    f.0.borrow_mut().sample = asking();
    let mut h = open_advanced(&f);
    h.get_by_label("This sends a small sample (text) to https://api.openai.com through OpenAI. You are billed for it by the service.");
    assert!(f.0.borrow().tests.is_empty(), "nothing was sent by looking at the question");
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().dismissals, 1);
    assert!(f.0.borrow().tests.is_empty());
    // Asked again, agreeing sends it.
    f.0.borrow_mut().sample = asking();
    settle(&mut h);
    h.get_by_label("Send the test").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().tests, [("generate-image".to_owned(), true)]);
}

#[test]
fn a_running_test_can_be_cancelled() {
    let f = Features::default();
    ready_image_tool(&f);
    f.0.borrow_mut().sample = Sample::Running { feature: "generate-image".into() };
    let mut h = open_advanced(&f);
    h.get_by_label("Testing…");
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert_eq!(f.0.borrow().test_cancels, 1);
}

#[test]
fn a_sample_for_one_tool_does_not_show_on_another() {
    let f = Features::default();
    ready_image_tool(&f);
    {
        let mut w = f.0.borrow_mut();
        let mut video = tool("generate-video", vec![choice("openai", "sora", "Sora, OpenAI", false)]);
        video.bound = Some((ConnectionId::new("openai").unwrap(), ModelId::new("sora").unwrap()));
        video.status = ToolStatus::Ready { label: "Sora".into(), local: false };
        video.costly = true;
        w.tools.push(video);
        w.sample = Sample::Passed { feature: "generate-image".into(), summary: "Returned 1 image result(s)".into(), millis: 900 };
    }
    let h = open_advanced(&f);
    assert_eq!(h.query_all_by_label_contains("Works.").count(), 1, "only under its own tool");
    h.get_by_label("A hosted video sample can cost noticeably more than an image");
}
