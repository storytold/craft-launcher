//! Modal dialogs: release notes, uninstall confirmation, settings.

use crate::rtl::LocalizedTooltip as _;

use crate::i18n::fmt;
use artcraft_launcher_core::protocol::Command;
use artcraft_launcher_core::{Os, PackageKind};
use egui::{RichText, vec2};

use crate::app::{Dialog, Host, LauncherUi};
use crate::settings::{CheckEvery, Settings};
use crate::theme::{self, ThemeChoice, Tokens};
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
        let modal = egui::Modal::new(egui::Id::new("dialog").with(dialog.key()))
            .frame(egui::Frame::new().fill(t.bg).stroke(egui::Stroke::new(1.0, t.line_strong)).shadow(theme::shadow(&t)).inner_margin(24));
        let mut close = false;
        let response = match &mut dialog {
            Dialog::Notes(id) => modal.show(ctx, |ui| close = self.notes_dialog(ui, id)),
            Dialog::ConfirmUninstall(id) => modal.show(ctx, |ui| close = self.uninstall_dialog(ui, id)),
            Dialog::Install { app, folder } => modal.show(ctx, |ui| close = self.install_dialog(ui, app, folder, host)),
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
        let modal = egui::Modal::new(egui::Id::new("feed_alert").with(&alert.id))
            .frame(egui::Frame::new().fill(t.bg).stroke(egui::Stroke::new(1.0, t.line_strong)).shadow(theme::shadow(&t)).inner_margin(24));
        let mut ok = false;
        modal.show(ctx, |ui| {
            ui.set_width(440.0f32.min(ctx.content_rect().width() - 80.0));
            crate::rtl::paragraph(ui, &alert.title, egui::FontId::new(18.0, theme::semibold()), t.ink_strong);
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.5).show(ui, |ui| {
                crate::rtl::paragraph(ui, &alert.body, egui::FontId::proportional(14.0), t.muted);
            });
            ui.add_space(14.0);
            // A row: a right-to-left layout straight in the column would fill the height left.
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ok = widgets::button(ui, tl!("OK"), Kind::Primary, 90.0).clicked();
                    if let Some(link) = &alert.link
                        && widgets::button(ui, tl!("Open link"), Kind::Secondary, 110.0).localized_hover_text(link.as_str()).clicked()
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
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                let response = ui.label(widgets::heading(title, 22.0, t.ink_strong));
                crate::rtl::accessible(response, title);
                if !subtitle.is_empty() {
                    widgets::hud(ui, subtitle, t.faint);
                }
            });
        });
        ui.add_space(14.0);
    }

    fn notes_dialog(&mut self, ui: &mut egui::Ui, id: &str) -> bool {
        let t = Tokens::get(ui.ctx());
        let Some(entry) = self.entry(id).cloned() else { return true };
        let Some(release) = entry.release().cloned() else { return true };
        ui.set_width(560.0f32.min(ui.ctx().content_rect().width() - 80.0));
        let date = release.published_at.map(|d| fmt(tl!("Released {date}"), &[("date", &crate::views::date_text(d))])).unwrap_or_default();
        Self::title(ui, crate::icons::app(id), &format!("{} {}", entry.app.name, release.version_label()), &date);
        egui::ScrollArea::vertical().max_height(ui.ctx().content_rect().height() * 0.6).show(ui, |ui| {
            if release.notes.is_empty() {
                crate::rtl::paragraph(
                    ui,
                    tl!("These notes weren't fetched (GitHub's request limit was reached). They're on the release page."),
                    egui::FontId::proportional(14.0),
                    t.muted,
                );
            } else {
                crate::notes::show_scaled(ui, &release.notes, self.notes_zoom);
            }
        });
        ui.add_space(12.0);
        let mut close = false;
        ui.horizontal(|ui| {
            widgets::hud_link(ui, tl!("Open on GitHub"), &release.html_url);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = widgets::button(ui, tl!("Close"), Kind::Secondary, 90.0).clicked();
            });
        });
        close
    }

    fn launcher_update_dialog(&mut self, ui: &mut egui::Ui, r: &artcraft_launcher_core::Release) -> bool {
        let t = Tokens::get(ui.ctx());
        ui.set_width(480.0f32.min(ui.ctx().content_rect().width() - 80.0));
        let title = fmt(tl!("ArtCraft Launcher {version} is available."), &[("version", &r.version_label())]);
        Self::title(ui, Some(crate::icons::artcraft_mark()), &title, "");
        crate::rtl::paragraph(
            ui,
            tl!("A new version is available. Would you like to relaunch with the updated version?"),
            egui::FontId::proportional(14.0),
            t.muted,
        );
        ui.add_space(14.0);
        let mut close = false;
        ui.horizontal(|ui| {
            widgets::hud_link(ui, tl!("What's new"), &r.html_url);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.caps.self_update {
                    if widgets::button(ui, tl!("Update and restart"), Kind::Primary, 0.0)
                        .localized_hover_text(tl!("Downloads the new version, replaces this one and opens it again"))
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
        crate::rtl::paragraph(ui, how, egui::FontId::proportional(14.0), t.muted);
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
        crate::rtl::paragraph(
            ui,
            &fmt(tl!("{path} already exists. Installing {name} will overwrite it."), &[("path", path), ("name", app.name)]),
            egui::FontId::proportional(14.0),
            t.muted,
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

    fn install_dialog(&mut self, ui: &mut egui::Ui, id: &str, folder: &mut String, host: &mut dyn Host) -> bool {
        let t = Tokens::get(ui.ctx());
        let Some(app) = self.entry(id).map(|e| e.app) else { return true };
        ui.set_width(440.0f32.min(ui.ctx().content_rect().width() - 80.0));
        Self::title(ui, None, app.name, tl!("Install folder"));
        let default = self.settings.install_locations.root.as_deref().unwrap_or(tl!("System default"));
        Self::folder_field(ui, folder, Some(default), host);
        if self.platform.is_some_and(|p| p.os == Os::Linux) {
            crate::rtl::paragraph(ui, tl!("Custom folders use AppImage on Linux."), egui::FontId::proportional(14.0), t.muted);
        }
        ui.add_space(14.0);
        let mut close = false;
        ui.horizontal(|ui| {
            if widgets::button(ui, tl!("Install"), Kind::Primary, 100.0).clicked() {
                if folder.is_empty() {
                    self.settings.install_locations.per_app.remove(id);
                } else {
                    self.settings.install_locations.per_app.insert(id.to_string(), folder.clone());
                }
                self.configure();
                self.send(Command::Install { app: id.to_string() });
                close = true;
            }
            close |= widgets::button(ui, tl!("Cancel"), Kind::Secondary, 90.0).clicked();
        });
        close
    }

    fn folder_field(ui: &mut egui::Ui, folder: &mut String, default: Option<&str>, host: &mut dyn Host) {
        let label = crate::rtl::label(ui, tl!("Install folder"), egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color());
        ui.add(
            egui::TextEdit::singleline(folder)
                .id_salt("install_folder")
                .hint_text(default.unwrap_or(tl!("System default")))
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
        crate::rtl::label(ui, tl!("Apps are placed inside this folder."), egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color());
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

    fn settings_dialog(&mut self, ui: &mut egui::Ui, host: &mut dyn Host) -> bool {
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| self.settings_dialog_body(ui, host)).inner
    }

    fn settings_dialog_body(&mut self, ui: &mut egui::Ui, host: &mut dyn Host) -> bool {
        // Leave room for the frame (48 px) and a 16 px gutter on each side.
        let viewport = ui.ctx().content_rect();
        ui.set_width(520.0_f32.min((viewport.width() - 80.0).max(0.0)));
        // The area remembers its previous size. Reset the available height so the scroll area
        // measures against the viewport immediately, including on the first frame after a resize.
        ui.set_max_height((viewport.height() - 80.0).max(0.0));
        ui.spacing_mut().scroll = egui::style::ScrollStyle { foreground_color: true, ..egui::style::ScrollStyle::solid() };
        let title_top = ui.cursor().top();
        Self::title(ui, Some(crate::icons::artcraft_mark()), tl!("Settings"), &format!("ArtCraft Launcher {}", env!("CARGO_PKG_VERSION")));
        let title_height = ui.cursor().top() - title_top;
        let quit_width = widgets::button_width(ui, tl!("Quit ArtCraft Launcher"), 0.0);
        let done_width = widgets::button_width(ui, tl!("Done"), 90.0);
        let stack_actions = self.caps.manage_apps && quit_width + done_width + ui.spacing().item_spacing.x > ui.available_width();
        let footer_height = if stack_actions { 2.0 * theme::CONTROL_H + ui.spacing().item_spacing.y } else { theme::CONTROL_H };
        let body_height = (viewport.height() - 80.0 - title_height - footer_height - 24.0 - ui.spacing().item_spacing.y).max(0.0);
        // A stable 16px gutter separates right-aligned content from the scrollbar.
        let persian = crate::i18n::current().is_rtl();
        if persian {
            ui.spacing_mut().scroll.bar_inner_margin = 12.0;
        }
        egui::ScrollArea::vertical().id_salt("settings-body").max_height(body_height).auto_shrink([false, true]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin { left: 0, right: if persian { 16 } else { 0 }, top: 0, bottom: 0 }).show(ui, |ui| {
                ui.with_layout(crate::rtl::content_column(), |ui| {
                    self.settings_contents(ui, host);
                });
            });
        });

        ui.add_space(24.0);
        let mut close = false;
        let quit = |ui: &mut egui::Ui, host: &mut dyn Host| {
            if self.caps.manage_apps && widgets::button(ui, tl!("Quit ArtCraft Launcher"), Kind::Secondary, 0.0).clicked() {
                host.quit();
            }
        };
        if stack_actions {
            quit(ui, host);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = widgets::button(ui, tl!("Done"), Kind::Primary, 90.0).clicked();
            });
        } else {
            ui.horizontal(|ui| {
                quit(ui, host);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = widgets::button(ui, tl!("Done"), Kind::Primary, 90.0).clicked();
                });
            });
        }
        close
    }

    fn settings_folder(ui: &mut egui::Ui, folder: &mut String, host: &mut dyn Host) {
        // Paths always retain LTR editing, even inside the RTL settings body.
        let previous = folder.clone();
        let field = ui.add(
            egui::TextEdit::singleline(folder)
                .id_salt("settings_install_folder")
                .hint_text(crate::rtl::visual(tl!("System default")))
                .desired_width(f32::INFINITY),
        );
        field.widget_info(|| egui::WidgetInfo::text_edit(ui.is_enabled(), &previous, folder.as_str(), tl!("System default")));
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, tl!("Browse…"), Kind::Secondary, 108.0).clicked()
                && let Some(selected) = host.pick_folder(if folder.is_empty() { None } else { Some(folder.as_str()) })
            {
                *folder = selected;
            }
            if widgets::button(ui, tl!("Use default"), Kind::Secondary, 128.0).clicked() {
                folder.clear();
            }
        });
    }

    fn settings_contents(&mut self, ui: &mut egui::Ui, host: &mut dyn Host) {
        let t = Tokens::get(ui.ctx());
        let section = |ui: &mut egui::Ui, s: &str| {
            ui.add_space(14.0);
            if crate::i18n::current().is_rtl() {
                crate::rtl::label(ui, s, egui::FontId::proportional(11.0), t.faint);
            } else {
                widgets::hud(ui, s, t.faint);
            }
            ui.add_space(6.0);
        };
        let row = |ui: &mut egui::Ui, label: &str, control_width: f32, add: &mut dyn FnMut(&mut egui::Ui)| {
            let option_font =
                if crate::i18n::current().is_rtl() { egui::FontId::new(14.0, theme::medium()) } else { egui::TextStyle::Body.resolve(ui.style()) };
            let font = egui::TextStyle::Body.resolve(ui.style());
            let label_width = ui.painter().layout_no_wrap(label.to_string(), font, t.ink).size().x;
            if label_width + control_width + ui.spacing().item_spacing.x > ui.available_width() {
                crate::rtl::label(ui, label, option_font.clone(), t.ink);
                add(ui);
            } else {
                ui.horizontal(|ui| {
                    crate::rtl::label(ui, label, option_font.clone(), t.ink);
                    ui.with_layout(crate::rtl::content_trailing(egui::Align::Center), |ui| add(ui));
                });
            }
        };

        let mut lang_code = self.settings.language.clone();
        let installed = self.cjk_font;
        row(ui, tl!("Language"), 180.0, &mut |ui| {
            egui::ComboBox::from_id_salt("language")
                .selected_text(Settings::language_label(&lang_code, installed))
                .width(180.0)
                .popup_style(widgets::locale_popup_style())
                .show_ui(ui, |ui| {
                    widgets::popup_body(ui, |ui| {
                        widgets::popup_value(ui, &mut lang_code, Settings::LANGUAGE_AUTO.to_string(), tl!("Auto"));
                        for lang in crate::i18n::Lang::all() {
                            widgets::popup_value(ui, &mut lang_code, lang.code().to_string(), &Settings::language_label(lang.code(), installed));
                        }
                    })
                });
        });
        if lang_code != self.settings.language {
            self.settings.language = lang_code;
            crate::i18n::set_current(crate::app::effective_lang(&self.settings.language));
        }
        let pref = crate::i18n::Lang::from_pref(&self.settings.language);
        if pref.needs_cjk_font() && !theme::cjk_font_available(pref) {
            // In English on purpose: the language asked for can't be drawn.
            let note = "No font for this language was found on this system, so the interface stays in English. \
                        Installing one (for example Noto Sans CJK) and restarting fixes it.";
            crate::rtl::paragraph(ui, note, egui::FontId::proportional(12.0), t.warn);
        }

        section(ui, tl!("Updates"));
        let mut every = self.settings.check_every;
        row(ui, tl!("Check for updates"), 150.0, &mut |ui| {
            let response = egui::ComboBox::from_id_salt("every")
                .selected_text(crate::rtl::visual(every.label()))
                .width(150.0)
                .popup_style(widgets::locale_popup_style())
                .show_ui(ui, |ui| {
                    widgets::popup_body(ui, |ui| {
                        for e in CheckEvery::ALL {
                            widgets::popup_value(ui, &mut every, e, &e.label());
                        }
                    })
                });
            if crate::i18n::current().is_rtl() {
                response.response.widget_info(|| {
                    let mut info = egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), tl!("Check for updates"));
                    info.current_text_value = Some(every.label());
                    info
                });
            }
        });
        self.settings.check_every = every;
        if self.caps.manage_apps
            && widgets::locale_checkbox(ui, &mut self.settings.auto_update, tl!("Automatically download and install updates")).changed()
            && self.settings.auto_update
        {
            self.trigger_auto_updates();
        }
        widgets::locale_checkbox(ui, &mut self.settings.notify, tl!("Notify me when an installed app has an update"));
        widgets::locale_checkbox(ui, &mut self.settings.prereleases, tl!("Include pre-releases (betas and release candidates)"));

        if self.caps.manage_apps {
            section(ui, tl!("Default install folder"));
            if self.custom_install_folders() {
                let mut folder = self.settings.install_locations.root.clone().unwrap_or_default();
                if crate::i18n::current().is_rtl() {
                    Self::settings_folder(ui, &mut folder, host);
                } else {
                    Self::folder_field(ui, &mut folder, None, host);
                }
                self.settings.install_locations.root = (!folder.is_empty()).then_some(folder);
                ui.label(
                    RichText::new(crate::rtl::visual(tl!("New installs use this folder. Updates stay in their current location.")))
                        .size(12.0)
                        .color(t.muted),
                );
                if self.platform.is_some_and(|p| p.os == Os::Linux) {
                    crate::rtl::paragraph(ui, tl!("Custom folders use AppImage on Linux."), egui::FontId::proportional(12.0), t.muted);
                }
            } else {
                crate::rtl::paragraph(ui, tl!("The system manages install locations here."), egui::FontId::proportional(14.0), t.muted);
            }
        }

        if self.caps.manage_apps && self.platform.is_some_and(|p| p.os == Os::Windows) {
            section(ui, tl!("Installing on Windows"));
            let mut choice = self
                .settings
                .preferred_kinds
                .first()
                .copied()
                .filter(|kind| matches!(kind, PackageKind::Msi | PackageKind::PortableZip))
                .or(Some(PackageKind::Msi));
            row(ui, tl!("Install new apps as"), 210.0, &mut |ui| {
                let label = choice.map_or_else(|| crate::i18n::t(PackageKind::Msi.label()), |kind| crate::i18n::t(kind.label()));
                egui::ComboBox::from_id_salt("windows-kind").selected_text(label).width(210.0).popup_style(widgets::locale_popup_style()).show_ui(
                    ui,
                    |ui| {
                        widgets::popup_body(ui, |ui| {
                            for kind in [PackageKind::Msi, PackageKind::PortableZip] {
                                widgets::popup_value(ui, &mut choice, Some(kind), crate::i18n::t(kind.label()));
                            }
                        })
                    },
                );
            });
            self.settings.preferred_kinds = choice.into_iter().collect();
            crate::rtl::paragraph(ui, tl!("Updates always use the format an app was installed with."), egui::FontId::proportional(12.0), t.muted);
        }

        if self.caps.manage_apps && self.platform.is_some_and(|p| p.os == Os::Linux) {
            section(ui, tl!("Installing on Linux"));
            let is_flatpak = crate::settings::is_flatpak_sandbox();
            let is_appimage = crate::settings::is_appimage();
            let default_kind = if is_flatpak {
                Some(PackageKind::Flatpak)
            } else if is_appimage {
                Some(PackageKind::AppImage)
            } else {
                None
            };
            let mut choice = self.settings.preferred_kinds.first().copied().or(default_kind);
            row(ui, tl!("Install new apps as"), 210.0, &mut |ui| {
                let default_label = match default_kind {
                    Some(k) => crate::i18n::t(k.label()),
                    None => tl!("System package (recommended)"),
                };
                let label = choice.map_or(default_label, |k| crate::i18n::t(k.label()));
                egui::ComboBox::from_id_salt("kind").selected_text(label).width(210.0).popup_style(widgets::locale_popup_style()).show_ui(ui, |ui| {
                    widgets::popup_body(ui, |ui| {
                        if !is_flatpak && !is_appimage {
                            widgets::popup_value(ui, &mut choice, None, tl!("System package (recommended)"));
                        }
                        for k in [PackageKind::Deb, PackageKind::Rpm, PackageKind::Flatpak, PackageKind::AppImage] {
                            widgets::popup_value(ui, &mut choice, Some(k), crate::i18n::t(k.label()));
                        }
                        // The tarball is unpacked and run inside the launcher's own sandbox, so it
                        // is only ever on offer there (as the fallback when the host has no Flatpak).
                        if is_flatpak {
                            widgets::popup_value(ui, &mut choice, Some(PackageKind::TarGz), crate::i18n::t(PackageKind::TarGz.label()));
                        }
                    })
                });
            });
            self.settings.preferred_kinds = choice.into_iter().collect();
            crate::rtl::paragraph(ui, tl!("Updates always use the format an app was installed with."), egui::FontId::proportional(12.0), t.muted);
        }

        if self.caps.manage_apps && (!crate::i18n::current().is_rtl() || host.has_tray() || host.has_desktops() || host.login_item().is_some()) {
            section(ui, tl!("Running in the background"));
            if host.has_tray() {
                widgets::locale_checkbox(ui, &mut self.settings.close_to_tray, tl!("Keep running in the tray when the window is closed"));
            }
            if host.has_desktops() {
                widgets::locale_checkbox(ui, &mut self.settings.show_on_current_desktop, tl!("Show the window on the current desktop"))
                    .localized_hover_text(tl!("When on, opening the launcher from the menu bar brings its window to the desktop you're on. When off, macOS switches you to the desktop the window was on."));
            }
            if let Some(mut on) = host.login_item()
                && widgets::locale_checkbox(ui, &mut on, tl!("Start ArtCraft Launcher when I log in")).changed()
                && let Err(e) = host.set_login_item(on)
            {
                let ctx = ui.ctx().clone();
                self.toast(&ctx, fmt(tl!("Couldn't change the login item: {e}"), &[("e", &e)]), true);
            }
        }

        section(ui, tl!("Community"));
        crate::rtl::paragraph(ui, tl!("Questions, ideas and news from the ArtCraft team"), egui::FontId::proportional(14.0), t.muted);
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            use artcraft_launcher_core::catalog::{DISCORD_URL, GITHUB_URL, WEBSITE_URL};
            if widgets::button(ui, tl!("Join the Discord"), Kind::Primary, 0.0).localized_hover_text(DISCORD_URL).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(DISCORD_URL));
            }
            if widgets::button(ui, tl!("Website"), Kind::Secondary, 0.0).localized_hover_text(WEBSITE_URL).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(WEBSITE_URL));
            }
            if widgets::button(ui, tl!("GitHub"), Kind::Secondary, 0.0).localized_hover_text(GITHUB_URL).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(GITHUB_URL));
            }
            // Wrap the social links as a group so their icons stay together.
            let count = artcraft_launcher_core::catalog::SOCIAL.len() as f32;
            let width = count * (16.0 + 8.0) + (count - 1.0).max(0.0) * 4.0;
            ui.allocate_ui_with_layout(vec2(width, theme::CONTROL_H), crate::rtl::content_row(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                widgets::social_icons(ui, 16.0);
            });
        });

        section(ui, tl!("Appearance"));
        let mut theme_choice = self.settings.theme;
        ui.horizontal(|ui| {
            widgets::cells(ui, &mut theme_choice, &ThemeChoice::ALL.map(|c| (c, Cell { label: c.label(), tip: c.label(), count: None })));
        });
        self.settings.theme = theme_choice;
    }
}
