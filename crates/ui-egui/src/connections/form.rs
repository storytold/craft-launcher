//! The add and edit form. Adding starts from a chosen service, so the form asks only for what that
//! service needs: a hosted one wants a key, a local one wants its address. The name, the address
//! of a hosted service and the rarely used options sit under Details.

use craft_ai_model::{Field, Locality, PRESETS, Preset};
use egui::{RichText, Ui};

use super::list::{test_text, title};
use super::pick::{Group, group};
use super::{ConnectionsPort, ConnectionsUi, Screen, TestState};
use crate::i18n::fmt;
use crate::theme::Tokens;
use crate::widgets::{self, Kind};

enum Act {
    Save,
    Test,
    /// Back to the list of services while adding, or to the connections while editing.
    Back,
}

impl ConnectionsUi {
    pub(super) fn form(&mut self, ui: &mut Ui, port: &mut dyn ConnectionsPort) {
        let t = Tokens::get(ui.ctx());
        let testing = self.is_testing();
        let mut act = None;
        let Screen::Form(form) = &mut self.screen else { return };
        let adding = form.draft.editing.is_none();
        let via_pick = form.via_pick;
        let preset: Option<&'static Preset> = PRESETS.iter().find(|p| form.preset == Some(p.key));
        let hosted = adding && preset.is_some_and(|p| group(p) == Group::Hosted);

        if adding {
            let name = preset.map_or_else(|| tl!("a provider").to_owned(), |p| crate::i18n::t(p.name).to_owned());
            title(ui, &fmt(tl!("Connect {name}"), &[("name", &name)]), "");
            if let Some(p) = preset {
                ui.add(egui::Label::new(RichText::new(crate::i18n::t(p.note)).color(t.muted)).wrap());
                ui.add_space(14.0);
            }
        } else {
            title(ui, tl!("Edit connection"), "");
        }

        let bad = |field: Field| form.error.as_ref().is_some_and(|e| e.field == Some(field));
        let label_colour = |field: Field| if bad(field) { t.bad } else { t.faint };
        let mut edited = false;
        let mut moved = false;

        // What the person has to give: a hosted service's key, or a local one's address.
        if !hosted {
            widgets::hud(ui, tl!("Address"), label_colour(Field::Address));
            moved |= ui.add(egui::TextEdit::singleline(&mut form.draft.endpoint).desired_width(f32::INFINITY).hint_text("https://…")).changed();
            ui.add_space(8.0);
        }
        if form.draft.uses_key {
            widgets::hud(ui, tl!("Key"), label_colour(Field::Key));
            let hint = if adding { tl!("Paste the key here") } else { tl!("Leave empty to keep the saved key") };
            edited |= ui.add(egui::TextEdit::singleline(&mut form.draft.key).password(true).desired_width(f32::INFINITY).hint_text(hint)).changed();
            ui.add_space(8.0);
        }
        match form.draft.locality() {
            Some(Locality::Local) => {
                ui.label(RichText::new(tl!("Data sent to this connection stays on this computer.")).size(12.0).color(t.good));
            }
            Some(Locality::Remote) => {
                ui.label(RichText::new(tl!("Data sent to this connection leaves this computer.")).size(12.0).color(t.warn));
            }
            None => {}
        }
        ui.add_space(8.0);

        let details_open = !adding || bad(Field::Label) || (hosted && bad(Field::Address));
        egui::CollapsingHeader::new(RichText::new(tl!("Details")).color(t.muted)).id_salt("details").default_open(details_open).show(ui, |ui| {
            widgets::hud(ui, tl!("Name"), label_colour(Field::Label));
            ui.add(egui::TextEdit::singleline(&mut form.draft.label).desired_width(f32::INFINITY));
            if hosted {
                ui.add_space(8.0);
                widgets::hud(ui, tl!("Address"), label_colour(Field::Address));
                moved |= ui.add(egui::TextEdit::singleline(&mut form.draft.endpoint).desired_width(f32::INFINITY)).changed();
            }
            ui.add_space(8.0);
            edited |= ui.checkbox(&mut form.draft.uses_key, tl!("Uses a key")).changed();
        });
        edited |= moved;

        if let Some(e) = &form.error {
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(&e.message).color(t.bad)).wrap());
        }
        // Changing the address or key makes an earlier test, and the models it found, out of date.
        if moved {
            form.draft.models.clear();
        }
        if edited && matches!(form.test, TestState::Passed { .. } | TestState::Failed(_)) {
            form.test = TestState::Idle;
        }
        let (line, colour) = test_text(&form.test, t);
        if let Some(line) = line {
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(line).size(12.0).color(colour)).wrap());
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, tl!("Save"), Kind::Primary, 90.0).clicked() {
                act = Some(Act::Save);
            }
            ui.add_enabled_ui(!testing, |ui| {
                if widgets::button(ui, tl!("Test"), Kind::Secondary, 90.0).clicked() {
                    act = Some(Act::Test);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if adding { tl!("Back") } else { tl!("Cancel") };
                if widgets::button(ui, label, Kind::Secondary, 90.0).clicked() {
                    act = Some(Act::Back);
                }
            });
        });

        match act {
            Some(Act::Save) => {
                self.submit(port);
            }
            Some(Act::Test) => self.test_form(port),
            Some(Act::Back) if via_pick => self.begin_pick(),
            Some(Act::Back) => self.back(),
            None => {}
        }
    }
}
