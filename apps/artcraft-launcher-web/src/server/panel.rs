//! Floating, nonmodal shell for the server-mode document controls.

use std::cell::Cell;
use std::rc::Rc;

use artcraft_launcher_ui_egui::{i18n, theme::Tokens};
use egui::Color32;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{CssStyleDeclaration, Document, Event, EventTarget, HtmlElement, KeyboardEvent, MouseEvent, PointerEvent, Window};

const LAYER_ID: &str = "artcraft-cloud-layer";
const PANEL_ID: &str = "artcraft-cloud-panel";
const HEADING_ID: &str = "artcraft-cloud-heading";
const POSITION_KEY: &str = "artcraft-launcher.cloud-control.position.v1";
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const CLOUD_DIAMETER: f64 = 54.0;
const EDGE: f64 = 12.0;
const GAP: f64 = 10.0;
const PANEL_WIDTH: f64 = 376.0;
const PANEL_HEIGHT: f64 = 550.0;
const DRAG_THRESHOLD: f64 = 6.0;
const KEY_STEP: f64 = 16.0;

pub(super) struct Panel {
    state: Rc<Surface>,
    body: HtmlElement,
    layer: HtmlElement,
    stylesheet: HtmlElement,
    listeners: Vec<Listener>,
}

impl Panel {
    pub(super) fn mount(document: &Document, window: &Window) -> Result<Self, JsValue> {
        if document.get_element_by_id(LAYER_ID).is_some() {
            return Err(JsValue::from_str("The cloud panel is already mounted"));
        }
        let page = document.body().ok_or_else(|| JsValue::from_str("The cloud panel requires a document body"))?;
        let head = document.head().ok_or_else(|| JsValue::from_str("The cloud panel requires a document head"))?;
        let stylesheet = html(document, "style", "")?;
        stylesheet.set_text_content(Some(&stylesheet_text()));
        let layer = html(document, "div", LAYER_ID)?;
        layer.set_id(LAYER_ID);
        let language = i18n::system_lang();
        let cloud = i18n::tr(language, "Cloud");
        let control = cloud_control(document, cloud)?;
        let panel = html(document, "section", "artcraft-cloud-panel")?;
        panel.set_id(PANEL_ID);
        panel.set_attribute("role", "region")?;
        panel.set_attribute("aria-labelledby", HEADING_ID)?;
        panel.set_hidden(true);

        let header = html(document, "header", "artcraft-cloud-header")?;
        let heading = html(document, "h2", "artcraft-cloud-heading")?;
        heading.set_id(HEADING_ID);
        heading.set_text_content(Some(cloud));
        let close = html(document, "button", "artcraft-cloud-close")?;
        close.set_attribute("type", "button")?;
        close.set_text_content(Some(i18n::tr(language, "Close")));
        header.append_child(&heading)?;
        header.append_child(&close)?;
        let body = html(document, "div", "artcraft-cloud-body")?;
        panel.append_child(&header)?;
        panel.append_child(&body)?;
        layer.append_child(&panel)?;
        layer.append_child(&control)?;

        let state = Rc::new(Surface {
            document: document.clone(),
            window: window.clone(),
            control,
            panel,
            close,
            position: Cell::new(read_position(window)),
            drag: Cell::new(None),
            suppress_click: Cell::new(false),
        });
        let mut shell = Self { state, body, layer, stylesheet, listeners: Vec::new() };
        shell.listen()?;
        shell.state.layout();
        head.append_child(&shell.stylesheet)?;
        page.append_child(&shell.layer)?;
        Ok(shell)
    }

    pub(super) fn body(&self) -> &HtmlElement {
        &self.body
    }

    pub(super) fn open(&self) {
        self.state.open();
    }

    pub(super) fn close(&self) {
        self.state.close();
    }

    fn listen(&mut self) -> Result<(), JsValue> {
        let state = &self.state;
        let control: &EventTarget = state.control.as_ref();
        for (name, handle) in [
            ("pointerdown", Surface::pointer_down as Handler),
            ("pointermove", Surface::pointer_move),
            ("pointerup", Surface::pointer_up),
            ("pointercancel", Surface::pointer_cancel),
            ("lostpointercapture", Surface::pointer_cancel),
            ("click", Surface::control_click),
            ("keydown", Surface::control_key),
        ] {
            self.listeners.push(Listener::add(control, name, state, handle)?);
        }
        self.listeners.push(Listener::add(state.close.as_ref(), "click", state, |state, event| {
            event.stop_propagation();
            state.close();
        })?);
        self.listeners.push(Listener::add(state.panel.as_ref(), "keydown", state, Surface::escape_key)?);
        self.listeners.push(Listener::add(state.window.as_ref(), "resize", state, |state, _| state.layout())?);
        if let Some(viewport) = state.window.visual_viewport() {
            for name in ["resize", "scroll"] {
                self.listeners.push(Listener::add(viewport.as_ref(), name, state, |state, _| state.layout())?);
            }
        }
        Ok(())
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.listeners.clear();
        self.state.cancel_drag();
        self.layer.remove();
        self.stylesheet.remove();
    }
}

type Handler = fn(&Surface, &Event);

struct Listener {
    target: EventTarget,
    name: &'static str,
    callback: Closure<dyn FnMut(Event)>,
}

impl Listener {
    fn add(target: &EventTarget, name: &'static str, state: &Rc<Surface>, handle: Handler) -> Result<Self, JsValue> {
        let state = Rc::downgrade(state);
        let callback = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            if let Some(state) = state.upgrade() {
                handle(&state, &event);
            }
        });
        target.add_event_listener_with_callback(name, callback.as_ref().unchecked_ref())?;
        Ok(Self { target: target.clone(), name, callback })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.target.remove_event_listener_with_callback(self.name, self.callback.as_ref().unchecked_ref());
    }
}

#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

const DEFAULT_POSITION: Point = Point { x: 1.0, y: 0.82 };

impl Point {
    fn pointer(event: &PointerEvent) -> Self {
        Self { x: f64::from(event.client_x()), y: f64::from(event.client_y()) }
    }

    fn difference(self, other: Self) -> Self {
        Self { x: self.x - other.x, y: self.y - other.y }
    }
}

#[derive(Clone, Copy)]
struct Drag {
    id: i32,
    origin: Point,
    previous: Point,
    start_position: Point,
    moved: bool,
}

struct Surface {
    document: Document,
    window: Window,
    control: HtmlElement,
    panel: HtmlElement,
    close: HtmlElement,
    position: Cell<Point>,
    drag: Cell<Option<Drag>>,
    suppress_click: Cell<bool>,
}

impl Surface {
    fn open(&self) {
        if !self.panel.hidden() {
            return;
        }
        self.layout();
        self.panel.set_hidden(false);
        let _ = self.control.set_attribute("aria-expanded", "true");
        let _ = self.close.focus();
    }

    fn close(&self) {
        if self.panel.hidden() {
            return;
        }
        let restore_focus = self.document.active_element().is_some_and(|active| self.panel.contains(Some(active.as_ref())));
        self.panel.set_hidden(true);
        let _ = self.control.set_attribute("aria-expanded", "false");
        if restore_focus {
            let _ = self.control.focus();
        }
    }

    fn control_click(&self, event: &Event) {
        event.stop_propagation();
        let pointer_click = event.dyn_ref::<PointerEvent>().is_some_and(|click| click.pointer_id() >= 0)
            || event.dyn_ref::<MouseEvent>().is_some_and(|click| click.detail() != 0);
        if self.suppress_click.replace(false) && pointer_click {
            event.prevent_default();
            return;
        }
        if self.panel.hidden() { self.open() } else { self.close() }
    }

    fn pointer_down(&self, event: &Event) {
        let Some(pointer) = event.dyn_ref::<PointerEvent>() else { return };
        if self.drag.get().is_some() || !pointer.is_primary() || pointer.button() != 0 {
            return;
        }
        self.suppress_click.set(false);
        let id = pointer.pointer_id();
        if self.control.set_pointer_capture(id).is_err() {
            return;
        }
        let origin = Point::pointer(pointer);
        self.drag.set(Some(Drag { id, origin, previous: origin, start_position: self.position.get(), moved: false }));
    }

    fn pointer_move(&self, event: &Event) {
        if let Some(pointer) = event.dyn_ref::<PointerEvent>()
            && self.drag_to(pointer)
        {
            event.prevent_default();
        }
    }

    fn drag_to(&self, event: &PointerEvent) -> bool {
        let Some(mut drag) = self.drag.get().filter(|drag| drag.id == event.pointer_id()) else { return false };
        let cursor = Point::pointer(event);
        let distance = cursor.difference(drag.origin);
        if !drag.moved && distance.x.hypot(distance.y) < DRAG_THRESHOLD {
            return false;
        }
        if !drag.moved {
            let _ = self.control.set_attribute("data-dragging", "");
        }
        self.move_by(cursor.difference(drag.previous));
        drag.moved = true;
        drag.previous = cursor;
        self.drag.set(Some(drag));
        true
    }

    fn pointer_up(&self, event: &Event) {
        let Some(pointer) = event.dyn_ref::<PointerEvent>() else { return };
        if !self.drag.get().is_some_and(|drag| drag.id == pointer.pointer_id()) {
            return;
        }
        self.drag_to(pointer);
        if let Some(drag) = self.drag.take() {
            self.release_capture(drag.id);
            if drag.moved {
                self.suppress_click.set(true);
                self.persist_position();
            }
        }
    }

    fn pointer_cancel(&self, event: &Event) {
        let Some(pointer) = event.dyn_ref::<PointerEvent>() else { return };
        if self.drag.get().is_some_and(|drag| drag.id == pointer.pointer_id()) {
            self.cancel_drag();
        }
    }

    fn cancel_drag(&self) {
        if let Some(drag) = self.drag.take() {
            self.release_capture(drag.id);
            self.suppress_click.set(true);
            self.position.set(drag.start_position);
            self.layout();
        }
    }

    fn release_capture(&self, id: i32) {
        let _ = self.control.remove_attribute("data-dragging");
        if self.control.has_pointer_capture(id) {
            let _ = self.control.release_pointer_capture(id);
        }
    }

    fn control_key(&self, event: &Event) {
        let Some(key) = usable_key(event) else { return };
        let (x, y) = match key.key().as_str() {
            "ArrowLeft" => (-KEY_STEP, 0.0),
            "ArrowRight" => (KEY_STEP, 0.0),
            "ArrowUp" => (0.0, -KEY_STEP),
            "ArrowDown" => (0.0, KEY_STEP),
            "Escape" => return self.escape_key(event),
            "Enter" | " " => return self.suppress_click.set(false),
            _ => return,
        };
        event.prevent_default();
        event.stop_propagation();
        if self.drag.get().is_none() {
            self.move_by(Point { x, y });
            self.persist_position();
        }
    }

    fn escape_key(&self, event: &Event) {
        if !usable_key(event).is_some_and(|key| key.key() == "Escape") {
            return;
        }
        if self.drag.get().is_some() {
            self.cancel_drag();
        } else if !self.panel.hidden() {
            self.close();
        } else {
            return;
        }
        event.prevent_default();
        event.stop_propagation();
    }

    fn move_by(&self, delta: Point) {
        let frame = Frame::measure(&self.window);
        let travel = frame.travel();
        let current = self.position.get();
        self.position.set(Point { x: shift(current.x, delta.x, travel.x), y: shift(current.y, delta.y, travel.y) });
        self.place(frame);
    }

    fn persist_position(&self) {
        if let Ok(Some(storage)) = self.window.local_storage() {
            let point = self.position.get();
            let _ = storage.set_item(POSITION_KEY, &format!("{:.6},{:.6}", point.x, point.y));
        }
    }

    fn layout(&self) {
        self.place(Frame::measure(&self.window));
    }

    fn place(&self, frame: Frame) {
        let travel = frame.travel();
        let position = self.position.get();
        let anchor = Point { x: frame.left() + position.x * travel.x, y: frame.top() + position.y * travel.y };
        let style = self.control.style();
        set_px(&style, "left", anchor.x);
        set_px(&style, "top", anchor.y);
        set_px(&style, "width", frame.diameter);
        set_px(&style, "height", frame.diameter);

        let panel = frame.panel(anchor);
        let style = self.panel.style();
        set_px(&style, "left", panel.left);
        set_px(&style, "top", panel.top);
        set_px(&style, "width", panel.width);
        set_px(&style, "max-height", panel.height);
        let _ = style.set_property("transform", if panel.from_bottom { "translateY(-100%)" } else { "none" });
    }
}

fn usable_key(event: &Event) -> Option<&KeyboardEvent> {
    event.dyn_ref::<KeyboardEvent>().filter(|key| !key.default_prevented() && !key.is_composing())
}

fn shift(value: f64, pixels: f64, travel: f64) -> f64 {
    if travel > 0.0 { (value + pixels / travel).clamp(0.0, 1.0) } else { value }
}

fn read_position(window: &Window) -> Point {
    let position = window.local_storage().ok().flatten().and_then(|storage| storage.get_item(POSITION_KEY).ok().flatten());
    let Some(value) = position.filter(|value| value.len() <= 96) else { return DEFAULT_POSITION };
    let Some((x, y)) = value.split_once(',') else { return DEFAULT_POSITION };
    match (x.parse::<f64>(), y.parse::<f64>()) {
        (Ok(x), Ok(y)) if x.is_finite() && y.is_finite() => Point { x: x.clamp(0.0, 1.0), y: y.clamp(0.0, 1.0) },
        _ => DEFAULT_POSITION,
    }
}

struct Frame {
    origin: Point,
    width: f64,
    height: f64,
    edge: f64,
    diameter: f64,
}

struct Placement {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
    from_bottom: bool,
}

impl Frame {
    fn measure(window: &Window) -> Self {
        let (origin, width, height) = if let Some(viewport) = window.visual_viewport() {
            (Point { x: finite(viewport.offset_left()), y: finite(viewport.offset_top()) }, viewport.width(), viewport.height())
        } else {
            let width = window.inner_width().ok().and_then(|value| value.as_f64()).unwrap_or(1.0);
            let height = window.inner_height().ok().and_then(|value| value.as_f64()).unwrap_or(1.0);
            (Point { x: 0.0, y: 0.0 }, width, height)
        };
        let width = finite(width).max(1.0);
        let height = finite(height).max(1.0);
        let shortest = width.min(height);
        let edge = EDGE.min((shortest - CLOUD_DIAMETER).max(0.0) / 2.0);
        let diameter = CLOUD_DIAMETER.min(shortest - 2.0 * edge);
        Self { origin, width, height, edge, diameter }
    }

    fn left(&self) -> f64 {
        self.origin.x + self.edge
    }
    fn top(&self) -> f64 {
        self.origin.y + self.edge
    }
    fn right(&self) -> f64 {
        self.origin.x + self.width - self.edge
    }
    fn bottom(&self) -> f64 {
        self.origin.y + self.height - self.edge
    }

    fn travel(&self) -> Point {
        Point { x: (self.width - 2.0 * self.edge - self.diameter).max(0.0), y: (self.height - 2.0 * self.edge - self.diameter).max(0.0) }
    }

    fn panel(&self, anchor: Point) -> Placement {
        let width = PANEL_WIDTH.min(self.width - 2.0 * self.edge);
        let left_space = anchor.x - GAP - self.left();
        let right_space = self.right() - (anchor.x + self.diameter + GAP);
        let above_space = anchor.y - GAP - self.top();
        let below_space = self.bottom() - (anchor.y + self.diameter + GAP);
        let horizontal = left_space.max(right_space);
        let vertical = above_space.max(below_space);
        if horizontal >= width || (horizontal > 0.0 && vertical < 144.0 && horizontal >= vertical) {
            return self.side_panel(anchor, left_space, right_space);
        }
        if vertical > 0.0 {
            let from_bottom = above_space > below_space;
            let top = if from_bottom { anchor.y - GAP } else { anchor.y + self.diameter + GAP };
            let left = (anchor.x + (self.diameter - width) / 2.0).max(self.left()).min(self.right() - width);
            return Placement { left, top, width, height: PANEL_HEIGHT.min(vertical), from_bottom };
        }
        // There is no separate panel area in a viewport smaller than the control and its gap.
        // The control keeps the higher stacking order so the panel can always be dismissed.
        Placement { left: self.left(), top: self.top(), width, height: self.height - 2.0 * self.edge, from_bottom: false }
    }

    fn side_panel(&self, anchor: Point, left_space: f64, right_space: f64) -> Placement {
        let width = PANEL_WIDTH.min(left_space.max(right_space));
        let left = if left_space > right_space { anchor.x - GAP - width } else { anchor.x + self.diameter + GAP };
        let from_bottom = anchor.y + self.diameter / 2.0 >= self.origin.y + self.height / 2.0;
        let top = if from_bottom { anchor.y + self.diameter } else { anchor.y };
        let height = if from_bottom { top - self.top() } else { self.bottom() - top };
        Placement { left, top, width, height: PANEL_HEIGHT.min(height), from_bottom }
    }
}

fn finite(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn set_px(style: &CssStyleDeclaration, name: &str, value: f64) {
    let _ = style.set_property(name, &format!("{value:.2}px"));
}

fn html(document: &Document, tag: &str, class: &str) -> Result<HtmlElement, JsValue> {
    let element = document.create_element(tag)?;
    if !class.is_empty() {
        element.set_class_name(class);
    }
    element.dyn_into().map_err(Into::into)
}

fn cloud_control(document: &Document, label: &str) -> Result<HtmlElement, JsValue> {
    let control = html(document, "button", "artcraft-cloud-control")?;
    control.set_attribute("type", "button")?;
    control.set_attribute("aria-label", label)?;
    control.set_attribute("title", label)?;
    control.set_attribute("aria-controls", PANEL_ID)?;
    control.set_attribute("aria-expanded", "false")?;
    control.set_attribute("aria-keyshortcuts", "ArrowLeft ArrowRight ArrowUp ArrowDown Escape")?;
    let icon = document.create_element_ns(Some(SVG_NAMESPACE), "svg")?;
    for (name, value) in [("viewBox", "0 0 24 24"), ("aria-hidden", "true"), ("focusable", "false")] {
        icon.set_attribute(name, value)?;
    }
    let outline = document.create_element_ns(Some(SVG_NAMESPACE), "path")?;
    for (name, value) in [
        ("d", "M7 18.5h10.2a4 4 0 0 0 .5-8 5.8 5.8 0 0 0-11.1 1.3A3.4 3.4 0 0 0 7 18.5Z"),
        ("fill", "none"),
        ("stroke", "currentColor"),
        ("stroke-width", "1.6"),
        ("stroke-linejoin", "round"),
    ] {
        outline.set_attribute(name, value)?;
    }
    icon.append_child(&outline)?;
    control.append_child(&icon)?;
    Ok(control)
}

fn stylesheet_text() -> String {
    let light = token_rule(Tokens::LIGHT);
    let dark = token_rule(Tokens::DARK);
    format!("{}\n{light}\n@media (prefers-color-scheme: dark) {{ {dark} }}", include_str!("panel.css"))
}

fn token_rule(tokens: Tokens) -> String {
    let colors = [
        ("bg", tokens.bg),
        ("sunken", tokens.sunken),
        ("ink", tokens.ink),
        ("ink-strong", tokens.ink_strong),
        ("muted", tokens.muted),
        ("line", tokens.line),
        ("line-strong", tokens.line_strong),
        ("accent", tokens.accent),
        ("accent-ink", tokens.accent_ink),
        ("invert-bg", tokens.invert_bg),
        ("invert-fg", tokens.invert_fg),
        ("good", tokens.good),
        ("warn", tokens.warn),
        ("bad", tokens.bad),
    ];
    let variables: String = colors.into_iter().map(|(name, value)| format!("--ac-cloud-{name}:{};", color(value))).collect();
    format!(".artcraft-cloud-layer {{ color-scheme: {}; {variables} }}", if tokens.dark { "dark" } else { "light" })
}

fn color(value: Color32) -> String {
    let [red, green, blue, alpha] = value.to_srgba_unmultiplied();
    format!("rgba({red},{green},{blue},{:.3})", f64::from(alpha) / 255.0)
}
