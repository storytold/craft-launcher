//! The Model folders page: where the person keeps model files on this computer.
//!
//! The suite never moves, renames or deletes anything in a folder. The page registers a folder
//! (one the person picks, or the one the launcher proposes), shows what was found in it by kind of
//! model, and lets them look again, choose which folder new downloads go to, or forget a folder.
//! All disk work goes through [`FeaturesPort`]; this file only draws and asks.

use egui::{RichText, Ui};

use super::{FeaturesUi, FolderCard};
use crate::app::Host;
use crate::theme::Tokens;
use crate::widgets::{self, Kind};

/// What the page has read from the port. `None` means read it again before drawing.
#[derive(Default)]
pub(crate) struct FoldersUi {
    cards: Option<Vec<FolderCard>>,
    suggested: Option<String>,
}

impl FoldersUi {
    /// Forget what was read, so the next frame asks the port again.
    pub(crate) fn refresh(&mut self) {
        self.cards = None;
    }
}

/// What a press on the page asks for.
enum Act {
    None,
    /// Open the folder picker and add what is chosen.
    Choose,
    /// Add the folder the launcher proposes (made if it is not there yet).
    UseSuggested(String),
    Rescan(String),
    MakeDefault(String),
    Remove(String),
}

impl FeaturesUi {
    /// The page body: what the folders are for, one card per folder, and how to add one.
    pub(super) fn folders_body(&mut self, ui: &mut Ui, host: &mut dyn Host) {
        let t = Tokens::get(ui.ctx());
        let Some(port) = host.features() else { return };
        if self.folders.cards.is_none() {
            self.folders.cards = Some(port.model_folders());
            self.folders.suggested = port.suggested_model_folder();
        }
        let cards = self.folders.cards.clone().unwrap_or_default();
        let suggested = self.folders.suggested.clone();

        crate::connections::title(ui, tl!("Model folders"), tl!("Where your models live on this computer"));
        ui.add(
            egui::Label::new(
                RichText::new(tl!(
                    "Point ArtCraft at the folder where you keep your models. Your files stay exactly where they are: nothing is moved, renamed or deleted. Any folder layout works. Folder names like checkpoints, vae and loras are recognised, and so are the contents of model files."
                ))
                .color(t.muted),
            )
            .wrap(),
        );
        ui.add_space(18.0);

        let mut act = Act::None;
        if cards.is_empty() {
            act = empty_state(ui, &t, suggested.as_deref());
        } else {
            widgets::hud(ui, tl!("Your folders"), t.faint);
            ui.add_space(6.0);
            for card in &cards {
                if let Some(next) = folder_card(ui, &t, card) {
                    act = next;
                }
                ui.add_space(8.0);
            }
            ui.add_space(6.0);
            if widgets::button(ui, tl!("Add a folder"), Kind::Secondary, 0.0).clicked() {
                act = Act::Choose;
            }
        }
        ui.add_space(4.0);
        self.run(act, host, &cards);
    }

    fn run(&mut self, act: Act, host: &mut dyn Host, cards: &[FolderCard]) {
        let result = match act {
            Act::None => return,
            Act::Choose => {
                let start = cards.iter().find(|c| c.default).or(cards.first()).map(|c| c.path.clone());
                let Some(path) = host.pick_folder(start.as_deref()) else { return };
                host.features().map(|p| p.add_model_folder(&path, false))
            }
            Act::UseSuggested(path) => host.features().map(|p| p.add_model_folder(&path, true)),
            Act::Rescan(id) => host.features().map(|p| p.rescan_model_folder(&id)),
            Act::MakeDefault(id) => host.features().map(|p| p.set_default_model_folder(&id).map(|()| tl!("New downloads will go there.").to_owned())),
            Act::Remove(id) => host.features().map(|p| p.remove_model_folder(&id).map(|()| tl!("Forgotten. Its files were not touched.").to_owned())),
        };
        if let Some(result) = result {
            self.notice = Some(match result {
                Ok(text) => (text, false),
                Err(text) => (text, true),
            });
        }
        self.folders.refresh();
    }
}

fn empty_state(ui: &mut Ui, t: &Tokens, suggested: Option<&str>) -> Act {
    let mut act = Act::None;
    egui::Frame::new().stroke(t.hairline()).inner_margin(14).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(widgets::heading(tl!("No model folders yet"), 16.0, t.ink_strong));
        ui.add_space(6.0);
        if let Some(path) = suggested {
            ui.add(egui::Label::new(RichText::new(tl!("Suggested folder")).size(12.0).color(t.muted)));
            ui.add(egui::Label::new(RichText::new(path).size(12.0).color(t.ink)).wrap());
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(tl!("It is created for you if it is not there yet. You can choose a different folder instead."))
                        .size(12.0)
                        .color(t.muted),
                )
                .wrap(),
            );
            ui.add_space(10.0);
        }
        ui.horizontal(|ui| {
            if let Some(path) = suggested
                && widgets::button(ui, tl!("Use suggested folder"), Kind::Primary, 0.0).clicked()
            {
                act = Act::UseSuggested(path.to_owned());
            }
            if widgets::button(ui, tl!("Choose a folder"), Kind::Secondary, 0.0).clicked() {
                act = Act::Choose;
            }
        });
    });
    act
}

fn folder_card(ui: &mut Ui, t: &Tokens, card: &FolderCard) -> Option<Act> {
    let mut act = None;
    egui::Frame::new().stroke(t.hairline()).inner_margin(14).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(widgets::heading(&card.label, 16.0, t.ink_strong));
            if card.default {
                widgets::hud(ui, tl!("Downloads go here"), t.accent);
            }
            if card.read_only {
                widgets::hud(ui, tl!("Read only, from ComfyUI"), t.faint);
            }
            if card.missing {
                widgets::hud(ui, tl!("Not found"), t.bad);
            }
        });
        ui.add(egui::Label::new(RichText::new(&card.path).size(12.0).color(t.muted)).wrap());
        ui.add_space(6.0);
        let found = if card.kinds.is_empty() {
            tl!("No models found yet").to_owned()
        } else {
            card.kinds
                .iter()
                .map(|(kind, places)| if *places > 1 { format!("{kind} ({places})") } else { kind.clone() })
                .collect::<Vec<_>>()
                .join("  ·  ")
        };
        ui.add(egui::Label::new(RichText::new(found).size(12.0).color(t.ink)).wrap());
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if !card.read_only
                && !card.default
                && widgets::button(ui, tl!("Make default"), Kind::Secondary, 0.0)
                    .on_hover_text(tl!("New downloads are saved in this folder"))
                    .clicked()
            {
                act = Some(Act::MakeDefault(card.id.clone()));
            }
            if !card.missing
                && widgets::button(ui, tl!("Look again"), Kind::Secondary, 0.0)
                    .on_hover_text(tl!("Read the folder again for files added or moved"))
                    .clicked()
            {
                act = Some(Act::Rescan(card.id.clone()));
            }
            if !card.read_only
                && widgets::button(ui, tl!("Remove"), Kind::Secondary, 0.0)
                    .on_hover_text(tl!("Forget this folder. The files stay where they are"))
                    .clicked()
            {
                act = Some(Act::Remove(card.id.clone()));
            }
        });
    });
    act
}
