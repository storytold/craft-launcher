//! The Settings dialog: a large card in the shape of the app page (see `sheet.rs`), with the
//! groups of settings in boxes, two columns once the window is wide enough.

use artcraft_launcher_core::{Os, PackageKind};
use egui::{RichText, Ui, vec2};

use crate::app::{Host, LauncherUi};
use crate::i18n::fmt;
use crate::settings::{CheckEvery, Settings};
use crate::sheet;
use crate::theme::{self, ThemeChoice, Tokens};
use crate::widgets::{self, Cell, Kind};

/// A label with its control at the right; the control drops under the label when they don't fit.
fn row(ui: &mut Ui, label: &str, control_width: f32, add: &mut dyn FnMut(&mut Ui)) {
    let t = Tokens::get(ui.ctx());
    let font = egui::TextStyle::Body.resolve(ui.style());
    let label_width = ui.painter().layout_no_wrap(label.to_string(), font, t.ink).size().x;
    if label_width + control_width + ui.spacing().item_spacing.x > ui.available_width() {
        ui.label(label);
        add(ui);
    } else {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| add(ui));
        });
    }
}

fn note(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new(text).size(12.0).color(t.muted));
}

impl LauncherUi {
    pub(crate) fn settings_dialog(&mut self, ui: &mut Ui, host: &mut dyn Host) -> bool {
        let t = Tokens::get(ui.ctx());
        let size = sheet::size(ui.ctx(), vec2(1000.0, 820.0));
        ui.set_width(size.x);
        ui.set_max_height(size.y);
        let hero_h = sheet::hero(
            ui,
            Some(crate::icons::artcraft_mark()),
            tl!("Settings"),
            &format!("ArtCraft Launcher {}", env!("CARGO_PKG_VERSION")),
            |_| {},
        );

        let quit_width = widgets::button_width(ui, tl!("Quit ArtCraft Launcher"), 0.0);
        let done_width = widgets::button_width(ui, tl!("Done"), 90.0);
        let stack_actions = self.caps.manage_apps && quit_width + done_width + ui.spacing().item_spacing.x + 48.0 > size.x;
        let footer_h = if stack_actions { 2.0 * theme::CONTROL_H + ui.spacing().item_spacing.y } else { theme::CONTROL_H } + 32.0;
        let body_h = (size.y - hero_h - footer_h - 2.0).max(0.0);

        let wide = size.x >= theme::DIALOG_COLUMNS_FROM;
        let saved = sheet::accent_scroll(ui, &t, t.accent);
        egui::ScrollArea::vertical().id_salt("settings-body").max_height(body_h).auto_shrink([false, true]).show(ui, |ui| {
            *ui.visuals_mut() = saved;
            egui::Frame::new().inner_margin(egui::Margin::same(theme::PAD as i8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.settings_contents(ui, host, wide);
            });
        });

        let rule_y = ui.cursor().top();
        ui.painter().hline(ui.max_rect().x_range(), rule_y, t.hairline());
        let mut close = false;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(theme::PAD as i8, 16)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let quit = |ui: &mut Ui, host: &mut dyn Host| {
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
        });
        close
    }

    fn settings_contents(&mut self, ui: &mut Ui, host: &mut dyn Host, wide: bool) {
        // The columns are drawn from two closures that both need `self` and `host`: the pieces
        // are drawn one after the other, through a cell, never at the same time (were they ever,
        // the second would be skipped rather than panic).
        let state = std::cell::RefCell::new((&mut *self, host));
        sheet::columns(
            ui,
            wide,
            theme::COLUMN_GAP,
            |ui| {
                let Ok(mut state) = state.try_borrow_mut() else { return };
                let (this, _) = &mut *state;
                this.settings_language(ui);
                this.settings_updates(ui);
                this.settings_appearance(ui);
            },
            |ui| {
                let Ok(mut state) = state.try_borrow_mut() else { return };
                let (this, host) = &mut *state;
                this.settings_install(ui, &mut **host);
                this.settings_background(ui, &mut **host);
                this.settings_community(ui);
            },
        );
    }

    fn settings_language(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let mut lang_code = self.settings.language.clone();
        let installed = self.cjk_font;
        sheet::panel(ui, tl!("Language"), |ui| {
            let width = ui.available_width();
            egui::ComboBox::from_id_salt("language").selected_text(Settings::language_label(&lang_code, installed)).width(width).show_ui(ui, |ui| {
                ui.selectable_value(&mut lang_code, Settings::LANGUAGE_AUTO.to_string(), tl!("Auto"));
                for lang in crate::i18n::Lang::all() {
                    ui.selectable_value(&mut lang_code, lang.code().to_string(), Settings::language_label(lang.code(), installed));
                }
            });
        });
        if lang_code != self.settings.language {
            self.settings.language = lang_code;
            crate::i18n::set_current(crate::app::effective_lang(&self.settings.language));
        }
        let pref = crate::i18n::Lang::from_pref(&self.settings.language);
        if pref.needs_cjk_font() && !theme::cjk_font_available(pref) {
            // In English on purpose: the language asked for can't be drawn.
            let text = "No font for this language was found on this system, so the interface stays in English. \
                        Installing one (for example Noto Sans CJK) and restarting fixes it.";
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(t.warn)).wrap());
            ui.add_space(14.0);
        }
    }

    fn settings_updates(&mut self, ui: &mut Ui) {
        sheet::panel(ui, tl!("Updates"), |ui| {
            let mut every = self.settings.check_every;
            row(ui, tl!("Check for updates"), 150.0, &mut |ui| {
                egui::ComboBox::from_id_salt("every").selected_text(every.label()).width(150.0).show_ui(ui, |ui| {
                    for e in CheckEvery::ALL {
                        ui.selectable_value(&mut every, e, e.label());
                    }
                });
            });
            self.settings.check_every = every;
            if self.caps.manage_apps {
                // With the update service installed, this is the computer's setting: only an
                // administrator changes it (Windows asks), and nobody when Group Policy sets it.
                let machine = self.machine_auto_update;
                let can_change = !machine.is_some_and(|m| m.managed) && !self.machine_auto_update_pending;
                let mut on = self.settings.auto_update;
                if ui.add_enabled(can_change, egui::Checkbox::new(&mut on, tl!("Automatically download and install updates"))).changed() {
                    self.set_auto_update(on);
                }
                match machine {
                    Some(m) if m.managed => note(ui, tl!("Set by your administrator.")),
                    Some(_) => note(ui, tl!("For everyone on this computer. Changing it asks for administrator permission.")),
                    None => {}
                }
            }
            ui.checkbox(&mut self.settings.notify, tl!("Notify me when an installed app has an update"));
            ui.checkbox(&mut self.settings.prereleases, tl!("Include pre-releases (betas and release candidates)"));
        });
    }

    /// Where apps go and in what format, for the desktop builds that manage installs.
    fn settings_install(&mut self, ui: &mut Ui, host: &mut dyn Host) {
        if !self.caps.manage_apps {
            return;
        }
        sheet::panel(ui, tl!("Default install folder"), |ui| {
            if self.custom_install_folders() {
                let mut folder = self.settings.install_locations.root.clone().unwrap_or_default();
                Self::folder_field(ui, &mut folder, None, host);
                self.settings.install_locations.root = (!folder.is_empty()).then_some(folder);
                note(ui, tl!("New installs use this folder. Updates stay in their current location."));
                if self.platform.is_some_and(|p| p.os == Os::Linux) {
                    note(ui, tl!("On Linux, custom folders use AppImage format."));
                }
            } else {
                ui.label(tl!("The system manages install locations here."));
            }
        });

        if self.platform.is_some_and(|p| p.os == Os::Windows) {
            self.windows_kinds(ui);
        }

        if self.platform.is_some_and(|p| p.os == Os::Linux) {
            self.linux_kinds(ui);
        }
    }

    /// The Windows install-format chooser: an MSI or a portable ZIP.
    fn windows_kinds(&mut self, ui: &mut Ui) {
        sheet::panel(ui, tl!("Installing on Windows"), |ui| {
            let mut choice = self
                .settings
                .preferred_kinds
                .first()
                .copied()
                .filter(|kind| matches!(kind, PackageKind::Msi | PackageKind::PortableZip))
                .or(Some(PackageKind::Msi));
            row(ui, tl!("Install new apps as"), 210.0, &mut |ui| {
                let label = choice.map_or_else(|| crate::i18n::t(PackageKind::Msi.label()), |kind| crate::i18n::t(kind.label()));
                egui::ComboBox::from_id_salt("windows-kind").selected_text(label).width(210.0).show_ui(ui, |ui| {
                    for kind in [PackageKind::Msi, PackageKind::PortableZip] {
                        ui.selectable_value(&mut choice, Some(kind), crate::i18n::t(kind.label()));
                    }
                });
            });
            self.settings.preferred_kinds = choice.into_iter().collect();
            note(ui, tl!("Updates always use the format an app was installed with."));
        });
    }

    /// The Linux install-format chooser: system packages, Flatpak, AppImage, and the sandbox-only
    /// tarball, with the format the launcher itself runs with as the default suggestion.
    fn linux_kinds(&mut self, ui: &mut Ui) {
        sheet::panel(ui, tl!("Installing on Linux"), |ui| {
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
                egui::ComboBox::from_id_salt("kind").selected_text(label).width(210.0).show_ui(ui, |ui| {
                    if !is_flatpak && !is_appimage {
                        ui.selectable_value(&mut choice, None, tl!("System package (recommended)"));
                    }
                    for k in [PackageKind::Deb, PackageKind::Rpm, PackageKind::ArchPkg, PackageKind::Flatpak, PackageKind::AppImage] {
                        ui.selectable_value(&mut choice, Some(k), crate::i18n::t(k.label()));
                    }
                    // The tarball is unpacked and run inside the launcher's own sandbox, so it
                    // is only ever on offer there (as the fallback when the host has no Flatpak).
                    if is_flatpak {
                        ui.selectable_value(&mut choice, Some(PackageKind::TarGz), crate::i18n::t(PackageKind::TarGz.label()));
                    }
                });
            });
            self.settings.preferred_kinds = choice.into_iter().collect();
            note(ui, tl!("Updates always use the format an app was installed with."));
        });
    }

    fn settings_background(&mut self, ui: &mut Ui, host: &mut dyn Host) {
        // Nothing to show on a host with no tray, desktops or login item: no empty box.
        if !self.caps.manage_apps || !(host.has_tray() || host.has_desktops() || host.login_item().is_some()) {
            return;
        }
        sheet::panel(ui, tl!("Running in the background"), |ui| {
            if host.has_tray() {
                ui.checkbox(&mut self.settings.close_to_tray, tl!("Keep running in the tray when the window is closed"));
            }
            if host.has_desktops() {
                ui.checkbox(&mut self.settings.show_on_current_desktop, tl!("Show the window on the current desktop")).on_hover_text(tl!(
                    "When on, opening the launcher from the menu bar brings its window to the desktop you're on. When off, macOS switches you to the desktop the window was on."
                ));
            }
            if let Some(mut on) = host.login_item()
                && ui.checkbox(&mut on, tl!("Start ArtCraft Launcher when I log in")).changed()
                && let Err(e) = host.set_login_item(on)
            {
                let ctx = ui.ctx().clone();
                self.toast(&ctx, fmt(tl!("Couldn't change the login item: {e}"), &[("e", &e)]), true);
            }
        });
    }

    fn settings_community(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        sheet::panel(ui, tl!("Community"), |ui| {
            ui.label(RichText::new(tl!("Questions, ideas and news from the ArtCraft team")).color(t.muted));
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                use artcraft_launcher_core::catalog::{GITHUB_URL, WEBSITE_URL};
                if widgets::button(ui, tl!("Join the Discord"), Kind::Primary, 0.0).on_hover_text(self.discord_url).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(self.discord_url));
                }
                if widgets::button(ui, tl!("Website"), Kind::Secondary, 0.0).on_hover_text(WEBSITE_URL).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(WEBSITE_URL));
                }
                if widgets::button(ui, tl!("GitHub"), Kind::Secondary, 0.0).on_hover_text(GITHUB_URL).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(GITHUB_URL));
                }
                // Wrap the social links as a group so their icons stay together.
                let width = widgets::icon_links_width(artcraft_launcher_core::catalog::SOCIAL.len(), 16.0);
                ui.allocate_ui_with_layout(vec2(width, theme::CONTROL_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    widgets::social_icons(ui, 16.0);
                });
            });
        });
    }

    fn settings_appearance(&mut self, ui: &mut Ui) {
        let mut theme_choice = self.settings.theme;
        sheet::panel(ui, tl!("Appearance"), |ui| {
            ui.horizontal(|ui| {
                widgets::cells(ui, &mut theme_choice, &ThemeChoice::ALL.map(|c| (c, Cell { label: c.label(), tip: c.label(), count: None })));
            });
        });
        self.settings.theme = theme_choice;
    }
}
