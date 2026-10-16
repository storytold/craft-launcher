//! The Plugins page: import, inspect, repair, and uninstall local WebAssembly (ABI v1)
//! and compatible Adobe Photoshop (`.afs` / `.8bf` / preset) plug-ins per Craft app.

use artcraft_launcher_core::plugins::{InstalledPlugin, PluginFormat, PluginStatus, plugin_status};
use artcraft_launcher_core::protocol::Command;
use artcraft_launcher_core::{CraftApp, SUITE};
use egui::{Color32, CornerRadius, FontId, Rect, RichText, Sense, Ui, vec2};

use crate::app::{Dialog, LauncherUi, Page};
use crate::i18n::{current, tn, tr_ctx};
use crate::model::Tone;
use crate::settings::ViewMode;
use crate::theme::{self, Tokens, rgb, tint};
use crate::widgets::{self, Cell, Kind};

const PLUGIN_CARD_MIN_W: f32 = 340.0;
const PLUGIN_CARD_H: f32 = 304.0;
const STRIP_H: f32 = 40.0;
const CARD_PAD: f32 = 20.0;

impl LauncherUi {
    /// Opens the Plugins page filtered to a specific Craft app (or `"all"`).
    pub(crate) fn open_plugins_for(&mut self, app_id: &str) {
        self.page = Page::Plugins;
        self.plugin_app_filter = CraftApp::by_id(app_id).map_or("all", |a| a.id);
    }

    pub(crate) fn plugins_toolbar(&mut self, ui: &mut Ui) {
        self.announcements(ui);
        let all_plugins = &self.installed_plugins;
        ui.horizontal(|ui| {
            let mut filter_apps: Vec<&'static CraftApp> = Vec::new();
            for app in &SUITE {
                let has_any = all_plugins.iter().any(|p| p.app == app.id) || self.plugin_app_filter == app.id;
                if has_any {
                    filter_apps.push(app);
                }
            }

            let mut cells: Vec<(&'static str, Cell<'_>)> = Vec::with_capacity(1 + filter_apps.len());
            cells.push((
                "all",
                Cell {
                    label: tl!("All"),
                    tip: tl!("Browse and manage plugins"),
                    count: Some((all_plugins.len(), None)),
                },
            ));
            for app in filter_apps {
                let count = all_plugins.iter().filter(|p| p.app == app.id).count();
                cells.push((
                    app.id,
                    Cell {
                        label: app.name,
                        tip: tr_ctx(current(), app.id, app.tagline),
                        count: Some((count, None)),
                    },
                ));
            }
            widgets::cells(ui, &mut self.plugin_app_filter, &cells);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.caps.manage_apps
                    && widgets::button(ui, tl!("Import plugin…"), Kind::Primary, 0.0)
                        .on_hover_text(tl!(
                            "Supports native WebAssembly (.wasm ABI v1) plugins, Adobe Photoshop Filter Factory (.afs / .8bf) filters, and preset packs."
                        ))
                        .clicked()
                {
                    let target_app = if self.plugin_app_filter == "all" {
                        "photocraft".to_string()
                    } else {
                        self.plugin_app_filter.to_string()
                    };
                    self.dialog = Some(Dialog::ImportPlugin { app: target_app, path: String::new() });
                }
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.plugin_search)
                        .hint_text(tl!("Search plugins"))
                        .desired_width(200.0)
                        .min_size(vec2(0.0, theme::CONTROL_H))
                        .vertical_align(egui::Align::Center)
                        .margin(vec2(10.0, 0.0)),
                );
            });
        });
    }

    pub(crate) fn plugins_page(&mut self, ui: &mut Ui) {
        let tokens = Tokens::get(ui.ctx());
        self.plugins_toolbar(ui);
        ui.add_space(18.0);

        let needle = self.plugin_search.trim().to_lowercase();
        let filtered: Vec<InstalledPlugin> = self
            .installed_plugins
            .iter()
            .filter(|p| {
                let app_ok = self.plugin_app_filter == "all" || p.app == self.plugin_app_filter;
                let text_ok = needle.is_empty()
                    || p.name.to_lowercase().contains(&needle)
                    || p.tagline.to_lowercase().contains(&needle)
                    || p.author.to_lowercase().contains(&needle)
                    || p.format.label().to_lowercase().contains(&needle);
                app_ok && text_ok
            })
            .cloned()
            .collect();

        // The table is wider than a narrow window: let it scroll sideways too.
        let scroll = match self.settings.view {
            ViewMode::Grid => egui::ScrollArea::vertical(),
            ViewMode::Table => egui::ScrollArea::both(),
        };
        scroll.auto_shrink(false).show(ui, |ui| {
            if filtered.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    widgets::hud(ui, tl!("No plugins match"), tokens.muted);
                });
            } else {
                match self.settings.view {
                    ViewMode::Grid => {
                        if self.plugin_app_filter == "all" {
                            for (idx, app) in SUITE.iter().enumerate() {
                                let group: Vec<&InstalledPlugin> = filtered.iter().filter(|p| p.app == app.id).collect();
                                if group.is_empty() {
                                    continue;
                                }
                                ui.add_space(4.0);
                                let number = format!("{:02}", idx + 1);
                                ui.horizontal(|ui| {
                                    widgets::eyebrow(ui, &number, app.name);
                                    ui.add_space(8.0);
                                    widgets::hud(ui, tr_ctx(current(), app.id, app.tagline), tokens.faint);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        widgets::hud(ui, &tn(group.len() as u64, "{n} plugin", "{n} plugins"), tokens.faint);
                                    });
                                });
                                ui.add_space(10.0);
                                self.plugin_card_grid(ui, &group);
                                ui.add_space(24.0);
                            }
                        } else {
                            let refs: Vec<&InstalledPlugin> = filtered.iter().collect();
                            self.plugin_card_grid(ui, &refs);
                            ui.add_space(24.0);
                        }
                    }
                    ViewMode::Table => {
                        let refs: Vec<&InstalledPlugin> = filtered.iter().collect();
                        self.plugin_table(ui, &refs);
                        ui.add_space(24.0);
                    }
                }
            }
            ui.add_space(16.0);
        });
    }

    /// Draws the plugin grid using the exact hairline-overlap layout as [`LauncherUi::grid`],
    /// so neighbouring cards share a 1 px border and incomplete rows never leave floating lines.
    fn plugin_card_grid(&mut self, ui: &mut Ui, plugins: &[&InstalledPlugin]) {
        let width = ui.available_width();
        let cols = ((width / PLUGIN_CARD_MIN_W).floor() as usize).max(1);
        let card_w = (width + (cols as f32 - 1.0)) / cols as f32;
        let rows = plugins.len().div_ceil(cols);
        let height = rows as f32 * (PLUGIN_CARD_H - 1.0) + 1.0;
        let (area, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());

        for (i, p) in plugins.iter().enumerate() {
            let (row, col) = (i / cols, i % cols);
            let min = area.min + vec2(col as f32 * (card_w - 1.0), row as f32 * (PLUGIN_CARD_H - 1.0));
            self.plugin_card(ui, p, i, Rect::from_min_size(min, vec2(card_w, PLUGIN_CARD_H)));
        }
    }

    /// Draws the plugins in a hairline table matching [`LauncherUi::table`].
    fn plugin_table(&mut self, ui: &mut Ui, plugins: &[&InstalledPlugin]) {
        use egui_extras::{Column, TableBuilder};
        let t = Tokens::get(ui.ctx());
        const TITLE_SIZE: f32 = 16.0;
        const TAGLINE_SIZE: f32 = 12.0;
        let (title_h, tagline_h) =
            ui.ctx().fonts_mut(|f| (f.row_height(&FontId::new(TITLE_SIZE, theme::display())), f.row_height(&FontId::proportional(TAGLINE_SIZE))));
        let clip = ui.clip_rect();
        let span = ui.available_rect_before_wrap().x_range();
        let rule = |ui: &Ui| {
            let y = ui.max_rect().bottom() + ui.spacing().item_spacing.y / 2.0;
            ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(clip).hline(span, y, t.hairline());
        };
        egui::Frame::new().fill(t.bg).stroke(t.hairline()).inner_margin(egui::Margin::symmetric(16, 0)).show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(false)
                .vscroll(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::remainder().at_least(220.0).clip(true))
                .column(Column::exact(136.0).clip(true))
                .column(Column::exact(156.0).clip(true))
                .column(Column::exact(144.0).clip(true))
                .column(Column::exact(168.0))
                .column(Column::exact(84.0))
                .column(Column::exact(206.0))
                .header(42.0, |mut header| {
                    for (idx, label) in
                        [tl!("Plugin"), tl!("App"), tl!("Format"), tl!("Author"), tl!("Status"), tl!("Version"), ""].into_iter().enumerate()
                    {
                        header.col(|ui| {
                            if idx == 0 {
                                rule(ui);
                            }
                            if !label.is_empty() {
                                widgets::hud(ui, label, t.faint);
                            }
                        });
                    }
                })
                .body(|mut body| {
                    let last = plugins.len().saturating_sub(1);
                    for (i, plugin) in plugins.iter().enumerate() {
                        let status = plugin_status(plugin);
                        body.row(62.0, |mut row| {
                            row.col(|ui| {
                                if i < last {
                                    rule(ui);
                                }
                                if let Some(icon) = crate::icons::app(&plugin.app) {
                                    ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(38.0, 38.0)));
                                }
                                ui.add_space(4.0);
                                ui.vertical(|ui| {
                                    const GAP: f32 = 2.0;
                                    ui.spacing_mut().item_spacing.y = GAP;
                                    let block = title_h + GAP + tagline_h;
                                    ui.add_space(((ui.available_height() - block) / 2.0).max(0.0));
                                    ui.add(
                                        egui::Label::new(FontId::new(TITLE_SIZE, theme::display()).into_rich_text(&plugin.name, t.ink_strong))
                                            .truncate(),
                                    );
                                    let translated_tagline = tr_ctx(current(), "plugin", &plugin.tagline);
                                    ui.add(egui::Label::new(RichText::new(translated_tagline).size(TAGLINE_SIZE).color(t.muted)).truncate());
                                });
                            });
                            row.col(|ui| {
                                if let Some(app) = CraftApp::by_id(&plugin.app) {
                                    widgets::wordmark(ui, app, 15.0);
                                } else {
                                    ui.label(RichText::new(&plugin.app).size(13.0).color(t.muted));
                                }
                            });
                            row.col(|ui| {
                                let (fmt_bg, fmt_fg) = format_colors(&t, plugin.format);
                                widgets::tag(ui, plugin.format.label(), fmt_bg, fmt_fg);
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(RichText::new(&plugin.author).size(13.0).color(t.muted)).truncate());
                            });
                            row.col(|ui| {
                                let (label, tone) = match status {
                                    PluginStatus::UpToDate => (tl!("Installed · up to date"), Tone::Good),
                                    PluginStatus::NeedsRepair => (tl!("Needs repair"), Tone::Warn),
                                };
                                widgets::badge(ui, label, widgets::tone_color(&t, tone));
                            });
                            row.col(|ui| {
                                let ver = format!("v{}", plugin.version);
                                ui.label(RichText::new(ver).family(egui::FontFamily::Monospace).color(t.ink));
                            });
                            row.col(|ui| self.plugin_actions(ui, plugin, status, 204.0));
                        });
                    }
                });
        });
    }

    fn plugin_card(&mut self, ui: &mut Ui, plugin: &InstalledPlugin, idx: usize, rect: Rect) {
        let t = Tokens::get(ui.ctx());
        let app_def = CraftApp::by_id(&plugin.app);
        let app_color = app_def.map_or(t.accent, |a| rgb(a.color));
        let hovered = ui.rect_contains_pointer(rect);
        let lift = ui.ctx().animate_bool_with_time(ui.id().with(("plugin-card-hover", &plugin.id)), hovered, 0.3);

        let painter = ui.painter_at(rect.expand(16.0));
        painter.rect(rect, CornerRadius::ZERO, t.bg, t.hairline(), egui::StrokeKind::Inside);
        // On hover, a 2 px rule in the host app's colour wipes in from the left along the top.
        if lift > 0.0 {
            painter.rect_filled(Rect::from_min_size(rect.min, vec2(rect.width() * lift, 2.0)), CornerRadius::ZERO, app_color);
        }

        // Top strip: index tag in app colour + format tag (no redundant app name or external store links).
        let strip = Rect::from_min_size(rect.min, vec2(rect.width(), STRIP_H));
        painter.hline(strip.x_range(), strip.bottom() - 0.5, t.hairline());
        let number = format!("{:02}", idx + 1);
        {
            let mut strip_ui = ui.new_child(
                egui::UiBuilder::new().max_rect(strip.shrink2(vec2(CARD_PAD, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            widgets::tag(&mut strip_ui, &number, app_color, Color32::BLACK);
            strip_ui.add_space(6.0);
            let (fmt_bg, fmt_fg) = format_colors(&t, plugin.format);
            widgets::tag(&mut strip_ui, plugin.format.label(), fmt_bg, fmt_fg);
        }

        let status = plugin_status(plugin);
        let inner = Rect::from_min_max(egui::pos2(rect.left() + CARD_PAD, strip.bottom() + 16.0), rect.max - vec2(CARD_PAD, CARD_PAD));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        let ui = &mut child;

        // Header: App icon with hover lift + shadow, plugin title, and author.
        ui.horizontal(|ui| {
            const ICON: f32 = 46.0;
            let (slot, _) = ui.allocate_exact_size(vec2(ICON, ICON), Sense::hover());
            let icon_rect = Rect::from_center_size(slot.center(), vec2(ICON, ICON)).translate(vec2(0.0, -4.0 * lift));
            ui.painter().add(
                egui::epaint::Shadow { offset: [0, 6], blur: 14, spread: 0, color: Color32::from_black_alpha(71) }
                    .as_shape(icon_rect.shrink(4.0), CornerRadius::same(10)),
            );
            if let Some(icon) = crate::icons::app(&plugin.app) {
                egui::Image::new(icon).paint_at(ui, icon_rect);
            }
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.add(egui::Label::new(FontId::new(18.5, theme::display()).into_rich_text(&plugin.name, t.ink_strong)).truncate());
                widgets::hud(ui, &plugin.author, t.faint);
            });
        });

        ui.add_space(10.0);

        // Multi-line summary box so the plugin description is never cut off on a single line.
        let translated_tagline = tr_ctx(current(), "plugin", &plugin.tagline);
        egui::Frame::new().fill(t.sunken).stroke(t.hairline()).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(44.0);
            ui.set_max_height(44.0);
            egui::ScrollArea::vertical().id_salt(("plugin_desc_scroll", &plugin.id)).auto_shrink([false, false]).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(translated_tagline).size(12.5).color(t.muted)).wrap());
            });
        });

        ui.add_space(10.0);

        // Status badge + version
        ui.horizontal(|ui| {
            let (label, tone) = match status {
                PluginStatus::UpToDate => (tl!("Installed · up to date"), Tone::Good),
                PluginStatus::NeedsRepair => (tl!("Needs repair"), Tone::Warn),
            };
            widgets::badge(ui, label, widgets::tone_color(&t, tone));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ver = format!("v{}", plugin.version);
                widgets::hud(ui, &ver, t.faint);
            });
        });

        // Bottom action bar: Repair button + More menu (Repair, Show in Explorer, Uninstall)
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            let width = inner.width();
            ui.horizontal(|ui| self.plugin_actions(ui, plugin, status, width));
        });
    }

    fn plugin_actions(&mut self, ui: &mut Ui, plugin: &InstalledPlugin, status: PluginStatus, width: f32) {
        ui.spacing_mut().item_spacing.x = 6.0;
        let menu_space = theme::CONTROL_H + 6.0;
        let main_w = (width - menu_space).max(76.0);

        match status {
            PluginStatus::NeedsRepair => {
                if widgets::button(ui, tl!("Repair"), Kind::Primary, main_w).clicked() {
                    self.send(Command::RepairPlugin { id: plugin.id.clone() });
                }
            }
            PluginStatus::UpToDate => {
                if widgets::button(ui, tl!("Repair"), Kind::Secondary, main_w).clicked() {
                    self.send(Command::RepairPlugin { id: plugin.id.clone() });
                }
            }
        }

        let reveal = self.caps.reveal_label;
        widgets::more_menu(ui, |ui| {
            if ui.button(tl!("Repair")).clicked() {
                self.send(Command::RepairPlugin { id: plugin.id.clone() });
                ui.close();
            }
            if ui.button(crate::i18n::t(reveal)).clicked() {
                self.send(Command::RevealPlugin { id: plugin.id.clone() });
                ui.close();
            }
            ui.separator();
            if ui.button(RichText::new(tl!("Uninstall")).color(Tokens::get(ui.ctx()).bad)).clicked() {
                self.send(Command::UninstallPlugin { id: plugin.id.clone() });
                ui.close();
            }
        });
    }
}

fn format_colors(t: &Tokens, format: PluginFormat) -> (Color32, Color32) {
    let bg = match format {
        PluginFormat::NativeWasm => tint(t.accent, if t.dark { 0.22 } else { 0.14 }),
        PluginFormat::PhotoshopFilter => tint(t.good, if t.dark { 0.22 } else { 0.14 }),
        PluginFormat::PresetPack => t.sunken,
    };
    let fg = match format {
        PluginFormat::NativeWasm => t.accent_ink,
        PluginFormat::PhotoshopFilter => t.good,
        PluginFormat::PresetPack => t.muted,
    };
    (bg, fg)
}

trait FontIdExt {
    fn into_rich_text(self, text: &str, color: Color32) -> RichText;
}

impl FontIdExt for FontId {
    fn into_rich_text(self, text: &str, color: Color32) -> RichText {
        RichText::new(text).font(self).color(color)
    }
}
