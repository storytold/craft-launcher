//! The install step: what will be downloaded, how big it is, the licence to accept, progress.

use craft_ai_features::{Feature, Plan, State};
use egui::{RichText, Ui};

use super::text::{blocker_text, bytes};
use super::tool::hosted_buttons;
use super::{FeaturesPort, FeaturesUi, Install, Outcome, Snapshot};
use crate::i18n::fmt;
use crate::theme::Tokens;
use crate::widgets::{self, Kind};

impl FeaturesUi {
    pub(super) fn setup(&mut self, ui: &mut Ui, id: &str, snapshot: &Snapshot, port: &mut dyn FeaturesPort) -> Outcome {
        let t = Tokens::get(ui.ctx());
        let Some(feature) = self.features.iter().find(|f| f.id == id).cloned() else {
            self.back();
            return Outcome::None;
        };
        let plan = self.plan_for(snapshot, id).cloned();
        let running = match &snapshot.install {
            Install::Running { feature, .. } => feature == id,
            _ => false,
        };
        crate::connections::title(
            ui,
            &fmt(tl!("Set up {name}"), &[("name", crate::i18n::t(&feature.title))]),
            tl!("Runs on this computer through ComfyUI"),
        );

        if let Some(plan) = &plan {
            if running {
                self.progress(ui, &snapshot.install);
            } else {
                self.needs(ui, &feature, plan);
            }
        }
        match &snapshot.install {
            Install::Failed { feature: f, message } if f == id => {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(fmt(tl!("The download failed: {e}"), &[("e", message)])).color(t.bad)).wrap());
            }
            Install::Done { feature: f } if f == id && plan.as_ref().is_some_and(|p| p.state == State::Ready) => {
                ui.add_space(8.0);
                let ready = feature.local.as_ref().is_some_and(|r| r.usable);
                ui.label(
                    RichText::new(if ready {
                        tl!("Installed. This tool is ready on this computer.")
                    } else {
                        tl!("Model files installed. The app tool is not connected yet.")
                    })
                    .color(if ready { t.good } else { t.warn }),
                );
            }
            _ => {}
        }
        if let Some((text, error)) = &self.notice {
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(text).color(if *error { t.bad } else { t.muted })).wrap());
        }

        let mut outcome = Outcome::None;
        if !feature.cloud.is_empty() && !running {
            ui.add_space(14.0);
            widgets::hud(ui, tl!("Cannot run it here? Use a hosted service"), t.faint);
            ui.add_space(2.0);
            if let Some(next) = hosted_buttons(ui, &feature) {
                outcome = next;
            }
        }
        ui.add_space(16.0);
        let ready_to_start = plan.as_ref().is_some_and(|p| p.state == State::NeedsInstall && p.download_bytes > 0)
            && plan.as_ref().is_some_and(|p| p.licences.is_empty() || self.accepted);
        ui.horizontal(|ui| {
            if running {
                if widgets::button(ui, tl!("Cancel download"), Kind::Secondary, 0.0).clicked() {
                    port.cancel_install();
                }
            } else {
                let label = match plan.as_ref() {
                    Some(p) if p.download_bytes > 0 => fmt(tl!("Download {size}"), &[("size", &bytes(p.download_bytes))]),
                    _ => tl!("Download").to_owned(),
                };
                ui.add_enabled_ui(ready_to_start, |ui| {
                    if widgets::button(ui, &label, Kind::Primary, 0.0).clicked() {
                        self.start_install(id, port);
                    }
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, tl!("Back"), Kind::Secondary, 90.0).clicked() && !running {
                    self.back();
                }
            });
        });
        outcome
    }

    /// What is missing, the licence, and anything that stops it.
    fn needs(&mut self, ui: &mut Ui, feature: &Feature, plan: &Plan) {
        let t = Tokens::get(ui.ctx());
        if !plan.missing_files.is_empty() {
            widgets::hud(ui, tl!("Will be downloaded"), t.faint);
            ui.add_space(2.0);
            for file in &plan.missing_files {
                let host = url_host(&file.source.url());
                ui.add(
                    egui::Label::new(
                        RichText::new(fmt(
                            tl!("{file}, {size}, from {host}"),
                            &[("file", &file.filename), ("size", &bytes(file.size_bytes)), ("host", &host)],
                        ))
                        .color(t.ink),
                    )
                    .wrap(),
                );
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(fmt(tl!("Total {size}. It goes into your ComfyUI models folder."), &[("size", &bytes(plan.download_bytes))]))
                    .size(12.0)
                    .color(t.muted),
            );
        }
        for pack in &plan.missing_packs {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(fmt(
                        tl!("Also needs the {name} add-on in ComfyUI. Install it with ComfyUI Manager, then come back: {url}"),
                        &[("name", &pack.label), ("url", &pack.repo_url)],
                    ))
                    .color(t.warn),
                )
                .wrap(),
            );
        }
        for blocker in &plan.blockers {
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(blocker_text(Some(blocker))).color(t.bad)).wrap());
        }
        if !plan.licences.is_empty() {
            ui.add_space(10.0);
            widgets::hud(ui, tl!("Licence"), t.faint);
            ui.add_space(2.0);
            for licence in &plan.licences {
                ui.label(RichText::new(&licence.name).color(t.ink_strong));
                if let Some(note) = &licence.note {
                    ui.add(egui::Label::new(RichText::new(crate::i18n::t(note)).size(12.0).color(t.muted)).wrap());
                }
                if let Some(url) = &licence.url {
                    widgets::hud_link(ui, tl!("Read the licence"), url);
                }
            }
            ui.add_space(4.0);
            ui.checkbox(&mut self.accepted, tl!("I have read the licence and accept it"));
        }
        let _ = feature;
    }

    fn progress(&self, ui: &mut Ui, install: &Install) {
        let t = Tokens::get(ui.ctx());
        let Install::Running { file, verifying, done, total, step, of, .. } = install else { return };
        let fraction = if *total == 0 { 0.0 } else { (*done as f64 / *total as f64) as f32 };
        let line = if *verifying {
            fmt(tl!("Checking {file}"), &[("file", file)])
        } else {
            fmt(tl!("Downloading {file}: {done} of {total}"), &[("file", file), ("done", &bytes(*done)), ("total", &bytes(*total))])
        };
        ui.add(egui::Label::new(RichText::new(line).color(t.ink)).wrap());
        ui.add_space(4.0);
        ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).desired_width(ui.available_width()));
        if *of > 1 {
            ui.label(
                RichText::new(fmt(tl!("File {step} of {of}"), &[("step", &step.to_string()), ("of", &of.to_string())])).size(12.0).color(t.muted),
            );
        }
    }
}

/// The host part of an address, for "from huggingface.co".
fn url_host(url: &str) -> String {
    url.split("://").nth(1).and_then(|rest| rest.split('/').next()).unwrap_or(url).to_owned()
}
