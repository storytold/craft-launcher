//! Craft AI: the ArtCraft family's built-in assistant, as a dialog.
//!
//! Craft AI is a local Gemma 4 model the user chooses to download — never a cloud service. The
//! pane has three tabs:
//!
//! * **Assistant** — the app-specialised tasks of [`tasks`], asked of the model the user picked;
//!   answers arrive from a local model runtime ([`engine`]) on a worker thread.
//! * **Models** — the Gemma 4 catalogue ([`models`]) with each model's disk size and RAM need
//!   next to what this machine has, and the downloads: pulled by the runtime, or saved into the
//!   shared folder every ArtCraft app reads ([`hub`]) — one download serves them all.
//! * **About** — what runs where, and its honest limits.
//!
//! Downloading is optional; without a model the pane says so and nothing touches the network
//! beyond loopback. Craft AI is the family's, not this app's: the model files live in a shared
//! folder beside all the ArtCraft apps, one download of a model serves every app, and any model
//! a runtime already has is offered here the moment its app is opened. The native pane lives in
//! [`desktop`] — the web build shows a note instead, as a local runtime and worker threads are
//! of the native world.

#[cfg(not(target_arch = "wasm32"))]
mod desktop;
#[cfg(not(target_arch = "wasm32"))]
mod engine;
#[cfg(not(target_arch = "wasm32"))]
mod hub;
#[cfg(not(target_arch = "wasm32"))]
mod models;
#[cfg(not(target_arch = "wasm32"))]
mod tasks;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

use egui::Ui;

/// The window's `ui.show_craft_ai` flag id.
pub const ID: &str = "craftAi";

/// The pane. Native only: a local model runtime and worker threads are of that world; the web
/// build shows a note instead.
pub fn show(app: &mut crate::LauncherUi, ui: &mut Ui) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app; // the note draws alone; the pane itself is native-only
        dim(ui, &crate::i18n::t("Craft AI runs in the native app — it needs a local model runtime."));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        desktop::show(app, ui);
    }
}

/// A quiet aside line, the style the app's other panes use for theirs.
#[cfg(target_arch = "wasm32")]
fn dim(ui: &mut Ui, text: &str) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(text).size(12.0).color(t.muted));
}
