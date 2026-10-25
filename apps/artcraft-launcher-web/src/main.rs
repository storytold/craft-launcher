//! Zendaya Launcher in the browser.
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
