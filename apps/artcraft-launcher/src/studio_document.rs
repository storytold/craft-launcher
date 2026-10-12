//! Native vector subdocument inside a PhotoCraft smart layer. Pixels are a
//! replaceable display cache; save/reopen retains both editors' native models.
use photocraft_doc::{Document, Layer, LayerContent, LayerId, SmartObject, SmartSource};
use photocraft_geom::{Affine, Rect};
use std::sync::Arc;

const SOURCE: &str = "ArtCraft.vectorcraft";
const MAX_PIXELS: u64 = 4_194_304;

pub struct Bridge {
    pub photo_id: u64,
    pub layer: LayerId,
    pub vector_uid: u64,
    pub committed_revision: u64,
    source: Arc<Vec<u8>>,
}

pub struct Snapshot {
    pub key: [u64; 5],
    pub display: Arc<photocraft_engine::display_color::CanvasDisplay>,
    photo: Arc<Document>,
    vector: Arc<vectorcraft_doc::Document>,
    layer: LayerId,
    committed: bool,
}

impl Snapshot {
    pub fn render(self) -> Result<Document, String> {
        vectorcraft_engine::guard::catch_panic(|| self.render_inner())?
    }

    fn render_inner(self) -> Result<Document, String> {
        // A committed native revision already has a depth/profile-correct cache.
        if self.committed && self.photo.layer(self.layer).is_some_and(|layer| matches!(&layer.content, LayerContent::Smart(s) if s.cache.is_some())) {
            return Ok((*self.photo).clone());
        }
        let size = self.photo.size;
        let rendered = vectorcraft_render::Renderer::new().render_region(
            &self.vector,
            vectorcraft_geom::Rect::new(0.0, 0.0, f64::from(size.width), f64::from(size.height)),
            1.0,
            false,
        );
        let bytes = rendered.to_straight();
        if rendered.width != size.width || rendered.height != size.height || bytes.len() as u64 != size.area() * 4 {
            return Err("Vector renderer returned an unexpected display size.".into());
        }
        let surface = photocraft_raster::Surface::from_interleaved(photocraft_color::PixelFormat::RGBA8, Rect::from_size(size), &bytes);
        // VectorCraft's display render is sRGB. Convert that cache into the
        // owning document's actual profile, channel model and sample depth.
        // The original native vector colors remain in the embedded source.
        let surface = surface.convert(photocraft_color::PixelFormat { sample: self.photo.depth, ..photocraft_color::PixelFormat::RGBA8 });
        let destination = photocraft_engine::color_cmds::document_profile(&self.photo);
        let transform =
            photocraft_cms::Transform::new(photocraft_cms::builtin::srgb(), &destination, photocraft_cms::Intent::RelativeColorimetric, true)
                .map_err(|e| e.to_string())?;
        let surface =
            photocraft_engine::color_cmds::convert_surface(&surface, photocraft_color::ColorMode::Rgb, self.photo.pixel_format(), &transform);
        let mut doc = (*self.photo).clone();
        let layer = doc.layer_mut(self.layer).ok_or("The shared vector layer was removed.")?;
        if let LayerContent::Smart(smart) = &mut layer.content {
            smart.cache = Some(surface);
        }
        Ok(doc)
    }
}

fn supported(doc: &Document) -> Result<(), String> {
    if doc.size.is_empty() || doc.size.area() > MAX_PIXELS {
        return Err("Shared viewport preview supports documents up to 4 megapixels.".into());
    }
    let embedded = doc
        .walk()
        .into_iter()
        .filter(|(_, _, layer)| {
            matches!(&layer.content,
        LayerContent::Smart(smart) if matches!(&smart.source, SmartSource::Embedded { file_name, .. } if file_name == SOURCE))
        })
        .count();
    if embedded > 1 {
        return Err("This preview supports one VectorCraft contents layer per project; duplicated contents remain preserved.".into());
    }
    Ok(())
}

fn editable(layer: &Layer) -> Result<&SmartObject, String> {
    let LayerContent::Smart(smart) = &layer.content else {
        return Err("The shared vector layer was rasterized. Its former vectors will not overwrite it.".into());
    };
    if smart.transform != Affine::IDENTITY
        || smart.warp.is_some()
        || smart.perspective.is_some()
        || !smart.smart_filters.is_empty()
        || smart.stack_mode.is_some()
    {
        return Err("Undo the smart-object transform or filter before editing its vector contents in this preview.".into());
    }
    Ok(smart)
}

impl Bridge {
    pub fn attach(photo: &mut photocraft_engine::Session, vector: &mut vectorcraft_engine::Session) -> Result<Self, String> {
        let state = photo.active().ok_or("Open a document first.")?;
        supported(&state.doc)?;
        let photo_id = state.doc.id.0;
        let found = state.doc.walk().into_iter().map(|(_, _, layer)| layer).find(
            |l| matches!(&l.content, LayerContent::Smart(s) if matches!(&s.source, SmartSource::Embedded { file_name, .. } if file_name == SOURCE)),
        );
        let (layer, document) = if let Some(layer) = found {
            let smart = editable(layer)?;
            let SmartSource::Embedded { bytes, .. } = &smart.source else { return Err("Vector source is not embedded.".into()) };
            (layer.id, vectorcraft_format::load(bytes).map_err(|e| e.to_string())?)
        } else {
            let mut document = vectorcraft_doc::Document::new(f64::from(state.doc.size.width), f64::from(state.doc.size.height));
            document.title = state.doc.name.clone();
            document.units = vectorcraft_doc::Unit::Pixels;
            let bytes = vectorcraft_format::save_with(&document, &Default::default()).map_err(|e| e.to_string())?;
            let layer = Layer::new(
                "VectorCraft",
                LayerContent::Smart(SmartObject::new(
                    SmartSource::Embedded { file_name: SOURCE.into(), bytes: Arc::new(bytes) },
                    Affine::IDENTITY,
                    None,
                )),
            );
            let id = layer.id;
            photo
                .edit("Add VectorCraft contents", |doc, _| {
                    // A shared LightCraft look stays above new vector contents.
                    let at = doc
                        .layers
                        .iter()
                        .position(|l| {
                            matches!(&l.content,
                        LayerContent::Adjustment(photocraft_doc::Adjustment::ColorLookup { name, .. })
                        if name == "LightCraft tone / color" || name.starts_with("ArtCraft.LightCraft.v1:"))
                        })
                        .unwrap_or(doc.layers.len());
                    doc.layers.insert(at, layer);
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            (id, document)
        };
        let source = match &photo.active().ok_or("No shared document.")?.doc.layer(layer).ok_or("No shared layer.")?.content {
            LayerContent::Smart(SmartObject { source: SmartSource::Embedded { bytes, .. }, .. }) => bytes.clone(),
            _ => return Err("No embedded vector source.".into()),
        };
        vector.add_document(document, None);
        let state = vector.active().ok_or("Vector document was not opened.")?;
        Ok(Self { photo_id, layer, vector_uid: state.uid, committed_revision: state.revision, source })
    }

    pub fn resume(&self, photo: &photocraft_engine::Session, vector: &mut vectorcraft_engine::Session) -> Result<bool, String> {
        let Some(state) = photo.active().filter(|s| s.doc.id.0 == self.photo_id) else { return Ok(false) };
        let Some(layer) = state.doc.layer(self.layer) else { return Ok(false) };
        let smart = editable(layer)?;
        let same = matches!(&smart.source, SmartSource::Embedded { bytes, .. } if bytes == &self.source);
        let index = vector.documents().iter().position(|s| s.uid == self.vector_uid);
        Ok(same && index.is_some_and(|i| vector.set_active(i)))
    }

    pub fn snapshot(&self, photo: &photocraft_engine::Session, vector: &vectorcraft_engine::Session) -> Result<Snapshot, String> {
        let p = photo.active().filter(|s| s.doc.id.0 == self.photo_id).ok_or("The shared PhotoCraft document is no longer active.")?;
        let v = vector.active().filter(|s| s.uid == self.vector_uid).ok_or("This VectorCraft tab is outside the shared document.")?;
        supported(&p.doc)?;
        let layer = p.doc.layer(self.layer).ok_or("The shared vector layer was removed.")?;
        editable(layer)?;
        let display = photo.color.canvas_display_for(&p.doc, photo.color.main_display).map_err(|e| e.to_string())?;
        Ok(Snapshot {
            key: [p.doc.id.0, p.revision, v.uid, v.revision, display.key],
            display,
            photo: p.doc.clone(),
            vector: v.doc.clone(),
            layer: self.layer,
            committed: v.revision == self.committed_revision,
        })
    }

    #[cfg(test)]
    pub fn commit(&mut self, photo: &mut photocraft_engine::Session, vector: &vectorcraft_engine::Session) -> Result<(), String> {
        self.commit_cached(photo, vector, None)
    }

    pub fn commit_cached(
        &mut self,
        photo: &mut photocraft_engine::Session,
        vector: &vectorcraft_engine::Session,
        cached: Option<&([u64; 5], Document)>,
    ) -> Result<(), String> {
        let state = vector.active().filter(|s| s.uid == self.vector_uid).ok_or("This VectorCraft tab is outside the shared document.")?;
        if state.revision == self.committed_revision {
            return Ok(());
        }
        let revision = state.revision;
        let bytes = vectorcraft_format::save_with(&state.doc, &Default::default()).map_err(|e| e.to_string())?;
        let snapshot = self.snapshot(photo, vector)?;
        // Only the exact document, revisions and display configuration qualify.
        let mut preview = match cached.filter(|(key, _)| *key == snapshot.key) {
            Some((_, document)) => document.clone(),
            None => snapshot.render()?,
        };
        let layer = preview.layer_mut(self.layer).ok_or("The shared vector layer was removed.")?;
        let bytes = Arc::new(bytes);
        if let LayerContent::Smart(smart) = &mut layer.content {
            smart.source = SmartSource::Embedded { file_name: SOURCE.into(), bytes: bytes.clone() };
        }
        let replacement = layer.content.clone();
        let id = self.layer;
        photo
            .edit("Edit VectorCraft contents", |doc, _| {
                if let Some(layer) = doc.layer_mut(id) {
                    layer.content = replacement;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        self.committed_revision = revision;
        self.source = bytes;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_reuses_only_the_exact_preview_revision() {
        let mut photo = photocraft_engine::Session::new();
        photo.add_document(
            Document::new("cache", photocraft_geom::Size::new(64, 48), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U16),
            None,
        );
        let mut vector = vectorcraft_engine::Session::new();
        let mut bridge = Bridge::attach(&mut photo, &mut vector).unwrap();
        vector.execute("shape.rectangle", &serde_json::json!({"x":2,"y":2,"width":8,"height":8})).unwrap();
        let snapshot = bridge.snapshot(&photo, &vector).unwrap();
        let cached = (snapshot.key, snapshot.render().unwrap());
        bridge.commit_cached(&mut photo, &vector, Some(&cached)).unwrap();
        let committed = photo.active().unwrap().doc.clone();
        let cache_of = |doc: &Document| match &doc.layer(bridge.layer).unwrap().content {
            LayerContent::Smart(s) => s.cache.clone(),
            _ => None,
        };
        assert_eq!(cache_of(&committed), cache_of(&cached.1));
        vector.execute("shape.rectangle", &serde_json::json!({"x":30,"y":20,"width":20,"height":20})).unwrap();
        let expected = bridge.snapshot(&photo, &vector).unwrap().render().unwrap();
        bridge.commit_cached(&mut photo, &vector, Some(&cached)).unwrap();
        let smart_cache = |doc: &Document| match &doc.layer(bridge.layer).unwrap().content {
            LayerContent::Smart(s) => s.cache.clone(),
            _ => None,
        };
        assert_eq!(smart_cache(&photo.active().unwrap().doc), smart_cache(&expected));
        assert_ne!(smart_cache(&photo.active().unwrap().doc), smart_cache(&committed));
        assert!(photo.undo());
        assert_eq!(*photo.active().unwrap().doc, *committed);
    }
    #[test]
    fn unsupported_transform_does_not_overwrite_native_contents() {
        let mut photo = photocraft_engine::Session::new();
        photo.add_document(
            Document::new("test", photocraft_geom::Size::new(32, 24), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8),
            None,
        );
        let mut vector = vectorcraft_engine::Session::new();
        let mut bridge = Bridge::attach(&mut photo, &mut vector).unwrap();
        vector.execute("shape.rectangle", &serde_json::json!({"x":2,"y":2,"width":8,"height":8})).unwrap();
        photo
            .edit("Move contents", |doc, _| {
                if let LayerContent::Smart(smart) = &mut doc.layer_mut(bridge.layer).unwrap().content {
                    smart.transform = Affine::translate(5.0, 5.0);
                }
                Ok(())
            })
            .unwrap();
        let before = photo.active().unwrap().doc.clone();
        assert!(bridge.commit(&mut photo, &vector).is_err());
        assert!(Arc::ptr_eq(&before, &photo.active().unwrap().doc));
        assert!(bridge.resume(&photo, &mut vector).is_err());
        assert_eq!(vector.documents().len(), 1);
    }

    #[test]
    fn native_vectors_and_original_depth_round_trip_and_undo() {
        for depth in [photocraft_color::SampleType::U8, photocraft_color::SampleType::U16, photocraft_color::SampleType::F32] {
            let mut photo = photocraft_engine::Session::new();
            let mut original = Document::new("test", photocraft_geom::Size::new(64, 48), photocraft_color::ColorMode::Rgb, depth);
            let mut raster = photocraft_raster::Surface::new(original.pixel_format());
            raster.fill_rect(Rect::from_size(original.size), &[0.2, 0.4, 0.8, 1.0]);
            let original_layer = Layer::new("Original painting", LayerContent::Raster(raster));
            original.layers.push(original_layer.clone());
            photo.add_document(original, None);
            let mut vector = vectorcraft_engine::Session::new();
            let mut bridge = Bridge::attach(&mut photo, &mut vector).unwrap();
            vector.execute("shape.rectangle", &serde_json::json!({"x":8,"y":8,"width":24,"height":16})).unwrap();
            let native = vectorcraft_format::save_with(&vector.active().unwrap().doc, &Default::default()).unwrap();
            bridge.commit(&mut photo, &vector).unwrap();
            let saved = photocraft_format::save_to_bytes(&photo.active().unwrap().doc, &Default::default()).unwrap();
            let restored = photocraft_format::load_from_bytes(&saved).unwrap();
            assert_eq!(restored.depth, depth);
            assert_eq!(restored.layers.first(), Some(&original_layer));
            let LayerContent::Smart(smart) = &restored.layer(bridge.layer).unwrap().content else { panic!("missing smart layer") };
            let SmartSource::Embedded { bytes, .. } = &smart.source else { panic!("missing native vectors") };
            assert_eq!(bytes.as_ref(), &native);
            assert!(smart.cache.is_some());
            assert_eq!(smart.cache.as_ref().unwrap().format().sample, depth);
            assert!(bridge.resume(&photo, &mut vector).unwrap());
            assert_eq!(vector.documents().len(), 1);
            assert!(photo.undo());
            let LayerContent::Smart(smart) = &photo.active().unwrap().doc.layer(bridge.layer).unwrap().content else { panic!("missing smart layer") };
            let SmartSource::Embedded { bytes, .. } = &smart.source else { panic!("missing source") };
            assert_ne!(bytes.as_ref(), &native);
            assert!(!bridge.resume(&photo, &mut vector).unwrap());
        }
    }
}
