//! The Set up AI window: one place to set up anything, and what each thing switches on.
//!
//! The list is organised by what a person sets up (a model on this computer, a hosted service), not
//! by tools. Each card says which tools it switches on and in which Craft apps; tools turn on by
//! themselves as things are set up (`ToolDesk::auto_bind`). Which model each tool uses is only
//! something to change under Advanced.

use std::time::Duration;

use craft_ai_features::{Feature, Route, SetupOption, State};
use craft_ai_model::{ConnectionId, PRESETS, Preset};
use egui::{RichText, Ui, vec2};

use super::text::{blocker_text, bytes};
use super::{FeaturesPort, FeaturesUi, Install, Outcome, Page, Sample, Snapshot, ToolStatus};
use crate::app::Host;
use crate::connections::{ConnectionsUi, KeyView, Row, Screen as ConnScreen};
use crate::i18n::fmt;
use crate::theme::Tokens;
use crate::widgets::{self, Kind};

/// What a press on the home page asks for.
enum Act {
    None,
    Local(String),
    Connect(&'static Preset),
    Edit(ConnectionId),
    Remove(ConnectionId),
}

/// A saved connection that is the hosted service `preset`, if there is one.
fn connected<'a>(rows: &'a [Row], preset: &Preset) -> Option<&'a Row> {
    let same = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');
    rows.iter().find(|r| r.provider == preset.provider && same(&r.endpoint, preset.endpoint))
}

/// Whether a connection can be used right now.
fn working(row: &Row) -> bool {
    row.enabled && matches!(row.key, KeyView::Saved | KeyView::NotNeeded)
}

impl FeaturesUi {
    /// Draws the window. Returns true when the person closed it.
    pub fn show_setup(&mut self, ui: &mut Ui, host: &mut dyn Host, conns: &mut ConnectionsUi) -> bool {
        let snapshot = {
            let Some(port) = host.features() else { return true };
            self.ensure_loaded(port);
            port.snapshot()
        };
        if let Some(port) = host.connections() {
            conns.ensure_loaded(port);
        }
        let busy = snapshot.scanning
            || matches!(snapshot.install, Install::Running { .. })
            || matches!(snapshot.sample, Sample::Running { .. })
            || conns.is_testing();
        if busy {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        let t = Tokens::get(ui.ctx());
        let screen = ui.ctx().content_rect();
        // The history is a table and a chart: it gets a wider, taller window than the setup cards.
        let history = self.page == Page::History;
        let width = (if history { 920.0f32 } else { 760.0 }).min(screen.width() - 80.0);
        let height = if history { (screen.height() * 0.84).clamp(360.0, 880.0) } else { (screen.height() * 0.74).clamp(320.0, 640.0) };
        ui.set_width(width);

        let mut close = false;
        let mut advanced = false;
        let mut history = false;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(widgets::heading(tl!("Set up AI"), 22.0, t.ink_strong));
                widgets::hud(ui, tl!("For every Craft app"), t.faint);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                close = widgets::button(ui, tl!("Close"), Kind::Secondary, 90.0).clicked();
                if self.page == Page::Home {
                    advanced = widgets::button(ui, tl!("Advanced"), Kind::Secondary, 0.0)
                        .on_hover_text(tl!("Add another server, or choose which model each tool uses"))
                        .clicked();
                    history = widgets::button(ui, tl!("History"), Kind::Secondary, 0.0)
                        .on_hover_text(tl!("Every AI job your Craft apps have run, and what it cost"))
                        .clicked();
                }
            });
        });
        ui.add_space(14.0);
        if advanced {
            self.page = Page::Advanced;
        }
        if history && let Some(port) = host.features() {
            self.open_history(port);
        }

        ui.allocate_ui_with_layout(vec2(width, height), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt("setup-ai").auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width(width - 16.0);
                self.page_contents(ui, host, conns, &snapshot);
            });
        });
        close
    }

    fn page_contents(&mut self, ui: &mut Ui, host: &mut dyn Host, conns: &mut ConnectionsUi, snapshot: &Snapshot) {
        match self.page.clone() {
            Page::Home => {
                let act = self.home(ui, snapshot, conns);
                self.apply(act, conns);
            }
            Page::Local(id) => {
                let Some(port) = host.features() else { return };
                if let Outcome::AddConnection(preset) = self.setup(ui, &id, snapshot, port) {
                    self.begin_connect(conns, Some(preset));
                }
            }
            Page::Connect => {
                if matches!(conns.screen, ConnScreen::List) {
                    // The form was saved, cancelled or the removal confirmed: back to the list.
                    conns.open();
                    self.back();
                } else if let Some(port) = host.connections() {
                    conns.show(ui, port);
                }
            }
            Page::History => {
                let Some(port) = host.features() else { return };
                self.history_page(ui, port);
            }
            Page::Advanced => {
                let Some(port) = host.features() else { return };
                match self.advanced(ui, snapshot, port) {
                    Outcome::AddConnection(preset) => self.begin_connect(conns, Some(preset)),
                    Outcome::AddServer => self.begin_connect(conns, None),
                    Outcome::None | Outcome::Close => {}
                }
            }
        }
    }

    /// Opens the connection form for a service, or the list of every service when `None`.
    fn begin_connect(&mut self, conns: &mut ConnectionsUi, preset: Option<&'static Preset>) {
        match preset {
            Some(preset) => conns.begin_add(preset),
            None => conns.begin_pick(),
        }
        self.page = Page::Connect;
        self.notice = None;
    }

    fn apply(&mut self, act: Act, conns: &mut ConnectionsUi) {
        match act {
            Act::None => {}
            Act::Local(feature) => self.begin_setup(&feature),
            Act::Connect(preset) => self.begin_connect(conns, Some(preset)),
            Act::Edit(id) => {
                conns.begin_edit(&id);
                self.page = Page::Connect;
            }
            Act::Remove(id) => {
                conns.begin_remove(&id);
                self.page = Page::Connect;
            }
        }
    }

    fn home(&self, ui: &mut Ui, snapshot: &Snapshot, conns: &ConnectionsUi) -> Act {
        let t = Tokens::get(ui.ctx());
        let mut act = Act::None;
        ui.add(
            egui::Label::new(
                RichText::new(tl!("Set up one or more of these and the AI tools switch on in every Craft app that has them.")).color(t.muted),
            )
            .wrap(),
        );
        ui.add_space(14.0);
        self.switched_on(ui, snapshot);
        ui.add_space(18.0);

        for (heading, local) in [(tl!("On this computer"), true), (tl!("Hosted services"), false)] {
            let options: Vec<&SetupOption> = self.options.iter().filter(|o| matches!(o.route, Route::Local { .. }) == local).collect();
            if options.is_empty() {
                continue;
            }
            widgets::hud(ui, heading, t.faint);
            ui.add_space(6.0);
            for option in options {
                ui.push_id(&option.id, |ui| {
                    if let Some(next) = self.card(ui, option, snapshot, conns) {
                        act = next;
                    }
                });
                ui.add_space(8.0);
            }
            ui.add_space(8.0);
        }

        let unmatched: Vec<&Row> = conns.rows.iter().filter(|r| !self.is_listed(r)).collect();
        if !unmatched.is_empty() {
            widgets::hud(ui, tl!("Your other connections"), t.faint);
            ui.add_space(6.0);
            for row in unmatched {
                ui.push_id(row.id.as_str(), |ui| {
                    if let Some(next) = other_row(ui, row) {
                        act = next;
                    }
                });
                ui.add_space(6.0);
            }
            ui.add_space(8.0);
        }

        act
    }

    /// Whether a saved connection is one of the hosted services the list already shows a card for.
    fn is_listed(&self, row: &Row) -> bool {
        self.options.iter().any(|o| match &o.route {
            Route::Service { preset } => PRESETS.iter().find(|p| p.key == preset).is_some_and(|p| connected(std::slice::from_ref(row), p).is_some()),
            Route::Local { .. } => false,
        })
    }

    /// The tools that are on right now, or a line saying none are yet.
    fn switched_on(&self, ui: &mut Ui, snapshot: &Snapshot) {
        let t = Tokens::get(ui.ctx());
        widgets::hud(ui, tl!("Switched on"), t.faint);
        ui.add_space(4.0);
        let on: Vec<&Feature> = self.features.iter().filter(|f| ready(snapshot, &f.id)).collect();
        if on.is_empty() {
            ui.label(RichText::new(tl!("Nothing yet. Set something up below.")).color(t.muted));
            return;
        }
        ui.horizontal_wrapped(|ui| {
            for feature in on {
                ui.label(RichText::new("●").color(t.good));
                ui.label(RichText::new(crate::i18n::t(&feature.title)).color(t.ink_strong));
                ui.add_space(8.0);
            }
        });
    }

    fn card(&self, ui: &mut Ui, option: &SetupOption, snapshot: &Snapshot, conns: &ConnectionsUi) -> Option<Act> {
        let t = Tokens::get(ui.ctx());
        let mut act = None;
        egui::Frame::new().stroke(t.hairline()).inner_margin(14).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::heading(crate::i18n::t(&option.title), 16.0, t.ink_strong));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &option.route {
                    Route::Local { feature } => act = self.local_status(ui, feature, snapshot),
                    Route::Service { preset } => act = service_status(ui, preset, conns),
                });
            });
            let note = match &option.route {
                Route::Local { .. } => tl!("Runs on this computer through ComfyUI. Your pictures never leave it.").to_owned(),
                Route::Service { .. } => crate::i18n::t(&option.note).to_owned(),
            };
            ui.add(egui::Label::new(RichText::new(note).size(12.0).color(t.muted)).wrap());
            ui.add_space(6.0);
            self.unlocks(ui, option, snapshot);
            if !option.usable {
                // Once the files are here there is nothing left to download, so the note must not
                // say there is.
                let files_in_place =
                    matches!(&option.route, Route::Local { feature } if self.plan_for(snapshot, feature).is_some_and(|p| p.state == State::Ready));
                let note = if files_in_place {
                    tl!("The files are in place. The apps cannot run this model yet, and the tool switches on when they can.")
                } else {
                    tl!("The apps cannot run this model yet. You can download it now, and the tool switches on when they can.")
                };
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(note).size(12.0).color(t.warn)).wrap());
            }
        });
        act
    }

    /// "Switches on: Generate images, Generative fill. In PhotoCraft, DesignCraft."
    fn unlocks(&self, ui: &mut Ui, option: &SetupOption, snapshot: &Snapshot) {
        let t = Tokens::get(ui.ctx());
        let features: Vec<&Feature> = option.unlocks.iter().filter_map(|id| self.features.iter().find(|f| &f.id == id)).collect();
        ui.horizontal_wrapped(|ui| {
            widgets::hud(ui, if option.usable { tl!("Switches on") } else { tl!("Planned for") }, t.faint);
            ui.add_space(4.0);
            for (i, feature) in features.iter().enumerate() {
                if i > 0 {
                    ui.label(RichText::new("·").color(t.faint));
                }
                let colour = if ready(snapshot, &feature.id) { t.good } else { t.ink };
                ui.label(RichText::new(crate::i18n::t(&feature.title)).color(colour));
            }
        });
        let mut apps: Vec<&str> = Vec::new();
        for feature in &features {
            for usage in &feature.uses {
                if !apps.contains(&usage.app.as_str()) {
                    apps.push(&usage.app);
                }
            }
        }
        if !apps.is_empty() {
            ui.add(egui::Label::new(RichText::new(fmt(tl!("In {apps}"), &[("apps", &apps.join(", "))])).size(12.0).color(t.muted)).wrap());
        }
    }

    /// The right-hand side of a local card: ready, or a button saying what it takes.
    fn local_status(&self, ui: &mut Ui, feature: &str, snapshot: &Snapshot) -> Option<Act> {
        let t = Tokens::get(ui.ctx());
        let plan = self.plan_for(snapshot, feature)?;
        match plan.state {
            State::Ready => {
                widgets::badge(
                    ui,
                    if self.options.iter().any(|o| o.route == Route::Local { feature: feature.to_owned() } && !o.usable) {
                        tl!("Files in place")
                    } else {
                        tl!("Ready")
                    },
                    t.good,
                );
                None
            }
            State::NeedsInstall => {
                let label = if plan.download_bytes > 0 {
                    fmt(tl!("Set up, {size}"), &[("size", &bytes(plan.download_bytes))])
                } else {
                    tl!("Set up").to_owned()
                };
                widgets::button(ui, &label, Kind::Primary, 0.0).clicked().then(|| Act::Local(feature.to_owned()))
            }
            State::NoRuntime => {
                ui.label(RichText::new(tl!("ComfyUI was not found on this computer")).size(12.0).color(t.muted));
                None
            }
            State::Blocked => {
                ui.label(RichText::new(blocker_text(plan.blockers.first())).size(12.0).color(t.bad));
                None
            }
            State::NoLocalRoute => None,
        }
    }
}

fn service_status(ui: &mut Ui, preset_key: &str, conns: &ConnectionsUi) -> Option<Act> {
    let t = Tokens::get(ui.ctx());
    let preset = PRESETS.iter().find(|p| p.key == preset_key)?;
    let Some(row) = connected(&conns.rows, preset) else {
        return widgets::button(ui, tl!("Connect"), Kind::Primary, 0.0).clicked().then_some(Act::Connect(preset));
    };
    let mut act = None;
    if widgets::button(ui, tl!("Remove"), Kind::Secondary, 0.0).clicked() {
        act = Some(Act::Remove(row.id.clone()));
    }
    if widgets::button(ui, tl!("Edit"), Kind::Secondary, 0.0).clicked() {
        act = Some(Act::Edit(row.id.clone()));
    }
    if working(row) {
        widgets::badge(ui, tl!("Connected"), t.good);
    } else if !row.enabled {
        widgets::badge(ui, tl!("Turned off"), t.faint);
    } else {
        widgets::badge(ui, tl!("Key missing"), t.bad);
    }
    act
}

/// A saved connection that is not one of the listed services (a local server, a custom address).
fn other_row(ui: &mut Ui, row: &Row) -> Option<Act> {
    let t = Tokens::get(ui.ctx());
    let mut act = None;
    egui::Frame::new().stroke(t.hairline()).inner_margin(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(&row.label).color(if row.enabled { t.ink_strong } else { t.faint }));
            if row.local {
                widgets::badge(ui, tl!("This computer"), t.good);
            } else {
                widgets::badge(ui, tl!("Leaves this computer"), t.warn);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, tl!("Remove"), Kind::Secondary, 0.0).clicked() {
                    act = Some(Act::Remove(row.id.clone()));
                }
                if widgets::button(ui, tl!("Edit"), Kind::Secondary, 0.0).clicked() {
                    act = Some(Act::Edit(row.id.clone()));
                }
            });
        });
    });
    act
}

/// Whether a tool is switched on: it has a model that works right now.
fn ready(snapshot: &Snapshot, feature: &str) -> bool {
    snapshot.tools.iter().any(|t| t.feature == feature && matches!(t.status, ToolStatus::Ready { .. }))
}

impl FeaturesUi {
    /// Which model each tool uses, and the test that proves it. Only tools with a connected model.
    fn advanced(&mut self, ui: &mut Ui, snapshot: &Snapshot, port: &mut dyn FeaturesPort) -> Outcome {
        let t = Tokens::get(ui.ctx());
        crate::connections::title(ui, tl!("Advanced"), tl!("Servers, and which model each tool uses"));
        ui.add(
            egui::Label::new(
                RichText::new(tl!("Tools pick the best model that is connected on their own. Change one here, or test that it works."))
                    .color(t.muted),
            )
            .wrap(),
        );
        ui.add_space(14.0);
        let mut outcome = Outcome::None;
        widgets::hud(ui, tl!("Other servers"), t.faint);
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(tl!("Ollama, LM Studio, ComfyUI or any OpenAI-compatible server")).size(12.0).color(t.muted)).wrap());
        ui.add_space(6.0);
        if widgets::button(ui, tl!("Add another server"), Kind::Secondary, 0.0).clicked() {
            outcome = Outcome::AddServer;
        }
        ui.add_space(14.0);
        super::tool::hline(ui);
        ui.add_space(14.0);
        widgets::hud(ui, tl!("Models"), t.faint);
        ui.add_space(8.0);
        let mut shown = 0;
        for feature in self.features.clone() {
            if !snapshot.tools.iter().any(|s| s.feature == feature.id && !s.choices.is_empty()) {
                continue;
            }
            shown += 1;
            ui.label(widgets::heading(crate::i18n::t(&feature.title), 16.0, t.ink_strong));
            ui.add_space(4.0);
            if let Some(next) = self.tool_block(ui, &feature, snapshot, port) {
                outcome = next;
            }
            ui.add_space(10.0);
            super::tool::hline(ui);
            ui.add_space(10.0);
        }
        if shown == 0 {
            ui.label(RichText::new(tl!("No tool has a connected model yet.")).color(t.muted));
            ui.add_space(8.0);
        }
        if let Some((text, error)) = &self.notice {
            ui.add(egui::Label::new(RichText::new(text).color(if *error { t.bad } else { t.muted })).wrap());
            ui.add_space(8.0);
        }
        if widgets::button(ui, tl!("Back"), Kind::Secondary, 90.0).clicked() {
            self.back();
        }
        outcome
    }
}
