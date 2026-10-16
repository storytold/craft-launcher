//! Step one of adding a connection: which service. A list in plain groups (what leaves this
//! computer, and what never does) instead of a wall of buttons, so the choice explains itself.

use craft_ai_model::{PRESETS, Preset};
use egui::{Align2, CornerRadius, FontId, Response, RichText, Sense, Ui, pos2, vec2};

use super::ConnectionsUi;
use crate::fit::one_line;
use crate::theme::{self, Tokens};
use crate::widgets::{self, Kind};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Hosted,
    Local,
    Other,
}

/// Where a preset's data goes, from its address: https is somebody else's server, http is this
/// computer, and none is the person's own.
pub(crate) fn group(preset: &Preset) -> Group {
    if preset.endpoint.is_empty() {
        Group::Other
    } else if preset.endpoint.starts_with("http://") {
        Group::Local
    } else {
        Group::Hosted
    }
}

fn row(ui: &mut Ui, preset: &Preset) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
    let fill = if resp.hovered() { t.sunken } else { egui::Color32::TRANSPARENT };
    ui.painter().rect(rect, CornerRadius::ZERO, fill, t.hairline(), egui::StrokeKind::Inside);
    let name = crate::i18n::t(preset.name);
    let note = crate::i18n::t(preset.note);
    let width = rect.width() - 28.0 - 32.0;
    let title = one_line(ui, name, FontId::new(14.0, theme::medium()), t.ink_strong, width);
    let sub = one_line(ui, note, FontId::new(12.0, egui::FontFamily::Proportional), t.muted, width);
    ui.painter().galley(pos2(rect.left() + 14.0, rect.top() + 9.0), title, t.ink_strong);
    ui.painter().galley(pos2(rect.left() + 14.0, rect.top() + 29.0), sub, t.muted);
    ui.painter().text(pos2(rect.right() - 14.0, rect.center().y), Align2::RIGHT_CENTER, "→", FontId::new(14.0, theme::mono()), t.muted);
    resp.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name);
        info.current_text_value = Some(note.to_owned());
        info
    });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The three groups with their rows. Sets `chosen` when a row is pressed.
fn groups(ui: &mut Ui, chosen: &mut Option<&'static Preset>) {
    let t = Tokens::get(ui.ctx());
    for (group_id, heading, blurb) in [
        (Group::Hosted, tl!("Hosted services"), Some(tl!("You use your own key and the service bills you. What you send leaves this computer."))),
        (Group::Local, tl!("On this computer"), Some(tl!("Free. What you send never leaves this computer."))),
        (Group::Other, tl!("Other"), None),
    ] {
        ui.add_space(6.0);
        widgets::hud(ui, heading, t.faint);
        if let Some(blurb) = blurb {
            ui.add(egui::Label::new(RichText::new(blurb).size(12.0).color(t.muted)).wrap());
        }
        ui.add_space(6.0);
        for preset in PRESETS.iter().filter(|p| group(p) == group_id) {
            ui.push_id(preset.key, |ui| {
                if row(ui, preset).clicked() {
                    *chosen = Some(preset);
                }
            });
            ui.add_space(4.0);
        }
    }
}

impl ConnectionsUi {
    pub(super) fn pick(&mut self, ui: &mut Ui) {
        super::list::title(ui, tl!("Add a provider"), tl!("Choose the one you use"));
        // At the top, so it is in reach however long the list is.
        let cancel = widgets::button(ui, tl!("Cancel"), Kind::Secondary, 90.0).clicked();
        ui.add_space(8.0);
        let mut chosen: Option<&'static Preset> = None;
        groups(ui, &mut chosen);
        if let Some(preset) = chosen {
            self.begin_add(preset);
            if let super::Screen::Form(form) = &mut self.screen {
                form.via_pick = true;
            }
        } else if cancel {
            self.back();
        }
    }
}
