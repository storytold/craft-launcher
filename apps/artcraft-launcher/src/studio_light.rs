//! Reversible LightCraft tone/color settings in a native PhotoCraft adjustment.
//! A sampled LUT is a display evaluator, never a replacement for paint/vectors.
use lightcraft_engine::{
    catalog::{Op, Photo, PhotoId, Source},
    develop::DevelopSettings,
    pipeline,
};
use photocraft_doc::{Adjustment, Document, Layer, LayerContent};
use std::sync::Arc;

const PREFIX: &str = "ArtCraft.LightCraft.v1:";
const LOOK_NAME: &str = "LightCraft tone / color";
const SIZE: usize = 33;

fn settings(doc: &Document) -> Result<DevelopSettings, String> {
    let mut found = None;
    for (_, _, layer) in doc.walk() {
        if let LayerContent::Adjustment(Adjustment::ColorLookup { name, .. }) = &layer.content
            && is_look(layer)
        {
            if found.is_some() {
                return Err("Multiple LightCraft looks: keep one shared look before editing it.".into());
            }
            let key = format!("{PREFIX}{}", layer.id.0);
            // Older local test files kept settings in the lookup label. New
            // projects use native metadata so Properties shows a human label.
            let json = name
                .strip_prefix(PREFIX)
                .or_else(|| doc.metadata.text.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str()))
                .ok_or("Missing native LightCraft settings; the lookup layer is preserved.")?;
            found = Some(serde_json::from_str(json).map_err(|e| format!("Cannot read LightCraft settings: {e}"))?);
        }
    }
    Ok(found.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_geom::{Rect, Size};

    fn photo() -> photocraft_engine::Session {
        let mut doc = Document::new("shared", Size::new(64, 48), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8);
        let mut raster = photocraft_raster::Surface::new(doc.pixel_format());
        raster.fill_rect(Rect::from_size(doc.size), &[0.2, 0.4, 0.6, 1.0]);
        doc.layers.push(Layer::new("Painting", LayerContent::Raster(raster)));
        let mut session = photocraft_engine::Session::new();
        session.add_document(doc, None);
        session
    }

    #[test]
    fn shared_look_preserves_native_layers_settings_save_and_undo() {
        let mut photo = photo();
        let original = photo.active().unwrap().doc.layers[0].clone();
        let mut vector = vectorcraft_engine::Session::new();
        let mut bridge = crate::studio_document::Bridge::attach(&mut photo, &mut vector).unwrap();
        vector.execute("shape.rectangle", &serde_json::json!({"x":8,"y":8,"width":20,"height":16})).unwrap();
        bridge.commit(&mut photo, &vector).unwrap();
        let native = photo.active().unwrap().doc.layers[1].clone();
        let mut link = Link::default();
        let mut app = None;
        link.enter(&photo, &mut app).unwrap();
        let app = app.as_mut().unwrap();
        let source = app.session.source_now(PhotoId(1), lightcraft_engine::media::SourceLevel::Full).unwrap();
        assert!(source.get(15, 15).iter().all(|v| *v > 0.9), "vector shape must reach LightCraft");
        let mut edited = DevelopSettings::default();
        edited.light.exposure = 0.65;
        edited.color.saturation = 12.0;
        app.session.set_develop(PhotoId(1), edited.clone(), "Shared look").unwrap();
        let expected = app.session.render_now(PhotoId(1), 64, 48).unwrap().image;
        link.commit(&mut photo, app).unwrap();
        let doc = &photo.active().unwrap().doc;
        assert_eq!(doc.layers[0], original);
        assert_eq!(doc.layers[1], native);
        assert_eq!(doc.layers.len(), 3);
        let actual = photocraft_compose::render(doc, Rect::from_size(doc.size)).to_rgba8();
        let max_error = actual.pixels.chunks_exact(4).zip(&expected.data).flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c]))).max().unwrap();
        assert!(max_error <= 3, "LUT vs native LightCraft: {max_error} / 255");
        let saved = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
        let restored = photocraft_format::load_from_bytes(&saved).unwrap();
        assert_eq!(settings(&restored).unwrap(), edited);
        assert_eq!(restored.layers[0], original);
        assert_eq!(restored.layers[1], native);
        let mut reopened = photocraft_engine::Session::new();
        reopened.add_document(restored, None);
        let mut reopened_light = None;
        Link::default().enter(&reopened, &mut reopened_light).unwrap();
        let second = reopened_light.as_mut().unwrap().session.render_now(PhotoId(1), 64, 48).unwrap().image;
        assert_eq!(expected, second, "reopening must not apply the look twice");
        assert!(photo.undo());
        assert_eq!(photo.active().unwrap().doc.layers.len(), 2);
        let before = photo.active().unwrap().doc.clone();
        edited.light.shadows = 30.0;
        app.session.set_develop(PhotoId(1), edited, "Unsupported spatial tone").unwrap();
        assert!(link.commit(&mut photo, app).is_err());
        assert!(Arc::ptr_eq(&before, &photo.active().unwrap().doc));
    }

    #[test]
    fn unsupported_operations_and_modified_look_are_not_flattened() {
        let mut photo = photo();
        let mut link = Link::default();
        let mut app = None;
        link.enter(&photo, &mut app).unwrap();
        let mut edited = DevelopSettings::default();
        edited.light.shadows = 20.0;
        let before = photo.active().unwrap().doc.clone();
        app.as_mut().unwrap().session.set_develop(PhotoId(1), edited.clone(), "Shadows").unwrap();
        assert!(link.commit(&mut photo, app.as_ref().unwrap()).unwrap_err().contains("supports Exposure"));
        assert!(Arc::ptr_eq(&before, &photo.active().unwrap().doc));
        edited.light.shadows = 0.0;
        edited.light.exposure = 0.5;
        app.as_mut().unwrap().session.set_develop(PhotoId(1), edited, "Exposure").unwrap();
        link.commit(&mut photo, app.as_ref().unwrap()).unwrap();
        photo
            .edit("Hide look", |doc, _| {
                doc.layers.last_mut().unwrap().visible = false;
                Ok(())
            })
            .unwrap();
        let before = photo.active().unwrap().doc.clone();
        assert!(link.enter(&photo, &mut app).is_err());
        assert!(Arc::ptr_eq(&before, &photo.active().unwrap().doc));
        // A vector layer added after a look remains beneath it.
        let mut vector = vectorcraft_engine::Session::new();
        crate::studio_document::Bridge::attach(&mut photo, &mut vector).unwrap();
        assert!(is_look(photo.active().unwrap().doc.layers.last().unwrap()));
    }
}

fn is_look(layer: &Layer) -> bool {
    matches!(&layer.content, LayerContent::Adjustment(Adjustment::ColorLookup { name, .. }) if name == LOOK_NAME || name.starts_with(PREFIX))
}

fn supported(s: &DevelopSettings) -> Result<(), String> {
    let mut allowed = DevelopSettings::default();
    allowed.light.exposure = s.light.exposure;
    allowed.wb = s.wb;
    allowed.color = s.color;
    allowed.curve = s.curve.clone();
    allowed.mixer = s.mixer;
    allowed.grading = s.grading;
    allowed.treatment = s.treatment;
    allowed.bw_mix = s.bw_mix;
    allowed.calibration = s.calibration;
    allowed.disabled_sections = s.disabled_sections.clone();
    if &allowed != s {
        return Err("Shared LightCraft currently supports Exposure, white balance, color, curves, mixer, grading and calibration. Undo other operations in LightCraft before switching; your edits are still retained there.".into());
    }
    Ok(())
}

fn working(rgb: [f32; 3]) -> [f32; 3] {
    static MATRIX: std::sync::OnceLock<lightcraft_color::Mat3> = std::sync::OnceLock::new();
    MATRIX.get_or_init(|| lightcraft_color::SRGB.to_space(&lightcraft_color::WORKING)).apply_f32(rgb.map(lightcraft_color::transfer::srgb_to_linear))
}

fn adjustment(s: &DevelopSettings) -> Result<Adjustment, String> {
    supported(s)?;
    let cube = lightcraft_raster::Rgb32f::from_fn(SIZE * SIZE, SIZE, |x, y| {
        working([(x % SIZE) as f32 / (SIZE - 1) as f32, (x / SIZE) as f32 / (SIZE - 1) as f32, y as f32 / (SIZE - 1) as f32])
    });
    let mut request = pipeline::RenderRequest::fit(SIZE * SIZE, SIZE);
    request.depth = pipeline::OutputDepth::U16;
    let rendered = pipeline::render(&cube, &Default::default(), s, &request);
    let Some(pipeline::DeepImage { samples: pipeline::DeepSamples::U16(values), .. }) = rendered.deep else {
        return Err("LightCraft did not return the requested tone/color table.".into());
    };
    if values.len() != SIZE * SIZE * SIZE * 3 {
        return Err("Unexpected LightCraft table size.".into());
    }
    Ok(Adjustment::ColorLookup {
        name: LOOK_NAME.into(),
        lut: Some(Arc::new(values.into_iter().map(|v| f32::from(v) / 65535.0).collect())),
        size: SIZE as u32,
        tetrahedral: true,
        dither: false,
    })
}

#[derive(Default)]
pub struct Link {
    key: Option<(u64, u64)>,
    saved: Option<DevelopSettings>,
}

impl Link {
    pub fn enter(&mut self, photo: &photocraft_engine::Session, app: &mut Option<Box<lightcraft_ui_egui::LightcraftApp>>) -> Result<(), String> {
        let state = photo.active().ok_or("Open a shared document first.")?;
        let key = (state.doc.id.0, state.revision);
        if self.key == Some(key) && app.is_some() {
            return Ok(());
        }
        let doc = &state.doc;
        if doc.mode != photocraft_color::ColorMode::Rgb
            || doc.depth == photocraft_color::SampleType::F32
            || doc.icc_profile.is_some()
            || doc.size.is_empty()
            || doc.size.area() > 4_194_304
        {
            return Err(
                "The shared LightCraft look currently needs an unprofiled 8/16-bit sRGB document up to 4 megapixels. Original document data remains intact."
                    .into(),
            );
        }
        let saved = settings(doc)?;
        if let Some((index, layer)) = doc.layers.iter().enumerate().find(|(_, l)| is_look(l)) {
            let plain = Layer::new("", layer.content.clone());
            if index + 1 != doc.layers.len()
                || !layer.visible
                || layer.opacity != 1.0
                || layer.fill_opacity != 1.0
                || layer.clipped
                || layer.blend != plain.blend
                || layer.effects != plain.effects
                || layer.excluded_channels != 0
                || layer.blend_if != plain.blend_if
                || layer.advanced != plain.advanced
                || layer.mask.is_some()
                || layer.vector_mask.is_some()
            {
                return Err("To edit the shared look in LightCraft, place it at the top, enable it, and use full opacity without a mask. Its current PhotoCraft state is preserved.".into());
            }
            let mut expected = adjustment(&saved)?;
            if let (Adjustment::ColorLookup { name: expected_name, .. }, LayerContent::Adjustment(Adjustment::ColorLookup { name, .. })) =
                (&mut expected, &layer.content)
            {
                expected_name.clone_from(name);
            }
            if layer.content != LayerContent::Adjustment(expected) {
                return Err("The LightCraft lookup table was modified in PhotoCraft. Undo that change before editing its LightCraft settings; the current table is preserved.".into());
            }
        }
        let mut base = (**doc).clone();
        // The look processes the underlying document once, never its own output.
        for layer in &mut base.layers {
            if is_look(layer) {
                layer.visible = false;
            }
        }
        if base.walk().into_iter().any(|(_, depth, l)| depth > 0 && is_look(l)) {
            return Err("Move the LightCraft look out of its group before editing it.".into());
        }
        let buf = photocraft_compose::render(&base, photocraft_geom::Rect::from_size(base.size));
        // LightCraft is RGB-only; preserve alpha in PhotoCraft, reject transparency here.
        if buf.px.iter().any(|p| p[3] < 0.999) {
            return Err("Shared LightCraft currently needs an opaque canvas; transparency is preserved in the project.".into());
        }
        let pixels = Arc::new(lightcraft_raster::Rgb32f::from_fn(base.size.width as usize, base.size.height as usize, |x, y| {
            buf.px.get(y * base.size.width as usize + x).map(|p| working([p[0], p[1], p[2]])).unwrap_or_default()
        }));
        let mut session = lightcraft_engine::Session::new().with_system_clock();
        let loader = pixels.clone();
        session.media.file_loader = Some(Arc::new(move |_, _| Ok(((*loader).clone(), Default::default()))));
        let id = PhotoId(1);
        let mut record =
            Photo::new(id, Source::File { path: "artcraft://shared-document".into() }, &base.name, "PNG", base.size.width, base.size.height, "");
        record.develop = Arc::new(saved.clone());
        record.content_hash = Some(format!("{:016x}{:016x}", key.0, key.1));
        session.catalog.apply(Op::AddPhoto { photo: Box::new(record) }).map_err(|e| e.to_string())?;
        session.selection = lightcraft_engine::Selection::single(id);
        let mut next = lightcraft_ui_egui::LightcraftApp::new(session, Default::default());
        next.ui.view = lightcraft_ui_egui::state::ViewMode::Detail;
        next.ui.right = lightcraft_ui_egui::state::RightPanel::Edit;
        *app = Some(Box::new(next));
        self.key = Some(key);
        self.saved = Some(saved);
        Ok(())
    }

    pub fn commit(&mut self, photo: &mut photocraft_engine::Session, app: &lightcraft_ui_egui::LightcraftApp) -> Result<(), String> {
        let id = app.session.active().ok_or("The shared LightCraft photo was closed.")?;
        if id != PhotoId(1) {
            return Err("Select the shared LightCraft photo before switching.".into());
        }
        let current = app.session.develop_of(id).ok_or("Missing LightCraft settings.")?;
        if self.saved.as_ref() == Some(&*current) {
            return Ok(());
        }
        let state = photo.active().ok_or("Missing shared document.")?;
        if self.key != Some((state.doc.id.0, state.revision)) {
            return Err("The parent document changed while LightCraft was active; its data has been preserved.".into());
        }
        let look = adjustment(&current)?;
        let native_settings = serde_json::to_string(&*current).map_err(|e| e.to_string())?;
        photo
            .edit("Edit LightCraft tone/color", |doc, _| {
                let layer_id = if let Some(layer) = doc.layers.iter_mut().find(|l| is_look(l)) {
                    layer.content = LayerContent::Adjustment(look);
                    layer.id
                } else {
                    let layer = Layer::new(LOOK_NAME, LayerContent::Adjustment(look));
                    let id = layer.id;
                    doc.layers.push(layer);
                    id
                };
                let key = format!("{PREFIX}{}", layer_id.0);
                doc.metadata.text.retain(|(k, _)| *k != key);
                doc.metadata.text.push((key, native_settings));
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        self.saved = Some((*current).clone());
        self.key = photo.active().map(|s| (s.doc.id.0, s.revision));
        Ok(())
    }
}
