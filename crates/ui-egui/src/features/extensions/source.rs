//! What the Extensions screen needs from the shared store, as a trait the desktop shell implements.
//!
//! The store itself (`craft-ext-store`) is desktop only, so the shared UI sees it only through
//! [`ExtensionSource`] and plain [`Row`]s built from the wasm-safe contract types. A source never
//! panics and reports every failure as a sentence a person can read.

use std::collections::BTreeSet;
use std::path::Path;

use craft_ext_core::id::{AppId, ExtensionId};
use craft_ext_core::manifest::Kind;
use craft_ext_core::palette::Palette;
use craft_ext_core::tier::Tier;
use craft_ext_core::version::Version;

/// One installed extension with everything the screen draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: ExtensionId,
    pub version: Version,
    pub kind: Kind,
    pub name: String,
    /// Where it came from, as the launcher recorded it (never read from the package).
    pub tier: Tier,
    /// Apps the package says it works with.
    pub apps: Vec<AppId>,
    /// Apps it is switched on for.
    pub enabled: BTreeSet<AppId>,
    /// Apps wearing it as their look (themes only).
    pub worn: BTreeSet<AppId>,
    /// The colours of a theme, when its file could be read and passed the checks.
    pub palette: Option<Palette>,
}

/// The shared store as the screen sees it.
pub trait ExtensionSource {
    /// Changes whenever anything is installed, removed, switched or chosen, in any app.
    fn generation(&mut self) -> u64;
    /// Everything installed.
    fn rows(&mut self) -> Result<Vec<Row>, String>;
    /// Installs a package the person picked. Returns the name of what was added.
    fn add_file(&mut self, path: &Path) -> Result<String, String>;
    /// Removes an extension from every app.
    fn remove(&mut self, id: &ExtensionId) -> Result<(), String>;
    /// Switches an extension on or off for one app.
    fn set_enabled(&mut self, app: &AppId, id: &ExtensionId, on: bool) -> Result<(), String>;
    /// Chooses the theme an app wears, or none to give it back its own look.
    fn wear(&mut self, app: &AppId, id: Option<&ExtensionId>) -> Result<(), String>;
}
