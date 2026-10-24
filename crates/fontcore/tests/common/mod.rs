//! Builds minimal valid sfnt fonts so tests need no binary fixtures.

// Each test crate uses a different subset of these helpers.
#![allow(dead_code)]

/// A single-face TrueType font with the given family/subfamily names.
/// `salt` lands in the version string so otherwise-equal fonts hash apart.
pub fn ttf(family: &str, subfamily: &str, salt: &str) -> Vec<u8> {
    let names = [
        (1, family.to_owned()),
        (2, subfamily.to_owned()),
        (4, format!("{family} {subfamily}")),
        (5, format!("Version 1.0 {salt}")),
        (6, format!("{family}-{subfamily}").replace(' ', "")),
    ];
    sfnt(&base_tables(&names))
}

/// A single-face variable font: `ttf` plus an `fvar` table with a `wght`
/// axis (100–900) and one named instance.
pub fn variable_ttf(family: &str) -> Vec<u8> {
    let names = [(1, family.to_owned()), (2, "Regular".to_owned())];
    let mut tables = base_tables(&names);
    tables.push((*b"fvar", fvar()));
    sfnt(&tables)
}

fn base_tables(names: &[(u16, String)]) -> Vec<([u8; 4], Vec<u8>)> {
    vec![(*b"head", head()), (*b"hhea", hhea()), (*b"maxp", maxp()), (*b"name", name(names))]
}

/// A TrueType collection wrapping the given single-face fonts.
pub fn ttc(fonts: &[Vec<u8>]) -> Vec<u8> {
    let header_len = 12 + 4 * fonts.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(fonts.len() as u32).to_be_bytes());
    let mut offset = header_len;
    let mut body = Vec::new();
    for font in fonts {
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        // Table offsets inside a collection are relative to the file start.
        let num_tables = u16::from_be_bytes([font[4], font[5]]) as usize;
        let mut font = font.clone();
        for i in 0..num_tables {
            let at = 12 + i * 16 + 8;
            let old = u32::from_be_bytes(font[at..at + 4].try_into().unwrap());
            font[at..at + 4].copy_from_slice(&(old + offset as u32).to_be_bytes());
        }
        offset += font.len();
        body.extend_from_slice(&font);
    }
    out.extend_from_slice(&body);
    out
}

fn sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut tables = tables.to_vec();
    tables.sort_by_key(|(tag, _)| *tag);
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]); // searchRange etc.; unused by parsers we target
    let mut offset = 12 + 16 * tables.len();
    let mut data = Vec::new();
    for (tag, table) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(table.len() as u32).to_be_bytes());
        let mut padded = table.clone();
        padded.resize(table.len().next_multiple_of(4), 0);
        offset += padded.len();
        data.extend_from_slice(&padded);
    }
    out.extend_from_slice(&data);
    out
}

fn head() -> Vec<u8> {
    let mut t = vec![0u8; 54];
    t[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    t[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    t[18..20].copy_from_slice(&1000u16.to_be_bytes());
    t
}

fn hhea() -> Vec<u8> {
    let mut t = vec![0u8; 36];
    t[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    t[34..36].copy_from_slice(&1u16.to_be_bytes());
    t
}

fn maxp() -> Vec<u8> {
    let mut t = 0x0000_5000u32.to_be_bytes().to_vec();
    t.extend_from_slice(&1u16.to_be_bytes());
    t
}

fn fvar() -> Vec<u8> {
    let fixed = |v: i32| (v << 16).to_be_bytes();
    let mut t = Vec::new();
    // version 1.0, axesArrayOffset, reserved, axisCount, axisSize,
    // instanceCount, instanceSize
    for v in [1u16, 0, 16, 2, 1, 20, 1, 8] {
        t.extend_from_slice(&v.to_be_bytes());
    }
    t.extend_from_slice(b"wght");
    for v in [100, 400, 900] {
        t.extend_from_slice(&fixed(v));
    }
    t.extend_from_slice(&0u16.to_be_bytes()); // flags
    t.extend_from_slice(&256u16.to_be_bytes()); // axisNameID
    // Instance: subfamilyNameID, flags, then one coordinate (wght 700).
    t.extend_from_slice(&2u16.to_be_bytes());
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&fixed(700));
    t
}

fn name(records: &[(u16, String)]) -> Vec<u8> {
    let mut storage = Vec::new();
    let mut t = Vec::new();
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&(records.len() as u16).to_be_bytes());
    t.extend_from_slice(&((6 + 12 * records.len()) as u16).to_be_bytes());
    for (id, text) in records {
        let encoded: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        // Windows platform, Unicode BMP, US English.
        for v in [3u16, 1, 0x0409, *id, encoded.len() as u16, storage.len() as u16] {
            t.extend_from_slice(&v.to_be_bytes());
        }
        storage.extend_from_slice(&encoded);
    }
    t.extend_from_slice(&storage);
    t
}
