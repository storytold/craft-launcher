//! The other way to see the Add-ons page: themes as list rows, and plug-ins (or everything
//! installed) as cards. The page's grid/list switch picks between these and the original views
//! in `screen.rs`.
//!
//! Invariants: nothing here changes the store; every click becomes an [`Action`] that `screen.rs`
//! runs afterwards, and every sentence goes through `tl!`.

use craft_ext_core::manifest::Kind as ExtKind;
use egui::{Align, Layout, Rect, RichText, Sense, Ui};

use super::State;
use super::builtin::{self, BuiltIn};
use super::card;
use super::cards::{self, CARD_H};
use super::plan;
use super::screen::{Action, framed_rows, menu_button, sorted_apps, theme_card};
use super::source::Row;
use crate::i18n::fmt;
use crate::theme::{self, Tokens};
use crate::widgets::{self, Kind};

const PREVIEW: egui::Vec2 = egui::vec2(96.0, 60.0);

/// Themes as rows: a small picture, the name and where it is used, and the "Use in…" menu.
pub(super) fn themes_list(ui: &mut Ui, state: &State, actions: &mut Vec<Action>) {
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
        framed_rows(ui, themes, |ui, row| {
            theme_row(ui, row, actions);
            !row.worn.is_empty()
        });
    }
    if !built_in.is_empty() {
        ui.add_space(26.0);
        widgets::hud(ui, tl!("Built into your apps"), t.muted);
        ui.add_space(10.0);
        framed_rows(ui, built_in.iter(), |ui, b| {
            builtin_row(ui, b);
            false
        });
    }
}

fn preview(ui: &mut Ui, palette: Option<&craft_ext_core::palette::Palette>) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(PREVIEW, Sense::hover());
    if let Some(p) = palette {
        card::paint_preview(ui, rect, p);
    }
    ui.painter().rect_stroke(rect, egui::CornerRadius::ZERO, t.hairline(), egui::StrokeKind::Inside);
    ui.add_space(8.0);
}

/// How wide a row's text column is: what is left after the picture and room for the buttons, so
/// the chips (which fill their width) never push the row wider than the box.
fn text_width(ui: &Ui) -> f32 {
    (ui.available_width() - PREVIEW.x - 8.0 - 300.0).max(120.0)
}

fn theme_row(ui: &mut Ui, row: &Row, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    preview(ui, row.palette.as_ref());
    let worn = plan::worn_names(row);
    let text_w = text_width(ui);
    ui.vertical(|ui| {
        ui.set_width(text_w);
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.add(egui::Label::new(RichText::new(&row.name).size(15.0).family(theme::semibold()).color(t.ink_strong)).truncate());
        ui.horizontal(|ui| {
            if let Some(p) = &row.palette {
                widgets::badge(ui, if p.dark { tl!("Dark") } else { tl!("Light") }, t.muted);
            }
            ui.add(egui::Label::new(RichText::new(plan::trust_label(row.tier)).size(12.0).color(t.muted)).truncate());
        });
        if worn.is_empty() {
            ui.add(egui::Label::new(RichText::new(tl!("Not used yet")).size(12.5).color(t.faint)).truncate());
        } else {
            cards::app_chips(ui, &worn, 1);
        }
    });
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
            && widgets::button(ui, tl!("All apps"), Kind::Secondary, 0.0).on_hover_text(tl!("Use this theme in every app it is made for")).clicked()
        {
            actions.push(Action::Run(plan::everywhere_steps(row)));
        }
    });
}

fn builtin_row(ui: &mut Ui, b: &BuiltIn) {
    let t = Tokens::get(ui.ctx());
    preview(ui, Some(&b.palette));
    let text_w = text_width(ui);
    ui.vertical(|ui| {
        ui.set_width(text_w);
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.add(egui::Label::new(RichText::new(b.name).size(15.0).family(theme::semibold()).color(t.ink_strong)).truncate());
        ui.horizontal(|ui| {
            widgets::badge(ui, if b.palette.dark { tl!("Dark") } else { tl!("Light") }, t.muted);
            ui.add(egui::Label::new(RichText::new(fmt(tl!("Built into {app}"), &[("app", b.app_name)])).size(12.0).color(t.muted)).truncate());
        });
        ui.add(egui::Label::new(RichText::new(fmt(tl!("Choose it inside {app}"), &[("app", b.app_name)])).size(12.5).color(t.faint)).truncate());
    });
}

/// Plug-ins (grouped by app) or everything installed, as cards.
pub(super) fn items_grid(ui: &mut Ui, state: &State, actions: &mut Vec<Action>, only_plugins: bool) {
    let t = Tokens::get(ui.ctx());
    if only_plugins {
        let groups = plan::plugins_by_app(&state.rows, &state.focus);
        if groups.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| widgets::hud(ui, tl!("Nothing added yet"), t.muted));
            return;
        }
        for (app, rows) in groups {
            widgets::hud(ui, &plan::app_name(&app), t.muted);
            ui.add_space(8.0);
            row_cards(ui, state, actions, &rows);
            ui.add_space(18.0);
        }
        return;
    }
    let shown = plan::ordered_in(&state.rows, &state.focus);
    if shown.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| widgets::hud(ui, tl!("Nothing added yet"), t.muted));
        return;
    }
    row_cards(ui, state, actions, &shown);
}

fn row_cards(ui: &mut Ui, state: &State, actions: &mut Vec<Action>, rows: &[&Row]) {
    // Cards with a picture first, then the compact ones, each kind in its own rows: a short card
    // never sits beside a tall one with a hole under it. The order within each kind is kept.
    let mut rows: Vec<&Row> = rows.to_vec();
    rows.sort_by_key(|r| card_height(r) < CARD_H);
    let heights: Vec<f32> = rows.iter().map(|r| card_height(r)).collect();
    cards::grid_sized(ui, &heights, |ui, i, rect| {
        let row = rows[i];
        if row.kind == ExtKind::Theme && row.palette.is_some() {
            theme_card(ui, row, rect, actions);
        } else {
            plugin_card(ui, row, rect, state.confirm_remove.as_ref() == Some(&row.id), actions);
        }
    });
}

/// How tall a card needs to be. A theme has its preview and a plug-in its picture; a plug-in with
/// none starts at its name, so it is only as tall as what it shows: its details, the apps it is
/// on if any, and its buttons.
fn card_height(row: &Row) -> f32 {
    if row.thumbnail.is_some() || (row.kind == ExtKind::Theme && row.palette.is_some()) {
        CARD_H
    } else if plan::worn_names(row).is_empty() {
        PLAIN_H
    } else {
        PLAIN_H + 34.0
    }
}

/// A card with no picture and nothing in use: name, details, buttons.
const PLAIN_H: f32 = 156.0;

/// A plug-in (or resource) as a card: its picture when it has one, what it is, where it is on,
/// and Remove. Without a picture the card starts at its name and is only as tall as it needs.
fn plugin_card(ui: &mut Ui, row: &Row, rect: Rect, confirming: bool, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let Some(thumbnail) = &row.thumbnail else {
        let mut card = cards::top_plain(ui, rect, &row.name);
        plugin_card_body(&mut card, row, confirming, actions, 4.0);
        return;
    };
    let mut card = cards::top_with(ui, rect, row.id.as_str(), &row.name, false, |ui, picture| {
        let uri = format!("bytes://craft-plugin-thumbnail/{}/{}/{:016x}.png", row.id, row.version, thumbnail.cache_key);
        let alt = fmt(tl!("Thumbnail for {name}"), &[("name", &row.name)]);
        ui.painter().rect_filled(picture, egui::CornerRadius::ZERO, t.sunken);
        // A child of its own: `ui.put` would move the page's cursor back to the picture.
        let mut shown = ui.new_child(egui::UiBuilder::new().max_rect(picture).layout(Layout::top_down(Align::Center)));
        shown.add(egui::Image::from_bytes(uri, thumbnail.bytes.clone()).fit_to_exact_size(picture.size()).alt_text(alt));
    });
    plugin_card_body(&mut card, row, confirming, actions, 6.0);
}

/// What sits under a plug-in card's name: details, the apps it is on, and the buttons.
fn plugin_card_body(ui: &mut Ui, row: &Row, confirming: bool, actions: &mut Vec<Action>, lead: f32) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(lead);
    let details = format!("{} · v{} · {}", plan::kind_label(row.kind), row.version, plan::trust_label(row.tier));
    ui.add(egui::Label::new(RichText::new(details).size(12.0).color(t.muted)).truncate());
    ui.add_space(8.0);
    let worn = plan::worn_names(row);
    if !worn.is_empty() {
        cards::app_chips(ui, &worn, 1);
    }
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        ui.horizontal(|ui| {
            if confirming {
                if widgets::button(ui, tl!("Remove"), Kind::Danger, 0.0).clicked() {
                    actions.push(Action::Run(vec![plan::Step::Remove { id: row.id.clone() }]));
                }
                if widgets::button(ui, tl!("Keep"), Kind::Secondary, 0.0).clicked() {
                    actions.push(Action::CancelRemove);
                }
                return;
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
            if widgets::button(ui, tl!("Remove"), Kind::Secondary, 0.0).clicked() {
                actions.push(Action::AskRemove(row.id.clone()));
            }
        });
        if confirming {
            ui.label(RichText::new(tl!("Remove from every Craft app?")).color(t.muted));
        }
    });
}
