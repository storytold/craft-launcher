//! Native editors with shared painting and editable companion documents.
#[path = "suite_link.rs"]
mod links;
use artcraft_launcher_ui_egui::tl;
use eframe::App;
use serde_json::json;
use std::path::PathBuf;

const TITLE: &str = "ArtCraft Launcher — All apps studio";

const NAMES: [&str; 12] = [
    "PhotoCraft",
    "VectorCraft",
    "LightCraft",
    "DesignCraft",
    "PdfCraft",
    "EffectCraft",
    "FilmCraft",
    "WordCraft",
    "DeckCraft",
    "GridCraft",
    "CADCraft",
    "SoundCraft",
];

enum Native {
    Design(Box<designcraft_ui_egui::DesignApp>),
    Pdf(Box<pdfcraft_ui_egui::PdfCraftApp>),
    Effect(Box<effectcraft_ui_egui::EffectcraftApp>),
    Film(Box<filmcraft_ui_egui::FilmcraftApp>),
    Word(Box<wordcraft_ui_egui::WordApp>),
    Deck(Box<deckcraft_ui_egui::SlideApp>),
    Grid(Box<gridcraft_ui_egui::SheetApp>),
    Cad(Box<cadcraft_ui_egui::CadApp>),
    Sound(Box<soundcraft_ui_egui::SoundApp>),
}

impl Native {
    fn new(index: usize, path: &std::path::Path, rs: Option<&eframe::egui_wgpu::RenderState>) -> Result<Self, String> {
        let file = path.to_string_lossy().into_owned();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("test.png");
        let (width, height) = image::image_dimensions(path).map_err(|e| e.to_string())?;
        let aspect = f64::from(height) / f64::from(width.max(1));
        Ok(match index {
            3 => {
                let mut app = designcraft_ui_egui::DesignApp::new(designcraft_engine::Session::new(), Default::default());
                app.run("file.new", json!({}))?;
                app.run("file.place", json!({"path": file, "x": 40, "y": 40, "width": 400}))?;
                Self::Design(Box::new(app))
            }
            4 => {
                let mut app = pdfcraft_ui_egui::PdfCraftApp::new();
                app.open_bytes(name, None, photocraft_format::read_file(path).map_err(|e| e.to_string())?)?;
                Self::Pdf(Box::new(app))
            }
            5 => {
                let mut session = effectcraft_host::session();
                // This isolated test does not share installed AI model/config paths.
                session.models_dir = None;
                session
                    .execute("comp.new", json!({"name": "Artwork companion", "width": width, "height": height, "duration": 5}))
                    .map_err(|e| e.to_string())?;
                session.execute("file.import", json!({"paths": [file], "addToComp": true})).map_err(|e| e.to_string())?;
                Self::Effect(Box::new(effectcraft_ui_egui::EffectcraftApp::new(session)))
            }
            6 => {
                let mut session = filmcraft_engine::Session::default();
                session.execute("file.newProject", json!({"name": "Artwork companion"})).map_err(|e| e.to_string())?;
                let imported = session.execute("file.import", json!({"paths": [file]})).map_err(|e| e.to_string())?;
                let item =
                    imported.get("items").and_then(|v| v.as_array()).and_then(|v| v.first()).ok_or("FilmCraft did not import the test image.")?;
                session.execute("file.newSequence", json!({"name": "Artwork sequence", "fromItem": item})).map_err(|e| e.to_string())?;
                let mut app = filmcraft_ui_egui::FilmcraftApp::new(session);
                if let Some(rs) = rs {
                    app.set_wgpu(rs.clone());
                }
                Self::Film(Box::new(app))
            }
            7 => {
                let session = wordcraft_engine::Session::new(wordcraft_doc::Document::new());
                let mut app = wordcraft_ui_egui::WordApp::new(session, Default::default());
                app.run("insert.picture", json!({"path": file, "width": 400}))?;
                Self::Word(Box::new(app))
            }
            8 => {
                let mut app = deckcraft_ui_egui::SlideApp::new(deckcraft_engine::Session::with_new(), Default::default());
                app.run("insert.picture", json!({"path": file, "rect": [50,50,500,500.0*aspect]}))?;
                Self::Deck(Box::new(app))
            }
            9 => {
                let mut session = gridcraft_engine::Session::new();
                session.new_workbook();
                let mut app = gridcraft_ui_egui::SheetApp::new(session, Default::default());
                app.run("insert.picture", json!({"path":file,"at":"B2","width":500,"height":500.0*aspect}))?;
                Self::Grid(Box::new(app))
            }
            10 => {
                let mut session = cadcraft_engine::Session::empty();
                session.new_drawing(true);
                let mut app = cadcraft_ui_egui::CadApp::new(session, Default::default());
                app.run("rectang", json!({"p1":[0,0],"p2":[100,100.0*aspect]}))?;
                if let Some(rs) = rs {
                    app.set_wgpu(rs);
                }
                Self::Cad(Box::new(app))
            }
            11 => {
                let mut app = soundcraft_ui_egui::SoundApp::new(soundcraft_engine::Engine::default(), None, Default::default());
                app.run("track.new", json!({"count":2,"name":"Artwork soundtrack","format":"stereo"}))?;
                Self::Sound(Box::new(app))
            }
            _ => return Err("Unknown suite app.".into()),
        })
    }

    fn activate(&self, ctx: &egui::Context) {
        match self {
            Self::Design(_) => designcraft_ui_egui::theme::install_fonts(ctx, "en"),
            Self::Pdf(_) => pdfcraft_ui_egui::theme::install_fonts(ctx),
            Self::Effect(a) => effectcraft_ui_egui::theme::install(ctx, &a.tokens, "en"),
            Self::Film(a) => filmcraft_ui_egui::theme::install(ctx, &a.tokens),
            Self::Word(_) => wordcraft_ui_egui::theme::install_fonts(ctx),
            Self::Deck(_) => deckcraft_ui_egui::theme::install_fonts(ctx),
            Self::Grid(_) => gridcraft_ui_egui::SheetApp::setup_context(ctx, true),
            Self::Cad(_) => cadcraft_ui_egui::theme::install_fonts(ctx),
            Self::Sound(_) => {
                ctx.set_fonts(soundcraft_ui_egui::fonts::definitions());
                soundcraft_ui_egui::theme::apply(ctx, false);
            }
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        match self {
            Self::Pdf(a) => a.logic(ctx, frame),
            Self::Effect(a) => a.logic(ctx, frame),
            Self::Film(a) => a.logic(ctx, frame),
            Self::Design(a) => a.logic(ctx),
            Self::Word(a) => a.logic(ctx),
            Self::Deck(a) => a.logic(ctx),
            Self::Grid(a) => a.logic(ctx),
            Self::Cad(a) => a.logic(ctx),
            Self::Sound(a) => a.logic(ctx),
        }
    }
    fn input(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        match self {
            Self::Pdf(a) => a.raw_input_hook(ctx, raw),
            Self::Effect(a) => a.raw_input_hook(ctx, raw),
            Self::Film(a) => a.raw_input_hook(ctx, raw),
            Self::Design(a) => a.raw_input_hook(raw),
            Self::Word(a) => a.raw_input_hook(raw),
            Self::Deck(a) => a.raw_input_hook(raw),
            Self::Grid(a) => a.raw_input_hook(raw),
            Self::Cad(a) => a.raw_input_hook(raw),
            Self::Sound(a) => a.raw_input_hook(raw),
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        match self {
            Self::Pdf(a) => a.ui(ui, frame),
            Self::Effect(a) => a.ui(ui, frame),
            Self::Film(a) => a.ui(ui, frame),
            Self::Design(a) => a.ui(ui),
            Self::Word(a) => a.ui(ui),
            Self::Deck(a) => a.ui(ui),
            Self::Grid(a) => a.ui(ui),
            Self::Cad(a) => a.ui(ui),
            Self::Sound(a) => a.ui(ui),
        }
    }
}

struct Lab {
    apps: Vec<Option<Native>>,
    shared: crate::studio::Studio,
    light: Option<Box<lightcraft_ui_egui::LightcraftApp>>,
    light_link: crate::studio_light::Link,
    memories: Vec<Option<egui::Memory>>,
    styles: Vec<Option<[std::sync::Arc<egui::Style>; 2]>>,
    active: Option<usize>,
    book: links::Book,
    render_state: Option<eframe::egui_wgpu::RenderState>,
    status: String,
    settle: bool,
    close_prompt: bool,
    close_approved: bool,
}
impl Lab {
    fn new(path: PathBuf, rs: Option<eframe::egui_wgpu::RenderState>) -> Self {
        let mut shared = crate::studio::Studio::new();
        if let Some(render) = &rs {
            shared.photo.set_wgpu(render.clone());
        }
        shared.open(path.clone());
        Self {
            shared,
            light: None,
            light_link: Default::default(),
            apps: (0..NAMES.len()).map(|_| None).collect(),
            memories: (0..NAMES.len()).map(|_| None).collect(),
            styles: (0..NAMES.len()).map(|_| None).collect(),
            active: None,
            book: Default::default(),
            render_state: rs,
            status: String::new(),
            settle: false,
            close_prompt: false,
            close_approved: false,
        }
    }
    fn switch(&mut self, index: usize, ctx: &egui::Context) -> Result<(), String> {
        if self.active == Some(index) {
            return Ok(());
        }
        if index >= NAMES.len() {
            return Err("Unknown app.".into());
        }
        if self.active.is_some_and(|i| i < 2) && self.shared.pending_edit() {
            return Err(tl!("Finish the current tool or dialog before switching editors.").into());
        }
        if self.active == Some(2) {
            let light = self.light.as_ref().ok_or("Missing LightCraft session.")?;
            self.light_link.commit(&mut self.shared.photo.session, light)?;
        }
        // Commit the original native vector bridge before another app reads the document.
        if index > 1 && self.active.is_some_and(|i| i < 2) {
            self.shared.flush_shared_edits()?;
        }
        let cold = if index < 2 {
            self.memories.get(index).is_none_or(Option::is_none)
        } else if index == 2 {
            self.light.is_none()
        } else {
            self.apps.get(index).is_none_or(Option::is_none)
        };
        if index >= 3 {
            let path = self.book.source(&self.shared.photo.session, ctx)?.path.clone();
            if cold {
                let mut native = vectorcraft_engine::guard::catch_panic(|| Native::new(index, &path, self.render_state.as_ref()))??;
                self.book.enter(index, &mut native, true)?;
                if let Some(slot) = self.apps.get_mut(index) {
                    *slot = Some(native);
                }
            } else if let Some(native) = self.apps.get_mut(index).and_then(Option::as_mut) {
                self.book.enter(index, native, false)?;
            }
        }
        // Validate and prepare the shared source before touching the current UI state.
        if index == 2 {
            self.light_link.enter(&self.shared.photo.session, &mut self.light)?;
        }
        if let Some(old) = self.active {
            if let Some(memory) = self.memories.get_mut(old) {
                *memory = Some(ctx.memory(Clone::clone));
            }
            if let Some(style) = self.styles.get_mut(old) {
                *style = Some([ctx.style_of(egui::Theme::Dark), ctx.style_of(egui::Theme::Light)]);
            }
        }
        // Native apps reuse panel/widget IDs. Give each app its own egui state.
        let memory = self.memories.get(index).and_then(Option::as_ref).cloned().unwrap_or_default();
        if index < 2 {
            self.shared.switch(if index == 0 { crate::studio::Editor::Photo } else { crate::studio::Editor::Vector }, ctx)?;
            if index == 1 {
                self.shared.refresh_preview(ctx)?;
            }
        }
        // Memory includes queued font definitions: restore it BEFORE installing
        // the target fonts, otherwise the new stack is silently discarded.
        ctx.memory_mut(|m| *m = memory);
        match index {
            0 | 1 => {
                // Returning from another app may keep Studio's editor unchanged.
                if index == 0 {
                    photocraft_ui_egui::PhotocraftApp::setup_context(ctx, self.shared.photo.ui.theme);
                } else {
                    vectorcraft_ui_egui::theme::install_fonts(ctx);
                    vectorcraft_ui_egui::theme::apply(ctx, self.shared.vector.ui.brightness);
                }
            }
            2 => {
                lightcraft_ui_egui::theme::install_fonts(ctx);
                lightcraft_ui_egui::theme::apply(ctx);
            }
            _ => {
                if let Some(app) = self.apps.get(index).and_then(Option::as_ref) {
                    app.activate(ctx);
                }
            }
        }
        if let Some(style) = self.styles.get(index).and_then(Option::as_ref) {
            ctx.set_style_of(egui::Theme::Dark, style[0].clone());
            ctx.set_style_of(egui::Theme::Light, style[1].clone());
        }
        self.active = Some(index);
        self.settle = true;
        self.status = format!("{} · {}", NAMES.get(index).copied().unwrap_or(""), if cold { tl!("First open") } else { tl!("Retained session") });
        ctx.request_repaint();
        Ok(())
    }
}
impl App for Lab {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            if !self.close_approved {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_prompt = true;
            }
            return;
        }
        if self.active.is_some_and(|i| i < 2) {
            self.shared.logic(ctx, frame);
        } else if self.active == Some(2) {
            if let Some(a) = &mut self.light {
                a.logic(ctx);
            }
        } else if let Some(a) = self.active.and_then(|i| self.apps.get_mut(i)).and_then(Option::as_mut) {
            a.logic(ctx, frame);
        }
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.active.is_some_and(|i| i < 2) {
            self.shared.raw_input_hook(ctx, raw);
        } else if self.active == Some(2) {
            if let Some(a) = &mut self.light {
                a.raw_input_hook(raw);
            }
        } else if let Some(a) = self.active.and_then(|i| self.apps.get_mut(i)).and_then(Option::as_mut) {
            a.input(ctx, raw);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut requested = None;
        egui::Panel::top("suite-cycle").show(ui, |ui| {
            ui.label(tl!("One project: shared artwork and editable native companions. Save project keeps them together."));
            ui.horizontal_wrapped(|ui| {
                for (i, name) in NAMES.iter().enumerate() {
                    if ui.selectable_label(self.active == Some(i), *name).clicked() {
                        requested = Some(i);
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.button(tl!("Save project…")).clicked() {
                    let result = if self.active == Some(2) {
                        self.light.as_ref().ok_or("Missing LightCraft session.".to_owned()).and_then(|a| self.light_link.commit(&mut self.shared.photo.session,a))
                    } else { Ok(()) };
                    let result = result.and_then(|()| self.shared.flush_shared_edits()).and_then(|()| {
                        self.book.source(&self.shared.photo.session, &ctx)?;
                        self.book.save(&self.apps, &mut self.shared.photo.session)
                    }).and_then(|()| self.shared.save());
                    if let Err(e) = result { self.status = e; }
                }
                ui.label(&self.status);
            });
            if self.active == Some(2) {
                ui.label(tl!("Shared look: Exposure, white balance, color, curves, mixer, grading and calibration. Other LightCraft tools are not connected yet."));
            }
            if !self.shared.status.is_empty() { ui.label(&self.shared.status); }
            if self.active.is_some_and(|i| (3..10).contains(&i)) { ui.label(tl!("Native companion: artwork updates on return; this app's edits stay editable here and are saved with the project.")); }
            if self.active == Some(10) {
                ui.label(tl!("CADCraft: draft beside the live artwork reference. Native geometry is saved with this project."));
            }
            if self.active == Some(11) {
                ui.label(tl!("SoundCraft: create a soundtrack beside the live artwork. Tracks and embedded audio are saved with this project."));
            }
        });
        if let Some(i) = requested
            && let Err(e) = self.switch(i, &ctx)
        {
            self.status = e;
        }
        if self.close_prompt {
            egui::Window::new(tl!("Close the native app test?")).show(&ctx, |ui| {
                ui.label(tl!("This test keeps native sessions in memory. Closing discards any unsaved test edits."));
                if ui.button(tl!("Keep editing")).clicked() {
                    self.close_prompt = false;
                }
                if ui.button(tl!("Discard and close")).clicked() {
                    self.close_approved = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        }
        if std::mem::take(&mut self.settle) {
            ctx.request_repaint();
            return;
        }
        if self.active.is_some_and(|i| i < 2) {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()));
            self.shared.editor_ui(&mut child, frame);
        } else if self.active == Some(2) {
            if let Some(a) = &mut self.light {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()));
                a.ui(&mut child);
            }
        } else if let Some(a) = self.active.and_then(|i| self.apps.get_mut(i)).and_then(Option::as_mut) {
            if self.active.is_some_and(|i| i >= 10)
                && let Some(source) = self.book.reference()
            {
                egui::Panel::left("shared-artwork-reference").default_size(250.0).resizable(true).show(ui, |ui| {
                    ui.heading(tl!("Shared artwork"));
                    ui.add(
                        egui::Image::new(&source.texture)
                            .fit_to_exact_size(egui::vec2(ui.available_width(), ui.available_width() * source.height as f32 / source.width as f32)),
                    );
                    ui.label(tl!("The reference updates when you return from another editor."));
                });
            }
            // Some native frontends lay out from max_rect rather than the cursor.
            // Give them the remaining canvas so they cannot paint over the cycler.
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()));
            a.ui(&mut child, frame);
        } else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.label(tl!("Choose an app to test."));
            });
        }
        // Child apps update their standalone titles. The containing window is
        // still the launcher; native document names remain in their own tabs.
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(TITLE.into()));
    }
}

pub fn run(args: &[String]) -> eframe::Result {
    let path = args.iter().map(PathBuf::from).find(|p| p.is_file()).unwrap_or_default();
    let mut options = crate::native_options(false);
    options.viewport =
        options.viewport.with_decorations(true).with_inner_size([1440.0, 940.0]).with_app_id("ai.storyteller.artcraft-launcher.suite-test");
    eframe::run_native(TITLE, options, Box::new(move |cc| Ok(Box::new(Lab::new(path, cc.wgpu_render_state.clone())))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "rendered shared three-editor acceptance test"]
    fn shared_three_editors_cycle_save_and_reopen() {
        let path = PathBuf::from(std::env::var_os("ARTCRAFT_SUITE_FIXTURE").unwrap());
        let out = PathBuf::from(std::env::var_os("ARTCRAFT_SUITE_EVIDENCE").unwrap());
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 940.0))
            .with_max_steps(64)
            .wgpu_setup(crate::wgpu_options().wgpu_setup)
            .build_eframe(|cc| Lab::new(path, cc.wgpu_render_state.clone()));
        let ctx = h.ctx.clone();
        h.state_mut().switch(0, &ctx).unwrap();
        h.run_steps(6);
        h.state_mut().shared.photo.session.execute("paint.stroke", json!({"points":[[100,100],[200,170]],"color":"#ff4000","size":30})).unwrap();
        h.state_mut().switch(1, &ctx).unwrap();
        h.run_steps(6);
        h.state_mut().shared.vector.session.execute("shape.rectangle", &json!({"x":260,"y":120,"width":140,"height":100})).unwrap();
        h.state_mut().switch(2, &ctx).unwrap();
        h.run_steps(6);
        let native = h.state().shared.photo.session.active().unwrap().doc.layers.clone();
        let light = h.state_mut().light.as_mut().unwrap();
        let rendered = light.session.render_now(lightcraft_engine::catalog::PhotoId(1), 640, 400).unwrap().image;
        assert!(rendered.get(150, 135)[0] > 200, "PhotoCraft paint must be visible in LightCraft");
        assert!(rendered.get(300, 160).iter().all(|c| *c > 240), "VectorCraft shape must be visible in LightCraft");
        let mut settings = lightcraft_engine::develop::DevelopSettings::default();
        settings.light.exposure = 0.65;
        light.session.set_develop(lightcraft_engine::catalog::PhotoId(1), settings.clone(), "Shared exposure").unwrap();
        let mut timings = Vec::new();
        for pass in 0..2 {
            for index in [0, 1, 2] {
                let start = std::time::Instant::now();
                h.state_mut().switch(index, &ctx).unwrap();
                timings.push(json!({"app":NAMES[index],"pass":pass,"handoff_ms":start.elapsed().as_secs_f64()*1000.0}));
                for _ in 0..30 {
                    h.run_steps(1);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                h.render().unwrap().save(out.join(format!("shared-{}-{pass}.png", NAMES[index]))).unwrap();
            }
        }
        let doc = &h.state().shared.photo.session.active().unwrap().doc;
        assert_eq!(&doc.layers[..2], &native[..2], "original paint and vector layers cannot be flattened");
        assert_eq!(doc.layers.len(), 3);
        let project = out.join("Shared-three-editor-test.pcraft");
        photocraft_format::PcraftWriter::new().save_path(doc, &project, &Default::default()).unwrap();
        let mut reopened = Lab::new(project, None);
        reopened.switch(2, &ctx).unwrap();
        assert_eq!(*reopened.light.as_ref().unwrap().session.develop_of(lightcraft_engine::catalog::PhotoId(1)).unwrap(), settings);
        reopened.switch(1, &ctx).unwrap();
        let before = reopened.shared.vector.session.active().unwrap().revision;
        reopened.shared.vector.session.execute("shape.rectangle", &json!({"x":430,"y":200,"width":30,"height":30})).unwrap();
        assert!(reopened.shared.vector.session.active().unwrap().revision > before);
        reopened.switch(2, &ctx).unwrap();
        let light = reopened.light.as_mut().unwrap();
        let rendered = light.session.render_now(lightcraft_engine::catalog::PhotoId(1), 640, 400).unwrap().image;
        assert!(rendered.get(440, 210).iter().all(|c| *c > 240), "new native vector remains editable after reopening");
        settings.light.shadows = 30.0;
        light.session.set_develop(lightcraft_engine::catalog::PhotoId(1), settings, "Unsupported edit").unwrap();
        let before = reopened.shared.photo.session.active().unwrap().doc.clone();
        assert!(reopened.switch(0, &ctx).is_err());
        assert_eq!(reopened.active, Some(2));
        assert!(std::sync::Arc::ptr_eq(&before, &reopened.shared.photo.session.active().unwrap().doc));
        std::fs::write(out.join("shared-three-cycle.json"), serde_json::to_vec_pretty(&timings).unwrap()).unwrap();
        println!("SHARED_THREE {}", serde_json::to_string(&timings).unwrap());
    }

    #[test]
    fn vector_pdf_import_works_with_the_suite_renderer() {
        // Original fixture: one blue vector rectangle, no external assets/fonts.
        let content = "0 0 1 rg 10 10 80 50 re f\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 80] /Resources << >> /Contents 4 0 R >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
        let mut session = vectorcraft_engine::Session::new();
        vectorcraft_engine::cmd::fileio::open_bytes(&mut session, "suite.pdf", &bytes, None).unwrap();
        let image = vectorcraft_render::Renderer::new().render_region(
            &session.active().unwrap().doc,
            vectorcraft_geom::Rect::new(0.0, 0.0, 100.0, 80.0),
            1.0,
            false,
        );
        let pixels = image.to_straight();
        assert_eq!((image.width, image.height), (100, 80));
        let center = (40 * 100 + 50) * 4;
        assert_eq!(&pixels[center..center + 4], &[0, 0, 255, 255]);
    }

    #[test]
    #[ignore = "full native suite compatibility and timing run"]
    fn every_native_app_opens_and_retains_its_session() {
        let path = PathBuf::from(std::env::var_os("ARTCRAFT_SUITE_FIXTURE").expect("set the synthetic PNG fixture path"));
        let out = PathBuf::from(std::env::var_os("ARTCRAFT_SUITE_EVIDENCE").expect("set the evidence directory"));
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 940.0))
            .with_max_steps(64)
            .wgpu_setup(crate::wgpu_options().wgpu_setup)
            .build_eframe(|cc| Lab::new(path, cc.wgpu_render_state.clone()));
        if let Some(rs) = &h.state().render_state {
            let info = rs.adapter.get_info();
            std::fs::write(
                out.join("renderer.json"),
                serde_json::to_vec_pretty(&json!({
                    "name": info.name, "backend": format!("{:?}", info.backend),
                    "device_type": format!("{:?}", info.device_type)
                }))
                .unwrap(),
            )
            .unwrap();
        }
        let ctx = h.ctx.clone();
        let mut report = Vec::new();
        let mut red_before = [0usize; 12];
        for pass in 0..3 {
            if pass == 1 {
                h.state_mut().switch(0, &ctx).unwrap();
                h.state_mut()
                    .shared
                    .photo
                    .session
                    .execute("paint.stroke", json!({"points":[[170,140],[360,220]],"color":"#ff0000","size":65}))
                    .unwrap();
            }
            for (i, name) in NAMES.iter().enumerate() {
                let start = std::time::Instant::now();
                let result = h.state_mut().switch(i, &ctx);
                let switch_ms = start.elapsed().as_secs_f64() * 1000.0;
                let result = result.and_then(|()| {
                    vectorcraft_engine::guard::catch_panic(|| {
                        let start = std::time::Instant::now();
                        h.run_steps(6);
                        let frame_ms = start.elapsed().as_secs_f64() * 1000.0;
                        let start = std::time::Instant::now();
                        let _rendered = h.render().map_err(|e| e.to_string())?;
                        let render_ms = start.elapsed().as_secs_f64() * 1000.0;
                        if pass < 2 {
                            // Image decoders and native preview workers need elapsed time,
                            // not just six immediately consecutive headless frames.
                            for _ in 0..25 {
                                std::thread::sleep(std::time::Duration::from_millis(40));
                                h.run_steps(1);
                            }
                        }
                        let rendered = h.render().map_err(|e| e.to_string())?;
                        let red = rendered.pixels().filter(|p| p[0] > 230 && p[1] < 30 && p[2] < 30).count();
                        if pass == 0 {
                            red_before[i] = red;
                        } else if red < red_before[i] + 300 {
                            return Err(format!("{name} showed stale artwork: {red} red pixels, originally {}", red_before[i]));
                        }
                        rendered.save(out.join(format!("{name}-pass-{pass}.png"))).map_err(|e| e.to_string())?;
                        if pass == 0 {
                            rendered.save(out.join(format!("{name}.png"))).map_err(|e| e.to_string())?;
                        }
                        // The ten image-capable apps must display the synthetic blue
                        // jacket, not merely accept an import command or show a thumbnail.
                        if i < 10 {
                            let blue =
                                rendered.pixels().filter(|p| p[0].abs_diff(64) <= 5 && p[1].abs_diff(104) <= 5 && p[2].abs_diff(135) <= 5).count();
                            if blue < 4_000 {
                                return Err(format!("{name} did not display the fixture: {blue} matching pixels"));
                            }
                        }
                        Ok::<(f64, f64), String>((frame_ms, render_ms))
                    })
                    .and_then(|r| r)
                });
                report.push(json!({"app":name,"pass":pass,"switch_ms":switch_ms,
                    "six_ui_frames_ms":result.as_ref().ok().map(|r|r.0),
                    "gpu_render_readback_ms":result.as_ref().ok().map(|r|r.1),"error":result.err()}));
            }
        }
        std::fs::write(out.join("native-cycle-results.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        println!("SUITE_CYCLE {}", serde_json::to_string(&report).unwrap());
        assert!(report.iter().all(|r| r["error"].is_null()), "see native-cycle-results.json");
        assert!(h.state().apps.iter().skip(3).all(Option::is_some));
        assert!(h.state().light.is_some());
        let lab = h.state_mut();
        lab.book.save(&lab.apps, &mut lab.shared.photo.session).unwrap();
        photocraft_format::PcraftWriter::new()
            .save_path(&lab.shared.photo.session.active().unwrap().doc, &out.join("Twelve-app-project.pcraft"), &Default::default())
            .unwrap();
    }
}
