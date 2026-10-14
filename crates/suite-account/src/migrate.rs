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
//!
//! The earliest of these files held one key: `{"token": "…"}`. It names no deployment and no
//! subject. There is nothing to key an account on. Such a file is left exactly where it is too. A
//! made-up handle would file the token under something nothing ever looks up again. That reads as
//! a silent sign-out. An application that rewrites the file with the deployment and the subject is
//! adopted on the next startup. Adopting one launch late costs nothing.

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
    /// The deployment's base URL, with any trailing slash trimmed. Empty where the file predates
    /// the application recording it.
    pub service: String,
    /// The deployment's own identifier for the account: the OIDC subject. Empty where the file
    /// predates the application recording it.
    pub user: String,
    pub display: String,
    pub email: Option<String>,
    pub token: Secret,
}

impl Legacy {
    /// What the file does not say yet, when it cannot be keyed to an account.
    ///
    /// The account handle is a digest of the deployment and the subject. Both are therefore
    /// needed. A session written before the application recorded them carries a token alone.
    pub fn incomplete(&self) -> Option<&'static str> {
        if self.service.is_empty() {
            return Some("which deployment it belongs to");
        }
        if self.user.is_empty() {
            return Some("whose account it is");
        }
        None
    }
}

/// What happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Adopted {
    /// There was no file. There is nothing to take over.
    Nothing,
    /// The session is in the suite store now. The file is gone.
    Taken(Account),
    /// The file was left exactly where it is. The application goes on reading it. `why` says
    /// which case it was: the suite store can keep nothing here, or the file does not yet say
    /// enough to key an account on.
    Kept { why: String },
}

/// The shape on disk.
///
/// What an application writes today is `token`, `service`, `subject` and `display`. The file
/// predates this crate. The aliases therefore cover the other plausible spellings.
///
/// The token is the only key that has to be there: a file without one carries no session at all.
/// Everything else defaults. That is what lets the earliest `{"token": "…"}` file still parse.
/// [`adopt`] then leaves such a file alone. Keying an account on nothing is the alternative.
#[derive(Deserialize)]
struct Stored {
    #[serde(alias = "session", alias = "session_token", alias = "bearer", alias = "access_token")]
    token: String,
    #[serde(default, alias = "base", alias = "base_url", alias = "url", alias = "service_url", alias = "collabcraft")]
    service: String,
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
    if stored.token.trim().is_empty() {
        return Err(Error::Malformed(format!("{} carries no session", path.display())));
    }
    let service = accounts::normalise(&stored.service);
    // The subject is never stood in for. A handle built on anything else would file the token
    // where nothing looks for it. See [`Legacy::incomplete`].
    let user = stored.user.trim().to_owned();
    let display = if stored.display.trim().is_empty() { user.clone() } else { stored.display };
    Ok(Some(Legacy { service, user, display, email: stored.email, token: Secret::new(stored.token.trim()) }))
}

/// Hands a legacy session to the suite store and removes the file.
///
/// Call this once at startup, before asking the suite store who is signed in. Safe to call when
/// there is no file. Safe to call again after it has run.
pub fn adopt<S: TokenStore>(store: &Store<S>, path: &Path, now: i64) -> Result<Adopted, Error> {
    let Some(legacy) = read(path)? else { return Ok(Adopted::Nothing) };
    if let Some(unsaid) = legacy.incomplete() {
        return Ok(Adopted::Kept { why: format!("{} does not say {unsaid} yet", path.display()) });
    }
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
