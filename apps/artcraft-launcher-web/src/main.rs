//! ArtCraft Launcher in the browser.
//!
//! A browser can't install apps, so this build lists the Craft App Suite with each app's latest
//! release (read straight from the GitHub API, which allows cross-origin requests) and offers the
//! right download for the visitor's system.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_arch = "wasm32")]
mod hosted_catalog;
#[cfg(any(test, all(target_arch = "wasm32", feature = "server")))]
mod provider_protocol;
#[cfg(all(target_arch = "wasm32", feature = "server"))]
mod server;
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("artcraft-launcher-web is the browser build: run `trunk serve` in apps/artcraft-launcher-web.");
}

/// What a missing release page means to the browser's pre-release lookup: the repository has no
/// releases when its first page is missing, and the lookup is unfinished when a later page of an
/// already-continuing search disappears. `Err(None)` means "not known", never "no release".
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn missing_page(page: usize) -> Result<artcraft_launcher_core::protocol::LatestRelease, Option<i64>> {
    if page == 1 { Ok(artcraft_launcher_core::protocol::LatestRelease::None) } else { Err(None) }
}

/// The `lang` values in a page URL's query (`?lang=de`), not its `#fragment`: `?lang=de#apps`
/// is German, not the unknown tag `de#apps`.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn lang_params(href: &str) -> Vec<String> {
    let page = href.split('#').next().unwrap_or(href);
    let Some((_, query)) = page.split_once('?') else { return Vec::new() };
    query.split('&').filter_map(|pair| pair.split_once('=')).filter(|(k, v)| *k == "lang" && !v.is_empty()).map(|(_, v)| v.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::lang_params;

    #[test]
    fn a_missing_later_page_is_an_unfinished_lookup_not_an_empty_one() {
        assert!(matches!(super::missing_page(1), Ok(artcraft_launcher_core::protocol::LatestRelease::None)));
        for page in [2, 3, 5] {
            assert!(super::missing_page(page).is_err(), "page {page} vanishing leaves the answer unknown");
        }
    }

    #[test]
    fn lang_comes_from_the_query_not_the_fragment() {
        assert_eq!(lang_params("https://getartcraft.com/launcher/?lang=de"), ["de"]);
        assert_eq!(lang_params("https://getartcraft.com/launcher/?x=1&lang=pt-BR#apps"), ["pt-BR"]);
        assert!(lang_params("https://getartcraft.com/launcher/#apps?lang=de").is_empty());
        assert!(lang_params("https://getartcraft.com/launcher/?lang=").is_empty());
    }
}
