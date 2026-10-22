//! The header, toolbar, footer, the grid of cards and the table, laid out like the apps page on
//! getartcraft.com: hairline cells, mono labels and square controls.

use artcraft_launcher_core::details::AppDetails;
use artcraft_launcher_core::platform::pick_asset;
use artcraft_launcher_core::protocol::{Command, JobPhase};
use artcraft_launcher_core::status::AppStatus;
use artcraft_launcher_core::time;
use egui::{Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, Ui, vec2};

use crate::app::{Dialog, LauncherUi, TitleBar};
use crate::i18n::{current, fmt, t, tn, tr_ctx};
use crate::model::{self, AppEntry};
use crate::settings::{Filter, SortKey, ViewMode};
use crate::theme::{self, Tokens, rgb, tint};
use crate::widgets::{self, Kind};

/// Translate a relative timestamp ("just now", "5 min ago", …) into the current language.
fn ago_text(secs: i64, now: i64) -> String {
    match time::ago_kind(secs, now) {
        time::TimeAgo::JustNow => t("just now").to_string(),
        time::TimeAgo::Minutes(n) => tn(n, "{n} min ago", "{n} min ago"),
        time::TimeAgo::Hours(n) => tn(n, "{n} h ago", "{n} h ago"),
        time::TimeAgo::Yesterday => t("yesterday").to_string(),
        time::TimeAgo::Days(n) => tn(n, "{n} day ago", "{n} days ago"),
        time::TimeAgo::Date(secs) => date_text(secs),
    }
}

/// Translate an absolute date ("Oct 8, 2026" → "8 окт. 2026") into the current language, on this
/// computer's calendar (`local_time`). Every date the launcher shows is written by this.
pub(crate) fn date_text(secs: i64) -> String {
    let (y, m, d) = crate::local_time::civil(secs);
    let month = tr_ctx(current(), "month", time::month_short_en(m));
    fmt(tl!("{month} {day}, {year}"), &[("month", month), ("day", &d.to_string()), ("year", &y.to_string())])
}

/// What the main button of an app does.
enum Primary {
    Install,
    /// The download is already here; only the install step is left. Carries the button's label.
    Retry(String),
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
const CARD_H: f32 = 314.0;
/// The card's top strip: number tab and links.
const STRIP_H: f32 = 40.0;
const CARD_PAD: f32 = 20.0;
/// Room macOS's traffic lights take at the left of the header.
const TRAFFIC_LIGHTS_W: f32 = 72.0;

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
            egui::Image::new(crate::icons::artcraft_logo(t.dark)).alt_text(tl!("ArtCraft")).paint_at(ui, Rect::from_center_size(cell.center(), logo));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if bar == TitleBar::Custom {
                    widgets::window_buttons(ui, h);
                }
                if widgets::nav_icon(ui, "⚙", tl!("Settings"), h, false).clicked() {
                    self.dialog = Some(Dialog::Settings);
                }
                // The controls reserve their actual widths before the secondary text gets space.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if ui.available_width() >= 17.0 + widgets::hud_galley(ui, tl!("Craft App Suite"), t.muted).size().x {
                        widgets::rule(ui, h);
                        ui.add_space(16.0);
                        widgets::hud(ui, tl!("Craft App Suite"), t.muted);
                    }
                });
            });
        });
    }

    pub(crate) fn toolbar(&mut self, ui: &mut Ui) {
        if self.settings.filter == Filter::Hidden && self.settings.hidden.is_empty() {
            self.settings.filter = Filter::All;
        }
        let t = Tokens::get(ui.ctx());
        if self.dialog.is_none()
            && let Some(r) = self.launcher_update.clone()
        {
            egui::Frame::new()
                .fill(tint(t.accent, if t.dark { 0.10 } else { 0.07 }))
                .stroke(Stroke::new(1.0, tint(t.accent, 0.6)))
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        widgets::tag(ui, tl!("Update"), t.accent, Color32::WHITE);
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(fmt(tl!("ArtCraft Launcher {version} is available."), &[("version", &r.version_label())]))
                                .family(theme::medium())
                                .color(t.ink_strong),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(phase) = self.launcher_job {
                                let (fraction, caption) = match phase {
                                    JobPhase::Downloading { done, total } if total > 0 => (
                                        Some(done as f32 / total as f32),
                                        fmt(
                                            tl!("Downloading {done} of {total}"),
                                            &[("done", &widgets::human_bytes(done)), ("total", &widgets::human_bytes(total))],
                                        ),
                                    ),
                                    JobPhase::Downloading { .. } | JobPhase::Reconnecting { .. } => (None, tl!("Downloading…").to_string()),
                                    JobPhase::Verifying => (None, tl!("Verifying…").to_string()),
                                    JobPhase::Waiting => (None, tl!("Waiting for other installs…").to_string()),
                                    JobPhase::Installing | JobPhase::Removing => (None, tl!("Installing, then restarting…").to_string()),
                                };
                                // Another app's install can hang on a password prompt: the wait can be called off.
                                if phase == JobPhase::Waiting
                                    && widgets::glyph_button(ui, widgets::Glyph::Close, tl!("Cancel update"))
                                        .on_hover_text(tl!("Stop waiting; the update can be started again later"))
                                        .clicked()
                                {
                                    self.send(Command::Cancel { app: artcraft_launcher_core::catalog::LAUNCHER_ID.to_string() });
                                }
                                widgets::progress(ui, fraction, &caption, t.accent, 220.0);
                                return;
                            }
                            if widgets::glyph_button(ui, widgets::Glyph::Close, tl!("Dismiss")).clicked() {
                                self.launcher_update = None;
                            }
                            if self.caps.self_update {
                                if widgets::button(ui, tl!("Update and restart"), Kind::Primary, 0.0)
                                    .on_hover_text(tl!("Downloads the new version, replaces this one and opens it again"))
                                    .clicked()
                                {
                                    self.send(Command::UpdateLauncher);
                                }
                            } else {
                                let url = self
                                    .platform
                                    .and_then(|p| launcher_download(&r, p, crate::settings::is_flatpak_sandbox()))
                                    .unwrap_or_else(|| r.html_url.clone());
                                if widgets::button(ui, tl!("Download"), Kind::Primary, 0.0).clicked() {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                                }
                            }
                            ui.add_space(8.0);
                            widgets::hud_link(ui, tl!("What's new"), &r.html_url);
                        });
                    });
                });
            ui.add_space(14.0);
        }
        self.announcements(ui);
        let updates = self.count(Filter::Updates);
        let mut compact = false;
        ui.horizontal(|ui| {
            self.refresh_button(ui);
            // Hiding an app is only undone from here, so the way back to the hidden ones stays
            // while there are any: a toggle, lit while the hidden apps are what the page shows.
            if !self.settings.hidden.is_empty() {
                let showing = self.settings.filter == Filter::Hidden;
                let label = format!("{} {}", tl!("Hidden Apps"), self.settings.hidden.len());
                if widgets::button(ui, &label, if showing { Kind::Primary } else { Kind::Secondary }, 0.0)
                    .on_hover_text(tl!("Apps you hid"))
                    .clicked()
                {
                    self.settings.filter = if showing { Filter::All } else { Filter::Hidden };
                }
            }
            compact = ui.available_width() < theme::TOOLBAR_COMPACT_BELOW;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Right to left: the view switch at the far right, then the search.
                crate::bar::view_toggle(ui, &mut self.settings.view, [tl!("Show apps as cards"), tl!("Show apps as a table")]);
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text(tl!("Search apps"))
                        .desired_width(200.0)
                        .min_size(vec2(0.0, theme::CONTROL_H))
                        .vertical_align(egui::Align::Center)
                        .margin(vec2(10.0, 0.0)),
                );
                if self.settings.view == ViewMode::Grid && !compact {
                    self.sort_picker(ui);
                    // The caption only where it fits beside the filters (longer in some languages).
                    let sort = tl!("Sort");
                    if widgets::hud_galley(ui, sort, t.faint).size().x + ui.spacing().item_spacing.x <= ui.available_width() {
                        widgets::hud(ui, sort, t.faint);
                    }
                }
                if !compact && self.caps.manage_apps && updates > 0 {
                    self.update_all_button(ui, updates);
                }
            });
        });
        if compact {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if self.settings.view == ViewMode::Grid {
                    // "Sort" before the picker, as in the full-width row.
                    widgets::hud(ui, tl!("Sort"), t.faint);
                    self.sort_picker(ui);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.caps.manage_apps && updates > 0 {
                        self.update_all_button(ui, updates);
                    }
                });
            });
        }
    }

    fn refresh_button(&mut self, ui: &mut Ui) {
        if crate::bar::refresh_button(ui, tl!("Check for updates"), self.checking).clicked() {
            self.check_now();
        }
    }

    fn sort_picker(&mut self, ui: &mut Ui) {
        egui::ComboBox::from_id_salt("sort")
            .selected_text(RichText::new(grid_sort_label(grid_sort_now(&self.settings))).size(13.0))
            .width(190.0)
            .show_ui(ui, |ui| {
                let current = grid_sort_now(&self.settings);
                for choice in GRID_SORTS {
                    if ui.selectable_label(current == choice, grid_sort_label(choice)).clicked() {
                        self.settings.sort = choice.0;
                        self.settings.sort_descending = choice.1;
                    }
                }
            });
    }

    /// Update every eligible app; used in the compact and full-width toolbars.
    fn update_all_button(&mut self, ui: &mut Ui, updates: usize) {
        let label = if updates == 1 { tl!("Update 1 app").to_string() } else { fmt(tl!("Update all ({count})"), &[("count", &updates.to_string())]) };
        if widgets::button(ui, &label, Kind::Primary, 0.0).clicked() {
            let ids: Vec<&str> = self.entries.iter().filter(|e| self.in_filter(e, Filter::Updates) && e.job.is_none()).map(|e| e.app.id).collect();
            for id in ids {
                self.send(Command::Install { app: id.to_string() });
            }
        }
    }

    /// News from ArtCraft's release feed (and app renames), until the user dismisses each.
    fn announcements(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let shown: Vec<artcraft_launcher_core::feed::Notice> =
            self.announcements.iter().filter(|n| !self.settings.dismissed_notices.contains(&n.id)).take(3).cloned().collect();
        for notice in shown {
            egui::Frame::new().fill(t.sunken).stroke(t.hairline()).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if widgets::glyph_button(ui, widgets::Glyph::Close, tl!("Dismiss")).clicked() {
                            self.settings.dismissed_notices.insert(notice.id.clone());
                        }
                        if let Some(link) = &notice.link {
                            widgets::hud_link(ui, tl!("Learn more"), link);
                        }
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            if !notice.title.is_empty() {
                                ui.label(RichText::new(&notice.title).family(theme::semibold()).color(t.ink_strong));
                            }
                            if !notice.body.is_empty() {
                                ui.add(egui::Label::new(RichText::new(&notice.body).color(t.muted)).wrap());
                            }
                        });
                    });
                });
            });
            ui.add_space(8.0);
        }
    }

    pub(crate) fn footer(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let now = self.backend.now();
        let status = if self.checking {
            tl!("Checking for updates…").to_string()
        } else if let Some(at) = self.settings.last_check {
            let ago = ago_text(at, now);
            match self.sources_label() {
                Some(using) => fmt(tl!("Checked {ago} · using {sources}"), &[("ago", &ago), ("sources", &using)]),
                None => fmt(tl!("Checked {ago}"), &[("ago", &ago)]),
            }
        } else {
            tl!("Not checked yet").to_string()
        };
        // The full notice, and a short one for a narrow window.
        let notice = self.rate_limited_until.filter(|u| *u > now).map(|until| {
            let min = ((until - now) / 60).max(1).to_string();
            let full = fmt(tl!("GitHub's hourly limit was reached; full details return in {min} min"), &[("min", &min)]);
            let short = fmt(tl!("GitHub limit hit; back in {min} min"), &[("min", &min)]);
            (format!("· {full}"), format!("· {short}"))
        });
        use artcraft_launcher_core::catalog::{GITHUB_URL, SOCIAL, WEBSITE_URL};
        // Every link is an icon, reading Website, GitHub, Discord, then the social accounts.
        let named =
            |name: &'static str, label: &str, url: &'static str| widgets::IconLink { name, label: label.to_string(), tip: label.to_string(), url };
        let mut links = vec![
            named("Website", tl!("Website"), WEBSITE_URL),
            named("GitHub", tl!("GitHub"), GITHUB_URL),
            widgets::IconLink {
                name: "Discord",
                label: tl!("Discord").to_string(),
                tip: tl!("Join the ArtCraft community on Discord").to_string(),
                url: self.discord_url,
            },
        ];
        links.extend(SOCIAL.iter().map(|(name, url)| widgets::IconLink {
            name,
            label: (*name).to_string(),
            tip: fmt(tl!("ArtCraft on {name}"), &[("name", name)]),
            url,
        }));
        let icon = 13.0;
        let spacing = ui.spacing().item_spacing.x;
        // The icons on the right keep their size; the status on the left gets what is left and
        // is cut short with "…", never drawn under them.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            widgets::icon_links(ui, icon, &links);
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // While GitHub is rate limited, saying so matters more than when we last checked.
                let avail = ui.available_width();
                let text = |s: &str| RichText::new(s).size(12.0).color(t.warn);
                let width = |ui: &Ui, s: &str| {
                    egui::WidgetText::from(text(s)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body).size().x
                        + spacing
                };
                // The short notice once the full one would leave the status under a third.
                let notice = notice.map(|(full, short)| {
                    let w = width(ui, &full);
                    if w <= avail * 0.7 { (full, None, w) } else { (short.clone(), Some(full), width(ui, &short)) }
                });
                let notice_w = notice.as_ref().map_or(0.0, |(_, _, w)| *w);
                widgets::hud_elided(ui, &status, t.faint, (avail - notice_w).max(avail * 0.3));
                if let Some((shown, full, _)) = notice {
                    let resp = ui.add(egui::Label::new(text(&shown)).truncate());
                    if let Some(full) = full {
                        resp.on_hover_text(full);
                    }
                }
            });
        });
    }

    // ---- grid ----------------------------------------------------------------------------

    /// Installed apps first (`ids` arrives that way), then the rest under a "Not installed" header.
    pub(crate) fn grid(&mut self, ui: &mut Ui, ids: &[&'static str]) {
        let installed = ids.iter().take_while(|id| self.is_installed(id)).count();
        let (top, rest) = ids.split_at(installed);
        self.grid_block(ui, top);
        if !top.is_empty() && !rest.is_empty() {
            let t = Tokens::get(ui.ctx());
            ui.add_space(28.0);
            widgets::hud(ui, tl!("Not installed"), t.faint);
            ui.add_space(12.0);
        }
        self.grid_block(ui, rest);
        ui.add_space(24.0);
    }

    /// The "Third-party apps" section under the Craft Apps, drawn only once one has been added
    /// (the Add app button is in Settings). They are installed, updated and removed like the
    /// Craft Apps (their ⋯ menu also has "Remove app").
    pub(crate) fn third_party_section(&mut self, ui: &mut Ui, ids: &[&'static str], space_above: bool) {
        let t = Tokens::get(ui.ctx());
        if space_above {
            ui.add_space(28.0);
        }
        widgets::hud(ui, tl!("Third-party apps"), t.faint);
        ui.add_space(12.0);
        match self.settings.view {
            ViewMode::Grid => self.grid_block(ui, ids),
            ViewMode::Table => self.table(ui, ids),
        }
        ui.add_space(24.0);
    }

    /// Opens an app's page: the details drawer, or for a third-party app its README (fetched
    /// from GitHub unless it is already here).
    fn open_page(&mut self, id: &str) {
        if self.entry(id).is_some_and(|e| e.app.third_party) {
            // Opening the page is the "try again" for pictures that failed before.
            self.readme_images.retain(|_, picture| !matches!(picture, crate::details::Picture::Failed(_)));
            if !matches!(self.readmes.get(id), Some(crate::readme::Readme::Ready(_) | crate::readme::Readme::Loading)) {
                self.readmes.insert(id.to_string(), crate::readme::Readme::Loading);
                self.send(Command::FetchReadme { app: id.to_string() });
            }
            self.dialog = Some(Dialog::Readme(id.to_string()));
        } else {
            self.detail = Some(id.to_string());
        }
    }

    fn grid_block(&mut self, ui: &mut Ui, ids: &[&'static str]) {
        if ids.is_empty() {
            return;
        }
        let width = ui.available_width();
        // Neighbouring cards share their 1 px hairline, like the site's `gap-px` grid.
        let cols = ((width / CARD_MIN_W).floor() as usize).max(1);
        let card_w = (width + (cols as f32 - 1.0)) / cols as f32;
        let rows = ids.len().div_ceil(cols);
        let height = rows as f32 * (CARD_H - 1.0) + 1.0;
        let (area, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        for (i, id) in ids.iter().enumerate() {
            let (row, col) = (i / cols, i % cols);
            let min = area.min + vec2(col as f32 * (card_w - 1.0), row as f32 * (CARD_H - 1.0));
            self.card(ui, id, Rect::from_min_size(min, vec2(card_w, CARD_H)));
        }
    }

    fn card(&mut self, ui: &mut Ui, id: &str, rect: Rect) {
        let Some(entry) = self.entry(id).cloned() else { return };
        let t = Tokens::get(ui.ctx());
        let app = entry.app;
        let hovered = ui.rect_contains_pointer(rect);
        let wipe = if self.reduce_motion { 0.0 } else { 0.3 };
        let lift = ui.ctx().animate_bool_with_time(ui.id().with(("card-hover", id)), hovered, wipe);
        // An app that is not installed is drawn in greys (icon, name, version, accent), and in its
        // colours while the pointer is over its card.
        let installed = entry.status(self.platform, self.caps.manage_apps).is_installed();
        let in_colour = installed || hovered;
        // The text and accent use the theme's muted grey; the icon is a grey copy of itself.
        let app_color = if installed { rgb(app.color) } else { t.muted.lerp_to_gamma(rgb(app.color), lift) };
        // The whole card opens the app's page (a third-party app's is its README). Added before the
        // card's own links and buttons, so they sit on top of it and keep their clicks.
        if AppDetails::by_id(id).is_some() || app.third_party {
            let open = ui.interact(rect, ui.id().with(("card-open", id)), Sense::click());
            open.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, fmt(tl!("{name} details"), &[("name", app.name)])));
            if open.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.open_page(id);
            }
        }
        let painter = ui.painter_at(rect.expand(16.0));
        // A faint wash in the app's colour fades in under the pointer.
        let fill = t.bg.lerp_to_gamma(app_color, 0.05 * lift);
        painter.rect(rect, CornerRadius::ZERO, fill, t.hairline(), egui::StrokeKind::Inside);
        // On hover, a 2 px rule in the app's colour wipes in from the left along the top, inside the
        // hairlines the card shares with its neighbours (the site's rule sits within its grid gaps).
        if lift > 0.0 {
            let inside = rect.shrink(1.0);
            painter.rect_filled(Rect::from_min_size(inside.min, vec2(inside.width() * lift, 2.0)), CornerRadius::ZERO, app_color);
        }

        // The top strip: the version in the app's colour, and its GitHub page.
        let status = entry.status(self.platform, self.caps.manage_apps);
        let version = crate::card_parts::strip_version(&entry, &status);
        let strip = Rect::from_min_size(rect.min, vec2(rect.width(), STRIP_H));
        painter.hline(strip.x_range(), strip.bottom() - 0.5, t.hairline());
        {
            let mut strip_ui = ui.new_child(
                egui::UiBuilder::new().max_rect(strip.shrink2(vec2(CARD_PAD, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            if !version.is_empty() {
                let github_w = widgets::hud_galley(&strip_ui, &format!("{} ↗", tl!("GitHub")), t.muted).size().x;
                let room = (strip_ui.available_width() - github_w - 16.0).max(0.0);
                widgets::hud_elided(&mut strip_ui, &version, if in_colour { t.app_ink(app) } else { t.muted }, room);
            }
            strip_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                widgets::hud_link(ui, tl!("GitHub"), &app.repo_url());
            });
        }

        let inner = Rect::from_min_max(egui::pos2(rect.left() + CARD_PAD, strip.bottom() + 18.0), rect.max - vec2(CARD_PAD, CARD_PAD));
        // A detached child: the card's contents never move the next card along.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        let ui = &mut child;
        ui.horizontal(|ui| {
            const ICON: f32 = 56.0;
            let [name, tagline] = [FontId::new(22.0, theme::display()), FontId::proportional(13.5)];
            const GAP: f32 = 4.0;
            // The slot is as tall as the text beside it when that is taller, so the two share a centre.
            let block = lines_height(ui, &[&name, &tagline], GAP);
            let (slot, icon_response) = ui.allocate_exact_size(vec2(ICON, block.max(ICON)), Sense::hover());
            icon_response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, fmt(tl!("{name} icon"), &[("name", app.name)])));
            // The icon rises a little on hover, over a soft drop shadow (the site's only shadow).
            let icon_rect = Rect::from_center_size(slot.center(), vec2(ICON, ICON)).translate(vec2(0.0, -4.0 * lift));
            ui.painter().add(
                egui::epaint::Shadow { offset: [0, 6], blur: 14, spread: 0, color: Color32::from_black_alpha(71) }
                    .as_shape(icon_rect.shrink(4.0), CornerRadius::same(12)),
            );
            if let Some(icon) = crate::icons::app_image(ui.ctx(), app.id, in_colour) {
                icon.paint_at(ui, icon_rect);
            }
            ui.add_space(6.0);
            centered_lines(ui, &[&name, &tagline], GAP, |ui| {
                // The name's row is as tall as its text, not as a button.
                ui.spacing_mut().interact_size.y = 0.0;
                ui.horizontal(|ui| {
                    widgets::wordmark_in(ui, app, 22.0, in_colour);
                });
                ui.add(egui::Label::new(RichText::new(tr_ctx(current(), app.id, app.tagline)).font(tagline.clone()).color(t.muted)).truncate());
            });
        });
        ui.add_space(14.0);
        // Installed and up to date needs no badge: the strip has the version, and the group says the rest.
        if !matches!(status, AppStatus::UpToDate | AppStatus::Installed) {
            ui.horizontal(|ui| {
                let (text, tone) = model::entry_status_text(&entry, &status, self.caps.manage_apps);
                let r = widgets::badge(ui, &text, widgets::tone_color(&t, tone));
                if let Some(f) = entry.failure() {
                    r.on_hover_text(fmt(tl!("Couldn't check for updates: {f}"), &[("f", f)]));
                }
            });
            ui.add_space(8.0);
        }
        // Until a version is known, the sentence that says why (no release yet, installed…) stays.
        if version.is_empty() {
            let line = version_line(&entry, &status);
            if !line.trim().is_empty() {
                ui.label(RichText::new(line).size(12.5).color(t.muted));
                ui.add_space(8.0);
            }
        }
        if let Some(details) = AppDetails::by_id(id) {
            crate::card_parts::blurb(ui, details.headline, t.muted, 4);
        }
        if let Some(line) = crate::card_parts::last_updated(&entry) {
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(line).size(12.5).color(t.faint)).selectable(false));
        }
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
                .column(Column::exact(176.0))
                .column(Column::exact(150.0))
                .column(Column::exact(104.0))
                .column(Column::exact(206.0))
                .header(42.0, |mut header| {
                    for (key, label) in [
                        (Some(SortKey::Name), tl!("App")),
                        (Some(SortKey::Status), tl!("Status")),
                        (None, tr_ctx(current(), "column", "Version")),
                        (Some(SortKey::Released), tr_ctx(current(), "column", "Released")),
                        (None, ""),
                    ] {
                        header.col(|ui| {
                            if key == Some(SortKey::Name) {
                                rule(ui);
                            }
                            let Some(k) = key else {
                                // A column that is not sorted still names itself.
                                if !label.is_empty() {
                                    widgets::hud(ui, label, t.faint);
                                }
                                return;
                            };
                            let active = k == self.settings.sort;
                            let tip = if active { tl!("Click to reverse the order") } else { tl!("Click to sort by this column") };
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
                                // Clicking the app's cell opens its page (the content is added after, so it wins any hit).
                                if AppDetails::by_id(id).is_some() || entry.app.third_party {
                                    let open = ui.interact(ui.max_rect(), ui.id().with(("row-open", id)), Sense::click());
                                    open.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::WidgetType::Button,
                                            true,
                                            fmt(tl!("{name} details"), &[("name", entry.app.name)]),
                                        )
                                    });
                                    if open.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                        self.open_page(id);
                                    }
                                }
                                let installed = entry.status(self.platform, self.caps.manage_apps).is_installed();
                                if let Some(icon) = crate::icons::app_image(ui.ctx(), entry.app.id, installed) {
                                    ui.add(icon.fit_to_exact_size(vec2(38.0, 38.0)));
                                }
                                ui.add_space(4.0);
                                ui.vertical(|ui| {
                                    const GAP: f32 = 2.0;
                                    ui.spacing_mut().item_spacing.y = GAP;
                                    // Name and tagline as one block, centred on the icon.
                                    let block = title_h + GAP + tagline_h;
                                    ui.add_space(((ui.available_height() - block) / 2.0).max(0.0));
                                    widgets::wordmark_in(ui, entry.app, TITLE_SIZE, installed);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(tr_ctx(current(), entry.app.id, entry.app.tagline)).size(TAGLINE_SIZE).color(t.muted),
                                        )
                                        .truncate(),
                                    );
                                });
                            });
                            row.col(|ui| {
                                // Installed is all it says: being up to date is the absence of an update.
                                let (text, tone) = if status == AppStatus::UpToDate && entry.job.is_none() && entry.paused.is_none() {
                                    model::status_text(&AppStatus::Installed, self.caps.manage_apps)
                                } else {
                                    model::entry_status_text(&entry, &status, self.caps.manage_apps)
                                };
                                let r = widgets::badge(ui, &text, widgets::tone_color(&t, tone));
                                if let Some(f) = entry.failure() {
                                    r.on_hover_text(fmt(tl!("Couldn't check for updates: {f}"), &[("f", f)]));
                                }
                            });
                            row.col(|ui| {
                                // The version: what is installed, with an arrow to the new one in the app's colour
                                // when an update waits; the latest for an app not installed yet.
                                let mut mono = |text: String, color| ui.label(RichText::new(text).family(egui::FontFamily::Monospace).color(color));
                                match (status == AppStatus::UpdateAvailable).then(|| entry.installed_label().zip(entry.latest_label())).flatten() {
                                    Some((from, to)) => {
                                        mono(format!("{from} → {to}"), t.app_ink(entry.app));
                                    }
                                    None => match (entry.installed_label(), entry.latest_label()) {
                                        (Some(installed), _) => {
                                            mono(installed, t.ink);
                                        }
                                        (None, Some(latest)) => {
                                            mono(latest, t.faint);
                                        }
                                        (None, None) => {
                                            mono("—".into(), t.faint);
                                        }
                                    },
                                }
                            });
                            row.col(|ui| {
                                let when = entry.release().and_then(|r| r.published_at);
                                let r = ui.label(RichText::new(when.map_or_else(|| "—".into(), date_text)).color(t.muted));
                                if let Some(w) = when {
                                    r.on_hover_text(ago_text(w, now));
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

    // ---- actions -------------------------------------------------------------------------

    fn primary_action(&self, entry: &AppEntry, status: &AppStatus) -> Primary {
        if !self.caps.manage_apps {
            let platform = self.platform;
            let url = entry.release().zip(platform).and_then(|(r, p)| {
                let stems: Vec<&str> = entry.app.stems().collect();
                pick_asset(r, &stems, p, p.download_kinds()).map(|(a, _)| a.url.clone())
            });
            return match (status, url) {
                (AppStatus::NoRelease, _) => Primary::Unavailable(tl!("Coming soon")),
                (AppStatus::Checking, _) => Primary::Unavailable(tl!("Checking…")),
                (_, Some(url)) => Primary::Download(url),
                (_, None) => Primary::Unavailable(tl!("Not available")),
            };
        }
        match status {
            AppStatus::Available if entry.ready => Primary::Retry(tl!("Retry install").into()),
            AppStatus::UpdateAvailable if entry.ready => {
                Primary::Retry(fmt(tl!("Retry update to {version}"), &[("version", &entry.latest_label().unwrap_or_default())]))
            }
            AppStatus::Available => Primary::Install,
            AppStatus::UpdateAvailable => Primary::Update(entry.latest_label().unwrap_or_default()),
            AppStatus::UpToDate | AppStatus::Installed => Primary::Open,
            AppStatus::Checking => Primary::Unavailable(tl!("Checking…")),
            AppStatus::NoRelease => Primary::Unavailable(tl!("Coming soon")),
            AppStatus::Unavailable => Primary::Unavailable(if entry.failure().is_some() { tl!("Offline") } else { tl!("Not available") }),
        }
    }

    /// The main button (or progress) and the … menu, within `width`.
    pub(crate) fn actions(&mut self, ui: &mut Ui, entry: &AppEntry, status: &AppStatus, width: f32) {
        self.actions_in(ui, entry, status, width, true);
    }

    /// The main button and the `⋯` menu beside it. `links_in_menu` is false where the page shows
    /// the website, GitHub and releases links as buttons of its own.
    pub(crate) fn actions_in(&mut self, ui: &mut Ui, entry: &AppEntry, status: &AppStatus, width: f32, links_in_menu: bool) {
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
                if total > 0 {
                    fmt(tl!("{done} of {total}"), &[("done", &widgets::human_bytes(done)), ("total", &widgets::human_bytes(total))])
                } else {
                    widgets::human_bytes(done)
                }
            };
            let (fraction, caption, color) = match phase {
                JobPhase::Downloading { done, total } => {
                    let f = (total > 0).then(|| done as f32 / total as f32);
                    let pct = f.map(|f| format!(" · {:.0}%", f * 100.0)).unwrap_or_default();
                    let caption = if bar_w < 150.0 {
                        match f {
                            Some(f) => {
                                fmt(tl!("{done} of {total}"), &[("done", &format!("{:.0}%", f * 100.0)), ("total", &widgets::human_bytes(total))])
                            }
                            None => widgets::human_bytes(done),
                        }
                    } else if short {
                        format!("{}{pct}", amount(done, total))
                    } else {
                        format!("{} {}{pct}", tl!("Downloading"), amount(done, total))
                    };
                    (f, caption, rgb(entry.app.color))
                }
                JobPhase::Reconnecting { done, total } => {
                    let f = (total > 0).then(|| done as f32 / total as f32);
                    (f, if short { tl!("Reconnecting…").into() } else { format!("{} {}", tl!("Reconnecting…"), amount(done, total)) }, t.warn)
                }
                JobPhase::Verifying => (None, tl!("Verifying checksum…").into(), rgb(entry.app.color)),
                JobPhase::Waiting => (None, tl!("Waiting for another app…").into(), rgb(entry.app.color)),
                JobPhase::Installing => (None, tl!("Installing…").into(), rgb(entry.app.color)),
                JobPhase::Removing => (None, tl!("Removing…").into(), rgb(entry.app.color)),
            };
            let downloading = matches!(phase, JobPhase::Downloading { .. } | JobPhase::Reconnecting { .. });
            // A job waiting for the installer hasn't started it yet, so it can still be called off.
            let waiting = phase == JobPhase::Waiting;
            // Waiting has one square button beside the bar, like the menu otherwise.
            widgets::progress(ui, fraction, &caption, color, if downloading { bar_w } else { main_w });
            if downloading && widgets::glyph_button(ui, widgets::Glyph::Pause, tl!("Pause download")).clicked() {
                self.send(Command::Pause { app: id.clone() });
            }
            if (downloading || waiting)
                && widgets::glyph_button(ui, widgets::Glyph::Close, if waiting { tl!("Cancel") } else { tl!("Cancel download") }).clicked()
            {
                self.send(Command::Cancel { app: id });
            }
            return;
        }
        if let Some(p) = entry.paused
            && self.caps.manage_apps
        {
            let pct = if p.total > 0 { format!(" · {:.0}%", p.done as f32 / p.total as f32 * 100.0) } else { String::new() };
            // Resume, then discard and the menu (two squares).
            let resume_w = (width - 2.0 * (theme::CONTROL_H + gap)).max(90.0);
            // Narrow (the table): just tl!("Resume"); the tooltip says how far it got.
            let label = match resume_w {
                w if w < 150.0 => tl!("Resume").to_string(),
                w if w < 220.0 => format!("{}{pct}", tl!("Resume")),
                _ => format!("{}{pct}", tl!("Resume download")),
            };
            let resp = widgets::button(ui, &label, Kind::Primary, resume_w);
            let tip = if p.total > 0 {
                fmt(tl!("{done} of {total} downloaded"), &[("done", &widgets::human_bytes(p.done)), ("total", &widgets::human_bytes(p.total))])
            } else {
                fmt(tl!("{done} downloaded"), &[("done", &widgets::human_bytes(p.done))])
            };
            if resp.on_hover_text(tip).clicked() {
                self.install_app(&id);
            }
            if widgets::glyph_button(ui, widgets::Glyph::Close, tl!("Discard download")).clicked() {
                self.send(Command::Cancel { app: id.clone() });
            }
            self.menu(ui, entry, status, links_in_menu);
            return;
        }
        match self.primary_action(entry, status) {
            Primary::Install => {
                if widgets::button(ui, tl!("Install"), Kind::Primary, main_w).clicked() {
                    self.install_app(&id);
                }
            }
            Primary::Retry(label) => {
                let tip = tl!("The download is still here. It's checked again, then installed.");
                if widgets::button(ui, &label, Kind::Primary, main_w).on_hover_text(tip).clicked() {
                    self.install_app(&id);
                }
            }
            Primary::Update(v) => {
                if widgets::button(ui, &fmt(tl!("Update to {v}"), &[("v", &v)]), Kind::Primary, main_w).clicked() {
                    self.install_app(&id);
                }
            }
            Primary::Open => {
                if widgets::button(ui, tl!("Open"), Kind::Secondary, main_w).clicked() {
                    self.launch_app(&id);
                }
            }
            Primary::Download(url) => {
                let label = match self.platform {
                    Some(p) => fmt(tl!("Download for {os}"), &[("os", p.os.label())]),
                    None => tl!("Download").into(),
                };
                if widgets::button(ui, &label, Kind::Primary, main_w).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            }
            Primary::Unavailable(label) => {
                ui.add_enabled_ui(false, |ui| widgets::button(ui, label, Kind::Secondary, main_w));
            }
        }
        self.menu(ui, entry, status, links_in_menu);
    }

    fn menu(&mut self, ui: &mut Ui, entry: &AppEntry, status: &AppStatus, links: bool) {
        let id = entry.app.id.to_string();
        let manage = self.caps.manage_apps;
        let reveal = self.caps.reveal_label;
        widgets::more_menu(ui, |ui| {
            if entry.release().is_some() && ui.button(tl!("Release notes")).clicked() {
                self.dialog = Some(Dialog::Notes(id.clone()));
                ui.close();
            }
            // Only where Install would download: an up-to-date app would be reinstalled.
            let can_download_again =
                manage && entry.ready && entry.paused.is_none() && matches!(status, AppStatus::Available | AppStatus::UpdateAvailable);
            if can_download_again && ui.button(tl!("Download again")).clicked() {
                self.send(Command::Cancel { app: id.clone() });
                self.install_app(&id);
                ui.close();
            }
            if manage && status.is_installed() {
                if ui.button(tl!("Open")).clicked() {
                    self.launch_app(&id);
                    ui.close();
                }
                if ui.button(t(reveal)).clicked() {
                    self.send(Command::Reveal { app: id.clone() });
                    ui.close();
                }
                if *status == AppStatus::UpToDate && ui.button(tl!("Reinstall")).clicked() {
                    self.install_app(&id);
                    ui.close();
                }
                ui.separator();
                if ui.button(RichText::new(tl!("Uninstall…")).color(Tokens::get(ui.ctx()).bad)).clicked() {
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
                        pick_asset(r, &stems, p, &[*k])
                            .map(|(a, _)| (format!("{} · {}", crate::i18n::t(k.label()), widgets::human_bytes(a.size)), a.url.clone()))
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
            if manage && entry.app.third_party {
                ui.separator();
                if ui.button(RichText::new(tl!("Remove app…")).color(Tokens::get(ui.ctx()).bad)).clicked() {
                    self.dialog = Some(Dialog::ConfirmRemoveThirdParty(id.clone()));
                    ui.close();
                }
            }
            // The drawer shows the links as buttons, so its menu leaves them out (but keeps Hide).
            if links {
                ui.separator();
                let link = |ui: &mut Ui, label: &str, url: String| {
                    if ui.button(label).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        ui.close();
                    }
                };
                link(ui, tl!("Website"), entry.app.website());
                link(ui, tl!("GitHub repository"), entry.app.repo_url());
                link(ui, tl!("All releases"), entry.app.releases_url());
            }
            ui.separator();
            let hidden = self.settings.hidden.contains(entry.app.id);
            if ui.button(if hidden { tl!("Unhide App") } else { tl!("Hide App") }).clicked() {
                if hidden {
                    self.settings.hidden.remove(entry.app.id);
                    if self.settings.hidden.is_empty() && self.settings.filter == Filter::Hidden {
                        self.settings.filter = Filter::All;
                    }
                } else {
                    self.settings.hidden.insert(id);
                }
                ui.close();
            }
        });
    }
}

/// What the grid's sort dropdown offers: the order and, for names, which way. The installed apps
/// always sit above the rest.
const GRID_SORTS: [(SortKey, bool); 4] = [(SortKey::Name, false), (SortKey::Name, true), (SortKey::Released, true), (SortKey::Frequent, true)];

/// Which of [`GRID_SORTS`] the settings come to. Anything else (the old suite order, a table
/// column) reads as Name, A to Z.
pub(crate) fn grid_sort_now(s: &crate::settings::Settings) -> (SortKey, bool) {
    match s.sort {
        SortKey::Name => (SortKey::Name, s.sort_descending),
        SortKey::Released => (SortKey::Released, true),
        SortKey::Frequent => (SortKey::Frequent, true),
        _ => (SortKey::Name, false),
    }
}

fn grid_sort_label((key, descending): (SortKey, bool)) -> &'static str {
    match (key, descending) {
        (SortKey::Released, _) => tl!("Recently updated"),
        (SortKey::Frequent, _) => tl!("Frequently used"),
        (_, true) => tl!("Name (Z–A)"),
        _ => tl!("Name (A–Z)"),
    }
}

/// "0.4.1 installed → 0.5.0 released · Oct 8, 2026" and friends.
pub(crate) fn version_line(e: &AppEntry, status: &AppStatus) -> String {
    let released = e.release().and_then(|r| r.published_at).map(|at| format!(" · {}", date_text(at))).unwrap_or_default();
    let inst_ver = e.installed.as_ref().and_then(|i| i.version.as_ref().map(ToString::to_string));
    let latest = e.latest_label();
    match (status, inst_ver.as_deref(), latest.as_deref()) {
        (AppStatus::UpdateAvailable, Some(i), Some(l)) => {
            format!("{}{released}", fmt(tl!("{version} installed → {latest} released"), &[("version", i), ("latest", l)]))
        }
        (AppStatus::UpToDate, Some(i), _) => format!("{}{released}", fmt(tl!("Version {version}"), &[("version", i)])),
        (_, Some(i), Some(l)) => fmt(tl!("{installed} installed · latest {latest}"), &[("installed", i), ("latest", l)]),
        (_, None, Some(l)) if e.installed.is_some() => fmt(tl!("Installed · latest {latest}"), &[("latest", l)]),
        (_, Some(i), None) => fmt(tl!("Version {version} installed"), &[("version", i)]),
        (_, None, None) if e.installed.is_some() => tl!("Installed").to_string(),
        (_, None, Some(l)) => format!("{}{released}", fmt(tl!("Version {latest}"), &[("latest", l)])),
        (AppStatus::NoRelease, None, None) => tl!("No release published yet").to_string(),
        _ => " ".to_string(),
    }
}

/// The launcher's own download for this platform. `flatpak` is whether this copy runs as a
/// Flatpak; such a copy's download stays the `.flatpak` bundle — offering it an AppImage (or a
/// `.deb`) would point at replacing the sandboxed install with a different one.
pub(crate) fn launcher_download(r: &artcraft_launcher_core::Release, p: artcraft_launcher_core::Platform, flatpak: bool) -> Option<String> {
    use artcraft_launcher_core::PackageKind;
    let kinds: &[PackageKind] = match p.os {
        artcraft_launcher_core::Os::Linux if flatpak => &[PackageKind::Flatpak],
        artcraft_launcher_core::Os::Linux => &[PackageKind::AppImage, PackageKind::Deb],
        _ => p.default_kinds(),
    };
    pick_asset(r, &[artcraft_launcher_core::catalog::LAUNCHER_ID], p, kinds).map(|(a, _)| a.url.clone())
}

/// A column of single-line texts, one per font and `gap` apart, centred across the row it is in.
/// `ui.vertical` starts at the top of the row instead: a row only centres what it knows the height of.
/// Each line is measured as an empty galley, which is rounded to the pixel like a label, unlike `Fonts::row_height`.
fn centered_lines(ui: &mut Ui, fonts: &[&FontId], gap: f32, add_contents: impl FnOnce(&mut Ui)) {
    let height = lines_height(ui, fonts, gap);
    ui.allocate_ui_with_layout(vec2(ui.available_width(), height), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = gap;
        add_contents(ui);
    });
}

/// The height of [`centered_lines`] with these fonts.
fn lines_height(ui: &Ui, fonts: &[&FontId], gap: f32) -> f32 {
    let line = |font: &FontId| ui.fonts_mut(|f| f.layout_no_wrap(String::new(), font.clone(), Color32::PLACEHOLDER)).size().y;
    (fonts.iter().map(|font| line(font) + gap).sum::<f32>() - gap).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use artcraft_launcher_core::catalog::LAUNCHER_ID;
    use artcraft_launcher_core::{Arch, Asset, Platform, Release};

    fn release() -> Release {
        let asset = |name: &str| Asset { name: name.into(), size: 0, url: format!("https://dl.example/{name}"), sha256: None };
        Release {
            tag: "v0.4.1".into(),
            version: None,
            name: "ArtCraft Launcher v0.4.1".into(),
            published_at: None,
            html_url: "https://github.com/storytold/craft-launcher/releases/tag/v0.4.1".into(),
            notes: String::new(),
            prerelease: false,
            assets: vec![
                asset(&format!("{LAUNCHER_ID}-0.4.1-linux-x86_64.flatpak")),
                asset(&format!("{LAUNCHER_ID}-0.4.1-linux-x86_64.AppImage")),
                asset(&format!("{LAUNCHER_ID}-0.4.1-linux-x86_64.deb")),
            ],
        }
    }

    fn platform() -> Platform {
        Platform { os: artcraft_launcher_core::Os::Linux, arch: Arch::X64 }
    }

    #[test]
    fn a_flatpak_copy_of_the_launcher_gets_the_bundle() {
        let url = launcher_download(&release(), platform(), true).unwrap();
        assert_eq!(url, format!("https://dl.example/{LAUNCHER_ID}-0.4.1-linux-x86_64.flatpak"));
    }

    #[test]
    fn a_plain_linux_copy_still_gets_the_appimage() {
        let url = launcher_download(&release(), platform(), false).unwrap();
        assert_eq!(url, format!("https://dl.example/{LAUNCHER_ID}-0.4.1-linux-x86_64.AppImage"));
    }
}
