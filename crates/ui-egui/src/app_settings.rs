//! Per-app settings. Selecting a release edits a draft; only an explicit action installs it.

use crate::app::{Dialog, LauncherUi};
use crate::i18n::{fmt, msg};
use crate::theme::Tokens;
use crate::widgets::{self, Kind};
use artcraft_launcher_core::protocol::{AppPreferences, Command};
use artcraft_launcher_core::{InstallSource, Os, PackageKind, catalog::LAUNCHER_ID, platform::pick_asset};

pub(crate) fn release_retry_delay(failures: u32, minimum: u64) -> u64 {
    (5_u64 << failures.saturating_sub(1).min(4)).min(60).max(minimum)
}

pub(crate) struct Draft {
    pub app: String,
    tag: String,
    pin: bool,
    prereleases: bool,
    package: Option<PackageKind>,
    pub directory: String,
    custom: bool,
    installation_tab: bool,
}

impl Draft {
    fn preferences(&self) -> AppPreferences {
        AppPreferences {
            pinned_tag: self.pin.then(|| self.tag.clone()),
            install_dir: (self.custom && !self.directory.trim().is_empty()).then(|| self.directory.trim().to_string()),
            package: self.package,
        }
    }
}

impl LauncherUi {
    pub(crate) fn load_app_releases(&mut self, id: &str, page: u32, force: bool) {
        let Some(entry) = self.entry_mut(id) else { return };
        if entry.history_loading.is_some() {
            return;
        }
        entry.history_retry = None;
        entry.history_loading = Some(page);
        self.send(Command::LoadReleases { app: id.to_string(), page, force });
    }

    pub(crate) fn retry_app_releases(&mut self, ctx: &egui::Context) {
        let active = match &self.dialog {
            Some(Dialog::AppSettings(id)) => Some(id.clone()),
            _ => None,
        };
        let now = ctx.input(|i| i.time);
        let mut retry = None;
        for entry in self.entries.iter_mut().chain(std::iter::once(&mut self.launcher_entry)) {
            if active.as_deref() != Some(entry.app.id) {
                entry.history_retry = None;
                entry.history_failures = 0;
            } else if let Some(pending) = &entry.history_retry {
                if entry.history_loading.is_none() && now >= pending.at {
                    retry = Some((entry.app.id, pending.page));
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64((pending.at - now).clamp(0.0, 60.0)));
                }
            }
        }
        if let Some((id, page)) = retry {
            self.load_app_releases(id, page, true);
        }
    }

    pub(crate) fn open_app_settings(&mut self, id: &str) {
        let Some(entry) = self.entry(id) else { return };
        let preferences = self.settings.apps.get(id).cloned().unwrap_or_default();
        let tag = preferences
            .pinned_tag
            .clone()
            .or_else(|| {
                let installed = entry.installed.as_ref().and_then(|i| i.version.as_ref());
                entry
                    .releases
                    .iter()
                    .chain(entry.release())
                    .find(|r| r.version.as_ref().is_some_and(|v| Some(v) == installed))
                    .map(|r| r.tag.clone())
                    .or_else(|| installed.map(|v| format!("v{v}")))
            })
            .or_else(|| entry.release().map(|r| r.tag.clone()))
            .unwrap_or_default();
        let load = entry.history_page == 0 || entry.history_error.is_some();
        let pin = preferences.pinned_tag.is_some() || !self.settings.apps.contains_key(id);
        self.app_settings = Some(Draft {
            app: id.to_string(),
            tag,
            pin,
            prereleases: self.settings.prereleases,
            package: preferences.package,
            custom: preferences.install_dir.is_some(),
            directory: preferences.install_dir.unwrap_or_default(),
            installation_tab: false,
        });
        self.dialog = Some(Dialog::AppSettings(id.to_string()));
        if load {
            self.load_app_releases(id, 1, false);
        }
    }

    pub(crate) fn app_settings_dialog(&mut self, ui: &mut egui::Ui, id: &str) -> bool {
        let Some(entry) = self.entry(id).cloned() else { return true };
        let Some(mut draft) = self.app_settings.take().filter(|d| d.app == id) else { return true };
        let tokens = Tokens::get(ui.ctx());
        ui.set_width(560.0f32.min((ui.ctx().content_rect().width() - 64.0).max(260.0)));
        ui.label(widgets::heading(entry.app.name, 24.0, tokens.ink_strong));
        let launcher = id == LAUNCHER_ID;
        widgets::hud(ui, if launcher { tl!("Launcher version") } else { tl!("App settings") }, tokens.muted);
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut draft.installation_tab, false, tl!("Version"));
            if !launcher {
                ui.selectable_value(&mut draft.installation_tab, true, tl!("Installation"));
            }
        });
        ui.separator();
        let busy = entry.job.is_some();
        let mut refresh = false;
        let mut more = false;
        let mut install = false;
        let mut save = false;
        let mut selected = None;
        let os = self.platform.map(|p| p.os);
        let existing_kind = entry.installed.as_ref().and_then(|installed| match installed.source {
            InstallSource::Msi => Some(PackageKind::Msi),
            InstallSource::MacApp => Some(PackageKind::Dmg),
            InstallSource::Deb => Some(PackageKind::Deb),
            InstallSource::Rpm => Some(PackageKind::Rpm),
            InstallSource::Flatpak => Some(PackageKind::Flatpak),
            InstallSource::AppImage => Some(PackageKind::AppImage),
            InstallSource::Launcher if os == Some(Os::Windows) => Some(PackageKind::PortableZip),
            InstallSource::Launcher if installed.location.to_lowercase().ends_with(".appimage") => Some(PackageKind::AppImage),
            _ => None,
        });
        let kinds: Vec<_> = existing_kind.or(draft.package).map(|kind| vec![kind]).unwrap_or_else(|| self.install_kinds.clone());
        let stems: Vec<_> = entry.app.stems().collect();
        let mut releases = entry.releases.clone();
        if let Some(latest) = entry.release()
            && !releases.iter().any(|r| r.tag == latest.tag)
        {
            releases.insert(0, latest.clone());
        }
        if draft.tag.is_empty()
            && let Some(release) = releases.iter().find(|r| {
                (draft.prereleases || !r.prerelease && r.version.as_ref().is_none_or(|v| v.pre.is_empty()))
                    && self.platform.is_some_and(|p| pick_asset(r, &stems, p, &kinds).is_some())
            })
        {
            draft.tag = release.tag.clone();
        }
        let current = entry.installed.as_ref().and_then(|i| i.version.as_ref());
        let mut can_install = false;
        let mut same_version = false;
        egui::ScrollArea::vertical().max_height(ui.ctx().content_rect().height() * 0.55).show(ui, |ui| {
            if !draft.installation_tab {
                ui.label(fmt(
                    tl!("Installed: {version}"),
                    &[("version", &entry.installed_label().unwrap_or_else(|| tl!("Not installed").to_string()))],
                ));
                if self.settings.apps.get(id).is_some_and(|p| p.pinned_tag.is_some()) {
                    widgets::hud(ui, tl!("Version is kept"), tokens.muted);
                }
                ui.add_space(10.0);
                ui.label(tl!("Selected release"));
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt(("app-version", id))
                        .width(ui.available_width().min(420.0))
                        .selected_text(if draft.tag.is_empty() { tl!("Select a release").to_string() } else { draft.tag.clone() })
                        .show_ui(ui, |ui| {
                            for release in
                                releases.iter().filter(|r| draft.prereleases || !r.prerelease && r.version.as_ref().is_none_or(|v| v.pre.is_empty()))
                            {
                                let compatible = self.platform.is_some_and(|p| pick_asset(release, &stems, p, &kinds).is_some());
                                let label = if release.version.as_ref().is_some_and(|v| Some(v) == current) {
                                    format!("{} · {}", release.tag, tl!("Installed"))
                                } else if entry.release().is_some_and(|latest| latest.tag == release.tag) {
                                    format!("{} · {}", release.tag, tl!("Latest"))
                                } else {
                                    release.tag.clone()
                                };
                                ui.add_enabled_ui(compatible, |ui| {
                                    ui.selectable_value(&mut draft.tag, release.tag.clone(), label);
                                });
                            }
                        });
                    ui.checkbox(&mut draft.prereleases, tl!("Show pre-releases"));
                });
                ui.horizontal(|ui| {
                    if entry.history_loading.is_some() {
                        ui.spinner();
                        ui.label(tl!("Loading releases…"));
                    }
                    refresh = ui.add_enabled(entry.history_loading.is_none() && !busy, egui::Button::new(tl!("Refresh releases"))).clicked();
                    if entry.history_more {
                        more = ui.add_enabled(entry.history_loading.is_none() && !busy, egui::Button::new(tl!("Load older releases"))).clicked();
                    }
                });
                if let Some(error) = &entry.history_error {
                    ui.colored_label(tokens.bad, msg(error));
                }
                if let Some(retry) = &entry.history_retry {
                    let seconds = (retry.at - ui.ctx().input(|i| i.time)).max(0.0).ceil() as u64;
                    ui.label(fmt(tl!("Retrying automatically in {seconds}s…"), &[("seconds", &seconds.to_string())]));
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
                }
                if entry.history_page > 0 && releases.is_empty() {
                    ui.label(tl!("No published releases are available."));
                }
                selected = releases.iter().find(|r| r.tag == draft.tag).cloned();
                if let Some(release) = &selected {
                    let asset = self.platform.and_then(|p| pick_asset(release, &stems, p, &kinds));
                    same_version = release.version.as_ref().is_some_and(|v| Some(v) == current);
                    let downgrade = release.version.as_ref().zip(current).is_some_and(|(target, current)| target < current);
                    let blocked =
                        downgrade && asset.is_some_and(|(_, kind)| matches!(kind, PackageKind::Msi | PackageKind::Rpm | PackageKind::Flatpak));
                    let checksums = asset.is_some_and(|(asset, _)| asset.sha256.is_some() || release.sums_asset().is_some());
                    can_install = asset.is_some()
                        && checksums
                        && !blocked
                        && !same_version
                        && release.version.is_some()
                        && (!draft.custom || !draft.directory.trim().is_empty())
                        && (draft.prereleases || !release.prerelease && release.version.as_ref().is_none_or(|v| v.pre.is_empty()));
                    if blocked {
                        ui.label(tl!("This package cannot downgrade here. Use a portable installation instead."));
                    } else if asset.is_none() {
                        ui.label(tl!("No compatible download for this release."));
                    } else if !checksums {
                        ui.label(tl!("This release has no published checksum."));
                    } else if release.version.is_none() {
                        ui.label(tl!("This release's version cannot be verified."));
                    }
                    if downgrade && !blocked {
                        ui.label(tl!("An older version will replace the installed program."));
                    }
                    widgets::hud_link(ui, tl!("Release notes"), &release.html_url);
                }
                ui.add_space(12.0);
                ui.add_enabled_ui(!busy, |ui| {
                    ui.checkbox(&mut draft.pin, tl!("Keep this version after switching"));
                });
                ui.label(if draft.pin {
                    tl!("Automatic updates and Update all will leave this version unchanged.")
                } else {
                    tl!("Follow the latest release using the global update settings.")
                });
                ui.add_space(12.0);
                let label = fmt(tl!("Switch to {version}"), &[("version", &draft.tag)]);
                if launcher {
                    ui.label(tl!("The launcher will wait for other installs, switch versions and restart."));
                    ui.label(tl!("Older launcher releases may not support version preferences."));
                    if !self.caps.self_update {
                        ui.label(tl!("This development or externally managed launcher cannot switch versions in place."));
                    }
                }
                ui.add_enabled_ui(
                    can_install && !busy && entry.detected && if launcher { self.caps.self_update } else { self.caps.manage_apps },
                    |ui| {
                        install = widgets::button(ui, &label, Kind::Primary, 0.0).clicked();
                    },
                );
            } else {
                ui.label(tl!("Current installation folder"));
                if let Some(installed) = &entry.installed {
                    let location = &installed.location;
                    let binary_path = matches!(installed.source, InstallSource::Msi | InstallSource::AppImage | InstallSource::Binary)
                        || installed.source == InstallSource::Launcher && (os == Some(Os::Windows) || location.to_lowercase().ends_with(".appimage"));
                    let folder =
                        if binary_path { location.rsplit_once(['\\', '/']).map_or(location.as_str(), |(folder, _)| folder) } else { location };
                    ui.label(folder);
                    if ui.button(tl!("Open installation folder")).clicked() {
                        self.send(Command::Reveal { app: id.to_string() });
                    }
                    if existing_kind != Some(PackageKind::AppImage) {
                        ui.label(tl!("Version changes keep the existing installation folder and package format."));
                    }
                } else {
                    ui.label(tl!("Not installed"));
                }
                ui.add_space(12.0);
                ui.add_enabled_ui(!busy && entry.installed.is_none(), |ui| {
                    ui.label(tl!("Package format"));
                    egui::ComboBox::from_id_salt(("app-package", id))
                        .selected_text(draft.package.map_or(tl!("System default"), |k| crate::i18n::t(k.label())))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut draft.package, None, tl!("System default"));
                            let portable = match os {
                                Some(Os::Windows) => Some(PackageKind::PortableZip),
                                _ => None,
                            };
                            if let Some(kind) = portable {
                                ui.selectable_value(&mut draft.package, Some(kind), crate::i18n::t(kind.label()));
                            }
                        });
                });
                let custom_supported = existing_kind.or(draft.package) == Some(PackageKind::PortableZip);
                if !custom_supported && entry.installed.is_none() {
                    draft.custom = false;
                }
                ui.add_enabled_ui(custom_supported && !busy && entry.installed.is_none(), |ui| {
                    ui.checkbox(&mut draft.custom, tl!("Choose a folder for a new installation"));
                    if draft.custom {
                        ui.text_edit_singleline(&mut draft.directory);
                    }
                });
                if !custom_supported {
                    ui.label(tl!("Custom folders are available for portable ZIP packages."));
                }
                ui.add_space(10.0);
                if entry.installed.is_some() && ui.add_enabled(!busy, egui::Button::new(tl!("Uninstall…"))).clicked() {
                    self.dialog = Some(Dialog::ConfirmUninstall(id.to_string()));
                }
            }
        });
        ui.separator();
        if busy {
            ui.label(tl!("Installation in progress. You can close this window."));
        }
        let mut close = false;
        ui.horizontal(|ui| {
            save = ui
                .add_enabled(!busy && (!draft.pin || same_version) && !draft.installation_tab, egui::Button::new(tl!("Save update preferences")))
                .clicked();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = widgets::button(ui, tl!("Close"), Kind::Secondary, 90.0).clicked();
            });
        });
        if refresh {
            if let Some(entry) = self.entry_mut(id) {
                entry.history_failures = 0;
            }
            self.load_app_releases(id, 1, true);
        }
        if more {
            self.load_app_releases(id, entry.history_page.saturating_add(1), false);
        }
        if save {
            self.settings.apps.insert(id.to_string(), draft.preferences());
        }
        if install && let Some(release) = selected {
            self.send(Command::InstallRelease { app: id.to_string(), tag: release.tag, preferences: draft.preferences() });
        }
        self.app_settings = Some(draft);
        close
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Host, Settings};
    use artcraft_launcher_core::protocol::{Backend, Capabilities, Event, ReleaseError, ReleasePage};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Inbox = Rc<RefCell<Vec<Event>>>;
    type Sent = Rc<RefCell<Vec<Command>>>;
    struct Fake {
        inbox: Inbox,
        sent: Sent,
    }
    impl Backend for Fake {
        fn send(&mut self, command: Command) {
            self.sent.borrow_mut().push(command);
        }
        fn poll(&mut self) -> Vec<Event> {
            std::mem::take(&mut *self.inbox.borrow_mut())
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities { manage_apps: true, reveal_label: "Show in Explorer", self_update: false }
        }
        fn platform(&self) -> Option<artcraft_launcher_core::Platform> {
            None
        }
        fn now(&self) -> i64 {
            1_791_500_000
        }
    }
    struct NoHost;
    impl Host for NoHost {}

    fn setup() -> (LauncherUi, egui::Context, Inbox, Sent) {
        let ctx = egui::Context::default();
        let inbox = Rc::new(RefCell::new(Vec::new()));
        let sent = Rc::new(RefCell::new(Vec::new()));
        let ui = LauncherUi::new(&ctx, Box::new(Fake { inbox: inbox.clone(), sent: sent.clone() }), Settings::default());
        (ui, ctx, inbox, sent)
    }
    fn tick(ui: &mut LauncherUi, ctx: &egui::Context, time: f64) {
        let mut output = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |root| ui.logic(root.ctx(), &mut NoHost));
        output.textures_delta.clear();
    }
    fn failure(page: u32, retry_after: Option<u64>) -> Event {
        Event::Releases { app: "photocraft".into(), page, result: Err(ReleaseError { message: "GitHub answered HTTP 504".into(), retry_after }) }
    }
    fn loads(sent: &Sent) -> Vec<(u32, bool)> {
        sent.borrow()
            .iter()
            .filter_map(|c| match c {
                Command::LoadReleases { app, page, force } if app == "photocraft" => Some((*page, *force)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn release_history_retries_failed_page_with_backoff_until_success_and_keeps_existing_releases() {
        let (mut ui, ctx, inbox, sent) = setup();
        let entry = ui.entries.iter_mut().find(|e| e.app.id == "photocraft").unwrap();
        entry.history_page = 1;
        entry.releases.push(artcraft_launcher_core::Release { tag: "v0.7.0".into(), ..Default::default() });
        ui.open_app_settings("photocraft");
        ui.load_app_releases("photocraft", 2, false);
        inbox.borrow_mut().push(failure(2, Some(0)));
        tick(&mut ui, &ctx, 0.0);
        assert_eq!(ui.entry("photocraft").unwrap().releases[0].tag, "v0.7.0");
        tick(&mut ui, &ctx, 4.9);
        assert_eq!(loads(&sent), [(2, false)]);
        tick(&mut ui, &ctx, 5.0);
        tick(&mut ui, &ctx, 6.0);
        assert_eq!(loads(&sent), [(2, false), (2, true)]);
        inbox.borrow_mut().push(failure(2, Some(0)));
        tick(&mut ui, &ctx, 7.0);
        tick(&mut ui, &ctx, 16.9);
        assert_eq!(loads(&sent).len(), 2);
        tick(&mut ui, &ctx, 17.0);
        assert_eq!(loads(&sent).len(), 3);
        inbox.borrow_mut().push(Event::Releases { app: "photocraft".into(), page: 2, result: Ok(ReleasePage::default()) });
        tick(&mut ui, &ctx, 18.0);
        tick(&mut ui, &ctx, 500.0);
        let entry = ui.entry("photocraft").unwrap();
        assert!(entry.history_error.is_none() && entry.history_retry.is_none());
        assert_eq!(entry.releases[0].tag, "v0.7.0");
        assert_eq!(entry.history_page, 2);
        assert_eq!(loads(&sent).len(), 3);
        assert_eq!((1..=8).map(|n| release_retry_delay(n, 0)).collect::<Vec<_>>(), [5, 10, 20, 40, 60, 60, 60, 60]);
    }

    #[test]
    fn release_history_stops_retries_on_close_and_ignores_late_failure() {
        let (mut ui, ctx, inbox, sent) = setup();
        ui.open_app_settings("photocraft");
        inbox.borrow_mut().push(failure(1, Some(0)));
        tick(&mut ui, &ctx, 0.0);
        ui.dialog = None;
        tick(&mut ui, &ctx, 100.0);
        assert_eq!(loads(&sent).len(), 1);
        assert!(ui.entry("photocraft").unwrap().history_retry.is_none());
        ui.open_app_settings("photocraft");
        ui.dialog = None;
        inbox.borrow_mut().push(failure(1, Some(0)));
        tick(&mut ui, &ctx, 101.0);
        tick(&mut ui, &ctx, 200.0);
        assert_eq!(loads(&sent).len(), 2);
        assert!(ui.entry("photocraft").unwrap().history_retry.is_none());
    }

    #[test]
    fn release_history_respects_server_delay_and_does_not_retry_permanent_errors() {
        let (mut ui, ctx, inbox, sent) = setup();
        ui.open_app_settings("photocraft");
        inbox.borrow_mut().push(failure(1, Some(120)));
        tick(&mut ui, &ctx, 0.0);
        tick(&mut ui, &ctx, 119.9);
        assert_eq!(loads(&sent).len(), 1);
        tick(&mut ui, &ctx, 120.0);
        assert_eq!(loads(&sent).len(), 2);
        inbox.borrow_mut().push(failure(1, None));
        tick(&mut ui, &ctx, 121.0);
        tick(&mut ui, &ctx, 1000.0);
        assert_eq!(loads(&sent).len(), 2);
        assert!(ui.entry("photocraft").unwrap().history_retry.is_none());
    }
}
