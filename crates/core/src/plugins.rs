//! Local plugin state model and file-format detection for the Craft App Suite.
//!
//! Supports native ArtCraft WebAssembly plug-ins (`*.wasm`, ABI v1), compatible third-party /
//! Adobe Photoshop filters (`*.8bf` / `*.afs` Filter Factory), and preset packs
//! (`*.abr`, `*.grd`, `*.aco`, `*.ase`, `*.kys`, `*.cube`).

use serde::{Deserialize, Serialize};

/// How a plugin is packaged or bridged into a Craft App.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginFormat {
    /// Sandboxed ArtCraft WebAssembly module (`*.wasm`, ABI v1).
    NativeWasm,
    /// Classic Adobe Photoshop filter (`*.8bf` / `*.afs` Filter Factory) bridged to ABI v1.
    PhotoshopFilter,
    /// Compatible preset pack (`*.abr` brushes, `*.grd` gradients, `*.aco`/`*.ase` swatches, LUTs).
    PresetPack,
}

impl PluginFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::NativeWasm => "WASM ABI v1",
            Self::PhotoshopFilter => "Photoshop 8BF / AFS",
            Self::PresetPack => "Preset Pack",
        }
    }
}

/// Record of a local plugin installed on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub id: String,
    pub app: String,
    pub name: String,
    pub tagline: String,
    pub author: String,
    pub version: String,
    pub format: PluginFormat,
    pub location: String,
    /// Whether the `.wasm` / preset file on disk passed integrity checks and host-app registration.
    pub healthy: bool,
    /// True when imported from a local file (`.wasm`, `.afs`, `.8bf`, `.abr`, `.grd`, `.aco`, `.ase`, `.kys`, `.cube`).
    #[serde(default = "default_true")]
    pub imported: bool,
}

fn default_true() -> bool {
    true
}

/// Computed health status of an installed local plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    /// Installed and healthy.
    UpToDate,
    /// Installed record exists, but the file is missing, corrupted, or unlinked from host preferences.
    NeedsRepair,
}

/// Computes the status of a local plugin from its health state.
pub fn plugin_status(installed: &InstalledPlugin) -> PluginStatus {
    if installed.healthy { PluginStatus::UpToDate } else { PluginStatus::NeedsRepair }
}

/// Automatically infers which Craft App a plugin file belongs to from its extension and binary header.
pub fn detect_app_from_path(path: &std::path::Path, raw_header: Option<&[u8]>) -> Option<&'static str> {
    let ext = path.extension().and_then(|s| s.to_str()).map(str::to_ascii_lowercase)?;
    match ext.as_str() {
        "afs" | "8bf" | "abr" | "grd" | "aco" | "ase" | "kys" => Some("photocraft"),
        "cube" | "3dl" | "xmp" => Some("lightcraft"),
        "svg" => Some("vectorcraft"),
        "vst3" | "lv2" | "clap" => Some("soundcraft"),
        "wasm" => {
            if let Some(bytes) = raw_header {
                let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
                for app in &crate::SUITE {
                    if text.contains(app.id) {
                        return Some(app.id);
                    }
                }
            }
            Some("photocraft")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reflects_health() {
        let mut inst = InstalledPlugin {
            id: "org.photocraft.example.invert".into(),
            app: "photocraft".into(),
            name: "Invert Channels".into(),
            tagline: String::new(),
            author: String::new(),
            version: "1.0.0".into(),
            format: PluginFormat::NativeWasm,
            location: "/tmp/invert.wasm".into(),
            healthy: true,
            imported: true,
        };
        assert_eq!(plugin_status(&inst), PluginStatus::UpToDate);
        inst.healthy = false;
        assert_eq!(plugin_status(&inst), PluginStatus::NeedsRepair);
    }

    #[test]
    fn detects_target_app_from_extension_and_wasm_manifest() {
        use std::path::Path;
        assert_eq!(detect_app_from_path(Path::new("sepia.afs"), None), Some("photocraft"));
        assert_eq!(detect_app_from_path(Path::new("solarize.8bf"), None), Some("photocraft"));
        assert_eq!(detect_app_from_path(Path::new("kodak.cube"), None), Some("lightcraft"));
        assert_eq!(detect_app_from_path(Path::new("opt.wasm"), Some(b"\0asm\x01\0\0\0{\"id\":\"org.vectorcraft.opt\"}")), Some("vectorcraft"));
    }
}
