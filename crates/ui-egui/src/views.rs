//! The header, toolbar, footer, the grid of cards and the table, laid out like the apps page on
//! getartcraft.com: hairline cells, mono labels and square controls.

use artcraft_launcher_core::platform::pick_asset;
use artcraft_launcher_core::protocol::{Command, JobPhase};
use artcraft_launcher_core::status::AppStatus;
use artcraft_launcher_core::{AppGroup, SUITE, time};
use egui::{Color32, CornerRadius, Rect, RichText, Sense, Stroke, Ui, vec2};

use crate::app::{Dialog, LauncherUi, TitleBar};
use crate::model::{self, AppEntry};
use crate::settings::{Filter, SortKey, ViewMode};
use crate::theme::{self, Tokens, rgb, tint};
use crate::widgets::{self, Cell, Kind};

/// What the main button of an app does.
enum Primary {
    Install,
    Update(String),
    Open,
    Download(String),
    Unavailable(&'static str),
}

/// Height of the header bar (the site's nav).
pub(crate) const HEADER_H: f32 = 48.0;
/// Height of the footer bar.
pub(crate) const FOOTER_H: f32 = 34.0;

const CARD_MIN_W: f32 = 320.0;
const CARD_H: f32 = 280.0;
/// The card's top strip: number tab and links.
const STRIP_H: f32 = 40.0;
const CARD_PAD: f32 = 20.0;
/// Room macOS's traffic lights take at the left of the header.
const TRAFFIC_LIGHTS_W: f32 = 72.0;

/// A run of cards, under an optional (number, title) eyebrow.
type Section = (Option<(&'static str, &'static str)>, Vec<&'static str>);

impl LauncherUi {
    /// The header bar; with a custom or macOS title bar it is also the window's title bar.
    pub(crate) fn header(&mut self, ui: &mut Ui, bar: TitleBar) {
        let t = Tokens::get(ui.ctx());
        let h = HEADER_H;
        if bar != TitleBar::Native {
            widgets::title_bar_drag(ui);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if bar == TitleBar::MacOs {
                ui.add_space(TRAFFIC_LIGHTS_W);
            }
            // The ArtCraft wordmark exactly as supplied, in its own cell.
            let logo = vec2(14.0 * crate::icons::ARTCRAFT_LOGO_ASPECT, 14.0);
            let (cell, _) = ui.allocate_exact_size(vec2(logo.x + 40.0, h), Sense::hover());
            egui::Image::new(crate::icons::artcraft_logo(t.dark)).alt_text("ArtCraft").paint_at(ui, Rect::from_center_size(cell.center(), logo));
            widgets::rule(ui, h);
            ui.add_space(16.0);
            widgets::tag(ui, "Launcher", t.invert_bg, t.invert_fg);
            ui.add_space(12.0);
            widgets::hud(ui, "Craft App Suite", t.muted);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if bar == TitleBar::Custom {
                    widgets::window_buttons(ui, h);
                }
                if widgets::nav_icon(ui, "⚙", "Settings", h, false).clicked() {
                    self.dialog = Some(Dialog::Settings);
                }
                if widgets::nav_icon(ui, "⟳", "Check for updates", h, self.checking).clicked() {
                    self.check_now();
                }
                ui.add_space(6.0);
                if widgets::nav_link(ui, "Discord", "Join the ArtCraft community on Discord", h, false).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(artcraft_launcher_core::catalog::DISCORD_URL));
                }
                ui.add_space(6.0);
                widgets::rule(ui, h);
                ui.add_space(6.0);
                // Right to left: Table, then Grid, so they read Grid · Table.
                for (mode, label, tip) in [(ViewMode::Table, "Table", "Show apps as a table"), (ViewMode::Grid, "Grid", "Show apps as cards")] {
                    if widgets::nav_link(ui, label, tip, h, self.settings.view == mode).clicked() {
                        self.settings.view = mode;
                    }
                }
                ui.add_space(6.0);
                widgets::rule(ui, h);
            });
        });
    }

    pub(crate) fn toolbar(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        if let Some(r) = self.launcher_update.clone() {
            egui::Frame::new()
                .fill(tint(t.accent, if t.dark { 0.10 } else { 0.07 }))
                .stroke(Stroke::new(1.0, tint(t.accent, 0.6)))
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        widgets::tag(ui, "Update", t.accent, Color32::WHITE);
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!("ArtCraft Launcher {} is available.", r.version_label()))
                                .family(theme::medium())
                                .color(t.ink_strong),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(phase) = self.launcher_job {
                                let (fraction, caption) = match phase {
                                    JobPhase::Downloading { done, total } if total > 0 => (
                                        Some(done as f32 / total as f32),
                                        format!("Downloading {} of {}", widgets::human_bytes(done), widgets::human_bytes(total)),
                                    ),
                                    JobPhase::Downloading { .. } | JobPhase::Reconnecting { .. } => (None, "Downloading…".to_string()),
                                    JobPhase::Verifying => (None, "Verifying…".to_string()),
                                    JobPhase::Installing | JobPhase::Removing => (None, "Installing, then restarting…".to_string()),
                                };
                                widgets::progress(ui, fraction, &caption, t.accent, 220.0);
                                return;
                            }
                            if widgets::glyph_button(ui, widgets::Glyph::Close, "Dismiss").clicked() {
                                self.launcher_update = None;
                            }
                            if self.caps.self_update {
                                if widgets::button(ui, "Update and restart", Kind::Primary, 0.0)
                                    .on_hover_text("Downloads the new version, replaces this one and opens it again")
                                    .clicked()
                                {
                                    self.send(Command::UpdateLauncher);
                                }
                            } else {
                                let url = self.platform.and_then(|p| launcher_download(&r, p)).unwrap_or_else(|| r.html_url.clone());
                                if widgets::button(ui, "Download", Kind::Primary, 0.0).clicked() {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                                }
                            }
                            ui.add_space(8.0);
                            widgets::hud_link(ui, "What's new", &r.html_url);
                        });
                    });
                });
            ui.add_space(14.0);
        }
        ui.horizontal(|ui| {
            let updates = self.count(Filter::Updates);
            let filters: Vec<(Filter, Cell<'_>)> = [
                (Filter::All, "All", "Every Craft app", None),
                (Filter::Installed, "Installed", "Apps on this computer", None),
                (Filter::Updates, "Updates", "Installed apps with a newer version", Some(t.warn)),
                (Filter::NotInstalled, "Not installed", "Apps you can install", None),
            ]
            .into_iter()
            .filter(|(f, ..)| self.caps.manage_apps || *f == Filter::All)
            .map(|(f, label, tip, highlight)| (f, Cell { label, tip, count: Some((self.count(f), highlight)) }))
            .collect();
            widgets::cells(ui, &mut self.settings.filter, &filters);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.caps.manage_apps && updates > 0 {
                    let label = if updates == 1 { "Update 1 app".to_string() } else { format!("Update all ({updates})") };
                    if widgets::button(ui, &label, Kind::Primary, 0.0).clicked() {
                        let ids: Vec<&str> = self
                            .entries
                            .iter()
                            .filter(|e| e.status(self.platform, true) == AppStatus::UpdateAvailable && e.job.is_none())
                            .map(|e| e.app.id)
                            .collect();
                        for id in ids {
                            self.send(Command::Install { app: id.to_string() });
                        }
                    }
                    ui.add_space(4.0);
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search apps")
                        .desired_width(200.0)
                        .min_size(vec2(0.0, theme::CONTROL_H))
                        .vertical_align(egui::Align::Center)
                        .margin(vec2(10.0, 0.0)),
                );
                if self.settings.view == ViewMode::Grid {
                    egui::ComboBox::from_id_salt("sort")
                        .selected_text(RichText::new(sort_label(self.settings.sort)).size(13.0))
                        .width(140.0)
                        .show_ui(ui, |ui| {
                            for key in [
                                SortKey::Suite,
                                SortKey::Name,
                                SortKey::InspiredBy,
                                SortKey::Status,
                                SortKey::Installed,
                                SortKey::Latest,
                                SortKey::Released,
                            ] {
                                if ui.selectable_label(self.settings.sort == key, sort_label(key)).clicked() {
                                    self.settings.sort = key;
                                    self.settings.sort_descending = key.starts_descending();
                                }
                            }
                        });
                    widgets::hud(ui, "Sort", t.faint);
                }
            });
        });
    }

    pub(crate) fn footer(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let now = self.backend.now();
        ui.horizontal_centered(|ui| {
            if self.checking {
                widgets::hud(ui, "Checking for updates…", t.faint);
            } else if let Some(at) = self.settings.last_check {
                let via = self.last_source.map(|s| format!(" · {}", s.label())).unwrap_or_default();
                widgets::hud(ui, &format!("Checked {}{via}", time::format_ago(at, now)), t.faint);
            } else {
                widgets::hud(ui, "Not checked yet", t.faint);
            }
            if let Some(until) = self.rate_limited_until.filter(|u| *u > now) {
                ui.label(
                    RichText::new(format!("· GitHub's hourly limit was reached; full details return in {} min", ((until - now) / 60).max(1)))
                        .size(12.0)
                        .color(t.warn),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = match self.platform {
                    Some(p) if self.caps.manage_apps => format!("Installing for {}", p.label()),
                    Some(p) => format!("Downloads for {}", p.label()),
                    None => "No downloads for this system".into(),
                };
                widgets::hud(ui, &label, t.faint);
                ui.add_space(16.0);
                // Right to left, so this reads: Website · GitHub · Discord, then the social icons.
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    widgets::social_icons(ui, 13.0);
                });
                ui.add_space(12.0);
                ui.spacing_mut().item_spacing.x = 16.0;
                use artcraft_launcher_core::catalog::{DISCORD_URL, GITHUB_URL, WEBSITE_URL};
                widgets::hud_link(ui, "Discord", DISCORD_URL);
                widgets::hud_link(ui, "GitHub", GITHUB_URL);
                widgets::hud_link(ui, "Website", WEBSITE_URL);
            });
        });
    }

    // ---- grid ----------------------------------------------------------------------------

    pub(crate) fn grid(&mut self, ui: &mut Ui, ids: &[&'static str]) {
        let width = ui.available_width();
        // Neighbouring cards share their 1 px hairline, like the site's `gap-px` grid.
        let cols = ((width / CARD_MIN_W).floor() as usize).max(1);
        let card_w = (width + (cols as f32 - 1.0)) / cols as f32;
        // In suite order, split into the classic apps and the new ones.
        let grouped = self.settings.sort == SortKey::Suite && self.settings.filter == Filter::All && self.search.trim().is_empty();
        let sections: Vec<Section> = if grouped {
            [(AppGroup::Classic, "01", "Classic apps"), (AppGroup::New, "02", "New apps")]
                .iter()
                .map(|(g, index, title)| {
                    let list = ids.iter().copied().filter(|id| self.entry(id).is_some_and(|e| e.app.group == *g)).collect();
                    (Some((*index, *title)), list)
                })
                .collect()
        } else {
            vec![(None, ids.to_vec())]
        };
        let t = Tokens::get(ui.ctx());
        for (title, list) in sections {
            if list.is_empty() {
                continue;
            }
            if let Some((index, title)) = title {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    widgets::eyebrow(ui, index, title);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        widgets::hud(ui, &format!("{} apps", list.len()), t.faint);
                    });
                });
                ui.add_space(10.0);
            }
            let rows = list.len().div_ceil(cols);
            let height = rows as f32 * (CARD_H - 1.0) + 1.0;
            let (area, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
            for (i, id) in list.iter().enumerate() {
                let (row, col) = (i / cols, i % cols);
                let min = area.min + vec2(col as f32 * (card_w - 1.0), row as f32 * (CARD_H - 1.0));
                self.card(ui, id, Rect::from_min_size(min, vec2(card_w, CARD_H)));
            }
            ui.add_space(24.0);
        }
    }

    fn card(&mut self, ui: &mut Ui, id: &str, rect: Rect) {
        let Some(entry) = self.entry(id).cloned() else { return };
        let t = Tokens::get(ui.ctx());
        let app = entry.app;
        let hovered = ui.rect_contains_pointer(rect);
        let lift = ui.ctx().animate_bool_with_time(ui.id().with(("card-hover", id)), hovered, 0.3);
        let app_color = rgb(app.color);
        let painter = ui.painter_at(rect.expand(16.0));
        painter.rect(rect, CornerRadius::ZERO, t.bg, t.hairline(), egui::StrokeKind::Inside);
        // On hover, a 2 px rule in the app's colour wipes in from the left along the top.
        if lift > 0.0 {
            painter.rect_filled(Rect::from_min_size(rect.min, vec2(rect.width() * lift, 2.0)), CornerRadius::ZERO, app_color);
        }

        // The top strip: the app's number in its colour, and its pages.
        let strip = Rect::from_min_size(rect.min, vec2(rect.width(), STRIP_H));
        painter.hline(strip.x_range(), strip.bottom() - 0.5, t.hairline());
        let number = SUITE.iter().position(|a| a.id == app.id).map_or_else(String::new, |i| format!("{:02}", i + 1));
        {
            let mut strip_ui = ui.new_child(
                egui::UiBuilder::new().max_rect(strip.shrink2(vec2(CARD_PAD, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            widgets::tag(&mut strip_ui, &number, app_color, Color32::BLACK);
            strip_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                widgets::hud_link(ui, "GitHub", &app.repo_url());
                if let Some(site) = app.website() {
                    widgets::hud_link(ui, "Website", &site);
                }
            });
        }

        let status = entry.status(self.platform, self.caps.manage_apps);
        let inner = Rect::from_min_max(egui::pos2(rect.left() + CARD_PAD, strip.bottom() + 18.0), rect.max - vec2(CARD_PAD, CARD_PAD));
        // A detached child: the card's contents never move the next card along.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        let ui = &mut child;
        ui.horizontal(|ui| {
            const ICON: f32 = 56.0;
            let (slot, _) = ui.allocate_exact_size(vec2(ICON, ICON), Sense::hover());
            // The icon rises a little on hover, over a soft drop shadow (the site's only shadow).
            let icon_rect = slot.translate(vec2(0.0, -4.0 * lift));
            ui.painter().add(
                egui::epaint::Shadow { offset: [0, 6], blur: 14, spread: 0, color: Color32::from_black_alpha(71) }
                    .as_shape(icon_rect.shrink(4.0), CornerRadius::same(12)),
            );
            if let Some(icon) = crate::icons::app(app.id) {
                egui::Image::new(icon).paint_at(ui, icon_rect);
            }
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    widgets::wordmark(ui, app, 22.0);
                    if app.group == AppGroup::New {
                        widgets::badge(ui, "New", t.app_ink(app));
                    }
                });
                ui.add(egui::Label::new(RichText::new(app.tagline).size(13.5).color(t.muted)).truncate());
                ui.add(egui::Label::new(RichText::new(format!("Like {}", app.inspired_by())).size(12.0).color(t.faint)).truncate());
            });
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            let (text, tone) = model::entry_status_text(&entry, &status, self.caps.manage_apps);
            let r = widgets::badge(ui, text, widgets::tone_color(&t, tone));
            if let Some(f) = entry.failure() {
                r.on_hover_text(format!("Couldn't check for updates: {f}"));
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new(version_line(&entry, &status)).size(12.5).color(t.muted));
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.horizontal(|ui| self.actions(ui, &entry, &status, inner.width()));
        });
    }

    // ---- table ---------------------------------------------------------------------------

    pub(crate) fn table(&mut self, ui: &mut Ui, ids: &[&'static str]) {
        use egui_extras::{Column, TableBuilder};
        let t = Tokens::get(ui.ctx());
        let now = self.backend.now();
        let mut sort_clicked = None;
        // The app column's name and tagline line heights, to centre them on the icon.
        const TITLE_SIZE: f32 = 16.0;
        const TAGLINE_SIZE: f32 = 12.0;
        let (title_h, tagline_h) = ui
            .ctx()
            .fonts_mut(|f| (f.row_height(&egui::FontId::new(TITLE_SIZE, theme::display())), f.row_height(&egui::FontId::proportional(TAGLINE_SIZE))));
        // Rows are divided by hairlines across the whole table, drawn from the first cell and
        // clipped to the scroll area.
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
                .column(Column::exact(172.0).clip(true))
                .column(Column::exact(176.0))
                .column(Column::exact(84.0))
                .column(Column::exact(84.0))
                .column(Column::exact(104.0))
                .column(Column::exact(206.0))
                .header(42.0, |mut header| {
                    for (key, label) in [
                        (Some(SortKey::Name), "App"),
                        (Some(SortKey::InspiredBy), "Inspired by"),
                        (Some(SortKey::Status), "Status"),
                        (Some(SortKey::Installed), "Installed"),
                        (Some(SortKey::Latest), "Latest"),
                        (Some(SortKey::Released), "Released"),
                        (None, ""),
                    ] {
                        header.col(|ui| {
                            if key == Some(SortKey::Name) {
                                rule(ui);
                            }
                            let Some(k) = key else { return };
                            let active = k == self.settings.sort;
                            let tip = if active { "Click to reverse the order" } else { "Click to sort by this column" };
                            // Measure first so the whole header cell (label + arrow) is the target.
                            let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
                            let hovered = resp.hovered();
                            let color = if active || hovered { t.ink_strong } else { t.faint };
                            let galley = widgets::hud_galley(ui, label, color);
                            let text_pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
                            let arrow_x = text_pos.x + galley.size().x + 4.0;
                            let p = ui.painter();
                            p.galley(text_pos, galley, color);
                            // ▲ / ▼ for the active column; a faint hint on hover.
                            let arrow = if active {
                                Some(self.settings.sort_descending)
                            } else if hovered {
                                Some(k.starts_descending())
                            } else {
                                None
                            };
                            if let Some(down) = arrow {
                                let c = egui::pos2(arrow_x + 4.0, rect.center().y);
                                let (tip_y, base_y) = if down { (c.y + 3.0, c.y - 2.0) } else { (c.y - 3.0, c.y + 2.0) };
                                let pts = vec![egui::pos2(c.x - 3.5, base_y), egui::pos2(c.x + 3.5, base_y), egui::pos2(c.x, tip_y)];
                                let arrow_color = if active { t.accent_ink } else { t.faint };
                                p.add(egui::Shape::convex_polygon(pts, arrow_color, Stroke::NONE));
                            }
                            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
                            if resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                sort_clicked = Some(k);
                            }
                        });
                    }
                })
                .body(|mut body| {
                    let last = ids.len().saturating_sub(1);
                    for (i, id) in ids.iter().enumerate() {
                        let Some(entry) = self.entry(id).cloned() else { continue };
                        let status = entry.status(self.platform, self.caps.manage_apps);
                        body.row(62.0, |mut row| {
                            row.col(|ui| {
                                if i < last {
                                    rule(ui);
                                }
                                if let Some(icon) = crate::icons::app(entry.app.id) {
                                    ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(38.0, 38.0)));
                                }
                                ui.add_space(4.0);
                                ui.vertical(|ui| {
                                    const GAP: f32 = 2.0;
                                    ui.spacing_mut().item_spacing.y = GAP;
                                    // Name and tagline as one block, centred on the icon.
                                    let block = title_h + GAP + tagline_h;
                                    ui.add_space(((ui.available_height() - block) / 2.0).max(0.0));
                                    widgets::wordmark(ui, entry.app, TITLE_SIZE);
                                    ui.add(egui::Label::new(RichText::new(entry.app.tagline).size(TAGLINE_SIZE).color(t.muted)).truncate());
                                });
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(RichText::new(entry.app.inspired_by()).size(13.0).color(t.muted)).truncate());
                            });
                            row.col(|ui| {
                                let (text, tone) = model::entry_status_text(&entry, &status, self.caps.manage_apps);
                                let r = widgets::badge(ui, text, widgets::tone_color(&t, tone));
                                if let Some(f) = entry.failure() {
                                    r.on_hover_text(format!("Couldn't check for updates: {f}"));
                                }
                            });
                            row.col(|ui| {
                                let installed = entry.installed_label();
                                let color = if installed.is_some() { t.ink } else { t.faint };
                                ui.label(RichText::new(installed.unwrap_or_else(|| "—".into())).family(egui::FontFamily::Monospace).color(color));
                            });
                            row.col(|ui| {
                                let c = if status == AppStatus::UpdateAvailable { t.warn } else { t.ink };
                                ui.label(
                                    RichText::new(entry.latest_label().unwrap_or_else(|| "—".into())).family(egui::FontFamily::Monospace).color(c),
                                );
                            });
                            row.col(|ui| {
                                let when = entry.release().and_then(|r| r.published_at);
                                let r = ui.label(RichText::new(when.map_or_else(|| "—".into(), time::format_date)).color(t.muted));
                                if let Some(w) = when {
                                    r.on_hover_text(time::format_ago(w, now));
                                }
                            });
                            row.col(|ui| self.actions(ui, &entry, &status, 204.0));
                        });
                    }
                });
        });
        if let Some(k) = sort_clicked {
            if self.settings.sort == k {
                self.settings.sort_descending = !self.settings.sort_descending;
            } else {
                self.settings.sort = k;
                self.settings.sort_descending = k.starts_descending();
            }
        }
    }

    /// The trademark notice under the grid and the table.
    pub(crate) fn trademark_notice(&self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        ui.add_space(8.0);
        ui.add(egui::Label::new(RichText::new(artcraft_launcher_core::catalog::TRADEMARK_NOTICE).size(11.0).color(t.faint)).wrap());
    }

    // ---- actions -------------------------------------------------------------------------

    fn primary_action(&self, entry: &AppEntry, status: &AppStatus) -> Primary {
        if !self.caps.manage_apps {
            let platform = self.platform;
            let url = entry.release().zip(platform).and_then(|(r, p)| {
                let stems: Vec<&str> = entry.app.stems().collect();
                pick_asset(r, &stems, p, p.download_kinds()).map(|(a, _)| a.url.clone())
            });
            return match (status, url) {
                (AppStatus::NoRelease, _) => Primary::Unavailable("Coming soon"),
                (AppStatus::Checking, _) => Primary::Unavailable("Checking…"),
                (_, Some(url)) => Primary::Download(url),
                (_, None) => Primary::Unavailable("Not available"),
            };
        }
        match status {
            AppStatus::Available => Primary::Install,
            AppStatus::UpdateAvailable => Primary::Update(entry.latest_label().unwrap_or_default()),
            AppStatus::UpToDate | AppStatus::Installed => Primary::Open,
            AppStatus::Checking => Primary::Unavailable("Checking…"),
            AppStatus::NoRelease => Primary::Unavailable("Coming soon"),
            AppStatus::Unavailable => Primary::Unavailable(if entry.failure().is_some() { "Offline" } else { "Not available" }),
        }
    }

    /// The main button (or progress) and the … menu, within `width`.
    fn actions(&mut self, ui: &mut Ui, entry: &AppEntry, status: &AppStatus, width: f32) {
        let t = Tokens::get(ui.ctx());
        let id = entry.app.id.to_string();
        // One square button (the menu) beside the main one.
        let gap = ui.spacing().item_spacing.x;
        let main_w = (width - theme::CONTROL_H - gap).max(120.0);
        if let Some(phase) = entry.job {
            // Two square buttons (pause, cancel) beside the bar while downloading.
            let bar_w = (width - 2.0 * (theme::CONTROL_H + gap) - 4.0).max(100.0);
            let short = bar_w < 200.0;
            let amount = |done: u64, total: u64| {
                if total > 0 { format!("{} of {}", widgets::human_bytes(done), widgets::human_bytes(total)) } else { widgets::human_bytes(done) }
            };
            let (fraction, caption, color) = match phase {
                JobPhase::Downloading { done, total } => {
                    let f = (total > 0).then(|| done as f32 / total as f32);
                    let pct = f.map(|f| format!(" · {:.0}%", f * 100.0)).unwrap_or_default();
                    let caption = if bar_w < 150.0 {
                        match f {
                            Some(f) => format!("{:.0}% of {}", f * 100.0, widgets::human_bytes(total)),
                            None => widgets::human_bytes(done),
                        }
                    } else if short {
                        format!("{}{pct}", amount(done, total))
                    } else {
                        format!("Downloading {}{pct}", amount(done, total))
                    };
                    (f, caption, rgb(entry.app.color))
                }
                JobPhase::Reconnecting { done, total } => {
                    let f = (total > 0).then(|| done as f32 / total as f32);
                    (f, if short { "Reconnecting…".into() } else { format!("Reconnecting… {}", amount(done, total)) }, t.warn)
                }
                JobPhase::Verifying => (None, "Verifying checksum…".into(), rgb(entry.app.color)),
                JobPhase::Installing => (None, "Installing…".into(), rgb(entry.app.color)),
                JobPhase::Removing => (None, "Removing…".into(), rgb(entry.app.color)),
            };
            let downloading = matches!(phase, JobPhase::Downloading { .. } | JobPhase::Reconnecting { .. });
            widgets::progress(ui, fraction, &caption, color, if downloading { bar_w } else { main_w });
            if downloading {
                if widgets::glyph_button(ui, widgets::Glyph::Pause, "Pause download").clicked() {
                    self.send(Command::Pause { app: id.clone() });
                }
                if widgets::glyph_button(ui, widgets::Glyph::Close, "Cancel download").clicked() {
                    self.send(Command::Cancel { app: id });
                }
            }
            return;
        }
        if let Some(p) = entry.paused
            && self.caps.manage_apps
        {
            let pct = if p.total > 0 { format!(" · {:.0}%", p.done as f32 / p.total as f32 * 100.0) } else { String::new() };
            // Resume, then discard and the menu (two squares).
            let resume_w = (width - 2.0 * (theme::CONTROL_H + gap)).max(90.0);
            // Narrow (the table): just "Resume"; the tooltip says how far it got.
            let label = match resume_w {
                w if w < 150.0 => "Resume".to_string(),
                w if w < 220.0 => format!("Resume{pct}"),
                _ => format!("Resume download{pct}"),
            };
            let resp = widgets::button(ui, &label, Kind::Primary, resume_w);
            let tip = if p.total > 0 {
                format!("{} of {} downloaded", widgets::human_bytes(p.done), widgets::human_bytes(p.total))
            } else {
                format!("{} downloaded", widgets::human_bytes(p.done))
            };
            if resp.on_hover_text(tip).clicked() {
                self.send(Command::Install { app: id.clone() });
            }
            if widgets::glyph_button(ui, widgets::Glyph::Close, "Discard download").clicked() {
                self.send(Command::Cancel { app: id.clone() });
            }
            self.menu(ui, entry, status);
            return;
        }
        match self.primary_action(entry, status) {
            Primary::Install => {
                if widgets::button(ui, "Install", Kind::Primary, main_w).clicked() {
                    self.send(Command::Install { app: id.clone() });
                }
            }
            Primary::Update(v) => {
                if widgets::button(ui, &format!("Update to {v}"), Kind::Primary, main_w).clicked() {
                    self.send(Command::Install { app: id.clone() });
                }
            }
            Primary::Open => {
                if widgets::button(ui, "Open", Kind::Secondary, main_w).clicked() {
                    self.send(Command::Launch { app: id.clone() });
                }
            }
            Primary::Download(url) => {
                let label = match self.platform {
                    Some(p) => format!("Download for {}", p.os.label()),
                    None => "Download".into(),
                };
                if widgets::button(ui, &label, Kind::Primary, main_w).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            }
            Primary::Unavailable(label) => {
                ui.add_enabled_ui(false, |ui| widgets::button(ui, label, Kind::Secondary, main_w));
            }
        }
        self.menu(ui, entry, status);
    }

    fn menu(&mut self, ui: &mut Ui, entry: &AppEntry, status: &AppStatus) {
        let id = entry.app.id.to_string();
        let manage = self.caps.manage_apps;
        let reveal = self.caps.reveal_label;
        widgets::more_menu(ui, |ui| {
            if entry.release().is_some() && ui.button("Release notes").clicked() {
                self.dialog = Some(Dialog::Notes(id.clone()));
                ui.close();
            }
            if manage && status.is_installed() {
                if ui.button("Open").clicked() {
                    self.send(Command::Launch { app: id.clone() });
                    ui.close();
                }
                if ui.button(reveal).clicked() {
                    self.send(Command::Reveal { app: id.clone() });
                    ui.close();
                }
                if *status == AppStatus::UpToDate && ui.button("Reinstall").clicked() {
                    self.send(Command::Install { app: id.clone() });
                    ui.close();
                }
                ui.separator();
                if ui.button(RichText::new("Uninstall…").color(Tokens::get(ui.ctx()).bad)).clicked() {
                    self.dialog = Some(Dialog::ConfirmUninstall(id.clone()));
                    ui.close();
                }
            }
            if !manage && let (Some(r), Some(p)) = (entry.release(), self.platform) {
                let stems: Vec<&str> = entry.app.stems().collect();
                let options: Vec<(String, String)> = p
                    .download_kinds()
                    .iter()
                    .filter_map(|k| {
                        pick_asset(r, &stems, p, &[*k]).map(|(a, _)| (format!("{} · {}", k.label(), widgets::human_bytes(a.size)), a.url.clone()))
                    })
                    .collect();
                if !options.is_empty() {
                    ui.separator();
                    for (label, url) in options {
                        if ui.button(label).clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                            ui.close();
                        }
                    }
                }
            }
            ui.separator();
            let link = |ui: &mut Ui, label: &str, url: String| {
                if ui.button(label).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    ui.close();
                }
            };
            match entry.app.website() {
                Some(site) => link(ui, "Website", site),
                None => {
                    ui.add_enabled(false, egui::Button::new("Website (coming soon)"));
                }
            }
            link(ui, "GitHub repository", entry.app.repo_url());
            link(ui, "All releases", entry.app.releases_url());
        });
    }
}

fn sort_label(k: SortKey) -> &'static str {
    match k {
        SortKey::Suite => "Suite order",
        SortKey::Name => "Name",
        SortKey::InspiredBy => "Inspired by",
        SortKey::Status => "Status",
        SortKey::Installed => "Installed version",
        SortKey::Latest => "Latest version",
        SortKey::Released => "Newest release",
    }
}

/// "0.4.1 installed → 0.5.0 released · Oct 8, 2026" and friends.
fn version_line(e: &AppEntry, status: &AppStatus) -> String {
    let released = e.release().and_then(|r| r.published_at).map(|at| format!(" · {}", time::format_date(at))).unwrap_or_default();
    match (status, e.installed_label(), e.latest_label()) {
        (AppStatus::UpdateAvailable, Some(i), Some(l)) => format!("{i} installed → {l} released{released}"),
        (AppStatus::UpToDate, Some(i), _) => format!("Version {i}{released}"),
        (_, Some(i), Some(l)) => format!("{i} installed · latest {l}"),
        (_, Some(i), None) => format!("Version {i} installed"),
        (_, None, Some(l)) => format!("Version {l}{released}"),
        (AppStatus::NoRelease, None, None) => "No release published yet".into(),
        _ => " ".into(),
    }
}

/// The launcher's own download for this platform.
fn launcher_download(r: &artcraft_launcher_core::Release, p: artcraft_launcher_core::Platform) -> Option<String> {
    use artcraft_launcher_core::PackageKind;
    let kinds: &[PackageKind] = match p.os {
        artcraft_launcher_core::Os::Linux => &[PackageKind::AppImage, PackageKind::Deb],
        _ => p.default_kinds(),
    };
    pick_asset(r, &[artcraft_launcher_core::catalog::LAUNCHER_ID], p, kinds).map(|(a, _)| a.url.clone())
}
