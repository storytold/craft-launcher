//! Browser startup catalog refresh. Storage failures leave the compiled fallback usable.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use artcraft_launcher_core::hosted_catalog::{
    CATALOG_URL, MAX_CATALOG_BYTES, MAX_ICON_BYTES, MAX_SIGNATURE_BYTES, SIGNATURE_URL, VerifiedCatalog, valid_icon,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

const CACHE_KEY: &str = "artcraft-launcher.signed-catalog.v1";
static CHECKED: AtomicBool = AtomicBool::new(false);

pub async fn refresh_once() {
    if CHECKED.swap(true, Ordering::SeqCst) {
        return;
    }
    let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
    let remote = fetch_pair().await;
    let catalog = match remote {
        Ok((catalog, signature)) => match VerifiedCatalog::verify(&catalog, &signature) {
            Ok(verified) => {
                if let (Some(storage), Ok(text), Ok(sig)) = (&storage, String::from_utf8(catalog), String::from_utf8(signature))
                    && let Ok(json) = serde_json::to_string(&(text, sig))
                {
                    let _ = storage.set_item(CACHE_KEY, &json);
                }
                Some(verified)
            }
            Err(why) => {
                log::warn!("hosted catalog rejected: {why}");
                None
            }
        },
        Err(why) => {
            log::warn!("hosted catalog unavailable: {why}");
            None
        }
    }
    .or_else(|| {
        let saved = storage.as_ref()?.get_item(CACHE_KEY).ok()??;
        if saved.len() > MAX_CATALOG_BYTES * 6 + MAX_SIGNATURE_BYTES + 128 {
            return None;
        }
        let (text, sig): (String, String) = serde_json::from_str(&saved).ok()?;
        VerifiedCatalog::verify(text.as_bytes(), sig.as_bytes()).ok()
    });
    if let Some(catalog) = catalog {
        if catalog.rejected_entries > 0 {
            log::warn!("ignored {} invalid catalog entries", catalog.rejected_entries);
        }
        let mut icons = HashMap::new();
        for (id, url, digest) in catalog.icons() {
            if artcraft_launcher_core::catalog::app_icon_png(id).is_some_and(|b| valid_icon(b, digest)) {
                continue;
            }
            // The browser's HTTP cache keeps content-addressed icons; hashes are checked every time.
            match fetch_bytes(url, MAX_ICON_BYTES).await {
                Ok(bytes) if valid_icon(&bytes, digest) => {
                    icons.insert(id.to_string(), bytes);
                }
                Ok(_) => {}
                Err(_) => break, // An offline browser must not wait one timeout for every icon.
            }
        }
        catalog.activate(icons);
    }
}

async fn fetch_pair() -> Result<(Vec<u8>, Vec<u8>), String> {
    let catalog = fetch_bytes(CATALOG_URL, MAX_CATALOG_BYTES).await?;
    let signature = fetch_bytes(SIGNATURE_URL, MAX_SIGNATURE_BYTES).await?;
    Ok((catalog, signature))
}

/// Stream and cap the actual body, even if content-length is missing or dishonest.
async fn fetch_bytes(url: &str, max: usize) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or("no browser window")?;
    let init = web_sys::RequestInit::new();
    init.set_mode(web_sys::RequestMode::Cors);
    init.set_redirect(web_sys::RequestRedirect::Error);
    init.set_signal(Some(&web_sys::AbortSignal::timeout_with_u32(20_000)));
    let request = web_sys::Request::new_with_str_and_init(url, &init).map_err(|_| "invalid catalog request")?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| "catalog request failed")?
        .dyn_into()
        .map_err(|_| "invalid catalog response")?;
    if response.status() != 200 {
        return Err(format!("catalog server answered {}", response.status()));
    }
    let announced = response.headers().get("content-length").ok().flatten().and_then(|v| v.parse::<usize>().ok());
    if announced.is_some_and(|n| n > max) {
        return Err("catalog response is too large".into());
    }
    let reader: web_sys::ReadableStreamDefaultReader =
        response.body().ok_or("catalog response has no body")?.get_reader().dyn_into().map_err(|_| "catalog response cannot be streamed")?;
    let mut bytes = Vec::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(|_| "catalog stream failed")?;
        if js_sys::Reflect::get(&chunk, &"done".into()).ok().and_then(|v| v.as_bool()) == Some(true) {
            return Ok(bytes);
        }
        let value = js_sys::Reflect::get(&chunk, &"value".into()).map_err(|_| "invalid catalog stream chunk")?;
        let chunk = js_sys::Uint8Array::new(&value);
        if chunk.length() as usize > max.saturating_sub(bytes.len()) {
            let _ = reader.cancel();
            return Err("catalog response is too large".into());
        }
        bytes.extend_from_slice(&chunk.to_vec());
    }
}
