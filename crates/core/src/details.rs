//! Hand-written product pages: what each app is, in the launcher's own words.
//!
//! The text is compiled in, so it works offline and on the web, and nothing from the network is
//! ever shown as page copy. What changes with every release (version, date, size, notes) comes
//! from the GitHub release instead. An app without an entry here simply has no details page.
//!
//! The pictures are not compiled in: they are fetched when a page is opened (see
//! [`AppDetails::screenshot_url`]), so the repository and the binary stay small.

/// One feature: a short name and a sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feature {
    pub title: &'static str,
    pub text: &'static str,
}

/// A screenshot: `key` names the file to fetch (`<app>/<key>.jpg` in the pictures repository).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shot {
    pub key: &'static str,
    pub caption: &'static str,
}

const fn shot(key: &'static str, caption: &'static str) -> Shot {
    Shot { key, caption }
}

/// The editorial half of an app's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppDetails {
    /// Matches [`crate::CraftApp::id`].
    pub id: &'static str,
    /// One line under the name.
    pub headline: &'static str,
    /// One or two paragraphs, separated by a blank line.
    pub about: &'static str,
    pub features: &'static [Feature],
    /// The first is the lead picture. All are 8:5.
    pub shots: &'static [Shot],
    /// Where it runs, for display (`"macOS, Windows, Linux, web"`).
    pub platforms: &'static str,
}

const fn feature(title: &'static str, text: &'static str) -> Feature {
    Feature { title, text }
}

/// Where the pictures live: one 1200 px wide, 8:5 JPEG per [`Shot`], at `<app>/<key>.jpg`.
///
/// Pinned to a tag (`SCREENSHOTS_REV`) so that what a launcher fetches never changes under it, and
/// so that a saved copy never goes stale. To change a picture, publish a new tag and bump the
/// revision: older launchers keep working from the tag they know.
pub const SCREENSHOTS_BASE: &str = "https://raw.githubusercontent.com/storytold/craft-launcher-assets";

/// The tag of the pictures repository the launcher reads. Also part of the saved copies' path.
pub const SCREENSHOTS_REV: &str = "v1";

/// The largest picture the launcher accepts. The real ones are 60 to 150 KB.
pub const MAX_SCREENSHOT_BYTES: usize = 2 << 20;

/// The largest decoded picture the launcher will hand to the image loader, in pixels.
///
/// The real screenshots are 1200 px wide and 8:5 (≈0.9 MP). A byte cap alone does not bound the
/// decoded size: a tiny JPEG can declare 65,535 × 65,535, and the loader allocates
/// width × height × channels *before* decoding, turning a 2 MiB response into a multi-gigabyte
/// allocation (CWE-789). 8192 × 8192 (67 MP) is far above any real screenshot yet refuses the
/// pathological headers.
pub const MAX_SCREENSHOT_PIXELS: u64 = 8192 * 8192;

/// True when `bytes` start like a JPEG file. A captive portal's login page, a proxy's error page or
/// a truncated answer is refused before it is saved or decoded.
pub fn looks_like_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// Reads the pixel dimensions `(width, height)` declared in a JPEG's frame header, without
/// decoding any pixels. `None` when the bytes aren't a JPEG we can read a frame size from.
///
/// It walks the marker segments to the Start-Of-Frame (SOFn) marker, whose payload begins with
/// `precision (1) · height (2, big-endian) · width (2)`.
pub fn jpeg_dimensions(bytes: &[u8]) -> Option<(u16, u16)> {
    if !looks_like_jpeg(bytes) {
        return None;
    }
    let mut i = 2; // past the SOI (FF D8).
    let mut dims = None;
    while i + 1 < bytes.len() {
        // Markers are FF, with any number of FF fill bytes before the type byte.
        if bytes[i] != 0xFF {
            break;
        }
        let mut j = i;
        while j < bytes.len() && bytes[j] == 0xFF {
            j += 1;
        }
        let marker = *bytes.get(j)?;
        i = j + 1;
        match marker {
            // Standalone markers with no length: RSTn, SOI, EOI, TEM.
            0x01 | 0xD0..=0xD9 => continue,
            // SOS (Start of Scan): marker headers end, entropy-coded scan data begins.
            0xDA => break,
            _ => {}
        }
        let len = u16::from_be_bytes([*bytes.get(i)?, *bytes.get(i + 1)?]) as usize;
        if len < 2 {
            return None;
        }
        // Only SOF0 (0xC0: baseline), SOF1 (0xC1: extended sequential), and SOF2 (0xC2: progressive)
        // are supported by standard image rendering decoders.
        if matches!(marker, 0xC0..=0xC2) {
            if dims.is_some() {
                // Ambiguous / multiple frame markers: reject to prevent frame smuggling (CWE-789).
                return None;
            }
            let h = u16::from_be_bytes([*bytes.get(i + 3)?, *bytes.get(i + 4)?]);
            let w = u16::from_be_bytes([*bytes.get(i + 5)?, *bytes.get(i + 6)?]);
            dims = Some((w, h));
        } else if matches!(marker, 0xC3 | 0xC5..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            // Unsupported frame marker (e.g. lossless 0xC3): the rendering decoder will skip it or fail,
            // so accepting its dimensions would let a subsequent large frame bypass the budget (CWE-789).
            return None;
        }
        i += len;
    }
    dims
}

/// Whether `bytes` is a JPEG whose declared dimensions are within [`MAX_SCREENSHOT_PIXELS`], so it
/// is safe to hand to an image loader that allocates from the header before decoding.
pub fn screenshot_within_budget(bytes: &[u8]) -> bool {
    match jpeg_dimensions(bytes) {
        Some((w, h)) => w != 0 && h != 0 && u64::from(w) * u64::from(h) <= MAX_SCREENSHOT_PIXELS,
        // A JPEG we can't read a frame size from is refused rather than trusted.
        None => false,
    }
}

/// Where every Craft App runs (getartcraft.com lists the same for each of them).
const PLATFORMS: &str = "macOS, Windows, Linux, FreeBSD, web";

pub const DETAILS: [AppDetails; 14] = [
    AppDetails {
        id: "photocraft",
        headline: "The image editor you already know how to use.",
        about: "Layers, masks, adjustment layers, layer styles, type, vectors and brushes in a native app. Open source, offline, and yours.\n\n\
            PhotoCraft keeps the menus, shortcuts and panels your hands already know, so there is \
            nothing to relearn.",
        features: &[
            feature("Native GPU canvas", "No Electron and no web view: a fast native compositor."),
            feature("Real layered files", "Opens and saves layered PSD and PSB documents."),
            feature("Non-destructive editing", "Adjustment layers, live-preview filters, smart objects and layer styles."),
            feature("On-device selection", "Subject and object selection and mask refinement run locally."),
            feature("Serious color", "8-, 16- and 32-bit RGB, CMYK, Lab and Grayscale, with ICC color management and soft proofing."),
        ],
        shots: &[
            shot("demo", "Curves and type layers"),
            shot("adjustments", "Levels with a live histogram"),
            shot("layer-styles", "Layer styles on live type"),
            shot("masks", "Selections and masked adjustments"),
        ],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "vectorcraft",
        headline: "Vector illustration, with familiar tools and editable effects.",
        about: "A native vector illustration app with the pen, panels and shortcuts illustrators expect. \
                Open source, and it also runs in a browser.",
        features: &[
            feature(
                "The workflow you know",
                "Pen, direct selection, shape booleans, snapping guides and an appearance panel, laid out the way you expect.",
            ),
            feature("Genuinely fast", "Drawings with thousands of shapes stay responsive on a retina display."),
            feature(
                "Booleans that don't fail",
                "Shape booleans work on the exact curves, so there is no \"cannot perform operation\", and undo is unlimited.",
            ),
            feature("Live, editable effects", "Blends, envelope distort, gradient mesh, repeats, glows and drop shadows all stay editable."),
            feature("Image tracing", "Turn raster images into clean vector paths, with presets for the common cases."),
            feature("Open formats", "Native JSON documents, SVG and PDF import and export, and PNG, JPEG and WebP output."),
        ],
        shots: &[
            shot("neon", "The Neon Drive poster"),
            shot("ribbons", "Live blends"),
            shot("sheet", "A feature sheet and the Layers panel"),
            shot("bezier", "Pathfinder and exact booleans"),
        ],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "filmcraft",
        headline: "Professional video editing, rebuilt from scratch.",
        about: "A native video editor with source and program monitors, a real timeline and \
                every trim mode.",
        features: &[
            feature("A timeline you already know", "Source and program monitors, three-point editing, every trim mode and J/K/L trimming."),
            feature("Frame-exact time", "An integer timebase keeps 23.976 and 29.97 drop-frame footage exact, with no drift."),
            feature("Its own codecs", "No FFmpeg inside: native support for H.264, HEVC, ProRes, VP9, AAC and Opus."),
            feature("Color grading", "Curves, color wheels, HSL secondaries, LUTs and HDR and log workflows, with scopes to check the result."),
            feature(
                "Effects and keyframes",
                "Video effects and transitions, keyframes with value and velocity graphs, and a linear-light GPU compositor.",
            ),
            feature("Broadcast-ready audio", "EBU R128 loudness meters, a mixer with automation and native audio effects."),
        ],
        shots: &[
            shot("color", "The Color workspace, with scopes"),
            shot("assembly", "The Assembly workspace"),
            shot("keyframes", "Keyframes in Effect Controls"),
        ],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "lightcraft",
        headline: "Your photos. Your pixels. Your machine.",
        about: "A fast photo library and non-destructive raw developer. \
                No account, no cloud, no telemetry and no subscription.",
        features: &[
            feature("Non-destructive by design", "A wide-gamut, 32-bit float pipeline that never touches your originals."),
            feature("Every control you reach for", "Light, color, effects, tone curve, a color mixer, color grading wheels and built-in presets."),
            feature("Precise masking", "Brush, gradient and range masks, plus sky, subject and background masks."),
            feature("Native raw decoding", "Support for the main camera raw formats: DNG, CR2, ARW, NEF, RAF, RW2, PEF and ORF."),
            feature("A library that keeps up", "Albums, ratings, flags and search, with grids that stay smooth on large catalogs."),
            feature("Instant feedback", "The GPU pipeline updates the preview as you move a slider, even on large raw files."),
        ],
        shots: &[
            shot("hero", "The Edit view"),
            shot("curve", "The tone curve"),
            shot("grading", "Color grading wheels"),
            shot("masking", "A radial mask over a sky mask"),
        ],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "pdfcraft",
        headline: "The open-source PDF workbench.",
        about: "Read, organize, combine, split and secure PDFs in a fast, native app. No account, no telemetry and no cloud.",
        features: &[
            feature("Faithful rendering", "World scripts, vertical Japanese, color emoji and transparency."),
            feature("Search as you type", "Instant find, and text selection that follows the document's real reading order."),
            feature("Organize like cards", "Rotate, delete, insert and reorder pages, with deep undo behind every change."),
            feature("Combine, extract, split", "Assemble and break apart documents while keeping links, form fields, layers and bookmarks intact."),
            feature("Fearless saves", "Incremental, atomic saves with autosave and crash recovery."),
            feature("Built-in security", "Opens files from RC4 to AES-256 encryption and honors the author's permissions."),
        ],
        shots: &[],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "effectcraft",
        headline: "Motion graphics and visual effects.",
        about: "A native compositor with compositions, layers, keyframes, effects, expressions, 3D cameras and \
                lights, and a render queue. Young, moving fast, and already usable.",
        features: &[
            feature(
                "Animate the way you know",
                "Familiar panels and keyframes (linear, bezier, hold and eased), with a graph editor for value and speed curves.",
            ),
            feature(
                "Layers of every kind",
                "Solids, shapes, text, footage, nested compositions, nulls and adjustment layers, with parenting, track mattes and blend modes.",
            ),
            feature(
                "A large effects library",
                "Blur, color correction, distortion, generators, keying, particles and simulation, stylize and time effects.",
            ),
            feature("Real 3D", "3D layers with cameras, depth of field, and lights that cast soft ray-traced shadows."),
            feature("Expressions", "JavaScript expressions with wiggle, loops and layer references, edited inline."),
            feature("Export anywhere", "H.264 and ProRes with alpha, PNG, TIFF and EXR sequences, GIF, and Lottie import and export."),
        ],
        shots: &[
            shot("hero", "An animated composition"),
            shot("graph", "The Graph Editor"),
            shot("effects", "Particles and Effect Controls"),
            shot("3d", "3D layers with cameras and lights"),
        ],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "designcraft",
        headline: "Page layout and publishing.",
        about: "A native layout app with spreads, parent pages, threaded stories, styles, swatches, text wrap and \
                a paragraph composer. No subscription, no license server and no telemetry.",
        features: &[
            feature("The layout tools you know", "Spreads and parent pages, frames and threaded stories, with paragraph and character styles."),
            feature(
                "Beautiful type",
                "A Knuth–Plass paragraph composer, dictionary hyphenation, optical margin alignment and baseline grids, with line breaks that match on screen and in PDF.",
            ),
            feature("Print-ready PDF", "Real selectable text with embedded fonts, CMYK and spot colors, bleed, crop marks and PDF/A output."),
            feature("Fast", "Multithreaded rendering, instant undo and cached text composition."),
            feature("Open formats", "A documented native format, PNG export and layout interchange import and export."),
            feature("Built for automation", "Every menu item, tool, panel and dialog can be driven over a JSON control channel and an MCP server."),
        ],
        shots: &[shot("spread", "A magazine spread with a threaded story")],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "wordcraft",
        headline: "The familiar word processor, native and offline.",
        about: "A word processor with the ribbon, shortcuts and behavior you already know. \
                Open source, and built from public specifications.",
        features: &[
            feature(
                "Familiar",
                "Ribbon tabs and groups, a live Styles gallery and the editing behavior you expect, from list continuation to Format Painter.",
            ),
            feature(
                "Your files",
                "Opens and saves .docx, and also .odt, .rtf, .html, .md, .tex and .txt. Opens older .doc files and exports PDF with real, selectable text.",
            ),
            feature("Fast", "Paragraph layout is cached, so typing stays responsive even in long documents."),
            feature("Private", "Spelling, grammar and everything else work offline."),
            feature("Everywhere", "Work on macOS, Windows, Linux, BSD and the web."),
            feature(
                "Built for agents",
                "Every action is a command, driven from the ribbon, shortcuts, a command-line tool, a JSON control channel or an MCP server.",
            ),
        ],
        shots: &[
            shot("hero", "The Home tab and the Navigation pane"),
            shot("review", "Track Changes and comments"),
            shot("design", "The Design tab"),
            shot("layout", "The Layout tab"),
        ],
        platforms: "macOS, Windows, Linux, BSD, web",
    },
    AppDetails {
        id: "deckcraft",
        headline: "Create slides, tell stories and present.",
        about: "A presentation app with slide masters, shapes, animations and a presenter view. \
                In alpha, heading for beta.",
        features: &[
            feature("Make decks", "Slide masters and layouts with placeholders that inherit formatting, themes, sections, notes and comments."),
            feature(
                "Draw",
                "Preset shapes with adjust handles, connectors that stay glued to shapes, freeform tools, merged shapes, gradients, shadows and glow.",
            ),
            feature("Write", "In-place text editing with full font and paragraph formatting, bullets and numbering, columns and autofit."),
            feature("Add content", "Pictures with crop and corrections, styled tables, charts, audio and video."),
            feature(
                "Present",
                "Transitions including Morph, animations and motion paths, a full-screen slide show, presenter view and rehearsed timings.",
            ),
            feature("Exchange files", "Opens and saves .pptx presentation files and exports PDF (slides, notes pages and handouts), PNG and JPEG."),
        ],
        shots: &[
            shot("editor", "Editing a slide, with sections and notes"),
            shot("shape", "The Shape Format tab"),
            shot("sorter", "The slide sorter"),
            shot("format", "The Format Shape pane"),
        ],
        platforms: "macOS, Windows, Linux, BSD, web",
    },
    AppDetails {
        id: "gridcraft",
        headline: "Spreadsheets you already know how to use.",
        about: "A spreadsheet with the ribbon, formula bar, sheet tabs and shortcuts you know. No subscription, no license server and no telemetry.",
        features: &[
            feature(
                "Familiar",
                "The ribbon, formula bar, Name Box, fill handle, point-mode formulas, Format Cells, Paste Special, conditional formatting, tables and charts.",
            ),
            feature(
                "Compatible",
                "XLSX is the native format, with formulas, tables, conditional formats, charts, pivot tables and print settings round-tripping. CSV and TSV too.",
            ),
            feature(
                "Capable",
                "A dependency-graph calculation engine with dynamic arrays, LET and LAMBDA, structured table references, sort, AutoFilter and Flash Fill.",
            ),
            feature("Fast", "Copy-on-write workbooks, incremental recalculation and a grid that only lays out what is on screen."),
            feature(
                "Agent-native",
                "Every ribbon button, menu item, dialog and keystroke is a command, available over a JSON control channel, an MCP server or the CLI.",
            ),
            feature("Everywhere", "Work on the desktop and the web."),
        ],
        shots: &[
            shot("hero", "A quarterly sales dashboard"),
            shot("budget", "A household budget"),
            shot("pivot", "A pivot table"),
            shot("chart", "Formatting a chart"),
        ],
        platforms: "macOS, Windows, Linux, BSD, web",
    },
    AppDetails {
        id: "cadcraft",
        headline: "Computer-aided drafting, with a familiar workflow.",
        about: "A drafting app with the command line, object snaps and workflow that drafters already know. Free, with no \
                account and no subscription. In early, fast development.",
        features: &[
            feature(
                "The workflow you know",
                "Type L, click two points, type @5<45 and press Enter. Prompts with clickable keywords, polar tracking, direct distance entry and grips work the way drafting habit expects.",
            ),
            feature(
                "Open files",
                "Reads and writes DXF natively (R12 through 2018) and opens and saves DWG (R13 through 2018). Exports SVG and PNG.",
            ),
            feature("Fast and native", "A responsive, native drafting workspace."),
            feature(
                "A full drafting toolset",
                "Draw, modify, dimension, hatch and block commands, layers with a properties manager, paper-space layouts and PDF plotting.",
            ),
            feature("Built for agents", "Every menu item, tool and prompt is a command, available over MCP, a JSON control channel or the CLI."),
        ],
        shots: &[shot("apartment", "An apartment floor plan"), shot("bracket", "A mechanical bracket drawing"), shot("layout", "A layout sheet")],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "soundcraft",
        headline: "Record, edit and mix your audio.",
        about: "A digital audio workstation that follows the workflow audio engineers already know: an Edit window for \
                arranging and editing, and a Mix window for balancing. Pre-alpha.",
        features: &[
            feature(
                "Editing",
                "Shuffle, Slip, Spot and Grid modes, Smart, Grabber, Trim and other tools, fades and crossfades, playlists with comping lanes and clip gain.",
            ),
            feature(
                "Mixing",
                "Faders and pans with automation, sends, busses, aux inputs, VCA masters, routing folders and automatic plugin delay compensation.",
            ),
            feature(
                "Automation",
                "Volume, pan, mute, send and every plugin parameter, with Write, Touch, Latch and Trim passes recorded live from the faders.",
            ),
            feature(
                "Plugins",
                "EQ, dynamics, reverbs, modulation effects, a subtractive synth and a drum synth, all original, and CLAP plugins load too.",
            ),
        ],
        shots: &[
            shot("edit", "The Edit window with a nine-track session"),
            shot("mix", "The Mix window"),
            shot("automation", "Automation and memory locations"),
            shot("score", "The Score Editor"),
        ],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "figcraft",
        headline: "Interface design and prototyping.",
        about: "Design screens, components and prototypes in a native app that opens instantly and works offline, \
                with no account and no telemetry. Early alpha.",
        features: &[
            feature(
                "Auto layout",
                "Rows, columns, wrap and grids with padding, gaps and Hug, Fill or Fixed sizing, plus constraints and layout grids.",
            ),
            feature(
                "Components",
                "Instances with overrides, variants and properties, styles, and variables with modes bound to fills, strokes and effects.",
            ),
            feature("Vectors", "Real vector networks, the pen and pencil, the shape builder, editable booleans and variable width strokes."),
            feature(
                "Prototypes",
                "Connections, triggers and animated transitions, variables with conditionals, flows and a presentation view with device frames.",
            ),
            feature(
                "Hand off",
                "Inspect mode with measurements, annotations, ready-for-dev status, and code for CSS, HTML, React, SwiftUI and Compose.",
            ),
            feature(
                "Your files",
                "Plain .figcraft files on your own disk. SVG, PNG, JPEG, WebP and PDF in and out, and .fig files open, best effort.",
            ),
        ],
        shots: &[],
        platforms: PLATFORMS,
    },
    AppDetails {
        id: "solidcraft",
        headline: "Parametric 3D CAD, with editable sketches and solids.",
        about: "A parametric solid modeller by Tangency Labs. Sketch, constrain and build solids on an editable feature \
                timeline: change a dimension and the model rebuilds. Early, but usable for simple parts. Open source, \
                and it can be driven by AI agents through its MCP server.",
        features: &[
            feature(
                "Sketches and constraints",
                "Lines, arcs, splines and text, solved by SolidCraft's own constraint solver, with parameter-driven dimensions.",
            ),
            feature("Features", "Extrude, revolve, sweep, loft, fillet, chamfer, shell, holes, threads and patterns on a parametric timeline."),
            feature("Files", "STEP, IGES, 3MF, STL, OBJ and glTF in and out where supported, and .sc designs."),
            feature("Desktop, web and agents", "A desktop app, a browser app, a headless CLI and an MCP server, all running the same commands."),
        ],
        shots: &[],
        platforms: PLATFORMS,
    },
];

impl AppDetails {
    /// Where the picture `key` of this app is published, or `None` if the page has no such picture.
    /// The only way to turn a request into an address: callers never build one from their own text.
    pub fn screenshot_url(&self, key: &str) -> Option<String> {
        self.shots.iter().find(|s| s.key == key).map(|s| format!("{SCREENSHOTS_BASE}/{SCREENSHOTS_REV}/{}/{}.jpg", self.id, s.key))
    }

    pub fn by_id(id: &str) -> Option<&'static AppDetails> {
        DETAILS.iter().find(|d| d.id == id)
    }

    pub fn paragraphs(&self) -> impl Iterator<Item = &'static str> {
        self.about.split("\n\n").map(str::trim).filter(|p| !p.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CraftApp, SUITE};

    #[test]
    fn every_page_belongs_to_a_suite_app_once() {
        let mut ids: Vec<_> = DETAILS.iter().map(|d| d.id).collect();
        for id in &ids {
            assert!(CraftApp::by_id(id).is_some(), "{id}");
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), DETAILS.len());
    }

    #[test]
    fn every_app_has_a_page() {
        for a in &SUITE {
            assert!(AppDetails::by_id(a.id).is_some(), "{}", a.id);
        }
    }

    #[test]
    fn picture_addresses_come_only_from_the_pages_own_pictures() {
        let page = AppDetails::by_id("photocraft").unwrap();
        assert_eq!(
            page.screenshot_url("demo").as_deref(),
            Some("https://raw.githubusercontent.com/storytold/craft-launcher-assets/v1/photocraft/demo.jpg")
        );
        for key in ["", "nope", "../vectorcraft/neon", "demo/../masks", "demo.jpg", "/etc/passwd", "demo?x=1", "demo#"] {
            assert_eq!(page.screenshot_url(key), None, "{key:?}");
        }
        // Every picture of every page has an address of the one shape, with nothing to escape.
        for d in &DETAILS {
            for s in d.shots {
                let url = d.screenshot_url(s.key).unwrap();
                let path = url.strip_prefix(&format!("{SCREENSHOTS_BASE}/{SCREENSHOTS_REV}/")).unwrap();
                assert_eq!(path, format!("{}/{}.jpg", d.id, s.key));
                assert!(path.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '/' | '-' | '.')), "{path}");
            }
        }
    }

    #[test]
    fn only_jpeg_files_are_accepted() {
        assert!(looks_like_jpeg(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]));
        assert!(!looks_like_jpeg(b""));
        assert!(!looks_like_jpeg(&[0xFF, 0xD8]));
        assert!(!looks_like_jpeg(b"<!DOCTYPE html>"));
        // A PNG's signature.
        assert!(!looks_like_jpeg(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
    }

    /// A minimal JPEG: SOI, an APP0 segment, then a baseline SOF0 declaring `w × h`.
    fn jpeg_of(w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8]; // SOI
        b.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0, length 4, two payload bytes
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]); // SOF0, length 17, precision 8
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[0x03]); // component count (payload need not be complete for parsing)
        b
    }

    #[test]
    fn reads_jpeg_frame_dimensions() {
        assert_eq!(jpeg_dimensions(&jpeg_of(1200, 750)), Some((1200, 750)));
        assert_eq!(jpeg_dimensions(&jpeg_of(65535, 65535)), Some((65535, 65535)));
        // Not a JPEG, or no frame header present.
        assert_eq!(jpeg_dimensions(b"<html>"), None);
        assert_eq!(jpeg_dimensions(&[0xFF, 0xD8]), None);

        // Unsupported C3 frame marker is rejected (CWE-789).
        let mut c3 = vec![0xFF, 0xD8, 0xFF, 0xC3, 0x00, 0x11, 0x08];
        c3.extend_from_slice(&100u16.to_be_bytes());
        c3.extend_from_slice(&100u16.to_be_bytes());
        c3.push(0x03);
        assert_eq!(jpeg_dimensions(&c3), None);

        // Multiple / conflicting SOF markers are rejected.
        let mut multi = vec![0xFF, 0xD8];
        multi.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        multi.extend_from_slice(&100u16.to_be_bytes());
        multi.extend_from_slice(&100u16.to_be_bytes());
        multi.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        multi.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        multi.extend_from_slice(&9000u16.to_be_bytes());
        multi.extend_from_slice(&9000u16.to_be_bytes());
        multi.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        assert_eq!(jpeg_dimensions(&multi), None);
    }

    #[test]
    fn oversized_screenshots_are_refused_before_decoding() {
        assert!(screenshot_within_budget(&jpeg_of(1200, 750)));
        assert!(screenshot_within_budget(&jpeg_of(8192, 8192)));
        // The decompression-bomb header: a tiny file declaring billions of pixels.
        assert!(!screenshot_within_budget(&jpeg_of(65535, 65535)));
        assert!(!screenshot_within_budget(&jpeg_of(8193, 8192)));
        assert!(!screenshot_within_budget(&jpeg_of(0, 100)));
        // A non-JPEG (or a JPEG we can't size) is refused rather than trusted.
        assert!(!screenshot_within_budget(b"<html>"));
    }

    #[test]
    fn pages_are_filled_in() {
        for d in &DETAILS {
            assert!(!d.headline.is_empty() && !d.features.is_empty() && d.paragraphs().next().is_some(), "{}", d.id);
        }
    }
}
