//! Small building blocks in getartcraft.com's style: mono labels, square buttons, badges and
//! hairline cell groups.
//!
//! Upper-case text is drawn upper case but announced to screen readers as written.

use std::sync::Arc;

use egui::{Color32, CornerRadius, Galley, Rect, Response, RichText, Sense, Stroke, Ui, vec2};

use crate::model::Tone;
use crate::theme::{self, Tokens};

pub fn tone_color(t: &Tokens, tone: Tone) -> Color32 {
    match tone {
        Tone::Good => t.good,
        Tone::Warn => t.warn,
        Tone::Info => t.accent_ink,
        Tone::Neutral => t.muted,
        Tone::Faint => t.faint,
    }
}

/// `text` laid out upper case in `family`, `spacing` points between letters.
pub fn caps(ui: &Ui, text: &str, size: f32, family: egui::FontFamily, spacing: f32, color: Color32) -> Arc<Galley> {
    let format = egui::TextFormat { font_id: egui::FontId::new(size, family), color, extra_letter_spacing: spacing, ..Default::default() };
    ui.painter().layout_job(egui::text::LayoutJob::single_section(text.to_uppercase(), format))
}

/// A `.hud-label`: 11 px Geist Mono, upper case, widely spaced.
pub fn hud_galley(ui: &Ui, text: &str, color: Color32) -> Arc<Galley> {
    caps(ui, text, theme::HUD_SIZE, theme::mono(), theme::HUD_SPACING, color)
}

/// Paints `galley` at the left of `rect`, centred vertically.
fn paint_left(ui: &Ui, rect: Rect, galley: Arc<Galley>, color: Color32) {
    let pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    ui.painter().galley_with_override_text_color(pos, galley, color);
}

/// Paints `galley` centred in `rect`.
fn paint_centred(ui: &Ui, rect: Rect, galley: Arc<Galley>, color: Color32) {
    ui.painter().galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, color);
}

/// A mono label.
pub fn hud(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = hud_galley(ui, text, color);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::hover());
    paint_left(ui, rect, galley, color);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    resp
}

/// A mono label cut to one line with "…" where it is wider than `max_width`; the full text shows
/// on hover and is what screen readers read.
pub fn hud_elided(ui: &mut Ui, text: &str, color: Color32, max_width: f32) -> Response {
    let format = egui::TextFormat {
        font_id: egui::FontId::new(theme::HUD_SIZE, theme::mono()),
        color,
        extra_letter_spacing: theme::HUD_SPACING,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::single_section(text.to_uppercase(), format);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.min(ui.available_width()).max(0.0));
    let galley = ui.painter().layout_job(job);
    let elided = galley.elided;
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::hover());
    paint_left(ui, rect, galley, color);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    if elided { resp.on_hover_text(text) } else { resp }
}

/// "01 / Features": a section eyebrow, the index faint.
pub fn eyebrow(ui: &mut Ui, index: &str, label: &str) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        hud(ui, index, t.faint);
        hud(ui, " / ", t.faint);
        hud(ui, label, t.muted);
    });
}

/// A solid chip with a bold mono label: an app's number tab (the site's `AppTab`).
pub fn tag(ui: &mut Ui, text: &str, fill: Color32, ink: Color32) -> Response {
    let galley = caps(ui, text, theme::HUD_SIZE, theme::mono_bold(), theme::HUD_SPACING, ink);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(16.0, 8.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    paint_centred(ui, rect, galley, ink);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    resp
}

/// A vertical hairline `height` tall: divides the header's cells.
pub fn rule(ui: &mut Ui, height: f32) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, height), Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), t.hairline());
}

/// A square header cell with a symbol and a hairline on its left; inverts on hover. While `busy`
/// it shows a spinner and can't be clicked.
pub fn nav_icon(ui: &mut Ui, symbol: &str, tip: &str, height: f32, busy: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(height, height), if busy { Sense::hover() } else { Sense::click() });
    let p = ui.painter();
    let hovered = resp.hovered() && !busy;
    if hovered {
        p.rect_filled(rect, CornerRadius::ZERO, t.invert_bg);
    }
    p.vline(rect.left() + 0.5, rect.y_range(), t.hairline());
    if busy {
        egui::Spinner::new().size(16.0).color(t.muted).paint_at(ui, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)));
    } else {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, symbol, egui::FontId::proportional(17.0), if hovered { t.invert_fg } else { t.muted });
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, !busy, tip));
    let resp = resp.on_hover_text(tip);
    if busy { resp } else { resp.on_hover_cursor(egui::CursorIcon::PointingHand) }
}

/// How far in from the window's edges a custom title bar's window resizes ([`window_edges`]).
/// The title bar's own controls leave this band alone.
const RESIZE_EDGE: f32 = 5.0;

/// Custom title bars: dragging the header's empty space moves the window, and double-clicking it
/// maximizes or restores it. Call before drawing the header, so its controls stay on top.
pub fn title_bar_drag(ui: &mut Ui) {
    let r = ui.max_rect();
    let rect = Rect::from_min_max(r.min + vec2(RESIZE_EDGE, RESIZE_EDGE), r.max - vec2(RESIZE_EDGE, 0.0));
    let resp = ui.interact(rect, ui.id().with("title-bar"), Sense::click_and_drag());
    if resp.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if resp.double_clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
}

/// A window button in a custom title bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowButton {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Minimize, maximize (or restore) and close, `height` tall, for a right-to-left layout: close
/// ends up at the right.
pub fn window_buttons(ui: &mut Ui, height: f32) {
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    if window_button(ui, WindowButton::Close, height).clicked() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    let size = if maximized { WindowButton::Restore } else { WindowButton::Maximize };
    if window_button(ui, size, height).clicked() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
    if window_button(ui, WindowButton::Minimize, height).clicked() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
    rule(ui, height);
}

fn window_button(ui: &mut Ui, button: WindowButton, height: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(46.0, height), Sense::hover());
    // Clickable below the resize band along the top (and, for close, the right edge).
    let right = if button == WindowButton::Close { RESIZE_EDGE } else { 0.0 };
    let hit = Rect::from_min_max(rect.min + vec2(0.0, RESIZE_EDGE), rect.max - vec2(right, 0.0));
    let resp = ui.interact(hit, ui.id().with(("window-button", button as u8)), Sense::click());
    let (fill, ink) = match (button, resp.hovered()) {
        (WindowButton::Close, true) => (Color32::from_rgb(0xc4, 0x2b, 0x1c), Color32::WHITE),
        (_, true) => (t.line, t.ink_strong),
        (_, false) => (Color32::TRANSPARENT, t.muted),
    };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::ZERO, fill);
    let c = rect.center();
    let stroke = Stroke::new(1.0, ink);
    let label = match button {
        WindowButton::Minimize => tl!("Minimize"),
        WindowButton::Maximize => tl!("Maximize"),
        WindowButton::Restore => tl!("Restore"),
        WindowButton::Close => tl!("Close"),
    };
    // Half the glyph's size: 10 px, like Windows' own caption buttons.
    let s = 5.0;
    match button {
        WindowButton::Minimize => {
            p.hline(c.x - s..=c.x + s, c.y, stroke);
        }
        WindowButton::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, vec2(2.0 * s, 2.0 * s)), CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
        }
        WindowButton::Restore => {
            // A window in front of another.
            let front = Rect::from_min_size(c + vec2(-s, -s + 2.0), vec2(2.0 * s - 2.0, 2.0 * s - 2.0));
            p.rect_stroke(front, CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
            p.hline(c.x - s + 2.0..=c.x + s, c.y - s, stroke);
            p.vline(c.x + s, c.y - s..=c.y + s - 2.0, stroke);
        }
        WindowButton::Close => {
            p.line_segment([c + vec2(-s, -s), c + vec2(s, s)], stroke);
            p.line_segment([c + vec2(-s, s), c + vec2(s, -s)], stroke);
        }
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    resp.on_hover_text(label)
}

/// Custom title bars: a hairline round the window, and resizing it from its edges and corners.
pub fn window_edges(ctx: &egui::Context) {
    use egui::CursorIcon as C;
    use egui::viewport::ResizeDirection as D;
    let (maximized, fullscreen) = ctx.input(|i| (i.viewport().maximized.unwrap_or(false), i.viewport().fullscreen.unwrap_or(false)));
    if maximized || fullscreen {
        return;
    }
    let t = Tokens::get(ctx);
    let r = ctx.content_rect();
    ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("window-border"))).rect_stroke(
        r,
        CornerRadius::ZERO,
        t.hairline(),
        egui::StrokeKind::Inside,
    );
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()).filter(|p| r.contains(*p)) else { return };
    let near = |d: f32| d < RESIZE_EDGE;
    // A corner takes a little of each edge next to it.
    let corner = |d: f32| d < 3.0 * RESIZE_EDGE;
    let (left, right, top, bottom) = (p.x - r.left(), r.right() - p.x, p.y - r.top(), r.bottom() - p.y);
    let hit = if (near(top) && corner(left)) || (near(left) && corner(top)) {
        Some((D::NorthWest, C::ResizeNorthWest))
    } else if (near(top) && corner(right)) || (near(right) && corner(top)) {
        Some((D::NorthEast, C::ResizeNorthEast))
    } else if (near(bottom) && corner(left)) || (near(left) && corner(bottom)) {
        Some((D::SouthWest, C::ResizeSouthWest))
    } else if (near(bottom) && corner(right)) || (near(right) && corner(bottom)) {
        Some((D::SouthEast, C::ResizeSouthEast))
    } else if near(top) {
        Some((D::North, C::ResizeNorth))
    } else if near(bottom) {
        Some((D::South, C::ResizeSouth))
    } else if near(left) {
        Some((D::West, C::ResizeWest))
    } else if near(right) {
        Some((D::East, C::ResizeEast))
    } else {
        None
    };
    if let Some((direction, cursor)) = hit {
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
        }
    }
}

/// A mono link with an ↗: muted, ink on hover. Opens `url` in the browser.
pub fn hud_link(ui: &mut Ui, text: &str, url: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let galley = hud_galley(ui, &format!("{text} ↗"), t.muted);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
    paint_left(ui, rect, galley, if resp.hovered() { t.ink_strong } else { t.muted });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, text));
    let resp = resp.on_hover_text(url).on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
    resp
}

/// A small square status badge: 10 px mono on the sunken surface, in `color`.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let t = Tokens::get(ui.ctx());
    let galley = caps(ui, text, 10.0, theme::mono(), 1.0, color);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 12.0, 20.0), Sense::hover());
    ui.painter().rect(rect, CornerRadius::ZERO, t.sunken, t.hairline(), egui::StrokeKind::Inside);
    paint_centred(ui, rect, galley, color);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    resp
}

/// Which of the site's button variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A solid block in the inverted colours; fades a little on hover.
    Primary,
    /// Outlined; inverts on hover.
    Secondary,
    /// Solid red; fades a little on hover.
    Danger,
}

/// The natural width of a button, including its horizontal padding.
pub fn button_width(ui: &Ui, text: &str, minimum: f32) -> f32 {
    let galley = caps(ui, text, theme::HUD_SIZE, theme::mono_bold(), theme::BUTTON_SPACING, Tokens::get(ui.ctx()).ink);
    (galley.size().x + 32.0).max(minimum)
}

/// A square button with a mono upper-case label, at least `width` wide.
pub fn button(ui: &mut Ui, text: &str, kind: Kind, width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let galley = caps(ui, text, theme::HUD_SIZE, theme::mono_bold(), theme::BUTTON_SPACING, t.ink);
    let size = vec2((galley.size().x + 32.0).max(width), theme::CONTROL_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    let hovered = resp.hovered() && enabled;
    let (fill, stroke, color) = match kind {
        Kind::Primary => (t.invert_bg.gamma_multiply(if hovered { 0.8 } else { 1.0 }), Stroke::NONE, t.invert_fg),
        Kind::Danger => (t.bad.gamma_multiply(if hovered { 0.8 } else { 1.0 }), Stroke::NONE, Color32::WHITE),
        Kind::Secondary if hovered => (t.invert_bg, Stroke::NONE, t.invert_fg),
        Kind::Secondary => (t.bg, Stroke::new(1.0, t.line_strong), t.ink),
    };
    // Disabled: the whole button at half strength, like the site's `opacity-50`.
    let fade = |c: Color32| if enabled { c } else { c.gamma_multiply(0.5) };
    ui.painter().rect(rect, CornerRadius::ZERO, fade(fill), Stroke::new(stroke.width, fade(stroke.color)), egui::StrokeKind::Inside);
    paint_centred(ui, rect, galley, fade(color));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// One option in a [`cells`] group.
pub struct Cell<'a> {
    pub label: &'a str,
    pub tip: &'a str,
    /// A count after the label, and the colour it takes when it isn't zero.
    pub count: Option<(usize, Option<Color32>)>,
}

/// Mutually exclusive options in a row of hairline cells (the site's share bar); the chosen one
/// is inverted. Returns whether the choice changed.
pub fn cells<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, Cell<'_>)]) -> bool {
    cells_apart(ui, value, options, 0.0)
}

/// Like [`cells`], but with `gap` between the options: with a gap each option has a hairline box
/// of its own instead of sharing one.
pub fn cells_apart<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, Cell<'_>)], gap: f32) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut changed = false;
    // In a right-to-left parent the row fills from the right: keep the options' order.
    let rtl = ui.layout().prefer_right_to_left();
    let ordered: Vec<&(T, Cell<'_>)> = if rtl { options.iter().rev().collect() } else { options.iter().collect() };
    let mut rects = Vec::with_capacity(options.len());
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (opt, cell) in ordered {
            let (rect, chosen) = paint_cell(ui, value, opt, cell, &t);
            changed |= chosen;
            rects.push(rect);
        }
    });
    // The hairlines: an outline each when apart; otherwise one outline and a rule between neighbours.
    if gap > 0.0 {
        for r in &rects {
            ui.painter().rect_stroke(*r, CornerRadius::ZERO, t.hairline(), egui::StrokeKind::Inside);
        }
    } else if let (Some(first), Some(last)) = (rects.first(), rects.last()) {
        let p = ui.painter();
        p.rect_stroke(first.union(*last), CornerRadius::ZERO, t.hairline(), egui::StrokeKind::Inside);
        let (top, bottom) = (first.top(), first.bottom());
        for r in rects.iter().skip(1) {
            let x = if rtl { r.right() } else { r.left() };
            p.vline(x, top..=bottom, t.hairline());
        }
    }
    changed
}

/// Paints one cell: its label and count over a hairline box, and its hover/click behaviour.
/// Returns the cell's rect (for the row's hairlines) and whether the click chose it.
fn paint_cell<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, opt: &T, cell: &Cell<'_>, t: &Tokens) -> (Rect, bool) {
    let mut changed = false;
    let on = *value == *opt;
    let label = hud_galley(ui, cell.label, t.ink);
    let count = cell.count.map(|(n, highlight)| {
        let c = if on { t.invert_fg.gamma_multiply(0.6) } else { highlight.filter(|_| n > 0).unwrap_or(t.faint) };
        (hud_galley(ui, &n.to_string(), c), c)
    });
    let w = label.size().x + count.as_ref().map_or(0.0, |(g, _)| g.size().x + 8.0) + 24.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, theme::CONTROL_H), Sense::click());
    let hovered = resp.hovered();
    let (fill, ink) = match (on, hovered) {
        (true, _) => (t.invert_bg, t.invert_fg),
        (false, true) => (t.sunken, t.ink_strong),
        (false, false) => (Color32::TRANSPARENT, t.muted),
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    let text_rect = rect.shrink2(vec2(12.0, 0.0));
    let label_w = label.size().x;
    paint_left(ui, text_rect, label, ink);
    if let Some((g, c)) = count {
        paint_left(ui, text_rect.with_min_x(text_rect.left() + label_w + 8.0), g, c);
    }
    let name = match cell.count {
        Some((n, _)) => format!("{} {n}", cell.label),
        None => cell.label.to_string(),
    };
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, &name));
    if resp.on_hover_text(cell.tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
        *value = *opt;
        changed = true;
    }
    (rect, changed)
}

/// A slim square progress bar with a caption above it.
pub fn progress(ui: &mut Ui, fraction: Option<f32>, caption: &str, color: Color32, width: f32) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(width, theme::CONTROL_H), Sense::hover());
    let p = ui.painter();
    let bar = Rect::from_min_size(egui::pos2(rect.left(), rect.bottom() - 4.0), vec2(width, 4.0));
    p.rect_filled(bar, CornerRadius::ZERO, t.line);
    match fraction {
        Some(f) => {
            p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * f.clamp(0.0, 1.0), bar.height())), CornerRadius::ZERO, color);
        }
        None => {
            // Keep the segment moving through the track boundary without resetting visibly.
            let width = bar.width();
            let segment_width = width * 0.3;
            let x = (ui.input(|i| i.time) * 0.8).fract() as f32 * width;
            for left in [x, x - width] {
                let seg = Rect::from_min_size(egui::pos2(bar.left() + left, bar.top()), vec2(segment_width, bar.height())).intersect(bar);
                if seg.is_positive() {
                    p.rect_filled(seg, CornerRadius::ZERO, color);
                }
            }
            ui.ctx().request_repaint();
        }
    }
    p.text(egui::pos2(rect.left(), rect.top() + 9.0), egui::Align2::LEFT_CENTER, caption, egui::FontId::new(12.0, theme::medium()), t.muted);
    let resp = ui.interact(rect, ui.id().with(("progress", caption)), Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ProgressIndicator, true, caption));
}

pub fn human_bytes(n: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let n = n as f64;
    if n >= 1024.0 * MB {
        format!("{:.1} GB", n / (1024.0 * MB))
    } else if n >= MB {
        format!("{:.0} MB", n / MB)
    } else {
        format!("{:.0} KB", (n / 1024.0).max(1.0))
    }
}

/// Which drawn glyph a [`glyph_button`] shows (drawn, so no font needs the symbol).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Pause,
    Close,
}

/// A small outlined square button with a drawn glyph; inverts on hover.
pub fn glyph_button(ui: &mut Ui, glyph: Glyph, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(theme::CONTROL_H, theme::CONTROL_H), Sense::click());
    let hovered = resp.hovered();
    let p = ui.painter();
    if hovered {
        p.rect_filled(rect, CornerRadius::ZERO, t.invert_bg);
    } else {
        p.rect_stroke(rect, CornerRadius::ZERO, t.hairline(), egui::StrokeKind::Inside);
    }
    let c = if hovered { t.invert_fg } else { t.muted };
    let m = rect.center();
    match glyph {
        Glyph::Pause => {
            for dx in [-3.0, 3.0] {
                p.rect_filled(Rect::from_center_size(m + vec2(dx, 0.0), vec2(2.5, 10.0)), CornerRadius::ZERO, c);
            }
        }
        Glyph::Close => {
            let s = 4.5;
            p.line_segment([m + vec2(-s, -s), m + vec2(s, s)], Stroke::new(1.5, c));
            p.line_segment([m + vec2(-s, s), m + vec2(s, -s)], Stroke::new(1.5, c));
        }
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip));
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The "…" button that opens an app's menu: an outlined square like [`glyph_button`].
pub fn more_menu<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) {
    let t = Tokens::get(ui.ctx());
    let btn =
        egui::Button::new(RichText::new("…").size(15.0).color(t.muted)).fill(Color32::TRANSPARENT).min_size(vec2(theme::CONTROL_H, theme::CONTROL_H));
    egui::containers::menu::MenuButton::from_button(btn).ui(ui, |ui| {
        ui.set_min_width(200.0);
        add_contents(ui)
    });
}

/// ArtCraft's social accounts as a row of small monochrome icons (faint, brighter on hover).
/// In a right-to-left layout they still read left to right.
pub fn social_icons(ui: &mut Ui, size: f32) {
    let links: Vec<IconLink> = artcraft_launcher_core::catalog::SOCIAL
        .iter()
        .map(|(name, url)| IconLink { name, label: (*name).to_string(), tip: crate::i18n::fmt(tl!("ArtCraft on {name}"), &[("name", name)]), url })
        .collect();
    icon_links(ui, size, &links);
}

/// One logo that opens a page: `name` picks the picture, `label` is what a screen reader says,
/// `tip` is the hover text.
pub struct IconLink<'a> {
    pub name: &'a str,
    pub label: String,
    pub tip: String,
    pub url: &'a str,
}

/// The width [`icon_links`] takes for `count` logos `size` wide.
pub fn icon_links_width(count: usize, size: f32) -> f32 {
    let n = count as f32;
    n * (size + 2.0 * crate::theme::ICON_LINK_PAD) + (n - 1.0).max(0.0) * crate::theme::ICON_LINK_GAP
}

/// A row of logo links, given in reading order; each is `size` wide, spaced by the theme's
/// `ICON_LINK_PAD` and `ICON_LINK_GAP` wherever the row is. In a right-to-left parent they still
/// read in the order given.
pub fn icon_links(ui: &mut Ui, size: f32, links: &[IconLink<'_>]) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = crate::theme::ICON_LINK_GAP;
        for link in links_in_order(ui, links) {
            icon_link(ui, size, link);
        }
    });
}

fn links_in_order<'l, 'a>(ui: &Ui, links: &'l [IconLink<'a>]) -> Vec<&'l IconLink<'a>> {
    if ui.layout().prefer_right_to_left() { links.iter().rev().collect() } else { links.iter().collect() }
}

fn icon_link(ui: &mut Ui, size: f32, link: &IconLink<'_>) {
    let Some(icon) = crate::icons::social(link.name) else { return };
    let t = Tokens::get(ui.ctx());
    let pad = crate::theme::ICON_LINK_PAD;
    let (rect, resp) = ui.allocate_exact_size(vec2(size + 2.0 * pad, size + 2.0 * pad), Sense::click());
    let color = if resp.hovered() { t.ink_strong } else { t.faint };
    egui::Image::new(icon).tint(color).paint_at(ui, rect.shrink(pad));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, &link.label));
    let resp = resp.on_hover_text(&link.tip).on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(link.url));
    }
}

/// An app's name as the site sets it: Archivo, with "Craft" in the app's colour.
pub fn wordmark(ui: &mut Ui, app: &artcraft_launcher_core::CraftApp, size: f32) -> Response {
    wordmark_in(ui, app, size, true)
}

/// [`wordmark`], with "Craft" in the app's colour or, when `in_colour` is false, in grey.
pub fn wordmark_in(ui: &mut Ui, app: &artcraft_launcher_core::CraftApp, size: f32, in_colour: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let (prefix, craft) = app.name.strip_suffix("Craft").map_or((app.name, ""), |p| (p, "Craft"));
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat {
        font_id: egui::FontId::new(size, theme::display()),
        color,
        extra_letter_spacing: -0.03 * size,
        ..Default::default()
    };
    job.append(prefix, 0.0, format(t.ink_strong));
    job.append(craft, 0.0, format(if in_colour { t.app_ink(app) } else { t.muted }));
    ui.add(egui::Label::new(job).selectable(false))
}

/// A heading in the display face, tracked in like the site's (`tracking-[-0.02em]`).
pub fn heading(text: &str, size: f32, color: Color32) -> RichText {
    RichText::new(text).size(size).family(theme::display()).color(color).extra_letter_spacing(-0.02 * size)
}
