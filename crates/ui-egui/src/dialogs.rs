//! Modal dialogs: release notes, uninstall confirmation, settings.

use crate::i18n::fmt;
use artcraft_launcher_core::Os;
use artcraft_launcher_core::platform::pick_asset;
use artcraft_launcher_core::protocol::Command;
use egui::{RichText, vec2};

use crate::app::{Dialog, FolderChoice, Host, LauncherUi};
use crate::theme::{self, Tokens};
use crate::widgets::{self, Cell, Kind};

impl LauncherUi {
    pub(crate) fn dialogs(&mut self, ctx: &egui::Context, host: &mut dyn Host) {
        let Some(mut dialog) = self.dialog.clone() else {
            self.alert_popup(ctx);
            return;
        };
        let t = Tokens::get(ctx);
        // An area is laid out in the size it had last frame, so dialogs can't share one id: the
        // uninstall prompt would open as tall as Settings and never shrink (#88).
        // The large dialogs run their header to the card's edges; the small prompts keep a margin.
        let large = matches!(dialog, Dialog::Settings | Dialog::Notes(_));
        let modal = egui::Modal::new(egui::Id::new("dialog").with(dialog.key())).frame(if large {
            crate::sheet::frame(&t)
        } else {
            egui::Frame::new().fill(t.bg).stroke(egui::Stroke::new(1.0, t.line_strong)).shadow(theme::shadow(&t)).inner_margin(theme::PAD as i8)
        });
        let mut close = false;
        let response = match &mut dialog {
            Dialog::Notes(id) => modal.show(ctx, |ui| close = self.notes_dialog(ui, id)),
            Dialog::ConfirmUninstall(id) => modal.show(ctx, |ui| close = self.uninstall_dialog(ui, id)),
            Dialog::Install { app, choice, folder, make_default } => {
                modal.show(ctx, |ui| close = self.install_dialog(ui, app, choice, folder, make_default, host))
            }
            Dialog::Replace { app, path } => modal.show(ctx, |ui| close = self.replace_dialog(ui, app, path)),
            Dialog::Settings => modal.show(ctx, |ui| close = self.settings_dialog(ui, host)),
            Dialog::LauncherUpdate(r) => modal.show(ctx, |ui| close = self.launcher_update_dialog(ui, r)),
        };
        if close || response.should_close() {
            self.dialog = None;
        } else {
            self.dialog = Some(dialog);
        }
    }

    /// The first alert from the release feed the user hasn't acknowledged, as a popup.
    fn alert_popup(&mut self, ctx: &egui::Context) {
        let Some(alert) = self.alerts.iter().find(|a| !self.settings.dismissed_notices.contains(&a.id)).cloned() else { return };
        let t = Tokens::get(ctx);
        let modal = egui::Modal::new(egui::Id::new("feed_alert").with(&alert.id)).frame(
            egui::Frame::new().fill(t.bg).stroke(egui::Stroke::new(1.0, t.line_strong)).shadow(theme::shadow(&t)).inner_margin(theme::PAD as i8),
        );
        let mut ok = false;
        modal.show(ctx, |ui| {
            ui.set_width(440.0f32.min(ctx.content_rect().width() - 80.0));
            ui.label(RichText::new(&alert.title).size(18.0).family(theme::semibold()).color(t.ink_strong));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.5).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&alert.body).color(t.muted)).wrap());
            });
            ui.add_space(14.0);
            // A row: a right-to-left layout straight in the column would fill the height left.
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ok = widgets::button(ui, tl!("OK"), Kind::Primary, 90.0).clicked();
                    if let Some(link) = &alert.link
                        && widgets::button(ui, tl!("Open link"), Kind::Secondary, 110.0).on_hover_text(link.as_str()).clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(link.clone()));
                    }
                });
            });
        });
        // Only OK acknowledges it: Escape or a click outside leaves it for later.
        if ok {
            self.settings.dismissed_notices.insert(alert.id);
        }
    }

    fn title(ui: &mut egui::Ui, icon: Option<egui::ImageSource<'static>>, title: &str, subtitle: &str) {
        let t = Tokens::get(ui.ctx());
        ui.horizontal(|ui| {
            if let Some(icon) = icon {
                ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(40.0, 40.0)));
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(widgets::heading(title, theme::DIALOG_TITLE, t.ink_strong));
                if !subtitle.is_empty() {
                    widgets::hud(ui, subtitle, t.faint);
                }
            });
        });
        ui.add_space(14.0);
    }

    /// An app's release notes: a large card like the app page, the notes in a scrolling body.
    fn notes_dialog(&mut self, ui: &mut egui::Ui, id: &str) -> bool {
        let Some(entry) = self.entry(id).cloned() else { return true };
        let Some(release) = entry.release().cloned() else { return true };
        let t = Tokens::get(ui.ctx());
        let size = crate::sheet::size(ui.ctx(), vec2(880.0, 760.0));
        ui.set_width(size.x);
        ui.set_max_height(size.y);
        let date = release.published_at.map(|d| fmt(tl!("Released {date}"), &[("date", &crate::views::date_text(d))])).unwrap_or_default();
        let hero_h = crate::sheet::hero(ui, crate::icons::app(id), &format!("{} {}", entry.app.name, release.version_label()), &date, |_| {});
        let footer_h = theme::CONTROL_H + 32.0;
        let body_h = (size.y - hero_h - footer_h - 2.0).max(0.0);
        let saved = crate::sheet::accent_scroll(ui, &t, t.accent);
        egui::ScrollArea::vertical().id_salt("notes-body").max_height(body_h).auto_shrink([false, true]).show(ui, |ui| {
            *ui.visuals_mut() = saved;
            egui::Frame::new().inner_margin(egui::Margin::same(theme::PAD as i8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if release.notes.is_empty() {
                    ui.label(tl!("These notes weren't fetched (GitHub's request limit was reached). They're on the release page."));
                } else {
                    crate::notes::show_scaled(ui, &release.notes, self.notes_zoom);
                }
            });
        });
        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), t.hairline());
        let mut close = false;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(theme::PAD as i8, 16)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                widgets::hud_link(ui, tl!("Open on GitHub"), &release.html_url);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = widgets::button(ui, tl!("Close"), Kind::Secondary, 90.0).clicked();
                });
            });
        });
        close
    }

    fn launcher_update_dialog(&mut self, ui: &mut egui::Ui, r: &artcraft_launcher_core::Release) -> bool {
        let t = Tokens::get(ui.ctx());
        ui.set_width(480.0f32.min(ui.ctx().content_rect().width() - 80.0));
        let title = fmt(tl!("ArtCraft Launcher {version} is available."), &[("version", &r.version_label())]);
        Self::title(ui, Some(crate::icons::artcraft_mark()), &title, "");
        let message = if self.caps.self_update {
            tl!("A new version is available. Would you like to relaunch with the updated version?")
        } else {
            tl!("A new version is available. Download it to replace this version?")
        };
        ui.label(RichText::new(message).color(t.muted));
        ui.add_space(14.0);
        let mut close = false;
        ui.horizontal(|ui| {
            widgets::hud_link(ui, tl!("What's new"), &r.html_url);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.caps.self_update {
                    if widgets::button(ui, tl!("Update and restart"), Kind::Primary, 0.0)
                        .on_hover_text(tl!("Downloads the new version, replaces this one and opens it again"))
                        .clicked()
                    {
                        self.send(Command::UpdateLauncher);
                        close = true;
                    }
                } else {
                    let url = self
                        .platform
                        .and_then(|p| crate::views::launcher_download(r, p, crate::settings::is_flatpak_sandbox()))
                        .unwrap_or_else(|| r.html_url.clone());
                    if widgets::button(ui, tl!("Download"), Kind::Primary, 0.0).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        close = true;
                    }
                }
                if widgets::button(ui, tl!("Later"), Kind::Secondary, 80.0).clicked() {
                    close = true;
                }
            });
        });
        close
    }

    fn uninstall_dialog(&mut self, ui: &mut egui::Ui, id: &str) -> bool {
        let Some(entry) = self.entry(id).cloned() else { return true };
        let t = Tokens::get(ui.ctx());
        ui.set_width(420.0);
        Self::title(ui, crate::icons::app(id), &fmt(tl!("Uninstall {name}?"), &[("name", entry.app.name)]), "");
        let how = match entry.installed.as_ref().map(|i| i.source) {
            Some(artcraft_launcher_core::InstallSource::MacApp) => tl!("The app will be moved to the Trash. Your documents and settings stay."),
            Some(artcraft_launcher_core::InstallSource::Msi) => {
                tl!("Windows will remove it like any installed program. Your documents and settings stay.")
            }
            Some(
                artcraft_launcher_core::InstallSource::Deb
                | artcraft_launcher_core::InstallSource::Rpm
                | artcraft_launcher_core::InstallSource::Pacman,
            ) => tl!("Your system's package manager will remove it, and may ask for your password. Your documents and settings stay."),
            _ => tl!("The app will be removed. Your documents and settings stay."),
        };
        ui.label(RichText::new(how).color(t.muted));
        ui.add_space(20.0);
        let mut close = false;
        // A row: a right-to-left layout straight in the column would fill the height left.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, tl!("Uninstall"), Kind::Danger, 110.0).clicked() {
                    self.send(Command::Uninstall { app: id.to_string() });
                    close = true;
                }
                if widgets::button(ui, tl!("Cancel"), Kind::Secondary, 90.0).clicked() {
                    close = true;
                }
            });
        });
        close
    }

    fn replace_dialog(&mut self, ui: &mut egui::Ui, id: &str, path: &str) -> bool {
        let Some(app) = self.entry(id).map(|e| e.app) else { return true };
        let t = Tokens::get(ui.ctx());
        ui.set_width(440.0f32.min(ui.ctx().content_rect().width() - 80.0));
        Self::title(ui, crate::icons::app(id), tl!("Replace what's there?"), "");
        ui.label(
            RichText::new(fmt(tl!("{path} already exists. Installing {name} will overwrite it."), &[("path", path), ("name", app.name)]))
                .color(t.muted),
        );
        ui.add_space(20.0);
        let mut close = false;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, tl!("Replace"), Kind::Danger, 110.0).clicked() {
                    self.send(Command::Replace { app: id.to_string() });
                    close = true;
                }
                if widgets::button(ui, tl!("Cancel"), Kind::Secondary, 90.0).clicked() {
                    close = true;
                }
            });
        });
        close
    }

    fn install_dialog(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        choice: &mut FolderChoice,
        folder: &mut String,
        make_default: &mut bool,
        host: &mut dyn Host,
    ) -> bool {
        let Some(app) = self.entry(id).map(|e| e.app) else { return true };
        ui.set_width(440.0f32.min(ui.ctx().content_rect().width() - 80.0));
        Self::title(ui, None, app.name, tl!("Install folder"));
        let root = self.settings.install_locations.root.clone();
        let options = [
            (FolderChoice::AppDefault, Cell { label: tl!("App default"), tip: tl!("The default folder set in Settings."), count: None }),
            (FolderChoice::System, Cell { label: tl!("System default"), tip: tl!("The system's usual place for apps."), count: None }),
            (FolderChoice::Custom, Cell { label: tl!("Custom folder"), tip: tl!("A folder for this app only."), count: None }),
        ];
        let ink = Tokens::get(ui.ctx()).ink;
        let row_width: f32 = options.iter().map(|(_, cell)| widgets::hud_galley(ui, cell.label, ink).size().x + 24.0).sum();
        if row_width <= ui.available_width() {
            ui.horizontal(|ui| widgets::cells(ui, choice, &options));
        } else {
            // Longer translations don't fit in one row: stack the options, a box each.
            widgets::cells_apart(ui, choice, &options, 4.0);
        }
        ui.add_space(8.0);
        match *choice {
            FolderChoice::AppDefault => match &root {
                Some(root) => {
                    ui.label(RichText::new(root.as_str()).strong());
                    ui.label(tl!("The default folder set in Settings."));
                }
                None => {
                    ui.label(tl!("No default folder is set, so this is the same as the system default."));
                }
            },
            FolderChoice::System => {
                ui.label(tl!("The system's usual place for apps."));
            }
            FolderChoice::Custom => {
                let label = ui.label(tl!("Install folder"));
                ui.add(egui::TextEdit::singleline(folder).id_salt("install_folder").desired_width(f32::INFINITY)).labelled_by(label.id);
                ui.label(tl!("Apps are placed inside this folder."));
                if widgets::button(ui, tl!("Browse…"), Kind::Secondary, 100.0).clicked()
                    && let Some(selected) = host.pick_folder(if folder.is_empty() { root.as_deref() } else { Some(folder.as_str()) })
                {
                    *folder = selected;
                }
            }
        }
        let custom = (*choice == FolderChoice::Custom).then(|| folder.trim()).filter(|f| !f.is_empty()).map(str::to_string);
        let target = match *choice {
            FolderChoice::AppDefault => root.clone(),
            FolderChoice::System => None,
            FolderChoice::Custom => custom.clone().or_else(|| root.clone()),
        };
        self.free_space_note(ui, id, target);
        // Only a folder that isn't already the default can become it.
        let can_make_default = custom.as_ref().is_some_and(|f| root.as_ref() != Some(f));
        if can_make_default {
            ui.checkbox(make_default, tl!("Make this my default install folder"));
            ui.label(tl!("You can change the default anytime in Settings."));
        }
        let goes_to_custom_folder = match *choice {
            FolderChoice::AppDefault => root.is_some(),
            FolderChoice::System => false,
            FolderChoice::Custom => custom.is_some() || root.is_some(),
        };
        if goes_to_custom_folder && self.platform.is_some_and(|p| p.os == Os::Linux) {
            ui.label(tl!("Custom folders use AppImage on Linux."));
        }
        ui.add_space(14.0);
        let mut close = false;
        ui.horizontal(|ui| {
            if widgets::button(ui, tl!("Install"), Kind::Primary, 100.0).clicked() {
                let locations = &mut self.settings.install_locations;
                locations.per_app.remove(id);
                locations.system.remove(id);
                match (*choice, custom) {
                    // Kept even with no default folder yet, so setting one later doesn't move it.
                    (FolderChoice::System, _) => {
                        locations.system.insert(id.to_string());
                    }
                    // The app follows the new default rather than keeping its own copy of it.
                    (FolderChoice::Custom, Some(folder)) if can_make_default && *make_default => locations.root = Some(folder),
                    (FolderChoice::Custom, Some(folder)) => {
                        locations.per_app.insert(id.to_string(), folder);
                    }
                    // The app default, or a custom folder left empty.
                    _ => {}
                }
                self.configure();
                self.send(Command::Install { app: id.to_string() });
                close = true;
            }
            close |= widgets::button(ui, tl!("Cancel"), Kind::Secondary, 90.0).clicked();
        });
        close
    }

    /// How much room the drive under `target` has, and a warning when the app may not fit.
    fn free_space_note(&mut self, ui: &mut egui::Ui, id: &str, target: Option<String>) {
        // The backend measures off the UI thread: asked again when the folder changes, and every
        // few seconds while the dialog is open.
        let now = ui.input(|i| i.time);
        if self.free_space_asked.as_ref().is_none_or(|(asked, at)| *asked != target || now - at > 5.0) {
            self.send(Command::FreeSpace { folder: target.clone() });
            self.free_space_asked = Some((target.clone(), now));
        }
        // A late answer for a folder no longer chosen isn't shown.
        let Some(free) = self.free_space.as_ref().filter(|(folder, _)| *folder == target).and_then(|(_, bytes)| *bytes) else { return };
        let download = self.platform.zip(self.entry(id)).and_then(|(p, entry)| {
            let stems: Vec<&str> = entry.app.stems().collect();
            pick_asset(entry.release()?, &stems, p, p.default_kinds()).map(|(asset, _)| asset.size)
        });
        let t = Tokens::get(ui.ctx());
        ui.label(RichText::new(fmt(tl!("{size} free on this drive"), &[("size", &widgets::human_bytes(free))])).color(t.muted));
        // Unpacking needs about as much room again as the download itself.
        if let Some(download) = download.filter(|d| free < d.saturating_mul(2)) {
            let warning =
                fmt(tl!("This drive may not have enough space. The download alone is {size}."), &[("size", &widgets::human_bytes(download))]);
            ui.label(RichText::new(warning).color(t.bad));
        }
    }

    pub(crate) fn folder_field(ui: &mut egui::Ui, folder: &mut String, default: Option<&str>, host: &mut dyn Host) {
        let label = ui.label(tl!("Install folder"));
        ui.add(
            egui::TextEdit::singleline(folder)
                .id_salt("install_folder")
                .hint_text(default.unwrap_or(tl!("System default")))
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
        ui.label(tl!("Apps are placed inside this folder."));
        ui.horizontal(|ui| {
            if widgets::button(ui, tl!("Browse…"), Kind::Secondary, 100.0).clicked()
                && let Some(selected) = host.pick_folder(if folder.is_empty() { default } else { Some(folder.as_str()) })
            {
                *folder = selected;
            }
            if widgets::button(ui, tl!("Use default"), Kind::Secondary, 120.0).clicked() {
                folder.clear();
            }
        });
    }
}
