//! Real Craft CLI adapters for the retained document shell. The graph and source
//! are authoritative; PNG files are disposable 8-bit sRGB evaluation caches.
use artcraft_launcher_core::CraftApp;
use artcraft_launcher_core::document::{Document, EngineStamp, Operation, Program};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Engine {
    pub exe: PathBuf,
    pub stamp: EngineStamp,
}
pub type Engines = BTreeMap<Program, Engine>;

pub fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
fn file_digest(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Reuse the launcher's install detection. Never accept an executable path from a project.
pub fn discover() -> Engines {
    let paths = crate::paths::Paths::new().ok();
    let manifest = paths.map(|p| crate::manifest::Manifest::load(&p.manifest())).unwrap_or_default();
    let snapshot = crate::detect::Snapshot::take();
    Program::ALL
        .into_iter()
        .filter_map(|program| {
            let app = CraftApp::by_id(program.id())?;
            let installed = crate::detect::detect(app, &manifest, &snapshot)?;
            let installed_path = PathBuf::from(installed.location);
            let exe = installed_path.with_file_name(format!("{}-cli{}", program.id(), std::env::consts::EXE_SUFFIX));
            if !exe.is_file() {
                return None;
            }
            let stamp = EngineStamp { version: installed.version.map(|v| v.to_string()).unwrap_or_default(), sha256: file_digest(&exe).ok()? };
            Some((program, Engine { exe, stamp }))
        })
        .collect()
}

pub fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > limit {
        return Err("File exceeds the document size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("File grew beyond the document size limit".into());
    }
    Ok(bytes)
}

pub fn open_source(path: &Path, engines: &Engines) -> Result<Document, String> {
    let source = read_limited(path, 32 * 1024 * 1024)?;
    let _ = decode(&source)?;
    Ok(Document::new(
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        source.clone(),
        digest(&source),
        engines.iter().map(|(p, e)| (*p, e.stamp.clone())).collect(),
    ))
}

pub fn load(path: &Path) -> Result<Document, String> {
    let bytes = read_limited(path, 192 * 1024 * 1024)?;
    let doc: Document = serde_json::from_slice(&bytes).map_err(|e| format!("Invalid shell document: {e}"))?;
    doc.validate()?;
    if digest(&doc.source) != doc.source_sha256 {
        return Err("The original source checksum does not match".into());
    }
    let _ = decode(&doc.source)?;
    Ok(doc)
}

pub fn save(path: &Path, doc: &Document) -> Result<(), String> {
    doc.validate()?;
    if digest(&doc.source) != doc.source_sha256 {
        return Err("Original source checksum mismatch".into());
    }
    let bytes = serde_json::to_vec(doc).map_err(|e| e.to_string())?;
    atomic_write(path, &bytes)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or("No destination folder")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(".shell-{}-{}.tmp", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|e| e.to_string())?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&temp, path).map_err(|e| format!("Could not commit {}: {e}", path.display()))
}

fn decode(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| format!("This build supports 8-bit PNG and JPEG images: {e}"))?;
    if !matches!(image.color(), image::ColorType::Rgb8 | image::ColorType::Rgba8 | image::ColorType::L8 | image::ColorType::La8) {
        return Err("This build cannot preserve higher-bit-depth images; the source was not changed".into());
    }
    Ok(image)
}

fn run(engine: &Engine, args: &[String], dir: &Path) -> Result<(), String> {
    let mut command = crate::process::command(&engine.exe);
    command
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PHOTOCRAFT_CONFIG_DIR", dir.join("profile"))
        .env("PHOTOCRAFT_NO_SYSTEM_FONTS", "1")
        .env("VECTORCRAFT_NO_PREFS", "1")
        .env("LIGHTCRAFT_NO_PREFS", "1")
        .env("TEMP", dir)
        .env("TMP", dir);
    let mut child = command.spawn().map_err(|e| format!("Could not start {}: {e}", engine.exe.display()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(s) = stdout {
            let _ = s.take(256 * 1024).read_to_end(&mut bytes);
        }
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(s) = stderr {
            let _ = s.take(256 * 1024).read_to_end(&mut bytes);
        }
        bytes
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if start.elapsed() < Duration::from_secs(90) => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("Engine exceeded its 90-second time limit".to_owned());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(e.to_string());
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status?.success() {
        return Err(format!("Engine failed: {} {}", String::from_utf8_lossy(&stderr), String::from_utf8_lossy(&stdout)));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Rendered {
    pub png: Vec<u8>,
    pub evaluated: usize,
    pub cached: usize,
    pub elapsed_ms: u128,
    pub native_documents: Vec<PathBuf>,
}

pub fn evaluate(doc: &Document, engines: &Engines, cache: &Path) -> Result<Rendered, String> {
    let start = Instant::now();
    doc.validate()?;
    if digest(&doc.source) != doc.source_sha256 {
        return Err("Source checksum mismatch".into());
    }
    let graph = doc.graph()?;
    let order = graph.evaluation_order()?;
    // Check pinning even for cache hits. Do not silently switch an old project to a new engine.
    for program in
        order.iter().filter_map(|id| graph.node(*id)).filter(|n| n.enabled).map(|n| n.operation.program()).collect::<std::collections::BTreeSet<_>>()
    {
        let engine = engines.get(&program).ok_or_else(|| format!("{} engine is unavailable", program.name()))?;
        if doc.engines.get(&program) != Some(&engine.stamp) || file_digest(&engine.exe)? != engine.stamp.sha256 {
            return Err(format!("{} changed version. Keep the original engine available to replay this document", program.name()));
        }
    }
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let mut png = Vec::new();
    decode(&doc.source)?.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    let (mut evaluated, mut cached) = (0, 0);
    let mut native_documents = Vec::new();
    for id in order {
        let node = graph.node(id).ok_or("Missing operation")?;
        if !node.enabled {
            continue;
        }
        let engine = engines.get(&node.operation.program()).ok_or("Missing engine")?;
        let key = digest(&serde_json::to_vec(&(digest(&png), &node.operation, &engine.stamp, "shell-srgb8-v1")).map_err(|e| e.to_string())?);
        let dir = cache.join(key);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let input = dir.join("input.png");
        let output = dir.join("output.png");
        let native = dir.join(match node.operation.program() {
            Program::PhotoCraft => "editable.pcraft",
            Program::VectorCraft => "editable.vectorcraft",
            Program::LightCraft => "settings.json",
        });
        let marker = dir.join("complete.sha256");
        let cached_output = read_limited(&output, 64 * 1024 * 1024).ok().filter(|data| {
            std::fs::read_to_string(&marker).ok().is_some_and(|hash| hash == digest(data)) && native.is_file() && decode(data).is_ok()
        });
        if let Some(data) = cached_output {
            png = data;
            cached += 1;
            native_documents.push(native);
            continue;
        }
        atomic_write(&input, &png)?;
        let s = |p: &Path| p.to_string_lossy().into_owned();
        match &node.operation {
            Operation::HueSaturation { hue, saturation } | Operation::Levels { black: hue, white: saturation, .. } => {
                let (command, params) = match &node.operation {
                    Operation::HueSaturation { .. } => ("layer.newAdjustmentLayer.hueSaturation", json!({"hue":hue,"saturation":saturation})),
                    Operation::Levels { black, white, gamma } => {
                        ("layer.newAdjustmentLayer.levels", json!({"inBlack":black,"inWhite":white,"gamma":gamma}))
                    }
                    _ => return Err("Invalid photo operation".into()),
                };
                run(
                    engine,
                    &["run".into(), s(&input), "--cmd".into(), command.into(), "--params".into(), params.to_string(), "--out".into(), s(&native)],
                    &dir,
                )?;
                run(engine, &["convert".into(), s(&native), s(&output)], &dir)?;
            }
            Operation::Rectangle { x, y, width, height, color } => {
                let params = json!({"x":x,"y":y,"width":width,"height":height});
                let fill = json!({"color": format!("#{:02x}{:02x}{:02x}",color[0],color[1],color[2])});
                run(
                    engine,
                    &[
                        "run".into(),
                        "--in".into(),
                        s(&input),
                        "--cmd".into(),
                        "shape.rectangle".into(),
                        "--params".into(),
                        params.to_string(),
                        "--cmd".into(),
                        "paint.setFill".into(),
                        "--params".into(),
                        fill.to_string(),
                        "--export".into(),
                        s(&native),
                        "--export".into(),
                        s(&output),
                    ],
                    &dir,
                )?;
            }
            Operation::Exposure { stops } => {
                atomic_write(&native, &serde_json::to_vec(&node.operation).map_err(|e| e.to_string())?)?;
                run(engine, &["render".into(), s(&input), "-o".into(), s(&output), "--set".into(), format!("light.exposure={stops}")], &dir)?;
            }
        }
        png = read_limited(&output, 64 * 1024 * 1024)?;
        let _ = decode(&png)?;
        atomic_write(&marker, digest(&png).as_bytes())?;
        evaluated += 1;
        native_documents.push(native);
    }
    Ok(Rendered { png, evaluated, cached, elapsed_ms: start.elapsed().as_millis(), native_documents })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_save_replaces_and_corruption_is_reported() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("project.artcraftdoc");
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(8, 8, image::Rgba([80, 100, 120, 255]))
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let mut d = Document::new("test.png".into(), bytes.clone(), digest(&bytes), BTreeMap::new());
        save(&path, &d).unwrap();
        d.name = "changed.png".into();
        save(&path, &d).unwrap();
        assert_eq!(load(&path).unwrap().name, "changed.png");
        d.source[0] ^= 1;
        assert!(save(&path, &d).is_err());
        assert_eq!(load(&path).unwrap().name, "changed.png");
    }
    #[test]
    #[ignore = "requires the three installed Craft CLI engines"]
    fn real_engines_replay_replacement_and_keep_vector_document() {
        let engines = discover();
        assert_eq!(engines.len(), 3);
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.png");
        image::RgbImage::from_pixel(96, 64, image::Rgb([70, 110, 150])).save(&source).unwrap();
        let mut d = open_source(&source, &engines).unwrap();
        let mut g = d.graph().unwrap().clone();
        g.append(Operation::HueSaturation { hue: 30., saturation: 20. }).unwrap();
        g.append(Operation::Rectangle { x: 10., y: 10., width: 20., height: 20., color: [220, 80, 30] }).unwrap();
        g.append(Operation::Exposure { stops: 0.3 }).unwrap();
        d.commit(0, g.clone()).unwrap();
        let first = evaluate(&d, &engines, root.path()).unwrap();
        assert_eq!(first.evaluated, 3);
        assert!(first.native_documents.iter().any(|p| p.extension().is_some_and(|e| e == "vectorcraft")));
        let vector_path = first.native_documents.iter().find(|p| p.extension().is_some_and(|e| e == "vectorcraft")).unwrap();
        let vector_cli = &engines.get(&Program::VectorCraft).unwrap().exe;
        let info = crate::process::run(vector_cli.to_str().unwrap(), &["info", vector_path.to_str().unwrap()], Duration::from_secs(30)).unwrap();
        let info: serde_json::Value = serde_json::from_str(&info).unwrap();
        assert_eq!(info["kinds"]["Rectangle"], 1);
        assert_eq!(info["kinds"]["Image"], 1);
        let second = evaluate(&d, &engines, root.path()).unwrap();
        assert_eq!(second.cached, 3);
        assert_eq!(first.png, second.png);
        let mut bypassed = d.clone();
        let mut disabled = g.clone();
        for node in &mut disabled.nodes {
            node.enabled = false;
        }
        bypassed.commit(bypassed.generation, disabled).unwrap();
        let bypass = evaluate(&bypassed, &BTreeMap::new(), root.path()).unwrap();
        let original = evaluate(&open_source(&source, &engines).unwrap(), &engines, root.path()).unwrap();
        assert_eq!(bypass.png, original.png);
        assert_eq!(bypass.evaluated, 0);
        g.node_mut(1).unwrap().operation = Operation::Levels { black: 10., white: 240., gamma: 1.3 };
        d.commit(1, g).unwrap();
        let replaced = evaluate(&d, &engines, root.path()).unwrap();
        assert_ne!(first.png, replaced.png);
        d.undo().unwrap();
        assert_eq!(evaluate(&d, &engines, root.path()).unwrap().png, first.png);
        assert_eq!(file_digest(&source).unwrap(), d.source_sha256);
        let project = root.path().join("retained.artcraftdoc");
        save(&project, &d).unwrap();
        assert_eq!(evaluate(&load(&project).unwrap(), &engines, root.path()).unwrap().png, first.png);
        // A missing engine or changed pin must fail before silently substituting data.
        let mut absent = engines.clone();
        absent.remove(&Program::LightCraft);
        assert!(evaluate(&d, &absent, root.path()).is_err());
        let mut wrong_version = d.clone();
        wrong_version.engines.get_mut(&Program::PhotoCraft).unwrap().sha256 = "different".into();
        assert!(evaluate(&wrong_version, &engines, root.path()).is_err());
        // The original user example: replace an operation ten later executions back.
        let mut many = d.graph().unwrap().clone();
        for _ in 0..10 {
            many.append(Operation::Exposure { stops: 0.05 }).unwrap();
        }
        d.commit(d.generation, many.clone()).unwrap();
        let before = evaluate(&d, &engines, root.path()).unwrap();
        many.node_mut(1).unwrap().operation = Operation::Levels { black: 15., white: 240., gamma: 1.2 };
        d.commit(d.generation, many).unwrap();
        let after = evaluate(&d, &engines, root.path()).unwrap();
        assert_ne!(before.png, after.png);
        d.undo().unwrap();
        assert_eq!(evaluate(&d, &engines, root.path()).unwrap().png, before.png);
        eprintln!(
            "Three-engine proof: initial={}ms, cached={}ms, 13 operations={}ms, replacement={}ms",
            first.elapsed_ms, second.elapsed_ms, before.elapsed_ms, after.elapsed_ms
        );
        if let Some(dir) = std::env::var_os("ARTCRAFT_SHELL_EVIDENCE") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            save(&dir.join("Procedural-Test.artcraftdoc"), &d).unwrap();
            std::fs::write(dir.join("before.png"), &before.png).unwrap();
            std::fs::write(dir.join("replaced.png"), &after.png).unwrap();
            std::fs::copy(vector_path, dir.join("Editable-Rectangle.vectorcraft")).unwrap();
            std::fs::write(
                dir.join("engine-proof.json"),
                serde_json::to_vec_pretty(&json!({
                    "engines": d.engines, "native_vector": info, "initial_ms": first.elapsed_ms,
                    "cached_ms": second.elapsed_ms, "thirteen_operations_ms": before.elapsed_ms,
                    "replacement_ms": after.elapsed_ms, "source_unchanged": file_digest(&source).unwrap() == d.source_sha256,
                    "undo_restores_exact_bytes": true, "save_reopen": true, "missing_engine_rejected": true, "changed_engine_rejected": true
                }))
                .unwrap(),
            )
            .unwrap();
        }
    }
}
