//! A native document workspace inside the existing launcher. Rendering and file
//! dialogs run off the UI thread; only completed, durably saved revisions commit.
use artcraft_launcher_core::document::{Choice, Choices, Document, Graph, Operation, Program};
use artcraft_launcher_native::document::{self as engine, Engines, Rendered};
use artcraft_launcher_ui_egui::{
    i18n::t,
    theme::Tokens,
    tl,
    widgets::{self, Kind},
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

enum Job {
    Open(PathBuf),
    Edit(u64, Document),
    PickFiles(bool),
    Save(Document),
    Export(Vec<u8>),
}
enum Event {
    Ready(Engines),
    Opened(PathBuf, Result<(Document, Rendered), String>),
    Edited(u64, Result<(Document, Rendered), String>),
    Files(Vec<PathBuf>, bool),
    Notice(Result<String, String>),
}

struct Tab {
    id: u64,
    key: String,
    doc: Document,
    texture: Option<egui::TextureHandle>,
    png: Vec<u8>,
    program: Program,
    selected: Option<u64>,
    draft: Graph,
    status: String,
}

pub struct Workspace {
    pub visible: bool,
    jobs: Sender<Job>,
    events: Receiver<Event>,
    engines: Option<Engines>,
    tabs: Vec<Tab>,
    active: usize,
    serial: u64,
    busy: bool,
    choices: Choices,
    pending: VecDeque<PathBuf>,
    binder_key: Option<String>,
    node_view: bool,
    error: Option<String>,
    notice: String,
    recovery: PathBuf,
}

impl Workspace {
    pub fn new(root: PathBuf, files: Vec<PathBuf>, visible: bool) -> Self {
        let (jobs, rx) = channel();
        let (tx, events) = channel();
        let recovery = root.join("recovery");
        let worker_root = root.clone();
        std::thread::spawn(move || {
            let engines = engine::discover();
            let _ = tx.send(Event::Ready(engines.clone()));
            let cache = worker_root.join("cache");
            for job in rx {
                let event = match job {
                    Job::Open(path) => {
                        let result = (|| {
                            let doc = if path.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("artcraftdoc")) {
                                engine::load(&path)?
                            } else {
                                engine::open_source(&path, &engines)?
                            };
                            let rendered = engine::evaluate(&doc, &engines, &cache)?;
                            Ok((doc, rendered))
                        })();
                        Event::Opened(path, result)
                    }
                    Job::Edit(id, doc) => {
                        let result = engine::evaluate(&doc, &engines, &cache).and_then(|rendered| {
                            engine::save(&worker_root.join("recovery").join(format!("{id}.artcraftdoc")), &doc)?;
                            Ok((doc, rendered))
                        });
                        Event::Edited(id, result)
                    }
                    Job::PickFiles(binder) => {
                        let files = rfd::FileDialog::new()
                            .add_filter("Images / ArtCraft documents", &["png", "jpg", "jpeg", "artcraftdoc"])
                            .pick_files()
                            .unwrap_or_default();
                        Event::Files(files, binder)
                    }
                    Job::Save(doc) => {
                        let path = rfd::FileDialog::new()
                            .add_filter("ArtCraft document", &["artcraftdoc"])
                            .set_file_name(format!("{}.artcraftdoc", doc.name))
                            .save_file();
                        Event::Notice(match path {
                            Some(path) => engine::save(&path.with_extension("artcraftdoc"), &doc).map(|()| "Document and history saved".into()),
                            None => Ok(String::new()),
                        })
                    }
                    Job::Export(png) => {
                        let path = rfd::FileDialog::new().add_filter("PNG preview", &["png"]).set_file_name("Preview.png").save_file();
                        Event::Notice(match path {
                            Some(path) if path.with_extension("png").exists() => {
                                Err("Choose a new filename: preview export never overwrites an existing image".into())
                            }
                            Some(path) => engine::atomic_write(&path.with_extension("png"), &png)
                                .map(|()| "PNG preview exported; save the ArtCraft document to retain edit history".into()),
                            None => Ok(String::new()),
                        })
                    }
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
        Self {
            visible,
            jobs,
            events,
            engines: None,
            tabs: Vec::new(),
            active: 0,
            serial: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
            busy: false,
            choices: Choices::default(),
            pending: files.into(),
            binder_key: None,
            node_view: false,
            error: None,
            notice: String::new(),
            recovery,
        }
    }

    fn send(&mut self, job: Job) {
        if self.jobs.send(job).is_ok() {
            self.busy = true;
        } else {
            self.error = Some("Document worker stopped".into());
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        for event in self.events.try_iter().collect::<Vec<_>>() {
            match event {
                Event::Ready(engines) => self.engines = Some(engines),
                Event::Opened(path, result) => {
                    self.busy = false;
                    match result {
                        Ok((doc, rendered)) => {
                            self.serial = self.serial.saturating_add(1);
                            let key = self.binder_key.clone().unwrap_or_else(|| path.to_string_lossy().into_owned());
                            let draft = doc.graph().cloned().unwrap_or_default();
                            let compatible = self.compatible();
                            let program = match self.choices.route(&key, &compatible) {
                                Choice::Direct(p) => p,
                                _ => Program::PhotoCraft,
                            };
                            let mut tab = Tab {
                                id: self.serial,
                                key,
                                doc,
                                texture: None,
                                png: Vec::new(),
                                program,
                                selected: None,
                                draft,
                                status: String::new(),
                            };
                            install_render(ctx, &mut tab, rendered);
                            let existing_binder = tab.key.starts_with("binder:") && self.tabs.iter().any(|other| other.key == tab.key);
                            self.tabs.push(tab);
                            if !existing_binder {
                                self.active = self.tabs.len().saturating_sub(1);
                            }
                            self.visible = true;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                Event::Edited(id, result) => {
                    self.busy = false;
                    match result {
                        Ok((doc, rendered)) => {
                            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                                tab.draft = doc.graph().cloned().unwrap_or_default();
                                tab.doc = doc;
                                install_render(ctx, tab, rendered);
                            }
                        }
                        Err(e) => {
                            self.error = Some(e);
                            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                                tab.draft = tab.doc.graph().cloned().unwrap_or_default();
                            }
                        }
                    }
                }
                Event::Files(files, binder) => {
                    self.busy = false;
                    self.binder_key = if binder && !files.is_empty() {
                        self.serial = self.serial.saturating_add(1);
                        Some(format!("binder:{}", self.serial))
                    } else {
                        None
                    };
                    self.pending.extend(files);
                }
                Event::Notice(result) => {
                    self.busy = false;
                    match result {
                        Ok(text) => self.notice = text,
                        Err(e) => self.error = Some(e),
                    }
                }
            }
        }
        let waiting_choice = self.tabs.get(self.active).is_some_and(|tab| self.choices.route(&tab.key, &self.compatible()) == Choice::Ask);
        if self.engines.is_some()
            && !self.busy
            && !waiting_choice
            && let Some(path) = self.pending.pop_front()
        {
            let canonical = path.canonicalize().unwrap_or(path);
            if let Some(index) = self.tabs.iter().position(|tab| tab.key == canonical.to_string_lossy()) {
                self.active = index;
            } else {
                self.send(Job::Open(canonical));
            }
        }
        if self.busy || self.engines.is_none() || !self.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn compatible(&self) -> Vec<Program> {
        self.engines.as_ref().map(|e| e.keys().copied().collect()).unwrap_or_default()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let tokens = Tokens::get(&ctx);
        self.poll(&ctx);
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>());
        if !dropped.is_empty() {
            if self.busy || !self.pending.is_empty() {
                self.error = Some(tl!("Wait for the current operation before dropping another file").into());
            } else {
                self.binder_key = None;
                self.pending.extend(dropped);
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.heading(tl!("Document workspace"));
            ui.add_enabled_ui(!self.busy, |ui| {
                if widgets::button(ui, tl!("Open files"), Kind::Primary, 0.).clicked() {
                    self.send(Job::PickFiles(false));
                }
                if widgets::button(ui, tl!("Open pages as binder"), Kind::Secondary, 0.).clicked() {
                    self.send(Job::PickFiles(true));
                }
                if widgets::button(ui, tl!("Recovery folder"), Kind::Secondary, 0.).clicked()
                    && let Err(e) = std::fs::create_dir_all(&self.recovery)
                        .map_err(|e| e.to_string())
                        .and_then(|()| artcraft_launcher_native::process::open(self.recovery.as_os_str()))
                {
                    self.error = Some(e);
                }
            });
            ui.toggle_value(&mut self.node_view, tl!("Node graph"));
            if self.busy {
                ui.spinner();
                ui.label(tl!("Evaluating; last completed result shown"));
            }
        });
        ui.label(
            egui::RichText::new(tl!("Experimental · four replayable tools · 8-bit sRGB PNG/JPEG · native app menus are not captured"))
                .color(tokens.muted),
        );
        if !self.notice.is_empty() {
            ui.label(t(&self.notice));
        }
        if let Some(error) = self.error.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(tokens.bad, error);
                if ui.button(tl!("Dismiss")).clicked() {
                    self.error = None;
                }
            });
        }
        ui.separator();
        let mut close = None;
        egui::ScrollArea::horizontal().id_salt("document-tabs").max_height(32.).show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, tab) in self.tabs.iter().enumerate() {
                    if ui.selectable_label(self.active == i, &tab.doc.name).clicked() {
                        self.active = i;
                    }
                    if ui.add_enabled(!self.busy, egui::Button::new("×")).on_hover_text(tl!("Close document; recovery copy is retained")).clicked() {
                        close = Some(i);
                    }
                }
            });
        });
        if let Some(index) = close {
            if let Some(tab) = self.tabs.get(index)
                && self.tabs.iter().filter(|other| other.key == tab.key).count() == 1
                && self.pending.is_empty()
            {
                self.choices.close(&tab.key);
            }
            self.tabs.remove(index);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
        if self.tabs.is_empty() {
            ui.add_space(35.);
            ui.heading(tl!("One document. An editable recipe."));
            ui.label(tl!("Open an image, choose your first toolset, then add operations in any program column."));
            ui.label(tl!("Replacing an earlier operation reevaluates its dependants. Save an ArtCraft document to keep the source and history."));
            ui.label(tl!("Binder opening applies one toolset choice to the selected image pages. PDF editing is not supported in this build."));
            return;
        }
        self.chooser(&ctx);
        if let Some(key) = self.tabs.get(self.active).filter(|tab| tab.key.starts_with("binder:")).map(|tab| tab.key.clone()) {
            let pages = self.tabs.iter().enumerate().filter(|(_, tab)| tab.key == key).map(|(i, tab)| (i, tab.doc.name.clone())).collect::<Vec<_>>();
            egui::ComboBox::from_id_salt("binder-page")
                .selected_text(artcraft_launcher_ui_egui::i18n::fmt(
                    tl!("Page {page} of {count}"),
                    &[
                        ("page", &(pages.iter().position(|(i, _)| *i == self.active).unwrap_or(0) + 1).to_string()),
                        ("count", &pages.len().to_string()),
                    ],
                ))
                .show_ui(ui, |ui| {
                    for (page, (index, name)) in pages.iter().enumerate() {
                        ui.selectable_value(&mut self.active, *index, format!("{} · {name}", page + 1));
                    }
                });
        }
        let compatible = self.compatible();
        let waiting = self.tabs.get(self.active).is_some_and(|tab| self.choices.route(&tab.key, &compatible) == Choice::Ask);
        let mut action = None;
        let mut save = None;
        let mut export = None;
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        ui.add_enabled_ui(!self.busy && !waiting, |ui| {
            ui.horizontal(|ui| {
                for p in &compatible {
                    ui.selectable_value(&mut tab.program, *p, p.name());
                }
                if ui.add_enabled(tab.doc.cursor > 0, egui::Button::new(tl!("Undo"))).clicked() {
                    let mut d = tab.doc.clone();
                    if d.undo().is_ok() {
                        action = Some(d);
                    }
                }
                if ui.add_enabled(tab.doc.cursor + 1 < tab.doc.revisions.len(), egui::Button::new(tl!("Redo"))).clicked() {
                    let mut d = tab.doc.clone();
                    if d.redo().is_ok() {
                        action = Some(d);
                    }
                }
                if ui.button(tl!("Save document")).clicked() {
                    save = Some(tab.doc.clone());
                }
                if ui
                    .button(tl!("Export flat PNG…"))
                    .on_hover_text(tl!("PNG keeps the visible image only; save the document for editable history"))
                    .clicked()
                {
                    export = Some(tab.png.clone());
                }
            });
        });
        let available = ui.available_size();
        let viewport_height = (available.y * 0.48).clamp(140., 420.);
        let (viewport, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), viewport_height), egui::Sense::hover());
        ui.painter().rect_filled(viewport, 0., tokens.sunken);
        if let Some(texture) = &tab.texture {
            let size = texture.size_vec2();
            let scale = ((viewport.width() - 20.) / size.x).min((viewport.height() - 20.) / size.y).min(2.);
            egui::Image::new(texture).paint_at(ui, egui::Rect::from_center_size(viewport.center(), size * scale));
        }
        ui.label(&tab.status);
        let mut apply = false;
        ui.add_enabled_ui(!self.busy && !waiting, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(tl!("Add operation:"));
                let mut op = None;
                match tab.program {
                    Program::PhotoCraft => {
                        if ui.button(tl!("Hue / Saturation")).clicked() {
                            op = Some(Operation::HueSaturation { hue: 25., saturation: 10. });
                        }
                        if ui.button(tl!("Levels")).clicked() {
                            op = Some(Operation::Levels { black: 10., white: 245., gamma: 1.15 });
                        }
                    }
                    Program::VectorCraft => {
                        if ui.button(tl!("Rectangle")).clicked() {
                            op = Some(Operation::Rectangle { x: 80., y: 80., width: 100., height: 60., color: [220, 110, 45] });
                        }
                    }
                    Program::LightCraft => {
                        if ui.button(tl!("Exposure")).clicked() {
                            op = Some(Operation::Exposure { stops: 0.3 });
                        }
                    }
                }
                if let Some(op) = op {
                    match tab.draft.append(op) {
                        Ok(id) => {
                            tab.selected = Some(id);
                            apply = true;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            });
            egui::ScrollArea::vertical().max_height((available.y - viewport_height - 110.).max(180.)).show(ui, |ui| {
                if self.node_view {
                    graph_view(ui, tab);
                } else {
                    columns(ui, tab);
                }
                if let Some(id) = tab.selected {
                    ui.separator();
                    let choices = tab.draft.nodes.iter().map(|n| (n.id, n.operation.label())).collect::<Vec<_>>();
                    if let Some(node) = tab.draft.node_mut(id) {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong(format!("#{} {}", node.id, t(node.operation.label())));
                            ui.checkbox(&mut node.enabled, tl!("Enabled"));
                            egui::ComboBox::from_id_salt("operation-input")
                                .selected_text(node.input.map(|i| format!("#{i}")).unwrap_or_else(|| tl!("Source").into()))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut node.input, None, tl!("Source"));
                                    for (other, label) in &choices {
                                        if *other != id {
                                            ui.selectable_value(&mut node.input, Some(*other), format!("#{other} {}", t(label)));
                                        }
                                    }
                                });
                            if matches!(node.operation, Operation::HueSaturation { .. }) && ui.button(tl!("Replace with Levels")).clicked() {
                                node.operation = Operation::Levels { black: 10., white: 245., gamma: 1.15 };
                            }
                            if matches!(node.operation, Operation::Levels { .. }) && ui.button(tl!("Replace with Hue / Saturation")).clicked() {
                                node.operation = Operation::HueSaturation { hue: 0., saturation: 0. };
                            }
                        });
                        parameters(ui, &mut node.operation);
                    }
                    ui.horizontal(|ui| {
                        if ui.button(tl!("Use as output")).clicked() {
                            tab.draft.output = Some(id);
                        }
                        if widgets::button(ui, tl!("Apply edit"), Kind::Primary, 0.).clicked() {
                            apply = true;
                        }
                        if ui.button(tl!("Discard pending edit")).clicked() {
                            tab.draft = tab.doc.graph().cloned().unwrap_or_default();
                        }
                        if tab.doc.graph().is_ok_and(|g| g != &tab.draft) {
                            ui.label(tl!("Pending edit; preview shows the committed revision"));
                        }
                    });
                }
            });
        });
        if apply {
            let mut doc = tab.doc.clone();
            match doc.commit(doc.generation, tab.draft.clone()) {
                Ok(()) => action = Some(doc),
                Err(e) => self.error = Some(e),
            }
        }
        let id = tab.id;
        if let Some(doc) = action {
            self.send(Job::Edit(id, doc));
        } else if let Some(doc) = save {
            self.send(Job::Save(doc));
        } else if let Some(png) = export {
            self.send(Job::Export(png));
        }
    }

    fn chooser(&mut self, ctx: &egui::Context) {
        let compatible = self.compatible();
        let Some(tab) = self.tabs.get(self.active) else { return };
        match self.choices.route(&tab.key, &compatible) {
            Choice::Direct(p) => {
                let key = tab.key.clone();
                self.choices.choose(key, p);
                return;
            }
            Choice::Unavailable => {
                self.error = Some("No compatible Craft CLI engines are installed".into());
                return;
            }
            Choice::Ask => {}
        }
        let key = tab.key.clone();
        let name = tab.doc.name.clone();
        let mut choice = None;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("document-choice")).show(ctx, |ui| {
            ui.heading(tl!("Choose the first toolset"));
            ui.label(name);
            if key.starts_with("binder:") {
                ui.label(tl!("This choice applies to all pages in this binder."));
            } else {
                ui.label(tl!("This choice applies only to this open file."));
            }
            for p in compatible {
                if widgets::button(ui, p.name(), Kind::Primary, 220.).clicked() {
                    choice = Some(p);
                }
            }
            if ui.button(tl!("Cancel")).clicked() {
                cancel = true;
            }
        });
        if let Some(p) = choice {
            self.choices.choose(key.clone(), p);
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.program = p;
            }
        }
        if cancel {
            self.tabs.remove(self.active);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
            if key.starts_with("binder:") {
                self.pending.clear();
                self.binder_key = None;
            }
        }
    }
}

fn install_render(ctx: &egui::Context, tab: &mut Tab, result: Rendered) {
    if let Ok(image) = image::load_from_memory(&result.png) {
        let rgba = image.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        tab.texture = Some(ctx.load_texture(
            format!("document-{}", tab.id),
            egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
            egui::TextureOptions::LINEAR,
        ));
    }
    tab.status = artcraft_launcher_ui_egui::i18n::fmt(
        tl!("{ms} ms · {evaluated} evaluated · {cached} cached · revision {revision}"),
        &[
            ("ms", &result.elapsed_ms.to_string()),
            ("evaluated", &result.evaluated.to_string()),
            ("cached", &result.cached.to_string()),
            ("revision", &tab.doc.generation.to_string()),
        ],
    );
    tab.png = result.png;
}
fn columns(ui: &mut egui::Ui, tab: &mut Tab) {
    ui.columns(3, |columns| {
        for (column, p) in columns.iter_mut().zip(Program::ALL) {
            column.strong(p.name());
            column.separator();
            for node in tab.draft.nodes.iter().filter(|n| n.operation.program() == p) {
                let label = format!("{} #{} {}", if node.enabled { "●" } else { "○" }, node.id, t(node.operation.label()));
                if column.selectable_label(tab.selected == Some(node.id), label).clicked() {
                    tab.selected = Some(node.id);
                    tab.program = p;
                }
            }
        }
    });
}
fn graph_view(ui: &mut egui::Ui, tab: &mut Tab) {
    let tokens = Tokens::get(ui.ctx());
    let height = (tab.draft.nodes.len() as f32 * 50. + 40.).max(100.);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let width = (rect.width() / 3. - 14.).max(90.);
    let mut positions = std::collections::BTreeMap::new();
    let source = egui::pos2(rect.center().x, rect.top() + 12.);
    ui.painter().text(source, egui::Align2::CENTER_CENTER, tl!("Source"), egui::FontId::proportional(13.), tokens.muted);
    for (row, node) in tab.draft.nodes.iter().enumerate() {
        let col = Program::ALL.iter().position(|p| *p == node.operation.program()).unwrap_or(0);
        let r = egui::Rect::from_min_size(
            egui::pos2(rect.left() + col as f32 * (width + 14.), rect.top() + 32. + row as f32 * 50.),
            egui::vec2(width, 36.),
        );
        positions.insert(node.id, r);
    }
    for node in &tab.draft.nodes {
        if let Some(r) = positions.get(&node.id) {
            let from = node.input.and_then(|id| positions.get(&id)).map(|r| r.center_bottom()).unwrap_or(source);
            ui.painter().arrow(from, r.center_top() - from, egui::Stroke::new(1.5, tokens.line_strong));
            ui.painter().rect_filled(*r, 0., if tab.selected == Some(node.id) { tokens.accent } else { tokens.sunken });
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                format!("#{} {}{}", node.id, t(node.operation.label()), if node.enabled { "" } else { " ○" }),
                egui::FontId::proportional(12.),
                if tab.selected == Some(node.id) { tokens.invert_fg } else { tokens.ink },
            );
            let response = ui.interact(*r, ui.id().with(node.id), egui::Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    true,
                    tab.selected == Some(node.id),
                    format!("#{} {}", node.id, t(node.operation.label())),
                )
            });
            if response.clicked() {
                tab.selected = Some(node.id);
                tab.program = node.operation.program();
            }
        }
    }
}
fn parameters(ui: &mut egui::Ui, op: &mut Operation) {
    ui.horizontal_wrapped(|ui| match op {
        Operation::HueSaturation { hue, saturation } => {
            ui.add(egui::Slider::new(hue, -180. ..=180.).text(tl!("Hue")));
            ui.add(egui::Slider::new(saturation, -100. ..=100.).text(tl!("Saturation")));
        }
        Operation::Levels { black, white, gamma } => {
            ui.add(egui::Slider::new(black, 0. ..=253.).text(tl!("Black")));
            ui.add(egui::Slider::new(white, 2. ..=255.).text(tl!("White")));
            ui.add(egui::Slider::new(gamma, 0.1..=4.).text(tl!("Gamma")));
        }
        Operation::Rectangle { x, y, width, height, color } => {
            for (label, value) in [("X", x), ("Y", y), ("Width", width), ("Height", height)] {
                ui.label(t(label));
                ui.add(egui::DragValue::new(value).speed(1.));
            }
            ui.color_edit_button_srgb(color);
        }
        Operation::Exposure { stops } => {
            ui.add(egui::Slider::new(stops, -5. ..=5.).text(tl!("Exposure")));
        }
    });
}

pub fn data_root() -> PathBuf {
    artcraft_launcher_native::paths::Paths::new()
        .map(|p| p.data.join("documents"))
        .unwrap_or_else(|_| std::env::temp_dir().join("artcraft-documents"))
}

pub fn is_document_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| matches!(e.to_string_lossy().to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "artcraftdoc"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn chooser_columns_and_nodes_send_the_same_editable_document() {
        let (jobs, commands) = channel();
        let (events, rx) = channel();
        let workspace = Workspace {
            visible: true,
            jobs,
            events: rx,
            engines: None,
            tabs: Vec::new(),
            active: 0,
            serial: 0,
            busy: false,
            choices: Choices::default(),
            pending: VecDeque::new(),
            binder_key: None,
            node_view: false,
            error: None,
            notice: String::new(),
            recovery: PathBuf::new(),
        };
        let engines = Program::ALL
            .into_iter()
            .map(|p| {
                (
                    p,
                    engine::Engine {
                        exe: PathBuf::new(),
                        stamp: artcraft_launcher_core::document::EngineStamp { version: "test".into(), sha256: "test".into() },
                    },
                )
            })
            .collect();
        events.send(Event::Ready(engines)).unwrap();
        let doc = Document::new("UI interaction fixture.png".into(), vec![1], "fixture".into(), Default::default());
        let render = || Rendered { png: Vec::new(), evaluated: 0, cached: 0, elapsed_ms: 0, native_documents: Vec::new() };
        events.send(Event::Opened("first.png".into(), Ok((doc.clone(), render())))).unwrap();
        let mut fonts = false;
        let mut h = Harness::builder().with_size(egui::vec2(1120., 900.)).build_ui_state(
            move |ui, state: &mut Workspace| {
                artcraft_launcher_ui_egui::i18n::set_current(artcraft_launcher_ui_egui::Lang::EN);
                if !fonts {
                    artcraft_launcher_ui_egui::theme::install_fonts(ui.ctx(), false);
                    fonts = true;
                    ui.ctx().request_repaint();
                    return;
                }
                artcraft_launcher_ui_egui::theme::apply(ui.ctx(), artcraft_launcher_ui_egui::theme::ThemeChoice::Dark);
                state.ui(ui);
            },
            workspace,
        );
        h.run_steps(5);
        h.get_by_label("Choose the first toolset");
        h.get_all_by_role_and_label(egui::accesskit::Role::Button, "PhotoCraft").find(|n| n.rect().width() >= 200.).unwrap().click();
        h.run_steps(4);
        assert!(h.query_by_label("Choose the first toolset").is_none());
        h.get_by_label("Hue / Saturation").click();
        h.run_steps(3);
        let Job::Edit(id, edited) = commands.try_recv().unwrap() else { panic!("Expected an edit") };
        assert!(matches!(edited.graph().unwrap().node(1).unwrap().operation, Operation::HueSaturation { .. }));
        events.send(Event::Edited(id, Ok((edited, render())))).unwrap();
        h.run_steps(4);
        h.get_by_label("Replace with Levels").click();
        h.run_steps(3);
        h.get_by_label("Apply edit").click();
        h.run_steps(3);
        let Job::Edit(id, replaced) = commands.try_recv().unwrap() else { panic!("Expected replacement") };
        assert!(matches!(replaced.graph().unwrap().node(1).unwrap().operation, Operation::Levels { .. }));
        events.send(Event::Edited(id, Ok((replaced, render())))).unwrap();
        h.run_steps(4);
        h.get_by_label("Node graph").click();
        h.run_steps(4);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "#1 Levels").click();
        h.run_steps(3);
        assert_eq!(h.state().tabs[0].selected, Some(1));
        h.get_by_label("Enabled").click();
        h.run_steps(3);
        h.get_by_label("Apply edit").click();
        h.run_steps(3);
        let Job::Edit(id, bypassed) = commands.try_recv().unwrap() else { panic!("Expected bypass") };
        assert!(!bypassed.graph().unwrap().node(1).unwrap().enabled);
        events.send(Event::Edited(id, Ok((bypassed, render())))).unwrap();
        h.run_steps(4);
        h.get_by_label("Undo").click();
        h.run_steps(3);
        let Job::Edit(id, undone) = commands.try_recv().unwrap() else { panic!("Expected undo") };
        assert!(undone.graph().unwrap().node(1).unwrap().enabled);
        events.send(Event::Edited(id, Ok((undone, render())))).unwrap();
        h.run_steps(4);
        events.send(Event::Opened("second.png".into(), Ok((doc, render())))).unwrap();
        h.run_steps(4);
        h.get_by_label("Choose the first toolset");
        if let Some(dir) = std::env::var_os("ARTCRAFT_SHELL_EVIDENCE") {
            h.render().unwrap().save(PathBuf::from(dir).join("ui-chooser-harness.png")).unwrap();
        }
    }
}
