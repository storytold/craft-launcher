//! Source management is separate from official app rows and automatic installation.

use std::collections::BTreeMap;

use artcraft_launcher_core::community::{CommunityAction, CommunitySource, CommunityStatus, MAX_SOURCES};
use artcraft_launcher_core::protocol::{Command, JobPhase};

use crate::LauncherUi;
use crate::theme::Tokens;
use crate::widgets::{self, Kind};

#[derive(Default)]
pub(crate) struct CommunityUi {
    repo: String,
    name: String,
    package: String,
    trusted: bool,
    error: Option<String>,
    entries: BTreeMap<String, Entry>,
}

struct Entry {
    source: CommunitySource,
    status: CommunityStatus,
    busy: bool,
    phase: Option<JobPhase>,
    action: CommunityAction,
}

impl LauncherUi {
    pub(crate) fn community_busy(&self) -> bool {
        self.community.entries.values().any(|e| e.busy)
    }

    pub(crate) fn community_tick(&mut self) {
        if !self.caps.manage_apps || self.community_busy() {
            return;
        }
        // One initial lookup at a time, and only for user-enabled repositories. Never install
        // through the official auto-update setting or contact the official release feed.
        if let Some(source) =
            self.settings.sources.iter().take(MAX_SOURCES).find(|s| s.enabled && !self.community.entries.contains_key(&s.key())).cloned()
        {
            self.community_action(source, CommunityAction::Check);
        }
    }

    fn community_action(&mut self, source: CommunitySource, action: CommunityAction) {
        let entry = self.community.entries.entry(source.key()).or_insert_with(|| Entry {
            source: source.clone(),
            status: Default::default(),
            busy: false,
            phase: None,
            action: action.clone(),
        });
        entry.source = source.clone();
        entry.busy = true;
        entry.action = action.clone();
        entry.phase = None;
        entry.status.error = None;
        self.backend.send(Command::Community { source, action });
    }

    pub(crate) fn community_progress(&mut self, source: CommunitySource, phase: JobPhase) {
        if let Some(entry) = self.community.entries.get_mut(&source.key())
            && entry.source == source
            && self.settings.sources.contains(&source)
        {
            entry.phase = Some(phase);
        }
    }

    pub(crate) fn community_finished(&mut self, source: CommunitySource, mut status: CommunityStatus) {
        if let Some(entry) = self.community.entries.get_mut(&source.key())
            && entry.source == source
            && self.settings.sources.contains(&source)
        {
            if status.release.is_none() && (entry.action == CommunityAction::Launch || status.error.is_some()) {
                status.release = entry.status.release.take();
            }
            entry.status = status;
            entry.busy = false;
            entry.phase = None;
        }
    }

    fn add_source(&mut self) {
        let result = (|| {
            if !self.community.trusted {
                return Err(tl!("Confirm that you trust this publisher.").to_string());
            }
            if self.settings.sources.len() >= MAX_SOURCES {
                return Err(tl!("You can add up to 32 sources.").to_string());
            }
            let source = CommunitySource::new(&self.community.repo, &self.community.name, &self.community.package)?;
            if self.settings.sources.iter().any(|s| s.key() == source.key()) {
                return Err(tl!("This source is already added.").to_string());
            }
            self.settings.sources.push(source);
            Ok(())
        })();
        self.community.error = result.err();
        if self.community.error.is_none() {
            self.community.repo.clear();
            self.community.name.clear();
            self.community.package.clear();
            self.community.trusted = false;
        }
    }

    pub(crate) fn sources_contents(&mut self, ui: &mut egui::Ui) {
        let t = Tokens::get(ui.ctx());
        widgets::hud(ui, tl!("Official source"), t.faint);
        ui.hyperlink_to("storytold · Craft App Suite", "https://github.com/storytold");
        ui.add_space(12.0);
        widgets::hud(ui, tl!("Community sources"), t.faint);
        ui.label(tl!("Community apps are not official ArtCraft apps. Only add publishers you trust."));
        ui.label(tl!("Removing a source keeps its installed files. Community updates are always manual."));
        if !self.caps.manage_apps {
            ui.label(tl!("Manage community sources in the desktop launcher."));
            return;
        }
        for source in self.settings.sources.iter().take(MAX_SOURCES).cloned().collect::<Vec<_>>() {
            let key = source.key();
            ui.push_id(&key, |ui| {
                ui.separator();
                ui.label(egui::RichText::new(&source.name).strong());
                widgets::hud(ui, tl!("Unofficial"), t.warn);
                if source.validate().is_ok() {
                    ui.hyperlink_to(&source.repo, source.url());
                }
                let entry = self.community.entries.get(&key);
                let busy = entry.is_some_and(|e| e.busy);
                let mut enabled = source.enabled;
                if ui.add_enabled(!busy, egui::Checkbox::new(&mut enabled, tl!("Enabled"))).changed() {
                    if let Some(saved) = self.settings.sources.iter_mut().find(|s| s.key() == key) {
                        saved.enabled = enabled;
                    }
                    self.community.entries.remove(&key);
                }
                let entry = self.community.entries.get(&key);
                // Clone only displayed strings, not every release asset and its notes on every frame.
                let installed = entry.and_then(|e| e.status.installed.clone());
                let latest = entry.and_then(|e| e.status.release.as_ref()).map(|r| r.version_label());
                let error = entry.and_then(|e| e.status.error.clone());
                let compatible = self
                    .platform
                    .zip(entry.and_then(|e| e.status.release.as_ref()))
                    .is_some_and(|(platform, release)| source.package_asset(release, platform).is_some());
                let phase = entry.and_then(|e| e.phase);
                if let Some(version) = &installed {
                    ui.label(crate::fmt(tl!("Installed: {version}"), &[("version", version)]));
                }
                if let Some(version) = &latest {
                    ui.label(crate::fmt(tl!("Latest: {version}"), &[("version", version)]));
                }
                if let Some(error) = &error {
                    ui.colored_label(t.warn, crate::msg(error));
                }
                if busy {
                    match phase {
                        Some(JobPhase::Downloading { done, total } | JobPhase::Reconnecting { done, total }) => {
                            let fraction = if total > 0 { (done as f64 / total as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
                            ui.add(egui::ProgressBar::new(fraction).show_percentage());
                        }
                        Some(JobPhase::Installing) => {
                            ui.label(tl!("Installing…"));
                        }
                        _ => {
                            ui.label(tl!("Checking…"));
                        }
                    }
                    if widgets::button(ui, tl!("Cancel"), Kind::Secondary, 0.0).clicked() {
                        self.backend.send(Command::Community { source: source.clone(), action: CommunityAction::Cancel });
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled_ui(source.enabled && !busy, |ui| {
                        if widgets::button(ui, tl!("Refresh"), Kind::Secondary, 0.0).clicked() {
                            self.community_action(source.clone(), CommunityAction::Check);
                        }
                        if installed.is_some() && widgets::button(ui, tl!("Open"), Kind::Primary, 0.0).clicked() {
                            self.community_action(source.clone(), CommunityAction::Launch);
                        }
                        let newer = latest.is_some() && installed != latest;
                        if compatible && newer && widgets::button(ui, tl!("Install / update"), Kind::Primary, 0.0).clicked() {
                            self.community_action(source.clone(), CommunityAction::Install);
                        }
                        if source.validate().is_ok() && widgets::button(ui, tl!("Releases"), Kind::Secondary, 0.0).clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(format!("{}/releases", source.url())));
                        }
                    });
                    ui.add_enabled_ui(!busy, |ui| {
                        if widgets::button(ui, tl!("Remove source"), Kind::Secondary, 0.0).clicked() {
                            self.settings.sources.retain(|s| s.key() != key);
                            self.community.entries.remove(&key);
                        }
                    });
                });
            });
        }
        ui.separator();
        widgets::hud(ui, tl!("Add source"), t.faint);
        let label = ui.label(tl!("GitHub repository"));
        ui.add(egui::TextEdit::singleline(&mut self.community.repo).char_limit(180).hint_text("owner/repository").desired_width(f32::INFINITY))
            .labelled_by(label.id);
        let label = ui.label(tl!("Display name"));
        ui.add(egui::TextEdit::singleline(&mut self.community.name).char_limit(80).desired_width(f32::INFINITY)).labelled_by(label.id);
        let label = ui.label(tl!("Package name"));
        ui.add(egui::TextEdit::singleline(&mut self.community.package).char_limit(100).hint_text("pdfcraft").desired_width(f32::INFINITY))
            .labelled_by(label.id);
        ui.label(tl!("Use the release filename prefix, even if the fork has a different repository name."));
        ui.label(tl!("Managed installs require SHA256SUMS.txt and a Windows portable ZIP or Linux AppImage."));
        ui.checkbox(&mut self.community.trusted, tl!("I trust this publisher and want to add this unofficial source."));
        if widgets::button(ui, tl!("Add source"), Kind::Primary, 0.0).clicked() {
            self.add_source();
        }
        if let Some(error) = &self.community.error {
            ui.colored_label(t.warn, crate::msg(error));
        }
    }
}
