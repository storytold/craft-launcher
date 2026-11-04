//! Drawing the Extensions page and running what the person clicks.
//!
//! Drawing only collects [`Action`]s; they run afterwards against the source, so no drawing code
//! holds the store and the store is read again after every change.

use std::time::Duration;

use craft_ext_core::id::{AppId, ExtensionId};
use egui::{Align, Layout, Rect, RichText, Ui, pos2, vec2};

use super::builtin;
use super::cards::{self, CARD_H, CARD_H_SHORT};
use super::lists;
use super::plan::{self, Step};
use super::source::{ExtensionSource, Row};
use super::{Page, State, Tab};
use crate::app::{Host, LauncherUi};
use crate::i18n::{current, fmt, tr_ctx};
use crate::settings::ViewMode;
use crate::theme::{self, Tokens};
use crate::widgets::{self, Cell, Kind};

/// Something the person asked for while the page was drawn.
pub(super) enum Action {
    Tab(Tab),
    AddFile,
    AskRemove(ExtensionId),
    CancelRemove,
    Run(Vec<Step>),
    ShowAll,
}

impl LauncherUi {
    /// The Apps | Extensions switch, drawn inside the toolbar row of whichever page is showing.
    /// Nothing is drawn where this build has no extensions store.
    pub(crate) fn page_cells(&mut self, ui: &mut Ui, has_extensions: bool) {
        if !has_extensions {
            return;
        }
        let mut page = if self.extensions.open { Page::Extensions } else { Page::Apps };
        let options = [
            (Page::Apps, Cell { label: tl!("Apps"), tip: tl!("Install and update Craft apps"), count: None }),
            (Page::Extensions, Cell { label: tl!("Add-ons"), tip: tl!("Themes, plug-ins and resources for your Craft apps"), count: None }),
        ];
        // Laid out in the parent's direction: a row, so the options sit side by side, a gap apart.
        if widgets::cells_apart(ui, &mut page, &options, widgets::NAV_GAP) {
            self.extensions.open = page == Page::Extensions;
            self.extensions.loaded = false;
            self.extensions.confirm_remove = None;
        }
    }

    /// Goes to the Extensions page, narrowed to what another app asked for: only what works with
    /// `app`, and/or only one `kind` (`theme`, `plugin` or `resource`). Anything that is not an app
    /// id or a kind is ignored. Does nothing visible where this build has no extensions store.
    pub fn open_extensions(&mut self, app: Option<&str>, kind: Option<&str>) {
        let (focus, themes_tab) = plan::focus_for(app, kind);
        self.extensions.open = true;
        self.extensions.loaded = false;
        self.extensions.confirm_remove = None;
        self.extensions.tab = match focus.kind {
            Some(craft_ext_core::manifest::Kind::Plugin) => Tab::Plugins,
            _ if themes_tab => Tab::Themes,
            _ => Tab::Installed,
        };
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
        let (themes_n, plugins_n, installed_n) = {
            let state = &self.extensions;
            (
                plan::themes_in(&state.rows, &state.focus).len() + builtin::shown(&state.focus).len(),
                plan::ordered_in(&state.rows, &state.focus).iter().filter(|r| r.kind == craft_ext_core::manifest::Kind::Plugin).count(),
                plan::ordered_in(&state.rows, &state.focus).len(),
            )
        };
        let mut tab = self.extensions.tab;
        // Plug-ins, then everything installed, then themes.
        let tabs = [
            (Tab::Plugins, Cell { label: tl!("Plug-ins"), tip: tl!("Plug-ins"), count: Some((plugins_n, None)) }),
            (
                Tab::Installed,
                Cell { label: tr_ctx(current(), "filter", "Installed"), tip: tl!("Everything you have added"), count: Some((installed_n, None)) },
            ),
            (Tab::Themes, Cell { label: tl!("Themes"), tip: tl!("Colour themes for your Craft apps"), count: Some((themes_n, None)) }),
        ];
        // Themes open as cards and the lists as rows, until the person picks one for them all.
        let mut view = self.extensions.view.unwrap_or(if self.extensions.tab == Tab::Themes { ViewMode::Grid } else { ViewMode::Table });
        let before = view;
        // One row, like the apps page: Apps | Extensions, the list to show, and Add from file.
        ui.horizontal(|ui| {
            self.page_cells(ui, true);
            if crate::bar::dropdown(ui, "extensions_list", &mut tab, &tabs) {
                actions.push(Action::Tab(tab));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                crate::bar::view_toggle(ui, &mut view, [tl!("Show add-ons as cards"), tl!("Show add-ons as a list")]);
                ui.add_space(4.0);
                if widgets::button(ui, tl!("Add from file"), Kind::Primary, 0.0)
                    .on_hover_text(tl!("Themes change how your Craft apps look. Only add files from people you trust."))
                    .clicked()
                {
                    actions.push(Action::AddFile);
                }
            });
        });
        ui.add_space(14.0);
        if view != before {
            self.extensions.view = Some(view);
        }
        let state = &self.extensions;
        if state.demo {
            ui.label(RichText::new(tl!("Example data, not installed")).size(12.0).color(t.warn));
            ui.add_space(10.0);
        }
        focus_banner(ui, &state.focus, &mut actions);
        ui.add_space(18.0);
        if let Some(reason) = &state.error {
            let text = fmt(tl!("Couldn't read your installed add-ons: {reason}"), &[("reason", reason)]);
            ui.add(egui::Label::new(RichText::new(text).color(t.bad)).wrap());
            ui.add_space(10.0);
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            match (state.tab, view) {
                (Tab::Themes, ViewMode::Grid) => themes_grid(ui, state, &mut actions),
                (Tab::Themes, ViewMode::Table) => lists::themes_list(ui, state, &mut actions),
                (Tab::Plugins, ViewMode::Table) => installed_list(ui, state, &mut actions, true),
                (Tab::Installed, ViewMode::Table) => installed_list(ui, state, &mut actions, false),
                (Tab::Plugins, ViewMode::Grid) => lists::items_grid(ui, state, &mut actions, true),
                (Tab::Installed, ViewMode::Grid) => lists::items_grid(ui, state, &mut actions, false),
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
        self.demo = source.is_demo();
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
pub(super) fn menu_button(ui: &mut Ui, label: &str, add_contents: impl FnOnce(&mut Ui)) {
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

pub(super) fn sorted_apps(row: &Row) -> Vec<&AppId> {
    let mut apps: Vec<&AppId> = row.apps.iter().collect();
    apps.sort_by_key(|a| plan::app_name(a).to_lowercase());
    apps
}

fn themes_grid(ui: &mut Ui, state: &State, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let themes = plan::themes_in(&state.rows, &state.focus);
    let built_in = builtin::shown(&state.focus);
    if themes.is_empty() {
        ui.add_space(if built_in.is_empty() { 40.0 } else { 6.0 });
        ui.vertical_centered(|ui| {
            widgets::hud(ui, tl!("No themes yet"), t.muted);
            ui.add_space(6.0);
            ui.label(RichText::new(tl!("Add a theme from a file and it appears here.")).color(t.faint));
        });
    } else {
        cards::grid(ui, themes.len(), CARD_H, |ui, i, rect| theme_card(ui, themes[i], rect, actions));
    }
    if !built_in.is_empty() {
        ui.add_space(26.0);
        widgets::hud(ui, tl!("Built into your apps"), t.muted);
        ui.add_space(10.0);
        cards::grid(ui, built_in.len(), CARD_H_SHORT, |ui, i, rect| cards::builtin(ui, &built_in[i], rect));
    }
}

pub(super) fn theme_card(ui: &mut Ui, row: &Row, rect: Rect, actions: &mut Vec<Action>) {
    let Some(palette) = &row.palette else { return };
    let t = Tokens::get(ui.ctx());
    let worn = plan::worn_names(row);
    let mut card = cards::top(ui, rect, row.id.as_str(), &row.name, palette, !worn.is_empty());
    let ui = &mut card;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        widgets::badge(ui, if palette.dark { tl!("Dark") } else { tl!("Light") }, t.muted);
        ui.add(egui::Label::new(RichText::new(plan::trust_label(row.tier)).size(12.0).color(t.muted)).truncate());
    });
    ui.add_space(8.0);
    if worn.is_empty() {
        ui.add(egui::Label::new(RichText::new(tl!("Not used yet")).size(12.5).color(t.faint)).truncate());
    } else {
        cards::app_chips(ui, &worn, 2);
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

fn installed_list(ui: &mut Ui, state: &State, actions: &mut Vec<Action>, only_plugins: bool) {
    let t = Tokens::get(ui.ctx());
    if only_plugins {
        let groups = plan::plugins_by_app(&state.rows, &state.focus);
        if groups.is_empty() {
            empty_list(ui, &t);
            return;
        }
        for (app, rows) in groups {
            widgets::hud(ui, &plan::app_name(&app), t.muted);
            ui.add_space(8.0);
            installed_rows(ui, state, actions, rows);
            ui.add_space(18.0);
        }
        return;
    }
    let shown = plan::ordered_in(&state.rows, &state.focus);
    if shown.is_empty() {
        empty_list(ui, &t);
        return;
    }
    installed_rows(ui, state, actions, shown);
}

fn empty_list(ui: &mut Ui, t: &Tokens) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| widgets::hud(ui, tl!("Nothing added yet"), t.muted));
}

fn installed_rows<'a>(ui: &mut Ui, state: &State, actions: &mut Vec<Action>, shown: impl IntoIterator<Item = &'a Row>) {
    framed_rows(ui, shown, |ui, row| {
        let confirming = state.confirm_remove.as_ref() == Some(&row.id);
        installed_row(ui, row, confirming, actions);
        !row.worn.is_empty()
    });
}

/// Rows in one hairline box, each divided by a hairline. `draw` paints a row's contents and says
/// whether it is in use, which marks it with an accent bar at its left edge.
pub(super) fn framed_rows<T>(ui: &mut Ui, shown: impl IntoIterator<Item = T>, mut draw: impl FnMut(&mut Ui, T) -> bool) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::new().stroke(t.hairline()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        for item in shown {
            let mut in_use = false;
            let line = egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| in_use = draw(ui, item));
            });
            ui.painter().hline(line.response.rect.x_range(), line.response.rect.bottom(), t.hairline());
            if in_use {
                let r = line.response.rect;
                ui.painter().rect_filled(Rect::from_min_max(r.min, pos2(r.left() + 3.0, r.bottom())), egui::CornerRadius::ZERO, t.accent);
            }
        }
    });
}

fn installed_row(ui: &mut Ui, row: &Row, confirming: bool, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    if row.kind == craft_ext_core::manifest::Kind::Plugin {
        // With no picture there is no empty box: the text starts at the row's edge.
        if let Some(thumbnail) = &row.thumbnail {
            let uri = format!("bytes://craft-plugin-thumbnail/{}/{}/{:016x}.png", row.id, row.version, thumbnail.cache_key);
            let alt = fmt(tl!("Thumbnail for {name}"), &[("name", &row.name)]);
            ui.add(egui::Image::from_bytes(uri, thumbnail.bytes.clone()).fit_to_exact_size(vec2(48.0, 48.0)).corner_radius(6).alt_text(alt));
            ui.add_space(4.0);
        }
    }
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add(egui::Label::new(RichText::new(&row.name).size(15.0).family(theme::semibold()).color(t.ink_strong)).truncate());
        let details = format!("{} · v{} · {}", plan::kind_label(row.kind), row.version, plan::trust_label(row.tier));
        ui.add(egui::Label::new(RichText::new(details).size(12.5).color(t.muted)).truncate());
        let worn = plan::worn_names(row);
        if !worn.is_empty() {
            ui.add_space(6.0);
            cards::app_chips(ui, &worn, 2);
        }
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
