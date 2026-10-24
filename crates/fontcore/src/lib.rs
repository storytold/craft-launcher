//! FontCore: a local, open-source font vault for the Craft App Suite.
//!
//! Mirrored from [calebnanji/FontCore](https://github.com/calebnanji/FontCore) (commit 8059118,
//! `fonts.json` schema v1, stable). Keep the two in step: the layout is specified by
//! `schema/fonts.v1.json`, and `tests/schema.rs` checks this code against it.
//!
//! FontCore never touches the operating system's font registry. Fonts are
//! copied into a vault directory and described by a single `fonts.json`
//! manifest. Management tools mutate the vault through [`Vault`]; suite apps
//! (photocraft, wordcraft, pdfcraft, ...) read it at launch through
//! [`load_for_app`] or [`Vault::fonts_for_app`].
//!
//! ```no_run
//! let set = fontcore::load_for_app("ai.storytold.photocraft")?;
//! for font in &set.fonts {
//!     let bytes = font.read()?;
//!     for face in font.faces() {
//!         // register face.index of `bytes` with the app's text engine
//! #       let _ = (&bytes, face);
//!     }
//! }
//! # Ok::<(), fontcore::Error>(())
//! ```

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod app_id;
mod error;
mod license;
mod loader;
mod manifest;
mod metadata;
mod timestamp;
mod vault;

pub use app_id::AppId;
pub use error::{Error, Result};
pub use license::{CustomLicense, License};
pub use loader::{FontSet, ResolvedFont, load_for_app};
pub use manifest::{FontEntry, MANIFEST_FILE, Manifest, SCHEMA_VERSION};
pub use metadata::{FaceInfo, FontFormat, inspect};
pub use timestamp::Timestamp;
pub use vault::{AddOptions, AddOutcome, FONTS_DIR, HOME_ENV, Issue, Vault};
