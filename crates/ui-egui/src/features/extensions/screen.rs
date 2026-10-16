//! Drawing the Extensions page and running what the person clicks.
//!
//! Drawing only collects [`Action`]s; they run afterwards against the source, so no drawing code
//! holds the store and the store is read again after every change.

use std::time::Duration;

use craft_ext_core::id::{AppId, ExtensionId};
use egui::{Align, Layout, Rect, RichText, Sense, Ui, pos2, vec2};

use super::card;
use super::plan::{self, Step};
use super::source::{ExtensionSource, Row};
use super::{Page, State, Tab};
use crate::app::{Host, LauncherUi};
use crate::i18n::{current, fmt, tr_ctx};
use crate::theme::{self, Tokens};
use crate::widgets::{self, Cell, Kind};

const CARD_MIN_W: f32 = 290.0;
const CARD_H: f32 = 268.0;
const PREVIEW_H: f32 = 124.0;
const CARD_PAD: f32 = 18.0;

/// Something the person asked for while the page was drawn.
enum Action {
    Tab(Tab),
    AddFile,
    AskRemove(ExtensionId),
    CancelRemove,
    Run(Vec<Step>),
    ShowAll,
}

impl LauncherUi {
    /// The Apps | Extensions switch at the top of the page. Nothing is drawn, and the apps stay
    /// in view, where this build has no extensions store.
    pub(crate) fn page_switch(&mut self, ui: &mut Ui, host: &mut dyn Host) {
        if host.extensions().is_none() {
            self.extensions.open = false;
            return;
        }
        let mut page = if self.extensions.open { Page::Extensions } else { Page::Apps };
        let options = [
            (Page::Apps, Cell { label: tl!("Apps"), tip: tl!("Install and update Craft apps"), count: None }),
            (Page::Extensions, Cell { label: tl!("Extensions"), tip: tl!("Themes for your Craft apps"), count: None }),
        ];
        if widgets::cells(ui, &mut page, &options) {
            self.extensions.open = page == Page::Extensions;
            self.extensions.loaded = false;
            self.extensions.confirm_remove = None;
        }
        ui.add_space(14.0);
    }

    /// Goes to the Extensions page, narrowed to what another app asked for: only what works with
    /// `app`, and/or only one `kind` (`theme`, `plugin` or `resource`). Anything that is not an app
    /// id or a kind is ignored. Does nothing visible where this build has no extensions store.
    pub fn open_extensions(&mut self, app: Option<&str>, kind: Option<&str>) {
        let (focus, themes_tab) = plan::focus_for(app, kind);
        self.extensions.open = true;
        self.extensions.loaded = false;
        self.extensions.confirm_remove = None;
        self.extensions.tab = if themes_tab { Tab::Themes } else { Tab::Installed };
        self.extensions.focus = focus;
    }

    /// Whether the Extensions page is the one showing.
    pub fn showing_extensions(&self) -> bool {
        self.extensions.open
    }

    /// The Extensions page.
    pub(crate) fn extensions_page(&mut self, ui: &mut Ui, host: &mut dyn Host) {
        let ctx = ui.ctx().clone();
        self.sync_extensions(&ctx, host);
        let t = Tokens::get(&ctx);
        let mut actions = Vec::new();
        let state = &self.extensions;
        ui.horizontal(|ui| {
            ui.label(widgets::heading(tl!("Extensions"), 24.0, t.ink_strong));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::button(ui, tl!("Add from file"), Kind::Primary, 0.0).clicked() {
                    actions.push(Action::AddFile);
                }
            });
        });
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(RichText::new(tl!("Themes change how your Craft apps look. Only add files from people you trust.")).color(t.muted))
                .wrap(),
        );
        ui.add_space(14.0);
        focus_banner(ui, &state.focus, &mut actions);
        let mut tab = state.tab;
        let tabs = [
            (
                Tab::Themes,
                Cell {
                    label: tl!("Themes"),
                    tip: tl!("Colour themes for your Craft apps"),
                    count: Some((plan::themes_in(&state.rows, &state.focus).len(), None)),
                },
            ),
            (
                Tab::Installed,
                Cell {
                    label: tr_ctx(current(), "filter", "Installed"),
                    tip: tl!("Everything you have added"),
                    count: Some((plan::ordered_in(&state.rows, &state.focus).len(), None)),
                },
            ),
        ];
        if widgets::cells(ui, &mut tab, &tabs) {
            actions.push(Action::Tab(tab));
        }
        ui.add_space(18.0);
        if let Some(reason) = &state.error {
            let text = fmt(tl!("Couldn't read the installed extensions: {reason}"), &[("reason", reason)]);
            ui.add(egui::Label::new(RichText::new(text).color(t.bad)).wrap());
            ui.add_space(10.0);
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            match state.tab {
                Tab::Themes => themes_grid(ui, state, &mut actions),
                Tab::Installed => installed_list(ui, state, &mut actions),
            }
            ui.add_space(16.0);
        });
        self.run_extension_actions(&ctx, host, actions);
    }

    /// Reads the store when the page opens and whenever something changed, in any app.
    fn sync_extensions(&mut self, ctx: &egui::Context, host: &mut dyn Host) {
        let now = ctx.input(|i| i.time);
        ctx.request_repaint_after(Duration::from_secs(1));
        if self.extensions.loaded && now < self.extensions.next_poll {
            return;
        }
        self.extensions.next_poll = now + 1.0;
        let Some(source) = host.extensions() else { return };
        if !self.extensions.loaded || source.generation() != self.extensions.generation {
            self.extensions.reload(source);
        }
    }

    fn run_extension_actions(&mut self, ctx: &egui::Context, host: &mut dyn Host, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Tab(tab) => self.extensions.tab = tab,
                Action::ShowAll => self.extensions.focus = plan::Focus::default(),
                Action::AskRemove(id) => self.extensions.confirm_remove = Some(id),
                Action::CancelRemove => self.extensions.confirm_remove = None,
                Action::AddFile => self.add_extension_file(ctx, host),
                Action::Run(steps) => self.run_extension_steps(ctx, host, &steps),
            }
        }
    }

    fn add_extension_file(&mut self, ctx: &egui::Context, host: &mut dyn Host) {
        let Some(path) = host.pick_file(tl!("Add an extension"), &["zip"]) else { return };
        let Some(source) = host.extensions() else { return };
        let outcome = source.add_file(&path);
        self.extensions.reload(source);
        match outcome {
            Ok(name) => self.toast(ctx, fmt(tl!("Added {name}"), &[("name", &name)]), false),
            Err(reason) => self.toast(ctx, fmt(tl!("Couldn't add that file: {reason}"), &[("reason", &reason)]), true),
        }
    }

    fn run_extension_steps(&mut self, ctx: &egui::Context, host: &mut dyn Host, steps: &[Step]) {
        let removed: Vec<String> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Remove { id } => self.extensions.rows.iter().find(|r| &r.id == id).map(|r| r.name.clone()),
                _ => None,
            })
            .collect();
        let Some(source) = host.extensions() else { return };
        let mut failure = None;
        for step in steps {
            let done = match step {
                Step::SetEnabled { app, id, on } => source.set_enabled(app, id, *on),
                Step::Wear { app, id } => source.wear(app, id.as_ref()),
                Step::Remove { id } => source.remove(id),
            };
            if let Err(reason) = done {
                failure = Some(reason);
                break;
            }
        }
        self.extensions.reload(source);
        self.extensions.confirm_remove = None;
        match failure {
            Some(reason) => self.toast(ctx, fmt(tl!("Couldn't change that: {reason}"), &[("reason", &reason)]), true),
            None => {
                for name in removed {
                    self.toast(ctx, fmt(tl!("Removed {name}"), &[("name", &name)]), false);
                }
            }
        }
    }
}

impl State {
    /// Reads the generation and then the rows, so a change made in between is noticed next time.
    pub(super) fn reload(&mut self, source: &mut dyn ExtensionSource) {
        self.generation = source.generation();
        match source.rows() {
            Ok(rows) => {
                self.rows = rows;
                self.error = None;
            }
            Err(reason) => self.error = Some(reason),
        }
        self.loaded = true;
    }
}

/// A button that opens a menu, drawn like the other square controls.
fn menu_button(ui: &mut Ui, label: &str, add_contents: impl FnOnce(&mut Ui)) {
    let t = Tokens::get(ui.ctx());
    let text = RichText::new(label.to_uppercase()).family(theme::mono_bold()).size(theme::HUD_SIZE).color(t.ink);
    let button = egui::Button::new(text).min_size(vec2(0.0, theme::CONTROL_H));
    egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
        ui.set_min_width(200.0);
        add_contents(ui);
    });
}

/// "Showing only works with PhotoCraft" and a way back, when another app narrowed the page.
fn focus_banner(ui: &mut Ui, focus: &plan::Focus, actions: &mut Vec<Action>) {
    if focus.is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let mut what = Vec::new();
    if let Some(app) = &focus.app {
        what.push(fmt(tl!("works with {app}"), &[("app", &plan::app_name(app))]));
    }
    match focus.kind {
        Some(craft_ext_core::manifest::Kind::Theme) => what.push(tl!("themes").to_string()),
        Some(craft_ext_core::manifest::Kind::Plugin) => what.push(tl!("plug-ins").to_string()),
        Some(craft_ext_core::manifest::Kind::Resource) => what.push(tl!("resources").to_string()),
        None => {}
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(fmt(tl!("Showing only {what}"), &[("what", &what.join(", "))])).color(t.muted));
        if widgets::button(ui, tl!("Show all"), Kind::Secondary, 0.0).clicked() {
            actions.push(Action::ShowAll);
        }
    });
    ui.add_space(10.0);
}

fn sorted_apps(row: &Row) -> Vec<&AppId> {
    let mut apps: Vec<&AppId> = row.apps.iter().collect();
    apps.sort_by_key(|a| plan::app_name(a).to_lowercase());
    apps
}

fn themes_grid(ui: &mut Ui, state: &State, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let themes = plan::themes_in(&state.rows, &state.focus);
    if themes.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            widgets::hud(ui, tl!("No themes yet"), t.muted);
            ui.add_space(6.0);
            ui.label(RichText::new(tl!("Add a theme from a file and it appears here.")).color(t.faint));
        });
        return;
    }
    let width = ui.available_width();
    // Neighbouring cards share their hairline, like the apps grid.
    let cols = ((width / CARD_MIN_W).floor() as usize).max(1);
    let card_w = (width + (cols as f32 - 1.0)) / cols as f32;
    let rows = themes.len().div_ceil(cols);
    let (area, _) = ui.allocate_exact_size(vec2(width, rows as f32 * (CARD_H - 1.0) + 1.0), Sense::hover());
    for (i, row) in themes.into_iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let min = area.min + vec2(c as f32 * (card_w - 1.0), r as f32 * (CARD_H - 1.0));
        theme_card(ui, row, Rect::from_min_size(min, vec2(card_w, CARD_H)), actions);
    }
}

fn theme_card(ui: &mut Ui, row: &Row, rect: Rect, actions: &mut Vec<Action>) {
    let Some(palette) = &row.palette else { return };
    let t = Tokens::get(ui.ctx());
    ui.painter_at(rect.expand(1.0)).rect(rect, egui::CornerRadius::ZERO, t.bg, t.hairline(), egui::StrokeKind::Inside);
    let picture = Rect::from_min_size(rect.min + vec2(1.0, 1.0), vec2(rect.width() - 2.0, PREVIEW_H));
    card::paint_preview(ui, picture, palette);
    ui.painter().hline(picture.x_range(), picture.bottom(), t.hairline());
    ui.interact(picture, ui.id().with(("theme-preview", row.id.as_str())), Sense::hover())
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, fmt(tl!("Preview of {name}"), &[("name", &row.name)])));

    let inner = Rect::from_min_max(pos2(rect.left() + CARD_PAD, picture.bottom() + 14.0), rect.max - vec2(CARD_PAD, CARD_PAD));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    let ui = &mut child;
    ui.add(egui::Label::new(RichText::new(&row.name).size(16.0).family(theme::semibold()).color(t.ink_strong)).truncate());
    ui.horizontal(|ui| {
        widgets::badge(ui, if palette.dark { tl!("Dark") } else { tl!("Light") }, t.muted);
        ui.add(egui::Label::new(RichText::new(plan::trust_label(row.tier)).size(12.0).color(t.muted)).truncate());
    });
    ui.add_space(2.0);
    match plan::in_use_line(row) {
        Some(apps) => {
            let text = fmt(tl!("In use: {apps}"), &[("apps", &apps)]);
            ui.add(egui::Label::new(RichText::new(text).size(12.5).color(t.accent_ink)).truncate());
        }
        None => {
            ui.add(egui::Label::new(RichText::new(tl!("Not used yet")).size(12.5).color(t.faint)).truncate());
        }
    }
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        ui.horizontal(|ui| {
            menu_button(ui, tl!("Use in…"), |ui| {
                for app in sorted_apps(row) {
                    let mut on = row.worn.contains(app);
                    if ui.checkbox(&mut on, plan::app_name(app)).changed() {
                        actions.push(Action::Run(plan::wear_steps(row, app)));
                        ui.close();
                    }
                }
            });
            if row.apps.len() > 1
                && row.worn.len() < row.apps.len()
                && widgets::button(ui, tl!("All apps"), Kind::Secondary, 0.0)
                    .on_hover_text(tl!("Use this theme in every app it is made for"))
                    .clicked()
            {
                actions.push(Action::Run(plan::everywhere_steps(row)));
            }
        });
    });
}

fn installed_list(ui: &mut Ui, state: &State, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let shown = plan::ordered_in(&state.rows, &state.focus);
    if shown.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            widgets::hud(ui, tl!("Nothing added yet"), t.muted);
        });
        return;
    }
    egui::Frame::new().stroke(t.hairline()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        for row in shown {
            let confirming = state.confirm_remove.as_ref() == Some(&row.id);
            let line = egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| installed_row(ui, row, confirming, actions));
            });
            ui.painter().hline(line.response.rect.x_range(), line.response.rect.bottom(), t.hairline());
        }
    });
}

fn installed_row(ui: &mut Ui, row: &Row, confirming: bool, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add(egui::Label::new(RichText::new(&row.name).size(15.0).family(theme::semibold()).color(t.ink_strong)).truncate());
        let details = format!("{} · v{} · {}", plan::kind_label(row.kind), row.version, plan::trust_label(row.tier));
        ui.add(egui::Label::new(RichText::new(details).size(12.5).color(t.muted)).truncate());
    });
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if confirming {
            if widgets::button(ui, tl!("Remove"), Kind::Danger, 0.0).clicked() {
                actions.push(Action::Run(vec![Step::Remove { id: row.id.clone() }]));
            }
            if widgets::button(ui, tl!("Keep"), Kind::Secondary, 0.0).clicked() {
                actions.push(Action::CancelRemove);
            }
            ui.label(RichText::new(tl!("Remove from every Craft app?")).color(t.muted));
            return;
        }
        if widgets::button(ui, tl!("Remove"), Kind::Secondary, 0.0).clicked() {
            actions.push(Action::AskRemove(row.id.clone()));
        }
        menu_button(ui, tl!("Switched on in…"), |ui| {
            for app in sorted_apps(row) {
                let mut on = row.enabled.contains(app);
                if ui.checkbox(&mut on, plan::app_name(app)).changed() {
                    actions.push(Action::Run(plan::enable_steps(row, app)));
                    ui.close();
                }
            }
        });
    });
}
