//! `cargo xtask icns <out.icns> <in.png>…`: pack PNG images into a macOS `.icns`, on any OS
//! (`iconutil` only exists on macOS).
//!
//! macOS icon services misread PNG in the 16 and 32 px non-Retina slots. Use the legacy RGB
//! and separate alpha-mask slots there, like macOS's image tools. Retina and larger slots
//! keep the original PNG bytes: a 32 px image still supplies the 16 pt @2x slot.

use std::io::Cursor;
use std::path::Path;

use image::{ImageFormat, ImageReader, Limits};

use crate::ico::png_size;

/// The PNG entry types, by pixel size.
const SLOTS: [(u32, &[u8; 4]); 9] = [
    (32, b"ic11"), // 16 @2x
    (64, b"icp6"),
    (64, b"ic12"), // 32 @2x
    (128, b"ic07"),
    (256, b"ic08"),
    (256, b"ic13"), // 128 @2x
    (512, b"ic09"),
    (512, b"ic14"),  // 256 @2x
    (1024, b"ic10"), // 512 @2x
];

/// The legacy 1× RGB and alpha mask. The uncompressed form uses four bytes per
/// pixel; macOS ignores the first and reads transparency from the separate mask.
/// This also avoids a macOS decoder quirk that drops the last blue sample in RLE.
fn legacy_pixels(png: &[u8], size: u32) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(size);
    limits.max_image_height = Some(size);
    limits.max_alloc = Some(1024 * 1024);
    reader.limits(limits);
    let rgba = reader.decode().map_err(|e| format!("could not read {size} px icon: {e}"))?.into_rgba8();
    let mut rgb = Vec::with_capacity(rgba.as_raw().len());
    let mut mask = Vec::with_capacity(rgba.as_raw().len() / 4);
    for &image::Rgba([r, g, b, a]) in rgba.pixels() {
        rgb.extend_from_slice(&[0, r, g, b]);
        mask.push(a);
    }
    Ok((rgb, mask))
}

fn append_entry(body: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<(), String> {
    let len = u32::try_from(data.len() + 8).map_err(|_| "entry too large for an .icns".to_string())?;
    body.extend_from_slice(kind);
    body.extend_from_slice(&len.to_be_bytes());
    body.extend_from_slice(data);
    Ok(())
}

/// An `.icns` file holding square PNGs of 16, 32, 64, 128, 256, 512 or 1024 px.
pub fn pack(pngs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    if pngs.is_empty() {
        return Err("need at least one PNG".into());
    }
    let mut body = Vec::new();
    for png in pngs {
        let (w, h) = png_size(png)?;
        if w != h {
            return Err(format!("icon images must be square, got {w}x{h}"));
        }
        let legacy = match w {
            16 => Some((b"is32", b"s8mk")),
            32 => Some((b"il32", b"l8mk")),
            _ => None,
        };
        let types: Vec<&[u8; 4]> = SLOTS.iter().filter(|(size, _)| *size == w).map(|(_, kind)| *kind).collect();
        if legacy.is_none() && types.is_empty() {
            return Err(format!("no .icns slot for a {w} px image (use 16, 32, 64, 128, 256, 512 or 1024)"));
        }
        if let Some((rgb_kind, mask_kind)) = legacy {
            let (rgb, mask) = legacy_pixels(png, w)?;
            append_entry(&mut body, rgb_kind, &rgb)?;
            append_entry(&mut body, mask_kind, &mask)?;
        }
        for kind in types {
            append_entry(&mut body, kind, png)?;
        }
    }
    let total = u32::try_from(body.len() + 8).map_err(|_| ".icns too large".to_string())?;
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn run(args: &[&str]) -> Result<(), String> {
    let [out, inputs @ ..] = args else {
        return Err("usage: cargo xtask icns <out.icns> <in.png>…".into());
    };
    let pngs = inputs.iter().map(|p| std::fs::read(p).map_err(|e| format!("read {p}: {e}"))).collect::<Result<Vec<_>, _>>()?;
    let icns = pack(&pngs)?;
    std::fs::write(Path::new(out), &icns).map_err(|e| format!("write {out}: {e}"))?;
    println!("{out}: {} images, {} bytes", pngs.len(), icns.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_png(size: u32) -> (image::RgbaImage, Vec<u8>) {
        use image::ImageEncoder;
        // Synthetic image: unequal RGB channels and transparent/opaque pixels expose swapping
        // the planes, a missing mask, or copying the PNG into a legacy slot.
        let pixels = image::RgbaImage::from_fn(size, size, |x, y| match (x + y) % 4 {
            0 => image::Rgba([0, 0, 0, 0]),
            1 => image::Rgba([0, 0, 255, 128]),
            _ => image::Rgba([x as u8, (2 * y) as u8, 200, 255]),
        });
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png).write_image(pixels.as_raw(), size, size, image::ExtendedColorType::Rgba8).unwrap();
        (pixels, png)
    }

    fn fake_png(size: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 1, 2, 3, 4]);
        v
    }

    /// The (type, payload) entries of an `.icns`.
    fn entries(icns: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut at = 8;
        while at < icns.len() {
            let len = u32::from_be_bytes(icns[at + 4..at + 8].try_into().unwrap()) as usize;
            out.push((String::from_utf8(icns[at..at + 4].to_vec()).unwrap(), icns[at + 8..at + len].to_vec()));
            at += len;
        }
        out
    }

    #[test]
    fn fills_every_slot_of_each_size() {
        let (a, b) = (sample_png(32).1, fake_png(1024));
        let icns = pack(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize, icns.len());
        let e = entries(&icns);
        let kinds: Vec<&str> = e.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["il32", "l8mk", "ic11", "ic10"]);
        assert_eq!(e[2].1, a, "the high-resolution PNG is unchanged");
        assert_eq!(e[3].1, b);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_decodes_each_small_icon_with_its_original_colors_and_transparency() {
        let (small, small_png) = sample_png(16);
        let (medium, medium_png) = sample_png(32);
        let folder = tempfile::tempdir().unwrap();
        let icns = folder.path().join("icon.icns");
        let extracted = folder.path().join("icon.iconset");
        std::fs::write(&icns, pack(&[small_png, medium_png]).unwrap()).unwrap();
        let output = std::process::Command::new("/usr/bin/iconutil").args(["-c", "iconset"]).arg(&icns).arg("-o").arg(&extracted).output().unwrap();
        assert!(output.status.success(), "iconutil: {}", String::from_utf8_lossy(&output.stderr));
        for (file, expected) in [("icon_16x16.png", small), ("icon_32x32.png", medium)] {
            let actual = image::open(extracted.join(file)).unwrap().into_rgba8();
            assert_eq!(actual.dimensions(), expected.dimensions(), "{file}");
            let mismatch = actual.enumerate_pixels().find(|&(x, y, pixel)| pixel != expected.get_pixel(x, y));
            assert!(
                mismatch.is_none(),
                "{file} did not round-trip through macOS iconutil; first differing pixel: {:?}",
                mismatch.map(|(x, y, pixel)| (x, y, pixel, expected.get_pixel(x, y)))
            );
        }
    }

    #[test]
    fn rejects_bad_input() {
        assert!(pack(&[]).is_err());
        assert!(pack(&[fake_png(48)]).is_err());
        assert!(pack(&[b"GIF89a".to_vec()]).is_err());
    }

    #[test]
    fn committed_icon_is_valid() {
        let path = crate::root().join("assets/app-icon/artcraft-launcher.icns");
        let Ok(icns) = std::fs::read(&path) else { return };
        assert_eq!(&icns[..4], b"icns");
        let entries = entries(&icns);
        for (kind, size) in [("s8mk", 16), ("l8mk", 32)] {
            assert!(entries.iter().any(|(k, data)| k == kind && data.len() == size * size), "expected a {size} px alpha mask in {}", path.display());
        }
        assert!(!entries.iter().any(|(k, _)| matches!(k.as_str(), "icp4" | "icp5")));
        assert!(entries.iter().any(|(k, _)| k == "ic10"), "expected a 1024 px image in {}", path.display());
    }
}
