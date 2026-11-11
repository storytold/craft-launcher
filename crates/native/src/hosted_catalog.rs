//! Startup-only signed catalog refresh with an atomic, reverified disk fallback.
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Once;

use artcraft_launcher_core::hosted_catalog::{
    CATALOG_URL, MAX_CATALOG_BYTES, MAX_ICON_BYTES, MAX_SIGNATURE_BYTES, SIGNATURE_URL, VerifiedCatalog, valid_icon,
};
use serde::{Deserialize, Serialize};

use crate::http::Http;
use crate::paths::{Paths, write_atomic};

static LOADED: Once = Once::new();

#[derive(Serialize, Deserialize)]
struct Saved {
    catalog: String,
    signature: String,
}

pub fn load_once(http: Option<&Http>, paths: &Paths) {
    LOADED.call_once(|| {
        let cache = paths.feed_cache().with_file_name("launcher-catalog.json");
        let remote = http.ok_or_else(|| "HTTPS unavailable".to_string()).and_then(|h| {
            let catalog = h.get_catalog_bytes(CATALOG_URL, MAX_CATALOG_BYTES)?.ok_or("hosted catalog is not published")?;
            let signature = h.get_catalog_bytes(SIGNATURE_URL, MAX_SIGNATURE_BYTES)?.ok_or("catalog signature is not published")?;
            let verified = VerifiedCatalog::verify(&catalog, &signature)?;
            let saved = Saved {
                catalog: String::from_utf8(catalog).map_err(|_| "catalog is not UTF-8")?,
                signature: String::from_utf8(signature).map_err(|_| "signature is not UTF-8")?,
            };
            Ok((verified, saved))
        });
        let catalog = match remote {
            Ok((verified, saved)) => {
                if let Ok(bytes) = serde_json::to_vec(&saved)
                    && let Err(why) = write_atomic(&cache, &bytes)
                {
                    log::warn!("could not cache signed catalog: {why}");
                }
                Some(verified)
            }
            Err(why) => {
                log::warn!("hosted catalog unavailable: {why}; trying verified cache, then bundled catalog");
                saved_catalog(&cache)
            }
        };
        if let Some(catalog) = catalog {
            if catalog.rejected_entries > 0 {
                log::warn!("ignored {} invalid catalog entries", catalog.rejected_entries);
            }
            let mut icons = HashMap::new();
            let mut network_available = true;
            for (id, url, digest) in catalog.icons() {
                if artcraft_launcher_core::catalog::app_icon_png(id).is_some_and(|b| valid_icon(b, digest)) {
                    continue;
                }
                let saved = cache.with_file_name(format!("catalog-icon-{digest}.png"));
                let icon = read_capped(&saved, MAX_ICON_BYTES).filter(|b| valid_icon(b, digest)).or_else(|| {
                    if !network_available {
                        return None;
                    }
                    let bytes = match http?.get_catalog_bytes(url, MAX_ICON_BYTES) {
                        Ok(bytes) => bytes?,
                        Err(_) => {
                            network_available = false;
                            return None;
                        }
                    };
                    if !valid_icon(&bytes, digest) {
                        return None;
                    }
                    if let Err(why) = write_atomic(&saved, &bytes) {
                        log::warn!("could not cache catalog icon: {why}");
                    }
                    Some(bytes)
                });
                if let Some(icon) = icon {
                    icons.insert(id.to_string(), icon);
                }
            }
            catalog.activate(icons);
        }
    });
}

fn saved_catalog(path: &Path) -> Option<VerifiedCatalog> {
    // JSON escaping can expand a signed TOML byte to six bytes.
    let bytes = read_capped(path, MAX_CATALOG_BYTES * 6 + MAX_SIGNATURE_BYTES + 128)?;
    let saved: Saved = serde_json::from_slice(&bytes).ok()?;
    VerifiedCatalog::verify(saved.catalog.as_bytes(), saved.signature.as_bytes()).ok()
}

fn read_capped(path: &Path, max: usize) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > max as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= max).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the live signed catalog at getartcraft.com"]
    fn live_catalog_is_verified_cached_and_activated() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        load_once(Some(&http), &paths);
        let cache = paths.feed_cache().with_file_name("launcher-catalog.json");
        let verified = saved_catalog(&cache).expect("live catalog must be cached and verify with the pinned key");
        assert_eq!(verified.rejected_entries, 0);
        let icons: Vec<_> = verified.icons().collect();
        assert!(!icons.is_empty());
        for (id, _, digest) in icons {
            assert!(artcraft_launcher_core::hosted_catalog::apps().iter().any(|app| app.id == id));
            let icon = artcraft_launcher_core::catalog::app_icon_png(id).unwrap();
            assert!(valid_icon(icon, digest), "invalid live icon for {id}");
        }
    }

    #[test]
    fn cached_catalog_is_reverified_and_tampered_cache_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        let mut saved = Saved {
            catalog: include_str!("../../core/tests/fixtures/hosted-catalog.toml").into(),
            signature: include_str!("../../core/tests/fixtures/hosted-catalog.toml.sig").into(),
        };
        write_atomic(&path, &serde_json::to_vec(&saved).unwrap()).unwrap();
        assert!(saved_catalog(&path).is_some());
        saved.catalog.push('\n');
        write_atomic(&path, &serde_json::to_vec(&saved).unwrap()).unwrap();
        assert!(saved_catalog(&path).is_none());
        write_atomic(&path, b"not JSON").unwrap();
        assert!(saved_catalog(&path).is_none());
    }

    #[test]
    fn capped_cache_reads_reject_announced_or_actual_oversize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache");
        std::fs::write(&path, b"abcd").unwrap();
        assert_eq!(read_capped(&path, 4).unwrap(), b"abcd");
        assert!(read_capped(&path, 3).is_none());
    }
}
