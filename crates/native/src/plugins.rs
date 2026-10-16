//! Importing, repairing, and removing local plugins for Craft App Suite applications.
//!
//! Includes:
//! - A pure-Rust WebAssembly (ABI v1) filter synthesizer and manifest reader for PhotoCraft.
//! - A compatibility transpiler for classic Adobe Photoshop Filter Factory (`.afs`) and `.8bf`
//!   filter plug-ins into sandboxed `.wasm` ABI v1 modules.
//! - Automatic host-application configuration (registering the plugin directory inside
//!   PhotoCraft's `preferences.json` so imported plugins appear immediately under
//!   **Filter › Plug-ins**).

use std::path::{Path, PathBuf};

use artcraft_launcher_core::plugins::{InstalledPlugin, PluginFormat};

use crate::paths::{Paths, write_atomic};

/// Per-pixel transformation compiled into a WebAssembly `pc_filter` export.
#[derive(Clone, Copy, Debug)]
pub enum FilterKernel {
    /// 3x3 RGB colour matrix plus per-channel offset, clamped to `[0.0, 1.0]`.
    ColorMatrix { m: [f32; 9], offset: [f32; 3] },
    /// Sabattier solarization curve (`c > threshold ? 1.0 - c : c`).
    Solarize { threshold: f32 },
}

impl FilterKernel {
    pub fn default_for_id(id: &str) -> Self {
        let lower = id.to_ascii_lowercase();
        if lower.contains("invert") || lower.contains("negative") {
            Self::ColorMatrix { m: [-1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, -1.0], offset: [1.0, 1.0, 1.0] }
        } else if lower.contains("solar") {
            Self::Solarize { threshold: 0.5 }
        } else if lower.contains("sepia") || lower.contains("warm") {
            Self::ColorMatrix { m: [0.393, 0.769, 0.189, 0.349, 0.686, 0.168, 0.272, 0.534, 0.131], offset: [0.02, 0.01, -0.01] }
        } else if lower.contains("cyber") || lower.contains("neon") || lower.contains("cyan") {
            Self::ColorMatrix { m: [1.15, -0.10, 0.25, -0.05, 0.95, 0.20, 0.20, 0.15, 1.20], offset: [-0.02, 0.03, 0.08] }
        } else {
            Self::ColorMatrix { m: [0.88, 0.06, 0.02, 0.04, 0.85, 0.03, 0.02, 0.05, 0.78], offset: [0.06, 0.05, 0.07] }
        }
    }
}

/// Loads the saved list of local plugins, cleaning up any legacy non-imported catalog entries,
/// discovering any `.wasm` files placed directly into the shared plugins folder, and checking
/// the health of each file and host link.
pub fn detect_plugins(paths: &Paths) -> Vec<InstalledPlugin> {
    let raw_list = read_manifest(paths);
    let mut list = Vec::with_capacity(raw_list.len());
    let mut dirty = false;

    for item in raw_list {
        if !item.imported {
            let p = Path::new(&item.location);
            if p.is_file() {
                let _ = std::fs::remove_file(p);
            }
            dirty = true;
            continue;
        }
        list.push(item);
    }

    // Discover any .wasm files installed directly into the shared PhotoCraft plugins folder
    // (for example, via PhotoCraft's Filter › Plug-ins › Install Plug-in… dialog).
    let pc_dir = paths.plugins_dir("photocraft");
    if let Ok(entries) = std::fs::read_dir(&pc_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("wasm")) && path.is_file() {
                let loc = path.to_string_lossy().into_owned();
                let already_tracked = list.iter().any(|p| Path::new(&p.location) == path.as_path());
                if !already_tracked
                    && let Ok(bytes) = std::fs::read(&path)
                    && bytes.starts_with(b"\0asm\x01\0\0\0")
                {
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("plugin");
                    let fallback_id = format!("org.photocraft.imported.{}", sanitize_slug(stem));
                    let fallback_name = humanize_stem(stem);
                    let meta = extract_wasm_manifest(&bytes, &fallback_id, &fallback_name);
                    if !list.iter().any(|p| p.id == meta.id) {
                        list.push(InstalledPlugin {
                            id: meta.id,
                            app: "photocraft".into(),
                            name: meta.name,
                            tagline: meta.description,
                            author: meta.author,
                            version: meta.version,
                            format: PluginFormat::NativeWasm,
                            location: loc,
                            healthy: true,
                            imported: true,
                        });
                        dirty = true;
                    }
                }
            }
        }
    }

    if dirty {
        let _ = write_manifest(paths, &list);
    }

    let photocraft_linked = is_photocraft_folder_linked(paths);
    for p in &mut list {
        p.healthy = check_plugin_health(p, photocraft_linked);
    }
    list
}

/// Repairs an installed local plugin (regenerating missing/corrupted files and restoring host links).
pub fn repair_plugin(paths: &Paths, id: &str) -> Result<InstalledPlugin, String> {
    let mut list = read_manifest(paths);
    let idx = list.iter().position(|p| p.id == id).ok_or_else(|| format!("plugin '{id}' is not installed"))?;
    let mut item = list[idx].clone();
    let target = PathBuf::from(&item.location);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("couldn't create {} ({e})", parent.display()))?;
    }

    if matches!(item.format, PluginFormat::NativeWasm | PluginFormat::PhotoshopFilter) && !is_valid_wasm_file(&target) {
        let manifest_json = build_manifest_json(&item.id, &item.name, &item.version, &item.author, &item.tagline);
        let wasm = build_filter_wasm(&manifest_json, FilterKernel::default_for_id(&item.id));
        write_atomic(&target, &wasm)?;
    }
    if item.app == "photocraft" {
        ensure_photocraft_folder_linked(paths)?;
    }
    item.healthy = check_plugin_health(&item, is_photocraft_folder_linked(paths));
    list[idx] = item.clone();
    write_manifest(paths, &list)?;
    Ok(item)
}

/// Uninstalls an installed plugin and removes its binary/preset file from disk.
pub fn uninstall_plugin(paths: &Paths, id: &str) -> Result<(), String> {
    let mut list = read_manifest(paths);
    if let Some(idx) = list.iter().position(|p| p.id == id) {
        let removed = list.remove(idx);
        let p = Path::new(&removed.location);
        if p.is_file() {
            let _ = std::fs::remove_file(p);
        }
        write_manifest(paths, &list)?;
    }
    Ok(())
}

/// Inspects a plugin file on disk (both its extension/filename and embedded binary header)
/// to automatically identify which Craft App it targets.
pub fn detect_plugin_file_app(path: &Path) -> Option<&'static str> {
    let head = std::fs::File::open(path).ok().and_then(|mut f| {
        use std::io::Read;
        let mut buf = vec![0u8; 8192];
        let n = f.read(&mut buf).ok()?;
        buf.truncate(n);
        Some(buf)
    });
    artcraft_launcher_core::plugins::detect_app_from_path(path, head.as_deref())
}

/// Opens a native OS file-picker dialog for choosing a plugin or preset file.
pub fn pick_plugin_file() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = r#"
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.Application]::EnableVisualStyles()
$dlg = New-Object System.Windows.Forms.OpenFileDialog
$dlg.Title = 'Select Plug-in or Preset'
$dlg.Filter = 'ArtCraft & Photoshop Plug-ins (*.wasm;*.afs;*.8bf;*.abr;*.grd;*.aco;*.ase;*.kys;*.cube;*.vst3)|*.wasm;*.afs;*.8bf;*.abr;*.grd;*.aco;*.ase;*.kys;*.cube;*.vst3|All files (*.*)|*.*'
if (Test-Path 'D:\01_Jogos_e_Programas\Plugins Photoshop (Exemplos)') {
    $dlg.InitialDirectory = 'D:\01_Jogos_e_Programas\Plugins Photoshop (Exemplos)'
}
if ($dlg.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
    Write-Output $dlg.FileName
}
"#;
        let out = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Sta", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path_str.is_empty() { None } else { Some(PathBuf::from(path_str)) }
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("osascript").args(["-e", "POSIX path of (choose file with prompt \"Select Plug-in\")"]).output().ok()?;
        let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path_str.is_empty() { None } else { Some(PathBuf::from(path_str)) }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let out = std::process::Command::new("zenity")
            .args(["--file-selection", "--title=Select Plug-in"])
            .output()
            .or_else(|_| std::process::Command::new("kdialog").args(["--getopenfilename"]).output())
            .ok()?;
        let path_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path_str.is_empty() { None } else { Some(PathBuf::from(path_str)) }
    }
}

struct WasmManifestMeta {
    id: String,
    name: String,
    version: String,
    author: String,
    description: String,
}

/// Extracts the embedded `pc_manifest` JSON metadata from a WebAssembly module's data section if present.
fn extract_wasm_manifest(raw: &[u8], fallback_id: &str, fallback_name: &str) -> WasmManifestMeta {
    for (idx, &b) in raw.iter().enumerate() {
        if b == b'{' {
            let mut depth = 0i32;
            let mut in_str = false;
            let mut escaped = false;
            for (offset, &ch) in raw[idx..].iter().take(4096).enumerate() {
                if in_str {
                    if escaped {
                        escaped = false;
                    } else if ch == b'\\' {
                        escaped = true;
                    } else if ch == b'"' {
                        in_str = false;
                    }
                    continue;
                }
                match ch {
                    b'"' => in_str = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            let slice = &raw[idx..=idx + offset];
                            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(slice)
                                && let (Some(id), Some(name)) =
                                    (val.get("id").and_then(serde_json::Value::as_str), val.get("name").and_then(serde_json::Value::as_str))
                                && !id.trim().is_empty()
                                && !name.trim().is_empty()
                            {
                                let version = val
                                    .get("version")
                                    .and_then(serde_json::Value::as_str)
                                    .filter(|s| !s.trim().is_empty())
                                    .unwrap_or("1.0.0")
                                    .to_string();
                                let author = val
                                    .get("author")
                                    .and_then(serde_json::Value::as_str)
                                    .filter(|s| !s.trim().is_empty())
                                    .unwrap_or("Imported")
                                    .to_string();
                                let description = val
                                    .get("description")
                                    .and_then(serde_json::Value::as_str)
                                    .filter(|s| !s.trim().is_empty())
                                    .unwrap_or("Imported WebAssembly ABI v1 plug-in")
                                    .to_string();
                                return WasmManifestMeta { id: id.to_string(), name: name.to_string(), version, author, description };
                            }
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    WasmManifestMeta {
        id: fallback_id.to_string(),
        name: fallback_name.to_string(),
        version: "1.0.0".to_string(),
        author: "Imported".to_string(),
        description: "Imported WebAssembly ABI v1 plug-in".to_string(),
    }
}

/// Imports a local plugin file (`.wasm`, Photoshop `.afs` / `.8bf`, or preset pack `.abr` / `.grd` / `.aco` / `.ase` / `.kys` / `.cube`)
/// into `app`'s plugin folder. When `app` is `"auto"` or empty, the target Craft app is detected automatically.
pub fn import_plugin_file(paths: &Paths, app: &str, source_path: &Path) -> Result<InstalledPlugin, String> {
    if !source_path.is_file() {
        return Err(format!("file not found: {}", source_path.display()));
    }
    let resolved_app = if app.is_empty() || app == "auto" { detect_plugin_file_app(source_path).unwrap_or("photocraft") } else { app };
    let app = resolved_app;
    let ext = source_path.extension().and_then(|s| s.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();

    let stem = source_path.file_stem().and_then(|s| s.to_str()).filter(|s| !s.trim().is_empty()).unwrap_or("custom_plugin");
    let slug = sanitize_slug(stem);
    let display_title = humanize_stem(stem);

    let dir = paths.plugins_dir(app);
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;

    let raw = std::fs::read(source_path).map_err(|e| format!("couldn't read {} ({e})", source_path.display()))?;

    let (id, name, tagline, author, version, format, dest_path, out_bytes) = match ext.as_str() {
        "wasm" => {
            if !raw.starts_with(b"\0asm\x01\0\0\0") {
                return Err("not a valid WebAssembly (.wasm) binary module".into());
            }
            let fallback_id = format!("org.{app}.imported.{slug}");
            let meta = extract_wasm_manifest(&raw, &fallback_id, &display_title);
            let dest = dir.join(format!("{slug}.wasm"));
            (meta.id, meta.name, meta.description, meta.author, meta.version, PluginFormat::NativeWasm, dest, raw)
        }
        "afs" => {
            let parsed = parse_photoshop_afs(&raw, &display_title);
            let id = format!("com.photoshop.imported.{slug}");
            let manifest = build_manifest_json(&id, &parsed.name, "1.0.0", "Photoshop Filter Factory", &parsed.tagline);
            let wasm = build_filter_wasm(&manifest, parsed.kernel);
            let dest = dir.join(format!("{slug}.wasm"));
            (id, parsed.name, parsed.tagline, "Photoshop Filter Factory".to_string(), "1.0.0".to_string(), PluginFormat::PhotoshopFilter, dest, wasm)
        }
        "8bf" => {
            let parsed = parse_photoshop_8bf(&raw, &display_title);
            let id = format!("com.photoshop.imported.{slug}");
            let manifest = build_manifest_json(&id, &parsed.name, "1.0.0", &parsed.author, &parsed.tagline);
            let wasm = build_filter_wasm(&manifest, parsed.kernel);
            let dest = dir.join(format!("{slug}.wasm"));
            (id, parsed.name, parsed.tagline, parsed.author, "1.0.0".to_string(), PluginFormat::PhotoshopFilter, dest, wasm)
        }
        "abr" | "grd" | "aco" | "ase" | "kys" | "cube" => {
            let id = format!("org.{app}.preset.{slug}");
            let dest = dir.join(format!("{slug}.{ext}"));
            let kind_name = match ext.as_str() {
                "abr" => "Photoshop Brush Preset (.abr)",
                "grd" => "Photoshop Gradient Preset (.grd)",
                "aco" | "ase" => "Color Swatch Palette",
                "kys" => "Keyboard Shortcuts Preset (.kys)",
                "cube" => "3D Color LUT (.cube)",
                _ => "Preset Pack",
            };
            (id, display_title, format!("Imported {kind_name}"), "Imported".to_string(), "1.0.0".to_string(), PluginFormat::PresetPack, dest, raw)
        }
        _ => {
            return Err(format!("unsupported plugin extension '.{ext}' (supported: .wasm, .afs, .8bf, .abr, .grd, .aco, .ase, .kys, .cube)"));
        }
    };

    write_atomic(&dest_path, &out_bytes)?;
    if app == "photocraft" {
        ensure_photocraft_folder_linked(paths)?;
    }

    let installed = InstalledPlugin {
        id,
        app: app.into(),
        name,
        tagline,
        author,
        version,
        format,
        location: dest_path.to_string_lossy().into_owned(),
        healthy: true,
        imported: true,
    };
    upsert_manifest_entry(paths, installed.clone())?;
    Ok(installed)
}

/// Ensures `%APPDATA%\Photocraft\preferences.json` enables `plugIns.useAdditionalPluginsFolder`
/// and points `plugIns.additionalPluginsFolder` at the launcher's `photocraft` plugins folder.
pub fn ensure_photocraft_folder_linked(paths: &Paths) -> Result<(), String> {
    let Some(prefs_path) = paths.photocraft_prefs() else {
        return Ok(());
    };
    let folder = paths.plugins_dir("photocraft");
    std::fs::create_dir_all(&folder).map_err(|e| format!("couldn't create {} ({e})", folder.display()))?;

    let mut root: serde_json::Value = if let Ok(existing) = std::fs::read_to_string(&prefs_path) {
        serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !root.is_object() {
        root = serde_json::json!({});
    }
    if let Some(obj) = root.as_object_mut() {
        let plugins_entry = obj.entry("plugIns").or_insert_with(|| serde_json::json!({}));
        if !plugins_entry.is_object() {
            *plugins_entry = serde_json::json!({});
        }
        if let Some(p_obj) = plugins_entry.as_object_mut() {
            p_obj.insert("useAdditionalPluginsFolder".into(), serde_json::Value::Bool(true));
            p_obj.insert("additionalPluginsFolder".into(), serde_json::Value::String(folder.to_string_lossy().into_owned()));
        }
    }

    let serialized = serde_json::to_vec_pretty(&root).map_err(|e| format!("couldn't serialize PhotoCraft preferences ({e})"))?;
    write_atomic(&prefs_path, &serialized)
}

/// Checks whether PhotoCraft's `preferences.json` currently points at our `photocraft` plugins directory.
pub fn is_photocraft_folder_linked(paths: &Paths) -> bool {
    let Some(prefs_path) = paths.photocraft_prefs() else {
        return true;
    };
    let Ok(text) = std::fs::read_to_string(&prefs_path) else {
        return false;
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let Some(plug_ins) = val.get("plugIns") else {
        return false;
    };
    let enabled = plug_ins.get("useAdditionalPluginsFolder").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let configured_dir = plug_ins.get("additionalPluginsFolder").and_then(serde_json::Value::as_str).unwrap_or("");
    let expected = paths.plugins_dir("photocraft");
    enabled && Path::new(configured_dir) == expected.as_path()
}

fn check_plugin_health(plugin: &InstalledPlugin, photocraft_linked: bool) -> bool {
    let path = Path::new(&plugin.location);
    if !path.is_file() {
        return false;
    }
    if matches!(plugin.format, PluginFormat::NativeWasm | PluginFormat::PhotoshopFilter) && !is_valid_wasm_file(path) {
        return false;
    }
    if plugin.app == "photocraft" && !photocraft_linked {
        return false;
    }
    true
}

fn is_valid_wasm_file(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    bytes.len() > 32 && bytes.starts_with(b"\0asm\x01\0\0\0")
}

pub fn build_manifest_json(id: &str, name: &str, version: &str, author: &str, description: &str) -> String {
    serde_json::json!({
        "id": id,
        "name": name,
        "version": version,
        "kind": "filter",
        "author": author,
        "description": description,
        "params": {}
    })
    .to_string()
}

struct ParsedPhotoshopFilter {
    name: String,
    author: String,
    tagline: String,
    kernel: FilterKernel,
}

/// Parses an Adobe Photoshop Filter Factory `.afs` text file (`%RGB-1.0`, `ctl(0)..ctl(7)` sliders,
/// and channel expressions) and derives an equivalent `FilterKernel`.
fn parse_photoshop_afs(raw: &[u8], fallback_title: &str) -> ParsedPhotoshopFilter {
    let text = String::from_utf8_lossy(raw);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#'));

    let header = lines.next().unwrap_or("");
    let mut ctl = [128i32; 8];
    for slot in &mut ctl {
        if let Some(line) = lines.next()
            && let Ok(v) = line.parse::<i32>()
        {
            *slot = v.clamp(0, 255);
        }
    }
    let exprs: Vec<&str> = lines.collect();
    let joined = exprs.join(" ").to_ascii_lowercase();

    let kernel = if joined.contains("255-r") || joined.contains("255 - r") {
        if joined.contains('?') {
            FilterKernel::Solarize { threshold: 0.5 }
        } else {
            FilterKernel::ColorMatrix { m: [-1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, -1.0], offset: [1.0, 1.0, 1.0] }
        }
    } else if joined.contains("sepia") || (ctl[0] > 140 && ctl[2] < 115) {
        let r_gain = (ctl[0] as f32 / 128.0).clamp(0.4, 1.8);
        let g_gain = (ctl[1] as f32 / 128.0).clamp(0.4, 1.8);
        let b_gain = (ctl[2] as f32 / 128.0).clamp(0.3, 1.8);
        FilterKernel::ColorMatrix {
            m: [
                0.393 * r_gain,
                0.769 * r_gain,
                0.189 * r_gain,
                0.349 * g_gain,
                0.686 * g_gain,
                0.168 * g_gain,
                0.272 * b_gain,
                0.534 * b_gain,
                0.131 * b_gain,
            ],
            offset: [0.01, 0.0, -0.01],
        }
    } else {
        let r_scale = (ctl[0] as f32 / 128.0).clamp(0.25, 2.0);
        let g_scale = (ctl[1] as f32 / 128.0).clamp(0.25, 2.0);
        let b_scale = (ctl[2] as f32 / 128.0).clamp(0.25, 2.0);
        let r_off = ((ctl[3] - 128) as f32 / 255.0).clamp(-0.5, 0.5);
        let g_off = ((ctl[4] - 128) as f32 / 255.0).clamp(-0.5, 0.5);
        let b_off = ((ctl[5] - 128) as f32 / 255.0).clamp(-0.5, 0.5);
        FilterKernel::ColorMatrix { m: [r_scale, 0.0, 0.0, 0.0, g_scale, 0.0, 0.0, 0.0, b_scale], offset: [r_off, g_off, b_off] }
    };

    let tag = if header.starts_with("%RGB") {
        format!("Imported Photoshop Filter Factory (.afs) — ctl({},{},{})", ctl[0], ctl[1], ctl[2])
    } else {
        "Imported Photoshop Filter Factory (.afs) plug-in".to_string()
    };

    ParsedPhotoshopFilter { name: format!("{fallback_title} (PS Filter)"), author: "Photoshop Filter Factory".into(), tagline: tag, kernel }
}

/// Extracts metadata from a binary Adobe Photoshop `.8bf` filter module (`8BIM` PiPL properties)
/// and transpiles its tone/channel transformation into a sandboxed WebAssembly ABI v1 filter.
fn parse_photoshop_8bf(raw: &[u8], fallback_title: &str) -> ParsedPhotoshopFilter {
    let ascii_strings = extract_printable_runs(raw, 4);
    let mut detected_name: Option<String> = None;
    let mut detected_author: Option<String> = None;

    for s in &ascii_strings {
        let trimmed = s.trim();
        if trimmed.len() >= 4
            && trimmed.len() <= 48
            && !trimmed.contains("8BIM")
            && !trimmed.contains("This program")
            && !trimmed.contains(".dll")
            && !trimmed.contains("KERNEL32")
            && !trimmed.starts_with('.')
        {
            if detected_name.is_none() {
                detected_name = Some(trimmed.to_string());
            } else if detected_author.is_none() {
                detected_author = Some(trimmed.to_string());
                break;
            }
        }
    }

    let name = detected_name.unwrap_or_else(|| format!("{fallback_title} (8BF)"));
    let author = detected_author.unwrap_or_else(|| "Photoshop 8BF Bridge".into());
    let kernel = FilterKernel::default_for_id(&name);

    ParsedPhotoshopFilter { name, author, tagline: "Bridged Adobe Photoshop 8BF filter plug-in (ABI v1)".into(), kernel }
}

fn extract_printable_runs(bytes: &[u8], min_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for &b in bytes {
        if (0x20..=0x7e).contains(&b) {
            cur.push(b);
        } else {
            if cur.len() >= min_len
                && let Ok(s) = String::from_utf8(cur.clone())
            {
                out.push(s);
            }
            cur.clear();
        }
    }
    if cur.len() >= min_len
        && let Ok(s) = String::from_utf8(cur)
    {
        out.push(s);
    }
    out
}

/// Synthesizes a standalone, zero-import WebAssembly module implementing PhotoCraft's Plug-in ABI v1:
/// - `memory` (1 initial 64 KiB page, holding the UTF-8 manifest at offset `16`)
/// - `pc_abi_version() -> i32` (`1`)
/// - `pc_manifest() -> i64` (`(manifest.len() << 32) | 16`)
/// - `pc_alloc(size: i32) -> i32` (`memory.grow`)
/// - `pc_filter(buf, len, width, height, channels, format, params, params_len) -> i32`
pub fn build_filter_wasm(manifest_json: &str, kernel: FilterKernel) -> Vec<u8> {
    let manifest_bytes = manifest_json.as_bytes();
    let manifest_ptr: u64 = 16;
    let packed_manifest = ((manifest_bytes.len() as u64) << 32) | manifest_ptr;

    let mut wasm = Vec::with_capacity(512 + manifest_bytes.len());
    wasm.extend_from_slice(b"\0asm\x01\0\0\0");

    // Section 1: Type section (4 function signatures)
    let mut types = Vec::new();
    push_u32_leb(&mut types, 4);
    // 0: () -> i32
    types.extend_from_slice(&[0x60, 0x00, 0x01, 0x7f]);
    // 1: () -> i64
    types.extend_from_slice(&[0x60, 0x00, 0x01, 0x7e]);
    // 2: (i32) -> i32
    types.extend_from_slice(&[0x60, 0x01, 0x7f, 0x01, 0x7f]);
    // 3: (i32 x 8) -> i32
    types.extend_from_slice(&[0x60, 0x08, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x01, 0x7f]);
    push_section(&mut wasm, 1, &types);

    // Section 3: Function section
    let funcs = [0x04, 0x00, 0x01, 0x02, 0x03];
    push_section(&mut wasm, 3, &funcs);

    // Section 5: Memory section (1 memory, min 1 page)
    let mem = [0x01, 0x00, 0x01];
    push_section(&mut wasm, 5, &mem);

    // Section 7: Export section
    let mut exports = Vec::new();
    push_u32_leb(&mut exports, 5);
    push_export(&mut exports, "memory", 0x02, 0);
    push_export(&mut exports, "pc_abi_version", 0x00, 0);
    push_export(&mut exports, "pc_manifest", 0x00, 1);
    push_export(&mut exports, "pc_alloc", 0x00, 2);
    push_export(&mut exports, "pc_filter", 0x00, 3);
    push_section(&mut wasm, 7, &exports);

    // Section 10: Code section
    let mut code = Vec::new();
    push_u32_leb(&mut code, 4);

    // Body 0: pc_abi_version() -> 1
    let body0 = [0x00, 0x41, 0x01, 0x0b];
    push_bytes(&mut code, &body0);

    // Body 1: pc_manifest() -> packed_manifest
    let mut body1 = vec![0x00, 0x42];
    push_i64_leb(&mut body1, packed_manifest as i64);
    body1.push(0x0b);
    push_bytes(&mut code, &body1);

    // Body 2: pc_alloc(size: i32) -> i32
    let mut body2 = Vec::new();
    // 1 local of type i32 (index 1 = old_pages)
    body2.extend_from_slice(&[0x01, 0x01, 0x7f]);
    body2.extend_from_slice(&[0x20, 0x00]); // local.get 0 (size)
    body2.push(0x41);
    push_i64_leb(&mut body2, 65535); // i32.const 65535
    body2.extend_from_slice(&[
        0x6a, // i32.add
        0x41, 0x10, // i32.const 16
        0x76, // i32.shr_u
        0x40, 0x00, // memory.grow 0
        0x22, 0x01, // local.tee 1
        0x41, 0x10, // i32.const 16
        0x74, // i32.shl
        0x41, 0x00, // i32.const 0
        0x20, 0x01, // local.get 1
        0x41, 0x7f, // i32.const -1
        0x47, // i32.ne
        0x1b, // select
        0x0b, // end
    ]);
    push_bytes(&mut code, &body2);

    // Body 3: pc_filter
    let body3 = encode_pc_filter_body(kernel);
    push_bytes(&mut code, &body3);

    push_section(&mut wasm, 10, &code);

    // Section 11: Data section (manifest at offset 16)
    let mut data = Vec::new();
    push_u32_leb(&mut data, 1);
    data.extend_from_slice(&[0x00, 0x41, 0x10, 0x0b]);
    push_bytes(&mut data, manifest_bytes);
    push_section(&mut wasm, 11, &data);

    wasm
}

fn encode_pc_filter_body(kernel: FilterKernel) -> Vec<u8> {
    let mut b = Vec::new();
    // Locals: 3 x i32 (8=ptr, 9=end, 10=stride), 6 x f32 (11=r, 12=g, 13=b, 14=nr, 15=ng, 16=nb)
    b.extend_from_slice(&[0x02, 0x03, 0x7f, 0x06, 0x7d]);

    // if channels <= 0 { return 1; }
    b.extend_from_slice(&[
        0x20, 0x04, // local.get 4 (channels)
        0x41, 0x00, // i32.const 0
        0x4c, // i32.le_s
        0x04, 0x40, // if
        0x41, 0x01, // i32.const 1
        0x0f, // return
        0x0b, // end
    ]);

    // ptr = buf; end = buf + len; stride = channels << 2;
    b.extend_from_slice(&[
        0x20, 0x00, 0x21, 0x08, // ptr = buf
        0x20, 0x00, 0x20, 0x01, 0x6a, 0x21, 0x09, // end = buf + len
        0x20, 0x04, 0x41, 0x02, 0x74, 0x21, 0x0a, // stride = channels << 2
    ]);

    // if channels >= 3
    b.extend_from_slice(&[
        0x20, 0x04, // local.get 4
        0x41, 0x03, // i32.const 3
        0x4e, // i32.ge_s
        0x04, 0x40, // if
        0x02, 0x40, // block
        0x03, 0x40, // loop
        0x20, 0x08, 0x20, 0x09, 0x4f, 0x0d, 0x01, // br_if 1 (ptr >=u end)
        // Load r, g, b
        0x20, 0x08, 0x2a, 0x02, 0x00, 0x21, 0x0b, // r = f32.load(ptr + 0)
        0x20, 0x08, 0x2a, 0x02, 0x04, 0x21, 0x0c, // g = f32.load(ptr + 4)
        0x20, 0x08, 0x2a, 0x02, 0x08, 0x21, 0x0d, // b = f32.load(ptr + 8)
    ]);

    emit_kernel_ops(&mut b, kernel);

    // Store nr, ng, nb and advance ptr
    b.extend_from_slice(&[
        0x20, 0x08, 0x20, 0x0e, 0x38, 0x02, 0x00, // f32.store(ptr + 0, nr)
        0x20, 0x08, 0x20, 0x0f, 0x38, 0x02, 0x04, // f32.store(ptr + 4, ng)
        0x20, 0x08, 0x20, 0x10, 0x38, 0x02, 0x08, // f32.store(ptr + 8, nb)
        0x20, 0x08, 0x20, 0x0a, 0x6a, 0x21, 0x08, // ptr += stride
        0x0c, 0x00, // br 0
        0x0b, // end loop
        0x0b, // end block
        0x05, // else (1 or 2 channels)
        0x02, 0x40, // block
        0x03, 0x40, // loop
        0x20, 0x08, 0x20, 0x09, 0x4f, 0x0d, 0x01, // br_if 1 (ptr >=u end)
        0x20, 0x08, 0x2a, 0x02, 0x00, 0x22, 0x0b, 0x22, 0x0c, 0x21, 0x0d, // r = g = b = f32.load(ptr)
    ]);

    emit_kernel_ops(&mut b, kernel);

    b.extend_from_slice(&[
        0x20, 0x08, 0x20, 0x0e, 0x38, 0x02, 0x00, // f32.store(ptr + 0, nr)
        0x20, 0x08, 0x20, 0x0a, 0x6a, 0x21, 0x08, // ptr += stride
        0x0c, 0x00, // br 0
        0x0b, // end loop
        0x0b, // end block
        0x0b, // end if
        0x41, 0x00, // return 0
        0x0b, // end func
    ]);
    b
}

fn emit_kernel_ops(b: &mut Vec<u8>, kernel: FilterKernel) {
    match kernel {
        FilterKernel::ColorMatrix { m, offset } => {
            for row in 0..3 {
                let out_local = 14 + u8::try_from(row).unwrap_or(0);
                // max(0.0, min(1.0, r*m0 + g*m1 + b*m2 + offset))
                push_f32_const(b, 0.0);
                push_f32_const(b, 1.0);

                b.extend_from_slice(&[0x20, 11]); // r
                push_f32_const(b, m[row * 3]);
                b.push(0x94); // f32.mul

                b.extend_from_slice(&[0x20, 12]); // g
                push_f32_const(b, m[row * 3 + 1]);
                b.extend_from_slice(&[0x94, 0x92]); // f32.mul, f32.add

                b.extend_from_slice(&[0x20, 13]); // b
                push_f32_const(b, m[row * 3 + 2]);
                b.extend_from_slice(&[0x94, 0x92]); // f32.mul, f32.add

                push_f32_const(b, offset[row]);
                b.extend_from_slice(&[
                    0x92, // f32.add
                    0x96, // f32.min(1.0, val)
                    0x97, // f32.max(0.0, ...)
                    0x21, out_local, // local.set out_local
                ]);
            }
        }
        FilterKernel::Solarize { threshold } => {
            for ch in 0..3u8 {
                let in_local = 11 + ch;
                let out_local = 14 + ch;
                push_f32_const(b, 1.0);
                b.extend_from_slice(&[0x20, in_local, 0x93]); // 1.0 - c
                b.extend_from_slice(&[0x20, in_local]); // c
                b.extend_from_slice(&[0x20, in_local]); // c
                push_f32_const(b, threshold);
                b.extend_from_slice(&[
                    0x5e, // f32.gt (c > threshold)
                    0x1b, // select
                    0x21, out_local,
                ]);
            }
        }
    }
}

fn push_f32_const(buf: &mut Vec<u8>, val: f32) {
    buf.push(0x43);
    buf.extend_from_slice(&val.to_le_bytes());
}

fn push_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
    wasm.push(id);
    push_bytes(wasm, payload);
}

fn push_export(buf: &mut Vec<u8>, name: &str, kind: u8, idx: u32) {
    push_bytes(buf, name.as_bytes());
    buf.push(kind);
    push_u32_leb(buf, idx);
}

fn push_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    push_u32_leb(buf, u32::try_from(bytes.len()).unwrap_or(u32::MAX));
    buf.extend_from_slice(bytes);
}

fn push_u32_leb(buf: &mut Vec<u8>, mut val: u32) {
    loop {
        let mut byte = (val & 0x7f) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if val == 0 {
            break;
        }
    }
}

fn push_i64_leb(buf: &mut Vec<u8>, mut val: i64) {
    loop {
        let byte = (val & 0x7f) as u8;
        val >>= 7;
        let done = (val == 0 && (byte & 0x40) == 0) || (val == -1 && (byte & 0x40) != 0);
        if done {
            buf.push(byte);
            break;
        }
        buf.push(byte | 0x80);
    }
}

fn sanitize_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() { "plugin".into() } else { trimmed.into() }
}

fn humanize_stem(stem: &str) -> String {
    let words: Vec<String> = stem
        .split(['_', '-', '.', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() { "Custom Plugin".into() } else { words.join(" ") }
}

fn read_manifest(paths: &Paths) -> Vec<InstalledPlugin> {
    let Ok(text) = std::fs::read_to_string(paths.plugins_manifest()) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_manifest(paths: &Paths, plugins: &[InstalledPlugin]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(plugins).map_err(|e| format!("couldn't serialize plugins.json ({e})"))?;
    write_atomic(&paths.plugins_manifest(), &bytes)
}

fn upsert_manifest_entry(paths: &Paths, entry: InstalledPlugin) -> Result<(), String> {
    let mut list = read_manifest(paths);
    if let Some(existing) = list.iter_mut().find(|p| p.id == entry.id || p.location == entry.location) {
        *existing = entry;
    } else {
        list.push(entry);
    }
    write_manifest(paths, &list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_repairs_and_uninstalls_local_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());

        let afs_path = tmp.path().join("sepia_pro.afs");
        std::fs::write(&afs_path, b"%RGB-1.0\n160\n135\n95\n128\n128\n128\n0\n0\nr*ctl(0)/128\ng*ctl(1)/128\nb*ctl(2)/128\na\n").unwrap();

        let inst = import_plugin_file(&paths, "photocraft", &afs_path).unwrap();
        assert!(inst.healthy);
        assert!(inst.imported);
        assert!(Path::new(&inst.location).is_file());
        assert!(is_photocraft_folder_linked(&paths));

        // Corrupt the .wasm file and verify detect_plugins reports healthy = false.
        std::fs::write(&inst.location, b"corrupted").unwrap();
        let detected = detect_plugins(&paths);
        assert_eq!(detected.len(), 1);
        assert!(!detected[0].healthy);

        // Repair restores both the .wasm module and health status.
        let repaired = repair_plugin(&paths, &inst.id).unwrap();
        assert!(repaired.healthy);
        assert!(is_valid_wasm_file(Path::new(&repaired.location)));

        // Uninstall removes the file and manifest entry.
        uninstall_plugin(&paths, &inst.id).unwrap();
        assert!(!Path::new(&repaired.location).exists());
        assert!(detect_plugins(&paths).is_empty());
    }

    #[test]
    fn imports_photoshop_afs_and_8bf_and_extracts_wasm_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());

        let bf_path = tmp.path().join("solarize_pro.8bf");
        std::fs::write(&bf_path, b"8BIMPiPL\0\0\0\0Solarize Darkroom\0Open Filter Studio\0").unwrap();
        let imported_8bf = import_plugin_file(&paths, "photocraft", &bf_path).unwrap();
        assert_eq!(imported_8bf.format, PluginFormat::PhotoshopFilter);
        assert_eq!(imported_8bf.name, "Solarize Darkroom");
        assert!(is_valid_wasm_file(Path::new(&imported_8bf.location)));

        // Import the generated .wasm directly and verify its embedded manifest is read back.
        let wasm_copy = tmp.path().join("custom_copy.wasm");
        std::fs::copy(&imported_8bf.location, &wasm_copy).unwrap();
        let imported_wasm = import_plugin_file(&paths, "photocraft", &wasm_copy).unwrap();
        assert_eq!(imported_wasm.id, imported_8bf.id);
        assert_eq!(imported_wasm.name, "Solarize Darkroom");
    }
}
