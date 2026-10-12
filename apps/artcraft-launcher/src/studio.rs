//! In-process native editors over one layered document. Experimental desktop host.
use crate::studio_document::Bridge;
use artcraft_canvas::SharedView;
use artcraft_launcher_ui_egui::tl;
use eframe::App;
use photocraft_ui_egui::PhotocraftApp;
use std::path::PathBuf;
use vectorcraft_ui_egui::VectorcraftApp;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Editor {
    Photo,
    Vector,
}

pub(crate) struct Studio {
    pub(crate) photo: PhotocraftApp,
    pub(crate) vector: VectorcraftApp,
    editor: Option<Editor>,
    bridge: Option<Bridge>,
    view: SharedView,
    pub(crate) status: String,
    preview_revision: Option<[u64; 5]>,
    preview_document: Option<([u64; 5], photocraft_doc::Document)>,
    preview_worker: Result<crate::studio_preview::Worker, String>,
    settle_fonts: bool,
    close_requested: bool,
    close_approved: bool,
}

pub fn run(args: &[String]) -> eframe::Result {
    let path = args.iter().map(PathBuf::from).find(|p| p.is_file());
    let mut options = crate::native_options(false);
    options.viewport =
        options.viewport.with_decorations(true).with_app_id("ai.storyteller.artcraft-launcher.studio").with_inner_size([1440.0, 940.0]);
    eframe::run_native(
        "ArtCraft Launcher",
        options,
        Box::new(move |cc| {
            let mut app = Studio::new();
            if let Some(rs) = &cc.wgpu_render_state {
                app.photo.set_wgpu(rs.clone());
            }
            if let Some(path) = path {
                app.open(path);
            }
            Ok(Box::new(app))
        }),
    )
}

impl Studio {
    pub(crate) fn new() -> Self {
        Self {
            photo: PhotocraftApp::new(photocraft_engine::Session::new(), crate::studio_services::photo()),
            vector: VectorcraftApp::new(vectorcraft_engine::Session::new(), vectorcraft_ui_egui::Services::default()),
            editor: None,
            bridge: None,
            view: SharedView::new(0, 0),
            status: String::new(),
            preview_revision: None,
            preview_document: None,
            preview_worker: crate::studio_preview::Worker::new(),
            settle_fonts: false,
            close_requested: false,
            close_approved: false,
        }
    }

    pub(crate) fn open(&mut self, path: PathBuf) {
        let result = (|| {
            if !crate::studio_services::supported_file(&path.to_string_lossy()) {
                return Err(tl!("This shared viewport preview opens PNG, JPEG, and .pcraft files.").into());
            }
            let bytes = photocraft_format::read_file(&path).map_err(|e| e.to_string())?;
            let imported =
                vectorcraft_engine::guard::catch_panic(|| photocraft_io::import(&path.to_string_lossy(), &bytes))?.map_err(|e| e.to_string())?;
            if imported.preview_only {
                return Err("This file opened as a preview and cannot become a shared document.".into());
            }
            self.photo.session.add_document(imported.document, None);
            self.photo.sync_views();
            let id = self.photo.session.active().ok_or("No document.")?.doc.id.0;
            self.view = SharedView::new(id, 1);
            self.editor = None;
            self.bridge = None;
            self.preview_revision = None;
            self.preview_document = None;
            self.vector.shared_presentation = None;
            self.status = imported.warnings.join("\n");
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            self.status = error;
        }
    }

    pub(crate) fn switch(&mut self, target: Editor, ctx: &egui::Context) -> Result<(), String> {
        if self.editor == Some(target) {
            return Ok(());
        }
        if self.pending_edit() {
            return Err(tl!("Finish the current tool or dialog before switching editors.").into());
        }
        if self.editor == Some(Editor::Vector) {
            // Native Close has already asked to save/discard its own edits. A
            // closed replica must not trap the user in an empty editor.
            if self.bridge.as_ref().is_some_and(|b| !self.vector.session.documents().iter().any(|s| s.uid == b.vector_uid)) {
                self.bridge = None;
                self.vector.shared_presentation = None;
            }
            if self.bridge.is_some() {
                self.commit_vector()?;
                if let Some(state) = self.vector.session.active_mut() {
                    state.mark_saved();
                }
            }
        }
        match target {
            Editor::Photo => {
                let id = self.photo.session.active().ok_or("Open a document first.")?.doc.id.0;
                self.photo.shared_canvas = Some(self.view.bind(id));
                PhotocraftApp::setup_context(ctx, self.photo.ui.theme);
            }
            Editor::Vector => {
                // Reload native bytes after a parent undo. Ordinary round trips
                // retain the native VectorCraft history. A parent history change
                // replaces its committed replica, avoiding stale duplicate tabs.
                if !self.bridge.as_ref().map(|b| b.resume(&self.photo.session, &mut self.vector.session)).transpose()?.unwrap_or(false) {
                    let obsolete = self.bridge.as_ref().and_then(|b| {
                        self.vector.session.documents().iter().position(|s| s.uid == b.vector_uid && s.revision == b.committed_revision)
                    });
                    let bridge = Bridge::attach(&mut self.photo.session, &mut self.vector.session)?;
                    if let Some(index) = obsolete {
                        // The parent document/history already owns this exact native snapshot.
                        self.vector.session.close_document(index);
                        if let Some(index) = self.vector.session.documents().iter().position(|s| s.uid == bridge.vector_uid) {
                            self.vector.session.set_active(index);
                        }
                    }
                    self.bridge = Some(bridge);
                    self.preview_revision = None;
                    self.preview_document = None;
                }
                self.vector.sync_views();
                let id = self.vector.session.active().ok_or("No vector document.")?.uid;
                self.refresh_preview(ctx)?;
                self.vector.shared_canvas = Some(self.view.bind(id));
                vectorcraft_ui_egui::theme::install_fonts(ctx);
                vectorcraft_ui_egui::theme::apply(ctx, self.vector.ui.brightness);
            }
        }
        self.editor = Some(target);
        self.settle_fonts = true;
        ctx.request_repaint();
        Ok(())
    }

    pub(crate) fn refresh_preview(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let bridge = self.bridge.as_ref().ok_or("No shared vector document.")?;
        let snapshot = bridge.snapshot(&self.photo.session, &self.vector.session)?;
        let key = snapshot.key;
        let worker = self.preview_worker.as_mut().map_err(|e| e.clone())?;
        let ready = worker.poll()?.filter(|(k, _)| *k == key);
        if self.preview_revision == Some(key) {
            return Ok(());
        }
        let preview = if let Some((_, result)) = ready {
            result?
        } else if self.preview_revision.is_none() {
            // Prepare the first image on handoff; subsequent pointer-driven edits
            // composite in the worker while native handles stay interactive.
            crate::studio_preview::render(snapshot)?
        } else {
            worker.submit(snapshot)?;
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
            return Ok(());
        };
        let image = preview.image;
        self.preview_document = Some((key, preview.document));
        let pixels = egui::ColorImage::from_rgba_unmultiplied([image.width as usize, image.height as usize], &image.pixels);
        let texture = match self.vector.shared_presentation.take() {
            Some(mut prior) => {
                prior.texture.set(pixels, egui::TextureOptions::LINEAR);
                prior.texture
            }
            None => ctx.load_texture("shared-document", pixels, egui::TextureOptions::LINEAR),
        };
        self.vector.shared_presentation = Some(vectorcraft_ui_egui::shared_canvas::Presentation {
            native_document: bridge.vector_uid,
            texture,
            bounds: vectorcraft_geom::Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height)),
        });
        self.preview_revision = Some(key);
        ctx.request_repaint();
        Ok(())
    }

    fn commit_vector(&mut self) -> Result<(), String> {
        let bridge = self.bridge.as_mut().ok_or("No shared vector document.")?;
        let before = bridge.snapshot(&self.photo.session, &self.vector.session)?.key;
        bridge.commit_cached(&mut self.photo.session, &self.vector.session, self.preview_document.as_ref())?;
        // Committing the exact displayed contents changes native source bytes and
        // parent history, but not pixels. Keep the existing GPU texture alive.
        let after = bridge.snapshot(&self.photo.session, &self.vector.session)?.key;
        if self.preview_revision == Some(before) {
            self.preview_revision = Some(after);
        }
        if let Some((key, _)) = &mut self.preview_document
            && *key == before
        {
            *key = after;
        }
        Ok(())
    }

    fn dirty(&self) -> bool {
        self.pending_edit()
            || self.photo.session.documents().iter().any(|s| s.is_dirty())
            || self.vector.session.documents().iter().any(|s| s.is_dirty())
            || self
                .bridge
                .as_ref()
                .is_some_and(|b| self.vector.session.active().is_some_and(|s| s.uid == b.vector_uid && s.revision != b.committed_revision))
    }

    pub(crate) fn pending_edit(&self) -> bool {
        match self.editor {
            Some(Editor::Photo) => !photocraft_ui_egui::shared_canvas::can_handoff(&self.photo),
            Some(Editor::Vector) => self.vector.session.in_interaction() || self.vector.session.tool_wants_text() || self.vector.ui.dialog.is_some(),
            None => false,
        }
    }

    pub(crate) fn save(&mut self) -> Result<(), String> {
        if self.pending_edit() {
            return Err(tl!("Finish the current tool or dialog before saving the project.").into());
        }
        if self.editor == Some(Editor::Vector) && self.bridge.is_some() {
            self.commit_vector()?;
        }
        let state = self.photo.session.active().ok_or("Open a document first.")?;
        let Some(path) =
            rfd::FileDialog::new().add_filter("PhotoCraft native document", &["pcraft"]).set_file_name("ArtCraft-project.pcraft").save_file()
        else {
            return Ok(());
        };
        if path.extension().is_none_or(|s| !s.eq_ignore_ascii_case("pcraft")) {
            return Err("Save the shared project as .pcraft to preserve both editors' native data.".into());
        }
        // The existing native writer replaces atomically and preserves the previous file on failure.
        vectorcraft_engine::guard::catch_panic(|| photocraft_format::PcraftWriter::new().save_path(&state.doc, &path, &Default::default()))?
            .map_err(|e| e.to_string())?;
        if let Some(state) = self.photo.session.active_mut() {
            state.saved_revision = state.revision;
            state.path = Some(path.to_string_lossy().into_owned());
        }
        self.mark_vector_snapshot_saved();
        self.status = path.display().to_string();
        Ok(())
    }

    fn mark_vector_snapshot_saved(&mut self) {
        // A standalone native tab is not covered by the shared project save.
        if let Some(state) = self
            .vector
            .session
            .active_mut()
            .filter(|s| self.bridge.as_ref().is_some_and(|b| s.uid == b.vector_uid && s.revision == b.committed_revision))
        {
            state.mark_saved();
        }
    }
}

impl Studio {
    #[cfg(feature = "studio-suite")]
    pub(crate) fn flush_shared_edits(&mut self) -> Result<(), String> {
        if self.pending_edit() {
            return Err(tl!("Finish the current tool or dialog before switching editors.").into());
        }
        if self.editor == Some(Editor::Vector) && self.bridge.is_some() {
            self.commit_vector()?;
        }
        Ok(())
    }
    pub(crate) fn editor_ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.settle_fonts) {
            ctx.request_repaint();
            return;
        }
        match self.editor {
            Some(Editor::Photo) => self.photo.ui(ui, frame),
            Some(Editor::Vector) => {
                self.vector.ui(ui);
                if let Err(error) = self.refresh_preview(&ctx) {
                    self.status = error;
                }
            }
            None => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.heading(tl!("Choose an editor for this file"));
                    ui.label(tl!("PhotoCraft and VectorCraft share this project's canvas and native contents."));
                });
            }
        }
    }
}

impl App for Studio {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            if !self.close_approved && self.dirty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_requested = true;
            }
            // The host owns the window-close decision for the combined project.
            return;
        }
        match self.editor {
            Some(Editor::Photo) => {
                self.photo.logic(ctx, frame);
                if let Some(id) = self.photo.session.active().map(|s| s.doc.id.0)
                    && id != self.view.document
                {
                    self.view = SharedView::new(id, 1);
                    self.bridge = None;
                    self.editor = None;
                }
            }
            Some(Editor::Vector) => self.vector.logic(ctx),
            None => {}
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        match self.editor {
            Some(Editor::Photo) => self.photo.raw_input_hook(ctx, raw),
            Some(Editor::Vector) => self.vector.raw_input_hook(raw),
            None => {}
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut target = None;
        egui::Panel::top("studio-cycler").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("ArtCraft Launcher");
                if ui.add_enabled(!self.dirty(), egui::Button::new(tl!("Open…"))).clicked()
                    && let Some(path) = rfd::FileDialog::new().add_filter("PNG / JPEG / PhotoCraft", &["png", "jpg", "jpeg", "pcraft"]).pick_file()
                {
                    self.open(path);
                }
                if ui.button(tl!("Save project…")).clicked()
                    && let Err(error) = self.save()
                {
                    self.status = error;
                }
                if ui.selectable_label(self.editor == Some(Editor::Photo), "PhotoCraft").clicked() {
                    target = Some(Editor::Photo);
                }
                if ui.selectable_label(self.editor == Some(Editor::Vector), "VectorCraft").clicked() {
                    target = Some(Editor::Vector);
                }
                ui.label(tl!("Shared viewport preview"));
            });
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
        });
        if let Some(target) = target
            && let Err(error) = self.switch(target, &ctx)
        {
            self.status = error;
        }
        if self.close_requested {
            egui::Window::new(tl!("Unsaved shared document")).collapsible(false).show(&ctx, |ui| {
                if ui.button(tl!("Save project…")).clicked() {
                    if let Err(error) = self.save() {
                        self.status = error;
                    }
                    if !self.dirty() {
                        self.close_requested = false;
                        self.close_approved = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                if ui.button(tl!("Keep editing")).clicked() {
                    self.close_requested = false;
                }
                if ui.button(tl!("Discard and close")).clicked() {
                    self.close_approved = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        }
        self.editor_ui(ui, frame);
        if let Some(state) = self.photo.session.active() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} — ArtCraft Launcher", state.doc.name)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// Isolated host timings; excludes window presentation and GPU frame latency.
    #[test]
    #[ignore = "manual performance measurement"]
    fn measure_editor_cycles() {
        for (width, height) in [(640, 400), (1600, 1000)] {
            let mut app = Studio::new();
            let mut doc = photocraft_doc::Document::new(
                "Cycle benchmark",
                photocraft_geom::Size::new(width, height),
                photocraft_color::ColorMode::Rgb,
                photocraft_color::SampleType::U8,
            );
            let mut raster = photocraft_raster::Surface::new(doc.pixel_format());
            raster.fill_rect(photocraft_geom::Rect::from_size(doc.size), &[0.18, 0.3, 0.4, 1.0]);
            doc.layers.push(photocraft_doc::Layer::new("Painting", photocraft_doc::LayerContent::Raster(raster)));
            app.view = SharedView::new(doc.id.0, 1);
            app.photo.session.add_document(doc, None);
            let ctx = egui::Context::default();
            app.switch(Editor::Photo, &ctx).unwrap();
            let start = std::time::Instant::now();
            app.switch(Editor::Vector, &ctx).unwrap();
            let cold = start.elapsed().as_secs_f64() * 1000.0;
            app.vector.session.execute("shape.rectangle", &serde_json::json!({"x":40,"y":40,"width":200,"height":120})).unwrap();
            let start = std::time::Instant::now();
            app.switch(Editor::Photo, &ctx).unwrap();
            let edited = start.elapsed().as_secs_f64() * 1000.0;
            let start = std::time::Instant::now();
            app.switch(Editor::Vector, &ctx).unwrap();
            let key = app.bridge.as_ref().unwrap().snapshot(&app.photo.session, &app.vector.session).unwrap().key;
            while app.preview_revision != Some(key) {
                assert!(start.elapsed().as_secs() < 15, "preview did not finish");
                std::thread::sleep(std::time::Duration::from_millis(1));
                app.refresh_preview(&ctx).unwrap();
            }
            let ready = start.elapsed().as_secs_f64() * 1000.0;
            app.switch(Editor::Photo, &ctx).unwrap();
            let mut cycles = Vec::new();
            for _ in 0..5 {
                let start = std::time::Instant::now();
                app.switch(Editor::Vector, &ctx).unwrap();
                assert_eq!(app.preview_revision, Some(key), "warm switch must already have the current canvas");
                app.switch(Editor::Photo, &ctx).unwrap();
                cycles.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            app.switch(Editor::Vector, &ctx).unwrap();
            app.vector.session.execute("shape.rectangle", &serde_json::json!({"x":260,"y":180,"width":40,"height":30})).unwrap();
            let key = app.bridge.as_ref().unwrap().snapshot(&app.photo.session, &app.vector.session).unwrap().key;
            let start = std::time::Instant::now();
            while app.preview_revision != Some(key) {
                assert!(start.elapsed().as_secs() < 15);
                app.refresh_preview(&ctx).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let preview_update = start.elapsed().as_secs_f64() * 1000.0;
            let start = std::time::Instant::now();
            app.switch(Editor::Photo, &ctx).unwrap();
            let cached_commit = start.elapsed().as_secs_f64() * 1000.0;
            println!(
                "STUDIO_PERF {}",
                serde_json::json!({"width":width,"height":height,"cold_vector_ms":cold,"edited_to_photo_ms":edited,"updated_canvas_ready_ms":ready,"unchanged_roundtrip_ms":cycles,"preview_update_ms":preview_update,"cached_edited_to_photo_ms":cached_commit})
            );
        }
    }

    #[test]
    fn real_editors_share_native_content_and_camera() {
        let mut h =
            egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 940.0)).with_pixels_per_point(1.5).with_max_steps(64).wgpu().build_eframe(
                |cc| {
                    let mut app = Studio::new();
                    let mut doc = photocraft_doc::Document::new(
                        "Shared canvas test",
                        photocraft_geom::Size::new(640, 400),
                        photocraft_color::ColorMode::Rgb,
                        photocraft_color::SampleType::U8,
                    );
                    let mut raster = photocraft_raster::Surface::new(doc.pixel_format());
                    raster.fill_rect(photocraft_geom::Rect::from_size(doc.size), &[0.18, 0.3, 0.4, 1.0]);
                    doc.layers.push(photocraft_doc::Layer::new("Painting", photocraft_doc::LayerContent::Raster(raster)));
                    let id = doc.id.0;
                    app.photo.session.add_document(doc, None);
                    app.photo.sync_views();
                    app.view = SharedView::new(id, 1);
                    if let Some(rs) = &cc.wgpu_render_state {
                        app.photo.set_wgpu(rs.clone());
                    }
                    app
                },
            );
        h.run_steps(4);
        h.get_by_label("Choose an editor for this file");
        let ctx = h.ctx.clone();
        h.state_mut().switch(Editor::Photo, &ctx).unwrap();
        h.run_steps(5);
        let canvas = h.state().photo.last_canvas_rect;
        assert!(canvas.width() > 300.0);
        h.state_mut().photo.session.execute("paint.stroke", serde_json::json!({"points":[[100,100],[200,170]],"color":"#ff4000","size":30})).unwrap();
        h.run_steps(4);
        h.state_mut().photo.ui.tool = photocraft_ui_egui::state::Tool::Brush;
        let xf = photocraft_ui_egui::canvas::ViewXform::active(&h.state().photo).unwrap();
        let from = xf.to_screen(120.0, 260.0);
        let to = xf.to_screen(240.0, 280.0);
        let before = h.state().photo.session.active().unwrap().revision;
        h.event(egui::Event::PointerMoved(from));
        h.step();
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        h.event(egui::Event::PointerMoved(to));
        h.step();
        h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(4);
        assert!(h.state().photo.session.active().unwrap().revision > before);
        let camera = h.state().view.camera().unwrap();
        h.state_mut().switch(Editor::Vector, &ctx).unwrap();
        h.run_steps(5);
        assert!(h.state().vector.canvas_rect.unwrap().width() > 300.0);
        let v = h.state().vector.view().unwrap();
        assert!((v.zoom * f64::from(ctx.pixels_per_point()) - camera.zoom).abs() < 1e-5);
        h.state_mut().vector.select_tool("rectangle");
        let xf = vectorcraft_ui_egui::canvas::Xf::new(h.state().vector.canvas_rect.unwrap(), h.state().vector.view().unwrap());
        let from = xf.to_screen(vectorcraft_geom::Point::new(260.0, 120.0));
        let to = xf.to_screen(vectorcraft_geom::Point::new(420.0, 240.0));
        let before = h.state().vector.session.active().unwrap().revision;
        h.event(egui::Event::PointerMoved(from));
        h.step();
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        h.event(egui::Event::PointerMoved(to));
        h.step();
        h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(5);
        let revision = h.state().vector.session.active().unwrap().revision;
        assert!(revision > before);
        for _ in 0..200 {
            if h.state().preview_revision.is_some_and(|key| key[3] == revision) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            h.step();
        }
        assert!(h.state().preview_revision.is_some_and(|key| key[3] == revision));
        let cached_texture = h.state().vector.shared_presentation.as_ref().unwrap().texture.id();
        if let Some(dir) = std::env::var_os("ARTCRAFT_STUDIO_EVIDENCE") {
            h.render().unwrap().save(PathBuf::from(&dir).join("native-vectorcraft-shared.png")).unwrap();
        }
        h.state_mut().switch(Editor::Photo, &ctx).unwrap();
        h.run_steps(5);
        assert_eq!(h.state().photo.session.active().unwrap().doc.layers.len(), 2);
        if let Some(dir) = std::env::var_os("ARTCRAFT_STUDIO_EVIDENCE") {
            h.render().unwrap().save(PathBuf::from(&dir).join("native-photocraft-shared.png")).unwrap();
            let doc = &h.state().photo.session.active().unwrap().doc;
            photocraft_format::PcraftWriter::new()
                .save_path(doc, &PathBuf::from(&dir).join("shared-native-project.pcraft"), &Default::default())
                .unwrap();
        }
        h.state_mut().switch(Editor::Vector, &ctx).unwrap();
        assert_eq!(h.state().vector.shared_presentation.as_ref().unwrap().texture.id(), cached_texture);
        assert_eq!(
            h.state().preview_revision,
            Some(h.state().bridge.as_ref().unwrap().snapshot(&h.state().photo.session, &h.state().vector.session).unwrap().key)
        );
        h.run_steps(5);
        assert_eq!(h.state().vector.session.documents().len(), 1);
        assert!(h.state().status.is_empty(), "{}", h.state().status);

        // Parent undo changes the native source; the new replica replaces the
        // old committed tab rather than leaving a stale duplicate behind.
        h.state_mut().switch(Editor::Photo, &ctx).unwrap();
        h.run_steps(5);
        assert!(h.state_mut().photo.session.undo());
        h.state_mut().switch(Editor::Vector, &ctx).unwrap();
        h.run_steps(5);
        assert_eq!(h.state().vector.session.documents().len(), 1);
        assert_eq!(h.state().bridge.as_ref().unwrap().vector_uid, h.state().vector.session.active().unwrap().uid);
        assert!(h.state_mut().vector.session.close_document(0));
        h.state_mut().switch(Editor::Photo, &ctx).unwrap();
        h.run_steps(5);
        assert!(h.state().editor == Some(Editor::Photo));

        h.state_mut().vector.session.add_document(vectorcraft_doc::Document::new(100.0, 100.0), None);
        h.state_mut().vector.session.execute("shape.rectangle", &serde_json::json!({"x":1,"y":1,"width":10,"height":10})).unwrap();
        h.state_mut().mark_vector_snapshot_saved();
        assert!(h.state().vector.session.active().unwrap().is_dirty(), "saving the shared project cannot mark an unrelated tab saved");
    }
}
