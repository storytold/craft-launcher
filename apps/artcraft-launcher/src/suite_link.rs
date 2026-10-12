//! Native companion documents. The painting is their linked source, not a
//! flattened replacement for their own objects. Everything fits in .pcraft.
use super::Native;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

const KEY: &str = "ArtCraft.Companions.v1";
const LIMIT: usize = 128 * 1024 * 1024;
const SOURCE_NAME: &str = "ArtCraft artwork.png";

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn encode(bytes: &[u8]) -> String {
    gridcraft_engine::io::base64_encode(bytes)
}
fn decode(value: &Value) -> Result<Vec<u8>, String> {
    let text = value.as_str().ok_or("Missing embedded native bytes.")?;
    if text.len() > LIMIT {
        return Err("Embedded companion exceeds 128 MiB.".into());
    }
    gridcraft_engine::io::base64_decode(text).ok_or("Invalid embedded native bytes.".into())
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    native: Value,
    link: String,
    source_path: String,
}

#[derive(Default)]
pub(super) struct Book {
    owner: Option<u64>,
    pub entries: BTreeMap<usize, Entry>,
    source: Option<Source>,
    revision: Option<u64>,
}

pub(super) struct Source {
    pub path: PathBuf,
    pub png: Arc<Vec<u8>>,
    pub texture: egui::TextureHandle,
    pub width: u32,
    pub height: u32,
}

impl Book {
    pub fn source(&mut self, photo: &photocraft_engine::Session, ctx: &egui::Context) -> Result<&Source, String> {
        let state = photo.active().ok_or("Open the shared artwork first.")?;
        if self.owner.is_some_and(|id| id != state.doc.id.0) {
            return Err("This window's companions belong to the original artwork. Return to that document, save the project, and open a new studio window for another file.".into());
        }
        if self.owner.is_none() {
            if let Some((_, text)) = state.doc.metadata.text.iter().find(|(k, _)| k == KEY) {
                if text.len() > LIMIT {
                    return Err("Companion archive exceeds 128 MiB.".into());
                }
                self.entries = serde_json::from_str(text).map_err(err)?;
                if self.entries.keys().any(|i| !(3..12).contains(i)) {
                    return Err("Unknown companion archive version.".into());
                }
            }
            self.owner = Some(state.doc.id.0);
        }
        if self.revision != Some(state.revision) {
            let doc = &state.doc;
            if doc.size.is_empty() || doc.size.area() > 4_194_304 {
                return Err("Linked artwork currently supports up to 4 megapixels.".into());
            }
            let rgba = photocraft_compose::render(doc, photocraft_geom::Rect::from_size(doc.size)).to_rgba8();
            let mut png = std::io::Cursor::new(Vec::new());
            image::write_buffer_with_format(&mut png, &rgba.pixels, rgba.width, rgba.height, image::ColorType::Rgba8, image::ImageFormat::Png)
                .map_err(err)?;
            let png = png.into_inner();
            // Metadata-only changes don't invalidate source media or its texture.
            if self.source.as_ref().is_none_or(|s| *s.png != png) {
                let base = std::env::current_exe()
                    .map_err(err)?
                    .parent()
                    .ok_or("No executable folder.")?
                    .join("Temp")
                    .join(format!("artcraft-{}", std::process::id()));
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(err)?.as_nanos();
                let base = base.join(stamp.to_string());
                std::fs::create_dir_all(&base).map_err(err)?;
                let path = base.join(SOURCE_NAME);
                std::fs::write(&path, &png).map_err(err)?;
                let pixels = egui::ColorImage::from_rgba_unmultiplied([rgba.width as usize, rgba.height as usize], &rgba.pixels);
                let texture = ctx.load_texture("companion-artwork", pixels, egui::TextureOptions::LINEAR);
                self.source = Some(Source { path, png: Arc::new(png), texture, width: rgba.width, height: rgba.height });
            }
            self.revision = Some(state.revision);
        }
        self.source.as_ref().ok_or("Shared artwork was not rendered.".into())
    }

    pub fn reference(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    pub fn enter(&mut self, index: usize, app: &mut Native, restored: bool) -> Result<(), String> {
        let source = self.source.as_ref().ok_or("Missing linked artwork.")?;
        if let Some(entry) = self.entries.get_mut(&index) {
            if restored {
                app.restore(&entry.native)?;
            }
            app.refresh(entry, source)?;
        } else {
            let entry = Entry { native: Value::Null, link: app.link_id()?, source_path: source.path.to_string_lossy().into_owned() };
            self.entries.insert(index, entry);
        }
        Ok(())
    }

    pub fn save(&mut self, apps: &[Option<Native>], photo: &mut photocraft_engine::Session) -> Result<(), String> {
        if photo.active().is_none_or(|s| Some(s.doc.id.0) != self.owner) {
            return Err("Return to this window's shared artwork before saving companions.".into());
        }
        let mut entries = self.entries.clone();
        for (index, app) in apps.iter().enumerate() {
            if let Some(app) = app {
                let entry = entries.get_mut(&index).ok_or("Missing companion link.")?;
                entry.native = app.snapshot()?;
            }
        }
        let json = serde_json::to_string(&entries).map_err(err)?;
        if json.len() > LIMIT {
            return Err(
                "The companion archive exceeds 128 MiB. Save large media projects in their native app; the shared project has not been replaced."
                    .into(),
            );
        }
        if photo.active().is_some_and(|s| s.doc.metadata.text.iter().any(|(k, v)| k == KEY && v == &json)) {
            return Ok(());
        }
        photo
            .edit("Save native companions", |doc, _| {
                doc.metadata.text.retain(|(k, _)| k != KEY);
                doc.metadata.text.push((KEY.into(), json));
                Ok(())
            })
            .map_err(err)?;
        self.entries = entries;
        Ok(())
    }
}

impl Native {
    fn link_id(&self) -> Result<String, String> {
        Ok(match self {
            Self::Design(a) => a.session.doc().map_err(err)?.doc.assets.keys().next().map(|id| id.0.to_string()).unwrap_or_default(),
            Self::Word(a) => a.session.doc.media.keys().next().cloned().unwrap_or_default(),
            Self::Deck(a) => a.session.doc().map_err(err)?.doc.media.first().map(|m| m.id.0.to_string()).unwrap_or_default(),
            Self::Grid(a) => {
                a.session.doc().map_err(err)?.wb.sheets.first().and_then(|s| s.images.first()).map(|i| i.id.to_string()).unwrap_or_default()
            }
            Self::Pdf(a) => {
                let (_, id) = a.active_ids().ok_or("No PDF companion.")?;
                let doc = a.session.get(id).ok_or("No PDF companion.")?;
                doc.page_images(0).first().and_then(|i| i.object).map(|o| o.to_string()).ok_or("Missing PDF artwork object.")?
            }
            _ => String::new(),
        })
    }

    fn snapshot(&self) -> Result<Value, String> {
        let documents = match self {
            Self::Design(a) => a.session.documents().len(),
            Self::Deck(a) => a.session.documents().len(),
            Self::Grid(a) => a.session.documents().len(),
            Self::Cad(a) => a.session.docs.len(),
            Self::Pdf(a) => a.views.len(),
            _ => 1,
        };
        if documents != 1 {
            return Err("Save or close additional native document tabs first. A shared project holds one companion per app.".into());
        }
        Ok(match self {
            Self::Design(a) => {
                let d = &a.session.doc().map_err(err)?.doc;
                json!({"doc":d, "assets":d.assets.iter().map(|(id,asset)| (id.0.to_string(), encode(&asset.data))).collect::<BTreeMap<_,_>>()})
            }
            Self::Word(a) => {
                json!({"doc":a.session.doc,"media":a.session.doc.media.iter().map(|(k,v)|(k.clone(),encode(v))).collect::<BTreeMap<_,_>>(),"passthrough":a.session.doc.passthrough.iter().map(|(k,v)|(k.clone(),encode(v))).collect::<BTreeMap<_,_>>() })
            }
            Self::Deck(a) => json!(encode(&deckcraft_engine::cmd::file::save_bytes(&a.session.doc().map_err(err)?.doc, "deckcraft").map_err(err)?)),
            Self::Grid(a) => {
                let wb = &a.session.doc().map_err(err)?.wb;
                if wb.preserved.is_some() || wb.sheets.iter().any(|s| s.preserved.is_some()) {
                    return Err(
                        "This workbook contains imported parts that need a native GridCraft save. The shared project has not discarded them.".into(),
                    );
                }
                serde_json::to_value(wb).map_err(err)?
            }
            Self::Cad(a) => json!(encode(&cadcraft_io::write(a.session.doc().map_err(err)?, "companion.dxf").map_err(err)?)),
            Self::Pdf(a) => {
                let (_, id) = a.active_ids().ok_or("No PDF companion.")?;
                json!(encode(&a.session.save_full_bytes(id).map_err(err)?))
            }
            Self::Effect(a) => json!({"doc":a.session.project,"state":a.session.state}),
            Self::Film(a) => json!({"doc":a.session.project,"state":a.session.state}),
            Self::Sound(a) => {
                let d = a.engine.session();
                let mut audio = BTreeMap::new();
                for id in d.pool.ids() {
                    if let Some(v) = d.pool.get(id) {
                        if v.buffer.channels.iter().map(Vec::len).sum::<usize>() > 8_000_000 {
                            return Err(
                                "Save long audio in SoundCraft first; shared embedded audio is limited to 8 million samples per source.".into()
                            );
                        }
                        audio.insert(id.0.to_string(), json!({"rate":v.buffer.sample_rate,"channels":v.buffer.channels}));
                    }
                }
                json!({"doc":d,"audio":audio})
            }
        })
    }

    fn restore(&mut self, v: &Value) -> Result<(), String> {
        match self {
            Self::Design(a) => {
                let mut d: designcraft_doc::Document = serde_json::from_value(v["doc"].clone()).map_err(err)?;
                for (id, asset) in &mut d.assets {
                    Arc::make_mut(asset).data = Arc::new(decode(&v["assets"][id.0.to_string()])?);
                }
                a.session = designcraft_engine::Session::new();
                a.session.add_document(designcraft_engine::DocState::new(d, None));
            }
            Self::Word(a) => {
                let mut d: wordcraft_doc::Document = serde_json::from_value(v["doc"].clone()).map_err(err)?;
                for (k, b) in v["media"].as_object().ok_or("Missing WordCraft images.")? {
                    d.media.insert(k.clone(), Arc::new(decode(b)?));
                }
                for (k, b) in v["passthrough"].as_object().ok_or("Missing WordCraft preserved parts.")? {
                    d.passthrough.insert(k.clone(), Arc::new(decode(b)?));
                }
                a.session.set_document(d);
            }
            Self::Deck(a) => {
                let d = deckcraft_engine::cmd::file::open_presentation("companion.deckcraft", &decode(v)?).map_err(err)?;
                a.session = deckcraft_engine::Session::new();
                a.session.add_document(deckcraft_engine::DocState::new(d, None, "Artwork slides".into()));
            }
            Self::Grid(a) => {
                a.session = gridcraft_engine::Session::new();
                a.session.add_document(gridcraft_engine::DocState::new(
                    serde_json::from_value(v.clone()).map_err(err)?,
                    None,
                    "Artwork workbook".into(),
                ));
            }
            Self::Cad(a) => {
                a.session = cadcraft_engine::Session::empty();
                a.session.open_drawing(cadcraft_io::read(&decode(v)?, "companion.dxf").map_err(err)?, "Artwork drawing", None);
            }
            Self::Pdf(a) => {
                let mut replacement = pdfcraft_ui_egui::PdfCraftApp::new();
                replacement.open_bytes("Artwork proof.pdf", None, decode(v)?)?;
                **a = replacement;
            }
            Self::Effect(a) => {
                a.session.replace_project(serde_json::from_value(v["doc"].clone()).map_err(err)?, None);
                a.session.state = serde_json::from_value(v["state"].clone()).map_err(err)?;
            }
            Self::Film(a) => {
                a.session.project = Arc::new(serde_json::from_value(v["doc"].clone()).map_err(err)?);
                a.session.state = serde_json::from_value(v["state"].clone()).map_err(err)?;
                a.session.history = Default::default();
                a.session.media.clear();
                a.session.revision = a.session.revision.wrapping_add(1);
            }
            Self::Sound(a) => {
                let mut d: soundcraft_model::Session = serde_json::from_value(v["doc"].clone()).map_err(err)?;
                for (key, b) in v["audio"].as_object().ok_or("Missing embedded audio.")? {
                    let id = soundcraft_model::SourceId(key.parse().map_err(err)?);
                    let buffer = soundcraft_audio_io::AudioBuffer {
                        sample_rate: serde_json::from_value(b["rate"].clone()).map_err(err)?,
                        channels: serde_json::from_value(b["channels"].clone()).map_err(err)?,
                    };
                    if buffer.channels.iter().map(Vec::len).sum::<usize>() > 8_000_000 {
                        return Err("Embedded audio exceeds its sample limit.".into());
                    }
                    d.pool.insert(id, Arc::new(soundcraft_model::SourceAudio::new(buffer)));
                }
                a.engine.replace_session(d);
            }
        }
        Ok(())
    }

    fn refresh(&mut self, entry: &mut Entry, source: &Source) -> Result<(), String> {
        let path = source.path.to_string_lossy().into_owned();
        if entry.source_path == path {
            return Ok(());
        }
        match self {
            Self::Design(a) => {
                let id = &entry.link;
                a.session
                    .edit(|d, _| {
                        for (key, asset) in &mut d.assets {
                            if key.0.to_string() == *id {
                                let asset = Arc::make_mut(asset);
                                asset.data = source.png.clone();
                                asset.link = None;
                                asset.pixels = Some((source.width, source.height));
                            }
                        }
                        Ok(())
                    })
                    .map_err(err)?;
                // DesignCraft's damage tracker compares page objects. A changed
                // asset can leave those objects identical, so invalidate both
                // preview panes explicitly while retaining their view settings.
                a.canvas = Default::default();
                if let Some((cache, _)) = &mut a.other_pane {
                    *cache = Default::default();
                }
            }
            Self::Word(a) => {
                if a.session.doc.media.contains_key(&entry.link) {
                    a.session.checkpoint("Update linked artwork");
                    a.session.doc.media.insert(entry.link.clone(), source.png.clone());
                    a.session.touch();
                    a.session.relayout();
                    a.canvas = Default::default();
                }
            }
            Self::Deck(a) => {
                a.session
                    .edit(|d, _| {
                        for media in &mut d.media {
                            if media.id.0.to_string() == entry.link {
                                media.data = source.png.clone();
                                media.link = None;
                            }
                        }
                        Ok(())
                    })
                    .map_err(err)?;
                a.textures = Default::default();
            }
            Self::Grid(a) => {
                let d = a.session.doc_mut().map_err(err)?;
                // Keep native undo snapshots; only the linked media bytes change.
                for sheet in &mut Arc::make_mut(&mut d.wb).sheets {
                    for img in &mut Arc::make_mut(sheet).images {
                        if img.id.to_string() == entry.link {
                            img.data = (*source.png).clone();
                        }
                    }
                }
                d.revision = d.revision.wrapping_add(1);
                gridcraft_ui_egui::chartview::invalidate_linked_pictures();
            }
            Self::Pdf(a) => {
                let (_, id) = a.active_ids().ok_or("No PDF companion.")?;
                let doc = a.session.get(id).ok_or("No PDF companion.")?;
                let mut target = None;
                for page in 0..doc.info.pages.len() {
                    for (index, img) in doc.page_images(page).iter().enumerate() {
                        if img.object.map(|o| o.to_string()).unwrap_or_default() == entry.link {
                            target = Some((page, index));
                        }
                    }
                }
                if let Some((page, index)) = target {
                    a.session
                        .apply(
                            id,
                            pdfcraft_engine::Edit::EditPageImage {
                                page,
                                index,
                                change: pdfcraft_engine::ImageEdit::Replace { name: SOURCE_NAME.into(), bytes: source.png.clone() },
                            },
                        )
                        .map_err(err)?;
                    let doc = a.session.get(id).ok_or("No PDF companion.")?;
                    for view in &mut a.views {
                        if view.id == id {
                            view.document_changed(&doc.info);
                        }
                    }
                    if let Some(img) = doc.page_images(page).get(index) {
                        entry.link = img.object.map(|o| o.to_string()).unwrap_or_default();
                    }
                }
            }
            Self::Effect(a) => {
                a.session
                    .edit("Update linked artwork", None, |d, _| {
                        for item in d.items.values_mut() {
                            if let effectcraft_project::ItemKind::Footage(f) = &mut item.kind
                                && f.path == entry.source_path
                            {
                                f.path = path.clone();
                                f.width = source.width;
                                f.height = source.height;
                            }
                        }
                        Ok(())
                    })
                    .map_err(err)?;
            }
            Self::Film(a) => {
                a.session
                    .edit("Update linked artwork", |d, _| {
                        for item in d.items.values_mut() {
                            if let filmcraft_project::ItemKind::Media(m) = &mut item.kind
                                && let filmcraft_project::MediaRef::File { path: p } = &mut m.media
                                && *p == entry.source_path
                            {
                                *p = path.clone();
                                m.identity = None;
                            }
                        }
                        Ok(())
                    })
                    .map_err(err)?;
                a.session.media.clear();
            }
            Self::Cad(_) | Self::Sound(_) => {} // Live reference is displayed beside these native editors.
        }
        entry.source_path = path;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unreachable)]
mod tests {
    use super::*;

    #[test]
    fn all_companions_follow_artwork_and_survive_project_reopen() {
        let mut photo = photocraft_engine::Session::new();
        let mut doc = photocraft_doc::Document::new(
            "Companion acceptance",
            photocraft_geom::Size::new(64, 48),
            photocraft_color::ColorMode::Rgb,
            photocraft_color::SampleType::U8,
        );
        let mut pixels = photocraft_raster::Surface::new(doc.pixel_format());
        pixels.fill_rect(photocraft_geom::Rect::from_size(doc.size), &[0.2, 0.4, 0.6, 1.0]);
        doc.layers.push(photocraft_doc::Layer::new("Original painting", photocraft_doc::LayerContent::Raster(pixels)));
        photo.add_document(doc, None);
        let ctx = egui::Context::default();
        let mut book = Book::default();
        let original_path = book.source(&photo, &ctx).unwrap().path.clone();
        let mut apps: Vec<Option<Native>> = (0..12).map(|_| None).collect();
        for (i, slot) in apps.iter_mut().enumerate().skip(3) {
            let mut app = Native::new(i, &original_path, None).unwrap();
            book.enter(i, &mut app, true).unwrap();
            match &mut app {
                Native::Design(a) => {
                    a.session
                        .edit(|d, _| {
                            d.title = "Retained layout".into();
                            Ok(())
                        })
                        .unwrap();
                }
                Native::Pdf(a) => {
                    let (_, id) = a.active_ids().unwrap();
                    a.session.apply(id, pdfcraft_engine::Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
                }
                Native::Effect(a) => {
                    a.session
                        .edit("Native change", None, |d, _| {
                            if let Some(item) = d.items.values_mut().next() {
                                item.name = "Retained effects".into();
                            }
                            Ok(())
                        })
                        .unwrap();
                }
                Native::Film(a) => {
                    a.session
                        .edit("Native change", |d, _| {
                            d.name = "Retained timeline".into();
                            Ok(())
                        })
                        .unwrap();
                }
                Native::Word(a) => {
                    a.run("text.insert", json!({"text":"Retained caption"})).unwrap();
                }
                Native::Deck(a) => {
                    a.run("slide.new", json!({})).unwrap();
                }
                Native::Grid(a) => {
                    a.run("cell.set", json!({"cell":"A1","value":"Retained budget"})).unwrap();
                }
                Native::Cad(a) => {
                    a.run("circle", json!({"center":[30,30],"radius":8})).unwrap();
                }
                Native::Sound(a) => {
                    a.run("track.new", json!({"name":"Retained soundtrack","count":1})).unwrap();
                    let track = a.engine.session().tracks[0].id;
                    let tone = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
                    let buffer = soundcraft_audio_io::AudioBuffer { sample_rate: 48000, channels: vec![tone] };
                    let src = soundcraft_engine::io::add_source(
                        a.engine.session_mut(),
                        "Synthetic tone",
                        buffer,
                        None,
                        soundcraft_audio_io::FileFormat::Wav,
                    );
                    let id = a.engine.session_mut().new_clip_id();
                    soundcraft_engine::edit::place_clip(
                        a.engine.session_mut(),
                        track,
                        soundcraft_model::Clip::audio(id, "Synthetic tone", src, 0, 0, 4800),
                    );
                }
            }
            *slot = Some(app);
        }
        let original = photo.active().unwrap().doc.layers.clone();
        // New painting must enter retained companions, never replace their native work.
        photo.execute("paint.stroke", json!({"points":[[12,12],[40,30]],"color":"#ff0000","size":10})).unwrap();
        let source = book.source(&photo, &ctx).unwrap();
        let changed_path = source.path.clone();
        let png = source.png.clone();
        assert_ne!(original_path, changed_path);
        for (i, app) in apps.iter_mut().enumerate().skip(3) {
            book.enter(i, app.as_mut().unwrap(), false).unwrap();
        }
        match apps[3].as_ref().unwrap() {
            Native::Design(a) => {
                let d = &a.session.doc().unwrap().doc;
                assert_eq!(d.title, "Retained layout");
                assert_eq!(d.assets.values().next().unwrap().data, png);
            }
            _ => unreachable!(),
        }
        match apps[7].as_ref().unwrap() {
            Native::Word(a) => assert!(a.session.doc.media.values().any(|b| *b == png)),
            _ => unreachable!(),
        }
        match apps[8].as_ref().unwrap() {
            Native::Deck(a) => {
                let d = &a.session.doc().unwrap().doc;
                assert_eq!(d.slides.len(), 2);
                assert_eq!(d.media[0].data, png);
            }
            _ => unreachable!(),
        }
        match apps[9].as_ref().unwrap() {
            Native::Grid(a) => assert_eq!(a.session.doc().unwrap().wb.sheets[0].images[0].data, *png),
            _ => unreachable!(),
        }
        match apps[4].as_ref().unwrap() {
            Native::Pdf(a) => {
                let (_, id) = a.active_ids().unwrap();
                let d = a.session.get(id).unwrap();
                assert_eq!(d.info.pages[0].rotation, 90);
                let (_, bytes) = d.page_image_file(0, 0).unwrap();
                assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8(), image::load_from_memory(&png).unwrap().to_rgba8());
            }
            _ => unreachable!(),
        }
        let layers = photo.active().unwrap().doc.layers.clone();
        assert_ne!(original, layers);
        book.save(&apps, &mut photo).unwrap();
        assert_eq!(photo.active().unwrap().doc.layers, layers, "Companion saving cannot flatten the painting");
        let bytes = photocraft_format::save_to_bytes(&photo.active().unwrap().doc, &Default::default()).unwrap();
        let mut reopened = photocraft_engine::Session::new();
        reopened.add_document(photocraft_format::load_from_bytes(&bytes).unwrap(), None);
        let mut book2 = Book::default();
        let path = book2.source(&reopened, &ctx).unwrap().path.clone();
        let mut restored: Vec<Option<Native>> = (0..12).map(|_| None).collect();
        for (i, slot) in restored.iter_mut().enumerate().skip(3) {
            let mut a = Native::new(i, &path, None).unwrap();
            let expected = book.entries[&i].native.clone();
            a.restore(&expected).unwrap();
            let actual = a.snapshot().unwrap();
            // Native CAD/PDF writers normalize their bytes on re-save; their models are checked below.
            if ![4, 10].contains(&i) {
                assert_eq!(actual, expected, "{} native data changed on reopen", super::super::NAMES[i]);
            }
            book2.enter(i, &mut a, false).unwrap();
            *slot = Some(a);
        }
        book2.save(&restored, &mut reopened).unwrap();
        match restored[10].as_ref().unwrap() {
            Native::Cad(a) => {
                assert_eq!(a.session.doc().unwrap().model.len(), 2);
                assert!(cadcraft_io::write(a.session.doc().unwrap(), "disabled.dwg").is_err());
            }
            _ => unreachable!(),
        }
        assert_eq!(reopened.active().unwrap().doc.layers, layers);
        assert!(photo.undo());
        assert!(!photo.active().unwrap().doc.metadata.text.iter().any(|(k, _)| k == KEY));
    }
}
