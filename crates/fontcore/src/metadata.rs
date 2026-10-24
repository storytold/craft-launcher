use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ttf_parser::{Face, Language, name_id};

use crate::manifest::default_true;
use crate::{Error, Result};

/// On-disk container format of a vault font.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FontFormat {
    /// TrueType outlines (`.ttf`).
    Ttf,
    /// CFF outlines (`.otf`).
    Otf,
    /// TrueType/OpenType collection (`.ttc`, `.otc`).
    Ttc,
}

impl FontFormat {
    /// Detects the format from the file signature, ignoring the extension.
    pub fn sniff(data: &[u8]) -> Result<Self> {
        match data.get(..4) {
            Some([0, 1, 0, 0]) | Some(b"true") => Ok(Self::Ttf),
            Some(b"OTTO") => Ok(Self::Otf),
            Some(b"ttcf") => Ok(Self::Ttc),
            Some(b"wOFF") | Some(b"wOF2") => {
                Err(Error::UnsupportedFormat("WOFF/WOFF2 web fonts are not supported; convert to TTF or OTF first".into()))
            }
            _ => Err(Error::UnsupportedFormat("unrecognized font file signature".into())),
        }
    }

    /// The file extension FontCore stores this format under, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Ttf => "ttf",
            Self::Otf => "otf",
            Self::Ttc => "ttc",
        }
    }
}

/// Naming and style information for one face inside a font file, plus
/// whether apps may load it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceInfo {
    /// Index of the face inside a collection; 0 for single-face files.
    pub index: u32,
    pub family: String,
    pub subfamily: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postscript_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// OS/2 weight class, 1–1000.
    pub weight: u16,
    pub italic: bool,
    pub monospaced: bool,
    /// `false` hides this face from apps. Mainly useful for collections,
    /// where the other faces stay available.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Fields this build does not know about, preserved on rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Parses a font file and returns its format and every face it contains.
pub fn inspect(data: &[u8]) -> Result<(FontFormat, Vec<FaceInfo>)> {
    let format = FontFormat::sniff(data)?;
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    if count == 0 {
        return Err(Error::InvalidFont("font collection contains no faces".into()));
    }
    let faces = (0..count)
        .map(|index| {
            let face = Face::parse(data, index).map_err(|e| Error::InvalidFont(format!("face {index}: {e}")))?;
            Ok(face_info(&face, index))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((format, faces))
}

fn face_info(face: &Face, index: u32) -> FaceInfo {
    let postscript_name = name(face, name_id::POST_SCRIPT_NAME);
    let family = name(face, name_id::TYPOGRAPHIC_FAMILY)
        .or_else(|| name(face, name_id::FAMILY))
        .or_else(|| postscript_name.clone())
        .unwrap_or_else(|| "Unknown".into());
    let subfamily = name(face, name_id::TYPOGRAPHIC_SUBFAMILY).or_else(|| name(face, name_id::SUBFAMILY)).unwrap_or_else(|| "Regular".into());
    FaceInfo {
        index,
        family,
        subfamily,
        full_name: name(face, name_id::FULL_NAME),
        postscript_name,
        version: name(face, name_id::VERSION),
        weight: face.weight().to_number().clamp(1, 1000),
        italic: face.is_italic(),
        monospaced: face.is_monospaced(),
        enabled: true,
        extra: Map::new(),
    }
}

/// Looks up a name record, preferring US English over other languages.
fn name(face: &Face, id: u16) -> Option<String> {
    let records = || face.names().into_iter().filter(move |n| n.name_id == id);
    records()
        .filter(|n| n.language() == Language::English_UnitedStates)
        .chain(records())
        .find_map(|n| n.to_string())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}
