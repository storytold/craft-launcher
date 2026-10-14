//! Taking over a per-application session file.
//!
//! An application that signed in before the suite store existed wrote its own session beside its
//! own settings. GridCraft's was `%APPDATA%\GridCraft\collabcraft-session.json`. Upgrading must not
//! sign anybody out. The application therefore hands that session to the suite store once and
//! deletes the file. The token then lives in the keychain and the whole suite can use it.
//!
//! The application does this, never the launcher. The file belongs to the application. Nothing
//! should reach into another application's folder to delete a file.
//!
//! The file is only deleted once the suite store holds the session. Where the suite store can keep
//! nothing (no keychain on this machine, or this process is isolated) the file stays exactly where
//! it is. Removing it would sign the user out. That is the one thing migration must not do.

use std::path::Path;

use serde::Deserialize;

use crate::accounts::{self, Account};
use crate::error::Error;
use crate::secret::Secret;
use crate::store::TokenStore;
use crate::{NewAccount, Store};

/// What a legacy session file said.
#[derive(Debug)]
pub struct Legacy {
    pub service: String,
    pub user: String,
    pub display: String,
    pub email: Option<String>,
    pub token: Secret,
}

/// What happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Adopted {
    /// There was no file. There is nothing to take over.
    Nothing,
    /// The session is in the suite store now. The file is gone.
    Taken(Account),
    /// The suite store can keep nothing here. The file was left alone and the application goes on
    /// reading it. `why` says what the keychain reported.
    Kept { why: String },
}

/// The shape on disk.
///
/// Read with aliases because the file predates this crate and several spellings are plausible. A
/// missing field is not fatal. Carrying a session over needs the deployment URL and the token. The
/// rest is re-read from the deployment on the next call to
/// [`crate::service::Service::identity`].
#[derive(Deserialize)]
struct Stored {
    #[serde(alias = "base", alias = "base_url", alias = "url", alias = "service_url", alias = "collabcraft")]
    service: String,
    #[serde(alias = "session", alias = "session_token", alias = "bearer", alias = "access_token")]
    token: String,
    #[serde(default, alias = "id", alias = "sub", alias = "subject", alias = "user_id")]
    user: String,
    #[serde(default, alias = "name", alias = "display_name")]
    display: String,
    #[serde(default)]
    email: Option<String>,
}

/// The largest session file that will be read.
const MAX_BYTES: u64 = 64 << 10;

/// Reads a legacy session file. A missing file is `Ok(None)`.
pub fn read(path: &Path) -> Result<Option<Legacy>, Error> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_BYTES => {
            return Err(Error::Malformed(format!("{} is {} bytes, too large to be a session", path.display(), meta.len())));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(format!("couldn't read {} ({e})", path.display()))),
    }
    let bytes = std::fs::read(path).map_err(|e| Error::Io(format!("couldn't read {} ({e})", path.display())))?;
    let stored: Stored = serde_json::from_slice(&bytes).map_err(|e| Error::Malformed(format!("{} didn't parse ({e})", path.display())))?;
    let service = accounts::normalise(&stored.service);
    if service.is_empty() || stored.token.trim().is_empty() {
        return Err(Error::Malformed(format!("{} names no deployment or carries no session", path.display())));
    }
    let user = if stored.user.is_empty() { service.clone() } else { stored.user };
    let display = if stored.display.is_empty() { user.clone() } else { stored.display };
    Ok(Some(Legacy { service, user, display, email: stored.email, token: Secret::new(stored.token.trim()) }))
}

/// Hands a legacy session to the suite store and removes the file.
///
/// Call this once at startup, before asking the suite store who is signed in. Safe to call when
/// there is no file. Safe to call again after it has run.
pub fn adopt<S: TokenStore>(store: &Store<S>, path: &Path, now: i64) -> Result<Adopted, Error> {
    let Some(legacy) = read(path)? else { return Ok(Adopted::Nothing) };
    if let Err(why) = store.usable() {
        return Ok(Adopted::Kept { why: why.to_string() });
    }
    let new = NewAccount { service: legacy.service, user: legacy.user, display: legacy.display, email: legacy.email, linked: false };
    let account = store.sign_in(&new, &legacy.token, now)?;
    // Only now, with the session held somewhere better, does the file go.
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::Io(format!("the session moved to the keychain. {} is still there ({e})", path.display()))),
    }
    Ok(Adopted::Taken(account))
}
