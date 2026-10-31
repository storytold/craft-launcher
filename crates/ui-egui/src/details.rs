//! An app's details page: a card that pops up in the middle of the window over a dimmed grid, with
//! arrows to the next and previous app and a click anywhere outside it to close. Hand-written copy
//! and screenshots come from `core::details`; the live facts (version, date, download size, release
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
use crate::views::date_text;
use crate::widgets::{self, Glyph, Kind};

/// Screenshots are 8:5.
const SHOT_ASPECT: f32 = 0.625;
const PAD: f32 = theme::PAD;
/// As tall as the name, headline, "Like …" and version lines beside it, so the icon fills the row.
const ICON: f32 = 112.0;
/// Compact header values once the page is scrolled: a single control-height row with a smaller
/// wordmark and tighter vertical padding.
const COMPACT_TITLE: f32 = 22.0;
const COMPACT_PAD: f32 = 12.0;
const COMPACT_STACK_BELOW: f32 = 572.0;
const COMPACT_ACTION_WIDE_W: f32 = 240.0;
const COMPACT_ACTION_NARROW_W: f32 = 204.0;
/// Scroll distance (in points) considered at the very top of the body.
const TOP_EPSILON: f32 = 0.5;
/// The previous / next buttons over a picture.
const ARROW: f32 = 36.0;
const SLIDE_SECS: f32 = 0.22;
/// The width of the main button and its menu in the page's top, when the page is wide.
const ACTION_W: f32 = 300.0;

/// Active mouse-wheel transition for the collapsible hero bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum WheelHeroTarget {
    #[default]
    None,
    Collapse,
    Expand,
}

/// Pre-computed metrics and text layout for the collapsible details hero at a given width.
struct HeroGeometry {
    full_w: f32,
    inner_w: f32,
    compact_stack: bool,
    has_icon: bool,
    icon_advance: f32,
    text_w: f32,
    wordmark_exp_h: f32,
    wordmark_cmp_h: f32,
    headline_h: f32,
    version: String,
    has_version: bool,
    version_h: f32,
    identity_h: f32,
    expanded_h: f32,
    compact_h: f32,
}

/// Per-app scroll and layout metrics stored in egui's temporary memory so the hero and scroll bar
/// share one continuous scroll range.
#[derive(Clone, Copy, Debug, Default)]
struct HeroScroll {
    reset_for_new_app: bool,
    wheel_target: WheelHeroTarget,
    last_collapse: f32,
    offset_y: f32,
    body_h: f32,
    content_w: f32,
}

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
            // The backend already enforces the pixel budget; re-check here so bytes can never
            // reach egui's image loader (which allocates from the JPEG header) unvalidated.
            Ok(bytes) if artcraft_launcher_core::details::screenshot_within_budget(&bytes) => Self::Ready(bytes.into()),
            Ok(_) => Self::Failed("the picture's dimensions are too large".into()),
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
            self.act_command(Command::FetchScreenshots { app: details.id.to_string() });
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
            let scroll_key = Id::unique("details-hero-scroll").with(id.as_str());
            ctx.data_mut(|d| {
                let mut s = d.get_temp::<HeroScroll>(scroll_key).unwrap_or_default();
                s.reset_for_new_app = true;
                s.wheel_target = WheelHeroTarget::None;
                s.last_collapse = 0.0;
                s.offset_y = 0.0;
                d.insert_temp(scroll_key, s);
            });
            let app = id.clone();
            self.request_pictures(&app);
        }
        // Asked every frame, even while closed, so the first opening starts from 0 and slides.
        let open = self.detail.is_some();
        let slide = if self.reduce_motion { 0.0 } else { SLIDE_SECS };
        let k = ctx.animate_bool_with_time_and_easing(Id::unique("details-slide"), open, slide, easing::cubic_out);
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
        if self.entry(&id).is_none() {
            return;
        }
        if !open {
            self.detail_zoom = false;
        }
        self.details_keys(ctx, details, open);

        let t = Tokens::get(ctx);
        // The whole window is the backdrop; the card sits in the middle and rises a little as it fades in.
        let area = ctx.content_rect();
        let width = (area.width() - 64.0).clamp(480.0, 1280.0).min((area.width() - 16.0).max(1.0));
        let height = (area.height() - 40.0).clamp(320.0, 1000.0).min((area.height() - 8.0).max(1.0));
        let panel = Rect::from_center_size(area.center() + vec2(0.0, (1.0 - k) * 24.0), vec2(width, height));
        self.details_scrim(ctx, area, k, &t);
        self.details_page(ctx, details, &id, panel, k, &t);
        if self.detail_zoom && open {
            self.lightbox(ctx, details, &t);
        }
    }

    /// The card: the panel's chrome and the scrollable page inside it, at `panel`.
    fn details_page(&mut self, ctx: &egui::Context, details: &AppDetails, id: &str, panel: Rect, k: f32, t: &Tokens) {
        let entry = self.entry(id).cloned();
        let Some(entry) = entry else { return };
        let wide = panel.width() >= theme::PAGE_COLUMNS_FROM;
        let (previous, next) = self.neighbour_apps(id);
        egui::Area::new(Id::unique("details-drawer"))
            .order(Order::Foreground)
            .fixed_pos(panel.min)
            .constrain(false)
            .role(egui::Role::Dialog)
            .accessible_name(format!("{}: {}", tl!("App details"), entry.app.name))
            .show(ctx, |ui| {
                ui.set_opacity(k.clamp(0.0, 1.0));
                let painter = ui.painter();
                painter.add(theme::shadow(t).as_shape(panel, CornerRadius::ZERO));
                painter.rect_filled(panel, CornerRadius::ZERO, t.bg);
                painter.rect_stroke(panel, CornerRadius::ZERO, Stroke::new(1.0, t.line_strong), egui::StrokeKind::Inside);

                ui.set_min_size(panel.size());
                ui.set_max_size(panel.size());
                ui.spacing_mut().item_spacing.y = 0.0;

                // A bar that stays on screen says there is more below the fold; in the app's colour.
                let page_visuals = crate::sheet::accent_scroll(ui, t, t.app_ink(entry.app));
                let bar_space = ui.spacing().scroll.bar_inner_margin + ui.spacing().scroll.bar_width + ui.spacing().scroll.bar_outer_margin;

                let scroll_key = Id::unique("details-hero-scroll").with(id);
                let mut scroll_state = ctx.data_mut(|d| d.get_temp::<HeroScroll>(scroll_key)).unwrap_or_default();
                if scroll_state.reset_for_new_app {
                    scroll_state.wheel_target = WheelHeroTarget::None;
                    scroll_state.last_collapse = 0.0;
                    scroll_state.offset_y = 0.0;
                }
                let hero_w = if scroll_state.content_w > 0.0 { scroll_state.content_w } else { (panel.width() - bar_space).max(1.0) };
                let mut hero_geom = self.hero_geometry(ui, &entry, details, t, wide, hero_w);
                let (expanded_h, compact_h) = (hero_geom.expanded_h, hero_geom.compact_h);
                let collapse_range = (expanded_h - compact_h).max(1.0);
                let compact_viewport_h = (panel.height() - compact_h).max(0.0);
                let raw_max_scroll = (expanded_h + scroll_state.body_h - panel.height()).max(0.0);
                let can_scroll = raw_max_scroll > 1.0;
                let max_scroll = if can_scroll { raw_max_scroll.max(collapse_range) } else { 0.0 };
                if !can_scroll {
                    scroll_state.wheel_target = WheelHeroTarget::None;
                    scroll_state.last_collapse = 0.0;
                    scroll_state.offset_y = 0.0;
                }

                let pointer_in_panel = !self.detail_zoom && self.dialog.is_none() && ui.rect_contains_pointer(panel);
                let pointer_down = ui.input(|i| i.pointer.primary_down() || i.pointer.primary_pressed());
                let has_wheel_event = pointer_in_panel && ui.input(|i| i.raw.events.iter().any(|e| matches!(e, egui::Event::MouseWheel { .. })));
                let wheel_y = if pointer_in_panel { ui.input(|i| i.smooth_scroll_delta.y) } else { 0.0 };

                let prev_target = scroll_state.wheel_target;
                let prev_collapse = scroll_state.last_collapse;
                if !can_scroll || (pointer_down && wheel_y == 0.0) {
                    scroll_state.wheel_target = WheelHeroTarget::None;
                } else {
                    match prev_target {
                        WheelHeroTarget::Collapse => {
                            if wheel_y > 0.0 {
                                scroll_state.wheel_target = WheelHeroTarget::Expand;
                            } else if prev_collapse >= 1.0 - 1e-3 && ((has_wheel_event && wheel_y < 0.0) || wheel_y >= -0.05) {
                                scroll_state.wheel_target = WheelHeroTarget::None;
                            }
                        }
                        WheelHeroTarget::Expand => {
                            if wheel_y < 0.0 {
                                scroll_state.wheel_target = WheelHeroTarget::Collapse;
                            } else if prev_collapse <= 1e-3 && ((has_wheel_event && wheel_y > 0.0) || wheel_y <= 0.05) {
                                scroll_state.wheel_target = WheelHeroTarget::None;
                            }
                        }
                        WheelHeroTarget::None => {
                            if wheel_y < 0.0 && scroll_state.offset_y < collapse_range - TOP_EPSILON {
                                scroll_state.wheel_target = WheelHeroTarget::Collapse;
                            } else if wheel_y > 0.0
                                && scroll_state.offset_y > TOP_EPSILON
                                && scroll_state.offset_y - wheel_y <= collapse_range + TOP_EPSILON
                            {
                                scroll_state.wheel_target = WheelHeroTarget::Expand;
                            }
                        }
                    }
                }

                let anim_id = Id::unique("details-hero-compact").with(id);
                let slide = if self.reduce_motion || scroll_state.reset_for_new_app { 0.0 } else { SLIDE_SECS };
                let lock_wheel = scroll_state.wheel_target != WheelHeroTarget::None;
                let (effective_scroll_y, collapse) = match scroll_state.wheel_target {
                    WheelHeroTarget::Collapse => {
                        let c = ctx.animate_bool_with_time_and_easing(anim_id, true, slide, easing::cubic_out);
                        (c * collapse_range, c)
                    }
                    WheelHeroTarget::Expand => {
                        let c = ctx.animate_bool_with_time_and_easing(anim_id, false, slide, easing::cubic_out);
                        (c * collapse_range, c)
                    }
                    WheelHeroTarget::None => {
                        let y = if can_scroll { scroll_state.offset_y.clamp(0.0, max_scroll) } else { 0.0 };
                        let c = if can_scroll { (y / collapse_range).clamp(0.0, 1.0) } else { 0.0 };
                        let _ = ctx.animate_bool_with_time_and_easing(anim_id, c >= 0.5, 0.0, easing::cubic_out);
                        (y, c)
                    }
                };
                scroll_state.last_collapse = collapse;
                let hero_h = egui::lerp(expanded_h..=compact_h, collapse);

                if lock_wheel && wheel_y != 0.0 {
                    ui.ctx().input_mut(|i| i.smooth_scroll_delta.y = 0.0);
                }

                // One scroll position per app, so the next app opens at its top.
                let mut scroll_area =
                    egui::ScrollArea::vertical().id_salt(("details-scroll", id.to_string())).max_height(panel.height()).auto_shrink([false, false]);
                if scroll_state.reset_for_new_app || lock_wheel {
                    scroll_area = scroll_area.vertical_scroll_offset(effective_scroll_y);
                }
                let body_visuals = page_visuals.clone();
                let scroll_out = scroll_area.show(ui, |ui| {
                    // The bar's colours are for the bar alone.
                    *ui.visuals_mut() = body_visuals;
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.allocate_exact_size(vec2(ui.available_width(), expanded_h), Sense::hover());
                    let body_clip = ui.clip_rect().with_min_y(panel.top() + hero_h);
                    let body_h = ui
                        .scope_builder(egui::UiBuilder::new(), |ui| {
                            ui.set_clip_rect(body_clip);
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 8.0;
                            let body_frame = egui::Frame::new().inner_margin(Margin::same(PAD as i8)).show(ui, |ui| {
                                self.details_body(ui, &entry, details, wide, t);
                            });
                            body_frame.response.rect.height()
                        })
                        .inner;
                    scroll_state.body_h = body_h;
                    if can_scroll && body_h < compact_viewport_h {
                        ui.add_space(compact_viewport_h - body_h);
                    }
                });

                *ui.visuals_mut() = page_visuals;
                let actual_w = scroll_out.inner_rect.width().max(1.0);
                if (actual_w - hero_geom.full_w).abs() > 1e-3 {
                    hero_geom = self.hero_geometry(ui, &entry, details, t, wide, actual_w);
                    ctx.request_repaint();
                }
                scroll_state.content_w = actual_w;
                let hero_h = egui::lerp(hero_geom.expanded_h..=hero_geom.compact_h, collapse);
                let hero_bounds = Rect::from_min_size(scroll_out.inner_rect.min, vec2(actual_w, hero_h));
                let mut hero_ui = ui.new_child(egui::UiBuilder::new().max_rect(hero_bounds));
                hero_ui.set_clip_rect(hero_bounds.intersect(ui.clip_rect()));
                self.details_hero(&mut hero_ui, &entry, details, t, &hero_geom, (previous.as_deref(), next.as_deref()), wide, collapse, k);
                ui.painter().rect_stroke(panel, CornerRadius::ZERO, Stroke::new(1.0, t.line_strong), egui::StrokeKind::Inside);

                let next_offset_y = if lock_wheel { effective_scroll_y } else { scroll_out.state.clamped_offset().y.max(0.0) };
                if (next_offset_y - effective_scroll_y).abs() > 1e-3 {
                    ctx.request_repaint();
                }
                scroll_state.offset_y = next_offset_y;
                scroll_state.reset_for_new_app = false;
                ctx.data_mut(|d| d.insert_temp(scroll_key, scroll_state));
            });
    }

    /// The page below the hero: the story, pictures, features and release facts, in two columns
    /// when the card is wide (release facts under both when there are pictures to make columns)
    /// and stacked when it isn't.
    fn details_body(&mut self, ui: &mut Ui, entry: &AppEntry, details: &AppDetails, wide: bool, t: &Tokens) {
        ui.set_width(ui.available_width());
        let app_color = rgb(entry.app.color);
        if wide && details.shots.is_empty() {
            // No pictures to fill the left side: the story and features share it, and the
            // release facts take the right, so neither side is left empty.
            let gap = theme::COLUMN_GAP;
            let total = ui.available_width();
            let left_w = ((total - gap) * 0.58).floor();
            let right_w = total - gap - left_w;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.vertical(|ui| {
                    ui.set_width(left_w);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    for p in details.paragraphs() {
                        ui.add(egui::Label::new(RichText::new(p).size(14.5).color(t.ink)).wrap());
                    }
                    ui.add_space(14.0);
                    features(ui, details, entry.app, t);
                });
                ui.vertical(|ui| {
                    ui.set_width(right_w);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    self.release_section(ui, entry, details, t, 2);
                });
            });
        } else if wide {
            let gap = theme::COLUMN_GAP;
            let total = ui.available_width();
            let left_w = ((total - gap) * 0.58).floor();
            let right_w = total - gap - left_w;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.vertical(|ui| {
                    ui.set_width(left_w);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    self.gallery(ui, details, t, app_color);
                    ui.add_space(8.0);
                    for p in details.paragraphs() {
                        ui.add(egui::Label::new(RichText::new(p).size(14.5).color(t.ink)).wrap());
                    }
                });
                ui.vertical(|ui| {
                    ui.set_width(right_w);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    features(ui, details, entry.app, t);
                });
            });
            // The release gets the full width under both columns, so neither column
            // is left with an empty tail: its facts sit in one row of four.
            ui.add_space(22.0);
            ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top() - 11.0, t.hairline());
            self.release_section(ui, entry, details, t, 4);
        } else {
            self.gallery(ui, details, t, app_color);
            ui.add_space(8.0);
            for p in details.paragraphs() {
                ui.add(egui::Label::new(RichText::new(p).size(14.5).color(t.ink)).wrap());
            }
            ui.add_space(14.0);
            features(ui, details, entry.app, t);
            ui.add_space(14.0);
            self.release_section(ui, entry, details, t, 2);
        }
    }

    /// The apps before and after `id` in the order the grid shows them (wrapping round), among
    /// those that have a page.
    fn neighbour_apps(&self, id: &str) -> (Option<String>, Option<String>) {
        let mut ids: Vec<&str> = self.visible_ids().into_iter().filter(|i| AppDetails::by_id(i).is_some()).collect();
        if !ids.contains(&id) {
            ids = artcraft_launcher_core::SUITE.iter().map(|a| a.id).filter(|i| AppDetails::by_id(i).is_some()).collect();
        }
        let Some(at) = ids.iter().position(|i| *i == id) else { return (None, None) };
        if ids.len() < 2 {
            return (None, None);
        }
        let n = ids.len();
        let around = |i: usize| ids.get(i % n).map(|s| (*s).to_string());
        (around(at + n - 1), around(at + 1))
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
        egui::Area::new(Id::unique("details-scrim")).order(Order::Middle).fixed_pos(area.min).show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(area.size(), Sense::click());
            ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::scrim(t, k));
            if resp.on_hover_cursor(egui::CursorIcon::Default).clicked() {
                self.detail = None;
            }
        });
    }

    /// Computes the shared geometry and text layout for the details hero at width `full_w`.
    fn hero_geometry(&self, ui: &mut Ui, entry: &AppEntry, details: &AppDetails, t: &Tokens, wide: bool, full_w: f32) -> HeroGeometry {
        let app = entry.app;
        let status = entry.status(self.platform, self.caps.manage_apps);
        let inner_w = (full_w - 2.0 * PAD).max(1.0);
        let compact_stack = full_w < COMPACT_STACK_BELOW;

        let has_icon = crate::icons::app(app.id).is_some();
        let icon_advance = if has_icon { ICON + 16.0 } else { 0.0 };
        let text_w = if wide { (inner_w - icon_advance - ACTION_W - 24.0).max(200.0) } else { (inner_w - icon_advance).max(40.0) };

        let wordmark_exp_h = ui.fonts_mut(|f| f.row_height(&egui::FontId::new(theme::PAGE_TITLE, theme::display())));
        let wordmark_cmp_h = ui.fonts_mut(|f| f.row_height(&egui::FontId::new(COMPACT_TITLE, theme::display())));
        let headline_h = ui.painter().layout(details.headline.to_string(), egui::FontId::proportional(15.0), t.ink, text_w).size().y;
        let version = crate::views::version_line(entry, &status);
        let has_version = !version.trim().is_empty();
        let version_h = if has_version { 5.0 + ui.fonts_mut(|f| f.row_height(&egui::FontId::proportional(12.5))) } else { 0.0 };
        let text_block_h = wordmark_exp_h + 5.0 + headline_h + version_h;
        let identity_h = if has_icon { ICON.max(text_block_h) } else { text_block_h };

        let after_identity_y = PAD + theme::CONTROL_H + 8.0 + identity_h + 30.0;
        let exp_links_top = if wide { after_identity_y } else { after_identity_y + theme::CONTROL_H + 8.0 };
        let expanded_h = exp_links_top + theme::CONTROL_H + PAD;
        let compact_h = if compact_stack { 2.0 * COMPACT_PAD + 2.0 * theme::CONTROL_H + 8.0 } else { 2.0 * COMPACT_PAD + theme::CONTROL_H };
        HeroGeometry {
            full_w,
            inner_w,
            compact_stack,
            has_icon,
            icon_advance,
            text_w,
            wordmark_exp_h,
            wordmark_cmp_h,
            headline_h,
            version,
            has_version,
            version_h,
            identity_h,
            expanded_h,
            compact_h,
        }
    }

    /// The drawer's top: a wash of the app's colour under a 2 px rule, the close button, the name,
    /// version and main button, and the app's links. Once the body scrolls, it smoothly collapses
    /// into a compact sticky bar — gliding the icon, wordmark, primary action (`Open` / `Install`
    /// and `⋯`) and `< > ×` buttons from their expanded positions into the compact row while fading
    /// out the secondary copy and external links.
    #[allow(clippy::too_many_arguments)]
    fn details_hero(
        &mut self,
        ui: &mut Ui,
        entry: &AppEntry,
        details: &AppDetails,
        t: &Tokens,
        geom: &HeroGeometry,
        around: (Option<&str>, Option<&str>),
        wide: bool,
        collapse: f32,
        drawer_opacity: f32,
    ) {
        let app = entry.app;
        let app_color = rgb(app.color);
        let status = entry.status(self.platform, self.caps.manage_apps);
        let top_left = ui.cursor().min;
        let full_w = geom.full_w;
        let inner_left = top_left.x + PAD;
        let inner_right = inner_left + geom.inner_w;
        let inner_w = geom.inner_w;
        let compact_stack = geom.compact_stack;
        let has_icon = geom.has_icon;
        let icon_advance = geom.icon_advance;
        let text_w = geom.text_w;
        let wordmark_exp_h = geom.wordmark_exp_h;
        let wordmark_cmp_h = geom.wordmark_cmp_h;
        let headline_h = geom.headline_h;
        let has_version = geom.has_version;
        let version_h = geom.version_h;
        let identity_h = geom.identity_h;

        // Expanded (collapse = 0.0) geometry.
        let row1_top = top_left.y + PAD;
        let row2_top = row1_top + theme::CONTROL_H + 8.0;
        let after_identity_y = row2_top + identity_h + 30.0;
        let exp_links_top = if wide { after_identity_y } else { after_identity_y + theme::CONTROL_H + 8.0 };
        let expanded_h = geom.expanded_h;

        let nav_count = 1 + usize::from(around.0.is_some()) + usize::from(around.1.is_some());
        let item_gap = ui.spacing().item_spacing.x;
        let nav_w = nav_count as f32 * theme::CONTROL_H + nav_count.saturating_sub(1) as f32 * item_gap;

        let exp_nav_rect = Rect::from_min_size(pos2(inner_right - nav_w, row1_top), vec2(nav_w, theme::CONTROL_H));
        let exp_icon_rect = Rect::from_min_size(pos2(inner_left, row2_top), vec2(ICON, ICON));
        let exp_wordmark_pos = pos2(inner_left + icon_advance, row2_top);
        let exp_action_rect = if wide {
            Rect::from_min_size(pos2(inner_right - ACTION_W, row2_top + (identity_h - theme::CONTROL_H) * 0.5), vec2(ACTION_W, theme::CONTROL_H))
        } else {
            Rect::from_min_size(pos2(inner_left, after_identity_y), vec2(inner_w, theme::CONTROL_H))
        };

        // Compact (collapse = 1.0) geometry.
        let cmp_row1_top = top_left.y + COMPACT_PAD;
        let compact_h = geom.compact_h;
        let cmp_nav_rect = Rect::from_min_size(pos2(inner_right - nav_w, cmp_row1_top), vec2(nav_w, theme::CONTROL_H));
        let cmp_icon_rect = Rect::from_min_size(pos2(inner_left, cmp_row1_top), vec2(theme::CONTROL_H, theme::CONTROL_H));
        let cmp_icon_advance = if has_icon { theme::CONTROL_H + 12.0 } else { 0.0 };
        let cmp_wordmark_pos = pos2(inner_left + cmp_icon_advance, cmp_row1_top + (theme::CONTROL_H - wordmark_cmp_h) * 0.5);
        let cmp_action_w = if wide { COMPACT_ACTION_WIDE_W } else { COMPACT_ACTION_NARROW_W };
        let cmp_action_rect = if compact_stack {
            Rect::from_min_size(pos2(inner_left, cmp_row1_top + theme::CONTROL_H + 8.0), vec2(inner_w, theme::CONTROL_H))
        } else {
            let action_right = cmp_nav_rect.left() - 17.0;
            Rect::from_min_size(pos2(action_right - cmp_action_w, cmp_row1_top), vec2(cmp_action_w, theme::CONTROL_H))
        };

        // Interpolate every shared element between expanded and compact.
        let lerp_rect = |a: Rect, b: Rect| Rect::from_min_max(a.min.lerp(b.min, collapse), a.max.lerp(b.max, collapse));
        let hero_h = egui::lerp(expanded_h..=compact_h, collapse);
        let nav_rect = lerp_rect(exp_nav_rect, cmp_nav_rect);
        let icon_rect = lerp_rect(exp_icon_rect, cmp_icon_rect);
        let action_rect = lerp_rect(exp_action_rect, cmp_action_rect);
        let wordmark_pos = exp_wordmark_pos.lerp(cmp_wordmark_pos, collapse);
        let title_s = egui::lerp(theme::PAGE_TITLE..=COMPACT_TITLE, collapse);
        let fade = (1.0 - collapse * 1.8).clamp(0.0, 1.0);
        let base_opacity = drawer_opacity.clamp(0.0, 1.0);

        let (hero_rect, _) = ui.allocate_exact_size(vec2(full_w, hero_h), Sense::hover());
        ui.painter().rect_filled(hero_rect, CornerRadius::ZERO, t.bg);
        ui.painter().rect_filled(hero_rect, CornerRadius::ZERO, tint(app_color, if t.dark { 0.09 } else { 0.07 }));

        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(hero_rect));
        child.set_clip_rect(hero_rect.intersect(ui.clip_rect()));

        if fade > 0.0 {
            let hud_w = (exp_nav_rect.left() - inner_left - 8.0).max(0.0);
            let hud_rect = Rect::from_min_size(pos2(inner_left, row1_top), vec2(hud_w, theme::CONTROL_H));
            child.scope_builder(egui::UiBuilder::new().max_rect(hud_rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                ui.set_opacity(base_opacity * fade);
                widgets::hud(ui, tl!("App details"), t.faint);
            });
        }

        child.scope_builder(egui::UiBuilder::new().max_rect(nav_rect).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            self.hero_nav_buttons(ui, around, t);
        });

        if let Some(icon) = crate::icons::app(app.id) {
            child.scope_builder(egui::UiBuilder::new().max_rect(icon_rect), |ui| {
                // Decorative: the wordmark beside it carries the app name.
                ui.add(egui::Image::new(icon).fit_to_exact_size(icon_rect.size()));
            });
        }

        let wordmark_right_limit = if compact_stack { nav_rect.left() - 8.0 } else { action_rect.left() - 12.0 };
        let wordmark_w = (wordmark_right_limit - wordmark_pos.x).max(40.0);
        let wordmark_rect = Rect::from_min_size(wordmark_pos, vec2(wordmark_w, wordmark_exp_h));
        let wordmark_bottom = child
            .scope_builder(egui::UiBuilder::new().max_rect(wordmark_rect).layout(egui::Layout::left_to_right(egui::Align::Min)), |ui| {
                widgets::wordmark(ui, app, title_s).rect.bottom()
            })
            .inner;

        if fade > 0.0 {
            let copy_top = wordmark_bottom + 5.0;
            let copy_rect = Rect::from_min_size(pos2(wordmark_pos.x, copy_top), vec2(text_w, headline_h + version_h + 8.0));
            child.scope_builder(egui::UiBuilder::new().max_rect(copy_rect).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
                ui.set_opacity(base_opacity * fade);
                ui.set_width(text_w);
                ui.spacing_mut().item_spacing.y = 5.0;
                ui.add(egui::Label::new(RichText::new(details.headline).size(15.0).color(t.ink)).wrap());
                if has_version {
                    ui.label(RichText::new(&geom.version).size(12.5).color(t.muted));
                }
            });
        }

        if !compact_stack && collapse > 0.0 {
            let rule_x = nav_rect.left() - 8.5;
            child.painter().vline(rule_x, nav_rect.y_range(), Stroke::new(1.0, t.line.gamma_multiply(collapse)));
        }

        child.scope_builder(egui::UiBuilder::new().max_rect(action_rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            self.actions_in(ui, entry, &status, action_rect.width(), false);
        });

        if fade > 0.0 {
            let links_shift_y = (wordmark_pos.y - exp_wordmark_pos.y) * 0.5;
            let links_rect = Rect::from_min_size(pos2(inner_left, exp_links_top + links_shift_y), vec2(inner_w, theme::CONTROL_H));
            child.scope_builder(egui::UiBuilder::new().max_rect(links_rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                ui.set_opacity(base_opacity * fade);
                link_buttons(ui, app);
            });
        }

        ui.painter().rect_filled(Rect::from_min_size(hero_rect.min, vec2(hero_rect.width(), 2.0)), CornerRadius::ZERO, app_color);
        ui.painter().hline(hero_rect.x_range(), hero_rect.bottom() - 0.5, t.hairline());
    }

    /// Close, next app and previous app, laid out right-to-left so they read `< > ×`.
    fn hero_nav_buttons(&mut self, ui: &mut Ui, around: (Option<&str>, Option<&str>), t: &Tokens) {
        if widgets::glyph_button(ui, Glyph::Close, tl!("Close (Esc)")).clicked() {
            self.detail = None;
        }
        if let (_, Some(next)) = around
            && step_button(ui, false, tl!("Next app"), t)
        {
            self.detail = Some(next.to_string());
        }
        if let (Some(previous), _) = around
            && step_button(ui, true, tl!("Previous app"), t)
        {
            self.detail = Some(previous.to_string());
        }
    }

    /// The facts of the latest release in cells, then its notes.
    fn release_section(&self, ui: &mut Ui, entry: &AppEntry, details: &AppDetails, t: &Tokens, per_row: usize) {
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
        for pair in facts.chunks(per_row) {
            ui.columns(per_row, |cols| {
                for (col, (label, value)) in cols.iter_mut().zip(pair) {
                    egui::Frame::new().stroke(t.hairline()).inner_margin(theme::BOX_MARGIN).show(col, |ui| {
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
        egui::Area::new(Id::unique("details-lightbox"))
            .order(Order::Tooltip)
            .fixed_pos(full.min)
            .role(egui::Role::Dialog)
            .accessible_name(format!("{}: {}", tl!("Enlarge picture"), CraftApp::by_id(details.id).map_or("", |app| app.name)))
            .show(ctx, |ui| {
                let (rect, backdrop) = ui.allocate_exact_size(full.size(), Sense::click());
                ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::backdrop());
                // As large as the window allows, but no larger than 1400 points: the files are 1200 px wide.
                let room = rect.shrink2(vec2(112.0, 96.0));
                let w = room.width().min(room.height() / SHOT_ASPECT).clamp(1.0, 1400.0);
                let pic = Rect::from_center_size(rect.center(), vec2(w, w * SHOT_ASPECT));
                match src {
                    Some(src) => egui::Image::new(src).paint_at(ui, pic),
                    // Flipped to a picture that hasn't arrived: the same frame, saying so.
                    None => placeholder(ui, pic, Id::unique("lightbox-placeholder"), failed.as_deref(), t, true),
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
                    if arrow_button(ui, prev, Id::unique("zoom-prev"), true, tl!("Previous picture (←)"), t) {
                        self.flip_shot(count, false);
                    }
                    if arrow_button(ui, next, Id::unique("zoom-next"), false, tl!("Next picture (→)"), t) {
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
                image.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Enlarge picture")));
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
                placeholder(ui, rect, Id::unique("lead-placeholder"), failed.as_deref(), t, false);
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
        if arrow_button(ui, prev, Id::unique("shot-prev"), true, tl!("Previous picture (←)"), t) {
            self.flip_shot(count, false);
        }
        if arrow_button(ui, next, Id::unique("shot-next"), false, tl!("Next picture (→)"), t) {
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
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, shot.caption));
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
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
    if let Some(why) = failed {
        resp.on_hover_text(why);
    }
}

/// A square header-style button with a chevron: previous (`left`) or next app. True when clicked.
fn step_button(ui: &mut Ui, left: bool, tip: &str, t: &Tokens) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(theme::CONTROL_H, theme::CONTROL_H), Sense::click());
    let (fill, ink) = if resp.hovered() { (t.invert_bg, t.invert_fg) } else { (Color32::TRANSPARENT, t.muted) };
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::ZERO, fill, t.hairline(), egui::StrokeKind::Inside);
    let (c, s) = (rect.center(), if left { -1.0 } else { 1.0 });
    painter.add(egui::Shape::line(
        vec![pos2(c.x - s * 3.0, c.y - 5.5), pos2(c.x + s * 3.0, c.y), pos2(c.x - s * 3.0, c.y + 5.5)],
        Stroke::new(1.75, ink),
    ));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked()
}

/// The app's links as a row of buttons each as wide as its label; the ↗ says each opens the
/// browser, like the links on the cards.
fn link_buttons(ui: &mut Ui, app: &CraftApp) {
    let links = [(tl!("Website"), app.website()), (tl!("GitHub"), app.repo_url()), (tl!("Releases"), app.releases_url())];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for (label, url) in links {
            if widgets::button(ui, &format!("{label} ↗"), Kind::Secondary, 0.0).on_hover_text(url.as_str()).clicked() {
                widgets::open_external_link(ui.ctx(), &url);
            }
        }
    });
}

/// The numbered feature cells.
fn features(ui: &mut Ui, details: &AppDetails, app: &CraftApp, t: &Tokens) {
    widgets::eyebrow(ui, "01", tl!("Features"));
    ui.add_space(4.0);
    for (i, f) in details.features.iter().enumerate() {
        egui::Frame::new().stroke(t.hairline()).inner_margin(theme::BOX_MARGIN).show(ui, |ui| {
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
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked()
}
