//! An app's details drawer: it slides in from the right over a dimmed grid. Hand-written copy and
//! screenshots come from `core::details`; the live facts (version, date, download size, release
//! notes) from the app's latest GitHub release.

use artcraft_launcher_core::CraftApp;
use std::sync::Arc;

use artcraft_launcher_core::details::AppDetails;
use artcraft_launcher_core::platform::pick_asset;
use artcraft_launcher_core::protocol::Command;
use egui::{Color32, CornerRadius, Id, Margin, Order, Rect, RichText, Sense, Stroke, Ui, emath::easing, pos2, vec2};

use crate::app::LauncherUi;
use crate::model::AppEntry;
use crate::theme::{self, Tokens, rgb, tint};
use crate::views::{FOOTER_H, HEADER_H, date_text};
use crate::widgets::{self, Glyph, Kind};

/// Screenshots are 8:5.
const SHOT_ASPECT: f32 = 0.625;
const PAD: f32 = 24.0;
/// As tall as the name, headline, "Like …" and version lines beside it, so the icon fills the row.
const ICON: f32 = 112.0;
/// The previous / next buttons over a picture.
const ARROW: f32 = 36.0;
const SLIDE_SECS: f32 = 0.22;

/// A details picture, as the backend has sent it.
pub(crate) enum Picture {
    /// Asked for, not here yet.
    Loading,
    Ready(Arc<[u8]>),
    /// Why it couldn't be had (offline, not published…).
    Failed(String),
}

impl Picture {
    pub(crate) fn from_result(result: Result<Vec<u8>, String>) -> Self {
        match result {
            Ok(bytes) => Self::Ready(bytes.into()),
            Err(why) => Self::Failed(why),
        }
    }
}

impl LauncherUi {
    /// Asks the backend for the pictures of `app`'s page that we don't have. Pictures that failed
    /// earlier are asked for again: opening the page is the "try again".
    fn request_pictures(&mut self, app: &str) {
        let Some(details) = AppDetails::by_id(app) else { return };
        let mut missing = false;
        for shot in details.shots {
            let key = (details.id.to_string(), shot.key.to_string());
            if !matches!(self.pictures.get(&key), Some(Picture::Ready(_) | Picture::Loading)) {
                self.pictures.insert(key, Picture::Loading);
                missing = true;
            }
        }
        if missing {
            self.send(Command::FetchScreenshots { app: details.id.to_string() });
        }
    }

    /// Why the picture couldn't be had, if that is what happened to it.
    fn failure(&self, app: &str, key: &str) -> Option<String> {
        match self.pictures.get(&(app.to_string(), key.to_string())) {
            Some(Picture::Failed(why)) => Some(why.clone()),
            _ => None,
        }
    }

    /// The picture to draw, once it has arrived.
    fn picture_source(&self, app: &str, key: &str) -> Option<egui::ImageSource<'static>> {
        match self.pictures.get(&(app.to_string(), key.to_string())) {
            Some(Picture::Ready(bytes)) => Some(egui::ImageSource::Bytes {
                uri: format!("bytes://screenshots/{app}/{key}.jpg").into(),
                bytes: egui::load::Bytes::Shared(bytes.clone()),
            }),
            _ => None,
        }
    }

    /// Draws the drawer for `self.detail`, sliding in and out. Call it every frame, after the
    /// main panels, so it covers them.
    pub(crate) fn details_drawer(&mut self, ctx: &egui::Context) {
        if let Some(id) = &self.detail
            && self.detail_shown.as_ref() != Some(id)
        {
            self.detail_shown = Some(id.clone());
            self.detail_shot = 0;
            self.detail_zoom = false;
            let app = id.clone();
            self.request_pictures(&app);
        }
        // Asked every frame, even while closed, so the first opening starts from 0 and slides.
        let open = self.detail.is_some();
        let slide = if self.reduce_motion { 0.0 } else { SLIDE_SECS };
        let k = ctx.animate_bool_with_time_and_easing(Id::new("details-slide"), open, slide, easing::cubic_out);
        let Some(id) = self.detail_shown.clone() else { return };
        let Some(details) = AppDetails::by_id(&id) else {
            self.detail = None;
            self.detail_shown = None;
            return;
        };
        if k <= 0.0 && !open {
            self.detail_shown = None;
            return;
        }
        let Some(entry) = self.entry(&id).cloned() else { return };
        if !open {
            self.detail_zoom = false;
        }
        self.details_keys(ctx, details, open);

        let t = Tokens::get(ctx);
        let area = drawer_area(ctx);
        let width = (area.width() * 0.5).clamp(440.0, 680.0).min(area.width());
        let left = area.right() - width * k;
        self.details_scrim(ctx, area, k, &t);

        // Not constrained to the screen: the drawer is partly off it while it slides, and a constrained
        // area would keep its contents in place while only the panel behind them moved.
        egui::Area::new(Id::new("details-drawer")).order(Order::Foreground).fixed_pos(pos2(left, area.top())).constrain(false).show(ctx, |ui| {
            let panel = Rect::from_min_size(pos2(left, area.top()), vec2(width, area.height()));
            let painter = ui.painter();
            painter.add(theme::shadow(&t).as_shape(panel, CornerRadius::ZERO));
            painter.rect_filled(panel, CornerRadius::ZERO, t.bg);
            painter.vline(panel.left() + 0.5, panel.y_range(), Stroke::new(1.0, t.line_strong));

            ui.set_min_size(panel.size());
            ui.set_max_size(panel.size());
            // A bar that stays on screen says there is more below the fold. egui's default one is
            // floating and invisible until the pointer is over it, so the drawer uses the solid style.
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 8.0;
                self.details_hero(ui, &entry, details, &t);
                egui::Frame::new().inner_margin(Margin::same(PAD as i8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.gallery(ui, details, &t, rgb(entry.app.color));
                    ui.add_space(8.0);
                    for p in details.paragraphs() {
                        ui.add(egui::Label::new(RichText::new(p).size(14.5).color(t.ink)).wrap());
                    }
                    ui.add_space(14.0);
                    features(ui, details, entry.app, &t);
                    ui.add_space(14.0);
                    self.release_section(ui, &entry, details, &t);
                });
            });
        });
        if self.detail_zoom && open {
            self.lightbox(ctx, details, &t);
        }
    }

    /// Esc closes the enlarged picture first, then the drawer; ← and → flip through the pictures.
    /// Nothing happens under a dialog, and the arrows leave a text field alone.
    fn details_keys(&mut self, ctx: &egui::Context, details: &AppDetails, open: bool) {
        if !open || self.dialog.is_some() {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.detail_zoom {
                self.detail_zoom = false;
            } else {
                self.detail = None;
            }
        }
        let shots = details.shots.len();
        if shots > 1 && !ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                self.flip_shot(shots, true);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                self.flip_shot(shots, false);
            }
        }
    }

    /// Moves to the next (or previous) of `count` pictures, wrapping around.
    fn flip_shot(&mut self, count: usize, forward: bool) {
        if count < 2 {
            return;
        }
        let at = self.detail_shot.min(count - 1);
        self.detail_shot = if forward { (at + 1) % count } else { (at + count - 1) % count };
    }

    /// The page behind the drawer, dimmed (egui can't blur what is under a layer); a click on it closes the drawer.
    fn details_scrim(&mut self, ctx: &egui::Context, area: Rect, k: f32, t: &Tokens) {
        egui::Area::new(Id::new("details-scrim")).order(Order::Middle).fixed_pos(area.min).show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(area.size(), Sense::click());
            ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::scrim(t, k));
            if resp.on_hover_cursor(egui::CursorIcon::Default).clicked() {
                self.detail = None;
            }
        });
    }

    /// The drawer's top: a wash of the app's colour under a 2 px rule, the close button, the name,
    /// version and main button, and the app's links.
    fn details_hero(&mut self, ui: &mut Ui, entry: &AppEntry, details: &AppDetails, t: &Tokens) {
        let app = entry.app;
        let app_color = rgb(app.color);
        let status = entry.status(self.platform, self.caps.manage_apps);
        let hero = egui::Frame::new().fill(tint(app_color, if t.dark { 0.09 } else { 0.07 })).inner_margin(Margin::same(PAD as i8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                widgets::hud(ui, tl!("App details"), t.faint);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::glyph_button(ui, Glyph::Close, tl!("Close (Esc)")).clicked() {
                        self.detail = None;
                    }
                });
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if let Some(icon) = crate::icons::app(app.id) {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(ICON, ICON)));
                }
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 5.0;
                    widgets::wordmark(ui, app, 32.0);
                    ui.add(egui::Label::new(RichText::new(details.headline).size(15.0).color(t.ink)).wrap());
                    // "Version 0.5.0 · Oct 8, 2026", or the installed-versus-latest line.
                    let version = crate::views::version_line(entry, &status);
                    if !version.trim().is_empty() {
                        ui.label(RichText::new(version).size(12.5).color(t.muted));
                    }
                    // What it is, like the grid and table badges.
                    if let Some(format) = match &entry.installed {
                        Some(_) => entry.installed_kind_label(),
                        None => entry.planned_kind_label(self.platform, &self.settings.preferred_kinds, &self.caps.install_kinds),
                    } {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let (_, tone) = crate::model::entry_status_text(entry, &status, self.caps.manage_apps);
                            crate::widgets::badge(ui, format, crate::widgets::tone_color(t, tone));
                        });
                    }
                });
            });
            ui.add_space(14.0);
            // The menu leaves out the links: they are the buttons right below.
            let row_w = ui.available_width();
            ui.horizontal(|ui| self.actions_in(ui, entry, &status, row_w, false));
            link_buttons(ui, app, row_w);
        });
        let r = hero.response.rect;
        ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width(), 2.0)), CornerRadius::ZERO, app_color);
        ui.painter().hline(r.x_range(), r.bottom() - 0.5, t.hairline());
    }

    /// The facts of the latest release in cells, then its notes.
    fn release_section(&self, ui: &mut Ui, entry: &AppEntry, details: &AppDetails, t: &Tokens) {
        widgets::eyebrow(ui, "02", tl!("Latest release"));
        ui.add_space(4.0);
        let Some(r) = entry.release() else {
            ui.label(RichText::new(tl!("No release published yet")).color(t.muted));
            return;
        };
        let size = self.platform.and_then(|p| {
            let stems: Vec<&str> = entry.app.stems().collect();
            pick_asset(r, &stems, p, p.default_kinds()).map(|(a, _)| widgets::human_bytes(a.size))
        });
        let facts: Vec<(&str, String)> = [
            (tl!("Version"), Some(r.version.as_ref().map_or_else(|| r.tag.clone(), |v| v.to_string()))),
            (tl!("Released"), r.published_at.map(date_text)),
            (tl!("Download"), size),
            (tl!("Platforms"), Some(details.platforms.to_string())),
        ]
        .into_iter()
        .filter_map(|(label, value)| value.map(|v| (label, v)))
        .collect();
        for pair in facts.chunks(2) {
            ui.columns(2, |cols| {
                for (col, (label, value)) in cols.iter_mut().zip(pair) {
                    egui::Frame::new().stroke(t.hairline()).inner_margin(Margin::same(12)).show(col, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 2.0;
                        widgets::hud(ui, label, t.faint);
                        ui.add(egui::Label::new(RichText::new(value).size(14.0).color(t.ink_strong)).wrap());
                    });
                }
            });
        }
        ui.add_space(8.0);
        crate::notes::show(ui, &r.notes);
    }

    /// The current screenshot, large, over the whole window; ‹ › flip through them, a click or Esc closes it.
    fn lightbox(&mut self, ctx: &egui::Context, details: &AppDetails, t: &Tokens) {
        let count = details.shots.len();
        let at = self.detail_shot.min(count.saturating_sub(1));
        let Some(shot) = details.shots.get(at) else {
            self.detail_zoom = false;
            return;
        };
        let src = self.picture_source(details.id, shot.key);
        let failed = self.failure(details.id, shot.key);
        let full = ctx.content_rect();
        egui::Area::new(Id::new("details-lightbox")).order(Order::Tooltip).fixed_pos(full.min).show(ctx, |ui| {
            let (rect, backdrop) = ui.allocate_exact_size(full.size(), Sense::click());
            ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::backdrop());
            // As large as the window allows, but no larger than 1400 points: the files are 1200 px wide.
            let room = rect.shrink2(vec2(112.0, 96.0));
            let w = room.width().min(room.height() / SHOT_ASPECT).clamp(1.0, 1400.0);
            let pic = Rect::from_center_size(rect.center(), vec2(w, w * SHOT_ASPECT));
            match src {
                Some(src) => egui::Image::new(src).paint_at(ui, pic),
                // Flipped to a picture that hasn't arrived: the same frame, saying so.
                None => placeholder(ui, pic, Id::new("lightbox-placeholder"), failed.as_deref(), t, true),
            }
            ui.painter().rect_stroke(pic, CornerRadius::ZERO, Stroke::new(1.0, theme::backdrop_ink(40)), egui::StrokeKind::Outside);
            let caption = if count > 1 { format!("{} · {}/{count}", shot.caption, at + 1) } else { shot.caption.to_string() };
            let ink = theme::backdrop_ink(190);
            let galley = widgets::hud_galley(ui, &caption, ink);
            let caption_at = pos2(rect.center().x - galley.size().x / 2.0, pic.bottom() + 14.0);
            ui.painter().galley(caption_at, galley, ink);

            if count > 1 {
                let y = pic.center().y - ARROW / 2.0;
                let prev = Rect::from_min_size(pos2(pic.left() - 16.0 - ARROW, y), vec2(ARROW, ARROW));
                let next = Rect::from_min_size(pos2(pic.right() + 16.0, y), vec2(ARROW, ARROW));
                if arrow_button(ui, prev, Id::new("zoom-prev"), true, tl!("Previous picture (←)"), t) {
                    self.flip_shot(count, false);
                }
                if arrow_button(ui, next, Id::new("zoom-next"), false, tl!("Next picture (→)"), t) {
                    self.flip_shot(count, true);
                }
            }
            let close =
                Rect::from_min_size(pos2(rect.right() - 16.0 - theme::CONTROL_H, rect.top() + 16.0), vec2(theme::CONTROL_H, theme::CONTROL_H));
            let mut done = backdrop.clicked();
            ui.scope_builder(egui::UiBuilder::new().max_rect(close), |ui| {
                done |= widgets::glyph_button(ui, Glyph::Close, tl!("Close (Esc)")).clicked();
            });
            if done {
                self.detail_zoom = false;
            }
        });
    }

    /// The lead screenshot, its caption, and a row of thumbnails to switch between them. A picture
    /// that hasn't arrived is a frame of the same size saying so, so nothing moves when it does.
    fn gallery(&mut self, ui: &mut Ui, details: &AppDetails, t: &Tokens, app_color: Color32) {
        let Some(&current) = details.shots.get(self.detail_shot).or(details.shots.first()) else { return };
        let w = ui.available_width();
        let pic = match self.picture_source(details.id, current.key) {
            Some(lead) => {
                let image = ui.add(egui::Image::new(lead).fit_to_exact_size(vec2(w, w * SHOT_ASPECT)).sense(Sense::click()));
                image.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Enlarge picture")));
                let rect = image.rect;
                // The arrows below are added after the picture, so they keep their own clicks.
                if image.on_hover_cursor(egui::CursorIcon::ZoomIn).on_hover_text(tl!("Click to enlarge")).clicked() {
                    self.detail_zoom = true;
                }
                rect
            }
            None => {
                let (rect, _) = ui.allocate_exact_size(vec2(w, w * SHOT_ASPECT), Sense::hover());
                let failed = self.failure(details.id, current.key);
                placeholder(ui, rect, Id::new("lead-placeholder"), failed.as_deref(), t, false);
                rect
            }
        };
        ui.painter().rect_stroke(pic, CornerRadius::ZERO, t.hairline(), egui::StrokeKind::Inside);
        let count = details.shots.len();
        if count < 2 {
            widgets::hud(ui, current.caption, t.faint);
            return;
        }
        // Previous and next, over the sides of the picture.
        let y = pic.center().y - ARROW / 2.0;
        let prev = Rect::from_min_size(pos2(pic.left() + 10.0, y), vec2(ARROW, ARROW));
        let next = Rect::from_min_size(pos2(pic.right() - 10.0 - ARROW, y), vec2(ARROW, ARROW));
        if arrow_button(ui, prev, Id::new("shot-prev"), true, tl!("Previous picture (←)"), t) {
            self.flip_shot(count, false);
        }
        if arrow_button(ui, next, Id::new("shot-next"), false, tl!("Next picture (→)"), t) {
            self.flip_shot(count, true);
        }
        widgets::hud(ui, &format!("{} · {}/{count}", current.caption, self.detail_shot.min(count - 1) + 1), t.faint);
        let gap = 8.0;
        let thumb_w = ((w - gap * (count as f32 - 1.0)) / count as f32).floor().max(1.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (i, shot) in details.shots.iter().enumerate() {
                let selected = i == self.detail_shot;
                let size = vec2(thumb_w, thumb_w * SHOT_ASPECT);
                let resp = match self.picture_source(details.id, shot.key) {
                    Some(src) => ui.add(egui::Image::new(src).fit_to_exact_size(size).sense(Sense::click())),
                    // Too small for words: a blank frame, still there to pick.
                    None => {
                        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                        ui.painter().rect_filled(rect, CornerRadius::ZERO, t.sunken);
                        resp
                    }
                };
                // The other pictures sit under a veil of the page's own background (a little lighter on hover):
                // they fade toward the page, not toward grey, in both themes.
                if !selected {
                    let veil = if resp.hovered() { 0.25 } else { 0.55 };
                    ui.painter().rect_filled(resp.rect, CornerRadius::ZERO, t.bg.gamma_multiply(veil));
                }
                let stroke = if selected { Stroke::new(2.0, app_color) } else { t.hairline() };
                ui.painter().rect_stroke(resp.rect, CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(shot.caption).clicked() {
                    self.detail_shot = i;
                }
            }
        });
    }
}

/// What stands in for a picture that isn't here: a frame of the picture's size saying why. `failed`
/// is the reason the picture couldn't be had; without one it is still on its way.
fn placeholder(ui: &mut Ui, rect: Rect, id: Id, failed: Option<&str>, t: &Tokens, over_backdrop: bool) {
    let text = if failed.is_some() { tl!("Pictures couldn't be loaded. Reopen this page to try again.") } else { tl!("Loading pictures…") };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, if over_backdrop { theme::backdrop_panel() } else { t.sunken });
    let ink = if over_backdrop { theme::backdrop_ink(190) } else { t.faint };
    let galley = ui.painter().layout(text.to_string(), egui::FontId::proportional(13.5), ink, (rect.width() - 32.0).max(1.0));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    let resp = ui.interact(rect, id, Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    if let Some(why) = failed {
        resp.on_hover_text(why);
    }
}

/// Between the header and the footer, which stay usable. In a window shorter than the two bars
/// together there is no room left: an empty area, never a negative one.
fn drawer_area(ctx: &egui::Context) -> Rect {
    let full = ctx.content_rect();
    let top = (full.top() + HEADER_H).min(full.bottom());
    let bottom = (full.bottom() - FOOTER_H).max(top);
    Rect::from_min_max(pos2(full.left(), top), pos2(full.right(), bottom))
}

/// The app's links as a row of buttons sharing `row_w`; the ↗ says each opens the browser, like the links on the cards.
fn link_buttons(ui: &mut Ui, app: &CraftApp, row_w: f32) {
    let links = [(tl!("Website"), app.website()), (tl!("GitHub"), app.repo_url()), (tl!("Releases"), app.releases_url())];
    let gap = ui.spacing().item_spacing.x;
    let link_w = ((row_w - gap * (links.len() as f32 - 1.0)) / links.len() as f32).floor();
    ui.horizontal(|ui| {
        for (label, url) in links {
            if widgets::button(ui, &format!("{label} ↗"), Kind::Secondary, link_w).on_hover_text(url.as_str()).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
        }
    });
}

/// The numbered feature cells.
fn features(ui: &mut Ui, details: &AppDetails, app: &CraftApp, t: &Tokens) {
    widgets::eyebrow(ui, "01", tl!("Features"));
    ui.add_space(4.0);
    for (i, f) in details.features.iter().enumerate() {
        egui::Frame::new().stroke(t.hairline()).inner_margin(Margin::symmetric(14, 12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                widgets::hud(ui, &format!("{:02}", i + 1), t.app_ink(app));
                ui.label(RichText::new(f.title).size(15.0).family(theme::semibold()).color(t.ink_strong));
            });
            ui.add(egui::Label::new(RichText::new(f.text).size(13.5).color(t.muted)).wrap());
        });
    }
}

/// A square button with a chevron, drawn over a picture: opaque enough to read on any image,
/// inverted on hover like the site's outlined buttons. True when clicked.
fn arrow_button(ui: &mut Ui, rect: Rect, id: Id, left: bool, tip: &str, t: &Tokens) -> bool {
    let resp = ui.interact(rect, id, Sense::click());
    let (fill, ink) = if resp.hovered() { (t.invert_bg, t.invert_fg) } else { (t.bg.gamma_multiply(0.92), t.ink) };
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::ZERO, fill, Stroke::new(1.0, t.line_strong), egui::StrokeKind::Inside);
    let (c, s) = (rect.center(), if left { -1.0 } else { 1.0 });
    let points = vec![pos2(c.x - s * 3.5, c.y - 6.0), pos2(c.x + s * 3.5, c.y), pos2(c.x - s * 3.5, c.y + 6.0)];
    painter.add(egui::Shape::line(points, Stroke::new(1.75, ink)));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked()
}
