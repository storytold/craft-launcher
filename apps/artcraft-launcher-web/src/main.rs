//! ArtCraft Launcher in the browser.
//!
//! A browser can't install apps, so this build lists the Craft App Suite with each app's latest
//! release (read straight from the GitHub API, which allows cross-origin requests) and offers the
//! right download for the visitor's system.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

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
    fn lang_comes_from_the_query_not_the_fragment() {
        assert_eq!(lang_params("https://getartcraft.com/launcher/?lang=de"), ["de"]);
        assert_eq!(lang_params("https://getartcraft.com/launcher/?x=1&lang=pt-BR#apps"), ["pt-BR"]);
        assert!(lang_params("https://getartcraft.com/launcher/#apps?lang=de").is_empty());
        assert!(lang_params("https://getartcraft.com/launcher/?lang=").is_empty());
    }
}
