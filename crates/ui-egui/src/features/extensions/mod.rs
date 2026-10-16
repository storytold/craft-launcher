//! The Extensions screen: themes (and later plug-ins and resources) shared by every Craft app.
//!
//! Invariants:
//! - The screen reads and changes the shared store only through [`ExtensionSource`], which the
//!   desktop shell implements. With no source (the web build) the screen does not exist.
//! - Installing never changes anyone's look: a theme is worn only after the person chooses it.
//! - Every sentence a person reads goes through `tl!` and is free of developer words
//!   (`tests::plain_words`). Extension names come from the package and are shown as written.
//! - Drawing is in `screen.rs` and `card.rs`, what a click means is in `plan.rs`, and `sample.rs`
//!   is an in-memory store for tests.

pub mod card;
pub mod plan;
pub mod sample;
mod screen;
pub mod source;
#[cfg(test)]
mod tests;

use craft_ext_core::id::ExtensionId;

pub use plan::Focus;
pub use source::{ExtensionSource, Row};

/// Which list the screen shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Themes,
    Installed,
}

/// The launcher's two pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    Apps,
    Extensions,
}

/// What the screen remembers between frames.
#[derive(Default)]
pub(crate) struct State {
    /// The Extensions page is showing instead of the apps.
    pub open: bool,
    pub tab: Tab,
    /// What another app asked the page to narrow to; empty when the person opened it themselves.
    pub focus: Focus,
    pub rows: Vec<Row>,
    /// The store's generation when `rows` was read.
    pub generation: u64,
    /// `rows` has been read at least once since the page opened.
    pub loaded: bool,
    /// The context time at which to look at the generation again.
    pub next_poll: f64,
    /// The extension whose removal is waiting for a yes.
    pub confirm_remove: Option<ExtensionId>,
    /// Why the list could not be read, in words.
    pub error: Option<String>,
}
