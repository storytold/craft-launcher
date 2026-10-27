//! The frames the launcher sends its elevated Windows helper.
//!
//! A request is a short header (`u32` length, then the header itself). An install follows the
//! header with the package bytes and their SHA-256, so the helper can stream a large MSI to disk
//! instead of holding it in memory. Keeping this code free of Windows APIs lets it be tested on
//! every platform.

pub(crate) const KIND_INSTALL: u8 = 1;
pub(crate) const KIND_UNINSTALL: u8 = 2;

pub(crate) const MAX_HEADER: usize = 64 << 10;
pub(crate) const MAX_DESTINATION: usize = 32 << 10;
pub(crate) const MAX_PRODUCT: usize = 64;
pub(crate) const MAX_MSI: u64 = 2 << 30; // 2 GiB

/// One thing the helper may be asked to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Install { destination: Option<String>, length: u64, expected: Option<[u8; 32]> },
    Uninstall { product: String },
}

/// An install header: the optional install folder, the package length, and the release checksum
/// the launcher verified the download against.
pub(crate) fn encode_install(destination: Option<&str>, length: u64, expected: Option<&[u8; 32]>) -> Result<Vec<u8>, String> {
    if length == 0 || length > MAX_MSI {
        return Err("the package is too large".into());
    }
    let mut out = vec![KIND_INSTALL];
    match destination {
        Some(destination) => {
            let bytes = destination.as_bytes();
            if bytes.is_empty() || bytes.len() > MAX_DESTINATION || destination.contains('\0') {
                return Err("the install folder isn't usable".into());
            }
            out.push(1);
            out.extend_from_slice(&u32::try_from(bytes.len()).map_err(|_| "the install folder is too long")?.to_be_bytes());
            out.extend_from_slice(bytes);
        }
        None => out.push(0),
    }
    out.extend_from_slice(&length.to_be_bytes());
    match expected {
        Some(expected) => {
            out.push(1);
            out.extend_from_slice(expected);
        }
        None => out.push(0),
    }
    Ok(out)
}

/// An uninstall header: the MSI product code.
pub(crate) fn encode_uninstall(product: &str) -> Result<Vec<u8>, String> {
    validate_product(product)?;
    let mut out = vec![KIND_UNINSTALL];
    out.extend_from_slice(&u32::try_from(product.len()).map_err(|_| "the product code is too long")?.to_be_bytes());
    out.extend_from_slice(product.as_bytes());
    Ok(out)
}

/// Reads a header the helper was sent.
pub(crate) fn decode(header: &[u8]) -> Result<Request, String> {
    if header.len() > MAX_HEADER {
        return Err("the install request is too large".into());
    }
    let mut reader = Reader { bytes: header, at: 0 };
    let request = match reader.u8()? {
        KIND_INSTALL => {
            let destination = match reader.u8()? {
                0 => None,
                1 => {
                    let length = reader.u32()? as usize;
                    if length == 0 || length > MAX_DESTINATION {
                        return Err("the install folder isn't usable".into());
                    }
                    let destination = std::str::from_utf8(reader.bytes(length)?).map_err(|_| "the install folder isn't text")?;
                    if destination.contains('\0') {
                        return Err("the install folder isn't usable".into());
                    }
                    Some(destination.to_string())
                }
                _ => return Err("unknown install header".into()),
            };
            let length = reader.u64()?;
            if length == 0 || length > MAX_MSI {
                return Err("the package is too large".into());
            }
            let expected = match reader.u8()? {
                0 => None,
                1 => {
                    let bytes = reader.bytes(32)?;
                    Some(<[u8; 32]>::try_from(bytes).map_err(|_| "truncated installer checksum")?)
                }
                _ => return Err("unknown installer checksum".into()),
            };
            Request::Install { destination, length, expected }
        }
        KIND_UNINSTALL => {
            let length = reader.u32()? as usize;
            if length == 0 || length > MAX_PRODUCT {
                return Err("the product code isn't usable".into());
            }
            let product = std::str::from_utf8(reader.bytes(length)?).map_err(|_| "the product code isn't text")?.to_string();
            validate_product(&product)?;
            Request::Uninstall { product }
        }
        _ => return Err("unknown request".into()),
    };
    if reader.at != header.len() {
        return Err("trailing request data".into());
    }
    Ok(request)
}

/// The release's SHA-256 as hex, for the header.
pub(crate) fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

/// Product codes are GUIDs from the Windows Installer registry; nothing else may reach `msiexec
/// /x`.
pub(crate) fn validate_product(product: &str) -> Result<(), String> {
    let ok = !product.is_empty()
        && product.len() <= MAX_PRODUCT
        && !product.starts_with('-')
        && product.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'{' | b'}' | b'-' | b'(' | b')'));
    if ok { Ok(()) } else { Err(format!("{product} isn't a product code")) }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(length).ok_or("truncated header")?;
        let part = self.bytes.get(self.at..end).ok_or("truncated header")?;
        self.at = end;
        Ok(part)
    }

    fn u8(&mut self) -> Result<u8, String> {
        self.bytes(1)?.first().copied().ok_or_else(|| "truncated header".to_string())
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.bytes(4)?;
        let value = <[u8; 4]>::try_from(bytes).map_err(|_| "truncated header")?;
        Ok(u32::from_be_bytes(value))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes = self.bytes(8)?;
        let value = <[u8; 8]>::try_from(bytes).map_err(|_| "truncated header")?;
        Ok(u64::from_be_bytes(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_round_trip() {
        let checksum = [0x5a; 32];
        for destination in [None, Some(r"C:\Program Files\PhotoCraft")] {
            for expected in [None, Some(&checksum)] {
                let header = encode_install(destination, 12345, expected).expect("a small install request");
                assert_eq!(
                    decode(&header),
                    Ok(Request::Install { destination: destination.map(str::to_string), length: 12345, expected: expected.copied() })
                );
            }
        }
    }

    #[test]
    fn checksums_are_read_from_hex() {
        let text = "5a".repeat(32);
        assert_eq!(parse_sha256(&text), Some([0x5a; 32]));
        assert_eq!(parse_sha256(&text[..63]), None);
        assert_eq!(parse_sha256(&"zz".repeat(32)), None);
    }

    #[test]
    fn uninstalls_round_trip() {
        let product = "{A6DE41B7-A80F-4C59-A17E-BB68065BF67E}";
        let header = encode_uninstall(product).expect("a product code");
        assert_eq!(decode(&header), Ok(Request::Uninstall { product: product.to_string() }));
    }

    #[test]
    fn malformed_headers_are_refused() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[99]).is_err());
        assert!(decode(&[KIND_INSTALL, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_err(), "unknown destination flag");
        assert!(decode(&[KIND_UNINSTALL, 0, 0, 0, 1, b'-']).is_err(), "an option isn't a product code");
        assert!(decode(&[KIND_UNINSTALL, 0, 0, 0, 1, b'a', 0]).is_err(), "trailing data");
        let mut header = encode_install(None, 1, None).expect("a small request");
        header.push(0);
        assert!(decode(&header).is_err());
        assert!(decode(&[KIND_UNINSTALL, 0, 0, 0, 0]).is_err(), "no product code");
        // An install header that ends before its length.
        assert!(decode(&[KIND_INSTALL, 0, 0, 0, 0, 0]).is_err());
        // An unknown checksum flag, and a checksum that ends early.
        assert!(decode(&[KIND_INSTALL, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2]).is_err());
        assert!(decode(&[KIND_INSTALL, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 1, 2]).is_err());
    }

    #[test]
    fn oversized_requests_are_refused() {
        assert!(encode_install(None, 0, None).is_err());
        assert!(encode_install(None, MAX_MSI + 1, None).is_err());
        let long = "a".repeat(MAX_DESTINATION + 1);
        assert!(encode_install(Some(&long), 1, None).is_err());
        assert!(encode_install(Some("bad\0folder"), 1, None).is_err());
        let long = "a".repeat(MAX_PRODUCT + 1);
        assert!(encode_uninstall(&long).is_err());
    }

    #[test]
    fn product_codes_are_checked() {
        for product in ["{A6DE41B7-A80F-4C59-A17E-BB68065BF67E}", "photocraft", "(A)"] {
            assert!(validate_product(product).is_ok(), "{product}");
        }
        for product in ["", "-o", "a;b", "a b", "a\nb", "a/b"] {
            assert!(validate_product(product).is_err(), "{product}");
        }
    }
}
