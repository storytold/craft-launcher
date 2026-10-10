//! `cargo xtask icns <out.icns> <in.png>…`: pack PNG images into a macOS `.icns`, on any OS
//! (`iconutil` only exists on macOS).
//!
//! Every entry is a PNG, which macOS reads for all of these types since 10.7. Each PNG fills
//! every slot of its pixel size: a 32 px image is both the 32 pt icon and the 16 pt @2x one.

use std::path::Path;

use crate::ico::png_size;

/// The PNG entry types, by pixel size.
const SLOTS: [(u32, &[u8; 4]); 11] = [
    (16, b"icp4"),
    (32, b"icp5"),
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

/// An `.icns` file holding `pngs`, which must be square and one of the sizes in [`SLOTS`].
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
        let types: Vec<&[u8; 4]> = SLOTS.iter().filter(|(size, _)| *size == w).map(|(_, kind)| *kind).collect();
        if types.is_empty() {
            return Err(format!("no .icns slot for a {w} px image (use 16, 32, 64, 128, 256, 512 or 1024)"));
        }
        let len = u32::try_from(png.len() + 8).map_err(|_| "PNG too large for an .icns".to_string())?;
        for kind in types {
            body.extend_from_slice(kind);
            body.extend_from_slice(&len.to_be_bytes());
            body.extend_from_slice(png);
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
        let (a, b) = (fake_png(32), fake_png(1024));
        let icns = pack(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize, icns.len());
        let e = entries(&icns);
        let kinds: Vec<&str> = e.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["icp5", "ic11", "ic10"]);
        assert_eq!(e[0].1, a);
        assert_eq!(e[2].1, b);
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
        assert!(entries(&icns).iter().any(|(k, _)| k == "ic10"), "expected a 1024 px image in {}", path.display());
    }
}
